//! MCP bridge core implementation
//!
//! Contains the McpBridge struct and its methods for forwarding requests,
//! managing heartbeats, and handling daemon communication.

use super::auth::{discover_auth_token, is_connection_error};
use super::config::{BridgeConfig, RestartBackoff};
use anyhow::{Context, Result};
use detrix_config::constants::{AUTHORIZATION_HEADER, BEARER_PREFIX};
use detrix_logging::{debug, error, info, warn};
use reqwest::Client;
use serde_json::Value;
use std::collections::HashSet;
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
    /// URL of the local file server (set after start_file_server)
    file_server_url: RwLock<Option<String>>,
    /// Host to advertise for file server (for daemon switching)
    file_server_host: RwLock<String>,
    /// Connections this bridge has auto-attached to (for cleanup on switch)
    attached_connections: RwLock<HashSet<String>>,
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

        let file_server_host_val = config.file_server_host.clone();

        Ok(Self {
            config,
            client,
            client_id,
            daemon_url,
            auth_token,
            restart_backoff,
            file_server_url: RwLock::new(None),
            file_server_host: RwLock::new(file_server_host_val),
            attached_connections: RwLock::new(HashSet::new()),
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
            self.config.config_port,
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
        let (url, token, file_server) = {
            let daemon_url = self.daemon_url.read().await;
            let auth_token = self.auth_token.read().await;
            let file_server_url = self.file_server_url.read().await;
            (
                format!("{}/mcp", daemon_url),
                auth_token.clone(),
                file_server_url.clone(),
            )
        };
        debug!("Forwarding request to {}", url);

        let mut req_builder = self
            .client
            .post(&url)
            .header(detrix_api::common::CLIENT_ID_HEADER, &self.client_id);

        // Add auth token if available
        if let Some(ref token) = token {
            req_builder =
                req_builder.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token));
        }

        // Add parent process info headers if available
        if let Some(ref parent) = self.config.parent_process {
            req_builder = req_builder
                .header("X-Detrix-Parent-Pid", parent.pid.to_string())
                .header("X-Detrix-Parent-Name", &parent.name)
                .header("X-Detrix-Bridge-Pid", parent.bridge_pid.to_string());
        }

        // Add file server URL so daemon can fetch source files from this machine
        if let Some(ref url) = file_server {
            req_builder = req_builder.header("X-Detrix-File-Server-Url", url);
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
            .header(detrix_api::common::CLIENT_ID_HEADER, &self.client_id);

        // Add auth token if available
        if let Some(ref token) = token {
            req_builder =
                req_builder.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token));
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
            .header(detrix_api::common::CLIENT_ID_HEADER, &self.client_id);

        // Add auth token if available
        if let Some(ref token) = token {
            req_builder =
                req_builder.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token));
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

        // Start local file server so daemon can fetch source files from this machine
        match super::file_server::start_file_server().await {
            Ok(port) => {
                let url = format!("http://127.0.0.1:{}", port);
                info!(url = %url, "Bridge file server ready");
                *self.file_server_url.write().await = Some(url);
            }
            Err(e) => {
                warn!(error = %e, "Failed to start bridge file server (file fetching disabled)");
            }
        }

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

            // Route request: local tool → daemon forward (with wake fallback)
            let mut response = match self.try_handle_local_tool(&request).await {
                Some(local_response) => local_response,
                None => self.forward_or_fallback(&request).await,
            };

            // Auto-switch daemon if wake response contains daemon_url
            self.maybe_auto_switch_daemon(&mut response).await;

            // Write response to stdout
            let response_str = serde_json::to_string(&response)?;
            debug!("Sending response: {}", response_str);
            stdout.write_all(response_str.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }

        Ok(())
    }

    /// Try to handle bridge-local tool calls
    ///
    /// Returns Some(response) if the request is a local tool call (switch_daemon, list_known_daemons),
    /// or None if the request should be forwarded to the daemon.
    async fn try_handle_local_tool(&self, request: &Value) -> Option<Value> {
        // Check if this is a tools/call request
        if request.get("method")?.as_str()? != "tools/call" {
            return None;
        }

        let params = request.get("params")?;
        let tool_name = params.get("name")?.as_str()?;
        let id = request.get("id").cloned().unwrap_or(Value::Null);

        match tool_name {
            "switch_daemon" => Some(self.handle_switch_daemon_tool(params, id).await),
            "list_known_daemons" => Some(self.handle_list_known_daemons_tool(id).await),
            _ => None, // Not a local tool, forward to daemon
        }
    }

    /// Handle the switch_daemon tool call
    async fn handle_switch_daemon_tool(&self, params: &Value, id: Value) -> Value {
        let arguments = match params.get("arguments") {
            Some(args) => args,
            None => {
                return jsonrpc_error(id, -32602, "Missing 'arguments' in switch_daemon request");
            }
        };

        // Extract parameters
        let url = arguments.get("url").and_then(|v| v.as_str());
        let alias = arguments.get("alias").and_then(|v| v.as_str());
        let close_connections = arguments
            .get("close_connections")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let disable_metrics = arguments
            .get("disable_metrics")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let file_server_host = arguments
            .get("file_server_host")
            .and_then(|v| v.as_str())
            .map(String::from);
        let force = arguments
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Validate parameters
        if url.is_none() && alias.is_none() {
            return jsonrpc_error(id, -32602, "Must specify either 'url' or 'alias'");
        }
        if url.is_some() && alias.is_some() {
            return jsonrpc_error(id, -32602, "Cannot specify both 'url' and 'alias'");
        }

        // Resolve daemon config
        let (target_url, is_production) = if let Some(alias_val) = alias {
            match detrix_config::daemons::DaemonsConfig::load_from_home() {
                Ok(config) => match config.find_by_alias(alias_val) {
                    Some(daemon) => (daemon.url.clone(), daemon.is_production),
                    None => {
                        return jsonrpc_error(
                            id,
                            -32602,
                            format!("Daemon alias '{}' not found", alias_val),
                        );
                    }
                },
                Err(e) => {
                    return serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": -32603,
                            "message": format!("Failed to load daemons config: {}", e)
                        },
                        "id": id
                    });
                }
            }
        } else {
            (url.unwrap().to_string(), false) // Direct URL, assume non-production
        };

        // Production confirmation
        if is_production && !force {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32603,
                    "message": "This is a PRODUCTION daemon. Add force=true to confirm switch."
                },
                "id": id
            });
        }

        // Get current daemon URL for cleanup operations
        let current_url = self.get_current_daemon_url().await;

        // Disable metrics created by this bridge (before switching)
        let mut disabled_count = 0;
        if disable_metrics {
            match self.disable_my_metrics_on_daemon(&current_url).await {
                Ok(count) => {
                    disabled_count = count;
                    info!(
                        disabled_count = count,
                        current_url, "Disabled my metrics before daemon switch"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "Failed to disable my metrics");
                }
            }
        }

        // Release this bridge's connection references (before switching)
        let mut released_refs = 0u64;
        let mut disconnected_conns = 0u64;
        if close_connections {
            match self.release_my_references_on_daemon(&current_url).await {
                Ok((released, disconnected)) => {
                    released_refs = released;
                    disconnected_conns = disconnected;
                    info!(
                        released,
                        disconnected,
                        current_url,
                        "Released my connection references before daemon switch"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "Failed to release connection references");
                }
            }
            // Clear tracked connections since we released them
            self.attached_connections.write().await.clear();
        }

        // Switch daemon
        match self
            .switch_daemon(target_url.clone(), file_server_host)
            .await
        {
            Ok(()) => {
                let mut msg = format!("Switched to daemon: {}\n", target_url);
                if close_connections {
                    msg.push_str(&format!(
                        "\nReleased {} references, disconnected {} connections on previous daemon\n",
                        released_refs, disconnected_conns
                    ));
                } else {
                    msg.push_str(&format!(
                        "\nNote: Connections on previous daemon ({}) are still active.\n",
                        current_url
                    ));
                }
                if disable_metrics {
                    msg.push_str(&format!(
                        "Disabled {} of my metrics on previous daemon\n",
                        disabled_count
                    ));
                } else {
                    msg.push_str(&format!(
                        "Note: Metrics on previous daemon ({}) are still running.\n",
                        current_url
                    ));
                }

                jsonrpc_success_text(id, msg)
            }
            Err(e) => jsonrpc_error(id, -32603, e),
        }
    }

    /// Handle the list_known_daemons tool call
    async fn handle_list_known_daemons_tool(&self, id: Value) -> Value {
        match detrix_config::daemons::DaemonsConfig::load_from_home() {
            Ok(config) => {
                if config.daemon.is_empty() {
                    return jsonrpc_success_text(
                        id,
                        "No saved daemons found in ~/.detrix/daemons.toml",
                    );
                }

                let mut lines = vec!["Saved Daemons:\n".to_string()];
                for daemon in &config.daemon {
                    let prod_marker = if daemon.is_production {
                        " [PRODUCTION]"
                    } else {
                        ""
                    };
                    lines.push(format!(
                        "  • {} → {}{}",
                        daemon.alias, daemon.url, prod_marker
                    ));
                }

                jsonrpc_success_text(id, lines.join("\n"))
            }
            Err(e) => jsonrpc_error(id, -32603, format!("Failed to load daemons config: {}", e)),
        }
    }

    /// Release all of this bridge's connection references on the specified daemon
    ///
    /// Calls the user-scoped `/api/v1/connections/release` endpoint with the bridge's client ID.
    /// Returns (references_released, connections_disconnected).
    async fn release_my_references_on_daemon(&self, daemon_url: &str) -> Result<(u64, u64)> {
        let release_url = format!("{}/api/v1/connections/release", daemon_url);
        let response = self
            .client
            .post(&release_url)
            .header(detrix_api::common::CLIENT_ID_HEADER, &self.client_id)
            .send()
            .await
            .context("Failed to release connection references")?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to release connection references: {}",
                response.status()
            );
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse release response")?;

        let released = result
            .get("referencesReleased")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let disconnected = result
            .get("connectionsDisconnected")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        Ok((released, disconnected))
    }

    /// Disable metrics created by this bridge on the specified daemon
    ///
    /// Lists all metrics and disables only those with `createdBy` matching this bridge's client ID.
    /// Returns the number of metrics successfully disabled.
    async fn disable_my_metrics_on_daemon(&self, daemon_url: &str) -> Result<usize> {
        // List all metrics
        let list_url = format!("{}/api/v1/metrics", daemon_url);
        let response = self
            .client
            .get(&list_url)
            .send()
            .await
            .context("Failed to list metrics")?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to list metrics: {}", response.status());
        }

        let metrics: Vec<serde_json::Value> = response
            .json()
            .await
            .context("Failed to parse metrics response")?;

        // Disable only metrics created by this bridge
        let mut disabled_count = 0;
        for metric in metrics {
            let created_by = metric.get("createdBy").and_then(|v| v.as_str());
            if created_by != Some(&self.client_id) {
                continue;
            }
            if let Some(metric_id) = metric.get("metricId").and_then(|v| v.as_str()) {
                let disable_url = format!("{}/api/v1/metrics/{}/disable", daemon_url, metric_id);
                match self.client.post(&disable_url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        disabled_count += 1;
                    }
                    Ok(resp) => {
                        warn!(metric_id, status = %resp.status(), "Failed to disable metric");
                    }
                    Err(e) => {
                        warn!(metric_id, error = %e, "Failed to disable metric");
                    }
                }
            }
        }

        Ok(disabled_count)
    }

    /// Force disconnect all connections on the specified daemon (admin)
    ///
    /// Calls the /api/v1/disconnect_all endpoint. This is the old non-scoped behavior.
    /// Only used for admin/force operations.
    #[allow(dead_code)]
    async fn force_disconnect_all_on_daemon(&self, daemon_url: &str) -> Result<usize> {
        let disconnect_url = format!("{}/api/v1/disconnect_all", daemon_url);
        let response = self
            .client
            .post(&disconnect_url)
            .send()
            .await
            .context("Failed to disconnect all")?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to disconnect all: {}", response.status());
        }

        let result: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse disconnect response")?;

        let count = result
            .get("adaptersStopped")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        Ok(count)
    }

    /// Auto-attach to a connection found in a forwarded response
    ///
    /// Inspects the response for `connectionId` and attaches if not already tracked.
    async fn auto_attach_from_response(&self, response: &Value) {
        if let Some(conn_id) = extract_connection_id_from_response(response) {
            let already_attached = self.attached_connections.read().await.contains(&conn_id);
            if !already_attached {
                let daemon_url = self.daemon_url.read().await.clone();
                let attach_url = format!("{}/api/v1/connections/{}/attach", daemon_url, conn_id);
                match self
                    .client
                    .post(&attach_url)
                    .header(detrix_api::common::CLIENT_ID_HEADER, &self.client_id)
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        self.attached_connections
                            .write()
                            .await
                            .insert(conn_id.clone());
                        debug!("Auto-attached to connection {}", conn_id);
                    }
                    Ok(resp) => {
                        debug!(
                            "Failed to auto-attach to connection {}: {}",
                            conn_id,
                            resp.status()
                        );
                    }
                    Err(e) => {
                        debug!("Failed to auto-attach to connection {}: {}", conn_id, e);
                    }
                }
            }
        }
    }

    /// Switch to a different daemon URL at runtime
    ///
    /// File server behavior: The file server keeps running on the same port.
    /// Only the advertised host changes (new_file_server_host). This allows
    /// the new daemon to fetch files from the bridge's file server.
    ///
    /// # Arguments
    /// * `new_url` - The new daemon URL to switch to
    /// * `new_file_server_host` - Optional new host to advertise for file server
    pub async fn switch_daemon(
        &self,
        new_url: String,
        new_file_server_host: Option<String>,
    ) -> Result<(), String> {
        // Normalize URL: trim trailing slashes to prevent double-slash issues
        // (e.g., Pydantic AnyUrl adds trailing slash: "http://host:8090/")
        let new_url = new_url.trim_end_matches('/').to_string();

        // Validate URL is reachable
        let health_url = format!("{}/health", new_url);
        let resp = self
            .client
            .get(&health_url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("Failed to reach daemon at {}: {}", new_url, e))?;

        if !resp.status().is_success() {
            return Err(format!("Daemon health check failed: {}", resp.status()));
        }

        // Update daemon URL
        let mut url_guard = self.daemon_url.write().await;
        *url_guard = new_url.clone();
        drop(url_guard);

        // Update file server host if provided
        if let Some(host) = new_file_server_host {
            let mut host_guard = self.file_server_host.write().await;
            *host_guard = host;
        }

        info!("Switched to new daemon: {}", new_url);
        Ok(())
    }

    /// Get the current daemon URL
    pub async fn get_current_daemon_url(&self) -> String {
        self.daemon_url.read().await.clone()
    }

    /// Forward a request to the daemon, with wake fallback on failure.
    ///
    /// If the forwarded request fails and it was a `wake` tool call with an
    /// `app_url`, tries waking the app directly as a fallback.
    ///
    /// Fallback triggers on both transport errors (daemon unreachable) AND
    /// JSON-RPC errors from the daemon (e.g., daemon can't reach app due to
    /// auth mismatch in Docker/cloud setups).
    async fn forward_or_fallback(&self, request: &Value) -> Value {
        let result = self.forward_request(request.clone()).await;

        // Happy path: forward succeeded with a non-error response
        if let Ok(r) = result {
            if !is_jsonrpc_error(&r) {
                self.auto_attach_from_response(&r).await;
                return r;
            }

            // Daemon returned a JSON-RPC error — for wake calls, try direct fallback.
            // This handles the case where the local daemon can't reach the app
            // (e.g., auth mismatch when app is on a different daemon in Docker/cloud).
            if let Some(app_url) = extract_wake_app_url(request) {
                let error_msg = r
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                info!(
                    "Forwarded wake returned JSON-RPC error ({}), trying direct app wake at {}",
                    error_msg, app_url
                );
                if let Some(r) = self.try_direct_app_wake(&app_url, id.clone()).await {
                    self.auto_attach_from_response(&r).await;
                    return r;
                }
                return jsonrpc_error(
                    id,
                    -32603,
                    format!(
                        "Wake failed via daemon ({}) and direct app wake",
                        error_msg
                    ),
                );
            }

            // Non-wake JSON-RPC error — return as-is
            return r;
        }
        let e = result.unwrap_err();

        // Wake fallback: if this was a wake tool call, try direct app wake
        if let Some(app_url) = extract_wake_app_url(request) {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            info!(
                "Forwarded wake failed ({}), trying direct app wake at {}",
                e, app_url
            );
            if let Some(r) = self.try_direct_app_wake(&app_url, id.clone()).await {
                self.auto_attach_from_response(&r).await;
                return r;
            }
            return jsonrpc_error(
                id,
                -32603,
                format!("Wake failed via daemon ({}) and direct app wake", e),
            );
        }

        error!("Failed to forward request: {}", e);
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        jsonrpc_error(id, -32603, format!("Internal error: {}", e))
    }

    /// Check a wake response for `daemon_url` and auto-switch if present.
    ///
    /// This enables auto-discovery for Docker/cloud debugging:
    /// the daemon returns its `advertise_url` in the wake response, and the
    /// bridge auto-switches to it so the agent can observe without manual
    /// `switch_daemon`.
    async fn maybe_auto_switch_daemon(&self, response: &mut Value) {
        let Some(daemon_url) = extract_daemon_url_from_response(response) else {
            return;
        };
        let current_url = self.get_current_daemon_url().await;
        // Normalize trailing slashes for comparison (Pydantic AnyUrl adds trailing slash)
        if daemon_url.trim_end_matches('/') == current_url.trim_end_matches('/') {
            return;
        }

        info!(
            "Wake response contains daemon_url={}, auto-switching from {}",
            daemon_url, current_url
        );
        if let Err(e) = self.switch_daemon(daemon_url.clone(), None).await {
            warn!("Failed to auto-switch to daemon at {}: {}", daemon_url, e);
            return;
        }

        info!("Auto-switched to daemon at {}", daemon_url);
        append_to_response_text(
            response,
            format!("\n\nAuto-switched to daemon at {}", daemon_url),
        );
    }

    /// Try to wake an app directly (fallback when forwarded wake through daemon fails).
    ///
    /// Sends POST to `{app_url}/detrix/wake` and builds an MCP JSON-RPC response.
    async fn try_direct_app_wake(&self, app_url: &str, request_id: Value) -> Option<Value> {
        let wake_url = format!("{}/detrix/wake", app_url.trim_end_matches('/'));
        info!("Attempting direct app wake at {}", wake_url);

        let resp = match self
            .client
            .post(&wake_url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!("Direct app wake failed: {}", e);
                return None;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!("Direct app wake failed: status {} - {}", status, body);
            return None;
        }

        let wake_data: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse direct wake response: {}", e);
                return None;
            }
        };

        info!("Direct app wake succeeded: {:?}", wake_data);

        // Build MCP JSON-RPC response matching what the daemon's wake tool would return
        let result_json = serde_json::json!({
            "status": wake_data.get("status").and_then(|v| v.as_str()).unwrap_or("awake"),
            "debug_port": wake_data.get("debug_port").and_then(|v| v.as_i64()).unwrap_or(0),
            "connection_id": wake_data.get("connection_id").and_then(|v| v.as_str()).unwrap_or(""),
            "daemon_url": wake_data.get("daemon_url").and_then(|v| v.as_str()),
        });

        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "result": {
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&result_json).unwrap_or_default()
                }]
            },
            "id": request_id
        }))
    }
}

