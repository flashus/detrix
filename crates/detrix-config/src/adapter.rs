//! Debug adapter connection configuration
//!
//! Contains settings for connecting to DAP adapters (debugpy, delve, etc.)
//! including timeouts, reconnection policies, and batching thresholds.

use crate::constants::{
    DEFAULT_ADAPTER_PORT, DEFAULT_ADAPTER_START_MAX_RETRIES, DEFAULT_API_HOST,
    DEFAULT_BACKOFF_MULTIPLIER, DEFAULT_BATCH_CONCURRENCY, DEFAULT_BATCH_THRESHOLD,
    DEFAULT_CONNECTION_TIMEOUT_MS, DEFAULT_DAP_VALUE_DISPLAY_LIMIT, DEFAULT_EVENT_CHANNEL_CAPACITY,
    DEFAULT_HEALTH_CHECK_TIMEOUT_MS, DEFAULT_INITIAL_RECONNECT_DELAY_MS, DEFAULT_JITTER_RATIO,
    DEFAULT_MAX_CONNECTION_REFUSED_ATTEMPTS, DEFAULT_MAX_RECONNECT_ATTEMPTS,
    DEFAULT_MAX_RECONNECT_DELAY_MS, DEFAULT_REQUEST_TIMEOUT_MS, DEFAULT_RETRY_INTERVAL_MS,
    DEFAULT_SHUTDOWN_TIMEOUT_MS,
};
use serde::{Deserialize, Serialize};

// ============================================================================
// Adapter Connection Config
// ============================================================================

/// Debug adapter connection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterConnectionConfig {
    /// Default host for adapter connections
    #[serde(default = "default_adapter_host")]
    pub default_host: String,
    /// Default port for adapter connections (debugpy default)
    #[serde(default = "default_adapter_port")]
    pub default_port: u16,
    /// Connection timeout in milliseconds (for initial connection)
    #[serde(default = "default_connection_timeout_ms")]
    pub connection_timeout_ms: u64,
    /// Health check timeout in milliseconds (for adapter readiness checks)
    #[serde(default = "default_health_check_timeout_ms")]
    pub health_check_timeout_ms: u64,
    /// Shutdown timeout in milliseconds (for graceful adapter shutdown)
    #[serde(default = "default_shutdown_timeout_ms")]
    pub shutdown_timeout_ms: u64,
    /// Retry interval in milliseconds when connecting
    #[serde(default = "default_retry_interval_ms")]
    pub retry_interval_ms: u64,
    /// Maximum "connection refused" attempts before fast-fail
    /// When nothing is listening on the port, fail quickly instead of waiting for full timeout
    #[serde(default = "default_max_connection_refused_attempts")]
    pub max_connection_refused_attempts: u32,
    /// Maximum retries when starting an adapter (handles race conditions with concurrent start requests)
    #[serde(default = "default_adapter_start_max_retries")]
    pub adapter_start_max_retries: u32,
    /// Request timeout in milliseconds for DAP requests
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    /// Minimum number of metrics to trigger batch operations.
    #[serde(default = "default_batch_threshold")]
    pub batch_threshold: usize,
    /// Maximum concurrency for parallel adapter operations.
    #[serde(default = "default_batch_concurrency")]
    pub batch_concurrency: usize,
    /// Capacity of event channels in the DAP broker.
    #[serde(default = "default_event_channel_capacity")]
    pub event_channel_capacity: usize,
    /// Automatic reconnection configuration
    #[serde(default)]
    pub reconnect: ReconnectConfig,
    /// Maximum length for displayed variable values in DAP responses
    /// Longer values are truncated with "..." suffix
    /// Default: 100
    #[serde(default = "default_max_value_length")]
    pub max_value_length: usize,
}

fn default_adapter_host() -> String {
    DEFAULT_API_HOST.to_string()
}

fn default_adapter_port() -> u16 {
    DEFAULT_ADAPTER_PORT
}

fn default_connection_timeout_ms() -> u64 {
    DEFAULT_CONNECTION_TIMEOUT_MS
}

fn default_health_check_timeout_ms() -> u64 {
    DEFAULT_HEALTH_CHECK_TIMEOUT_MS
}

