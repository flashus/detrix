//! MCP bridge core implementation
//!
//! Contains the McpBridge struct and its methods for forwarding requests,
//! managing heartbeats, and handling daemon communication.

use super::auth::{discover_auth_token, is_connection_error};
use super::config::{BridgeConfig, RestartBackoff};
use anyhow::{Context, Result};
use detrix_logging::{debug, error, info, warn};
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{watch, RwLock};
use uuid::Uuid;

/// MCP stdio-to-HTTP bridge
pub struct McpBridge {
    config: BridgeConfig,
    client: Client,
    /// Unique client ID for daemon tracking
    client_id: String,
    /// Current daemon URL (may change after restart)
    /// pub(crate) for testing dynamic URL updates after daemon restart
    pub(crate) daemon_url: RwLock<String>,
    /// Current auth token (may change after daemon restart if daemon auto-generates new token)
    /// pub(crate) for testing token refresh after daemon restart
    pub(crate) auth_token: RwLock<Option<String>>,
    /// Backoff state for daemon restart attempts
    restart_backoff: RwLock<RestartBackoff>,
}

impl McpBridge {
    /// Create a new bridge with the given configuration
    pub fn new(config: BridgeConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_millis(config.timeout_ms))
            .build()
            .context("Failed to create HTTP client")?;

        // Generate unique client ID for daemon tracking
        let client_id = Uuid::new_v4().to_string();
        let daemon_url = RwLock::new(config.daemon_url.clone());
        let auth_token = RwLock::new(config.auth_token.clone());
        let restart_backoff = RwLock::new(RestartBackoff::new());

