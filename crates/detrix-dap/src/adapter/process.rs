//! Adapter Process State Management
//!
//! Core AdapterProcess struct that manages the debug adapter lifecycle.

use super::config::{AdapterConfig, ConnectionMode};
use super::connection::{connect_tcp, launch_subprocess};
use crate::{Capabilities, DapBroker, Error, Result};
use detrix_config::AdapterConnectionConfig;
use std::sync::Arc;
use tokio::process::Child;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// State of an adapter
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterState {
    /// Adapter is starting up
    Starting,
    /// Adapter is ready (initialized)
    Ready,
    /// Adapter has failed
    Failed,
    /// Adapter is shutting down
    ShuttingDown,
    /// Adapter is stopped
    Stopped,
}

/// Adapter process manager
pub struct AdapterProcess {
    /// Configuration
    pub(crate) config: AdapterConfig,

    /// Connection configuration (timeouts, retries)
    pub(crate) connection_config: AdapterConnectionConfig,

    /// Child process
    pub(crate) process: Arc<Mutex<Option<Child>>>,

    /// DAP broker (if connected)
    pub(crate) broker: Arc<Mutex<Option<Arc<DapBroker>>>>,

    /// Current state
    pub(crate) state: Arc<Mutex<AdapterState>>,

    /// Adapter capabilities (after initialization)
    pub(crate) capabilities: Arc<Mutex<Option<Capabilities>>>,
}

impl std::fmt::Debug for AdapterProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterProcess")
            .field("config", &self.config)
            .field("process", &"<process>")
            .field("broker", &"<broker>")
            .field("state", &"<state>")
            .field("capabilities", &"<capabilities>")
            .finish()
    }
}

impl AdapterProcess {
    /// Create a new adapter process manager with connection config
    pub fn new_with_config(
        config: AdapterConfig,
        connection_config: AdapterConnectionConfig,
    ) -> Self {
        Self {
            config,
            connection_config,
            process: Arc::new(Mutex::new(None)),
            broker: Arc::new(Mutex::new(None)),
            state: Arc::new(Mutex::new(AdapterState::Stopped)),
            capabilities: Arc::new(Mutex::new(None)),
        }
    }

    /// Create a new adapter process manager with default connection config
    pub fn new(config: AdapterConfig) -> Self {
        Self::new_with_config(config, AdapterConnectionConfig::default())
    }

    /// Get current state
    pub async fn state(&self) -> AdapterState {
        *self.state.lock().await
    }

    /// Check if adapter is in Ready state (non-blocking)
    ///
    /// Uses try_lock for a best-effort check without blocking.
    /// Returns false if the lock cannot be acquired.
    /// Also checks if the broker's reader task is alive to detect zombie connections.
    pub fn is_ready(&self) -> bool {
        // try_lock is sync and non-blocking
        let state_ready = match self.state.try_lock() {
            Ok(guard) => *guard == AdapterState::Ready,
            Err(_) => return false, // Lock contention - assume not ready
        };

        if !state_ready {
            return false;
        }

        // Also check broker reader task health for true liveness check
        match self.broker.try_lock() {
            Ok(guard) => match guard.as_ref() {
                Some(broker) => broker.is_alive(),
                None => false,
            },
            Err(_) => false, // Lock contention - assume not ready
        }
    }

    /// Get adapter capabilities (if initialized)
    pub async fn capabilities(&self) -> Option<Capabilities> {
        self.capabilities.lock().await.clone()
    }

    /// Get connection configuration
    pub fn connection_config(&self) -> &AdapterConnectionConfig {
        &self.connection_config
    }

