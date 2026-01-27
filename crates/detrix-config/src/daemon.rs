//! Daemon configuration for background service mode

use crate::constants::{
    DEFAULT_DAEMON_POLL_INTERVAL_MS, DEFAULT_DRAIN_TIMEOUT_MS, DEFAULT_HEALTH_CHECK_INTERVAL_MS,
    DEFAULT_LOG_RETENTION_DAYS, DEFAULT_MCP_BRIDGE_HEARTBEAT_INTERVAL_SECS,
    DEFAULT_MCP_BRIDGE_TIMEOUT_MS, DEFAULT_MCP_CLEANUP_INTERVAL_SECS,
    DEFAULT_MCP_HEARTBEAT_MAX_FAILURES, DEFAULT_MCP_HEARTBEAT_TIMEOUT_SECS,
    DEFAULT_MCP_TOOL_TIMEOUT_MS, DEFAULT_MCP_USAGE_HISTORY, DEFAULT_RESTART_DELAY_MS,
    DEFAULT_SHUTDOWN_GRACE_PERIOD_SECS,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ============================================================================
// Logging Config
// ============================================================================

/// Logging configuration for daemon and MCP processes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log directory (default: ~/detrix/log/)
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    /// Daemon log file name (default: detrix_daemon.log)
    #[serde(default = "default_daemon_log_file")]
    pub daemon_log_file: String,
    /// MCP log file prefix (default: detrix_mcp)
    /// Full filename will be {prefix}_{pid}.log
    #[serde(default = "default_mcp_log_prefix")]
    pub mcp_log_prefix: String,
    /// Enable file logging (default: true)
    #[serde(default = "default_file_logging_enabled")]
    pub file_logging_enabled: bool,
    /// Log file retention in days (default: 7)
    /// Log files older than this will be deleted at daemon startup.
    /// Set to 0 to disable automatic cleanup.
    #[serde(default = "default_log_retention_days")]
    pub log_retention_days: u32,
    /// Use UTC timestamps in logs (default: false = local time)
    #[serde(default)]
    pub use_utc: bool,
}

fn default_log_dir() -> PathBuf {
    crate::paths::default_log_dir()
}

fn default_daemon_log_file() -> String {
    crate::paths::DEFAULT_DAEMON_LOG_FILENAME.to_string()
}

fn default_mcp_log_prefix() -> String {
    crate::paths::DEFAULT_MCP_LOG_PREFIX.to_string()
}

fn default_file_logging_enabled() -> bool {
    true
}

fn default_log_retention_days() -> u32 {
    DEFAULT_LOG_RETENTION_DAYS
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            log_dir: default_log_dir(),
            daemon_log_file: default_daemon_log_file(),
            mcp_log_prefix: default_mcp_log_prefix(),
            file_logging_enabled: default_file_logging_enabled(),
            log_retention_days: default_log_retention_days(),
            use_utc: false,
        }
    }
}

impl LoggingConfig {
    /// Get full path to daemon log file (for tracing logs with daily rotation)
    pub fn daemon_log_path(&self) -> PathBuf {
        self.log_dir.join(&self.daemon_log_file)
    }

    /// Get full path to daemon startup log file (captures stdout/stderr of daemon process)
    pub fn daemon_startup_log_path(&self) -> PathBuf {
        self.log_dir
            .join(crate::paths::DEFAULT_DAEMON_STARTUP_LOG_FILENAME)
    }

    /// Get full path to MCP log file for a specific PID
    pub fn mcp_log_path(&self, pid: u32) -> PathBuf {
        self.log_dir
            .join(format!("{}_{}.log", self.mcp_log_prefix, pid))
    }

    /// Get full path to MCP log file for current process
    pub fn current_mcp_log_path(&self) -> PathBuf {
        self.mcp_log_path(std::process::id())
    }

    /// Get full path to lldb-serve log file for current process
    ///
    /// Returns `{log_dir}/lldb_serve_{pid}.log`
    pub fn current_lldb_serve_log_path(&self) -> PathBuf {
        self.log_dir
            .join(format!("lldb_serve_{}.log", std::process::id()))
    }
}

// ============================================================================
// Daemon Config
// ============================================================================

/// Daemon configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// PID file path
    #[serde(default = "default_pid_file_path")]
    pub pid_file: PathBuf,
    /// Graceful shutdown drain timeout in milliseconds.
    #[serde(default = "default_drain_timeout_ms")]
    pub drain_timeout_ms: u64,
    /// Health check interval for supervised connections in milliseconds
    /// Default: 5000ms (5 seconds)
    #[serde(default = "default_health_check_interval_ms")]
    pub health_check_interval_ms: u64,
    /// Restart delay after connection failure in milliseconds
    /// Default: 500ms
    #[serde(default = "default_restart_delay_ms")]
    pub restart_delay_ms: u64,
    /// \[ADVANCED\] Poll interval for daemon startup/shutdown checks in milliseconds
    /// Used by CLI when waiting for daemon to start or stop.
    /// Only change this if you have specific timing requirements.
    /// Default: 100ms
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,
}

