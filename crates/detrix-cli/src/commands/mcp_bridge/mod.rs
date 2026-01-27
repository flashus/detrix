//! MCP stdio-to-HTTP bridge
//!
//! Translates JSON-RPC over stdio to HTTP requests to the daemon's `/mcp` endpoint.
//! This allows Claude Code to use stdio transport while all operations go through the daemon.
//!
//! ## Architecture
//!
//! ```text
//! Claude Code (stdio) → detrix mcp --use-daemon → HTTP POST /mcp → Daemon
//! ```
//!
//! ## Why a Bridge?
//!
//! - Claude Code only supports stdio transport for MCP
//! - Daemon provides centralized state management (metrics, connections)
//! - Bridge translates stdio ↔ HTTP without duplicating state
//!
//! ## Client Tracking & Heartbeats
//!
//! Each bridge instance sends a unique X-Detrix-Client-Id header with each request.
//! A background heartbeat task sends periodic pings to keep the client registered
//! even during idle periods. This allows the daemon to track active MCP clients
//! for lifecycle management (auto-shutdown when all clients disconnect for
//! MCP-spawned daemons).

mod auth;
mod bridge;
mod config;
mod parent_detect;

// Re-exports
pub use bridge::McpBridge;
pub use config::BridgeConfig;
pub use parent_detect::detect_parent_process;

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
use auth::discover_auth_token;
use detrix_config::constants::ENV_DETRIX_MCP_DISCONNECT_TIMEOUT_MS;
use detrix_logging::{debug, info};
use std::path::PathBuf;
use std::sync::Arc;

