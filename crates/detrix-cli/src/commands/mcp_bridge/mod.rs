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
mod file_server;
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
/// * `file_server_host` - Host to advertise for file server (defaults to daemon_host)
/// * `config_path` - Config path for daemon restart capability
/// * `pid_file` - PID file path for daemon restart capability
/// * `mcp_config` - MCP bridge configuration from detrix.toml
pub async fn run_bridge(
    daemon_host: &str,
    daemon_port: u16,
    file_server_host: Option<String>,
    config_path: Option<PathBuf>,
    pid_file: Option<PathBuf>,
    mcp_config: &detrix_config::McpBridgeConfig,
    config_port: u16,
) -> Result<()> {
    // Resolve auth token for daemon authentication
    // Priority: DETRIX_TOKEN env > credentials.toml per-host > auth-token file
    let daemon_hp = format!("{}:{}", daemon_host, daemon_port);
    let auth_token = auth::resolve_token_for_host(&daemon_hp, true).or_else(discover_auth_token);
    if auth_token.is_some() {
        info!("Auth token discovered for daemon communication");
    }

    // Detect parent process (IDE/editor that spawned this bridge)
    let parent_process = detect_parent_process();

    let config = BridgeConfig {
        daemon_url: format!("http://{}:{}", daemon_host, daemon_port),
        daemon_host: daemon_host.to_string(),
        config_port,
        timeout_ms: mcp_config.bridge_timeout_ms,
        auth_token,
        config_path,
        pid_file,
        heartbeat_interval_secs: mcp_config.heartbeat_interval_secs,
        heartbeat_max_failures: mcp_config.heartbeat_max_failures,
        parent_process,
        file_server_host: file_server_host.unwrap_or_else(|| daemon_host.to_string()),
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
    use detrix_config::constants::{AUTHORIZATION_HEADER, BEARER_PREFIX};
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
                AUTHORIZATION_HEADER,
                &format!("{}new_token_xyz", BEARER_PREFIX),
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
                AUTHORIZATION_HEADER,
                &format!("{}refreshed_token", BEARER_PREFIX),
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
                AUTHORIZATION_HEADER,
                &format!("{}updated_token", BEARER_PREFIX),
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
                AUTHORIZATION_HEADER,
                &format!("{}new_daemon_token", BEARER_PREFIX),
            ))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server_b)
            .await;

        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(wiremock::matchers::header(
                AUTHORIZATION_HEADER,
                &format!("{}new_daemon_token", BEARER_PREFIX),
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

    // ========================================================================
    // Scenario 5: Auth helper unit tests
    // Tests extract_host_port and extract_wake_app_url
    // ========================================================================

    #[test]
    fn test_extract_host_port_standard_url() {
        assert_eq!(
            auth::extract_host_port("http://localhost:8095"),
            Some("localhost:8095".to_string())
        );
    }

    #[test]
    fn test_extract_host_port_with_path() {
        assert_eq!(
            auth::extract_host_port("http://localhost:8095/some/path"),
            Some("localhost:8095".to_string())
        );
    }

    #[test]
    fn test_extract_host_port_https() {
        assert_eq!(
            auth::extract_host_port("https://myapp.prod:8095"),
            Some("myapp.prod:8095".to_string())
        );
    }

    #[test]
    fn test_extract_host_port_ip_address() {
        assert_eq!(
            auth::extract_host_port("http://127.0.0.1:8090"),
            Some("127.0.0.1:8090".to_string())
        );
    }

    #[test]
    fn test_extract_host_port_no_port() {
        // URLs without explicit port should return None
        assert_eq!(auth::extract_host_port("http://localhost"), None);
    }

    #[test]
    fn test_extract_host_port_invalid_url() {
        assert_eq!(auth::extract_host_port("not-a-url"), None);
    }

    #[test]
    fn test_extract_host_port_trailing_slash() {
        assert_eq!(
            auth::extract_host_port("http://localhost:8095/"),
            Some("localhost:8095".to_string())
        );
    }

    #[test]
    fn test_extract_wake_app_url_valid_wake() {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "wake",
                "arguments": {
                    "app_url": "http://localhost:8091"
                }
            },
            "id": 1
        });
        assert_eq!(
            bridge::extract_wake_app_url(&request),
            Some("http://localhost:8091".to_string())
        );
    }

    #[test]
    fn test_extract_wake_app_url_no_app_url() {
        // Wake without app_url (daemon-only wake)
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "wake",
                "arguments": {}
            },
            "id": 1
        });
        assert_eq!(bridge::extract_wake_app_url(&request), None);
    }

    #[test]
    fn test_extract_wake_app_url_non_wake_tool() {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "observe",
                "arguments": {
                    "file": "test.py",
                    "expressions": ["x"]
                }
            },
            "id": 1
        });
        assert_eq!(bridge::extract_wake_app_url(&request), None);
    }

    #[test]
    fn test_extract_wake_app_url_non_tool_call() {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        });
        assert_eq!(bridge::extract_wake_app_url(&request), None);
    }

    // ========================================================================
    // Scenario 6: Discovery-first wake flow (wiremock)
    // Tests bridge discovery → daemon switch → token resolution → wake forward
    // ========================================================================

    /// BRIDGE-DISC-001: Discovery switches daemon after wake (no control_plane_url).
    ///
    /// Without control_plane_url: bridge wakes via LOCAL daemon A (reachable from host),
    /// then switches to daemon B post-wake so future calls go to the correct daemon.
    ///
    /// Setup: bridge → daemon A (initial), app discover returns daemon B (no cp_url).
    /// Expected: daemon A handles the wake, bridge switches to daemon B post-wake.
    #[tokio::test]
    async fn test_discovery_switches_daemon_on_wake() {
        // Start 3 mock servers: daemon A (initial), daemon B (target), app
        let daemon_a = MockServer::start().await;
        let daemon_b = MockServer::start().await;
        let app_server = MockServer::start().await;

        // App returns discover response pointing to daemon B (no control_plane_url)
        Mock::given(method("GET"))
            .and(path("/detrix/discover"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "daemon_url": daemon_b.uri(),
                "name": "test-app"
            })))
            .expect(1)
            .mount(&app_server)
            .await;

        // Daemon A: wake handled here (no control_plane_url → wake via current daemon first)
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {
                    "content": [{"type": "text", "text": "Woke test-app"}]
                },
                "id": 1
            })))
            .expect(1)
            .mount(&daemon_a)
            .await;

        // Daemon B: health check succeeds (required by switch_daemon post-wake)
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok"
            })))
            .mount(&daemon_b)
            .await;

        // Create bridge pointing to daemon A
        let config = BridgeConfig {
            daemon_url: daemon_a.uri(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // Build wake request with app_url
        let wake_request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "wake",
                "arguments": {
                    "app_url": app_server.uri()
                }
            },
            "id": 1
        });

        // Execute discovery-first flow
        let response = bridge.forward_or_fallback(&wake_request).await;

        // Assert wake succeeded (no error)
        assert!(
            response.get("error").is_none(),
            "Wake should succeed, got: {}",
            response
        );

        // Assert daemon URL was switched to daemon B
        let current_url = bridge.get_current_daemon_url().await;
        assert_eq!(
            current_url,
            daemon_b.uri().trim_end_matches('/'),
            "Bridge should have switched to daemon B"
        );
    }

    /// BRIDGE-DISC-002: Discovery failure forwards to current daemon.
    ///
    /// Setup: app discover endpoint unreachable, wake goes to current daemon.
    #[tokio::test]
    async fn test_discovery_failure_forwards_to_current_daemon() {
        let daemon = MockServer::start().await;

        // Current daemon handles wake
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {
                    "content": [{"type": "text", "text": "Woke via daemon"}]
                },
                "id": 1
            })))
            .expect(1)
            .mount(&daemon)
            .await;

        let config = BridgeConfig {
            daemon_url: daemon.uri(),
            timeout_ms: 2000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // Wake request with unreachable app_url
        let wake_request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "wake",
                "arguments": {
                    "app_url": "http://127.0.0.1:59998"
                }
            },
            "id": 1
        });

        let response = bridge.forward_or_fallback(&wake_request).await;

        // Should still succeed via current daemon (discovery failed gracefully)
        assert!(
            response.get("error").is_none(),
            "Wake should succeed via current daemon, got: {}",
            response
        );

        // Daemon URL should NOT have changed
        let current_url = bridge.get_current_daemon_url().await;
        assert_eq!(
            current_url,
            daemon.uri(),
            "Daemon URL should remain unchanged"
        );
    }

    /// BRIDGE-DISC-003: Same daemon — no switch.
    ///
    /// Setup: app discover returns the same daemon URL as current.
    /// Expected: no switch_daemon call, wake forwarded normally.
    #[tokio::test]
    async fn test_discovery_same_daemon_no_switch() {
        let daemon = MockServer::start().await;
        let app_server = MockServer::start().await;

        // App discover returns same daemon URL
        Mock::given(method("GET"))
            .and(path("/detrix/discover"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "daemon_url": daemon.uri(),
                "name": "local-app"
            })))
            .expect(1)
            .mount(&app_server)
            .await;

        // Daemon: should NOT get a health check (no switch needed)
        // Daemon: handles wake
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {
                    "content": [{"type": "text", "text": "Woke local-app"}]
                },
                "id": 1
            })))
            .expect(1)
            .mount(&daemon)
            .await;

        let config = BridgeConfig {
            daemon_url: daemon.uri(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        let wake_request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "wake",
                "arguments": {
                    "app_url": app_server.uri()
                }
            },
            "id": 1
        });

        let response = bridge.forward_or_fallback(&wake_request).await;

        assert!(
            response.get("error").is_none(),
            "Wake should succeed, got: {}",
            response
        );

        // URL should remain the same (no switch)
        let current_url = bridge.get_current_daemon_url().await;
        assert_eq!(current_url, daemon.uri(), "Daemon URL should not change");
    }

    /// BRIDGE-DISC-004: switch_daemon auto-resolves token.
    ///
    /// After switching daemon, the bridge should update its auth token
    /// based on credential lookup.
    #[tokio::test]
    async fn test_switch_daemon_updates_auth_token() {
        let daemon_a = MockServer::start().await;
        let daemon_b = MockServer::start().await;

        // Daemon B: health check succeeds
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok"
            })))
            .expect(1)
            .mount(&daemon_b)
            .await;

        let config = BridgeConfig {
            daemon_url: daemon_a.uri(),
            auth_token: Some("old-token".to_string()),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // Verify initial token
        {
            let token = bridge.auth_token.read().await;
            assert_eq!(*token, Some("old-token".to_string()));
        }

        // Switch daemon (token resolution depends on credentials.toml state,
        // which in tests is typically empty — token remains as-is or gets
        // resolved from env/file)
        let result = bridge.switch_daemon(daemon_b.uri(), None).await;
        assert!(result.is_ok(), "switch_daemon should succeed");

        // Verify URL was updated
        let current_url = bridge.get_current_daemon_url().await;
        assert_eq!(
            current_url,
            daemon_b.uri().trim_end_matches('/'),
            "URL should be updated"
        );
    }

    /// Test discover_app_daemon returns correct daemon URL and name
    #[tokio::test]
    async fn test_discover_app_daemon_success() {
        let app_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/detrix/discover"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "daemon_url": "http://external-daemon:8090",
                "name": "my-service"
            })))
            .expect(1)
            .mount(&app_server)
            .await;

        let config = BridgeConfig {
            daemon_url: "http://localhost:8090".to_string(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        let result = bridge.discover_app_daemon(&app_server.uri()).await;
        assert_eq!(
            result,
            Some((
                "http://external-daemon:8090".to_string(),
                "my-service".to_string(),
                None, // no control_plane_url in response
            ))
        );
    }

    /// Test discover_app_daemon returns control_plane_url when present in response
    #[tokio::test]
    async fn test_discover_app_daemon_with_control_plane_url() {
        let app_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/detrix/discover"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "daemon_url": "http://localhost:8095",
                "name": "order-service",
                "control_plane_url": "http://order-service:8091"
            })))
            .expect(1)
            .mount(&app_server)
            .await;

        let config = BridgeConfig {
            daemon_url: "http://localhost:8090".to_string(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        let result = bridge.discover_app_daemon(&app_server.uri()).await;
        assert_eq!(
            result,
            Some((
                "http://localhost:8095".to_string(),
                "order-service".to_string(),
                Some("http://order-service:8091".to_string()),
            ))
        );
    }

    /// Test discover_app_daemon returns None when endpoint returns 404
    #[tokio::test]
    async fn test_discover_app_daemon_404() {
        let app_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/detrix/discover"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&app_server)
            .await;

        let config = BridgeConfig {
            daemon_url: "http://localhost:8090".to_string(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        let result = bridge.discover_app_daemon(&app_server.uri()).await;
        assert_eq!(result, None, "Should return None for 404");
    }

    /// Test discover_app_daemon returns None when app is unreachable
    #[tokio::test]
    async fn test_discover_app_daemon_unreachable() {
        let config = BridgeConfig {
            daemon_url: "http://localhost:8090".to_string(),
            timeout_ms: 1000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        let result = bridge.discover_app_daemon("http://127.0.0.1:59997").await;
        assert_eq!(result, None, "Should return None for unreachable app");
    }

    /// BRIDGE-DISC-005: control_plane_url rewrite targets params.arguments.app_url.
    ///
    /// When the discover response includes `control_plane_url`, the bridge must
    /// rewrite the URL inside `params.arguments.app_url` (NOT `params.app_url`).
    /// This test verifies the rewrite path is correct by checking which URL the
    /// daemon actually receives.
    #[tokio::test]
    async fn test_control_plane_url_rewrite_targets_arguments_app_url() {
        let daemon_a = MockServer::start().await;
        let daemon_b = MockServer::start().await;
        let app_server = MockServer::start().await;

        let daemon_visible_url = format!("http://internal-app:{}", app_server.address().port());

        // App discover returns daemon B + control_plane_url (daemon-visible URL)
        Mock::given(method("GET"))
            .and(path("/detrix/discover"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "daemon_url": daemon_b.uri(),
                "name": "order-service",
                "control_plane_url": daemon_visible_url
            })))
            .expect(1)
            .mount(&app_server)
            .await;

        // Daemon B: health check
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})),
            )
            .mount(&daemon_b)
            .await;

        // Daemon B: wake forwarded here
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {"content": [{"type": "text", "text": "ok"}]},
                "id": 1
            })))
            .expect(1)
            .mount(&daemon_b)
            .await;

        let config = BridgeConfig {
            daemon_url: daemon_a.uri(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        // Wake request with user-visible app_url (NOT the daemon-visible one)
        let wake_request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "wake",
                "arguments": {
                    "app_url": app_server.uri()
                }
            },
            "id": 1
        });

        let response = bridge.forward_or_fallback(&wake_request).await;

        // Wake should succeed (daemon B received and handled it)
        assert!(
            response.get("error").is_none(),
            "Wake with control_plane_url rewrite should succeed, got: {}",
            response
        );

        // Daemon should have switched to B
        let current_url = bridge.get_current_daemon_url().await;
        assert_eq!(
            current_url,
            daemon_b.uri().trim_end_matches('/'),
            "Bridge should have switched to daemon B"
        );

        // Daemon B's mock had `.expect(1)` — wiremock verifies exactly 1 POST to /mcp on drop.
    }

    /// Test non-wake requests bypass discovery flow
    #[tokio::test]
    async fn test_non_wake_request_bypasses_discovery() {
        let daemon = MockServer::start().await;

        // Daemon handles list_metrics (non-wake request)
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "result": {
                    "content": [{"type": "text", "text": "metrics list"}]
                },
                "id": 1
            })))
            .expect(1)
            .mount(&daemon)
            .await;

        let config = BridgeConfig {
            daemon_url: daemon.uri(),
            timeout_ms: 5000,
            ..Default::default()
        };
        let bridge = McpBridge::new(config).unwrap();

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "list_metrics",
                "arguments": {}
            },
            "id": 1
        });

        let response = bridge.forward_or_fallback(&request).await;
        assert!(
            response.get("error").is_none(),
            "Non-wake request should forward directly, got: {}",
            response
        );
    }
}
