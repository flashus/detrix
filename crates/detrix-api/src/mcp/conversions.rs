//! Type conversions between MCP types and Detrix core types
//!
//! This module provides conversion functions similar to grpc/conversions.rs
//! but for MCP-specific types.

use chrono::{TimeZone, Utc};
use detrix_application::services::file_inspection_types::SourceLanguageExt;
use detrix_core::system_event::{SystemEvent, SystemEventType};
use detrix_core::{CapturedStackTrace, ExpressionValue, MemorySnapshot, Metric, MetricEvent};
use rmcp::model::*;
use serde::Serialize;

// Most conversions are handled inline in server.rs using existing gRPC converters
// This module is reserved for MCP-specific conversion needs

/// Format a microsecond timestamp into an ISO 8601 string and compute age in seconds.
fn format_timestamp_us(timestamp_us: i64) -> (String, i64) {
    let now_us = Utc::now().timestamp_micros();
    let age_seconds = (now_us - timestamp_us) / 1_000_000;
    let iso = match Utc.timestamp_micros(timestamp_us) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        chrono::LocalResult::Ambiguous(dt, _) => dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        chrono::LocalResult::None => "invalid timestamp".to_string(),
    };
    (iso, age_seconds)
}

/// Convert a metric to MCP Content for display
pub fn metric_to_content(metric: &Metric) -> Vec<Content> {
    vec![
        Content::text(format!("Metric: {}", metric.name)),
        Content::text(
            serde_json::to_string_pretty(metric)
                .unwrap_or_else(|_| "Error serializing metric".to_string()),
        ),
    ]
}

/// A wrapper for MetricEvent that adds human-readable timestamp information
/// for LLM agents to easily understand event freshness
#[derive(Debug, Serialize)]
pub struct MetricEventDisplay {
    /// Event ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// Metric name that generated this event
    pub metric_name: String,
    /// Human-readable ISO 8601 timestamp
    pub timestamp_iso: String,
    /// Seconds ago this event occurred (for freshness checking)
    pub age_seconds: i64,
    /// The captured value as a string
    pub value: String,
    /// Whether this was an error
    pub is_error: bool,
    /// Error message if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Connection that produced this event
    pub connection_id: String,
    /// Original timestamp in microseconds (for precise ordering)
    pub timestamp_us: i64,
    /// Captured stack trace (if introspection enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<CapturedStackTrace>,
    /// Captured memory snapshot (if introspection enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_snapshot: Option<MemorySnapshot>,
    /// Per-expression values (for multi-expression metrics)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<ExpressionValue>,
}

impl From<&MetricEvent> for MetricEventDisplay {
    fn from(event: &MetricEvent) -> Self {
        let (timestamp_iso, age_seconds) = format_timestamp_us(event.timestamp);

        // Use value_string if available, otherwise value_json
        let value = event
            .value_string()
            .map(|s| s.to_string())
            .unwrap_or_else(|| event.value_json().to_string());

        MetricEventDisplay {
            id: event.id,
            metric_name: event.metric_name.clone(),
            timestamp_iso,
            age_seconds,
            value,
            is_error: event.is_error,
            error_message: event.error_message.clone(),
            connection_id: event.connection_id.to_string(),
            timestamp_us: event.timestamp,
            stack_trace: event.stack_trace.clone(),
            memory_snapshot: event.memory_snapshot.clone(),
            values: event.values.clone(),
        }
    }
}

/// Convert events to display format with human-readable timestamps
pub fn events_to_display(events: &[MetricEvent]) -> Vec<MetricEventDisplay> {
    events.iter().map(MetricEventDisplay::from).collect()
}

// ============================================================================
// File Inspection Conversions
// ============================================================================

use detrix_application::{
    CodeLine, FileInspectionResult, FileOverview, LineInspectionResult, SourceLanguage,
    VariableSearchResult,
};

/// Format file inspection result as MCP content
///
/// Takes domain types and produces protocol-specific MCP response content.
pub fn format_inspection_result(
    language: SourceLanguage,
    result: FileInspectionResult,
    file_path: &str,
) -> Vec<Content> {
    let mut messages = Vec::new();

    match result {
        FileInspectionResult::VariableSearch(search) => {
            format_variable_search(&mut messages, language, search, file_path);
        }
        FileInspectionResult::LineInspection(inspection) => {
            format_line_inspection(&mut messages, language, inspection, file_path);
        }
        FileInspectionResult::Overview(overview) => {
            format_overview(&mut messages, language, overview, file_path);
        }
    }

    messages
}

