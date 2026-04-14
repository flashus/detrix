//! AgentService gRPC implementation
//!
//! Following Clean Architecture: This is a CONTROLLER that does DTO mapping ONLY.
//! ALL business logic is in services::AgentConnectionManager.
//!
//! This is the proto→domain boundary. All proto types are converted here before
//! being passed into the application layer (which speaks only in domain types).

use crate::generated::detrix::v1::agent_service_server::AgentService;
use crate::generated::detrix::v1::*;
use crate::state::ApiState;
use detrix_application::{
    AgentBinaryInfo, AgentCapabilities, AgentConnectionManagerRef, IncomingAgentMessage,
    OutgoingAgentMessage, RegisterResult, VariableInfo,
};
use futures::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::wrappers::{ReceiverStream, UnboundedReceiverStream};
use tonic::{Request, Response, Status, Streaming};

type AgentStream = Pin<Box<dyn Stream<Item = Result<ServerMessage, Status>> + Send>>;

#[derive(Debug, Clone)]
pub struct AgentServiceImpl {
    state: Arc<ApiState>,
}

impl AgentServiceImpl {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }

    fn agent_manager(&self) -> &AgentConnectionManagerRef {
        #[allow(clippy::expect_used)]
        self.state
            .context
            .agent_connection_manager
            .as_ref()
            .expect("AgentConnectionManager not configured")
    }
}

#[tonic::async_trait]
impl AgentService for AgentServiceImpl {
    type ConnectAgentStream = AgentStream;

    async fn connect_agent(
        &self,
        request: Request<Streaming<AgentMessage>>,
    ) -> Result<Response<Self::ConnectAgentStream>, Status> {
        let agent_manager = self.agent_manager().clone();
        let mut incoming = request.into_inner();

        // Create unbounded outgoing channel for server→agent messages.
        // RegisterAck + CreateConnection batch is enqueued synchronously in
        // register_atomic; bounded would risk deadlock.
        let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<OutgoingAgentMessage>();

        // Read first message — MUST be RegisterAgent
        let first_msg = incoming
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("first message must be RegisterAgent"))?;

        let agent_msg = match first_msg.msg {
            Some(agent_message::Msg::Register(reg)) => reg,
            _ => {
                return Err(Status::invalid_argument(
                    "first message must be RegisterAgent",
                ))
            }
        };

        // Convert proto → domain types
        let capabilities = AgentCapabilities {
            ebpf: agent_msg.capabilities.as_ref().is_some_and(|c| c.ebpf),
            dap_python: agent_msg
                .capabilities
                .as_ref()
                .is_some_and(|c| c.dap_python),
            dap_go: agent_msg.capabilities.as_ref().is_some_and(|c| c.dap_go),
            dap_rust: agent_msg.capabilities.as_ref().is_some_and(|c| c.dap_rust),
        };

        let binaries: Vec<AgentBinaryInfo> = agent_msg
            .binaries
            .into_iter()
            .map(|b| AgentBinaryInfo {
                binary_path: b.binary_path,
                pid: b.pid,
                build_info: b.build_info,
                has_dwarf: b.has_dwarf,
                exported_functions: b.exported_functions,
            })
            .collect();

        let agent_id = agent_msg.agent_id;
        let hostname = agent_msg.hostname;
        let agent_version = agent_msg.agent_version;

        // Convert outgoing domain channel → proto stream
        let (proto_tx, proto_rx) = tokio::sync::mpsc::channel(256);

        // Spawn mux task: domain → proto conversion
        let out_rx_stream = UnboundedReceiverStream::new(out_rx);
        tokio::spawn(async move {
            use futures::StreamExt;
            let tx = proto_tx;
            tokio::pin!(out_rx_stream);
            while let Some(msg) = out_rx_stream.next().await {
                let proto_msg = domain_to_proto(msg);
                if tx.send(Ok(proto_msg)).await.is_err() {
                    break; // Receiver dropped
                }
            }
        });

        // Register the agent (sends RegisterAck + CreateConnection via out_tx)
        match agent_manager
            .register_atomic(
                agent_id.clone(),
                hostname,
                agent_version,
                capabilities,
                binaries,
                out_tx,
            )
            .await
        {
            Ok(RegisterResult::Accepted) => {
                tracing::info!(agent_id = %agent_id, "agent registered");
            }
            Ok(RegisterResult::Rejected { reason }) => {
                // RegisterAck{accepted:false} already enqueued by register_atomic.
                // Return stream so agent receives rejection before close.
                tracing::info!(agent_id = %agent_id, reason = %reason, "agent registration rejected (expected)");
                return Ok(Response::new(Box::pin(ReceiverStream::new(proto_rx))));
            }
            Err(e) => {
                tracing::error!(agent_id = %agent_id, error = %e, "agent registration failed");
                return Err(Status::internal(format!("registration failed: {e}")));
            }
        }

