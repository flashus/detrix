//! ApiClient trait - Abstract interface for all API backends
//!
//! This trait defines all operations that can be performed via Detrix APIs.
//! Implementations exist for MCP, gRPC, and REST.
//!
//! Uses proto-generated types from detrix-api where compatible.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;

// Re-export proto types that are used directly
pub use detrix_api::GroupInfo;

// ============================================================================
// Response Types (API-agnostic)
// ============================================================================

/// Generic API response wrapper
#[derive(Debug, Clone)]
pub struct ApiResponse<T> {
    pub data: T,
    pub raw_response: Option<String>,
}

impl<T> ApiResponse<T> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            raw_response: None,
        }
    }

    pub fn with_raw(mut self, raw: String) -> Self {
        self.raw_response = Some(raw);
        self
    }
}

/// Connection info returned from API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub connection_id: String,
    pub host: String,
    pub port: u16,
    pub language: String,
    pub status: String,
}

/// Metric info returned from API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricInfo {
    pub name: String,
    pub location: String,
    pub expression: String,
    pub group: Option<String>,
    pub enabled: bool,
    pub mode: Option<String>,
}

/// Event info returned from API
/// Note: This must be compatible with MetricEventDisplay from the MCP server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventInfo {
    pub metric_name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub timestamp_iso: String,
    #[serde(default)]
    pub age_seconds: i64,
    #[serde(default)]
    pub is_error: bool,
    /// Captured stack trace (if introspection enabled)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stack_trace: Option<detrix_core::CapturedStackTrace>,
    /// Captured memory snapshot (if introspection enabled)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory_snapshot: Option<detrix_core::MemorySnapshot>,
    /// Additional fields from MetricEventDisplay (ignored for our purposes)
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Status info returned from API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusInfo {
    pub mode: String,
    pub uptime_seconds: u64,
    pub active_connections: usize,
    pub total_metrics: usize,
    pub enabled_metrics: usize,
}

/// Validation result returned from API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub is_safe: bool,
    pub issues: Vec<String>,
}

/// MCP usage statistics returned from API
/// Accepts both camelCase (REST) and snake_case (MCP) JSON keys
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpUsageInfo {
    #[serde(alias = "totalCalls")]
    pub total_calls: u64,
    #[serde(alias = "totalSuccess")]
    pub total_success: u64,
    #[serde(alias = "totalErrors")]
    pub total_errors: u64,
    #[serde(alias = "successRate")]
    pub success_rate: f64,
    #[serde(alias = "avgLatencyMs")]
    pub avg_latency_ms: f64,
    #[serde(alias = "topErrors")]
    pub top_errors: Vec<McpErrorCountInfo>,
    pub workflow: McpWorkflowInfo,
}

/// Error count for MCP usage stats
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpErrorCountInfo {
    pub code: String,
    pub count: u64,
}

/// Workflow statistics for MCP usage
/// Accepts both camelCase (REST) and snake_case (MCP) JSON keys
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpWorkflowInfo {
    #[serde(alias = "correctWorkflows")]
    pub correct_workflows: u64,
    #[serde(alias = "skippedInspect")]
    pub skipped_inspect: u64,
    #[serde(alias = "noConnection")]
    pub no_connection: u64,
    #[serde(alias = "adherenceRate")]
    pub adherence_rate: f64,
}

/// System event info returned from API
/// Matches SystemEventDisplay from MCP server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEventInfo {
    /// Event ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// Event type (metric_added, debugger_crash, connection_lost, etc.)
    pub event_type: String,
    /// Human-readable ISO 8601 timestamp
    #[serde(default)]
    pub timestamp_iso: String,
    /// Seconds ago this event occurred
    #[serde(default)]
    pub age_seconds: i64,
    /// Associated connection ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    /// Associated metric ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_id: Option<i64>,
    /// Metric name (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_name: Option<String>,
    /// Additional event details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Whether this event has been acknowledged
    #[serde(default)]
    pub acknowledged: bool,
    /// Original timestamp in microseconds
    #[serde(default)]
    pub timestamp_us: i64,
}

impl SystemEventInfo {
    /// Check if this is a connection-related event
    pub fn is_connection_event(&self) -> bool {
        matches!(
            self.event_type.as_str(),
            "connection_created" | "connection_closed" | "connection_lost" | "connection_restored"
        )
    }

