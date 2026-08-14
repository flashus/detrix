//! Adapter Manager — manages local adapter instances per connection.
//!
//! Creates and manages local adapters for each connection assigned to this agent.
//! For Go connections, uses eBPF uprobes. Rust may opt into scalar eBPF; DAP remains the default.
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
use detrix_dap::{GoAdapter, PythonAdapter, RustAdapter};
use detrix_ebpf::{
    resolve_backend, BackendDecision, CaptureBackend, CaptureConfig, EbpfAdapterFactory, ProfileId,
};
use detrix_logging::{debug, info, warn};
use detrix_ports::DapAdapterRef;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// Registry-backed eBPF construction for all supported profiles.
    ebpf_factory: EbpfAdapterFactory,
    /// Per-connection drop counters — used for accurate DropCountUpdate messages.
    connection_drop_counts: DashMap<String, Arc<AtomicU64>>,
    /// Language string per connection, populated from AgentCreateConnection.language.
    connection_languages: DashMap<String, String>,
    /// Immutable backend/profile decision for connection diagnostics and
    /// lifecycle correlation. Replaced atomically on reconnect.
    connection_decisions: DashMap<String, BackendDecision>,
    /// Selected debug-image provenance (`embedded`, `external`, `split`, or
    /// `missing`) for connection diagnostics.
    connection_debug_sources: DashMap<String, String>,
    /// Forwarder task handles — tracked so they can be aborted on close_connection.
    forwarder_handles: DashMap<String, JoinHandle<()>>,
    /// Events received by the forwarding stage from an adapter.
    pub events_received: Arc<AtomicU64>,
    /// Events received but not yet accounted as forwarded or dropped.
    pub events_in_flight: Arc<AtomicU64>,
    /// Forwarded-events counter — incremented by forward_batch on success.
    pub events_forwarded: Arc<AtomicU64>,
    pub events_decoded: Arc<AtomicU64>,
    pub kernel_events_dropped: Arc<AtomicU64>,
    pub decode_events_dropped: Arc<AtomicU64>,
    /// Requests an immediate full scanner snapshot after the server closes a
    /// target connection. This closes the race where a replacement process is
    /// born between two delta scans and would otherwise remain undiscovered.
    scan_refresh_requested: Arc<AtomicBool>,
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
        events_received: Arc<AtomicU64>,
        events_in_flight: Arc<AtomicU64>,
        events_forwarded: Arc<AtomicU64>,
        events_decoded: Arc<AtomicU64>,
        kernel_events_dropped: Arc<AtomicU64>,
        decode_events_dropped: Arc<AtomicU64>,
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
            ebpf_factory: EbpfAdapterFactory::new_with_config("/", capture_config.clone()),
            connection_drop_counts: DashMap::new(),
            connection_languages: DashMap::new(),
            connection_decisions: DashMap::new(),
            connection_debug_sources: DashMap::new(),
            forwarder_handles: DashMap::new(),
            events_received,
            events_in_flight,
            events_forwarded,
            events_decoded,
            kernel_events_dropped,
            decode_events_dropped,
            scan_refresh_requested: Arc::new(AtomicBool::new(false)),
            allowed_read_prefixes,
        }
    }

    pub fn scan_refresh_signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.scan_refresh_requested)
    }

    /// Create a new connection — dispatches by language.
    ///
    /// For Go connections, uses eBPF uprobes.
    /// DAP remains the default for Python/Rust unless Rust explicitly requests eBPF.
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
        let requested_profile = match infer_requested_profile(&language, &msg.capture_profile) {
            Ok(profile) => profile,
            Err(error) => {
                self.send_connection_update(&connection_id, ConnectionStatus::Failed, Some(&error))
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

        // Resolve the backend once before constructing any adapter. This makes
        // `auto`/`dap`/`ebpf` semantics transactional and keeps platform
        // fallback decisions out of the language-specific branches below.
        let backend_decision = if matches!(language.as_str(), "go" | "rust") {
            let debug_path = (!msg.debug_info_path.trim().is_empty())
                .then(|| PathBuf::from(&msg.debug_info_path));
            let (debug_ready, debug_source) = if requested_backend != CaptureBackend::Dap
                && EbpfAdapterFactory::is_available()
                && !msg.binary_path.trim().is_empty()
            {
                match self.ebpf_factory.preflight_debug_image(
                    requested_profile,
                    PathBuf::from(&msg.binary_path),
                    debug_path.as_deref(),
                ) {
                    Ok(metadata) => (true, format!("{:?}", metadata.source).to_ascii_lowercase()),
                    Err(_) => (false, "missing".into()),
                }
            } else {
                (false, "missing".into())
            };
            self.connection_debug_sources
                .insert(connection_id.clone(), debug_source);
            match resolve_backend(
                requested_backend,
                requested_profile,
                EbpfAdapterFactory::is_available(),
                debug_ready,
            ) {
                Ok(decision) => decision,
                Err(error) => {
                    self.send_connection_update(
                        &connection_id,
                        ConnectionStatus::Failed,
                        Some(&error.to_string()),
                    )
                    .await;
                    return;
                }
            }
        } else {
            if requested_backend == CaptureBackend::Ebpf {
                self.send_connection_update(
                    &connection_id,
                    ConnectionStatus::Failed,
                    Some("eBPF backend is unsupported for this language"),
                )
                .await;
                return;
            }
            BackendDecision {
                requested: requested_backend,
                selected: CaptureBackend::Dap,
                profile: requested_profile,
                reason: "language has no registered eBPF profile".into(),
            }
        };
        let selected_backend = backend_decision.selected;

        info!(
            connection_id = %connection_id,
            language = %language,
            capture_backend = ?requested_backend,
            capture_profile = ?requested_profile,
            "Creating connection"
        );

        // CreateConnection is normally serialized by the stream reader, but a
        // duplicate command can still arrive after a reconnect or retry. Stop
        // the previous local instance before replacing it so its probe and
        // forwarder do not leak events/resources under the same ID.
        self.replace_connection(&connection_id).await;

        // Record language for later use in handle_set_metric / handle_remove_metric.
        self.connection_languages
            .insert(connection_id.clone(), language.clone());
        self.connection_decisions
            .insert(connection_id.clone(), backend_decision.clone());
        // Initialise per-connection drop counter.
        self.connection_drop_counts
            .insert(connection_id.clone(), Arc::new(AtomicU64::new(0)));

        // All eBPF profiles share one construction/lifecycle path.  Language
        // branches below are reserved for DAP compatibility adapters; adding
        // a new eBPF profile therefore only requires registry/profile work.
        if selected_backend == CaptureBackend::Ebpf {
            let binary_path = PathBuf::from(&msg.binary_path);
            let debug_path = (!msg.debug_info_path.trim().is_empty())
                .then(|| PathBuf::from(&msg.debug_info_path));
            let adapter_result = self.ebpf_factory.create_adapter_with_debug_path(
                requested_profile,
                &binary_path,
                debug_path.as_deref(),
            );
            match adapter_result {
                Ok(adapter) => {
                    if let Err(error) = adapter.start().await {
                        warn!(connection_id = %connection_id, error = %error, "Failed to start eBPF adapter");
                        self.send_connection_update(
                            &connection_id,
                            ConnectionStatus::Failed,
                            Some(&error.to_string()),
                        )
                        .await;
                        return;
                    }
                    match adapter.subscribe_events().await {
                        Ok(event_rx) => {
                            let forward_adapter = adapter.clone();
                            self.adapters.insert(connection_id.clone(), adapter);
                            self.spawn_event_forwarder(
                                connection_id.clone(),
                                event_rx,
                                forward_adapter,
                            );
                            self.send_connection_update(
                                &connection_id,
                                ConnectionStatus::Connected,
                                None,
                            )
                            .await;
                        }
                        Err(error) => {
                            warn!(connection_id = %connection_id, error = %error, "Failed to subscribe eBPF adapter");
                            self.send_connection_update(
                                &connection_id,
                                ConnectionStatus::Failed,
                                Some(&error.to_string()),
                            )
                            .await;
                        }
                    }
                }
                Err(error) => {
                    warn!(connection_id = %connection_id, error = %error, "Failed to create eBPF adapter");
                    self.send_connection_update(
                        &connection_id,
                        ConnectionStatus::Failed,
                        Some(&error.to_string()),
                    )
                    .await;
                }
            }
            return;
        }

        match language.as_str() {
            "go" => {
                let binary_path = PathBuf::from(&msg.binary_path);
                // Preserve the compatibility default (Go + auto => eBPF), but
                // make an explicit DAP request authoritative. Previously the
                // requested backend was parsed and then ignored here, which
                // made `capture_backend=dap` unexpectedly attach a uprobe.
                let adapter_result: std::result::Result<DapAdapterRef, String> =
                    match selected_backend {
                        CaptureBackend::Dap => {
                            let port = match u16::try_from(msg.port) {
                                Ok(port) => port,
                                Err(_) => {
                                    self.send_connection_update(
                                        &connection_id,
                                        ConnectionStatus::Failed,
                                        Some(&format!("port {} out of valid range", msg.port)),
                                    )
                                    .await;
                                    return;
                                }
                            };
                            let config = GoAdapter::default_config(port).with_host(&msg.host);
                            Ok(Arc::new(GoAdapter::new(config, PathBuf::from("/")))
                                as DapAdapterRef)
                        }
                        CaptureBackend::Auto => {
                            let debug_path = (!msg.debug_info_path.trim().is_empty())
                                .then(|| PathBuf::from(&msg.debug_info_path));
                            self.ebpf_factory
                                .create_adapter_with_debug_path(
                                    requested_profile,
                                    &binary_path,
                                    debug_path.as_deref(),
                                )
                                .map_err(|error| error.to_string())
                        }
                        CaptureBackend::Ebpf => unreachable!(
                            "eBPF connections are handled by the shared registry path above"
                        ),
                    };
                match adapter_result {
                    Ok(adapter) => {
                        if let Err(e) = adapter.start().await {
                            warn!("Failed to start Go adapter: {e}");
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
                                let forward_adapter = adapter.clone();
                                self.adapters.insert(connection_id.clone(), adapter);
                                self.spawn_event_forwarder(
                                    connection_id.clone(),
                                    event_rx,
                                    forward_adapter,
                                );
                                self.send_connection_update(
                                    &connection_id,
                                    ConnectionStatus::Connected,
                                    None,
                                )
                                .await;
                            }
                            Err(e) => {
                                warn!("Failed to subscribe events from Go adapter: {e}");
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
                        warn!("Failed to create Go adapter: {e}");
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
                        let forward_adapter = adapter.clone();
                        self.adapters.insert(connection_id.clone(), adapter);
                        self.spawn_event_forwarder(
                            connection_id.clone(),
                            event_rx,
                            forward_adapter,
                        );
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
                        let forward_adapter = adapter.clone();
                        self.adapters.insert(connection_id.clone(), adapter);
                        self.spawn_event_forwarder(
                            connection_id.clone(),
                            event_rx,
                            forward_adapter,
                        );
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

    /// Atomically tear down the local resources for a connection ID before a
    /// retry/reconnect replaces it.  Keeping this operation in one seam makes
    /// duplicate CreateConnection commands testable and prevents a stale
    /// forwarder from surviving a new adapter.
    async fn replace_connection(&self, connection_id: &str) {
        if let Some((_, handle)) = self.forwarder_handles.remove(connection_id) {
            handle.abort();
        }
        if let Some((_, adapter)) = self.adapters.remove(connection_id) {
            let _ = adapter.stop().await;
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
        self.scan_refresh_requested.store(true, Ordering::Release);
        // Abort the forwarder task first so it stops producing events.
        if let Some((_, handle)) = self.forwarder_handles.remove(connection_id) {
            handle.abort();
        }
        if let Some((_, adapter)) = self.adapters.remove(connection_id) {
            let _ = adapter.stop().await;
        }
        self.connection_languages.remove(connection_id);
        self.connection_decisions.remove(connection_id);
        self.connection_debug_sources.remove(connection_id);
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
        self.connection_decisions.clear();
        self.connection_debug_sources.clear();
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
        adapter: DapAdapterRef,
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
        let events_received = Arc::clone(&self.events_received);
        let events_in_flight = Arc::clone(&self.events_in_flight);
        let events_forwarded = Arc::clone(&self.events_forwarded);
        let events_decoded = Arc::clone(&self.events_decoded);
        let kernel_events_dropped = Arc::clone(&self.kernel_events_dropped);
        let decode_events_dropped = Arc::clone(&self.decode_events_dropped);
        let connection_id_clone = connection_id.clone();
        let handle = tokio::spawn(async move {
            let mut batch = Vec::with_capacity(64);
            let mut last_kernel_drops = 0u64;
            let mut last_decode_drops = 0u64;
            let mut last_unavailable_fields = 0u64;
            let mut last_decoded_events = 0u64;
            let mut last_drop_report = tokio::time::Instant::now();
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
                                events_received.fetch_add(1, Ordering::Relaxed);
                                events_in_flight.fetch_add(1, Ordering::Relaxed);
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
                        &events_in_flight,
                        &connection_id_clone,
                        events,
                    )
                    .await;
                }

                if last_drop_report.elapsed() >= Duration::from_secs(1) {
                    let kernel_drops = adapter.get_total_drop_count().unwrap_or(0);
                    let decode_drops = adapter.get_decode_drop_count().unwrap_or(0);
                    let unavailable_fields = adapter.get_unavailable_field_count().unwrap_or(0);
                    let decoded_events = adapter.get_decoded_event_count().unwrap_or(0);
                    if kernel_drops > last_kernel_drops
                        || decode_drops > last_decode_drops
                        || unavailable_fields > last_unavailable_fields
                        || decoded_events > last_decoded_events
                    {
                        kernel_events_dropped.fetch_add(
                            kernel_drops.saturating_sub(last_kernel_drops),
                            Ordering::Relaxed,
                        );
                        decode_events_dropped.fetch_add(
                            decode_drops.saturating_sub(last_decode_drops),
                            Ordering::Relaxed,
                        );
                        events_decoded.fetch_add(
                            decoded_events.saturating_sub(last_decoded_events),
                            Ordering::Relaxed,
                        );
                        let _ = ctrl_tx.send(AgentMessage {
                            msg: Some(agent_message::Msg::DropCount(DropCountUpdate {
                                connection_id: connection_id_clone.clone(),
                                total_events_dropped: 0,
                                kernel_events_dropped: kernel_drops,
                                decode_events_dropped: decode_drops,
                                unavailable_fields,
                                events_decoded: decoded_events,
                            })),
                        });
                        last_kernel_drops = kernel_drops;
                        last_decode_drops = decode_drops;
                        last_unavailable_fields = unavailable_fields;
                        last_decoded_events = decoded_events;
                    }
                    last_drop_report = tokio::time::Instant::now();
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
        events_in_flight: &Arc<AtomicU64>,
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
                    kernel_events_dropped: 0,
                    decode_events_dropped: 0,
                    unavailable_fields: 0,
                    events_decoded: 0,
                })),
            });
        }
        events_in_flight.fetch_sub(event_count, Ordering::Relaxed);
    }

    async fn send_connection_update(
        &self,
        connection_id: &str,
        status: ConnectionStatus,
        error: Option<&str>,
    ) {
        let (selected_backend, capture_profile, backend_reason) = self
            .connection_decisions
            .get(connection_id)
            .map(|decision| {
                (
                    format!("{:?}", decision.selected).to_ascii_lowercase(),
                    decision.profile.as_str().to_string(),
                    decision.reason.clone(),
                )
            })
            .unwrap_or_default();
        let debug_image_source = self
            .connection_debug_sources
            .get(connection_id)
            .map(|value| value.clone())
            .unwrap_or_default();
        let decision = self.connection_decisions.get(connection_id);
        let (supported_envelope_schemas, supported_capture_profiles, max_capture_payload_bytes) =
            supported_capture_capabilities(decision.as_deref(), &capture_profile);
        let _ = self.ctrl_tx.send(AgentMessage {
            msg: Some(agent_message::Msg::ConnectionUpdate(
                AgentConnectionUpdate {
                    connection_id: connection_id.to_string(),
                    status: status.into(),
                    error_message: error.unwrap_or_default().to_string(),
                    selected_backend,
                    capture_profile,
                    backend_reason,
                    debug_image_source,
                    failure_class: classify_failure(error),
                    supported_envelope_schemas,
                    supported_capture_profiles,
                    max_capture_payload_bytes,
                    target_architecture: target_architecture_name().into(),
                },
            )),
        });
    }
}

fn target_architecture_name() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        "x86_64"
    }
    #[cfg(target_arch = "aarch64")]
    {
        "aarch64"
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        "unknown"
    }
}

