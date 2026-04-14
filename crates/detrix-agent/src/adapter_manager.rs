//! Adapter Manager — manages local adapter instances per connection.

use crate::error::{AgentError, Result};
use crate::proto_convert::{metric_event_to_proto, truncate_values_json};
use detrix_api::generated::detrix::v1::*;
use detrix_core::MetricEvent;
use detrix_logging::{info, warn};
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

    /// Create a new connection.
    pub async fn create_connection(&self, msg: AgentCreateConnection) {
        let connection_id = msg.connection_id.clone();
        info!(connection_id = %connection_id, "Creating connection");

        let _ = self.ctrl_tx.send(AgentMessage {
            msg: Some(agent_message::Msg::ConnectionUpdate(
                AgentConnectionUpdate {
                    connection_id,
                    status: ConnectionStatus::Connected.into(),
                    error_message: String::new(),
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
