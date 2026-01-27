//! System tools: query_events, acknowledge, usage, validate, inspect
//!
//! These tools provide system-level operations and diagnostics.

use crate::mcp::conversions::{
    format_inspection_result, parse_event_type, system_events_to_display,
};
use crate::mcp::error::ToMcpResult;
use crate::mcp::format::{format_response, ResponseFormat};
use crate::mcp::params::{
    AcknowledgeEventsParams, InspectFileParams, QuerySystemEventsParams, ValidateExpressionParams,
};
use crate::state::ApiState;
use detrix_application::{
    FileInspectionRequest, FileInspectionService, UsageSnapshot, ValidationResult,
};
use detrix_core::{ConnectionId, SafetyLevel, SourceLanguage};
use rmcp::model::Content;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

// ============================================================================
// query_system_events Implementation
// ============================================================================

/// Result of query_system_events operation
pub struct QuerySystemEventsResult {
    pub count: usize,
    pub unacknowledged_count: i64,
    pub formatted_output: String,
}

impl QuerySystemEventsResult {
    /// Build human-readable summary message
    pub fn build_message(&self) -> String {
        format!(
            "Found {} system events | {} unacknowledged",
            self.count, self.unacknowledged_count
        )
    }
}

/// Result when system event repository is not configured
pub struct SystemEventsNotConfigured;

impl SystemEventsNotConfigured {
    pub fn build_message(&self) -> &'static str {
        "⚠️ System event repository not configured. Events are being broadcast but not persisted.\n\
         Use 'subscribe_events' for real-time streaming instead."
    }
}

/// Core query_system_events implementation
pub async fn query_system_events_impl(
    state: &Arc<ApiState>,
    params: QuerySystemEventsParams,
) -> Result<Result<QuerySystemEventsResult, SystemEventsNotConfigured>, McpError> {
    // Check if system event repository is available
    let repo = match &state.system_event_repository {
        Some(repo) => repo,
        None => return Ok(Err(SystemEventsNotConfigured)),
    };

    // Convert u64 to i64 for repository (capped, always fits)
    let limit = params
        .limit
        .unwrap_or(detrix_config::DEFAULT_QUERY_LIMIT as u64)
        .min(detrix_config::DEFAULT_MAX_QUERY_LIMIT as u64) as i64;
    let format = ResponseFormat::from_str_opt(params.format.as_deref());

    // Build query based on parameters
    let events = if params.unacknowledged_only.unwrap_or(false) {
        // Query unacknowledged events
        repo.find_unacknowledged(limit)
            .await
            .mcp_context("Failed to query unacknowledged events")?
    } else if let Some(since_ts) = params.since_timestamp {
        // Query events since timestamp
        repo.find_since(since_ts, limit)
            .await
            .mcp_context("Failed to query events since timestamp")?
    } else if let Some(ref conn_id) = params.connection_id {
        // Query by connection ID
        let connection_id = ConnectionId::new(conn_id);
        repo.find_by_connection(&connection_id, limit)
            .await
            .mcp_context("Failed to query events by connection")?
    } else if let Some(ref event_types) = params.event_types {
        // Query by event type(s) - combine results from each type
        let mut all_events = Vec::new();
        for type_str in event_types {
            if let Some(event_type) = parse_event_type(type_str) {
                let type_events = repo
                    .find_by_type(event_type, limit)
                    .await
                    .mcp_context("Failed to query events by type")?;
                all_events.extend(type_events);
            }
        }
        // Sort by timestamp descending and limit
        all_events.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        all_events.truncate(limit as usize);
        all_events
    } else {
        // Query recent events
        repo.find_recent(limit)
            .await
            .mcp_context("Failed to query recent events")?
    };

    // Convert to display format
    let display_events = system_events_to_display(&events);

    // Format output
    let output = format_response(&display_events, format);

    // Get unacknowledged count
    let unacked_count = repo
        .count_unacknowledged()
        .await
        .mcp_context("Failed to count unacknowledged")?;

    Ok(Ok(QuerySystemEventsResult {
        count: events.len(),
        unacknowledged_count: unacked_count,
        formatted_output: output,
    }))
}

// ============================================================================
// acknowledge_events Implementation
// ============================================================================

/// Result of acknowledge_events operation
pub struct AcknowledgeEventsResult {
    pub acknowledged: u64,
    pub requested: usize,
}

impl AcknowledgeEventsResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "✅ Acknowledged {} event(s) (requested: {})",
            self.acknowledged, self.requested
        )
    }
}

/// Result when no event IDs provided
pub struct NoEventIdsProvided;

impl NoEventIdsProvided {
    pub fn build_message(&self) -> &'static str {
        "No event IDs provided to acknowledge."
    }
}

/// Core acknowledge_events implementation
pub async fn acknowledge_events_impl(
    state: &Arc<ApiState>,
    params: AcknowledgeEventsParams,
) -> Result<
    Result<AcknowledgeEventsResult, Result<SystemEventsNotConfigured, NoEventIdsProvided>>,
    McpError,
