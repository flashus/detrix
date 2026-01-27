//! MCP usage tracking entities
//!
//! Domain types for tracking MCP tool invocations and error patterns.
//! Used for prompt optimization and workflow analysis.

use serde::{Deserialize, Serialize};

/// Error codes for tracking misuse patterns
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpErrorCode {
    // Location format errors
    LocationMissingAt,
    LocationMissingLine,
    LocationInvalidLine,

    // Connection flow errors
    MetricBeforeConnection,
    ConnectionNotFound,
    ConnectionDisconnected,

    // Query errors
    QueryMissingRequiredParam,

    // Safety errors
    UnsafeExpression,

    // Unknown/other errors
    Other,
}

impl McpErrorCode {
    /// Convert error code to string for persistence
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LocationMissingAt => "location_missing_at",
            Self::LocationMissingLine => "location_missing_line",
            Self::LocationInvalidLine => "location_invalid_line",
            Self::MetricBeforeConnection => "metric_before_connection",
            Self::ConnectionNotFound => "connection_not_found",
            Self::ConnectionDisconnected => "connection_disconnected",
            Self::QueryMissingRequiredParam => "query_missing_required_param",
            Self::UnsafeExpression => "unsafe_expression",
            Self::Other => "other",
        }
    }

    /// Parse error code from string
    pub fn parse(s: &str) -> Self {
        match s {
            "location_missing_at" => Self::LocationMissingAt,
            "location_missing_line" => Self::LocationMissingLine,
            "location_invalid_line" => Self::LocationInvalidLine,
            "metric_before_connection" => Self::MetricBeforeConnection,
            "connection_not_found" => Self::ConnectionNotFound,
            "connection_disconnected" => Self::ConnectionDisconnected,
            "query_missing_required_param" => Self::QueryMissingRequiredParam,
            "unsafe_expression" => Self::UnsafeExpression,
            _ => Self::Other,
        }
    }

    /// Classify an MCP error message into an error code
    pub fn classify_error_message(message: &str) -> Self {
        let msg = message.to_lowercase();
        if msg.contains("must start with @") || msg.contains("must start with '@'") {
            Self::LocationMissingAt
        } else if msg.contains("expected @file#line") || msg.contains("expected @file.py#line") {
            Self::LocationMissingLine
        } else if msg.contains("invalid line number") {
            Self::LocationInvalidLine
        } else if msg.contains("connection") && msg.contains("not found") {
            Self::ConnectionNotFound
        } else if msg.contains("not connected") || msg.contains("disconnected") {
            Self::ConnectionDisconnected
        } else if msg.contains("name' or 'group'") || msg.contains("name or group") {
            Self::QueryMissingRequiredParam
        } else if msg.contains("unsafe") || msg.contains("forbidden") {
            Self::UnsafeExpression
        } else {
            Self::Other
        }
    }
}

/// Event representing a single MCP tool invocation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpUsageEvent {
    /// Timestamp in microseconds since Unix epoch
    pub timestamp_micros: i64,
    /// Tool name that was called
    pub tool_name: String,
    /// Whether the call succeeded
    pub success: bool,
    /// Error code if failed
    pub error_code: Option<McpErrorCode>,
    /// Latency in milliseconds
    pub latency_ms: u64,
    /// Session ID for correlation (optional)
    pub session_id: Option<String>,
    /// Prior tools called (JSON array)
    pub prior_tools: Vec<String>,
}

impl McpUsageEvent {
    /// Create a success event
    pub fn success(
        tool_name: impl Into<String>,
        latency_ms: u64,
        prior_tools: Vec<String>,
    ) -> Self {
        Self {
            timestamp_micros: chrono::Utc::now().timestamp_micros(),
            tool_name: tool_name.into(),
            success: true,
            error_code: None,
            latency_ms,
            session_id: None,
            prior_tools,
        }
    }

    /// Create an error event
    pub fn error(
        tool_name: impl Into<String>,
        error_code: McpErrorCode,
        latency_ms: u64,
        prior_tools: Vec<String>,
    ) -> Self {
        Self {
            timestamp_micros: chrono::Utc::now().timestamp_micros(),
            tool_name: tool_name.into(),
            success: false,
            error_code: Some(error_code),
            latency_ms,
            session_id: None,
            prior_tools,
        }
    }
}