        // Spawn read task: incoming proto messages → domain → dispatch
        let mgr_clone = agent_manager.clone();
        let agent_id_clone = agent_id.clone();
        tokio::spawn(async move {
            loop {
                match incoming.message().await {
                    Ok(Some(msg)) => {
                        if let Some(domain_msg) = proto_to_domain(msg) {
                            mgr_clone.dispatch(&agent_id_clone, domain_msg).await;
                        }
                    }
                    Ok(None) => {
                        // Stream closed
                        tracing::info!(agent_id = %agent_id_clone, "agent stream closed");
                        mgr_clone.unregister_agent(&agent_id_clone).await;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(agent_id = %agent_id_clone, error = %e, "agent stream error");
                        mgr_clone.unregister_agent(&agent_id_clone).await;
                        break;
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(proto_rx))))
    }
}

// ============================================================================
// Proto ↔ Domain Conversion
// ============================================================================

fn proto_to_domain(msg: AgentMessage) -> Option<IncomingAgentMessage> {
    match msg.msg? {
        agent_message::Msg::ConnectionUpdate(u) => {
            let status_val = u.status();
            let error_msg = u.error_message.clone();
            Some(IncomingAgentMessage::ConnectionUpdate {
                connection_id: detrix_core::ConnectionId(u.connection_id),
                status: match status_val {
                    ConnectionStatus::Connected => detrix_core::ConnectionStatus::Connected,
                    ConnectionStatus::Disconnected => detrix_core::ConnectionStatus::Disconnected,
                    ConnectionStatus::Failed => {
                        detrix_core::ConnectionStatus::Failed(error_msg.clone())
                    }
                    _ => detrix_core::ConnectionStatus::Disconnected,
                },
                error: if error_msg.is_empty() {
                    None
                } else {
                    Some(error_msg)
                },
            })
        }
        agent_message::Msg::Events(e) => {
            let events: Vec<detrix_core::MetricEvent> = e
                .events
                .into_iter()
                .filter_map(|se| {
                    serde_json::from_str::<detrix_core::MetricEvent>(&se.values_json).ok()
                })
                .collect();
            Some(IncomingAgentMessage::EventBatch {
                connection_id: detrix_core::ConnectionId(e.connection_id),
                events,
            })
        }
        agent_message::Msg::Heartbeat(h) => Some(IncomingAgentMessage::Heartbeat {
            cpu: h.cpu_usage,
            memory_bytes: h.memory_bytes,
            active_probes: h.active_probes,
            uptime_secs: h.uptime_seconds,
            events_forwarded: h.events_forwarded,
            events_dropped: h.events_dropped,
        }),
        agent_message::Msg::FileResponse(f) => Some(IncomingAgentMessage::FileResponse {
            request_id: f.request_id,
            content: f.content,
            error: if f.error.is_empty() {
                None
            } else {
                Some(f.error)
            },
        }),
        agent_message::Msg::InspectResponse(i) => Some(IncomingAgentMessage::InspectResponse {
            request_id: i.request_id,
            variables: i
                .variables
                .into_iter()
                .map(|v| VariableInfo {
                    name: v.name,
                    type_name: v.r#type.clone(),
                    line: v.line,
                })
                .collect(),
            error: if i.error.is_empty() {
                None
            } else {
                Some(i.error)
            },
        }),
        agent_message::Msg::DropCount(d) => Some(IncomingAgentMessage::DropCount {
            connection_id: detrix_core::ConnectionId(d.connection_id),
            total_events_dropped: d.total_events_dropped,
        }),
        agent_message::Msg::SetMetricAck(a) => Some(IncomingAgentMessage::SetMetricAck {
            request_id: a.request_id,
            verified: a.verified,
            actual_line: if a.actual_line == 0 {
                None
            } else {
                Some(a.actual_line)
            },
            error: if a.error.is_empty() {
                None
            } else {
                Some(a.error)
            },
        }),
        agent_message::Msg::RemoveMetricAck(a) => Some(IncomingAgentMessage::RemoveMetricAck {
            request_id: a.request_id,
            confirmed: a.confirmed,
            error: if a.error.is_empty() {
                None
            } else {
                Some(a.error)
            },
        }),
        agent_message::Msg::Pong(_) => Some(IncomingAgentMessage::Pong),
        agent_message::Msg::Error(e) => Some(IncomingAgentMessage::Error {
            code: e.code,
            message: e.message,
        }),
        agent_message::Msg::Register(_) => None, // Should not happen after initial registration
    }
}

fn domain_to_proto(msg: OutgoingAgentMessage) -> ServerMessage {
    use server_message::Msg;
    ServerMessage {
        msg: Some(match msg {
            OutgoingAgentMessage::RegisterAck {
                accepted,
                rejection_reason,
                min_compatible_version,
            } => Msg::RegisterAck(RegisterAck {
                accepted,
                rejection_reason,
                min_compatible_version,
            }),
            OutgoingAgentMessage::CreateConnection {
                connection_id,
                language,
                binary_path,
                host,
                port,
                safe_mode,
            } => Msg::CreateConnection(AgentCreateConnection {
                connection_id,
                language,
                binary_path,
                host,
                port,
                safe_mode,
            }),
            OutgoingAgentMessage::CloseConnection { connection_id } => {
                Msg::CloseConnection(AgentCloseConnection { connection_id })
            }
            OutgoingAgentMessage::SetMetric {
                request_id,
                connection_id,
                metric_name,
                file,
                line,
                expressions,
                enabled,
            } => Msg::SetMetric(SetMetric {
                request_id,
                connection_id,
                metric_name,
                file,
                line,
                expressions,
                enabled,
            }),
            OutgoingAgentMessage::RemoveMetric {
                request_id,
                connection_id,
                metric_name,
            } => Msg::RemoveMetric(RemoveMetric {
                request_id,
                connection_id,
                metric_name,
            }),
            OutgoingAgentMessage::ReadFile {
                request_id,
                connection_id,
                path,
            } => Msg::ReadFile(ReadFile {
                request_id,
                connection_id,
                path,
            }),
            OutgoingAgentMessage::InspectFile {
                request_id,
                connection_id,
                file,
                line,
                find_variable,
            } => Msg::InspectFile(InspectFile {
                request_id,
                connection_id,
                file,
                line,
                find_variable,
            }),
            OutgoingAgentMessage::Ping => Msg::Ping(Ping {}),
        }),
    }
}