> {
    // Check if system event repository is available
    let repo = match &state.system_event_repository {
        Some(repo) => repo,
        None => return Ok(Err(Ok(SystemEventsNotConfigured))),
    };

    if params.event_ids.is_empty() {
        return Ok(Err(Err(NoEventIdsProvided)));
    }

    let acknowledged = repo
        .acknowledge(&params.event_ids)
        .await
        .mcp_context("Failed to acknowledge events")?;

    Ok(Ok(AcknowledgeEventsResult {
        acknowledged,
        requested: params.event_ids.len(),
    }))
}

// ============================================================================
// get_mcp_usage Implementation
// ============================================================================

/// Result of get_mcp_usage operation
pub struct GetMcpUsageResult {
    pub snapshot: UsageSnapshot,
}

impl GetMcpUsageResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        let snapshot = &self.snapshot;
        let mut output = String::new();
        output.push_str("📊 MCP Usage Statistics\n");
        output.push_str("========================\n\n");

        output.push_str(&format!("Total Calls: {}\n", snapshot.total_calls));
        output.push_str(&format!(
            "Success Rate: {:.1}%\n",
            snapshot.success_rate * 100.0
        ));
        output.push_str(&format!(
            "Avg Latency: {:.1}ms\n\n",
            snapshot.avg_latency_ms
        ));

        // Error breakdown
        if !snapshot.top_errors.is_empty() {
            output.push_str("Top Errors:\n");
            for error in &snapshot.top_errors {
                output.push_str(&format!(
                    "  - {}: {} occurrences\n",
                    error.code, error.count
                ));
            }
            output.push('\n');
        }

        // Workflow stats
        output.push_str("Workflow Analysis:\n");
        output.push_str(&format!(
            "  - Correct (create_connection → inspect → add): {}\n",
            snapshot.workflow.correct_workflows
        ));
        output.push_str(&format!(
            "  - Skipped inspect_file: {}\n",
            snapshot.workflow.skipped_inspect
        ));
        output.push_str(&format!(
            "  - No connection established: {}\n",
            snapshot.workflow.no_connection
        ));

        // Add recommendations if there are issues
        if snapshot.success_rate < 0.9 && snapshot.total_calls > 10 {
            output.push_str("\n⚠️ Recommendations:\n");
            if snapshot.workflow.no_connection > 0 {
                output.push_str("  - Always call create_connection first before adding metrics\n");
            }
            if snapshot.workflow.skipped_inspect > snapshot.workflow.correct_workflows {
                output
                    .push_str("  - Use inspect_file to find the correct line before add_metric\n");
            }
        }

        output
    }

    /// Build JSON response
    pub fn build_json(&self) -> String {
        serde_json::to_string_pretty(&self.snapshot).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Core get_mcp_usage implementation
pub fn get_mcp_usage_impl(state: &Arc<ApiState>) -> GetMcpUsageResult {
    let snapshot = state.context.mcp_usage.snapshot();
    GetMcpUsageResult { snapshot }
}

// ============================================================================
// validate_expression Implementation
// ============================================================================

/// Result of validate_expression operation
pub struct ValidateExpressionResult {
    pub is_safe: bool,
    pub validation: ValidationResult,
}

impl ValidateExpressionResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        let status = if self.is_safe { "SAFE" } else { "UNSAFE" };
        format!("Expression is {}", status)
    }

    /// Build JSON response
    pub fn build_json(&self) -> String {
        serde_json::to_string_pretty(&self.validation)
            .unwrap_or_else(|_| "Error serializing result".to_string())
    }
}

/// Core validate_expression implementation
pub fn validate_expression_impl(
    state: &Arc<ApiState>,
    params: ValidateExpressionParams,
) -> Result<ValidateExpressionResult, McpError> {
    match state.context.metric_service.validate_expression(
        &params.expression,
        &params.language,
        SafetyLevel::Strict,
    ) {
        Ok(result) => Ok(ValidateExpressionResult {
            is_safe: result.is_safe,
            validation: result,
        }),
        Err(e) => Err(McpError::internal_error(
            format!("Failed to validate expression: {}", e),
            None,
        )),
    }
}

// ============================================================================
// inspect_file Implementation
// ============================================================================

/// Result of inspect_file operation
pub struct InspectFileResult {
    /// Detected source language (available for future use)
    #[allow(dead_code)]
    pub language: SourceLanguage,
    pub messages: Vec<Content>,
}

/// Core inspect_file implementation
pub fn inspect_file_impl(params: InspectFileParams) -> Result<InspectFileResult, McpError> {
    let request = FileInspectionRequest {
        file_path: params.file_path.clone(),
        line: params.line,
        find_variable: params.find_variable,
    };

    let service = FileInspectionService::new();
    let (language, result) = service
        .inspect(request)
        .mcp_context("File inspection failed")?;

    let messages = format_inspection_result(language, result, &params.file_path);
    Ok(InspectFileResult { language, messages })
}
