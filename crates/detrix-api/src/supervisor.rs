//! Event Listener Supervisor
//!
//! With the connection-based adapter approach (AdapterLifecycleManager), adapter health
//! is now monitored per-connection. This supervisor provides a simplified interface
//! for checking overall system status.

use crate::state::ApiState;
use detrix_config::constants::{DEFAULT_HEALTH_CHECK_INTERVAL_MS, DEFAULT_RESTART_DELAY_MS};
use detrix_config::DaemonConfig;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, info};

/// Configuration for the supervisor
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// How often to check adapter health (default: 5 seconds)
    pub health_check_interval: Duration,
    /// Delay before attempting to restart event listener after reconnection
    pub restart_delay: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            health_check_interval: Duration::from_millis(DEFAULT_HEALTH_CHECK_INTERVAL_MS),
            restart_delay: Duration::from_millis(DEFAULT_RESTART_DELAY_MS),
        }
    }
}

impl From<&DaemonConfig> for SupervisorConfig {
    fn from(config: &DaemonConfig) -> Self {
        Self {
            health_check_interval: Duration::from_millis(config.health_check_interval_ms),
            restart_delay: Duration::from_millis(config.restart_delay_ms),
        }
    }
}

/// Supervisor for monitoring overall adapter lifecycle
///
/// With connection-based adapters (AdapterLifecycleManager), the supervisor's role
/// is simplified to monitoring overall health rather than managing a singleton adapter.
pub struct Supervisor {
    /// Reference to the API state
    state: Arc<ApiState>,
    /// Configuration
    config: SupervisorConfig,
    /// Channel to signal shutdown
    shutdown_tx: watch::Sender<bool>,
    /// Channel to receive shutdown signal
    shutdown_rx: watch::Receiver<bool>,
}

impl Supervisor {
    /// Create a new supervisor
    pub fn new(state: Arc<ApiState>) -> Self {
        Self::with_config(state, SupervisorConfig::default())
    }

    /// Create a new supervisor with custom config
    pub fn with_config(state: Arc<ApiState>, config: SupervisorConfig) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            state,
            config,
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Check if any adapter is currently connected (via AdapterLifecycleManager)
    pub async fn is_connected(&self) -> bool {
        self.state
            .context
            .adapter_lifecycle_manager
            .has_connected_adapters()
            .await
    }

    /// Start the supervisor
    ///
    /// Returns a handle to the supervisor task.
    pub fn start(self: Arc<Self>) -> JoinHandle<()> {
        let supervisor = Arc::clone(&self);
        tokio::spawn(async move {
            supervisor.run().await;
        })
    }

    /// Run the supervisor loop (monitors connection health)
    async fn run(&self) {
        info!("🔍 Supervisor started, monitoring adapter health via lifecycle manager...");

        let mut shutdown_rx = self.shutdown_rx.clone();

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("Supervisor received shutdown signal");
                        break;
                    }
                }
                _ = tokio::time::sleep(self.config.health_check_interval) => {
                    let adapter_count = self.state.context.adapter_lifecycle_manager.adapter_count().await;
                    let has_connected = self.state.context.adapter_lifecycle_manager.has_connected_adapters().await;

                    // Update MCP tracker with current connection count
                    // This allows the tracker to make informed shutdown decisions
                    // (don't shutdown if there are active debugger connections)
                    self.state.mcp_client_tracker.update_connection_count(adapter_count);

                    debug!(
                        "Health check: {} adapter(s), connected={}",
                        adapter_count, has_connected
                    );
                }
            }
        }

        info!("Supervisor stopped");
    }

    /// Stop the supervisor
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop();
    }
}
