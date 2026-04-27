//! Agent — main struct, run loop, and reconnect logic.

use crate::adapter_manager::AdapterManager;
use crate::error::{AgentError, Result};
use crate::metrics_server::MetricsState;
use crate::scanner::{binary_info_to_proto, ProcScanner};
use detrix_api::generated::detrix::v1::agent_service_client::AgentServiceClient;
use detrix_api::generated::detrix::v1::{
    agent_message, server_message, AgentCapabilities, AgentMessage, FileResponse, Heartbeat,
    InspectResponse, Pong, RegisterAgent,
};
use detrix_config::AgentConfig;
use detrix_ebpf::CaptureConfig;
use detrix_logging::{error, info, warn};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::Request;

/// gRPC interceptor that attaches a Bearer token to every outgoing request.
struct AgentTokenInterceptor {
    token: Option<String>,
}

impl tonic::service::Interceptor for AgentTokenInterceptor {
    fn call(
        &mut self,
        mut req: tonic::Request<()>,
    ) -> std::result::Result<tonic::Request<()>, tonic::Status> {
        if let Some(ref token) = self.token {
            let val = format!("Bearer {token}")
                .parse()
                .map_err(|_| tonic::Status::internal("invalid bearer token value"))?;
            req.metadata_mut().insert("authorization", val);
        }
        Ok(req)
    }
}

/// The Agent connects to the server and manages local adapters.
pub struct Agent {
    config: AgentConfig,
    capture_config: CaptureConfig,
}

impl Agent {
    pub fn new(config: AgentConfig, capture_config: CaptureConfig) -> Self {
        Self {
            config,
            capture_config,
        }
    }

    /// Run the agent — connects to server, registers binaries, manages connections.
    pub async fn run(&self) -> Result<()> {
        let agent_id = self.load_or_generate_agent_id()?;
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string());

        info!(
            agent_id = %agent_id,
            hostname = %hostname,
            server = %self.config.server_grpc_url,
            "Starting detrix agent"
        );