fn default_pid_file_path() -> PathBuf {
    crate::paths::default_pid_path()
}

fn default_drain_timeout_ms() -> u64 {
    DEFAULT_DRAIN_TIMEOUT_MS
}

fn default_health_check_interval_ms() -> u64 {
    DEFAULT_HEALTH_CHECK_INTERVAL_MS
}

fn default_restart_delay_ms() -> u64 {
    DEFAULT_RESTART_DELAY_MS
}

fn default_poll_interval_ms() -> u64 {
    DEFAULT_DAEMON_POLL_INTERVAL_MS
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            pid_file: default_pid_file_path(),
            drain_timeout_ms: default_drain_timeout_ms(),
            health_check_interval_ms: DEFAULT_HEALTH_CHECK_INTERVAL_MS,
            restart_delay_ms: DEFAULT_RESTART_DELAY_MS,
            poll_interval_ms: DEFAULT_DAEMON_POLL_INTERVAL_MS,
            logging: LoggingConfig::default(),
        }
    }
}

// ============================================================================
// MCP Bridge Config
// ============================================================================

/// Configuration for MCP bridge communication with daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpBridgeConfig {
    /// HTTP timeout in milliseconds for bridge-to-daemon requests
    /// Default: 60000ms (60 seconds)
    #[serde(default = "default_bridge_timeout_ms")]
    pub bridge_timeout_ms: u64,
    /// Timeout in milliseconds for individual tool operations (add metric, toggle, etc.)
    /// Must be shorter than bridge_timeout_ms
    /// Default: 5000ms (5 seconds)
    #[serde(default = "default_tool_timeout_ms")]
    pub tool_timeout_ms: u64,
    /// Heartbeat interval in seconds (how often bridge sends heartbeat to daemon)
    /// Default: 10 seconds
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,
    /// Heartbeat timeout in seconds (daemon marks client stale after this)
    /// Default: 30 seconds
    #[serde(default = "default_heartbeat_timeout_secs")]
    pub heartbeat_timeout_secs: u64,
    /// Maximum consecutive heartbeat failures before attempting daemon restart
    /// Default: 2
    #[serde(default = "default_heartbeat_max_failures")]
    pub heartbeat_max_failures: u32,
    /// Grace period in seconds before daemon auto-shutdown when no clients
    /// Default: 10 seconds
    #[serde(default = "default_shutdown_grace_period_secs")]
    pub shutdown_grace_period_secs: u64,
    /// Cleanup interval in seconds for stale client detection
    /// Default: 5 seconds
    #[serde(default = "default_cleanup_interval_secs")]
    pub cleanup_interval_secs: u64,
    /// Maximum MCP usage history entries per tool
    /// Default: 20
    #[serde(default = "default_usage_history_size")]
    pub usage_history_size: usize,
}

fn default_bridge_timeout_ms() -> u64 {
    DEFAULT_MCP_BRIDGE_TIMEOUT_MS
}

fn default_tool_timeout_ms() -> u64 {
    DEFAULT_MCP_TOOL_TIMEOUT_MS
}

fn default_heartbeat_interval_secs() -> u64 {
    DEFAULT_MCP_BRIDGE_HEARTBEAT_INTERVAL_SECS
}

fn default_heartbeat_timeout_secs() -> u64 {
    DEFAULT_MCP_HEARTBEAT_TIMEOUT_SECS
}

fn default_heartbeat_max_failures() -> u32 {
    DEFAULT_MCP_HEARTBEAT_MAX_FAILURES
}

fn default_shutdown_grace_period_secs() -> u64 {
    DEFAULT_SHUTDOWN_GRACE_PERIOD_SECS
}

fn default_cleanup_interval_secs() -> u64 {
    DEFAULT_MCP_CLEANUP_INTERVAL_SECS
}

fn default_usage_history_size() -> usize {
    DEFAULT_MCP_USAGE_HISTORY
}

impl Default for McpBridgeConfig {
    fn default() -> Self {
        McpBridgeConfig {
            bridge_timeout_ms: default_bridge_timeout_ms(),
            tool_timeout_ms: default_tool_timeout_ms(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            heartbeat_timeout_secs: default_heartbeat_timeout_secs(),
            heartbeat_max_failures: default_heartbeat_max_failures(),
            shutdown_grace_period_secs: default_shutdown_grace_period_secs(),
            cleanup_interval_secs: default_cleanup_interval_secs(),
            usage_history_size: default_usage_history_size(),
        }
    }
}

impl McpBridgeConfig {
    /// Validate MCP bridge configuration constraints
    ///
    /// Returns a list of validation errors (empty if valid).
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Tool timeout must be less than bridge timeout
        // Individual operations should complete faster than the overall request
        if self.tool_timeout_ms >= self.bridge_timeout_ms {
            errors.push(format!(
                "mcp.tool_timeout_ms ({}) must be less than mcp.bridge_timeout_ms ({})",
                self.tool_timeout_ms, self.bridge_timeout_ms
            ));
        }

