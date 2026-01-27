//! gRPC client for connecting to Detrix daemon
//!
//! Uses shared port configuration from detrix-config to avoid duplication.
//! Uses shared auth middleware from detrix-api for authentication.

mod connections;
mod helpers;
mod metrics;
mod streaming;
mod types;

// Re-export client types
pub use connections::ConnectionsClient;
pub use metrics::MetricsClient;
pub use streaming::StreamingClient;

// Re-export DTO types (only those used externally)
pub use types::{AddMetricParams, ConnectionInfo, EventInfo, GroupInfo, MetricInfo, StatusInfo};

// Re-export shared auth types and daemon connection from detrix-api
pub use detrix_api::grpc::{connect_to_daemon_grpc, AuthChannel, DaemonEndpoints};
// Re-export for use by output/table.rs
pub use detrix_core::format_uptime;

use crate::utils::pid::{PidFile, PidInfo};
use anyhow::Result;
use detrix_config::{ApiConfig, ServiceType};
use std::path::Path;

/// Get default gRPC port from shared config
fn default_grpc_port() -> u16 {
    ApiConfig::default().grpc.port
}

/// Get default HTTP port from shared config
#[allow(dead_code)]
fn default_http_port() -> u16 {
    ApiConfig::default().rest.port
}

/// Get daemon info (PID and port) from PID file
#[allow(dead_code)]
pub fn get_daemon_info(pid_file_path: &Path) -> Option<PidInfo> {
    PidFile::read_info(pid_file_path).ok().flatten()
}

/// Get the HTTP port from PID file, or default
#[allow(dead_code)]
pub fn get_http_port_from_pid_file(pid_file_path: &Path) -> u16 {
    get_daemon_info(pid_file_path)
        .and_then(|info| info.port())
        .unwrap_or_else(default_http_port)
}

/// Get the gRPC port from PID file, or default
#[allow(dead_code)]
pub fn get_grpc_port_from_pid_file(pid_file_path: &Path) -> u16 {
    get_daemon_info(pid_file_path)
        .and_then(|info| info.get_port(ServiceType::Grpc))
        .unwrap_or_else(default_grpc_port)
}

