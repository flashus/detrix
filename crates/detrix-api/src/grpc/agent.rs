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
use detrix_core::ExpressionValue;
use futures::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::wrappers::{ReceiverStream, UnboundedReceiverStream};
use tonic::{Request, Response, Status, Streaming};
use tracing::warn;

type AgentStream = Pin<Box<dyn Stream<Item = Result<ServerMessage, Status>> + Send>>;

#[derive(Debug, Clone)]
pub struct AgentServiceImpl {
    state: Arc<ApiState>,
}

impl AgentServiceImpl {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }

    fn agent_manager(&self) -> Result<&AgentConnectionManagerRef, tonic::Status> {
        self.state
            .context
            .agent_connection_manager
            .as_ref()
            .ok_or_else(|| tonic::Status::unimplemented("Agent service not configured"))
    }
}

#[tonic::async_trait]
impl AgentService for AgentServiceImpl {
    type ConnectAgentStream = AgentStream;

    async fn connect_agent(
        &self,
        request: Request<Streaming<AgentMessage>>,
    ) -> Result<Response<Self::ConnectAgentStream>, Status> {
        let agent_manager = self.agent_manager()?.clone();
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
            supported_envelope_schemas: agent_msg
                .capabilities
                .as_ref()
                .map(|c| c.supported_envelope_schemas.clone())
                .unwrap_or_default(),
            supported_capture_profiles: agent_msg
                .capabilities
                .as_ref()
                .map(|c| c.supported_capture_profiles.clone())
                .unwrap_or_default(),
            max_capture_payload_bytes: agent_msg
                .capabilities
                .as_ref()
                .map(|c| c.max_capture_payload_bytes)
                .unwrap_or_default(),
        };

