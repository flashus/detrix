//! Adapter Manager — manages local adapter instances per connection.
//!
//! Creates and manages local adapters for each connection assigned to this agent.
//! For Go connections, uses eBPF uprobes. For Python/Rust, uses DAP adapters
//! (Phase 5: Hybrid DAP mode — useful when debuggers bind to 127.0.0.1 only).

use crate::error::{AgentError, Result};
use crate::proto_convert::{
    metric_event_to_proto, proto_set_metric_to_metric, truncate_values_json,
};
use dashmap::DashMap;
use detrix_api::generated::detrix::v1::{
    agent_message, AgentConnectionUpdate, AgentCreateConnection, AgentMessage, ConnectionStatus,
    DropCountUpdate, InspectFile, InspectResponse, MetricEventBatch, RemoveMetric, RemoveMetricAck,
    SerializedMetricEvent, SetMetric, SetMetricAck,
};
use detrix_core::{ConnectionId, Location, Metric, MetricEvent, ParseLanguageExt, SourceLanguage};
use detrix_dap::{PythonAdapter, RustAdapter};
use detrix_ebpf::{resolve_backend, CaptureBackend, CaptureConfig, EbpfAdapter, ProfileId};
use detrix_logging::{debug, info, warn};
use detrix_ports::DapAdapterRef;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Manages local adapter instances for each connection.
pub struct AdapterManager {
    adapters: Arc<DashMap<String, DapAdapterRef>>,
    ctrl_tx: mpsc::UnboundedSender<AgentMessage>,
    event_tx: mpsc::Sender<AgentMessage>,
    /// Global counter — used for heartbeat events_dropped field.
    events_dropped: Arc<AtomicU64>,
    capture_config: CaptureConfig,
    /// Per-connection drop counters — used for accurate DropCountUpdate messages.
    connection_drop_counts: DashMap<String, Arc<AtomicU64>>,
    /// Language string per connection, populated from AgentCreateConnection.language.
    connection_languages: DashMap<String, String>,
    /// Forwarder task handles — tracked so they can be aborted on close_connection.
    forwarder_handles: DashMap<String, JoinHandle<()>>,
    /// Forwarded-events counter — incremented by forward_batch on success.
    pub events_forwarded: Arc<AtomicU64>,
    /// Allowed directory prefixes for server-requested file reads.
    /// Empty = allow any readable path (with a warning).
    allowed_read_prefixes: Vec<PathBuf>,
}

impl AdapterManager {
    pub fn new(
        ctrl_tx: mpsc::UnboundedSender<AgentMessage>,
        event_tx: mpsc::Sender<AgentMessage>,
        events_dropped: Arc<AtomicU64>,
        capture_config: CaptureConfig,
        events_forwarded: Arc<AtomicU64>,
        allowed_read_prefixes: Vec<PathBuf>,
    ) -> Self {
        // Retain configured prefixes even when they do not exist yet. A target
        // directory may be mounted after the agent starts; canonicalization is
        // therefore performed at each read boundary.

        Self {
            adapters: Arc::new(DashMap::new()),
            ctrl_tx,
            event_tx,
            events_dropped,
            capture_config,
            connection_drop_counts: DashMap::new(),
            connection_languages: DashMap::new(),
            forwarder_handles: DashMap::new(),
            events_forwarded,
            allowed_read_prefixes,
        }
    }

