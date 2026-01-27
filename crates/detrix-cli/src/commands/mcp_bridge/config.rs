//! MCP bridge configuration types

use super::parent_detect::ParentProcessInfo;
use detrix_config::constants::{
    DEFAULT_API_HOST, DEFAULT_MCP_BRIDGE_HEARTBEAT_INTERVAL_SECS, DEFAULT_MCP_BRIDGE_TIMEOUT_MS,
    DEFAULT_MCP_HEARTBEAT_MAX_FAILURES, DEFAULT_REST_PORT,
};
use detrix_logging::warn;
use std::path::PathBuf;

/// MCP bridge configuration
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Daemon HTTP endpoint (e.g., "http://localhost:8080")
    pub daemon_url: String,
    /// Daemon host (for health checks and reconnection)
    pub daemon_host: String,
    /// Daemon port (for reconnection)
    pub daemon_port: u16,
    /// HTTP timeout in milliseconds
    pub timeout_ms: u64,
    /// Heartbeat interval in seconds (how often to ping daemon)
    /// Should be less than the daemon's heartbeat_timeout (30s default)
    pub heartbeat_interval_secs: u64,
    /// Max consecutive heartbeat failures before attempting daemon restart
    pub heartbeat_max_failures: u32,
    /// Optional auth token for daemon authentication
    pub auth_token: Option<String>,
    /// Config path for daemon restart
    pub config_path: Option<PathBuf>,
    /// PID file path for daemon restart
    pub pid_file: Option<PathBuf>,
    /// Parent process info (IDE/editor that spawned this bridge)
    pub parent_process: Option<ParentProcessInfo>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            daemon_url: format!("http://{}:{}", DEFAULT_API_HOST, DEFAULT_REST_PORT),
            daemon_host: DEFAULT_API_HOST.to_string(),
            daemon_port: DEFAULT_REST_PORT,
            timeout_ms: DEFAULT_MCP_BRIDGE_TIMEOUT_MS,
            heartbeat_interval_secs: DEFAULT_MCP_BRIDGE_HEARTBEAT_INTERVAL_SECS,
            heartbeat_max_failures: DEFAULT_MCP_HEARTBEAT_MAX_FAILURES,
            auth_token: None,
            config_path: None,
            pid_file: None,
            parent_process: None,
        }
    }
}

/// Backoff state for daemon restart attempts
pub struct RestartBackoff {
    /// Number of consecutive restart failures
    pub failures: u32,
    /// Earliest time for next restart attempt
    next_attempt: std::time::Instant,
}

impl RestartBackoff {
    pub fn new() -> Self {
        Self {
            failures: 0,
            next_attempt: std::time::Instant::now(),
        }
    }

    /// Record a restart failure and calculate next allowed attempt time
    pub fn record_failure(&mut self) {
        self.failures += 1;
        // Exponential backoff: 10s, 20s, 40s, 80s, 160s, max 320s (5+ minutes)
        let delay_secs = 10u64 * 2u64.pow(self.failures.saturating_sub(1).min(5));
        self.next_attempt = std::time::Instant::now() + std::time::Duration::from_secs(delay_secs);
        warn!(
            "Restart failed, backing off for {}s (failure #{})",
            delay_secs, self.failures
        );
    }

    /// Record a successful restart
    pub fn record_success(&mut self) {
        self.failures = 0;
        // Allow immediate retry if needed later
        self.next_attempt = std::time::Instant::now();
    }

    /// Check if enough time has passed to allow another restart attempt
    pub fn can_attempt(&self) -> bool {
        std::time::Instant::now() >= self.next_attempt
    }
}

impl Default for RestartBackoff {
    fn default() -> Self {
        Self::new()
    }
}
