//! Repository port trait for MCP usage persistence
//!
//! Enables historical analysis of MCP tool usage patterns
//! for data-driven prompt optimization.

use async_trait::async_trait;
use detrix_core::{error::Result, McpUsageEvent};

/// Aggregated usage statistics
#[derive(Debug, Clone, Default)]
pub struct UsageStats {
    /// Total tool calls in the time range
    pub total_calls: u64,
    /// Total successful calls
    pub total_success: u64,
    /// Total error calls
    pub total_errors: u64,
    /// Average latency in milliseconds
    pub avg_latency_ms: f64,
}

/// Error count by error code
#[derive(Debug, Clone)]
pub struct ErrorCountRow {
    /// Error code string
    pub error_code: String,
    /// Number of occurrences
    pub count: u64,
}

/// Tool call count
#[derive(Debug, Clone)]
pub struct ToolCountRow {
    /// Tool name
    pub tool_name: String,
    /// Number of calls
    pub count: u64,
    /// Success count
    pub success_count: u64,
    /// Error count
    pub error_count: u64,
}

/// Repository for MCP usage events
///
/// Stores usage events for historical analysis and prompt optimization.
/// Events are persisted with tool name, status, error code, and timing.
#[async_trait]
pub trait McpUsageRepository: Send + Sync {
    /// Save a usage event
    ///
    /// Returns the event ID on success.
    async fn save_event(&self, event: &McpUsageEvent) -> Result<i64>;

    /// Save multiple events in batch
    ///
    /// Returns the IDs of saved events.
    async fn save_batch(&self, events: &[McpUsageEvent]) -> Result<Vec<i64>>;

    /// Get aggregated stats for a time range
    ///
    /// # Arguments
    /// * `since_micros` - Start timestamp (inclusive), None for all time
    /// * `until_micros` - End timestamp (exclusive), None for now
    async fn get_stats(
        &self,
        since_micros: Option<i64>,
        until_micros: Option<i64>,
    ) -> Result<UsageStats>;

    /// Get error breakdown by code
    ///
    /// Returns error codes sorted by count (descending).
    async fn get_error_breakdown(&self, since_micros: Option<i64>) -> Result<Vec<ErrorCountRow>>;

    /// Get tool call counts
    ///
    /// Returns tool names with call counts, sorted by total calls (descending).
    async fn get_tool_counts(&self, since_micros: Option<i64>) -> Result<Vec<ToolCountRow>>;

    /// Delete old events (cleanup)
    ///
    /// Returns the number of deleted events.
    async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64>;

    /// Count all events
    async fn count_all(&self) -> Result<i64>;
}