    /// Start the adapter process and initialize DAP connection
    pub async fn start(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        if *state != AdapterState::Stopped {
            return Err(Error::Protocol(format!(
                "Adapter is not stopped (state: {:?})",
                *state
            )));
        }

        *state = AdapterState::Starting;
        drop(state);

        // Choose connection mode
        let result = match &self.config.connection_mode {
            ConnectionMode::Launch => {
                launch_subprocess(
                    &self.config,
                    &self.connection_config,
                    &self.process,
                    &self.broker,
                    &self.capabilities,
                )
                .await
            }
            ConnectionMode::Attach { host, port } => {
                connect_tcp(
                    host,
                    *port,
                    &self.config,
                    &self.connection_config,
                    &self.broker,
                    &self.capabilities,
                )
                .await
            }
            ConnectionMode::AttachRemote { host, port } => {
                connect_tcp(
                    host,
                    *port,
                    &self.config,
                    &self.connection_config,
                    &self.broker,
                    &self.capabilities,
                )
                .await
            }
            ConnectionMode::LaunchProgram { host, port, .. } => {
                connect_tcp(
                    host,
                    *port,
                    &self.config,
                    &self.connection_config,
                    &self.broker,
                    &self.capabilities,
                )
                .await
            }
            ConnectionMode::AttachPid { host, port, .. } => {
                connect_tcp(
                    host,
                    *port,
                    &self.config,
                    &self.connection_config,
                    &self.broker,
                    &self.capabilities,
                )
                .await
            }
        };

        // If connection failed, reset state to Failed so it can be retried
        if let Err(ref e) = result {
            tracing::warn!(
                "Adapter connection failed: {}, resetting state to Failed",
                e
            );
            let mut state = self.state.lock().await;
            *state = AdapterState::Failed;
        } else {
            let mut state = self.state.lock().await;
            *state = AdapterState::Ready;
            info!("Adapter started successfully: {}", self.config.adapter_id);
        }

        result
    }

    /// Get the DAP broker (if connected)
    pub async fn broker(&self) -> Result<Arc<DapBroker>> {
        let broker = self.broker.lock().await;
        broker
            .clone()
            .ok_or_else(|| Error::Communication("Adapter not connected".to_string()))
    }

    /// Check if adapter process is running
    pub async fn is_running(&self) -> bool {
        let mut process = self.process.lock().await;
        if let Some(child) = process.as_mut() {
            child.try_wait().unwrap_or(None).is_none()
        } else {
            false
        }
    }

    /// Stop the adapter process
    pub async fn stop(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        if *state == AdapterState::Stopped || *state == AdapterState::ShuttingDown {
            return Ok(());
        }

        *state = AdapterState::ShuttingDown;
        drop(state);

        info!("Stopping adapter: {}", self.config.adapter_id);

        // Kill process
        let mut process = self.process.lock().await;
        if let Some(mut child) = process.take() {
            match child.kill().await {
                Ok(_) => debug!("Adapter process killed"),
                Err(e) => warn!("Failed to kill adapter process: {}", e),
            }

            // Wait for process to exit
            match child.wait().await {
                Ok(status) => debug!("Adapter process exited with status: {}", status),
                Err(e) => warn!("Failed to wait for adapter process: {}", e),
            }
        }

        // Clear broker
        {
            let mut broker = self.broker.lock().await;
            *broker = None;
        }

        // Update state
        {
            let mut state = self.state.lock().await;
            *state = AdapterState::Stopped;
        }

        info!("Adapter stopped: {}", self.config.adapter_id);
        Ok(())
    }

    /// Health check - verify process and connection are alive
    pub async fn health_check(&self) -> Result<()> {
        let state = self.state().await;

        match state {
            AdapterState::Ready => {
                // Check process is running
                if !self.is_running().await {
                    tracing::error!("Adapter process is not running");
                    let mut state = self.state.lock().await;
                    *state = AdapterState::Failed;
                    return Err(Error::Communication("Adapter process died".to_string()));
                }

                // Could add ping/pong check here if DAP supported it
                Ok(())
            }
            AdapterState::Starting => Err(Error::Protocol("Adapter is still starting".to_string())),
            AdapterState::Failed => Err(Error::Communication("Adapter has failed".to_string())),
            AdapterState::ShuttingDown | AdapterState::Stopped => {
                Err(Error::Communication("Adapter is not running".to_string()))
            }
        }
    }
}

impl Drop for AdapterProcess {
    fn drop(&mut self) {
        // Try to stop the adapter when dropped
        // Note: This is best-effort since we can't await in Drop
        if let Some(mut child) = self.process.try_lock().ok().and_then(|mut p| p.take()) {
            let _ = child.start_kill();
        }
    }
}