/// Create a gRPC channel to the daemon using pre-discovered endpoints
///
/// Use this when you already have `DaemonEndpoints` (e.g., from `ClientContext`).
/// This avoids redundant endpoint discovery.
///
/// Authentication:
/// - Automatically discovers auth token from DETRIX_TOKEN env var or mcp-token file
/// - If found, adds Bearer token to all gRPC requests
pub async fn connect_with_endpoints(endpoints: &DaemonEndpoints) -> Result<AuthChannel> {
    connect_to_daemon_grpc(endpoints)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_config::constants::{DEFAULT_API_HOST, DEFAULT_GRPC_PORT, DEFAULT_REST_PORT};
    use detrix_config::paths::default_pid_path;
    use std::env;
    use std::net::TcpStream;
    use std::time::Duration;

    /// Check if a daemon appears to be running (via PID file or port probe)
    ///
    /// This is used to skip tests that assume no daemon is running.
    fn is_daemon_likely_running() -> bool {
        // Check PID file
        let pid_path = default_pid_path();
        if pid_path.exists() {
            return true;
        }

        // Check if default ports are open (port probing would find daemon)
        let timeout = Duration::from_millis(100);
        let grpc_addr = format!("{}:{}", DEFAULT_API_HOST, DEFAULT_GRPC_PORT);
        let http_addr = format!("{}:{}", DEFAULT_API_HOST, DEFAULT_REST_PORT);

        if let Ok(grpc_addr) = grpc_addr.parse() {
            if TcpStream::connect_timeout(&grpc_addr, timeout).is_ok() {
                return true;
            }
        }
        if let Ok(http_addr) = http_addr.parse() {
            if TcpStream::connect_timeout(&http_addr, timeout).is_ok() {
                return true;
            }
        }

        false
    }

    /// Test Scenario 1: gRPC connection fails gracefully when daemon is not running
    ///
    /// When no PID file exists and no explicit port is provided, `connect_with_endpoints()`
    /// attempts to connect to the default port as a fallback. If connection fails,
    /// the error message indicates no PID file was found and suggests alternatives.
    #[tokio::test]
    async fn test_connect_fails_gracefully_without_pid_file() {
        // Skip if a daemon is running (via PID file or port probe)
        if is_daemon_likely_running() {
            eprintln!(
                "Skipping test: daemon appears to be running (PID file exists or ports are open)"
            );
            return;
        }

        // Remove any port override env vars that might interfere
        env::remove_var(detrix_config::constants::ENV_DETRIX_GRPC_PORT_OVERRIDE);
        env::remove_var(detrix_config::constants::ENV_DETRIX_GRPC_PORT);

        // Discover endpoints without pre-loaded config (uses defaults)
        let endpoints = DaemonEndpoints::discover_with_config(None, None, None);

        // Attempt to connect - should fail (no daemon running on default port)
        let result = connect_with_endpoints(&endpoints).await;

        // Verify it failed
        assert!(result.is_err(), "Should fail when daemon is not running");

        // Verify error message is helpful (mentions PID file not found and fallback)
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("PID file not found"),
            "Error should mention PID file not found, got: {}",
            error_message
        );
    }

    /// Test that explicit port bypasses PID file check
    #[tokio::test]
    async fn test_connect_with_explicit_port_bypasses_pid_check() {
        // Remove any port override env vars that might interfere
        env::remove_var(detrix_config::constants::ENV_DETRIX_GRPC_PORT_OVERRIDE);
        env::remove_var(detrix_config::constants::ENV_DETRIX_GRPC_PORT);

        // Discover endpoints with explicit port override
        // Use a high port unlikely to be in use
        let endpoints = DaemonEndpoints::discover_with_config(Some(59999), None, None);

        // With explicit port, should attempt connection (will fail due to no server,
        // but error should be connection error, not "daemon not running")
        let result = connect_with_endpoints(&endpoints).await;

        // Should fail (no server on this port), but NOT with "daemon not running" error
        assert!(result.is_err());
        let error_message = result.unwrap_err().to_string();
        assert!(
            !error_message.contains("Daemon is not running"),
            "With explicit port, should attempt connection, not check PID file. Got: {}",
            error_message
        );
        // Should be a connection error
        assert!(
            error_message.contains("connect") || error_message.contains("Failed"),
            "Should be a connection error, got: {}",
            error_message
        );
    }

    /// Test explicit port override (function parameter) bypasses all other discovery
    ///
    /// This tests the highest-priority override path using function parameters,
    /// which avoids race conditions with environment variables in parallel tests.
    #[test]
    fn test_explicit_port_override_parameter() {
        // Explicit override should take effect regardless of PID file or env vars
        let explicit_port = 59999;
        let endpoints = DaemonEndpoints::discover_with_config(Some(explicit_port), None, None);

        assert_eq!(
            endpoints.grpc_port, explicit_port,
            "Explicit port override should be used"
        );
        assert!(
            !endpoints.from_pid_file,
            "from_pid_file should be false when explicit override is used"
        );
    }

    /// Test that discovery returns valid endpoints (either from daemon or defaults)
    ///
    /// Note: Environment variable tests are inherently racy in parallel test execution.
    /// This test verifies basic discovery works without relying on env var manipulation.
    #[test]
    fn test_discovery_returns_valid_endpoints() {
        let endpoints = DaemonEndpoints::discover_with_config(None, None, None);

        // Should always get a valid host
        assert!(!endpoints.host.is_empty(), "Host should not be empty");

        // Port should be within valid range
        assert!(endpoints.grpc_port > 0, "gRPC port should be positive");
        assert!(endpoints.http_port > 0, "HTTP port should be positive");

        // If daemon is running, should be from PID file or port probe
        // If not running, should be from config/default
        // Either way, we should have valid discovery_method
        use detrix_api::grpc::EndpointDiscoveryMethod;
        let valid_methods = [
            EndpointDiscoveryMethod::PidFile,
            EndpointDiscoveryMethod::PortProbe,
            EndpointDiscoveryMethod::EnvVar,
            EndpointDiscoveryMethod::ConfigOrDefault,
        ];
        assert!(
            valid_methods.contains(&endpoints.discovery_method),
            "Discovery method should be one of the valid options"
        );
    }

    /// Test the internal logic: verify PID file detection works correctly
    #[test]
    fn test_pid_file_detection_logic() {
        use crate::utils::pid::PidFile;

        // When PID file doesn't exist, read_info returns Ok(None)
        let non_existent = std::path::PathBuf::from("/tmp/definitely_does_not_exist_12345.pid");
        let result = PidFile::read_info(&non_existent);
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_none(),
            "Non-existent PID file should return None"
        );
    }
}
