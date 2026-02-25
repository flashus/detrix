//! MCP bridge core implementation
//!
//! Contains the McpBridge struct and its methods for forwarding requests,
//! managing heartbeats, and handling daemon communication.

use super::auth::{
    discover_auth_token, extract_host_port, is_connection_error, resolve_token_for_host,
};
use super::config::{BridgeConfig, RestartBackoff};
use anyhow::{Context, Result};
use detrix_config::constants::{
    AUTHORIZATION_HEADER, BEARER_PREFIX, DOCKER_INTERNAL_HOST, HEADER_BRIDGE_PID,
    HEADER_FILE_SERVER_TOKEN, HEADER_FILE_SERVER_URL, HEADER_PARENT_NAME, HEADER_PARENT_PID,
};
use detrix_core::UNKNOWN_WORKSPACE_ROOT;
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
    /// URL advertised to the daemon for fetching source files.
    /// Rebuilt from `file_server_port` + `file_server_host` when either changes.
    file_server_url: RwLock<Option<String>>,
    /// Actual bound port of the file server (0 = not started).
    file_server_port: RwLock<u16>,
    /// Host to advertise in the file server URL sent to the daemon.
    /// Defaults to "127.0.0.1" for local daemons; auto-switched to
    /// "host.docker.internal" when discovery switches to a Docker daemon.
    file_server_host: RwLock<String>,
    /// IP allowlist for the file server. Shared with the running server so it
    /// can be updated at runtime when the daemon changes.
    ///
    /// - `Some(set)`: only IPs in this set are allowed (resolved from daemon URL).
    ///   Loopback addresses are always included.
    /// - `None`: any IP is allowed — used for Docker where the daemon container
    ///   connects from an unpredictable bridge IP. Token auth guards the endpoint.
    allowed_file_server_ips: Arc<RwLock<Option<std::collections::HashSet<std::net::IpAddr>>>>,
    /// Container→host source path prefix mapping for Docker connections.
    ///
    /// When a Docker daemon sends file requests, paths are container-internal
    /// (e.g. `/src/examples/app/main.go`). This mapping rewrites them to the
    /// host-side equivalent so the file server can find the source files.
    ///
    /// - `None` — no rewriting (local daemon, paths are already host paths).
    /// - `Some((container_prefix, host_prefix))` — rewrite on every file request.
    source_prefix_map: super::file_server::SourcePrefixMap,
    /// Auth token used to protect the bridge file server.
    /// Sent to the daemon as `X-Detrix-File-Server-Token` so it can authenticate
    /// when fetching files (especially after switching to a Docker/remote daemon).
    file_server_token: RwLock<Option<String>>,
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

        // Seed the IP allowlist from the initial daemon URL (synchronous best-effort).
        // Loopback IPs are always allowed; if resolution fails we start with loopback only.
        // Unknown daemon IPs (e.g. Docker bridge) get learned on first successful token auth.
        let initial_ips = resolve_daemon_ips_sync(&config.daemon_url);

        Ok(Self {
            config,
            client,
            client_id,
            daemon_url,
            auth_token,
            restart_backoff,
            file_server_url: RwLock::new(None),
            file_server_port: RwLock::new(0),
            file_server_host: RwLock::new(file_server_host_val),
            file_server_token: RwLock::new(None),
            allowed_file_server_ips: Arc::new(RwLock::new(Some(initial_ips))),
            source_prefix_map: Arc::new(RwLock::new(None)),
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
        let (url, token, file_server, fs_token) = {
            let daemon_url = self.daemon_url.read().await;
            let auth_token = self.auth_token.read().await;
            let file_server_url = self.file_server_url.read().await;
            let file_server_token = self.file_server_token.read().await;
            (
                format!("{}/mcp", daemon_url),
                auth_token.clone(),
                file_server_url.clone(),
                file_server_token.clone(),
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
                .header(HEADER_PARENT_PID, parent.pid.to_string())
                .header(HEADER_PARENT_NAME, &parent.name)
                .header(HEADER_BRIDGE_PID, parent.bridge_pid.to_string());
        }

        // Add file server URL + token so daemon can fetch source files from this machine.
        // The token is the same one the file server was started with; without it the
        // daemon (especially a Docker daemon) would get 401 from the file server.
        if let Some(ref url) = file_server {
            req_builder = req_builder.header(HEADER_FILE_SERVER_URL, url);
            if let Some(ref t) = fs_token {
                req_builder = req_builder.header(HEADER_FILE_SERVER_TOKEN, t);
            }
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
                .header(HEADER_PARENT_PID, parent.pid.to_string())
                .header(HEADER_PARENT_NAME, &parent.name)
                .header(HEADER_BRIDGE_PID, parent.bridge_pid.to_string());
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

        // Start file server bound to 0.0.0.0 so Docker daemons can reach it via
        // host.docker.internal. Advertise with the configured host (default: 127.0.0.1).
        let auth_token_for_fs = self.auth_token.read().await.clone();
        let allowed_ips = Arc::clone(&self.allowed_file_server_ips);
        let prefix_map = Arc::clone(&self.source_prefix_map);
        match super::file_server::start_file_server(
            auth_token_for_fs.clone(),
            allowed_ips,
            prefix_map,
        )
        .await
        {
            Ok(port) => {
                let host = self.file_server_host.read().await.clone();
                let url = format!("http://{}:{}", host, port);
                info!(url = %url, "Bridge file server ready");
                *self.file_server_port.write().await = port;
                *self.file_server_url.write().await = Some(url);
                // Store token so it can be forwarded to the daemon via
                // X-Detrix-File-Server-Token on every request.
                *self.file_server_token.write().await = auth_token_for_fs;
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
        let token = self.auth_token.read().await.clone();
        let release_url = format!("{}/api/v1/connections/release", daemon_url);
        let mut req_builder = self
            .client
            .post(&release_url)
            .header(detrix_api::common::CLIENT_ID_HEADER, &self.client_id);
        if let Some(ref token) = token {
            req_builder =
                req_builder.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token));
        }
        let response = req_builder
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
        let token = self.auth_token.read().await.clone();

        // List all metrics
        let list_url = format!("{}/api/v1/metrics", daemon_url);
        let mut req_builder = self.client.get(&list_url);
        if let Some(ref token) = token {
            req_builder =
                req_builder.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token));
        }
        let response = req_builder.send().await.context("Failed to list metrics")?;

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
                let mut disable_req = self.client.post(&disable_url);
                if let Some(ref token) = token {
                    disable_req = disable_req
                        .header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token));
                }
                match disable_req.send().await {
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
        let token = self.auth_token.read().await.clone();
        let disconnect_url = format!("{}/api/v1/disconnect_all", daemon_url);
        let mut req_builder = self.client.post(&disconnect_url);
        if let Some(ref token) = token {
            req_builder =
                req_builder.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token));
        }
        let response = req_builder
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

    /// Detect the container→host source path mapping for a Docker connection.
    ///
    /// After waking a Docker app, the connection's `workspace_root` is a container-
    /// internal path (e.g. `/src/examples/app`). The file server runs on the host
    /// and needs to translate this to a host path.
    ///
    /// Algorithm: walk from the longest suffix of `container_workspace` down to 1
    /// component. For each suffix, check if `<host_cwd>/<suffix>` exists as a
    /// directory. The first match gives us:
    ///   - container prefix = everything before the matching suffix
    ///   - host prefix = host_cwd
    ///
    /// Example:
    ///   container: `/src/examples/docker-demo/client-app`
    ///   host_cwd:  `/Users/me/detrix-release`
    ///   → finds `/Users/me/detrix-release/examples/docker-demo/client-app` exists
    ///   → container_prefix = `/src`, host_prefix = `/Users/me/detrix-release`
    fn find_container_prefix_mapping(
        container_workspace: &str,
        host_cwd: &str,
    ) -> Option<(String, String)> {
        use std::path::{Component, Path, PathBuf};

        let comps: Vec<Component> = Path::new(container_workspace).components().collect();
        let n = comps.len();
        let host_path = Path::new(host_cwd);

        // Strategy 1: host_cwd is a PARENT of the container workspace on the host.
        //
        // Append decreasing suffixes of the container path to host_cwd and check
        // whether the resulting directory exists.
        //
        // Example: container=/src/examples/app, host_cwd=/home/user
        //   → try /home/user/src/examples/app  (suffix_len=3)
        //   → try /home/user/examples/app       (suffix_len=2) ← exists → mapping: /src → /home/user
        for suffix_len in (1..n).rev() {
            let suffix_start = n - suffix_len;
            let mut candidate = host_path.to_path_buf();
            for comp in &comps[suffix_start..] {
                candidate.push(comp);
            }
            if candidate.is_dir() {
                let container_prefix = comps[..suffix_start]
                    .iter()
                    .fold(PathBuf::new(), |mut p, c| {
                        p.push(c);
                        p
                    })
                    .to_string_lossy()
                    .into_owned();
                debug!(
                    container_prefix = %container_prefix,
                    host_prefix = %host_cwd,
                    "Auto-detected Docker source path mapping (parent strategy)"
                );
                return Some((container_prefix, host_cwd.to_string()));
            }
        }

        // Strategy 2: host_cwd IS the container workspace (or shares its trailing
        // components). Used when the MCP bridge is launched from inside the project
        // directory (e.g. Claude Code opened examples/docker-demo/client-app/).
        //
        // Find the longest common suffix of path components between the container
        // path and host_cwd, then strip it from both to get the prefixes.
        //
        // Example: container=/src/examples/app, host_cwd=/home/user/examples/app
        //   common suffix: examples/app (len=2)
        //   container prefix: /src, host prefix: /home/user → mapping: /src → /home/user
        let host_comps: Vec<Component> = host_path.components().collect();
        let hn = host_comps.len();
        // Keep at least 1 component on each side so the prefix is non-trivial.
        let max_suffix = (n - 1).min(hn - 1);
        let mut match_len = 0;
        for i in 1..=max_suffix {
            if comps[n - i] == host_comps[hn - i] {
                match_len = i;
            } else {
                break;
            }
        }
        if match_len > 0 {
            let container_prefix_comps = &comps[..n - match_len];
            let host_prefix_comps = &host_comps[..hn - match_len];
            if !container_prefix_comps.is_empty() && !host_prefix_comps.is_empty() {
                let container_prefix = container_prefix_comps
                    .iter()
                    .fold(PathBuf::new(), |mut p, c| {
                        p.push(c);
                        p
                    })
                    .to_string_lossy()
                    .into_owned();
                let host_prefix = host_prefix_comps
                    .iter()
                    .fold(PathBuf::new(), |mut p, c| {
                        p.push(c);
                        p
                    })
                    .to_string_lossy()
                    .into_owned();
                debug!(
                    container_prefix = %container_prefix,
                    host_prefix = %host_prefix,
                    "Auto-detected Docker source path mapping (suffix-match strategy)"
                );
                return Some((container_prefix, host_prefix));
            }
        }

        None
    }

    /// Fetch a connection's workspace_root from the current daemon and, if it looks
    /// like a container path, store a host path mapping in `source_prefix_map`.
    ///
    /// Called after a successful Docker app wake so that subsequent `observe` /
    /// `inspect_file` calls can find source files on the host.
    async fn detect_and_store_prefix_mapping(&self, connection_id: &str) {
        self.detect_and_store_prefix_mapping_inner(connection_id, None)
            .await;
    }

    /// Inner implementation that accepts an optional `host_cwd` override for testing.
    async fn detect_and_store_prefix_mapping_inner(
        &self,
        connection_id: &str,
        host_cwd_override: Option<&str>,
    ) {
        let daemon_url = self.get_current_daemon_url().await;
        let url = format!(
            "{}/api/v1/connections/{}",
            daemon_url.trim_end_matches('/'),
            connection_id
        );

        let token = self.auth_token.read().await.clone();
        let mut req = self.client.get(&url);
        if let Some(tok) = &token {
            req = req.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, tok));
        }

        let workspace_root = match req.send().await {
            Ok(r) if r.status().is_success() => {
                r.json::<serde_json::Value>().await.ok().and_then(|v| {
                    v.get("workspaceRoot")
                        .and_then(|s| s.as_str())
                        .map(String::from)
                })
            }
            Ok(r) => {
                warn!(
                    status = r.status().as_u16(),
                    conn_id = connection_id,
                    "Failed to fetch connection for path mapping (auth issue?)"
                );
                return;
            }
            Err(e) => {
                warn!(error = %e, conn_id = connection_id, "Network error fetching connection for path mapping");
                return;
            }
        };

        let Some(container_workspace) =
            workspace_root.filter(|r| !r.is_empty() && r != "/" && r != UNKNOWN_WORKSPACE_ROOT)
        else {
            debug!(
                conn_id = connection_id,
                "Connection has no usable workspaceRoot — skipping path mapping \
                 (file server will use lazy CWD-based fallback on first file request)"
            );
            return;
        };

        let host_cwd = if let Some(override_cwd) = host_cwd_override {
            override_cwd.to_string()
        } else {
            match std::env::current_dir() {
                Ok(p) => p.to_string_lossy().into_owned(),
                Err(_) => return,
            }
        };

        if let Some(mapping) = Self::find_container_prefix_mapping(&container_workspace, &host_cwd)
        {
            info!(
                container_prefix = %mapping.0,
                host_prefix = %mapping.1,
                "Docker source path mapping stored"
            );
            *self.source_prefix_map.write().await = Some(mapping);
        } else {
            debug!(
                container_workspace = %container_workspace,
                host_cwd = %host_cwd,
                "No Docker source path mapping found (paths don't share a common suffix)"
            );
        }
    }

    /// Fetch the first active connection from a Docker daemon and detect the
    /// container→host source path prefix mapping.
    ///
    /// Called automatically when switching to a Docker daemon so that file requests
    /// (observe / inspect_file) work even without an explicit `wake` call in the
    /// current session — i.e., when the Docker container is already running from
    /// a previous session.
    async fn detect_prefix_from_active_connections(&self, daemon_url: &str) {
        let url = format!("{}/api/v1/connections?active_only=true", daemon_url);

        let token = self.auth_token.read().await.clone();
        let mut req = self.client.get(&url);
        if let Some(tok) = &token {
            req = req.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, tok));
        }

        let conn_id = match req.timeout(std::time::Duration::from_secs(3)).send().await {
            Ok(r) if r.status().is_success() => {
                r.json::<serde_json::Value>().await.ok().and_then(|v| {
                    v.as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|c| c.get("connectionId"))
                        .and_then(|s| s.as_str())
                        .map(String::from)
                })
            }
            Ok(r) => {
                warn!(
                    status = r.status().as_u16(),
                    daemon_url,
                    "Failed to list connections for path mapping (401=missing credentials, add with: detrix auth add <host:port> --token <token>)"
                );
                return;
            }
            Err(e) => {
                debug!(error = %e, daemon_url, "Network error listing connections for path mapping");
                return;
            }
        };

        let Some(conn_id) = conn_id else {
            debug!(
                daemon_url,
                "No active connections on Docker daemon — path mapping deferred until wake"
            );
            return;
        };

        debug!(
            conn_id = %conn_id,
            "Auto-detecting source prefix map from existing Docker connection"
        );
        self.detect_and_store_prefix_mapping(&conn_id).await;
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

        // Resolve and update auth token for the new daemon.
        // Always update (even to None) so we don't accidentally forward the
        // previous daemon's token to the new one.
        if let Some(hp) = extract_host_port(&new_url) {
            let token = resolve_token_for_host(&hp, false);
            if token.is_none() {
                warn!(
                    "No credentials found for {}. Requests may fail with 401. \
                     Add credentials with: detrix auth add {} --token <token>",
                    hp, hp
                );
            }
            *self.auth_token.write().await = token;
        }

        // Always update the IP allowlist for the new daemon, even if the file server
        // host didn't change. This ensures any daemon switch (post-wake, auto-switch)
        // keeps the allowlist in sync with the actual daemon.
        // Note: for Docker, the daemon connects from an unpredictable bridge IP — the
        // file server will learn it on the first successful token-authenticated request.
        let is_docker_host = new_file_server_host.as_deref() == Some(DOCKER_INTERNAL_HOST);
        let new_allowed = resolve_daemon_ips_sync(&new_url);
        *self.allowed_file_server_ips.write().await = Some(new_allowed);

        // Update the source prefix map based on the new daemon type:
        // - Leaving Docker → clear the map (container paths won't apply to local daemon).
        // - Entering Docker → proactively detect from existing active connections so that
        //   observe/inspect_file work even in a new session without calling wake first.
        if is_docker_host {
            self.detect_prefix_from_active_connections(&new_url).await;
        } else {
            *self.source_prefix_map.write().await = None;
        }

        // Update file server host and rebuild advertised URL if host changed.
        if let Some(host) = new_file_server_host {
            *self.file_server_host.write().await = host.clone();
            let port = *self.file_server_port.read().await;
            if port > 0 {
                let new_fs_url = format!("http://{}:{}", host, port);
                info!(url = %new_fs_url, "File server advertise URL updated");
                *self.file_server_url.write().await = Some(new_fs_url);
            }
        }

        info!("Switched to new daemon: {}", new_url);
        Ok(())
    }

    /// Get the current daemon URL
    pub async fn get_current_daemon_url(&self) -> String {
        self.daemon_url.read().await.clone()
    }

    /// Forward a request to the daemon, with discovery-first flow for wake requests.
    ///
    /// For wake requests with `app_url`: discovers the app's daemon via `/detrix/discover`,
    /// looks up credentials, switches daemon if needed, then forwards through the daemon.
    ///
    /// For non-wake requests: forwards directly to the current daemon.
    pub(crate) async fn forward_or_fallback(&self, request: &Value) -> Value {
        // For wake requests: use discovery-first flow
        if let Some(app_url) = extract_wake_app_url(request) {
            return self.handle_wake_with_discovery(request, &app_url).await;
        }

        // Non-wake: forward normally
        let result = self.forward_request(request.clone()).await;

        match result {
            Ok(r) => {
                if !is_jsonrpc_error(&r) {
                    self.auto_attach_from_response(&r).await;
                }
                r
            }
            Err(e) => {
                error!("Failed to forward request: {}", e);
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                jsonrpc_error(id, -32603, format!("Internal error: {}", e))
            }
        }
    }

    /// Discovery-first wake flow.
    ///
    /// Key insight: the app registers its debugger with its *own* configured
    /// daemon (e.g. the Docker daemon), but the bridge's current daemon is the
    /// one reachable from the host. We must:
    ///   1. Discover which daemon the app will register with
    ///   2. Forward the wake to the CURRENT daemon (which CAN reach app_url from
    ///      the host network — e.g. localhost:8091 published from a Docker container)
    ///   3. AFTER wake succeeds, switch to the app's daemon so the connection
    ///      is on the right daemon for subsequent observe/metric calls
    ///
    /// `control_plane_url` in the discover response (new field) overrides step 2:
    /// when present it means the current daemon can also reach the app via that URL
    /// (Docker-internal name like http://order-service:8091). In that case we switch
    /// first and let the remote daemon handle the wake call.
    async fn handle_wake_with_discovery(&self, request: &Value, app_url: &str) -> Value {
        let id = request.get("id").cloned().unwrap_or(Value::Null);

        // 1. Discover which daemon the app uses (and optional daemon-visible URL)
        let discovery = self.discover_app_daemon(app_url).await;

        let target_daemon = discovery.as_ref().map(|(url, _, _)| url.as_str());
        let control_plane_url = discovery.as_ref().and_then(|(_, _, cp)| cp.as_deref());

        // 2. Decide how to route the wake:
        //    - If control_plane_url is provided, the daemon-internal URL is known →
        //      switch first, then let the remote daemon call control_plane_url.
        //    - Otherwise, wake through the CURRENT daemon (host-reachable app_url),
        //      then switch so the connection appears on the right daemon.
        let switch_before_wake = control_plane_url.is_some();

        if switch_before_wake {
            // Daemon knows how to reach the app (control_plane_url supplied)
            let cp_url = control_plane_url.unwrap();
            if let Some((daemon_url, app_name, _)) = &discovery {
                let current = self.get_current_daemon_url().await;
                if daemon_url.trim_end_matches('/') != current.trim_end_matches('/') {
                    info!(
                        "Discovery: {} uses daemon {}, switching from {} (control_plane_url={})",
                        app_name, daemon_url, current, cp_url
                    );
                    // control_plane_url is present → daemon is inside Docker.
                    // Advertise host.docker.internal so the Docker daemon can reach
                    // our file server, and open the IP restriction.
                    if let Err(e) = self
                        .switch_daemon(daemon_url.clone(), Some(DOCKER_INTERNAL_HOST.to_string()))
                        .await
                    {
                        warn!("Failed to switch to Docker daemon {}: {}", daemon_url, e);
                    }
                }
            }
            // Rewrite app_url with the daemon-visible URL (it lives in params.arguments.app_url)
            let mut wake_request = request.clone();
            if cp_url != app_url {
                if let Some(params) = wake_request.get_mut("params") {
                    if let Some(arguments) = params.get_mut("arguments") {
                        if let Some(obj) = arguments.as_object_mut() {
                            obj.insert("app_url".to_string(), Value::String(cp_url.to_string()));
                        }
                    }
                }
            }
            return match self.forward_request(wake_request).await {
                Ok(r) if !is_jsonrpc_error(&r) => {
                    self.auto_attach_from_response(&r).await;
                    // Docker wake succeeded: detect container→host path mapping so
                    // inspect_file / observe can find source files on the host.
                    if let Some(conn_id) = extract_connection_id_from_response(&r) {
                        self.detect_and_store_prefix_mapping(&conn_id).await;
                    }
                    r
                }
                Ok(r) => r,
                Err(e) => jsonrpc_error(id, -32603, format!("Wake failed: {}", e)),
            };
        }

        // Wake through CURRENT daemon (host-visible app_url works from here)
        let wake_result = match self.forward_request(request.clone()).await {
            Ok(r) if !is_jsonrpc_error(&r) => Ok(r),
            Ok(r) => return r,
            Err(e) => Err(jsonrpc_error(id, -32603, format!("Wake failed: {}", e))),
        };
        let wake_response = match wake_result {
            Ok(r) => r,
            Err(e) => return e,
        };

        // Now switch to the app's daemon so subsequent calls go to the right place
        if let Some(daemon_url) = target_daemon {
            let current = self.get_current_daemon_url().await;
            if daemon_url.trim_end_matches('/') != current.trim_end_matches('/') {
                let app_name = discovery
                    .as_ref()
                    .map(|(_, n, _)| n.as_str())
                    .unwrap_or("app");
                info!(
                    "Post-wake: {} registered with {}, switching from {}",
                    app_name, daemon_url, current
                );
                if let Err(e) = self.switch_daemon(daemon_url.to_string(), None).await {
                    warn!("Failed to switch to post-wake daemon {}: {}", daemon_url, e);
                }
            }
        } else {
            debug!(
                "Discovery failed for {}, forwarding to current daemon",
                app_url
            );
        }

        // Auto-attach on the (now-switched) daemon
        self.auto_attach_from_response(&wake_response).await;
        wake_response
    }

    /// Discover which daemon an app uses by calling its `/detrix/discover` endpoint.
    ///
    /// Returns `(daemon_url, app_name, control_plane_url)` on success.
    /// `control_plane_url` is the daemon-visible URL of the app's control plane
    /// (may differ from the user-visible URL in Docker/cloud deployments).
    /// This endpoint requires no authentication.
    pub(crate) async fn discover_app_daemon(
        &self,
        app_url: &str,
    ) -> Option<(String, String, Option<String>)> {
        let url = format!("{}/detrix/discover", app_url.trim_end_matches('/'));
        debug!("Discovering app daemon at {}", url);

        let resp = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            debug!("Discovery endpoint returned {}", resp.status());
            return None;
        }

        let data: Value = resp.json().await.ok()?;
        let daemon_url = data["daemon_url"].as_str()?.to_string();
        let name = data["name"].as_str().unwrap_or("unknown").to_string();
        let control_plane_url = data["control_plane_url"].as_str().map(|s| s.to_string());
        Some((daemon_url, name, control_plane_url))
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
}

