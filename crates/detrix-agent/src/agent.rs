//! Agent — main struct, run loop, and reconnect logic.

use crate::adapter_manager::AdapterManager;
use crate::error::{AgentError, Result};
use crate::metrics_server::MetricsState;
use crate::scanner::{binary_info_to_proto, ProcScanner};
use detrix_api::generated::detrix::v1::agent_service_client::AgentServiceClient;
use detrix_api::generated::detrix::v1::{
    agent_message, server_message, AgentCapabilities, AgentMessage, Heartbeat, Pong, RegisterAgent,
};
use detrix_config::AgentConfig;
use detrix_ebpf::CaptureConfig;
use detrix_logging::{error, info, warn};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::Request;

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

        // Create channels
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
        let (event_tx, mut event_rx) =
            mpsc::channel::<detrix_api::generated::detrix::v1::AgentMessage>(1024);
        let events_dropped = Arc::new(AtomicU64::new(0));

        // Create adapter manager
        let adapter_manager = Arc::new(AdapterManager::new(
            ctrl_tx.clone(),
            event_tx.clone(),
            Arc::clone(&events_dropped),
            self.capture_config.clone(),
        ));

        // Create scanner
        let mut scanner = ProcScanner::new(&self.config.scanner);

        // Initial scan
        let binaries = scanner.scan_full();
        info!(
            count = binaries.len(),
            "Initial scan found {} binaries",
            binaries.len()
        );

        // Main reconnect loop
        let mut reconnect_secs = self.config.reconnect_interval_secs;
        let max_reconnect_secs = self.config.reconnect_max_interval_secs;

        loop {
            match self
                .connect_and_run(
                    &agent_id,
                    &hostname,
                    &mut scanner,
                    &ctrl_tx,
                    &event_tx,
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

            // Drain any remaining messages
            while ctrl_rx.try_recv().is_ok() {}
            while event_rx.try_recv().is_ok() {}
        }
    }

    /// Connect to server and run the agent loop.
    #[allow(clippy::too_many_arguments)]
    async fn connect_and_run(
        &self,
        agent_id: &str,
        hostname: &str,
        scanner: &mut ProcScanner,
        ctrl_tx: &mpsc::UnboundedSender<AgentMessage>,
        _event_tx: &mpsc::Sender<AgentMessage>,
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

        // 4d. Auth interceptor - TODO: Apply auth interceptor - needs proper interceptor trait usage
        // For now, skip auth to focus on basic connectivity
        let _token = if let Some(ref path) = self.config.token_file {
            std::fs::read_to_string(path)
                .map(|s| s.trim().to_string())
                .map_err(|e| AgentError::Config(format!("token_file: {e}")))?
        } else {
            String::new()
        };

        // 4e. Open bidi stream and send RegisterAgent
        let mut client = AgentServiceClient::new(channel);
        let (stream_tx, stream_rx) = mpsc::channel::<AgentMessage>(256);
        let request = Request::new(ReceiverStream::new(stream_rx));
        let response = client
            .connect_agent(request)
            .await
            .map_err(|e| AgentError::Connection(e.to_string()))?;
        let mut incoming = response.into_inner();

        let binaries = scanner.scan_full();
        stream_tx
            .send(AgentMessage {
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
            })
            .await
            .map_err(|_| AgentError::Connection("stream closed".into()))?;

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
                            events_dropped: dropped.load(Ordering::Relaxed) as u32,
                            ..Default::default()
                        })),
                    };
                    let _ = ctrl_tx_h.send(heartbeat.clone());
                    let _ = stream_tx_h.send(heartbeat).await;
                }
            }
        });

        // 3. Scanner task
        tasks.spawn({
            let ctrl_tx_s = ctrl_tx.clone();
            let stream_tx_s = stream_tx.clone();
            let agent_id_s = agent_id.to_string();
            let hostname_s = hostname.to_string();
            let mut scanner_owned =
                std::mem::replace(scanner, ProcScanner::new(&self.config.scanner));
            let interval = Duration::from_secs(self.config.scanner.scan_interval_secs);
            async move {
                let mut ticker = tokio::time::interval(interval);
                loop {
                    ticker.tick().await;
                    if let Some(binaries) = scanner_owned.scan_delta() {
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

        // Wait for first task to finish
        tasks.join_next().await;
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