/// Run the MCP bridge (entry point from CLI)
///
/// # Arguments
/// * `daemon_host` - Host the daemon is running on (from api.host config)
/// * `daemon_port` - Port the daemon is running on
/// * `config_path` - Config path for daemon restart capability
/// * `pid_file` - PID file path for daemon restart capability
/// * `mcp_config` - MCP bridge configuration from detrix.toml
pub async fn run_bridge(
    daemon_host: &str,
    daemon_port: u16,
    config_path: Option<PathBuf>,
    pid_file: Option<PathBuf>,
    mcp_config: &detrix_config::McpBridgeConfig,
) -> Result<()> {
    // Discover auth token for daemon authentication
    let auth_token = discover_auth_token();
    if auth_token.is_some() {
        info!("Auth token discovered for daemon communication");
    }

    // Detect parent process (IDE/editor that spawned this bridge)
    let parent_process = detect_parent_process();

    let config = BridgeConfig {
        daemon_url: format!("http://{}:{}", daemon_host, daemon_port),
        daemon_host: daemon_host.to_string(),
        daemon_port,
        timeout_ms: mcp_config.bridge_timeout_ms,
        auth_token,
        config_path,
        pid_file,
        heartbeat_interval_secs: mcp_config.heartbeat_interval_secs,
        heartbeat_max_failures: mcp_config.heartbeat_max_failures,
        parent_process,
    };

    let bridge = Arc::new(McpBridge::new(config)?);

    // Set up signal handling for graceful shutdown
    #[cfg(unix)]
    let (result, got_signal) = {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm =
            signal(SignalKind::terminate()).context("Failed to install SIGTERM handler")?;
        let mut sigint =
            signal(SignalKind::interrupt()).context("Failed to install SIGINT handler")?;

        tokio::select! {
            result = bridge.clone().run() => {
                (result, false)
            }
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down MCP bridge");
                (Ok(()), true)
            }
            _ = sigint.recv() => {
                info!("Received SIGINT (Ctrl+C), shutting down MCP bridge");
                (Ok(()), true)
            }
        }
    };

    #[cfg(not(unix))]
    let (result, got_signal) = {
        tokio::select! {
            result = bridge.clone().run() => {
                (result, false)
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Received Ctrl+C, shutting down MCP bridge");
                (Ok(()), true)
            }
        }
    };

    // Send disconnect notification to daemon before exiting
    // This allows daemon to unregister this client and potentially auto-shutdown
    // Use configurable timeout (env var or default) to handle network latency
    let disconnect_timeout = std::time::Duration::from_millis(
        std::env::var(ENV_DETRIX_MCP_DISCONNECT_TIMEOUT_MS)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(detrix_config::constants::DEFAULT_MCP_DISCONNECT_TIMEOUT_MS),
    );
    match tokio::time::timeout(disconnect_timeout, bridge.send_disconnect()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            debug!("Failed to send disconnect notification: {}", e);
        }
        Err(_) => {
            debug!(
                "Disconnect notification timed out after {:?}",
                disconnect_timeout
            );
        }
    }

    // If we got a signal, force exit to avoid waiting for spawned tasks
    // (the heartbeat task would otherwise keep the runtime alive)
    if got_signal {
        std::process::exit(0);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_forward_request_success() {
        // Start mock server
        let mock_server = MockServer::start().await;

        // Set up mock response
        let request_body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        });

        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "result": {
                "tools": []
            },
            "id": 1
        });

        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(body_json(&request_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .mount(&mock_server)
            .await;

        // Create bridge
        let config = BridgeConfig {
            daemon_url: mock_server.uri(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // Forward request
        let response = bridge.forward_request(request_body).await.unwrap();

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert!(response["result"]["tools"].is_array());
    }

    #[tokio::test]
    async fn test_forward_request_daemon_error() {
        // Start mock server
        let mock_server = MockServer::start().await;

        // Set up error response
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&mock_server)
            .await;

        // Create bridge
        let config = BridgeConfig {
            daemon_url: mock_server.uri(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // Forward request
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        });

        let result = bridge.forward_request(request).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }

    #[tokio::test]
    async fn test_forward_request_connection_refused() {
        // Create bridge pointing to non-existent server
        let config = BridgeConfig {
            daemon_url: "http://127.0.0.1:59999".to_string(), // Unlikely to be in use
            timeout_ms: 1000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // Forward request
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        });

        let result = bridge.forward_request(request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_heartbeat_success_resets_failures() {
        // Start mock server with successful heartbeat response
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/mcp/heartbeat"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let config = BridgeConfig {
            daemon_url: mock_server.uri(),
            timeout_ms: 5000,
            heartbeat_max_failures: 2,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // Simulate some prior failures
        let prior_failures = 1;
        let new_failures = bridge.handle_heartbeat_tick(prior_failures, 2).await;

        // Successful heartbeat should reset to 0
        assert_eq!(new_failures, 0);
    }

    #[tokio::test]
    async fn test_heartbeat_failure_increments_count() {
        // Start mock server that returns error
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/mcp/heartbeat"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&mock_server)
            .await;

        // Health check also fails (daemon is down)
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&mock_server)
            .await;

        let config = BridgeConfig {
            daemon_url: mock_server.uri(),
            timeout_ms: 5000,
            heartbeat_max_failures: 3, // Set higher so restart isn't triggered
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // First failure
        let failures = bridge.handle_heartbeat_tick(0, 3).await;
        assert_eq!(failures, 1);

        // Second failure
        let failures = bridge.handle_heartbeat_tick(1, 3).await;
        assert_eq!(failures, 2);
    }

    #[tokio::test]
    async fn test_heartbeat_failure_healthy_daemon_resets_count() {
        // Start mock server where heartbeat fails but health check succeeds
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/mcp/heartbeat"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&mock_server)
            .await;

        // Health check succeeds (daemon is up, just heartbeat endpoint issue)
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let config = BridgeConfig {
            daemon_url: mock_server.uri(),
            timeout_ms: 5000,
            heartbeat_max_failures: 2,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // After reaching max failures, but daemon is healthy
        // Should reset to 0 (transient issue)
        let failures = bridge.handle_heartbeat_tick(1, 2).await;
        assert_eq!(failures, 0, "Should reset when daemon is healthy");
    }

    // ========================================================================
    // Scenario 2: MCP Bridge dynamic URL after port change
    // Tests Fix #3 & #4: heartbeat/disconnect use updated daemon_url RwLock
    // ========================================================================

    /// Test that heartbeat uses updated daemon_url after port change
    ///
    /// This tests Fix #3: When daemon restarts on a different port,
    /// heartbeats should go to the new port, not the original one.
    #[tokio::test]
    async fn test_heartbeat_uses_updated_daemon_url() {
        // Start two mock servers (simulating port A and port B)
        let mock_server_a = MockServer::start().await;
        let mock_server_b = MockServer::start().await;

        // Set up port A to fail (daemon "died")
        Mock::given(method("POST"))
            .and(path("/mcp/heartbeat"))
            .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
            .mount(&mock_server_a)
            .await;

        // Set up port B to succeed (new daemon)
        Mock::given(method("POST"))
            .and(path("/mcp/heartbeat"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1) // Expect exactly one request
            .mount(&mock_server_b)
            .await;

        // Create bridge pointing to port A initially
        let config = BridgeConfig {
            daemon_url: mock_server_a.uri(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = Arc::new(McpBridge::new(config).unwrap());

        // Simulate daemon restart: update daemon_url to port B
        {
            let mut url = bridge.daemon_url.write().await;
            *url = mock_server_b.uri();
        }

        // Send heartbeat - should go to port B
        let result = bridge.send_heartbeat().await;
        assert!(result.is_ok(), "Heartbeat should succeed on new port");

        // Verify mock_server_b received the request (expectation is checked on drop)
    }

    /// Test that disconnect uses updated daemon_url after port change
    ///
    /// This tests Fix #4: When daemon restarts on a different port,
    /// disconnect notification should go to the new port.
    #[tokio::test]
    async fn test_disconnect_uses_updated_daemon_url() {
        // Start two mock servers
        let mock_server_a = MockServer::start().await;
        let mock_server_b = MockServer::start().await;

        // Set up port B to handle disconnect
        Mock::given(method("POST"))
            .and(path("/mcp/disconnect"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server_b)
            .await;

        // Create bridge pointing to port A initially
        let config = BridgeConfig {
            daemon_url: mock_server_a.uri(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = Arc::new(McpBridge::new(config).unwrap());

        // Simulate daemon restart: update daemon_url to port B
        {
            let mut url = bridge.daemon_url.write().await;
            *url = mock_server_b.uri();
        }

        // Send disconnect - should go to port B
        let result = bridge.send_disconnect().await;
        assert!(result.is_ok(), "Disconnect should succeed on new port");
    }

    /// Test that request forwarding uses updated daemon_url
    #[tokio::test]
    async fn test_forward_request_uses_updated_daemon_url() {
        // Start two mock servers
        let mock_server_a = MockServer::start().await;
        let mock_server_b = MockServer::start().await;

        let response_body = serde_json::json!({
            "jsonrpc": "2.0",
            "result": {"tools": []},
            "id": 1
        });

        // Set up port B to handle MCP request
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .expect(1)
            .mount(&mock_server_b)
            .await;

        // Create bridge pointing to port A initially
        let config = BridgeConfig {
            daemon_url: mock_server_a.uri(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = Arc::new(McpBridge::new(config).unwrap());

        // Update daemon_url to port B
        {
            let mut url = bridge.daemon_url.write().await;
            *url = mock_server_b.uri();
        }

        // Forward request - should go to port B
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        });
        let result = bridge.try_forward_request(&request).await;
        assert!(result.is_ok(), "Request should succeed on new port");
    }

    // ========================================================================
    // Scenario 3: MCP Bridge auth token refresh after update
    // Tests Fix #5: auth token is re-discovered and used after daemon restart
    // ========================================================================

    /// Test that requests use updated auth token after refresh
    ///
    /// This tests Fix #5: When daemon restarts and generates a new token,
    /// subsequent requests should use the new token.
    #[tokio::test]
    async fn test_request_uses_updated_auth_token() {
        let mock_server = MockServer::start().await;

        // Set up mock to expect token "new_token_xyz"
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Bearer new_token_xyz",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {},
                "id": 1
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Create bridge with initial token "old_token_abc"
        let config = BridgeConfig {
            daemon_url: mock_server.uri(),
            auth_token: Some("old_token_abc".to_string()),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = Arc::new(McpBridge::new(config).unwrap());

        // Simulate token refresh (as would happen in try_restart_daemon)
        {
            let mut token = bridge.auth_token.write().await;
            *token = Some("new_token_xyz".to_string());
        }

        // Send request - should use new token
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "test",
            "id": 1
        });
        let result = bridge.try_forward_request(&request).await;
        assert!(result.is_ok(), "Request should succeed with new token");
    }

    /// Test that heartbeat uses updated auth token
    #[tokio::test]
    async fn test_heartbeat_uses_updated_auth_token() {
        let mock_server = MockServer::start().await;

        // Set up mock to expect new token
        Mock::given(method("POST"))
            .and(path("/mcp/heartbeat"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Bearer refreshed_token",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Create bridge with initial token
        let config = BridgeConfig {
            daemon_url: mock_server.uri(),
            auth_token: Some("initial_token".to_string()),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = Arc::new(McpBridge::new(config).unwrap());

        // Update token
        {
            let mut token = bridge.auth_token.write().await;
            *token = Some("refreshed_token".to_string());
        }

        // Send heartbeat
        let result = bridge.send_heartbeat().await;
        assert!(result.is_ok(), "Heartbeat should succeed with new token");
    }

    /// Test that disconnect uses updated auth token
    #[tokio::test]
    async fn test_disconnect_uses_updated_auth_token() {
        let mock_server = MockServer::start().await;

        // Set up mock to expect new token
        Mock::given(method("POST"))
            .and(path("/mcp/disconnect"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Bearer updated_token",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Create bridge with initial token
        let config = BridgeConfig {
            daemon_url: mock_server.uri(),
            auth_token: Some("old_token".to_string()),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = Arc::new(McpBridge::new(config).unwrap());

        // Update token
        {
            let mut token = bridge.auth_token.write().await;
            *token = Some("updated_token".to_string());
        }

        // Send disconnect
        let result = bridge.send_disconnect().await;
        assert!(result.is_ok(), "Disconnect should succeed with new token");
    }

    // ========================================================================
    // Scenario 4: Combined URL and token update (simulating full restart)
    // ========================================================================

    /// Test full restart scenario: both URL and token are updated
    ///
    /// This tests Fixes #3, #4, #5 together: after daemon restart,
    /// both the URL and auth token should be updated for all requests.
    #[tokio::test]
    async fn test_full_restart_updates_url_and_token() {
        // Start two mock servers (old port A, new port B)
        let mock_server_a = MockServer::start().await;
        let mock_server_b = MockServer::start().await;

        // Port A should NOT receive any requests (daemon died)
        // We don't set up any mocks on A to ensure requests don't go there

        // Port B expects request with new token
        Mock::given(method("POST"))
            .and(path("/mcp/heartbeat"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Bearer new_daemon_token",
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server_b)
            .await;

        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(wiremock::matchers::header(
                "Authorization",
                "Bearer new_daemon_token",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {},
                "id": 1
            })))
            .expect(1)
            .mount(&mock_server_b)
            .await;

        // Create bridge with old config
        let config = BridgeConfig {
            daemon_url: mock_server_a.uri(),
            auth_token: Some("old_daemon_token".to_string()),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = Arc::new(McpBridge::new(config).unwrap());

        // Simulate full restart: update both URL and token
        // (This is what try_restart_daemon does)
        {
            let mut url = bridge.daemon_url.write().await;
            *url = mock_server_b.uri();
        }
        {
            let mut token = bridge.auth_token.write().await;
            *token = Some("new_daemon_token".to_string());
        }

        // Send heartbeat - should go to port B with new token
        let heartbeat_result = bridge.send_heartbeat().await;
        assert!(
            heartbeat_result.is_ok(),
            "Heartbeat should succeed on new port with new token"
        );

        // Send request - should also go to port B with new token
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "test",
            "id": 1
        });
        let request_result = bridge.try_forward_request(&request).await;
        assert!(
            request_result.is_ok(),
            "Request should succeed on new port with new token"
        );
    }
}
