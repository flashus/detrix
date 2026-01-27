//! Output configuration for event streaming to external systems
//!
//! Supports GELF (Graylog Extended Log Format) over TCP, UDP, or HTTP.

use crate::constants::{
    DEFAULT_API_HOST, DEFAULT_CIRCUIT_BREAKER_FAILURE_THRESHOLD,
    DEFAULT_CIRCUIT_BREAKER_RESET_TIMEOUT_MS, DEFAULT_CIRCUIT_BREAKER_SUCCESS_THRESHOLD,
    DEFAULT_COMPRESSION_LEVEL, DEFAULT_GELF_HTTP_BATCH_SIZE, DEFAULT_GELF_HTTP_PATH,
    DEFAULT_GELF_PORT, DEFAULT_GELF_TCP_CONNECT_TIMEOUT_MS, DEFAULT_GELF_TCP_WRITE_TIMEOUT_MS,
    DEFAULT_GELF_TRANSPORT,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Output Config
// ============================================================================

/// Output configuration for event streaming to external systems
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputConfig {
    /// GELF/Graylog output configuration
    #[serde(default)]
    pub gelf: GelfOutputConfig,
}

/// GELF output configuration
///
/// Supports TCP, UDP, and HTTP transports to Graylog.
/// See: <https://go2docs.graylog.org/current/getting_in_log_data/gelf.html>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GelfOutputConfig {
    /// Enable GELF output
    #[serde(default)]
    pub enabled: bool,

    /// Transport type: "tcp", "udp", or "http"
    #[serde(default = "default_gelf_transport")]
    pub transport: String,

    /// Graylog server host
    #[serde(default = "default_gelf_host")]
    pub host: String,

    /// Graylog server port (default: 12201)
    #[serde(default = "default_gelf_port")]
    pub port: u16,

    /// Hostname to include in GELF messages (defaults to "detrix")
    #[serde(default)]
    pub source_host: Option<String>,

    /// TCP-specific settings
    #[serde(default)]
    pub tcp: GelfTcpSettings,

    /// UDP-specific settings
    #[serde(default)]
    pub udp: GelfUdpSettings,

    /// HTTP-specific settings
    #[serde(default)]
    pub http: GelfHttpSettings,

    /// Stream routing rules
    #[serde(default)]
    pub routes: Vec<GelfRouteConfig>,

    /// Extra fields to add to all GELF messages
    #[serde(default)]
    pub extra_fields: HashMap<String, String>,
}

fn default_gelf_transport() -> String {
    DEFAULT_GELF_TRANSPORT.to_string()
}

fn default_gelf_host() -> String {
    DEFAULT_API_HOST.to_string()
}

fn default_gelf_port() -> u16 {
    DEFAULT_GELF_PORT
}

impl Default for GelfOutputConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: DEFAULT_GELF_TRANSPORT.to_string(),
            host: DEFAULT_API_HOST.to_string(),
            port: DEFAULT_GELF_PORT,
            source_host: None,
            tcp: GelfTcpSettings::default(),
            udp: GelfUdpSettings::default(),
            http: GelfHttpSettings::default(),
            routes: Vec::new(),
            extra_fields: HashMap::new(),
        }
    }
}

/// TCP-specific GELF settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GelfTcpSettings {
    /// Connection timeout in milliseconds
    #[serde(default = "default_gelf_connect_timeout")]
    pub connect_timeout_ms: u64,

    /// Write timeout in milliseconds
    #[serde(default = "default_gelf_write_timeout")]
    pub write_timeout_ms: u64,

    /// Auto-reconnect on connection failure
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,

    /// Circuit breaker configuration (optional)
    #[serde(default)]
    pub circuit_breaker: Option<CircuitBreakerSettings>,
}

/// Circuit breaker settings for output protection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerSettings {
    /// Number of failures before opening the circuit
    #[serde(default = "default_circuit_breaker_failure_threshold")]
    pub failure_threshold: u32,

    /// Timeout in milliseconds before attempting to close the circuit
    #[serde(default = "default_circuit_breaker_reset_timeout_ms")]
    pub reset_timeout_ms: u64,

    /// Number of successes needed to close the circuit from half-open state
    #[serde(default = "default_circuit_breaker_success_threshold")]
    pub success_threshold: u32,
}

fn default_circuit_breaker_failure_threshold() -> u32 {
    DEFAULT_CIRCUIT_BREAKER_FAILURE_THRESHOLD
}

fn default_circuit_breaker_reset_timeout_ms() -> u64 {
    DEFAULT_CIRCUIT_BREAKER_RESET_TIMEOUT_MS
}