    /// Check if this is a disconnection event (lost, closed, or debugger crash)
    pub fn is_disconnection_event(&self) -> bool {
        matches!(
            self.event_type.as_str(),
            "connection_closed" | "connection_lost" | "debugger_crash"
        )
    }

    /// Check if this is a metric-related event
    pub fn is_metric_event(&self) -> bool {
        matches!(
            self.event_type.as_str(),
            "metric_added" | "metric_removed" | "metric_toggled"
        )
    }

    /// Check if this is a crash event
    pub fn is_crash_event(&self) -> bool {
        self.event_type == "debugger_crash"
    }
}

// ============================================================================
// Add Metric Request
// ============================================================================

/// Request for adding a metric
///
/// NOTE: `connection_id` is REQUIRED to enforce explicit relationship:
/// metric → MCP client → daemon → DAP adapter
#[derive(Debug, Clone)]
pub struct AddMetricRequest {
    pub name: String,
    pub location: String,
    pub expression: String,
    /// Connection ID is REQUIRED - enforces explicit metric→connection relationship
    pub connection_id: String,
    /// Language for the metric (e.g., "python", "go", "rust"). Defaults to "python" if not specified.
    pub language: Option<String>,
    pub group: Option<String>,
    pub mode: Option<String>,
    /// Whether the metric is enabled. Defaults to true if not specified.
    pub enabled: Option<bool>,
    pub sample_rate: Option<u32>,
    pub sample_interval_seconds: Option<u32>,
    pub max_per_second: Option<u32>,
    pub capture_stack_trace: Option<bool>,
    pub stack_trace_ttl: Option<u64>,
    pub stack_trace_full: Option<bool>,
    pub stack_trace_head: Option<u32>,
    pub stack_trace_tail: Option<u32>,
    pub capture_memory_snapshot: Option<bool>,
    pub snapshot_scope: Option<String>,
    pub snapshot_ttl: Option<u64>,
}

impl AddMetricRequest {
    /// Create a new add metric request with required fields
    ///
    /// `connection_id` is required to enforce explicit relationship between
    /// metric and the DAP connection it belongs to.
    pub fn new(name: &str, location: &str, expression: &str, connection_id: &str) -> Self {
        Self {
            name: name.to_string(),
            location: location.to_string(),
            expression: expression.to_string(),
            connection_id: connection_id.to_string(),
            language: None,
            group: None,
            mode: None,
            enabled: None, // Default to true on server side
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            capture_stack_trace: None,
            stack_trace_ttl: None,
            stack_trace_full: None,
            stack_trace_head: None,
            stack_trace_tail: None,
            capture_memory_snapshot: None,
            snapshot_scope: None,
            snapshot_ttl: None,
        }
    }

    /// Set the language for the metric
    pub fn with_language(mut self, language: &str) -> Self {
        self.language = Some(language.to_string());
        self
    }

    pub fn with_group(mut self, group: &str) -> Self {
        self.group = Some(group.to_string());
        self
    }

    pub fn with_mode(mut self, mode: &str) -> Self {
        self.mode = Some(mode.to_string());
        self
    }

    /// Set whether the metric should be enabled initially
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = Some(enabled);
        self
    }

    /// Create a disabled metric (for testing group enable)
    pub fn disabled(mut self) -> Self {
        self.enabled = Some(false);
        self
    }

    /// Create a new add metric request with a group
    pub fn new_with_group(
        name: &str,
        location: &str,
        expression: &str,
        connection_id: &str,
        group: &str,
    ) -> Self {
        Self::new(name, location, expression, connection_id).with_group(group)
    }

    /// Enable stack trace capture
    pub fn with_stack_trace(mut self) -> Self {
        self.capture_stack_trace = Some(true);
        self
    }

    /// Enable stack trace capture with TTL (seconds)
    pub fn with_stack_trace_ttl(mut self, ttl_seconds: u64) -> Self {
        self.capture_stack_trace = Some(true);
        self.stack_trace_ttl = Some(ttl_seconds);
        self
    }

    /// Enable stack trace capture with full stack
    pub fn with_full_stack_trace(mut self) -> Self {
        self.capture_stack_trace = Some(true);
        self.stack_trace_full = Some(true);
        self
    }

    /// Enable stack trace capture with head/tail limits
    pub fn with_stack_trace_slice(mut self, head: u32, tail: u32) -> Self {
        self.capture_stack_trace = Some(true);
        self.stack_trace_head = Some(head);
        self.stack_trace_tail = Some(tail);
        self
    }

    /// Enable memory snapshot capture
    pub fn with_memory_snapshot(mut self) -> Self {
        self.capture_memory_snapshot = Some(true);
        self
    }

    /// Enable memory snapshot capture with TTL (seconds)
    pub fn with_memory_snapshot_ttl(mut self, ttl_seconds: u64) -> Self {
        self.capture_memory_snapshot = Some(true);
        self.snapshot_ttl = Some(ttl_seconds);
        self
    }

    /// Enable memory snapshot capture with scope
    pub fn with_memory_snapshot_scope(mut self, scope: &str) -> Self {
        self.capture_memory_snapshot = Some(true);
        self.snapshot_scope = Some(scope.to_string());
        self
    }

    /// Enable both stack trace and memory snapshot
    pub fn with_introspection(mut self) -> Self {
        self.capture_stack_trace = Some(true);
        self.capture_memory_snapshot = Some(true);
        self
    }
}