// ============================================================================
// Response Inspection Helpers
// ============================================================================

/// Extract a connection ID from a daemon's JSON-RPC response
///
/// Looks for `connectionId` in the result content text (parsed as JSON).
fn extract_connection_id_from_response(response: &Value) -> Option<String> {
    let contents = response.pointer("/result/content")?.as_array()?;
    contents.iter().find_map(|item| {
        let text = item.get("text")?.as_str()?;
        let parsed: Value = serde_json::from_str(text).ok()?;
        parsed
            .get("connectionId")
            .or_else(|| parsed.get("connection_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

/// Extract `daemon_url` from a wake response's JSON-RPC result content.
fn extract_daemon_url_from_response(response: &Value) -> Option<String> {
    let contents = response.pointer("/result/content")?.as_array()?;
    contents.iter().find_map(|item| {
        let text = item.get("text")?.as_str()?;
        let parsed: Value = serde_json::from_str(text).ok()?;
        parsed
            .get("daemon_url")
            .or_else(|| parsed.get("daemonUrl"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    })
}

/// Append text to the first text content item in a JSON-RPC response.
fn append_to_response_text(response: &mut Value, text: String) {
    if let Some(contents) = response
        .pointer_mut("/result/content")
        .and_then(|v| v.as_array_mut())
    {
        if let Some(first) = contents.first_mut() {
            if let Some(existing) = first.get("text").and_then(|v| v.as_str()) {
                let new_text = format!("{}{}", existing, text);
                first["text"] = Value::String(new_text);
            }
        }
    }
}

/// Check if a JSON-RPC request is a `wake` tool call and extract the `app_url` argument.
fn extract_wake_app_url(request: &Value) -> Option<String> {
    if request.get("method")?.as_str()? != "tools/call" {
        return None;
    }
    let params = request.get("params")?;
    let tool_name = params.get("name")?.as_str()?;
    if tool_name != "wake" {
        return None;
    }
    let arguments = params.get("arguments")?;
    arguments
        .get("app_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Check if a JSON-RPC response is an error response.
fn is_jsonrpc_error(response: &Value) -> bool {
    response.get("error").is_some()
}

// ============================================================================
// JSON-RPC Response Helpers
// ============================================================================

/// Create a JSON-RPC success response with text content
fn jsonrpc_success_text(id: Value, text: impl Into<String>) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "result": {
            "content": [{
                "type": "text",
                "text": text.into()
            }]
        },
        "id": id
    })
}

/// Create a JSON-RPC error response
fn jsonrpc_error(id: Value, code: i32, message: impl Into<String>) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "error": {
            "code": code,
            "message": message.into()
        },
        "id": id
    })
}