/// Format variable search result
fn format_variable_search(
    messages: &mut Vec<Content>,
    language: SourceLanguage,
    search: VariableSearchResult,
    file_path: &str,
) {
    if search.definitions.is_empty() {
        messages.push(Content::text(format!(
            "⚠️ Variable '{}' not found in {}",
            search.variable_name, file_path
        )));

        if !search.similar_variables.is_empty() {
            messages.push(Content::text(format!(
                "Did you mean: {:?}",
                search.similar_variables
            )));
        }

        // For non-Python, add note about limited analysis
        if !language.supports_ast_analysis() {
            messages.push(Content::text(format!(
                "⚠️ Full variable scope analysis not available for {} files (text search only).",
                language.display_name()
            )));
        }
    } else {
        messages.push(Content::text(format!(
            "✓ Found '{}' defined at line(s): {:?}",
            search.variable_name, search.suggested_lines
        )));

        // Show context
        if let Some(context) = search.context {
            messages.push(Content::text("Code context:".to_string()));
            format_code_lines(messages, &context.lines);
        }

        // Suggest metric placement
        if let Some(first_line) = search.suggested_lines.first() {
            messages.push(Content::text(format!(
                "\n💡 Use: add_metric(location=\"@{}#{}\", expression=\"...\")",
                file_path, first_line
            )));
        }
    }
}

/// Format line inspection result
fn format_line_inspection(
    messages: &mut Vec<Content>,
    language: SourceLanguage,
    inspection: LineInspectionResult,
    file_path: &str,
) {
    messages.push(Content::text(format!(
        "📄 {} file: {} (line {})",
        language.display_name(),
        file_path,
        inspection.target_line
    )));

    // Show available variables (only for Python AST)
    if language.supports_ast_analysis() && !inspection.available_variables.is_empty() {
        messages.push(Content::text(format!(
            "Variables available at line {}: {:?}",
            inspection.target_line, inspection.available_variables
        )));
    }

    messages.push(Content::text(format!(
        "Code at line {}: {}",
        inspection.target_line, inspection.code_at_line
    )));

    // Show context
    messages.push(Content::text("Context:".to_string()));
    format_code_lines(messages, &inspection.context.lines);

    // Suggest metric placement
    messages.push(Content::text(format!(
        "\n💡 Use: add_metric(location=\"@{}#{}\", expression=\"...\")",
        file_path, inspection.target_line
    )));
}

/// Format file overview result
fn format_overview(
    messages: &mut Vec<Content>,
    language: SourceLanguage,
    overview: FileOverview,
    file_path: &str,
) {
    messages.push(Content::text(format!(
        "📄 {} file: {} ({} lines)",
        language.display_name(),
        file_path,
        overview.total_lines
    )));

    messages.push(Content::text(
        "Specify 'line' to see code context, or 'find_variable' to search for a variable."
            .to_string(),
    ));

    // Show preview if available
    if !overview.preview_lines.is_empty() {
        messages.push(Content::text("Preview (first 20 lines):".to_string()));
        messages.push(Content::text("---".to_string()));
        for line in &overview.preview_lines {
            messages.push(Content::text(format!(
                "{:4}: {}",
                line.line_number, line.code
            )));
        }
        if overview.total_lines > 20 {
            messages.push(Content::text(format!(
                "  ... ({} more lines)",
                overview.total_lines - 20
            )));
        }
    }
}

/// Format code lines with markers
fn format_code_lines(messages: &mut Vec<Content>, lines: &[CodeLine]) {
    for line in lines {
        let marker = if line.is_target { " >>> " } else { "     " };
        messages.push(Content::text(format!(
            "{}{:4}: {}",
            marker, line.line_number, line.code
        )));
    }
}

// ============================================================================
// System Event Conversions
// ============================================================================

/// A wrapper for SystemEvent that adds human-readable timestamp information
/// for LLM agents to easily understand event freshness
#[derive(Debug, Serialize)]
pub struct SystemEventDisplay {
    /// Event ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// Event type (metric_added, debugger_crash, etc.)
    pub event_type: String,
    /// Human-readable ISO 8601 timestamp
    pub timestamp_iso: String,
    /// Seconds ago this event occurred (for freshness checking)
    pub age_seconds: i64,
    /// Associated connection ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    /// Associated metric ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_id: Option<u64>,
    /// Metric name (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_name: Option<String>,
    /// Additional event details (JSON)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Whether this event has been acknowledged
    pub acknowledged: bool,
    /// Original timestamp in microseconds (for precise ordering)
    pub timestamp_us: i64,
}

impl From<&SystemEvent> for SystemEventDisplay {
    fn from(event: &SystemEvent) -> Self {
        let (timestamp_iso, age_seconds) = format_timestamp_us(event.timestamp);

        // Parse details JSON if present
        let details = event
            .details_json
            .as_ref()
            .and_then(|json| serde_json::from_str(json).ok());

        SystemEventDisplay {
            id: event.id,
            event_type: event.event_type.as_str().to_string(),
            timestamp_iso,
            age_seconds,
            connection_id: event.connection_id.as_ref().map(|c| c.to_string()),
            metric_id: event.metric_id,
            metric_name: event.metric_name.clone(),
            details,
            acknowledged: event.acknowledged,
            timestamp_us: event.timestamp,
        }
    }
}

/// Convert system events to display format with human-readable timestamps
pub fn system_events_to_display(events: &[SystemEvent]) -> Vec<SystemEventDisplay> {
    events.iter().map(SystemEventDisplay::from).collect()
}

/// Parse event type string to SystemEventType
pub fn parse_event_type(s: &str) -> Option<SystemEventType> {
    s.parse().ok()
}