// ============================================================================
// Observe Request (MCP-only convenience tool)
// ============================================================================

/// Request for the observe tool (MCP-only)
///
/// The observe tool simplifies the metric addition workflow:
/// - Auto-finds line if not specified
/// - Auto-selects connection if only one exists
/// - Auto-generates metric name if not specified
#[derive(Debug, Clone)]
pub struct ObserveRequest {
    /// File path to observe (relative or absolute)
    pub file: String,
    /// Expression to capture (e.g., "user.balance", "len(items)")
    pub expression: String,
    /// Line number (optional - auto-finds if not specified)
    pub line: Option<u32>,
    /// Connection ID (optional - auto-selects if only one connection exists)
    pub connection_id: Option<String>,
    /// Metric name (optional - auto-generated if not specified)
    pub name: Option<String>,
    /// Variable to search for when finding line
    pub find_variable: Option<String>,
    /// Group name for the metric
    pub group: Option<String>,
    /// Whether to capture stack traces
    pub capture_stack_trace: Option<bool>,
    /// Whether to capture memory snapshots
    pub capture_memory_snapshot: Option<bool>,
    /// TTL in seconds for introspection data
    pub ttl_seconds: Option<i64>,
}

impl ObserveRequest {
    /// Create a new observe request with required fields
    pub fn new(file: &str, expression: &str) -> Self {
        Self {
            file: file.to_string(),
            expression: expression.to_string(),
            line: None,
            connection_id: None,
            name: None,
            find_variable: None,
            group: None,
            capture_stack_trace: None,
            capture_memory_snapshot: None,
            ttl_seconds: None,
        }
    }

    pub fn with_line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }

    pub fn with_connection_id(mut self, connection_id: &str) -> Self {
        self.connection_id = Some(connection_id.to_string());
        self
    }

    pub fn with_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    pub fn with_find_variable(mut self, var: &str) -> Self {
        self.find_variable = Some(var.to_string());
        self
    }

    pub fn with_group(mut self, group: &str) -> Self {
        self.group = Some(group.to_string());
        self
    }

    pub fn with_stack_trace(mut self) -> Self {
        self.capture_stack_trace = Some(true);
        self
    }

    pub fn with_memory_snapshot(mut self) -> Self {
        self.capture_memory_snapshot = Some(true);
        self
    }

    pub fn with_introspection(mut self) -> Self {
        self.capture_stack_trace = Some(true);
        self.capture_memory_snapshot = Some(true);
        self
    }

    pub fn with_ttl(mut self, seconds: i64) -> Self {
        self.ttl_seconds = Some(seconds);
        self
    }
}

/// Response from observe tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveResponse {
    /// Whether the observe was successful
    pub success: bool,
    /// The created metric info
    pub metric_id: i64,
    /// Name of the created metric
    pub metric_name: String,
    /// File path
    pub file: String,
    /// Line number used
    pub line: u32,
    /// Expression being observed
    pub expression: String,
    /// How the line was determined
    pub line_source: String,
    /// Connection ID used
    pub connection_id: String,
    /// Alternative line locations found
    pub alternatives: Vec<(u32, String)>,
    /// Any warnings
    pub warnings: Vec<String>,
}

// ============================================================================
// Enable From Diff Request (MCP-only)
// ============================================================================