        Ok(Self {
            config,
            client,
            client_id,
            daemon_url,
            auth_token,
            restart_backoff,
        })
    }

    /// Check if daemon is healthy
    async fn is_daemon_healthy(&self) -> bool {
        let url = {
            let daemon_url = self.daemon_url.read().await;
            format!("{}/health", daemon_url)
        };

        match self.client.get(&url).send().await {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }

    /// Try to restart the daemon if config is available
    async fn try_restart_daemon(&self) -> Result<()> {
        let (config_path, pid_file) = match (&self.config.config_path, &self.config.pid_file) {
            (Some(config), Some(pid)) => (config.clone(), pid.clone()),
            _ => {
                debug!("Cannot restart daemon: config_path or pid_file not provided");
                anyhow::bail!("Daemon restart not available (missing config)");
            }
        };

        info!("Attempting to restart daemon...");

        // Spawn daemon using the same logic as mcp.rs
        let new_port = crate::commands::mcp::spawn_daemon_for_mcp(
            &self.config.daemon_host,
            &config_path.to_string_lossy(),
            &pid_file,
            self.config.daemon_port,
        )
        .await
        .context("Failed to restart daemon")?;

        // Update daemon URL with new port
        {
            let mut daemon_url = self.daemon_url.write().await;
            *daemon_url = format!("http://{}:{}", self.config.daemon_host, new_port);
        }

        // Re-discover auth token (daemon may have generated a new one)
        {
            let new_token = discover_auth_token();
            let mut auth_token = self.auth_token.write().await;
            if new_token != *auth_token {
                info!("Auth token updated after daemon restart");
            }
            *auth_token = new_token;
        }

        info!("Daemon restarted on port {}", new_port);
        Ok(())
    }

    /// Forward a JSON-RPC request to the daemon and return the response
    ///
    /// If the daemon is not responding and config is available, attempts to
    /// restart the daemon automatically.
    pub async fn forward_request(&self, request: Value) -> Result<Value> {
        // First attempt
        let result = self.try_forward_request(&request).await;

        // If successful, return immediately
        if result.is_ok() {
            return result;
        }

        let e = result.unwrap_err();

        // Check if this looks like a connection error using proper error type detection
        if !is_connection_error(&e) {
            // Not a connection error, propagate original error
            return Err(e);
        }

        warn!("Daemon not responding, checking health...");

        // Verify daemon is actually down
        if self.is_daemon_healthy().await {
            // Daemon is healthy but request failed for another reason
            return Err(e);
        }

        // Try to restart daemon
        if let Err(restart_err) = self.try_restart_daemon().await {
            debug!("Daemon restart failed: {}", restart_err);
            anyhow::bail!(
                "Daemon not responding and could not be restarted: {}. \
                Start daemon manually with: detrix daemon start",
                e
            );
        }

        // Retry after restart
        self.try_forward_request(&request).await
    }

    /// Internal method to forward request without retry logic
    pub async fn try_forward_request(&self, request: &Value) -> Result<Value> {
        let (url, token) = {
            let daemon_url = self.daemon_url.read().await;
            let auth_token = self.auth_token.read().await;
            (format!("{}/mcp", daemon_url), auth_token.clone())
        };
        debug!("Forwarding request to {}", url);

        let mut req_builder = self
            .client
            .post(&url)
            .header("X-Detrix-Client-Id", &self.client_id);

        // Add auth token if available
        if let Some(ref token) = token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        // Add parent process info headers if available
        if let Some(ref parent) = self.config.parent_process {
            req_builder = req_builder
                .header("X-Detrix-Parent-Pid", parent.pid.to_string())
                .header("X-Detrix-Parent-Name", &parent.name)
                .header("X-Detrix-Bridge-Pid", parent.bridge_pid.to_string());
        }

        let response = req_builder
            .json(request)
            .send()
            .await
            .context("Failed to send request to daemon")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Daemon returned error {}: {}", status, body);
        }

        let response_json: Value = response
            .json()
            .await
            .context("Failed to parse daemon response as JSON")?;

        Ok(response_json)
    }

    /// Send a heartbeat to the daemon to keep the client registered
    ///
    /// This uses the /mcp/heartbeat endpoint which just updates the client's
    /// last activity timestamp without processing any MCP request.
    pub async fn send_heartbeat(&self) -> Result<()> {
        let (url, token) = {
            let daemon_url = self.daemon_url.read().await;
            let auth_token = self.auth_token.read().await;
            (format!("{}/mcp/heartbeat", daemon_url), auth_token.clone())
        };
        debug!("Sending heartbeat to {}", url);

        let mut req_builder = self
            .client
            .post(&url)
            .header("X-Detrix-Client-Id", &self.client_id);

        // Add auth token if available
        if let Some(ref token) = token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        // Add parent process info headers if available
        if let Some(ref parent) = self.config.parent_process {
            req_builder = req_builder
                .header("X-Detrix-Parent-Pid", parent.pid.to_string())
                .header("X-Detrix-Parent-Name", &parent.name)
                .header("X-Detrix-Bridge-Pid", parent.bridge_pid.to_string());
        }

        let response = req_builder
            .send()
            .await
            .context("Failed to send heartbeat to daemon")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Daemon heartbeat returned error {}: {}", status, body);
        }

        Ok(())
    }

    /// Send a disconnect notification to the daemon
    ///
    /// This notifies the daemon that this MCP client is shutting down,
    /// allowing it to unregister the client and potentially auto-shutdown
    /// if no other clients remain connected.
    pub async fn send_disconnect(&self) -> Result<()> {
        let (url, token) = {
            let daemon_url = self.daemon_url.read().await;
            let auth_token = self.auth_token.read().await;
            (format!("{}/mcp/disconnect", daemon_url), auth_token.clone())
        };
        info!("Sending disconnect notification to {}", url);

        let mut req_builder = self
            .client
            .post(&url)
            .header("X-Detrix-Client-Id", &self.client_id);

        // Add auth token if available
        if let Some(ref token) = token {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", token));
        }

        let response = req_builder
            .send()
            .await
            .context("Failed to send disconnect to daemon")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Daemon disconnect returned error {}: {}", status, body);
        }

        info!("Disconnect notification sent successfully");
        Ok(())
    }

    /// Run the bridge, reading from stdin and writing to stdout
    ///
    /// This is the main entry point for the bridge. It reads JSON-RPC requests
    /// from stdin (one per line), forwards them to the daemon, and writes
    /// responses to stdout.
    ///
    /// A background task sends periodic heartbeats to keep the client registered
    /// with the daemon even during idle periods.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        info!(
            "MCP bridge started (client_id: {}), forwarding to {}",
            self.client_id, self.config.daemon_url
        );

        // Create a shutdown channel for the heartbeat task
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // Spawn heartbeat task
        let heartbeat_handle = self.spawn_heartbeat_task(shutdown_rx);

        // Run the main stdin/stdout loop
        let result = self.run_stdio_loop().await;

        // Signal heartbeat task to stop
        let _ = shutdown_tx.send(true);

        // Wait for heartbeat task to finish
        if let Err(e) = heartbeat_handle.await {
            warn!("Heartbeat task error: {}", e);
        }

        info!("MCP bridge stopped");
        result
    }

    /// Spawn a background task that sends periodic heartbeats to the daemon
    ///
    /// If heartbeat fails consecutively, proactively attempts to restart the daemon
    /// to ensure it's always available when the IDE sends requests.
    fn spawn_heartbeat_task(
        self: &Arc<Self>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        let bridge = Arc::clone(self);
        let interval = std::time::Duration::from_secs(bridge.config.heartbeat_interval_secs);
        let max_failures = bridge.config.heartbeat_max_failures;

        tokio::spawn(async move {
            info!(
                "Heartbeat task started (interval: {}s, max_failures: {})",
                bridge.config.heartbeat_interval_secs, max_failures
            );

            let mut consecutive_failures: u32 = 0;

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        consecutive_failures = bridge
                            .handle_heartbeat_tick(consecutive_failures, max_failures)
                            .await;
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Heartbeat task stopping");
                            break;
                        }
                    }
                }
            }
        })
    }

    /// Handle a single heartbeat tick, returning the new consecutive failure count
    pub async fn handle_heartbeat_tick(
        &self,
        mut consecutive_failures: u32,
        max_failures: u32,
    ) -> u32 {
        match self.send_heartbeat().await {
            Ok(()) => {
                if consecutive_failures > 0 {
                    info!("Heartbeat restored after {} failures", consecutive_failures);
                }
                debug!("Heartbeat sent successfully");
                // Reset backoff on successful heartbeat (daemon is healthy)
                self.restart_backoff.write().await.record_success();
                return 0; // Reset failures
            }
            Err(e) => {
                consecutive_failures += 1;
                warn!(
                    "Failed to send heartbeat ({}/{}): {}",
                    consecutive_failures, max_failures, e
                );
            }
        }

        // Check if we should attempt daemon restart
        if consecutive_failures < max_failures {
            return consecutive_failures;
        }

        // Verify daemon is actually down before restarting
        if self.is_daemon_healthy().await {
            debug!("Daemon healthy despite heartbeat failure");
            self.restart_backoff.write().await.record_success();
            return 0; // Transient issue, reset
        }

        // Check if backoff period has elapsed
        {
            let backoff = self.restart_backoff.read().await;
            if !backoff.can_attempt() {
                debug!(
                    "Waiting for backoff period before next restart attempt (failure #{})",
                    backoff.failures
                );
                return consecutive_failures;
            }
        }

        // Attempt proactive daemon restart
        info!("Daemon unresponsive, attempting proactive restart...");
        match self.try_restart_daemon().await {
            Ok(()) => {
                info!("Daemon restarted proactively");
                self.restart_backoff.write().await.record_success();
                0 // Reset failures
            }
            Err(restart_err) => {
                // Record failure for backoff calculation
                self.restart_backoff.write().await.record_failure();
                debug!("Proactive daemon restart failed: {}", restart_err);
                consecutive_failures
            }
        }
    }

    /// Run the main stdin/stdout processing loop
    async fn run_stdio_loop(&self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = reader
                .read_line(&mut line)
                .await
                .context("Failed to read from stdin")?;

            if bytes_read == 0 {
                // EOF - client disconnected
                info!("EOF on stdin, shutting down bridge");
                break;
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            debug!("Received request: {}", line);

            // Parse JSON-RPC request
            let request: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    error!("Invalid JSON: {}", e);
                    let error_response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": -32700,
                            "message": format!("Parse error: {}", e)
                        },
                        "id": null
                    });
                    let response_str = serde_json::to_string(&error_response)?;
                    stdout.write_all(response_str.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                    continue;
                }
            };

            // Forward to daemon
            let response = match self.forward_request(request.clone()).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Failed to forward request: {}", e);
                    let id = request.get("id").cloned().unwrap_or(Value::Null);
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": -32603,
                            "message": format!("Internal error: {}", e)
                        },
                        "id": id
                    })
                }
            };

            // Write response to stdout
            let response_str = serde_json::to_string(&response)?;
            debug!("Sending response: {}", response_str);
            stdout.write_all(response_str.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }

        Ok(())
    }
}
