//! Configuration types for Detrix
//!
//! All configuration structures are defined here. They use domain types
//! from `detrix-core` where appropriate (Location, MetricMode, SafetyLevel).
//!
//! # Module Organization
//!
//! Configuration is split into logical modules:
//! - `adapter` - DAP adapter connection settings
//! - `anchor` - Metric location tracking and auto-relocation settings
//! - `api` - gRPC and REST endpoint settings
//! - `daemon` - Background service settings
//! - `output` - GELF/Graylog output settings
//! - `safety` - Expression safety validation settings
//! - `storage` - SQLite and event batching settings
//! - `tui` - Terminal UI settings

// Re-export from submodules
mod adapter_mod {
    pub use crate::adapter::*;
}
mod anchor_mod {
    pub use crate::anchor::*;
}
mod api_mod {
    pub use crate::api::*;
}
mod daemon_mod {
    pub use crate::daemon::*;
}
mod output_mod {
    pub use crate::output::*;
}
mod safety_mod {
    pub use crate::safety::*;
}
mod storage_mod {
    pub use crate::storage::*;
}
mod tui_mod {
    pub use crate::tui::*;
}

// Re-export all types from submodules
pub use adapter_mod::*;
pub use anchor_mod::*;
pub use api_mod::*;
pub use daemon_mod::*;
pub use output_mod::*;
pub use safety_mod::*;
pub use storage_mod::*;
pub use tui_mod::*;

use crate::constants::{
    DEFAULT_AUDIT_RETENTION_DAYS, DEFAULT_AUTO_SLEEP_SECONDS, DEFAULT_MAX_EVAL_TIME_MS,
    DEFAULT_MAX_EXPRESSION_LENGTH, DEFAULT_MAX_METRICS_PER_GROUP, DEFAULT_MAX_METRICS_TOTAL,
    DEFAULT_MAX_PER_SECOND, DEFAULT_ON_ERROR, DEFAULT_RUNTIME_MODE,
    DEFAULT_SAMPLE_INTERVAL_SECONDS, DEFAULT_SAMPLE_RATE,
};
use detrix_core::{Location, MetricMode, SafetyLevel};
use serde::{Deserialize, Serialize};

// ============================================================================
// Main Config
// ============================================================================

/// Main Detrix configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub metadata: MetadataConfig,
    #[serde(default)]
    pub compatibility: CompatibilityConfig,
    #[serde(default)]
    pub project: ProjectConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub safety: SafetyConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub adapter: AdapterConnectionConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub mcp: McpBridgeConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub tui: TuiConfig,
    #[serde(default)]
    pub anchor: AnchorConfig,
    #[serde(default)]
    pub audit: AuditConfig,
    #[serde(default)]
    pub metric: Vec<MetricDefinition>,
}

impl Config {
    /// Returns a redacted copy of the config safe for API responses.
    ///
    /// Sensitive fields are replaced with "***REDACTED***":
    /// - `output.gelf.http.auth_token`
    /// - `api.auth.bearer_token`
    ///
    /// Use this method before serializing config for external consumption.
    pub fn redacted(&self) -> Self {
        let mut config = self.clone();

        // Redact GELF HTTP auth token
        if config.output.gelf.http.auth_token.is_some() {
            config.output.gelf.http.auth_token = Some("***REDACTED***".to_string());
        }

        // Redact API auth bearer token
        if config.api.auth.bearer_token.is_some() {
            config.api.auth.bearer_token = Some("***REDACTED***".to_string());
        }

        config
    }
}

/// Resolve a path relative to a base directory.
///
/// If the path is already absolute, returns it unchanged.
/// Otherwise, joins it with the base directory.
pub fn resolve_path<P: AsRef<std::path::Path>>(
    base_dir: &std::path::Path,
    path: P,
) -> std::path::PathBuf {
    let p = path.as_ref();
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

// ============================================================================
// Metadata & Compatibility
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataConfig {
    pub version: String,
    #[serde(default)]
    pub description: String,
}

impl Default for MetadataConfig {
    fn default() -> Self {
        MetadataConfig {
            version: "1.0".to_string(),
            description: String::new(),
        }
    }
}

/// Runtime compatibility requirements
///
/// **NOTE: NOT YET IMPLEMENTED** - These settings are parsed but not enforced.
/// Version checks are planned for a future release. The config section is
/// retained for forward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityConfig {
    /// Minimum Python version (e.g. "3.8")
    #[serde(default = "default_python_min")]
    pub python_min: String,
    /// Maximum Python version (e.g. "3.13")
    #[serde(default = "default_python_max")]
    pub python_max: String,
    /// Minimum Go version for delve (e.g. "1.18")
    #[serde(default = "default_go_min")]
    pub go_min: String,
}

fn default_python_min() -> String {
    "3.8".to_string()
}