/// Request for the enable_from_diff tool (MCP-only)
///
/// Parses git diffs containing print/log statements and automatically creates
/// Detrix metrics for them.
#[derive(Debug, Clone)]
pub struct EnableFromDiffRequest {
    /// Git diff content in unified format
    pub diff: String,
    /// Connection ID (optional - auto-selects if only one connection exists)
    pub connection_id: Option<String>,
    /// Group name for created metrics
    pub group: Option<String>,
    /// TTL in seconds for auto-cleanup
    pub ttl_seconds: Option<i64>,
}

impl EnableFromDiffRequest {
    /// Create a new request with the diff content
    pub fn new(diff: &str) -> Self {
        Self {
            diff: diff.to_string(),
            connection_id: None,
            group: None,
            ttl_seconds: None,
        }
    }

    pub fn with_connection_id(mut self, id: &str) -> Self {
        self.connection_id = Some(id.to_string());
        self
    }

    pub fn with_group(mut self, group: &str) -> Self {
        self.group = Some(group.to_string());
        self
    }

    pub fn with_ttl(mut self, seconds: i64) -> Self {
        self.ttl_seconds = Some(seconds);
        self
    }
}

/// Info about a successfully added metric from diff
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffMetricAdded {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub id: i64,
}

/// Info about a failed metric from diff
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffMetricFailed {
    pub file: String,
    pub line: u32,
    pub statement: String,
    pub error: String,
}

/// Info about a metric that needs manual review
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffMetricNeedsReview {
    pub file: String,
    pub line: u32,
    pub statement: String,
    pub suggested_expression: String,
}

/// Response from enable_from_diff tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnableFromDiffResponse {
    /// Whether any metrics were added
    pub success: bool,
    /// Metrics that were successfully added
    pub added: Vec<DiffMetricAdded>,
    /// Metrics that failed to add
    pub failed: Vec<DiffMetricFailed>,
    /// Metrics that need manual review
    pub needs_review: Vec<DiffMetricNeedsReview>,
    /// Connection ID used
    pub connection_id: String,
}

// ============================================================================
// Error Type
// ============================================================================

/// API error type
#[derive(Debug, Clone)]
pub struct ApiError {
    pub message: String,
    pub code: Option<String>,
    pub raw_response: Option<String>,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            raw_response: None,
        }
    }

    pub fn with_raw(mut self, raw: String) -> Self {
        self.raw_response = Some(raw);
        self
    }
}

pub type ApiResult<T> = Result<ApiResponse<T>, ApiError>;

// ============================================================================
// ApiClient Trait
// ============================================================================

/// Abstract interface for all Detrix API backends (MCP, gRPC, REST)
///
/// This trait enables writing test scenarios once and running them
/// against all API types.
#[async_trait]
pub trait ApiClient: Send + Sync {
    /// Get the API type name (for logging)
    fn api_type(&self) -> &'static str;

    // ========================================================================
    // Health & Status
    // ========================================================================

    /// Check if the API is healthy
    async fn health(&self) -> Result<bool, ApiError>;

    /// Get system status
    async fn get_status(&self) -> ApiResult<StatusInfo>;

    /// Wake Detrix (informational - shows current status)
    async fn wake(&self) -> ApiResult<String>;

    /// Sleep Detrix (stop all connections)
    async fn sleep(&self) -> ApiResult<String>;

    // ========================================================================
    // Connection Management
    // ========================================================================

    /// Create a new debugger connection
    ///
    /// For Rust with direct lldb-dap, pass the program path to launch.
    async fn create_connection(
        &self,
        host: &str,
        port: u16,
        language: &str,
    ) -> ApiResult<ConnectionInfo> {
        self.create_connection_with_program(host, port, language, None)
            .await
    }

    /// Create a new debugger connection with optional program path
    ///
    /// The program parameter is used for Rust direct lldb-dap launch mode.
    async fn create_connection_with_program(
        &self,
        host: &str,
        port: u16,
        language: &str,
        program: Option<&str>,
    ) -> ApiResult<ConnectionInfo>;

    /// Close a connection
    async fn close_connection(&self, connection_id: &str) -> ApiResult<()>;

    /// Get connection details
    async fn get_connection(&self, connection_id: &str) -> ApiResult<ConnectionInfo>;

    /// List all connections
    async fn list_connections(&self) -> ApiResult<Vec<ConnectionInfo>>;

    // ========================================================================
    // Metric Management
    // ========================================================================