fn supported_capture_capabilities(
    decision: Option<&BackendDecision>,
    capture_profile: &str,
) -> (Vec<u32>, Vec<String>, u32) {
    if decision.is_some_and(|decision| decision.selected == CaptureBackend::Ebpf)
        && capture_profile == "rust"
    {
        (vec![1], vec!["rust".to_string()], 4096)
    } else {
        (Vec::new(), Vec::new(), 0)
    }
}

/// Resolve the profile once at the connection boundary. Unknown source
/// languages fail closed rather than inheriting Go's runtime/layout policy.
/// This is the admission seam for future registry-provided profiles.
fn infer_requested_profile(
    language: &str,
    requested: &str,
) -> std::result::Result<ProfileId, String> {
    let language = language.trim().to_ascii_lowercase();
    let requested = requested.trim().to_ascii_lowercase();
    let profile = match requested.as_str() {
        "" => match language.as_str() {
            "go" => ProfileId::Go,
            "rust" => ProfileId::Rust,
            other => return Err(format!("unsupported source language/profile: {other}")),
        },
        "go" => ProfileId::Go,
        "rust" => ProfileId::Rust,
        other => return Err(format!("unsupported capture profile: {other}")),
    };
    if (language == "go" && profile != ProfileId::Go)
        || (language == "rust" && profile != ProfileId::Rust)
    {
        return Err("capture_profile does not match connection language".into());
    }
    Ok(profile)
}