fn default_python_max() -> String {
    "3.13".to_string()
}

fn default_go_min() -> String {
    "1.18".to_string()
}

impl Default for CompatibilityConfig {
    fn default() -> Self {
        CompatibilityConfig {
            python_min: default_python_min(),
            python_max: default_python_max(),
            go_min: default_go_min(),
        }
    }
}

// ============================================================================
// Project Config
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default = "default_base_path")]
    pub base_path: String,
}

fn default_base_path() -> String {
    ".".to_string()
}

impl Default for ProjectConfig {
    fn default() -> Self {
        ProjectConfig {
            base_path: default_base_path(),
        }
    }
}

// ============================================================================
// Defaults Config
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub mode: MetricMode,
    #[serde(default = "default_true")]
    pub thread_local: bool,
    #[serde(default = "default_true")]
    pub auto_adjust: bool,
    #[serde(default = "default_on_error")]
    pub on_error: String,
    #[serde(default = "default_max_eval_time_ms")]
    pub max_eval_time_ms: u32,
    #[serde(default)]
    pub safety_level: SafetyLevel,

    // Metric mode defaults
    /// Default sample rate for Sample mode (capture every Nth hit)
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    /// Default interval for SampleInterval mode (capture every N seconds)
    #[serde(default = "default_sample_interval_seconds")]
    pub sample_interval_seconds: u32,
    /// Default max events per second for Throttle mode
    #[serde(default = "default_max_per_second")]
    pub max_per_second: u32,
}

fn default_sample_rate() -> u32 {
    DEFAULT_SAMPLE_RATE
}

fn default_sample_interval_seconds() -> u32 {
    DEFAULT_SAMPLE_INTERVAL_SECONDS
}

fn default_max_per_second() -> u32 {
    DEFAULT_MAX_PER_SECOND
}

fn default_true() -> bool {
    true
}

fn default_on_error() -> String {
    DEFAULT_ON_ERROR.to_string()
}

fn default_max_eval_time_ms() -> u32 {
    DEFAULT_MAX_EVAL_TIME_MS
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        DefaultsConfig {
            enabled: default_true(),
            mode: MetricMode::default(),
            thread_local: default_true(),
            auto_adjust: default_true(),
            on_error: default_on_error(),
            max_eval_time_ms: default_max_eval_time_ms(),
            safety_level: SafetyLevel::default(),
            sample_rate: default_sample_rate(),
            sample_interval_seconds: default_sample_interval_seconds(),
            max_per_second: default_max_per_second(),
        }
    }
}

// ============================================================================
// Runtime Config
// ============================================================================

/// Runtime behavior configuration for the daemon
///
/// **NOTE: NOT YET IMPLEMENTED** - These settings are parsed but not applied.
/// Auto-sleep and wake-on-request features are planned for a future release.
/// The config section is retained for forward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default = "default_runtime_mode")]
    pub default_mode: String,
    #[serde(default = "default_true")]
    pub wake_on_request: bool,
    #[serde(default = "default_auto_sleep")]
    pub auto_sleep_after_seconds: u64,
}

fn default_runtime_mode() -> String {
    DEFAULT_RUNTIME_MODE.to_string()
}

fn default_auto_sleep() -> u64 {
    DEFAULT_AUTO_SLEEP_SECONDS
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            default_mode: default_runtime_mode(),
            wake_on_request: default_true(),
            auto_sleep_after_seconds: default_auto_sleep(),
        }
    }
}

// ============================================================================
// Limits Config
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    #[serde(default = "default_max_metrics_total")]
    pub max_metrics_total: usize,
    #[serde(default = "default_max_metrics_per_group")]
    pub max_metrics_per_group: usize,
    #[serde(default = "default_max_expression_length")]
    pub max_expression_length: usize,
}

fn default_max_metrics_total() -> usize {
    DEFAULT_MAX_METRICS_TOTAL
}

fn default_max_metrics_per_group() -> usize {
    DEFAULT_MAX_METRICS_PER_GROUP
}

fn default_max_expression_length() -> usize {
    DEFAULT_MAX_EXPRESSION_LENGTH
}

impl Default for LimitsConfig {
    fn default() -> Self {
        LimitsConfig {
            max_metrics_total: default_max_metrics_total(),
            max_metrics_per_group: default_max_metrics_per_group(),
            max_expression_length: default_max_expression_length(),
        }
    }
}

// ============================================================================
// Metric Config (Per-metric overrides)
// ============================================================================

/// Per-metric configuration overrides
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<MetricMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_level: Option<SafetyLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_eval_time_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// Metric definition from TOML config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDefinition {
    pub name: String,
    /// Required: Which connection this metric belongs to
    pub connection_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub location: Location,
    pub expression: String,
    pub language: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub mode: MetricMode,
    #[serde(default)]
    pub safety_level: SafetyLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