    /// Create a new connection — dispatches by language.
    ///
    /// For Go connections, uses eBPF uprobes.
    /// For Python/Rust, uses DAP adapters connecting to local debuggers.
    pub async fn create_connection(&self, msg: AgentCreateConnection) {
        let connection_id = msg.connection_id.clone();
        let language = msg.language.to_lowercase();
        let requested_backend = match CaptureBackend::parse(&msg.capture_backend) {
            Ok(backend) => backend,
            Err(error) => {
                self.send_connection_update(
                    &connection_id,
                    ConnectionStatus::Failed,
                    Some(&error.to_string()),
                )
                .await;
                return;
            }
        };
        let requested_profile = match msg.capture_profile.to_lowercase().as_str() {
            "" => match language.as_str() {
                "go" => ProfileId::Go,
                "rust" => ProfileId::Rust,
                _ => ProfileId::Go,
            },
            "go" => ProfileId::Go,
            "rust" => ProfileId::Rust,
            other => {
                self.send_connection_update(
                    &connection_id,
                    ConnectionStatus::Failed,
                    Some(&format!("unsupported capture profile: {other}")),
                )
                .await;
                return;
            }
        };

        let language_profile = match language.as_str() {
            "go" => Some(ProfileId::Go),
            "rust" => Some(ProfileId::Rust),
            _ => None,
        };
        if let Some(language_profile) = language_profile {
            if language_profile != requested_profile {
                self.send_connection_update(
                    &connection_id,
                    ConnectionStatus::Failed,
                    Some("capture_profile does not match connection language"),
                )
                .await;
                return;
            }
        }

        info!(
            connection_id = %connection_id,
            language = %language,
            capture_backend = ?requested_backend,
            capture_profile = ?requested_profile,
            "Creating connection"
        );

        // Rust eBPF is deliberately opt-in and fail-closed until the
        // privileged live gate is complete. Never route an explicit Rust
        // eBPF request through the Go adapter or silently downgrade it.
        if language == "rust" && requested_backend == CaptureBackend::Ebpf {
            let error = resolve_backend(
                CaptureBackend::Ebpf,
                ProfileId::Rust,
                cfg!(target_os = "linux"),
                false,
            )
            .expect_err("Rust eBPF must remain gated until debug-image/runtime support is enabled");
            self.send_connection_update(
                &connection_id,
                ConnectionStatus::Failed,
                Some(&error.to_string()),
            )
            .await;
            return;
        }

        // CreateConnection is normally serialized by the stream reader, but a
        // duplicate command can still arrive after a reconnect or retry. Stop
        // the previous local instance before replacing it so its probe and
        // forwarder do not leak events/resources under the same ID.
        if let Some((_, handle)) = self.forwarder_handles.remove(&connection_id) {
            handle.abort();
        }
        if let Some((_, adapter)) = self.adapters.remove(&connection_id) {
            let _ = adapter.stop().await;
        }

        // Record language for later use in handle_set_metric / handle_remove_metric.
        self.connection_languages
            .insert(connection_id.clone(), language.clone());
        // Initialise per-connection drop counter.
        self.connection_drop_counts
            .insert(connection_id.clone(), Arc::new(AtomicU64::new(0)));

        match language.as_str() {
            "go" => {
                let binary_path = PathBuf::from(&msg.binary_path);
                match EbpfAdapter::new_with_config(&binary_path, self.capture_config.clone()) {
                    Ok(adapter) => {
                        let adapter: DapAdapterRef = Arc::new(adapter);
                        if let Err(e) = adapter.start().await {
                            warn!("Failed to start EbpfAdapter: {e}");
                            self.send_connection_update(
                                &connection_id,
                                ConnectionStatus::Failed,
                                Some(&e.to_string()),
                            )
                            .await;
                            return;
                        }
                        match adapter.subscribe_events().await {
                            Ok(event_rx) => {
                                self.adapters.insert(connection_id.clone(), adapter);
                                self.spawn_event_forwarder(connection_id.clone(), event_rx);
                                self.send_connection_update(
                                    &connection_id,
                                    ConnectionStatus::Connected,
                                    None,
                                )
                                .await;
                            }
                            Err(e) => {
                                warn!("Failed to subscribe events from EbpfAdapter: {e}");
                                self.send_connection_update(
                                    &connection_id,
                                    ConnectionStatus::Failed,
                                    Some(&e.to_string()),
                                )
                                .await;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to create EbpfAdapter: {e}");
                        self.send_connection_update(
                            &connection_id,
                            ConnectionStatus::Failed,
                            Some(&e.to_string()),
                        )
                        .await;
                    }
                }
            }
            "python" => {
                let port = match u16::try_from(msg.port) {
                    Ok(p) => p,
                    Err(_) => {
                        warn!(port = msg.port, "Invalid port: out of u16 range");
                        self.send_connection_update(
                            &connection_id,
                            ConnectionStatus::Failed,
                            Some(&format!("port {} out of valid range", msg.port)),
                        )
                        .await;
                        return;
                    }
                };
                let config = PythonAdapter::default_config(port).with_host(&msg.host);
                let adapter = PythonAdapter::new(config, PathBuf::from("/"));
                let adapter: DapAdapterRef = Arc::new(adapter);
                if let Err(e) = adapter.start().await {
                    warn!("Failed to start PythonAdapter: {e}");
                    self.send_connection_update(
                        &connection_id,
                        ConnectionStatus::Failed,
                        Some(&e.to_string()),
                    )
                    .await;
                    return;
                }
                match adapter.subscribe_events().await {
                    Ok(event_rx) => {
                        self.adapters.insert(connection_id.clone(), adapter);
                        self.spawn_event_forwarder(connection_id.clone(), event_rx);
                        self.send_connection_update(
                            &connection_id,
                            ConnectionStatus::Connected,
                            None,
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!("Failed to subscribe events from PythonAdapter: {e}");
                        self.send_connection_update(
                            &connection_id,
                            ConnectionStatus::Failed,
                            Some(&e.to_string()),
                        )
                        .await;
                    }
                }
            }
            "rust" => {
                let port = match u16::try_from(msg.port) {
                    Ok(p) => p,
                    Err(_) => {
                        warn!(port = msg.port, "Invalid port: out of u16 range");
                        self.send_connection_update(
                            &connection_id,
                            ConnectionStatus::Failed,
                            Some(&format!("port {} out of valid range", msg.port)),
                        )
                        .await;
                        return;
                    }
                };
                let config = RustAdapter::default_config(port).with_host(&msg.host);
                let adapter = RustAdapter::new(config, PathBuf::from("/"));
                let adapter: DapAdapterRef = Arc::new(adapter);
                if let Err(e) = adapter.start().await {
                    warn!("Failed to start RustAdapter: {e}");
                    self.send_connection_update(
                        &connection_id,
                        ConnectionStatus::Failed,
                        Some(&e.to_string()),
                    )
                    .await;
                    return;
                }
                match adapter.subscribe_events().await {
                    Ok(event_rx) => {
                        self.adapters.insert(connection_id.clone(), adapter);
                        self.spawn_event_forwarder(connection_id.clone(), event_rx);
                        self.send_connection_update(
                            &connection_id,
                            ConnectionStatus::Connected,
                            None,
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!("Failed to subscribe events from RustAdapter: {e}");
                        self.send_connection_update(
                            &connection_id,
                            ConnectionStatus::Failed,
                            Some(&e.to_string()),
                        )
                        .await;
                    }
                }
            }
            _ => {
                warn!(language = %language, "Unsupported language for agent connection");
                self.send_connection_update(
                    &connection_id,
                    ConnectionStatus::Failed,
                    Some("Unsupported language"),
                )
                .await;
            }
        }
    }

    /// Resolve the SourceLanguage for a connection.
    fn connection_language(&self, connection_id: &str) -> crate::error::Result<SourceLanguage> {
        self.connection_languages
            .get(connection_id)
            .map(|v| Ok(v.value().parse_language_lossy()))
            .unwrap_or_else(|| {
                Err(AgentError::Scanner(format!(
                    "no language record for connection {connection_id}"
                )))
            })
    }

    /// Handle SetMetric.
    pub async fn handle_set_metric(&self, msg: SetMetric) {
        let connection_id = msg.connection_id.clone();
        let request_id = msg.request_id.clone();
        let language = match self.connection_language(&connection_id) {
            Ok(l) => l,
            Err(e) => {
                warn!(connection_id = %connection_id, "SetMetric: {e}");
                let _ = self.ctrl_tx.send(AgentMessage {
                    msg: Some(agent_message::Msg::SetMetricAck(SetMetricAck {
                        request_id,
                        verified: false,
                        actual_line: 0,
                        message: String::new(),
                        error: e.to_string(),
                    })),
                });
                return;
            }
        };

        let metric = match proto_set_metric_to_metric(&msg, language) {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to parse SetMetric: {e}");
                let _ = self.ctrl_tx.send(AgentMessage {
                    msg: Some(agent_message::Msg::SetMetricAck(SetMetricAck {
                        request_id,
                        verified: false,
                        actual_line: 0,
                        message: String::new(),
                        error: e.to_string(),
                    })),
                });
                return;
            }
        };

        let Some(adapter) = self.adapters.get(&connection_id).map(|e| e.clone()) else {
            warn!(connection_id = %connection_id, "Connection not found");
            let _ = self.ctrl_tx.send(AgentMessage {
                msg: Some(agent_message::Msg::SetMetricAck(SetMetricAck {
                    request_id,
                    verified: false,
                    actual_line: 0,
                    message: String::new(),
                    error: "Connection not found".to_string(),
                })),
            });
            return;
        };

        let ack = match adapter.set_metric(&metric).await {
            Ok(result) => SetMetricAck {
                request_id,
                verified: result.verified,
                actual_line: result.line,
                message: result.message.unwrap_or_default(),
                error: String::new(),
            },
            Err(e) => SetMetricAck {
                request_id,
                verified: false,
                actual_line: 0,
                message: String::new(),
                error: e.to_string(),
            },
        };
        let _ = self.ctrl_tx.send(AgentMessage {
            msg: Some(agent_message::Msg::SetMetricAck(ack)),
        });
    }

    /// Handle RemoveMetric.
    pub async fn handle_remove_metric(&self, msg: RemoveMetric) {
        let connection_id = msg.connection_id.clone();
        let request_id = msg.request_id.clone();
        let language = match self.connection_language(&connection_id) {
            Ok(l) => l,
            Err(e) => {
                warn!(connection_id = %connection_id, "RemoveMetric: {e}");
                let _ = self.ctrl_tx.send(AgentMessage {
                    msg: Some(agent_message::Msg::RemoveMetricAck(RemoveMetricAck {
                        request_id,
                        confirmed: false,
                        error: e.to_string(),
                    })),
                });
                return;
            }
        };

        let Some(adapter) = self.adapters.get(&connection_id).map(|e| e.clone()) else {
            warn!(connection_id = %connection_id, "Connection not found");
            let _ = self.ctrl_tx.send(AgentMessage {
                msg: Some(agent_message::Msg::RemoveMetricAck(RemoveMetricAck {
                    request_id,
                    confirmed: false,
                    error: "Connection not found".to_string(),
                })),
            });
            return;
        };

        let metric = Metric {
            id: None,
            name: msg.metric_name.clone(),
            connection_id: ConnectionId(connection_id.clone()),
            group: None,
            location: Location {
                file: String::new(),
                line: 0,
            },
            expressions: Vec::new(),
            language,
            enabled: false,
            mode: detrix_core::MetricMode::default(),
            condition: None,
            safety_level: detrix_core::SafetyLevel::default(),
            created_at: None,
            user_id: None,
            agent_id: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: detrix_core::AnchorStatus::default(),
        };

        let ack = match adapter.remove_metric(&metric).await {
            Ok(_) => RemoveMetricAck {
                request_id,
                confirmed: true,
                error: String::new(),
            },
            Err(e) => RemoveMetricAck {
                request_id,
                confirmed: false,
                error: e.to_string(),
            },
        };
        let _ = self.ctrl_tx.send(AgentMessage {
            msg: Some(agent_message::Msg::RemoveMetricAck(ack)),
        });
    }

    /// Close a connection — aborts the event forwarder task and stops the adapter.
    pub async fn close_connection(&self, connection_id: &str) {
        info!(connection_id, "Closing connection");
        // Abort the forwarder task first so it stops producing events.
        if let Some((_, handle)) = self.forwarder_handles.remove(connection_id) {
            handle.abort();
        }
        if let Some((_, adapter)) = self.adapters.remove(connection_id) {
            let _ = adapter.stop().await;
        }
        self.connection_languages.remove(connection_id);
        self.connection_drop_counts.remove(connection_id);
    }

    /// Stop all adapters and abort all forwarder tasks.
    pub async fn stop_all(&self) {
        // Abort all forwarder tasks first.
        let keys: Vec<String> = self
            .forwarder_handles
            .iter()
            .map(|e| e.key().clone())
            .collect();
        for key in &keys {
            if let Some((_, handle)) = self.forwarder_handles.remove(key) {
                handle.abort();
            }
        }
        // Stop all adapters.
        let keys: Vec<String> = self.adapters.iter().map(|e| e.key().clone()).collect();
        for key in keys {
            if let Some((_, adapter)) = self.adapters.remove(&key) {
                let _ = adapter.stop().await;
            }
        }
        self.connection_languages.clear();
        self.connection_drop_counts.clear();
    }

    /// Read a file from the local filesystem.
    ///
    /// If `allowed_read_prefixes` is configured, the requested path is canonicalised and
    /// must start with one of the allowed prefixes. This prevents a compromised server from
    /// requesting arbitrary files (e.g. SSH keys, /etc/shadow) from the agent host.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| AgentError::Scanner(format!("Cannot canonicalize {path}: {e}")))?;

        if !self.allowed_read_prefixes.is_empty() {
            let allowed = self.allowed_read_prefixes.iter().any(|prefix| {
                std::fs::canonicalize(prefix)
                    .ok()
                    .is_some_and(|canonical_prefix| canonical.starts_with(canonical_prefix))
            });
            if !allowed {
                return Err(AgentError::Scanner(format!(
                    "Path {path:?} is outside allowed read prefixes"
                )));
            }
        } else {
            debug!(
                path = %path,
                "read_file: allowed_read_prefixes not configured, no prefix check applied"
            );
        }

        std::fs::read(&canonical)
            .map_err(|e| AgentError::Scanner(format!("Cannot read {}: {e}", canonical.display())))
    }

    /// Inspect a binary for variable information.
    pub async fn inspect_file(&self, msg: InspectFile) -> InspectResponse {
        InspectResponse {
            request_id: msg.request_id,
            variables: Vec::new(),
            error: "Not yet implemented".to_string(),
        }
    }

    fn spawn_event_forwarder(
        &self,
        connection_id: String,
        mut event_rx: mpsc::Receiver<MetricEvent>,
    ) {
        let event_tx = self.event_tx.clone();
        let ctrl_tx = self.ctrl_tx.clone();
        // Per-connection drop counter for DropCountUpdate accuracy.
        let drop_counter = self
            .connection_drop_counts
            .entry(connection_id.clone())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone();
        // Global dropped counter — still incremented for heartbeat consistency.
        let global_dropped = Arc::clone(&self.events_dropped);
        let events_forwarded = Arc::clone(&self.events_forwarded);
        let connection_id_clone = connection_id.clone();
        let handle = tokio::spawn(async move {
            let mut batch = Vec::with_capacity(64);
            loop {
                let deadline = tokio::time::sleep(Duration::from_millis(100));
                tokio::pin!(deadline);

                let mut batch_ready = false;
                let mut stream_closed = false;

                while !batch_ready && !stream_closed {
                    tokio::select! {
                        event = event_rx.recv() => match event {
                            None => {
                                stream_closed = true;
                            }
                            Some(e) => {
                                batch.push(e);
                                if batch.len() >= 64 {
                                    batch_ready = true;
                                }
                            }
                        },
                        _ = &mut deadline => {
                            batch_ready = true;
                        }
                    }
                }

                let events = std::mem::take(&mut batch);
                if !events.is_empty() {
                    Self::forward_batch(
                        &event_tx,
                        &ctrl_tx,
                        &drop_counter,
                        &global_dropped,
                        &events_forwarded,
                        &connection_id_clone,
                        events,
                    )
                    .await;
                }

                if stream_closed {
                    break;
                }
            }
        });
        self.forwarder_handles.insert(connection_id, handle);
    }

    #[allow(clippy::too_many_arguments)]
    async fn forward_batch(
        event_tx: &mpsc::Sender<AgentMessage>,
        ctrl_tx: &mpsc::UnboundedSender<AgentMessage>,
        drop_counter: &Arc<AtomicU64>,
        global_dropped: &Arc<AtomicU64>,
        events_forwarded: &Arc<AtomicU64>,
        connection_id: &str,
        events: Vec<MetricEvent>,
    ) {
        let proto_events: Vec<SerializedMetricEvent> = events
            .iter()
            .map(|e| {
                let mut proto = metric_event_to_proto(e);
                proto.values_json = truncate_values_json(&proto.values_json);
                proto
            })
            .collect();

        let batch_msg = AgentMessage {
            msg: Some(agent_message::Msg::Events(MetricEventBatch {
                connection_id: connection_id.to_string(),
                events: proto_events,
            })),
        };

        let event_count = events.len() as u64;
        if event_tx.try_send(batch_msg).is_ok() {
            events_forwarded.fetch_add(event_count, Ordering::Relaxed);
        } else {
            // Increment global counter for heartbeat.
            global_dropped.fetch_add(event_count, Ordering::Relaxed);
            // Increment per-connection counter for accurate DropCountUpdate.
            let count = drop_counter.fetch_add(event_count, Ordering::Relaxed) + event_count;
            warn!(
                connection_id = %connection_id,
                dropped = count,
                batch_size = event_count,
                "Event channel full, dropping batch"
            );
            let _ = ctrl_tx.send(AgentMessage {
                msg: Some(agent_message::Msg::DropCount(DropCountUpdate {
                    connection_id: connection_id.to_string(),
                    total_events_dropped: count,
                })),
            });
        }
    }

    async fn send_connection_update(
        &self,
        connection_id: &str,
        status: ConnectionStatus,
        error: Option<&str>,
    ) {
        let _ = self.ctrl_tx.send(AgentMessage {
            msg: Some(agent_message::Msg::ConnectionUpdate(
                AgentConnectionUpdate {
                    connection_id: connection_id.to_string(),
                    status: status.into(),
                    error_message: error.unwrap_or_default().to_string(),
                },
            )),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn read_file_accepts_prefix_that_appears_after_manager_creation() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("detrix-agent-prefix-{suffix}"));
        let prefix = root.join("mounted");
        let file = prefix.join("source.go");
        let (ctrl_tx, _ctrl_rx) = mpsc::unbounded_channel();
        let (event_tx, _event_rx) = mpsc::channel(1);
        let manager = AdapterManager::new(
            ctrl_tx,
            event_tx,
            Arc::new(AtomicU64::new(0)),
            CaptureConfig::default(),
            Arc::new(AtomicU64::new(0)),
            vec![prefix],
        );

        std::fs::create_dir_all(&file.parent().unwrap()).expect("create simulated mount");
        std::fs::write(&file, b"package main\n").expect("create source");
        assert_eq!(
            manager.read_file(file.to_str().unwrap()).unwrap(),
            b"package main\n"
        );

        std::fs::remove_dir_all(root).expect("remove simulated mount");
    }
}