    /// Add a new metric
    async fn add_metric(&self, request: AddMetricRequest) -> ApiResult<String>;

    /// Observe a value in code (MCP-only convenience tool)
    ///
    /// The observe tool simplifies the metric addition workflow:
    /// - Auto-finds line if not specified
    /// - Auto-selects connection if only one exists
    /// - Auto-generates metric name if not specified
    ///
    /// Default implementation returns "not supported" for non-MCP backends.
    async fn observe(&self, _request: ObserveRequest) -> ApiResult<ObserveResponse> {
        Err(ApiError::new(format!(
            "observe tool is only supported on MCP backend, not {}",
            self.api_type()
        )))
    }

    /// Enable metrics from git diff (MCP-only convenience tool)
    ///
    /// Parses git diffs containing print/log statements and automatically
    /// creates Detrix metrics for them. Uses "smart" approach:
    /// - Auto-adds simple expressions (identifiers, attribute access)
    /// - Reports complex expressions for manual review
    ///
    /// Default implementation returns "not supported" for non-MCP backends.
    async fn enable_from_diff(
        &self,
        _request: EnableFromDiffRequest,
    ) -> ApiResult<EnableFromDiffResponse> {
        Err(ApiError::new(format!(
            "enable_from_diff tool is only supported on MCP backend, not {}",
            self.api_type()
        )))
    }

    /// Remove a metric
    async fn remove_metric(&self, name: &str) -> ApiResult<()>;

    /// Toggle metric enabled state
    async fn toggle_metric(&self, name: &str, enabled: bool) -> ApiResult<()>;

    /// Get metric by name
    async fn get_metric(&self, name: &str) -> ApiResult<MetricInfo>;

    /// List all metrics
    async fn list_metrics(&self) -> ApiResult<Vec<MetricInfo>>;

    /// Query metric events
    async fn query_events(&self, metric_name: &str, limit: u32) -> ApiResult<Vec<EventInfo>>;

    // ========================================================================
    // Group Management
    // ========================================================================

    /// Enable all metrics in a group
    async fn enable_group(&self, group: &str) -> ApiResult<usize>;

    /// Disable all metrics in a group
    async fn disable_group(&self, group: &str) -> ApiResult<usize>;

    /// List all groups
    async fn list_groups(&self) -> ApiResult<Vec<GroupInfo>>;

    // ========================================================================
    // Validation
    // ========================================================================

    /// Validate an expression for a given language
    ///
    /// # Arguments
    /// * `expression` - The expression to validate
    /// * `language` - The language for validation (e.g., "python", "go")
    async fn validate_expression(
        &self,
        expression: &str,
        language: &str,
    ) -> ApiResult<ValidationResult>;

    /// Inspect a file (for finding metric placement)
    async fn inspect_file(
        &self,
        file_path: &str,
        line: Option<u32>,
        find_variable: Option<&str>,
    ) -> ApiResult<String>;

    // ========================================================================
    // Configuration
    // ========================================================================

    /// Get current configuration
    async fn get_config(&self) -> ApiResult<String>;

    /// Update configuration
    async fn update_config(&self, config_toml: &str, persist: bool) -> ApiResult<()>;

    /// Validate configuration
    async fn validate_config(&self, config_toml: &str) -> ApiResult<bool>;

    /// Reload configuration from file
    async fn reload_config(&self) -> ApiResult<()>;

    // ========================================================================
    // System Events (MCP catch-up)
    // ========================================================================

    /// Query system events (metric CRUD, connection events, crashes)
    ///
    /// # Arguments
    /// * `limit` - Maximum number of events to return
    /// * `unacknowledged_only` - Only return unacknowledged events
    /// * `event_types` - Optional list of event types to filter
    async fn query_system_events(
        &self,
        limit: u32,
        unacknowledged_only: bool,
        event_types: Option<Vec<&str>>,
    ) -> ApiResult<Vec<SystemEventInfo>>;

    /// Acknowledge system events by IDs
    async fn acknowledge_system_events(&self, event_ids: Vec<i64>) -> ApiResult<u64>;

    // ========================================================================
    // MCP Usage Statistics
    // ========================================================================

    /// Get MCP tool usage statistics
    ///
    /// Returns statistics about how MCP tools are being used, including
    /// success rates, common errors, and workflow adherence.
    async fn get_mcp_usage(&self) -> ApiResult<McpUsageInfo>;
}
