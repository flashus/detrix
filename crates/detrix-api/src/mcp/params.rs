//! MCP Parameter Types
//!
//! This module contains all parameter structs for MCP tools.
//! Extracted from server.rs for better maintainability.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Metric Parameter Types
// ============================================================================

/// Default to true for enabled field
fn default_enabled() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for adding a new metric")]
pub struct AddMetricParams {
    #[schemars(description = "Unique metric name (letters, numbers, underscore, dash)")]
    pub name: String,
    #[schemars(
        description = "File path with optional line number. Formats: 'file.py#123', '@file.py#123', '\"expr\"@file.py#123', or 'file.py' with separate line parameter"
    )]
    pub location: String,
    #[schemars(
        description = "Line number (optional if included in location). Takes precedence over line in location string."
    )]
    pub line: Option<u32>,
    #[schemars(
        description = "Expression to evaluate (e.g., user.id, {'a': x, 'b': y}). For Go: simple variables recommended; non-variadic function calls work but BLOCK process (variadic like fmt.Sprintf NOT supported)."
    )]
    pub expression: String,
    #[schemars(
        description = "Connection ID to use for this metric. Required. Get from create_connection response or list_connections."
    )]
    pub connection_id: String,
    #[schemars(description = "Optional group name for organization")]
    pub group: Option<String>,
    #[schemars(description = "Whether the metric is enabled (default: true)")]
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[schemars(
        description = "Capture mode: stream, sample, sample_interval, first, or throttle (default: stream)"
    )]
    pub mode: Option<String>,
    #[schemars(description = "For sample mode: capture every Nth hit")]
    pub sample_rate: Option<u32>,
    #[schemars(description = "For sample_interval mode: capture every N seconds")]
    pub sample_interval_seconds: Option<u32>,
    #[schemars(description = "For throttle mode: max events per second")]
    pub max_per_second: Option<u32>,

    // Stack trace capture options
    #[schemars(description = "Enable stack trace capture (default: false)")]
    pub capture_stack_trace: Option<bool>,
    #[schemars(
        description = "Stack trace TTL in seconds (e.g., 10 for 10 seconds, null for continuous)"
    )]
    pub stack_trace_ttl: Option<u64>,
    #[schemars(description = "Capture full stack trace (default: false, captures head only)")]
    pub stack_trace_full: Option<bool>,
    #[schemars(description = "Number of frames to capture from the top of the stack (default: 5)")]
    pub stack_trace_head: Option<u32>,
    #[schemars(description = "Number of frames to capture from the bottom of the stack")]
    pub stack_trace_tail: Option<u32>,

    // Memory snapshot capture options
    #[schemars(description = "Enable memory snapshot capture (default: false)")]
    pub capture_memory_snapshot: Option<bool>,
    #[schemars(
        description = "Memory snapshot scope: 'local', 'global', or 'both' (default: 'local')"
    )]
    pub snapshot_scope: Option<String>,
    #[schemars(
        description = "Memory snapshot TTL in seconds (e.g., 10 for 10 seconds, null for continuous)"
    )]
    pub snapshot_ttl: Option<u64>,

    // Replacement behavior
    #[schemars(
        description = "If true, replace any existing metric at the same location (file#line). If false (default), return error when metric already exists at this location. DAP only supports one logpoint per line."
    )]
    pub replace: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for removing a metric")]
pub struct RemoveMetricParams {
    #[schemars(description = "Name of the metric to remove")]
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for toggling a metric")]
pub struct ToggleMetricParams {
    #[schemars(description = "Name of the metric")]
    pub name: String,
    #[schemars(description = "true to enable, false to disable")]
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for listing metrics")]
pub struct ListMetricsParams {
    #[schemars(description = "Filter by group name")]
    pub group: Option<String>,
    #[schemars(description = "Only show enabled metrics")]
    pub enabled_only: Option<bool>,
    #[schemars(
        description = "Output format: 'toon' (default, LLM-optimized) or 'json' (human-readable)"
    )]
    pub format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for querying metric events")]
pub struct QueryMetricsParams {
    #[schemars(description = "Metric name to query")]
    pub name: Option<String>,
    #[schemars(description = "Query all metrics in group")]
    pub group: Option<String>,
    #[schemars(description = "Max results (default: 100, max: 1000)")]
    pub limit: Option<u64>,
    #[schemars(
        description = "Output format: 'toon' (default, LLM-optimized) or 'json' (human-readable)"
    )]
    pub format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for getting a metric by name")]