        // Heartbeat interval must be less than heartbeat timeout
        // Otherwise daemon would mark client stale before receiving heartbeat
        if self.heartbeat_interval_secs >= self.heartbeat_timeout_secs {
            errors.push(format!(
                "mcp.heartbeat_interval_secs ({}) must be less than mcp.heartbeat_timeout_secs ({})",
                self.heartbeat_interval_secs, self.heartbeat_timeout_secs
            ));
        }

        // Cleanup interval should be at least as frequent as heartbeat timeout
        // to detect stale clients in a timely manner
        if self.cleanup_interval_secs > self.heartbeat_timeout_secs {
            errors.push(format!(
                "mcp.cleanup_interval_secs ({}) should not exceed mcp.heartbeat_timeout_secs ({})",
                self.cleanup_interval_secs, self.heartbeat_timeout_secs
            ));
        }

        errors
    }

    /// Validate that bridge timeout is longer than adapter connection timeout
    ///
    /// This ensures proper error propagation - if an adapter connection fails,
    /// the MCP bridge should wait long enough to receive the error response.
    pub fn validate_against_connection_timeout(&self, connection_timeout_ms: u64) -> Vec<String> {
        let mut errors = Vec::new();

        if self.bridge_timeout_ms <= connection_timeout_ms {
            errors.push(format!(
                "mcp.bridge_timeout_ms ({}) must be greater than adapter.connection_timeout_ms ({}) \
                to allow proper error propagation from adapter connections",
                self.bridge_timeout_ms, connection_timeout_ms
            ));
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logging_config_default() {
        let config = LoggingConfig::default();
        // Default log dir should be under detrix home
        assert!(config.log_dir.to_string_lossy().contains("detrix"));
        // Default daemon log file
        assert_eq!(config.daemon_log_file, "detrix_daemon.log");
        // Default MCP log prefix
        assert_eq!(config.mcp_log_prefix, "detrix_mcp");
        // File logging enabled by default
        assert!(config.file_logging_enabled);
    }

    #[test]
    fn test_logging_config_paths() {
        let config = LoggingConfig::default();

        // Daemon log path should combine log_dir and daemon_log_file
        let daemon_path = config.daemon_log_path();
        assert!(daemon_path.to_string_lossy().contains("detrix_daemon.log"));

        // MCP log path should include PID
        let mcp_path = config.mcp_log_path(12345);
        assert!(mcp_path.to_string_lossy().contains("detrix_mcp_12345.log"));
    }

    #[test]
    fn test_logging_config_custom_paths() {
        let config = LoggingConfig {
            log_dir: PathBuf::from("/custom/log/dir"),
            daemon_log_file: "custom_daemon.log".to_string(),
            mcp_log_prefix: "custom_mcp".to_string(),
            file_logging_enabled: true,
            log_retention_days: 7,
            use_utc: false,
        };

        let daemon_path = config.daemon_log_path();
        assert_eq!(
            daemon_path,
            PathBuf::from("/custom/log/dir/custom_daemon.log")
        );

        let mcp_path = config.mcp_log_path(999);
        assert_eq!(
            mcp_path,
            PathBuf::from("/custom/log/dir/custom_mcp_999.log")
        );
    }

    #[test]
    fn test_mcp_bridge_config_default_valid() {
        let config = McpBridgeConfig::default();
        let errors = config.validate();
        assert!(
            errors.is_empty(),
            "Default config should be valid: {:?}",
            errors
        );
    }

    #[test]
    fn test_mcp_bridge_config_heartbeat_interval_less_than_timeout() {
        // Make interval >= timeout (invalid)
        let config = McpBridgeConfig {
            heartbeat_interval_secs: 30,
            heartbeat_timeout_secs: 30,
            ..Default::default()
        };

        let errors = config.validate();
        assert!(!errors.is_empty());
        assert!(errors[0].contains("heartbeat_interval_secs"));
    }

    #[test]
    fn test_mcp_bridge_config_cleanup_interval_exceeds_timeout() {
        // Make cleanup interval > heartbeat timeout (warning)
        let config = McpBridgeConfig {
            cleanup_interval_secs: 60,
            heartbeat_timeout_secs: 30,
            ..Default::default()
        };

        let errors = config.validate();
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.contains("cleanup_interval_secs")));
    }

    #[test]
    fn test_mcp_bridge_timeout_vs_connection_timeout() {
        let config = McpBridgeConfig::default();

        // Bridge timeout (60000) should be > connection timeout (30000)
        let errors = config.validate_against_connection_timeout(30_000);
        assert!(errors.is_empty(), "Default should be valid: {:?}", errors);

        // If bridge timeout is less than or equal to connection timeout, it's invalid
        let mut short_bridge = config.clone();
        short_bridge.bridge_timeout_ms = 25_000;
        let errors = short_bridge.validate_against_connection_timeout(30_000);
        assert!(!errors.is_empty());
        assert!(errors[0].contains("bridge_timeout_ms"));
    }
}
