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
use detrix_core::{ConnectionId, Location, Metric, MetricEvent, SourceLanguage};
use detrix_dap::{PythonAdapter, RustAdapter};
use detrix_ebpf::{CaptureConfig, EbpfAdapter};
use detrix_logging::{info, warn};
use detrix_ports::DapAdapterRef;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Manages local adapter instances for each connection.
pub struct AdapterManager {
    adapters: Arc<DashMap<String, DapAdapterRef>>,
    ctrl_tx: mpsc::UnboundedSender<AgentMessage>,
    event_tx: mpsc::Sender<AgentMessage>,
    events_dropped: Arc<AtomicU64>,
    capture_config: CaptureConfig,
}

impl AdapterManager {
    pub fn new(
        ctrl_tx: mpsc::UnboundedSender<AgentMessage>,
        event_tx: mpsc::Sender<AgentMessage>,
        events_dropped: Arc<AtomicU64>,
        capture_config: CaptureConfig,
    ) -> Self {
        Self {
            adapters: Arc::new(DashMap::new()),
            ctrl_tx,
            event_tx,
            events_dropped,
            capture_config,
        }
    }

    /// Create a new connection — dispatches by language.
    ///
    /// For Go connections, uses eBPF uprobes.
    /// For Python/Rust, uses DAP adapters connecting to local debuggers.
    pub async fn create_connection(&self, msg: AgentCreateConnection) {
        let connection_id = msg.connection_id.clone();
        let language = msg.language.to_lowercase();

        info!(
            connection_id = %connection_id,
            language = %language,
            "Creating connection"
        );

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
                let config = PythonAdapter::default_config(msg.port as u16).with_host(&msg.host);
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
                let config = RustAdapter::default_config(msg.port as u16).with_host(&msg.host);
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

    /// Handle SetMetric.
    pub async fn handle_set_metric(&self, msg: SetMetric) {
        let connection_id = msg.connection_id.clone();
        let request_id = msg.request_id.clone();

        let metric = match proto_set_metric_to_metric(&msg) {
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
            language: SourceLanguage::Go,
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

    /// Close a connection.
    pub async fn close_connection(&self, connection_id: &str) {
        info!(connection_id, "Closing connection");
        if let Some((_, adapter)) = self.adapters.remove(connection_id) {
            let _ = adapter.stop().await;
        }
    }

    /// Stop all adapters.
    pub async fn stop_all(&self) {
        let keys: Vec<String> = self.adapters.iter().map(|e| e.key().clone()).collect();
        for key in keys {
            if let Some((_, adapter)) = self.adapters.remove(&key) {
                let _ = adapter.stop().await;
            }
        }
    }

    /// Read a file from the local filesystem.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        std::fs::read(path).map_err(|e| AgentError::Scanner(format!("Cannot read {path}: {e}")))
    }

    /// Inspect a binary for variable information.
    pub async fn inspect_file(&self, msg: InspectFile) -> InspectResponse {
        InspectResponse {
            request_id: msg.request_id,
            variables: Vec::new(),
            error: "Not yet implemented".to_string(),
        }
    }

    /// Forward a batch of metric events to the server.
    pub async fn forward_events(&self, connection_id: &str, events: Vec<MetricEvent>) {
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

        match self.event_tx.try_send(batch_msg) {
            Ok(()) => {}
            Err(_) => {
                let count = self.events_dropped.fetch_add(1, Ordering::Relaxed) + 1;
                warn!(
                    connection_id = %connection_id,
                    dropped = count,
                    "Event channel full, dropping batch"
                );
                let _ = self.ctrl_tx.send(AgentMessage {
                    msg: Some(agent_message::Msg::DropCount(DropCountUpdate {
                        connection_id: connection_id.to_string(),
                        total_events_dropped: count,
                    })),
                });
            }
        }
    }

    fn spawn_event_forwarder(
        &self,
        connection_id: String,
        mut event_rx: mpsc::Receiver<MetricEvent>,
    ) {
        let event_tx = self.event_tx.clone();
        let ctrl_tx = self.ctrl_tx.clone();
        let dropped = Arc::clone(&self.events_dropped);
        let connection_id_clone = connection_id.clone();
        tokio::spawn(async move {
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
                        &dropped,
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
    }

    async fn forward_batch(
        event_tx: &mpsc::Sender<AgentMessage>,
        ctrl_tx: &mpsc::UnboundedSender<AgentMessage>,
        dropped: &Arc<AtomicU64>,
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

        if event_tx.try_send(batch_msg).is_err() {
            let count =
                dropped.fetch_add(events.len() as u64, Ordering::Relaxed) + events.len() as u64;
            warn!(
                connection_id = %connection_id,
                dropped = count,
                batch_size = events.len(),
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