// ============================================================================
// Audit Config
// ============================================================================

/// Audit trail configuration
///
/// Controls which events are recorded for audit purposes and how long
/// they are retained.
///
/// **NOTE: NOT YET IMPLEMENTED** - These settings are parsed but audit logging
/// is not yet implemented. Audit trail feature is planned for a future release.
/// The config section is retained for forward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    /// Whether audit logging is enabled
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,

    /// Number of days to retain audit events (0 = forever)
    #[serde(default = "default_audit_retention_days")]
    pub retention_days: u32,

    /// Which event types to audit (empty = all audit events)
    ///
    /// Valid values: config_updated, config_validation_failed,
    /// api_call_executed, api_call_failed, authentication_failed,
    /// expression_validated
    #[serde(default)]
    pub event_types: Vec<String>,

    /// Include detailed change tracking (old/new values)
    #[serde(default = "default_audit_include_changes")]
    pub include_changes: bool,

    /// Include expression details in validation audit events
    #[serde(default = "default_audit_include_expressions")]
    pub include_expressions: bool,
}

fn default_audit_enabled() -> bool {
    true
}

fn default_audit_retention_days() -> u32 {
    DEFAULT_AUDIT_RETENTION_DAYS
}

fn default_audit_include_changes() -> bool {
    true
}

fn default_audit_include_expressions() -> bool {
    false // Expressions might contain sensitive info
}

impl Default for AuditConfig {
    fn default() -> Self {
        AuditConfig {
            enabled: default_audit_enabled(),
            retention_days: default_audit_retention_days(),
            event_types: Vec::new(), // Empty = all audit events
            include_changes: default_audit_include_changes(),
            include_expressions: default_audit_include_expressions(),
        }
    }
}

impl AuditConfig {
    /// Check if a specific event type should be audited
    pub fn should_audit(&self, event_type: &str) -> bool {
        if !self.enabled {
            return false;
        }
        // Empty list means audit all events
        if self.event_types.is_empty() {
            return true;
        }
        self.event_types.iter().any(|t| t == event_type)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert_eq!(config.runtime.default_mode, "sleep");
        assert_eq!(config.storage.storage_type, "sqlite");
        assert!(config.safety.enable_ast_analysis);
    }

    #[test]
    fn test_safety_config_sets() {
        let safety = SafetyConfig::default();
        let allowed = safety.python.allowed_set();
        let prohibited = safety.python.prohibited_set();

        assert!(allowed.contains("len"));
        assert!(allowed.contains("str"));
        assert!(prohibited.contains("eval"));
        assert!(prohibited.contains("exec"));
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("[runtime]"));
        assert!(toml_str.contains("[storage]"));
        assert!(toml_str.contains("[safety]"));
    }

    #[test]
    fn test_gelf_config_from_toml() {
        let toml_str = r#"
            [output.gelf]
            enabled = true
            transport = "tcp"
            host = "graylog.example.com"
            port = 12202
            source_host = "my-service"

            [output.gelf.tcp]
            connect_timeout_ms = 3000
            auto_reconnect = true

            [output.gelf.extra_fields]
            environment = "production"
            datacenter = "us-east-1"

            [[output.gelf.routes]]
            stream = "trading-bot"
            [output.gelf.routes.match]
            connection_id = "trading-*"

            [[output.gelf.routes]]
            stream = "errors"
            [output.gelf.routes.match]
            errors_only = true
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.output.gelf.enabled);
        assert_eq!(config.output.gelf.host, "graylog.example.com");
        assert_eq!(config.output.gelf.port, 12202);
        assert_eq!(
            config.output.gelf.source_host,
            Some("my-service".to_string())
        );
        assert_eq!(config.output.gelf.tcp.connect_timeout_ms, 3000);
        assert_eq!(
            config.output.gelf.extra_fields.get("environment"),
            Some(&"production".to_string())
        );
        assert_eq!(config.output.gelf.routes.len(), 2);
        assert_eq!(config.output.gelf.routes[0].stream, "trading-bot");
        assert!(config.output.gelf.routes[1].match_rule.errors_only);
    }

    #[test]
    fn test_config_redacted() {
        let mut config = Config::default();
        config.output.gelf.http.auth_token = Some("secret-token-123".to_string());

        let redacted = config.redacted();

        // Original should still have the secret
        assert_eq!(
            config.output.gelf.http.auth_token,
            Some("secret-token-123".to_string())
        );
        // Redacted should mask it
        assert_eq!(
            redacted.output.gelf.http.auth_token,
            Some("***REDACTED***".to_string())
        );
    }

    #[test]
    fn test_config_redacted_no_token() {
        let config = Config::default();
        let redacted = config.redacted();

        // If no token, should remain None
        assert_eq!(redacted.output.gelf.http.auth_token, None);
    }
}
