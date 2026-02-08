//! DTO types for gRPC client responses

use serde::{Deserialize, Serialize};

/// Simplified metric info for CLI display
#[derive(Debug, Serialize)]
pub struct MetricInfo {
    /// Metric ID (used when querying via gRPC)
    pub metric_id: u64,
    pub name: String,
    pub group: Option<String>,
    pub location_file: String,
    pub location_line: u32,
    pub expressions: Vec<String>,
    pub language: String,
    pub enabled: bool,
    pub mode: String,
    pub hit_count: u64,
    /// Last hit timestamp (populated via gRPC)
    pub last_hit_at: Option<i64>,
}

impl MetricInfo {
    /// Convenience: get the first expression (for display)
    #[allow(dead_code)]
    pub fn expression(&self) -> &str {
        self.expressions.first().map(|s| s.as_str()).unwrap_or("")
    }
}

/// Simplified event info for CLI display
#[derive(Debug, Serialize)]
pub struct EventInfo {
    pub metric_id: u64,
    pub metric_name: String,
    pub timestamp: i64,
    pub values_json: Vec<String>,
    pub thread_name: Option<String>,
    pub thread_id: Option<i64>,
}

impl EventInfo {
    /// Convenience: get the first value's JSON string (for display)
    pub fn value_json(&self) -> Option<&str> {
        self.values_json.first().map(|s| s.as_str())
    }
}

/// Simplified connection info for CLI display
#[derive(Debug, Serialize)]
pub struct ConnectionInfo {
    pub connection_id: String,
    pub host: String,
    pub port: u32,
    pub language: String,
    pub status: String,
    pub created_at: i64,
    pub connected_at: Option<i64>,
    pub last_active_at: Option<i64>,
}

/// Simplified group info for CLI display
#[derive(Debug, Serialize)]
pub struct GroupInfo {
    pub name: String,
    pub metric_count: u32,
    pub enabled_count: u32,
}

/// System status info for CLI display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusInfo {
    pub mode: String,
    pub uptime_seconds: u64,
    /// Human-readable uptime (e.g., "1d 02:30:45" or "02:30:45")
    #[serde(default)]
    pub uptime_formatted: String,
    /// Started timestamp (ISO 8601)
    #[serde(default)]
    pub started_at: String,
    pub active_connections: u32,
    pub total_metrics: u32,
    pub enabled_metrics: u32,
    pub total_events: u64,
    /// Daemon process info
    #[serde(default)]
    pub daemon: Option<DaemonInfoDto>,
    /// Connected MCP clients
    #[serde(default)]
    pub mcp_clients: Vec<McpClientDto>,
    /// Path to loaded configuration file
    #[serde(default)]
    pub config_path: String,
}

/// Daemon process info DTO
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DaemonInfoDto {
    pub pid: u32,
    pub ppid: u32,
}

/// Parent process info DTO
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParentProcessDto {
    pub pid: u32,
    pub name: String,
    pub bridge_pid: u32,
}

/// MCP client info DTO
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpClientDto {
    pub id: String,
    pub connected_at: String,
    pub connected_duration_secs: u64,
    pub last_activity: String,
    pub last_activity_ago_secs: u64,
    #[serde(default)]
    pub parent_process: Option<ParentProcessDto>,
}

/// Wake response info
#[derive(Debug, Serialize)]
pub struct WakeInfo {
    pub status: String,
    pub metrics_loaded: u32,
}

/// Config validation result
#[derive(Debug, Serialize)]
pub struct ConfigValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Expression validation result
#[derive(Debug, Serialize)]
pub struct ExpressionValidationResult {
    pub is_safe: bool,
    pub expression: String,
    pub language: String,
    pub violations: Vec<String>,
    pub warnings: Vec<String>,
}

/// MCP usage statistics
#[derive(Debug, Serialize)]
pub struct McpUsageInfo {
    pub total_calls: u64,
    pub total_success: u64,
    pub total_errors: u64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
    pub top_errors: Vec<McpErrorCount>,
    pub workflow: McpWorkflowStats,
}

/// MCP error count
#[derive(Debug, Serialize)]
pub struct McpErrorCount {
    pub code: String,
    pub count: u64,
}

/// MCP workflow statistics
#[derive(Debug, Default, Serialize)]
pub struct McpWorkflowStats {
    pub correct_workflows: u64,
    pub skipped_inspect: u64,
    pub no_connection: u64,
    pub adherence_rate: f64,
}

/// File inspection result
#[derive(Debug, Serialize)]
pub struct InspectFileInfo {
    pub success: bool,
    pub content: String,
}

/// Parameters for adding a metric
pub struct AddMetricParams {
    pub name: String,
    pub location: String,
    pub line: Option<u32>,
    pub expressions: Vec<String>,
    pub connection_id: String,
    pub group: Option<String>,
    pub enabled: bool,
    pub replace: bool,
}