        let binaries: Vec<AgentBinaryInfo> = agent_msg
            .binaries
            .into_iter()
            .map(|b| AgentBinaryInfo {
                binary_path: b.binary_path,
                pid: b.pid,
                inode: b.inode,
                build_info: b.build_info,
                has_dwarf: b.has_dwarf,
                exported_functions: b.exported_functions,
                language: match b.language.as_str() {
                    "rust" => detrix_core::SourceLanguage::Rust,
                    _ => detrix_core::SourceLanguage::Go,
                },
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
                tracing::debug!(kind = ?std::mem::discriminant(&msg), "Sending server command to agent stream");
                let proto_msg = domain_to_proto(msg);
                if tx.send(Ok(proto_msg)).await.is_err() {
                    break; // Receiver dropped
                }
            }
        });

        // Register the agent (sends RegisterAck + CreateConnection via out_tx)
        // Retain this sender as the stream identity. If the same agent_id
        // reconnects, the old stream must not unregister the replacement.
        let session_tx = out_tx.clone();
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
            // Limit concurrent dispatch tasks per stream to prevent unbounded
            // task spawning from a misbehaving or flooding agent.
            let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(64));
            loop {
                match incoming.message().await {
                    Ok(Some(msg)) => {
                        tracing::debug!(
                            agent_id = %agent_id_clone,
                            kind = ?msg.msg.as_ref().map(|m| std::mem::discriminant(m)),
                            "Agent message received on stream"
                        );
                        if let Some(domain_msg) = proto_to_domain(msg) {
                            // Dispatch on a separate task so handlers that await
                            // request/response work over the same gRPC stream
                            // do not block the read loop from receiving the ack.
                            let permit = semaphore.clone().acquire_owned().await.ok();
                            let mgr = mgr_clone.clone();
                            let agent_id = agent_id_clone.clone();
                            let session_tx = session_tx.clone();
                            tokio::spawn(async move {
                                let _permit = permit; // released when task drops
                                mgr.dispatch(&agent_id, &session_tx, domain_msg).await;
                            });
                        }
                    }
                    Ok(None) => {
                        // Stream closed
                        tracing::info!(agent_id = %agent_id_clone, "agent stream closed");
                        mgr_clone
                            .unregister_agent_if_current(&agent_id_clone, &session_tx)
                            .await;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(agent_id = %agent_id_clone, error = %e, "agent stream error");
                        mgr_clone
                            .unregister_agent_if_current(&agent_id_clone, &session_tx)
                            .await;
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
                selected_backend: u.selected_backend,
                capture_profile: u.capture_profile,
                backend_reason: u.backend_reason,
                debug_image_source: u.debug_image_source,
                failure_class: u.failure_class,
                supported_envelope_schemas: u.supported_envelope_schemas,
                supported_capture_profiles: u.supported_capture_profiles,
                max_capture_payload_bytes: u.max_capture_payload_bytes,
                target_architecture: u.target_architecture,
            })
        }
        agent_message::Msg::Events(e) => {
            let events: Vec<detrix_core::MetricEvent> = e
                .events
                .into_iter()
                .map(|se| {
                    let values = match serde_json::from_str::<Vec<ExpressionValue>>(&se.values_json)
                    {
                        Ok(values) => values,
                        Err(err) => {
                            warn!(
                                metric_id = se.metric_id,
                                metric_name = %se.metric_name,
                                error = %err,
                                "Failed to decode agent metric event values_json"
                            );
                            Vec::new()
                        }
                    };

                    detrix_core::MetricEvent {
                        id: None,
                        metric_id: detrix_core::MetricId(se.metric_id),
                        metric_name: se.metric_name,
                        connection_id: detrix_core::ConnectionId(e.connection_id.clone()),
                        timestamp: se.timestamp_ns / 1_000,
                        thread_name: if se.thread_name.is_empty() {
                            None
                        } else {
                            Some(se.thread_name)
                        },
                        thread_id: if se.thread_id == 0 {
                            None
                        } else {
                            Some(se.thread_id)
                        },
                        values,
                        is_error: se.is_error,
                        error_type: None,
                        error_message: if se.error_message.is_empty() {
                            None
                        } else {
                            Some(se.error_message)
                        },
                        request_id: None,
                        session_id: None,
                        stack_trace: None,
                        memory_snapshot: None,
                    }
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
            kernel_events_dropped: d.kernel_events_dropped,
            decode_events_dropped: d.decode_events_dropped,
            unavailable_fields: d.unavailable_fields,
            events_decoded: d.events_decoded,
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
        agent_message::Msg::Register(reg) => {
            // Mid-session re-registration: scanner detected binary changes.
            let binaries = reg
                .binaries
                .into_iter()
                .map(|b| AgentBinaryInfo {
                    binary_path: b.binary_path,
                    pid: b.pid,
                    inode: b.inode,
                    build_info: b.build_info,
                    has_dwarf: b.has_dwarf,
                    exported_functions: b.exported_functions,
                    // Re-registration carries the complete scanner snapshot,
                    // including the language selected by the agent.  Do not
                    // silently coerce every replacement binary to Go: doing
                    // so changes the connection identity for Rust targets
                    // and prevents lifecycle reconciliation from matching the
                    // original connection semantics.
                    language: match b.language.as_str() {
                        "rust" => detrix_core::SourceLanguage::Rust,
                        "python" => detrix_core::SourceLanguage::Python,
                        _ => detrix_core::SourceLanguage::Go,
                    },
                })
                .collect();
            Some(IncomingAgentMessage::RegisterUpdate { binaries })
        }
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
                capture_backend,
                capture_profile,
                debug_info_path,
            } => Msg::CreateConnection(AgentCreateConnection {
                connection_id,
                language,
                binary_path,
                host,
                port,
                safe_mode,
                capture_backend,
                capture_profile,
                debug_info_path,
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
                metric_id,
            } => Msg::SetMetric(SetMetric {
                request_id,
                connection_id,
                metric_name,
                file,
                line,
                expressions,
                enabled,
                metric_id,
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