fn default_circuit_breaker_success_threshold() -> u32 {
    DEFAULT_CIRCUIT_BREAKER_SUCCESS_THRESHOLD
}

impl Default for CircuitBreakerSettings {
    fn default() -> Self {
        Self {
            failure_threshold: DEFAULT_CIRCUIT_BREAKER_FAILURE_THRESHOLD,
            reset_timeout_ms: DEFAULT_CIRCUIT_BREAKER_RESET_TIMEOUT_MS,
            success_threshold: DEFAULT_CIRCUIT_BREAKER_SUCCESS_THRESHOLD,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_gelf_connect_timeout() -> u64 {
    DEFAULT_GELF_TCP_CONNECT_TIMEOUT_MS
}

fn default_gelf_write_timeout() -> u64 {
    DEFAULT_GELF_TCP_WRITE_TIMEOUT_MS
}

impl Default for GelfTcpSettings {
    fn default() -> Self {
        Self {
            connect_timeout_ms: DEFAULT_GELF_TCP_CONNECT_TIMEOUT_MS,
            write_timeout_ms: DEFAULT_GELF_TCP_WRITE_TIMEOUT_MS,
            auto_reconnect: true,
            circuit_breaker: None,
        }
    }
}

/// UDP-specific GELF settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GelfUdpSettings {
    /// Enable gzip compression (recommended for UDP)
    #[serde(default = "default_true")]
    pub compress: bool,

    /// Compression level (0-9, default: 6)
    #[serde(default = "default_compression_level")]
    pub compression_level: u32,
}

fn default_compression_level() -> u32 {
    DEFAULT_COMPRESSION_LEVEL
}

impl Default for GelfUdpSettings {
    fn default() -> Self {
        Self {
            compress: true,
            compression_level: DEFAULT_COMPRESSION_LEVEL,
        }
    }
}

/// HTTP-specific GELF settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GelfHttpSettings {
    /// HTTP path for GELF endpoint (default: "/gelf")
    #[serde(default = "default_gelf_http_path")]
    pub path: String,

    /// Request timeout in milliseconds
    #[serde(default = "default_gelf_http_timeout")]
    pub timeout_ms: u64,

    /// Batch size (messages per HTTP request)
    #[serde(default = "default_gelf_batch_size")]
    pub batch_size: usize,

    /// Flush interval in milliseconds (0 = manual flush only)
    #[serde(default)]
    pub flush_interval_ms: u64,

    /// Enable gzip compression for HTTP body
    #[serde(default)]
    pub compress: bool,

    /// Optional Bearer token for authentication
    #[serde(default)]
    pub auth_token: Option<String>,

    /// Circuit breaker configuration (optional)
    #[serde(default)]
    pub circuit_breaker: Option<CircuitBreakerSettings>,
}

fn default_gelf_http_path() -> String {
    DEFAULT_GELF_HTTP_PATH.to_string()
}

fn default_gelf_http_timeout() -> u64 {
    DEFAULT_GELF_TCP_WRITE_TIMEOUT_MS // Same default as TCP write timeout
}

fn default_gelf_batch_size() -> usize {
    DEFAULT_GELF_HTTP_BATCH_SIZE
}

impl Default for GelfHttpSettings {
    fn default() -> Self {
        Self {
            path: DEFAULT_GELF_HTTP_PATH.to_string(),
            timeout_ms: DEFAULT_GELF_TCP_WRITE_TIMEOUT_MS,
            batch_size: DEFAULT_GELF_HTTP_BATCH_SIZE,
            flush_interval_ms: 0,
            compress: false,
            auth_token: None,
            circuit_breaker: None,
        }
    }
}

/// GELF stream routing rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GelfRouteConfig {
    /// Match condition
    #[serde(default, rename = "match")]
    pub match_rule: GelfRouteMatch,

    /// Target Graylog stream name
    pub stream: String,

    /// Optional facility override
    #[serde(default)]
    pub facility: Option<String>,

    /// Optional GELF level override (0-7)
    #[serde(default)]
    pub level: Option<u8>,
}

/// Match conditions for GELF routing
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GelfRouteMatch {
    /// Match by connection ID (exact or glob pattern)
    #[serde(default)]
    pub connection_id: Option<String>,

    /// Match by metric group name
    #[serde(default)]
    pub group: Option<String>,

    /// Match by metric name pattern (regex)
    #[serde(default)]
    pub metric_name: Option<String>,

    /// Match error events only
    #[serde(default)]
    pub errors_only: bool,
}

// Tests for GelfOutputConfig are in types.rs::test_gelf_output_config_defaults
// and types.rs::test_gelf_config_from_toml (which tests TOML parsing)