pub struct GetMetricParams {
    #[schemars(description = "Name of the metric to retrieve")]
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for updating an existing metric")]
pub struct UpdateMetricParams {
    #[schemars(description = "Name of the metric to update")]
    pub name: String,
    #[schemars(description = "New expression to evaluate (optional)")]
    pub expression: Option<String>,
    #[schemars(description = "New group for the metric (optional)")]
    pub group: Option<String>,
    #[schemars(description = "Enable or disable the metric (optional)")]
    pub enabled: Option<bool>,
}

// ============================================================================
// Observe Tool Parameter Types (Simplified Workflow)
// ============================================================================

/// Parameters for the simplified observe workflow.
///
/// The `observe` tool combines connection selection, file inspection, and metric creation
/// into a single call. It's the recommended way to add metrics for most use cases.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(
    description = "Observe a value in code. Simplest way to add metrics - auto-finds line if not specified."
)]
pub struct ObserveParams {
    #[schemars(description = "File path to observe (relative or absolute)")]
    pub file: String,

    #[schemars(
        description = "Expression to capture (e.g., 'user.balance', 'len(items)'). For Go: simple variables recommended; non-variadic function calls work but BLOCK process (variadic like fmt.Sprintf NOT supported)."
    )]
    pub expression: String,

    #[schemars(description = "Line number (optional - auto-finds best line if not specified)")]
    pub line: Option<u32>,

    #[schemars(
        description = "Connection ID (optional - auto-selects if only one connection exists)"
    )]
    pub connection_id: Option<String>,

    #[schemars(
        description = "Metric name (optional - auto-generated from expression if not provided)"
    )]
    pub name: Option<String>,

    #[schemars(
        description = "Variable to search for when auto-finding line (e.g., 'user', 'response')"
    )]
    pub find_variable: Option<String>,

    #[schemars(description = "Group name for organizing metrics")]
    pub group: Option<String>,

    #[schemars(description = "Enable stack trace capture")]
    pub capture_stack_trace: Option<bool>,

    #[schemars(description = "Enable memory snapshot capture")]
    pub capture_memory_snapshot: Option<bool>,

    #[schemars(
        description = "TTL in seconds - metric auto-disables after this time (for temporary debugging)"
    )]
    pub ttl_seconds: Option<u64>,
}

// ============================================================================
// Enable From Diff Parameter Types
// ============================================================================

/// Parameters for enabling metrics from a git diff.
///
/// Parses git diffs containing print/log statements and automatically creates
/// Detrix metrics for them. Uses a "smart" approach: auto-adds what it can
/// parse confidently, reports failures with suggestions for manual `observe` calls.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(
    description = "Enable metrics from git diff. Parses print/log statements and creates observation points automatically."
)]
pub struct EnableFromDiffParams {
    #[schemars(
        description = "Git diff content in unified format (output of `git diff`). Contains added print/log statements to convert to metrics."
    )]
    pub diff: String,

    #[schemars(
        description = "Connection ID (optional - auto-selects if only one connection exists)"
    )]
    pub connection_id: Option<String>,

    #[schemars(description = "Group name for organizing created metrics")]
    pub group: Option<String>,

    #[schemars(
        description = "TTL in seconds - metrics auto-disable after this time (for temporary debugging)"
    )]
    pub ttl_seconds: Option<u64>,
}

// ============================================================================
// Connection Management Parameter Types
// ============================================================================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for creating a debugger connection")]
pub struct CreateConnectionParams {
    #[schemars(description = "Host address (e.g., '127.0.0.1', 'localhost')")]
    pub host: String,
    #[schemars(description = "Port number where the debugger is listening")]
    pub port: u32,
    #[schemars(
        description = "Language/adapter type. Required. Supported values: 'python', 'go', 'rust'"
    )]
    pub language: String,
    #[schemars(description = "Optional custom connection ID")]
    pub connection_id: Option<String>,
    #[schemars(
        description = "Program path to launch (for Rust/lldb-dap only). REQUIRED for Rust: First start lldb-dap with `lldb-dap --connection listen://HOST:PORT`, then specify the debug binary path here. Binary must have debug symbols (cargo build, NOT --release)."
    )]
    pub program: Option<String>,
    #[schemars(
        description = "Process ID to attach to (for Rust/lldb-dap only). When specified, lldb-dap will attach to this running process."
    )]
    pub pid: Option<u32>,
    #[serde(default)]
    #[schemars(
        description = "SafeMode: Only allow logpoints (non-blocking), disable breakpoint-based operations like function calls, stack traces, memory snapshots. Recommended for production environments."
    )]
    pub safe_mode: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for closing a debugger connection")]