        // Create metrics server
        let metrics_state = MetricsState::new();
        let metrics_port = self.config.metrics_port;
        let metrics_state_clone = metrics_state.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::metrics_server::start(metrics_port, metrics_state_clone).await {
                error!("Metrics server error: {e}");
            }
        });

        // Create scanner — wrapped in Arc<Mutex> so its PID-tracking state
        // (the `known` map) persists across reconnect cycles.
        let scanner = Arc::new(Mutex::new(ProcScanner::new(&self.config.scanner)));

        // Initial scan
        let binaries = scanner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .scan_full();
        info!(
            count = binaries.len(),
            "Initial scan found {} binaries",
            binaries.len()
        );

        // Main reconnect loop
        let mut reconnect_secs = self.config.reconnect_interval_secs;
        let max_reconnect_secs = self.config.reconnect_max_interval_secs;

        loop {
            let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
            let (event_tx, event_rx) =
                mpsc::channel::<detrix_api::generated::detrix::v1::AgentMessage>(1024);
            let events_dropped = Arc::new(AtomicU64::new(0));
            let adapter_manager = Arc::new(AdapterManager::new(
                ctrl_tx.clone(),
                event_tx.clone(),
                Arc::clone(&events_dropped),
                self.capture_config.clone(),
            ));

            match self
                .connect_and_run(
                    &agent_id,
                    &hostname,
                    Arc::clone(&scanner),
                    ctrl_rx,
                    &ctrl_tx,
                    event_rx,
                    &events_dropped,
                    adapter_manager.clone(),
                    &metrics_state,
                )
                .await
            {
                Ok(()) => {
                    info!("Agent disconnected cleanly");
                }
                Err(ref e) => {
                    warn!(error = %e, "Agent connection lost");
                }
            }

            info!(reconnect_in_secs = reconnect_secs, "Reconnecting...");
            tokio::time::sleep(Duration::from_secs(reconnect_secs)).await;
            reconnect_secs = (reconnect_secs * 2).min(max_reconnect_secs);
        }
    }

    /// Connect to server and run the agent loop.
    #[allow(clippy::too_many_arguments)]
    async fn connect_and_run(
        &self,
        agent_id: &str,
        hostname: &str,
        scanner: Arc<Mutex<ProcScanner>>,
        mut ctrl_rx: mpsc::UnboundedReceiver<AgentMessage>,
        ctrl_tx: &mpsc::UnboundedSender<AgentMessage>,
        mut event_rx: mpsc::Receiver<AgentMessage>,
        events_dropped: &Arc<AtomicU64>,
        adapter_manager: Arc<AdapterManager>,
        _metrics_state: &MetricsState,
    ) -> Result<()> {
        // 4c. gRPC channel setup (TLS support deferred)
        let channel = Channel::from_shared(self.config.server_grpc_url.clone())
            .map_err(|e| AgentError::Connection(e.to_string()))?
            .connect()
            .await
            .map_err(|e| AgentError::Connection(e.to_string()))?;

        // 4d. Read auth token and attach it to every outgoing gRPC request.
        let token = if let Some(ref path) = self.config.token_file {
            Some(
                std::fs::read_to_string(path)
                    .map(|s| s.trim().to_string())
                    .map_err(|e| AgentError::Config(format!("token_file: {e}")))?,
            )
        } else {
            None
        };
        let binaries = scanner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .scan_full();
        let register_msg = AgentMessage {
            msg: Some(agent_message::Msg::Register(RegisterAgent {
                agent_id: agent_id.to_string(),
                hostname: hostname.to_string(),
                agent_version: env!("CARGO_PKG_VERSION").to_string(),
                capabilities: Some(AgentCapabilities {
                    ebpf: cfg!(target_os = "linux"),
                    ..Default::default()
                }),
                binaries: binaries.iter().map(binary_info_to_proto).collect(),
            })),
        };

        // Queue the mandatory RegisterAgent frame before opening the bidi stream.
        // This avoids depending on transport/body scheduling to deliver the first
        // client item after the RPC has already been established.
        let mut client =
            AgentServiceClient::with_interceptor(channel, AgentTokenInterceptor { token });
        let (stream_tx, stream_rx) = mpsc::channel::<AgentMessage>(256);
        stream_tx
            .send(register_msg)
            .await
            .map_err(|_| AgentError::Connection("stream closed".into()))?;
        let request = Request::new(ReceiverStream::new(stream_rx));
        let response = client
            .connect_agent(request)
            .await
            .map_err(|e| AgentError::Connection(e.to_string()))?;
        let mut incoming = response.into_inner();

        // 4f. Read RegisterAck + drain initial CreateConnections
        let first = incoming
            .message()
            .await
            .map_err(|e| AgentError::Connection(e.to_string()))?
            .ok_or_else(|| AgentError::Connection("stream closed before RegisterAck".into()))?;
        let ack = match first.msg {
            Some(server_message::Msg::RegisterAck(a)) => a,
            _ => return Err(AgentError::Connection("expected RegisterAck".into())),
        };
        if !ack.accepted {
            return Err(AgentError::RegistrationRejected {
                reason: ack.rejection_reason,
            });
        }

        let drain_deadline =
            tokio::time::Instant::now() + Duration::from_secs(5 + binaries.len() as u64);
        let mut pending_msg: Option<server_message::Msg> = None;
        while let Ok(Ok(Some(msg))) =
            tokio::time::timeout_at(drain_deadline, incoming.message()).await
        {
            match msg.msg {
                Some(server_message::Msg::CreateConnection(cc)) => {
                    adapter_manager.create_connection(cc).await;
                }
                other => {
                    pending_msg = other;
                    break;
                }
            }
        }

        // 4g. Spawn inner tasks
        let mut tasks = tokio::task::JoinSet::new();

        // 1. Read task
        tasks.spawn({
            let am = Arc::clone(&adapter_manager);
            let mut incoming = incoming;
            let ctrl_tx_r = ctrl_tx.clone();
            async move {
                if let Some(msg) = pending_msg {
                    if matches!(msg, server_message::Msg::Ping(_)) {
                        let _ = ctrl_tx_r.send(AgentMessage {
                            msg: Some(agent_message::Msg::Pong(Pong {})),
                        });
                    }
                }
                while let Ok(Some(msg)) = incoming.message().await {
                    match msg.msg {
                        Some(server_message::Msg::CreateConnection(cc)) => {
                            am.create_connection(cc).await;
                        }
                        Some(server_message::Msg::CloseConnection(cc)) => {
                            am.close_connection(&cc.connection_id).await;
                        }
                        Some(server_message::Msg::SetMetric(sm)) => {
                            am.handle_set_metric(sm).await;
                        }
                        Some(server_message::Msg::RemoveMetric(rm)) => {
                            am.handle_remove_metric(rm).await;
                        }
                        Some(server_message::Msg::ReadFile(rf)) => {
                            let response = match am.read_file(&rf.path) {
                                Ok(content) => FileResponse {
                                    request_id: rf.request_id,
                                    path: rf.path,
                                    content,
                                    error: String::new(),
                                },
                                Err(e) => FileResponse {
                                    request_id: rf.request_id,
                                    path: rf.path,
                                    content: Vec::new(),
                                    error: e.to_string(),
                                },
                            };
                            if ctrl_tx_r
                                .send(AgentMessage {
                                    msg: Some(agent_message::Msg::FileResponse(response)),
                                })
                                .is_err()
                            {
                                tracing::debug!("ctrl channel closed, dropping FileResponse");
                            }
                        }
                        Some(server_message::Msg::InspectFile(req)) => {
                            let response: InspectResponse = am.inspect_file(req).await;
                            if ctrl_tx_r
                                .send(AgentMessage {
                                    msg: Some(agent_message::Msg::InspectResponse(response)),
                                })
                                .is_err()
                            {
                                tracing::debug!("ctrl channel closed, dropping InspectResponse");
                            }
                        }
                        Some(server_message::Msg::Ping(_)) => {
                            let _ = ctrl_tx_r.send(AgentMessage {
                                msg: Some(agent_message::Msg::Pong(Pong {})),
                            });
                        }
                        _ => {}
                    }
                }
            }
        });

        // 2. Heartbeat task
        tasks.spawn({
            let ctrl_tx_h = ctrl_tx.clone();
            let stream_tx_h = stream_tx.clone();
            let interval = Duration::from_secs(self.config.heartbeat_interval_secs);
            let dropped = Arc::clone(events_dropped);
            async move {
                let mut ticker = tokio::time::interval(interval);
                loop {
                    ticker.tick().await;
                    let heartbeat = AgentMessage {
                        msg: Some(agent_message::Msg::Heartbeat(Heartbeat {
                            events_dropped: dropped.load(Ordering::Relaxed).min(u32::MAX as u64)
                                as u32,
                            ..Default::default()
                        })),
                    };
                    let _ = ctrl_tx_h.send(heartbeat.clone());
                    let _ = stream_tx_h.send(heartbeat).await;
                }
            }
        });

        // 3. Forward adapter-control messages (ConnectionUpdate, SetMetricAck, etc.)
        tasks.spawn({
            let stream_tx_c = stream_tx.clone();
            async move {
                while let Some(msg) = ctrl_rx.recv().await {
                    if stream_tx_c.send(msg).await.is_err() {
                        break;
                    }
                }
            }
        });

        // 4. Forward metric event batches onto the gRPC stream.
        tasks.spawn({
            let stream_tx_e = stream_tx.clone();
            async move {
                while let Some(msg) = event_rx.recv().await {
                    if stream_tx_e.send(msg).await.is_err() {
                        break;
                    }
                }
            }
        });

        // 5. Scanner task — shares the Arc<Mutex<ProcScanner>> so the `known`
        // PID-tracking state survives reconnect cycles in the outer run() loop.
        tasks.spawn({
            let ctrl_tx_s = ctrl_tx.clone();
            let stream_tx_s = stream_tx.clone();
            let agent_id_s = agent_id.to_string();
            let hostname_s = hostname.to_string();
            let scanner_s = Arc::clone(&scanner);
            let interval = Duration::from_secs(self.config.scanner.scan_interval_secs);
            async move {
                let mut ticker = tokio::time::interval(interval);
                loop {
                    ticker.tick().await;
                    let maybe_binaries = {
                        let mut s = scanner_s.lock().unwrap_or_else(|p| p.into_inner());
                        s.scan_delta()
                    };
                    if let Some(binaries) = maybe_binaries {
                        let register = AgentMessage {
                            msg: Some(agent_message::Msg::Register(RegisterAgent {
                                agent_id: agent_id_s.clone(),
                                hostname: hostname_s.clone(),
                                agent_version: env!("CARGO_PKG_VERSION").to_string(),
                                capabilities: Some(AgentCapabilities {
                                    ebpf: cfg!(target_os = "linux"),
                                    ..Default::default()
                                }),
                                binaries: binaries.iter().map(binary_info_to_proto).collect(),
                            })),
                        };
                        let _ = ctrl_tx_s.send(register.clone());
                        let _ = stream_tx_s.send(register).await;
                    }
                }
            }
        });

        // Wait for first task to finish (typically the incoming-message reader
        // when the gRPC stream closes), then give remaining tasks up to 500ms
        // to flush any in-flight messages before aborting them.
        tasks.join_next().await;
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        tasks.abort_all();
        adapter_manager.stop_all().await;
        Ok(())
    }

    /// Load agent_id from file, or generate and persist a new one.
    fn load_or_generate_agent_id(&self) -> Result<String> {
        let path = &self.config.agent_id_file;

        if path.exists() {
            let id = std::fs::read_to_string(path)
                .map(|s| s.trim().to_string())
                .map_err(|e| AgentError::AgentId(format!("Cannot read agent ID file: {e}")))?;

            if id.is_empty() {
                return Err(AgentError::AgentId("Agent ID file is empty".to_string()));
            }

            info!(agent_id = %id, "Loaded existing agent ID");
            return Ok(id);
        }

        // Generate new UUID
        let id = uuid::Uuid::new_v4().to_string();

        // Persist to file
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::AgentId(format!("Cannot create agent ID directory: {e}"))
            })?;
        }

        std::fs::write(path, &id)
            .map_err(|e| AgentError::AgentId(format!("Cannot write agent ID file: {e}")))?;

        info!(agent_id = %id, path = %path.display(), "Generated new agent ID");
        Ok(id)
    }
}