fn classify_failure(error: Option<&str>) -> String {
    let Some(error) = error else {
        return String::new();
    };
    let text = error.to_ascii_lowercase();
    if text.contains("unsupported platform") {
        return "unsupported_platform".into();
    }
    if text.contains("missing debug") || text.contains("dwarf") {
        return "missing_debug_info".into();
    }
    if text.contains("unsupported abi") {
        return "unsupported_abi".into();
    }
    if text.contains("unsupported profile") || text.contains("unsupported language") {
        return "unsupported_profile".into();
    }
    if text.contains("plan") && (text.contains("reject") || text.contains("invalid")) {
        return "plan_rejected".into();
    }
    if text.contains("verifier") {
        return "verifier_rejected".into();
    }
    if text.contains("attach") {
        return "attach_failed".into();
    }
    if text.contains("decode") || text.contains("ring buffer") {
        return "decode_failed".into();
    }
    if text.contains("target exited") {
        return "target_exited".into();
    }
    "adapter_error".into()
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
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
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

    #[test]
    fn dap_rust_does_not_advertise_ebpf_envelope_capabilities() {
        let decision = BackendDecision {
            requested: CaptureBackend::Auto,
            selected: CaptureBackend::Dap,
            profile: ProfileId::Rust,
            reason: "fallback".into(),
        };
        assert_eq!(
            supported_capture_capabilities(Some(&decision), "rust"),
            (Vec::new(), Vec::new(), 0)
        );
    }

    #[test]
    fn explicit_rust_ebpf_advertises_versioned_capabilities() {
        let decision = BackendDecision {
            requested: CaptureBackend::Ebpf,
            selected: CaptureBackend::Ebpf,
            profile: ProfileId::Rust,
            reason: "explicit".into(),
        };
        assert_eq!(
            supported_capture_capabilities(Some(&decision), "rust"),
            (vec![1], vec!["rust".to_string()], 4096)
        );
    }

    #[test]
    fn unknown_language_does_not_fall_back_to_go_profile() {
        let error =
            infer_requested_profile("zig", "").expect_err("unknown language must fail closed");
        assert!(error.contains("unsupported source language/profile: zig"));
    }

    #[test]
    fn explicit_profile_must_match_builtin_language() {
        let error = infer_requested_profile("go", "rust").expect_err("mismatched profile");
        assert_eq!(error, "capture_profile does not match connection language");
    }

    #[tokio::test]
    async fn duplicate_connection_replaces_old_forwarder() {
        let (ctrl_tx, _ctrl_rx) = mpsc::unbounded_channel();
        let (event_tx, _event_rx) = mpsc::channel(1);
        let manager = AdapterManager::new(
            ctrl_tx,
            event_tx,
            Arc::new(AtomicU64::new(0)),
            CaptureConfig::default(),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Vec::new(),
        );
        let handle = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        manager.forwarder_handles.insert("duplicate".into(), handle);
        manager.replace_connection("duplicate").await;
        assert!(manager.forwarder_handles.get("duplicate").is_none());
    }
}
