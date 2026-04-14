//! Agent — main struct, run loop, and reconnect logic.

use crate::adapter_manager::AdapterManager;
use crate::error::{AgentError, Result};
use crate::metrics_server::MetricsState;
use crate::scanner::ProcScanner;
use detrix_config::AgentConfig;
use detrix_ebpf::CaptureConfig;
use detrix_logging::{error, info, warn};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// The Agent connects to the server and manages local adapters.
pub struct Agent {
    config: AgentConfig,
    _capture_config: CaptureConfig,
}

impl Agent {
    pub fn new(config: AgentConfig, capture_config: CaptureConfig) -> Self {
        Self {
            config,
            _capture_config: capture_config,
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
        let _adapter_manager =
            AdapterManager::new(ctrl_tx.clone(), event_tx.clone(), events_dropped.clone());

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
    async fn connect_and_run(
        &self,
        _agent_id: &str,
        _hostname: &str,
        _scanner: &mut ProcScanner,
        _ctrl_tx: &mpsc::UnboundedSender<detrix_api::generated::detrix::v1::AgentMessage>,
        _event_tx: &mpsc::Sender<detrix_api::generated::detrix::v1::AgentMessage>,
        _events_dropped: &Arc<AtomicU64>,
        metrics_state: &MetricsState,
    ) -> Result<()> {
        // TODO: Implement gRPC connection
        // 1. Build channel with TLS
        // 2. Create AgentServiceClient
        // 3. Open bidi stream
        // 4. Send RegisterAgent
        // 5. Spawn read task
        // 6. Spawn mux task
        // 7. Wait for stream close

        // Placeholder: simulate running
        metrics_state.uptime_secs.store(0, Ordering::Relaxed);

        // For now, just wait for ctrl signal
        tokio::signal::ctrl_c()
            .await
            .map_err(|e| AgentError::Connection(format!("Ctrl+C handler error: {e}")))
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