// ============================================================================
// File Server IP Resolution
// ============================================================================

/// Resolve the hostname from a daemon URL to a set of IPs, always including loopback.
///
/// Used to seed/update the file server IP allowlist when the daemon changes.
/// Falls back to loopback-only set on resolution failure.
fn resolve_daemon_ips_sync(daemon_url: &str) -> std::collections::HashSet<std::net::IpAddr> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
    let mut ips = std::collections::HashSet::new();
    // Always allow loopback.
    ips.insert(IpAddr::V4(Ipv4Addr::LOCALHOST));
    ips.insert(IpAddr::V6(Ipv6Addr::LOCALHOST));

    if let Some(host) = extract_host_only(daemon_url) {
        // Use (host, port) form of ToSocketAddrs for DNS resolution.
        if let Ok(addrs) = (host.as_str(), 0u16).to_socket_addrs() {
            for addr in addrs {
                ips.insert(addr.ip());
            }
        }
    }

    ips
}

/// Extract just the hostname (no port, no scheme, no path) from a URL.
fn extract_host_only(url: &str) -> Option<String> {
    // Strip scheme.
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    // Take authority (up to first '/').
    let authority = rest.split('/').next()?;
    // Strip port: for IPv6 [::1]:8090, strip brackets and port.
    let host = if authority.starts_with('[') {
        let end = authority.find(']')?;
        authority[1..end].to_string()
    } else {
        // host:port — take everything before the last ':'.
        match authority.rfind(':') {
            Some(colon) => authority[..colon].to_string(),
            None => authority.to_string(),
        }
    };
    Some(host)
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
pub(crate) fn extract_wake_app_url(request: &Value) -> Option<String> {
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

#[cfg(test)]
mod prefix_mapping_tests {
    use super::McpBridge;

    /// Create a temp directory tree and return the base path string.
    fn mk_tree(parts: &[&str]) -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = tmp.path().to_path_buf();
        for part in parts {
            p.push(part);
            std::fs::create_dir_all(&p).unwrap();
        }
        let base = tmp.path().to_string_lossy().into_owned();
        (tmp, base)
    }

    #[test]
    fn detects_typical_docker_mapping() {
        // Simulate: container workspace = /src/examples/docker-demo/client-app
        //           host cwd = /some/project  (which has examples/docker-demo/client-app)
        let (_tmp, host_cwd) = mk_tree(&["examples", "docker-demo", "client-app"]);
        let result = McpBridge::find_container_prefix_mapping(
            "/src/examples/docker-demo/client-app",
            &host_cwd,
        );
        let (container_prefix, host_prefix) = result.expect("Should detect mapping");
        assert_eq!(container_prefix, "/src");
        assert_eq!(host_prefix, host_cwd);
    }

    #[test]
    fn returns_none_when_no_match() {
        let (_tmp, host_cwd) = mk_tree(&["unrelated", "dir"]);
        let result = McpBridge::find_container_prefix_mapping(
            "/src/examples/docker-demo/client-app",
            &host_cwd,
        );
        assert!(
            result.is_none(),
            "Should return None when paths don't match"
        );
    }

    /// Strategy 2: host_cwd IS the container workspace (Claude Code opened the project dir).
    ///
    /// When the MCP bridge runs with CWD = examples/docker-demo/client-app (because
    /// Claude Code opened that directory), Strategy 1 fails (no subdirs match), so
    /// Strategy 2 strips the common suffix to derive the prefix.
    #[test]
    fn detects_mapping_when_host_cwd_is_container_workspace() {
        // Simulate: container workspace = /src/examples/docker-demo/client-app
        //           host cwd = /abs/detrix-release/examples/docker-demo/client-app
        //           (Claude Code opened the client-app directory directly)
        let (_tmp, base) = mk_tree(&["detrix-release", "examples", "docker-demo", "client-app"]);
        // host_cwd = the leaf (client-app), not the root
        let host_cwd = format!("{}/detrix-release/examples/docker-demo/client-app", base);
        let result = McpBridge::find_container_prefix_mapping(
            "/src/examples/docker-demo/client-app",
            &host_cwd,
        );
        let (container_prefix, host_prefix) = result.expect("Strategy 2 should detect mapping");
        assert_eq!(container_prefix, "/src");
        // host_prefix should be the parent that strips the common suffix
        assert!(
            host_prefix.ends_with("detrix-release"),
            "host_prefix ({host_prefix}) should end with the project root (detrix-release)"
        );
    }

    /// Strategy 2: single trailing component match.
    #[test]
    fn detects_mapping_single_suffix_component() {
        // container=/workspace/app, host_cwd=/home/user/app
        let (_tmp, base) = mk_tree(&["user", "app"]);
        let host_cwd = format!("{}/user/app", base);
        let result = McpBridge::find_container_prefix_mapping("/workspace/app", &host_cwd);
        let (container_prefix, host_prefix) = result.expect("Single suffix should match");
        assert_eq!(container_prefix, "/workspace");
        assert!(
            host_prefix.ends_with("user"),
            "host_prefix should be parent of app"
        );
    }

    #[test]
    fn shorter_common_suffix_works() {
        let (_tmp, host_cwd) = mk_tree(&["client-app"]);
        let result = McpBridge::find_container_prefix_mapping("/workspace/client-app", &host_cwd);
        let (container_prefix, _) = result.expect("Should find single-component suffix");
        assert_eq!(container_prefix, "/workspace");
    }

    // =========================================================================
    // Integration tests: detect_and_store_prefix_mapping_inner
    //
    // These tests spin up a minimal axum mock server that simulates the Detrix
    // daemon's GET /api/v1/connections/{id} endpoint.  The bridge fetches the
    // connection's workspaceRoot from this fake daemon and should auto-detect
    // the container→host prefix mapping.
    // =========================================================================

    /// Helper: start a one-shot axum mock server that serves a fixed JSON body at
    /// `GET /api/v1/connections/<conn_id>`.  Returns `(port, _keep_alive)`.
    /// The server lives until `_keep_alive` is dropped.
    async fn mock_connection_server(
        conn_id: &str,
        workspace_root: &str,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        use axum::{extract::Path, routing::get, Router};
        use std::sync::Arc;
        use tokio::net::TcpListener;

        let ws = Arc::new(workspace_root.to_string());
        let id = conn_id.to_string();

        let app = Router::new().route(
            "/api/v1/connections/{id}",
            get(move |Path(got_id): Path<String>| {
                let ws = Arc::clone(&ws);
                let expected = id.clone();
                async move {
                    if got_id == expected {
                        axum::Json(serde_json::json!({ "workspaceRoot": *ws }))
                    } else {
                        axum::Json(serde_json::json!({ "error": "not found" }))
                    }
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // Give the server a tick to bind
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        (port, handle)
    }

    fn make_bridge_for_daemon(daemon_url: &str) -> std::sync::Arc<McpBridge> {
        use super::super::config::BridgeConfig;
        let cfg = BridgeConfig {
            daemon_url: daemon_url.to_string(),
            ..Default::default()
        };
        std::sync::Arc::new(McpBridge::new(cfg).unwrap())
    }

    /// When the daemon returns a container workspace_root that shares a suffix
    /// with the host cwd, the prefix map should be populated.
    #[tokio::test]
    async fn detect_stores_prefix_map_on_match() {
        let (_tmp, host_cwd) = mk_tree(&["examples", "docker-demo", "client-app"]);
        let container_workspace = "/src/examples/docker-demo/client-app";
        let conn_id = "abc123";

        let (port, _server) = mock_connection_server(conn_id, container_workspace).await;
        let daemon_url = format!("http://127.0.0.1:{}", port);
        let bridge = make_bridge_for_daemon(&daemon_url);

        bridge
            .detect_and_store_prefix_mapping_inner(conn_id, Some(&host_cwd))
            .await;

        let map = bridge.source_prefix_map.read().await.clone();
        let (container_prefix, host_prefix) = map.expect("Prefix map should be set");
        assert_eq!(container_prefix, "/src");
        assert_eq!(host_prefix, host_cwd);
    }

    /// When no common suffix is found, the prefix map stays None.
    #[tokio::test]
    async fn detect_leaves_map_none_on_no_match() {
        let (_tmp, host_cwd) = mk_tree(&["unrelated", "dir"]);
        let container_workspace = "/src/examples/docker-demo/client-app";
        let conn_id = "abc456";

        let (port, _server) = mock_connection_server(conn_id, container_workspace).await;
        let daemon_url = format!("http://127.0.0.1:{}", port);
        let bridge = make_bridge_for_daemon(&daemon_url);

        bridge
            .detect_and_store_prefix_mapping_inner(conn_id, Some(&host_cwd))
            .await;

        let map = bridge.source_prefix_map.read().await.clone();
        assert!(map.is_none(), "No common suffix — map must stay None");
    }

    /// When the daemon returns a blank or /unknown workspace_root, no mapping is stored.
    #[tokio::test]
    async fn detect_ignores_unknown_workspace_root() {
        let (_tmp, host_cwd) = mk_tree(&["examples", "client-app"]);
        let conn_id = "abc789";

        let (port, _server) = mock_connection_server(conn_id, "/unknown").await;
        let daemon_url = format!("http://127.0.0.1:{}", port);
        let bridge = make_bridge_for_daemon(&daemon_url);

        bridge
            .detect_and_store_prefix_mapping_inner(conn_id, Some(&host_cwd))
            .await;

        let map = bridge.source_prefix_map.read().await.clone();
        assert!(map.is_none(), "/unknown workspace_root must be ignored");
    }

    /// The correct REST API path `/api/v1/connections/{id}` is used — not `/connections/{id}`.
    /// Verified by the mock server only responding to the correct path; a wrong path → 404 → no map.
    #[tokio::test]
    async fn detect_uses_api_v1_path() {
        use axum::{routing::get, Router};
        use tokio::net::TcpListener;

        // Server that ONLY responds at the correct path.
        let container_workspace = "/src/app";
        let app = Router::new().route(
            "/api/v1/connections/testid",
            get(move || async move {
                axum::Json(serde_json::json!({ "workspaceRoot": container_workspace }))
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let (_tmp, host_cwd) = mk_tree(&["app"]);
        let daemon_url = format!("http://127.0.0.1:{}", port);
        let bridge = make_bridge_for_daemon(&daemon_url);

        bridge
            .detect_and_store_prefix_mapping_inner("testid", Some(&host_cwd))
            .await;

        // If the correct path is used, the server responds with workspaceRoot → mapping is set.
        let map = bridge.source_prefix_map.read().await.clone();
        assert!(
            map.is_some(),
            "Correct /api/v1/connections/{{id}} path must be used — mapping should be detected"
        );
    }

    // =========================================================================
    // End-to-end: mock daemon + file server + prefix map
    //
    // Simulates the full Docker debugging flow:
    //   1. `detect_and_store_prefix_mapping_inner` fetches workspace_root from
    //      the mock daemon and stores the container→host mapping.
    //   2. The bridge file server (already running with the shared prefix map)
    //      rewrites incoming container paths before serving files.
    // =========================================================================

    /// Full end-to-end: mock daemon sets prefix map, file server rewrites paths.
    #[tokio::test]
    async fn e2e_docker_path_rewriting_via_prefix_map() {
        use super::super::file_server::start_file_server;
        use std::io::Write;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        // Create a real file on the host under a structure that mirrors the container
        // workspace suffix: container = /src/myapp, host = <tmp>/myapp
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("myapp");
        std::fs::create_dir_all(&app_dir).unwrap();
        let mut src_file = tempfile::NamedTempFile::new_in(&app_dir).unwrap();
        write!(src_file, "e2e file content").unwrap();
        let filename = src_file
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let host_cwd = tmp.path().to_string_lossy().into_owned();
        let container_workspace = "/src/myapp"; // same suffix as tmp/myapp

        // Start a mock daemon that returns this workspace_root for our connection
        let conn_id = "e2e_conn";
        let (mock_port, _server) = mock_connection_server(conn_id, container_workspace).await;
        let daemon_url = format!("http://127.0.0.1:{}", mock_port);

        // Start the bridge file server (no IP restriction, no auth)
        let prefix_map: super::super::file_server::SourcePrefixMap = Arc::new(RwLock::new(None));
        let allowed_ips = Arc::new(RwLock::new(None));
        let fs_port = start_file_server(None, allowed_ips, Arc::clone(&prefix_map))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Build a bridge pointing at the mock daemon, with the shared prefix map
        let bridge = make_bridge_for_daemon(&daemon_url);
        // Inject the shared prefix map into the bridge
        *bridge.source_prefix_map.write().await = None;

        // Simulate wake: detect + store the prefix mapping
        bridge
            .detect_and_store_prefix_mapping_inner(conn_id, Some(&host_cwd))
            .await;

        // Verify the prefix map was detected
        let detected_map = bridge.source_prefix_map.read().await.clone();
        let (c_prefix, h_prefix) = detected_map.expect("Map must be detected");
        assert_eq!(c_prefix, "/src");
        assert_eq!(h_prefix, host_cwd);

        // Now apply the same map to the file server (simulating what bridge.run() wires up)
        *prefix_map.write().await = Some((c_prefix, h_prefix));

        // File server request uses the container path — should be rewritten to host path
        let container_path = format!("/src/myapp/{}", filename);
        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/detrix/files/read", fs_port))
            .json(&serde_json::json!({ "path": container_path }))
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            200,
            "File should be found after path rewriting"
        );
        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["content"], "e2e file content");
    }
}
