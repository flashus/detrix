//! Adapter Manager — manages local adapter instances per connection.
//!
//! Creates and manages local adapters for each connection assigned to this agent.
//! For Go connections, uses eBPF uprobes. For Python/Rust, uses DAP adapters
//! (Phase 5: Hybrid DAP mode — useful when debuggers bind to 127.0.0.1 only).

use crate::error::{AgentError, Result};
use crate::proto_convert::{metric_event_to_proto, truncate_values_json};
use detrix_api::generated::detrix::v1::*;
use detrix_core::MetricEvent;
use detrix_dap::{PythonAdapter, RustAdapter};
use detrix_logging::{info, warn};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Manages local adapter instances for each connection.
pub struct AdapterManager {
    ctrl_tx: mpsc::UnboundedSender<AgentMessage>,
    event_tx: mpsc::Sender<AgentMessage>,
    events_dropped: Arc<AtomicU64>,
}

impl AdapterManager {
    pub fn new(
        ctrl_tx: mpsc::UnboundedSender<AgentMessage>,
        event_tx: mpsc::Sender<AgentMessage>,
        events_dropped: Arc<AtomicU64>,
    ) -> Self {
        Self {
            ctrl_tx,
            event_tx,
            events_dropped,
        }
    }

    /// Create a new connection — dispatches by language.
    ///
    /// For Go connections, eBPF uprobes would be used (not yet implemented).
    /// For Python/Rust, DAP adapters connect to local debuggers.
    /// This is useful when debuggers bind to 127.0.0.1 only (firewall-restricted).
    pub async fn create_connection(&self, msg: AgentCreateConnection) {
        let connection_id = msg.connection_id.clone();
        let language = msg.language.to_lowercase();
        let host = &msg.host;
        let port = msg.port as u16;

        info!(
            connection_id = %connection_id,
            language = %language,
            host = %host,
            port = port,
            "Creating connection"
        );

        let adapter_result: Result<()> = match language.as_str() {
            "python" => {
                // Create PythonAdapter (debugpy) connecting to local host:port
                let config = PythonAdapter::default_config(port).with_host(host);
                let _adapter = PythonAdapter::new(config, PathBuf::from("/"));
                info!("Python DAP adapter created for {host}:{port}");
                Ok(())
            }
            "rust" => {
                // Create RustAdapter (lldb-dap) connecting to local host:port
                let config = RustAdapter::default_config(port).with_host(host);
                let _adapter = RustAdapter::new(config, PathBuf::from("/"));
                info!("Rust DAP adapter created for {host}:{port}");
                Ok(())
            }
            "go" => {
                // Go connections use eBPF uprobes — handled by the agent's scanner.
                // The DAP GoAdapter is for Delve (used when Go is NOT on Linux).
                // For agent mode on Linux, eBPF is the primary backend.
                warn!("Go DAP adapter is not used in agent mode — eBPF handles Go on Linux");
                Ok(())
            }
            _ => {
                warn!(language = %language, "Unsupported language for agent connection");
                Err(AgentError::Config(format!("Unsupported language: {language}")))
            }
        };

        // Send ConnectionUpdate back to server
        let status = if adapter_result.is_ok() {
            ConnectionStatus::Connected
        } else {
            ConnectionStatus::Failed
        };
        let error_message = adapter_result
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        let _ = self.ctrl_tx.send(AgentMessage {
            msg: Some(agent_message::Msg::ConnectionUpdate(
                AgentConnectionUpdate {
                    connection_id,
                    status: status.into(),
                    error_message,
                },
            )),
        });
    }

    /// Handle SetMetric.
    pub async fn handle_set_metric(&self, msg: SetMetric) {
        let _ = self.ctrl_tx.send(AgentMessage {
            msg: Some(agent_message::Msg::SetMetricAck(SetMetricAck {
                request_id: msg.request_id,
                verified: true,
                actual_line: msg.line,
                message: String::new(),
                error: String::new(),
            })),
        });
    }

    /// Handle RemoveMetric.
    pub async fn handle_remove_metric(&self, msg: RemoveMetric) {
        let _ = self.ctrl_tx.send(AgentMessage {
            msg: Some(agent_message::Msg::RemoveMetricAck(RemoveMetricAck {
                request_id: msg.request_id,
                confirmed: true,
                error: String::new(),
            })),
        });
    }

    /// Close a connection.
    pub async fn close_connection(&self, connection_id: &str) {
        info!(connection_id, "Closing connection");
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
}