fn default_shutdown_timeout_ms() -> u64 {
    DEFAULT_SHUTDOWN_TIMEOUT_MS
}

fn default_retry_interval_ms() -> u64 {
    DEFAULT_RETRY_INTERVAL_MS
}

fn default_max_connection_refused_attempts() -> u32 {
    DEFAULT_MAX_CONNECTION_REFUSED_ATTEMPTS
}

fn default_adapter_start_max_retries() -> u32 {
    DEFAULT_ADAPTER_START_MAX_RETRIES
}

fn default_request_timeout_ms() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_MS
}

fn default_batch_threshold() -> usize {
    DEFAULT_BATCH_THRESHOLD
}

fn default_batch_concurrency() -> usize {
    DEFAULT_BATCH_CONCURRENCY
}

fn default_event_channel_capacity() -> usize {
    DEFAULT_EVENT_CHANNEL_CAPACITY
}

fn default_max_value_length() -> usize {
    DEFAULT_DAP_VALUE_DISPLAY_LIMIT
}

impl Default for AdapterConnectionConfig {
    fn default() -> Self {
        AdapterConnectionConfig {
            default_host: default_adapter_host(),
            default_port: default_adapter_port(),
            connection_timeout_ms: default_connection_timeout_ms(),
            health_check_timeout_ms: default_health_check_timeout_ms(),
            shutdown_timeout_ms: default_shutdown_timeout_ms(),
            retry_interval_ms: default_retry_interval_ms(),
            max_connection_refused_attempts: default_max_connection_refused_attempts(),
            adapter_start_max_retries: default_adapter_start_max_retries(),
            request_timeout_ms: default_request_timeout_ms(),
            batch_threshold: default_batch_threshold(),
            batch_concurrency: default_batch_concurrency(),
            event_channel_capacity: default_event_channel_capacity(),
            reconnect: ReconnectConfig::default(),
            max_value_length: default_max_value_length(),
        }
    }
}

// ============================================================================
// Reconnect Config
// ============================================================================

/// Automatic reconnection configuration with exponential backoff
///
/// Implements exponential backoff algorithm:
/// - First retry after `initial_delay_ms`
/// - Each subsequent retry doubles the delay
/// - Maximum delay capped at `max_delay_ms`
/// - Optional jitter to prevent thundering herd
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectConfig {
    /// Enable automatic reconnection
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum number of reconnection attempts (0 = unlimited)
    #[serde(default = "default_max_reconnect_attempts")]
    pub max_attempts: u32,
    /// Initial delay before first reconnect attempt (ms)
    #[serde(default = "default_initial_delay_ms")]
    pub initial_delay_ms: u64,
    /// Maximum delay between reconnect attempts (ms)
    #[serde(default = "default_max_delay_ms")]
    pub max_delay_ms: u64,
    /// Backoff multiplier (default: 2.0 for exponential)
    #[serde(default = "default_backoff_multiplier")]
    pub multiplier: f64,
    /// Add random jitter to prevent thundering herd (0.0-1.0)
    #[serde(default = "default_jitter")]
    pub jitter: f64,
    /// Re-register metrics after successful reconnection
    #[serde(default = "default_true")]
    pub restore_metrics: bool,
}

fn default_true() -> bool {
    true
}

fn default_max_reconnect_attempts() -> u32 {
    DEFAULT_MAX_RECONNECT_ATTEMPTS
}

fn default_initial_delay_ms() -> u64 {
    DEFAULT_INITIAL_RECONNECT_DELAY_MS
}

fn default_max_delay_ms() -> u64 {
    DEFAULT_MAX_RECONNECT_DELAY_MS
}

fn default_backoff_multiplier() -> f64 {
    DEFAULT_BACKOFF_MULTIPLIER
}

fn default_jitter() -> f64 {
    DEFAULT_JITTER_RATIO
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            max_attempts: default_max_reconnect_attempts(),
            initial_delay_ms: default_initial_delay_ms(),
            max_delay_ms: default_max_delay_ms(),
            multiplier: default_backoff_multiplier(),
            jitter: default_jitter(),
            restore_metrics: default_true(),
        }
    }
}