pub struct CloseConnectionParams {
    #[schemars(description = "Connection ID to close")]
    pub connection_id: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for getting a specific connection")]
pub struct GetConnectionParams {
    #[schemars(description = "Connection ID to retrieve")]
    pub connection_id: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for listing connections")]
pub struct ListConnectionsParams {
    #[schemars(description = "Only show active (connected) connections")]
    pub active_only: Option<bool>,
    #[schemars(
        description = "Output format: 'toon' (default, LLM-optimized) or 'json' (human-readable)"
    )]
    pub format: Option<String>,
}

// ============================================================================
// Validation and Inspection Parameter Types
// ============================================================================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for validating an expression")]
pub struct ValidateExpressionParams {
    /// Expression to validate for safety
    #[schemars(description = "Expression to validate for safety")]
    pub expression: String,
    /// Language (required): 'python', 'go', or 'rust'
    #[schemars(description = "Language: 'python', 'go', or 'rust'")]
    pub language: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(
    description = "Parameters for inspecting a file to find correct metric placement. Use BEFORE add_metric to find the right line for your expression."
)]
pub struct InspectFileParams {
    #[schemars(description = "Path to the Python file to inspect")]
    pub file_path: String,
    #[schemars(
        description = "Show variables available at this line number (use to verify metric placement)"
    )]
    pub line: Option<u32>,
    #[schemars(
        description = "Find where this variable is defined/assigned (e.g., 'pnl', 'user', 'response')"
    )]
    pub find_variable: Option<String>,
}

// ============================================================================
// Group Parameter Types
// ============================================================================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for group operations")]
pub struct GroupParams {
    #[schemars(description = "Name of the group")]
    pub group: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for listing groups")]
pub struct ListGroupsParams {
    #[schemars(
        description = "Output format: 'toon' (default, LLM-optimized) or 'json' (human-readable)"
    )]
    pub format: Option<String>,
}

// ============================================================================
// Config Parameter Types
// ============================================================================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for getting configuration")]
pub struct GetConfigParams {
    #[schemars(description = "Output format: 'toml' (default) or 'json'")]
    pub format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for updating configuration")]
pub struct UpdateConfigParams {
    #[schemars(description = "Partial TOML configuration to merge with current config")]
    pub config_toml: String,
    #[schemars(description = "Whether to persist changes to config file (default: true)")]
    pub persist: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for validating configuration")]
pub struct ValidateConfigParams {
    #[schemars(description = "TOML configuration to validate")]
    pub config_toml: String,
}

// ============================================================================
// System Event Parameter Types
// ============================================================================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(
    description = "Parameters for querying system events (crashes, connections, metric changes)"
)]
pub struct QuerySystemEventsParams {
    #[schemars(
        description = "Return events after this timestamp (microseconds since epoch). Use to get only new events since last query."
    )]
    pub since_timestamp: Option<i64>,
    #[schemars(
        description = "Filter by event types. Options: metric_added, metric_removed, metric_toggled, connection_created, connection_closed, connection_lost, connection_restored, debugger_crash"
    )]
    pub event_types: Option<Vec<String>>,
    #[schemars(description = "Filter by connection ID")]
    pub connection_id: Option<String>,
    #[schemars(description = "Only show unacknowledged events")]
    pub unacknowledged_only: Option<bool>,
    #[schemars(description = "Max results (default: 100, max: 1000)")]
    pub limit: Option<u64>,
    #[schemars(
        description = "Output format: 'toon' (default, LLM-optimized) or 'json' (human-readable)"
    )]
    pub format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Parameters for acknowledging system events")]
pub struct AcknowledgeEventsParams {
    #[schemars(description = "Event IDs to acknowledge")]
    pub event_ids: Vec<i64>,
}
