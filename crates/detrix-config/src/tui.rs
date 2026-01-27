//! TUI (Terminal User Interface) configuration

use crate::constants::{
    DEFAULT_TUI_DATA_REFRESH_CONNECTED_MS, DEFAULT_TUI_DATA_REFRESH_DISCONNECTED_MS,
    DEFAULT_TUI_EVENT_BUFFER_SIZE, DEFAULT_TUI_GRPC_TIMEOUT_MS, DEFAULT_TUI_IDLE_TIMEOUT_MS,
    DEFAULT_TUI_REFRESH_RATE_ACTIVE_MS, DEFAULT_TUI_REFRESH_RATE_IDLE_MS,
    DEFAULT_TUI_SYSTEM_EVENT_CAPACITY,
};
use serde::{Deserialize, Serialize};

// ============================================================================
// TUI Config
// ============================================================================

/// TUI (Terminal User Interface) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiConfig {
    /// Maximum events to keep in the rolling buffer
    #[serde(default = "default_event_buffer_size")]
    pub event_buffer_size: usize,

    /// Maximum system events to keep in the rolling buffer
    /// Default: 1000
    #[serde(default = "default_system_event_capacity")]
    pub system_event_capacity: usize,

    /// Refresh rate in milliseconds when user is actively interacting (60 FPS)
    #[serde(default = "default_refresh_rate_active_ms")]
    pub refresh_rate_active_ms: u64,

    /// Refresh rate in milliseconds when idle (4 FPS - saves CPU)
    #[serde(default = "default_refresh_rate_idle_ms")]
    pub refresh_rate_idle_ms: u64,

    /// Time in milliseconds before switching to idle refresh rate
    #[serde(default = "default_idle_timeout_ms")]
    pub idle_timeout_ms: u64,

    /// Data refresh interval when connected (milliseconds)
    /// How often to fetch data from the daemon when connected
    /// Default: 2000ms (2 seconds)
    #[serde(default = "default_data_refresh_connected_ms")]
    pub data_refresh_connected_ms: u64,

    /// Data refresh interval when disconnected (milliseconds)
    /// How often to retry connecting when disconnected
    /// Default: 5000ms (5 seconds)
    #[serde(default = "default_data_refresh_disconnected_ms")]
    pub data_refresh_disconnected_ms: u64,

    /// gRPC request timeout (milliseconds)
    /// Short timeout for responsiveness
    /// Default: 500ms
    #[serde(default = "default_grpc_timeout_ms")]
    pub grpc_timeout_ms: u64,

    /// Theme configuration
    #[serde(default)]
    pub theme: ThemeConfig,
}

fn default_event_buffer_size() -> usize {
    DEFAULT_TUI_EVENT_BUFFER_SIZE
}

fn default_system_event_capacity() -> usize {
    DEFAULT_TUI_SYSTEM_EVENT_CAPACITY
}

fn default_refresh_rate_active_ms() -> u64 {
    DEFAULT_TUI_REFRESH_RATE_ACTIVE_MS // ~60 FPS
}

fn default_refresh_rate_idle_ms() -> u64 {
    DEFAULT_TUI_REFRESH_RATE_IDLE_MS // 4 FPS
}

fn default_idle_timeout_ms() -> u64 {
    DEFAULT_TUI_IDLE_TIMEOUT_MS // Go idle after 500ms of no input
}

fn default_data_refresh_connected_ms() -> u64 {
    DEFAULT_TUI_DATA_REFRESH_CONNECTED_MS
}

fn default_data_refresh_disconnected_ms() -> u64 {
    DEFAULT_TUI_DATA_REFRESH_DISCONNECTED_MS
}

fn default_grpc_timeout_ms() -> u64 {
    DEFAULT_TUI_GRPC_TIMEOUT_MS
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            event_buffer_size: default_event_buffer_size(),
            system_event_capacity: default_system_event_capacity(),
            refresh_rate_active_ms: default_refresh_rate_active_ms(),
            refresh_rate_idle_ms: default_refresh_rate_idle_ms(),
            idle_timeout_ms: default_idle_timeout_ms(),
            data_refresh_connected_ms: default_data_refresh_connected_ms(),
            data_refresh_disconnected_ms: default_data_refresh_disconnected_ms(),
            grpc_timeout_ms: default_grpc_timeout_ms(),
            theme: ThemeConfig::default(),
        }
    }
}

// ============================================================================
// Theme Config
// ============================================================================

/// Theme configuration for TUI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    /// Theme name: "dark", "light", or path to custom theme file
    #[serde(default = "default_theme_name")]
    pub name: String,

    /// Optional color overrides (applied on top of base theme)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<ThemeColors>,
}

fn default_theme_name() -> String {
    "dark".to_string()
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: default_theme_name(),
            colors: None,
        }
    }
}

/// Custom color overrides for theming
///
/// Colors are specified as hex strings (e.g., "#1a1b26") or named colors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThemeColors {
    /// Background color
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,

    /// Default foreground/text color
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,

    /// Selected/highlighted item background
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,

    /// Header/title color
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,

    /// Border color
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,

    /// Connected status indicator
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected: Option<String>,

    /// Disconnected status indicator
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disconnected: Option<String>,

    /// Error text/indicator
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Warning text/indicator
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,

    /// Metric name text
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_name: Option<String>,

    /// Value text
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,

    /// Timestamp text
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}
