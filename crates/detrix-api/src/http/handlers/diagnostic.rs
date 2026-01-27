//! Diagnostic handlers
//!
//! Endpoints for expression validation, file inspection, and MCP usage statistics.
//!
//! Uses proto-generated types for requests where possible.
//! Uses From trait for clean conversion from service types to HTTP DTOs.

use super::parse_safety_level;
use crate::generated::detrix::v1::{
    InspectFileRequest as ProtoInspectFileRequest, ValidateExpressionRequest,
};
use crate::http::error::{HttpError, ToHttpResult};
use crate::state::ApiState;
use axum::{extract::State, Json};
use detrix_application::services::{ErrorCount, UsageSnapshot, WorkflowStats};
use detrix_application::{
    CodeLine, FileInspectionResult, FileOverview, LineInspectionResult, ValidationResult,
    VariableDefinition, VariableSearchResult,
};
use serde::Serialize;
use std::sync::Arc;
use tracing::info;

use crate::types::ValidateExpressionResponse;

/// Create ValidateExpressionResponse from validation result with request context
fn validation_response_from_result(
    result: ValidationResult,
    expression: String,
    language: String,
) -> ValidateExpressionResponse {
    ValidateExpressionResponse {
        is_safe: result.is_safe,
        expression,
        language,
        violations: result.errors,
        warnings: result.warnings,
        metadata: None,
    }
}

/// Create error response when validation itself fails
fn validation_response_from_error(
    error: String,
    expression: String,
    language: String,
) -> ValidateExpressionResponse {
    ValidateExpressionResponse {
        is_safe: false,
        expression,
        language,
        violations: vec![error],
        warnings: vec![],
        metadata: None,
    }
}

/// Validate if an expression is safe to use as a metric.
///
/// Performs safety analysis on the expression based on the language and
/// safety level. For Python, this includes AST analysis to detect dangerous
/// operations like `eval()`, `exec()`, file I/O, etc.
///
/// # Request Body
/// - `expression`: Expression to validate
/// - `language`: Language type ('python', 'go', 'rust')
/// - `safetyLevel`: Safety level ('strict', 'trusted') - default: from SafetyConfig
///
/// # Response
/// Returns `ValidateExpressionResponse` with safety status and any violations.
pub async fn validate_expression(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ValidateExpressionRequest>,
) -> Result<Json<ValidateExpressionResponse>, HttpError> {
    info!(
        "REST: validate_expression (expression='{}', language={})",
        req.expression, req.language
    );

    // Resolve safety level: use provided value or default from config
    let safety_level = match &req.safety_level {
        Some(level) => parse_safety_level(level).http_bad_request()?,
        None => state
            .config_service
            .get_config()
            .await
            .safety
            .default_safety_level(),
    };

    let result = state.context.metric_service.validate_expression(
        &req.expression,
        &req.language,
        safety_level,
    );

    match result {
        Ok(validation) => Ok(Json(validation_response_from_result(
            validation,
            req.expression,
            req.language,
        ))),
        Err(e) => Ok(Json(validation_response_from_error(
            e.to_string(),
            req.expression,
            req.language,
        ))),
    }
}

/// Response for inspect_file - line inspection variant
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectFileResponse {
    pub file_path: String,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_at_line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_variables: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable_definitions: Option<Vec<VariableDefinitionDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_lines: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similar_variables: Option<Vec<String>>,
    pub context_lines: Vec<CodeLineDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<u32>,
}

/// Code line DTO for file inspection
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeLineDto {
    pub line_number: u32,
    pub code: String,
    pub is_target: bool,
}

impl From<CodeLine> for CodeLineDto {
    fn from(line: CodeLine) -> Self {
        Self {
            line_number: line.line_number,
            code: line.code,
            is_target: line.is_target,
        }
    }
}

/// Variable definition DTO
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariableDefinitionDto {
    pub line: u32,
    pub scope: String,
    pub code: String,
}

impl From<VariableDefinition> for VariableDefinitionDto {
    fn from(def: VariableDefinition) -> Self {
        Self {
            line: def.line,
            scope: def.scope,
            code: def.code,
        }
    }
}

/// Inspect a file to find variables and code context
///
/// Use this endpoint before adding metrics to find the correct line number
/// and available variables. For Python files, provides full AST analysis.
pub async fn inspect_file(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<ProtoInspectFileRequest>,
) -> Result<Json<InspectFileResponse>, HttpError> {
    use detrix_application::{FileInspectionRequest, FileInspectionService};

    info!(
        "REST: inspect_file (file='{}', line={:?}, find_variable={:?})",
        req.file_path, req.line, req.find_variable
    );

    // Create inspection request - path validation is done in the service layer
    let inspection_request = FileInspectionRequest {
        file_path: req.file_path.clone(),
        line: req.line,
        find_variable: req.find_variable.clone(),
    };

    // Use FileInspectionService from application layer
    // Service validates path (length, null bytes, existence, traversal)
    let file_inspection = FileInspectionService::new();
    let (language, result) = file_inspection
        .inspect(inspection_request)
        .http_context("File inspection failed")?;

    // Convert domain result to DTO using From implementations
    let language_name = language.display_name().to_string();
    Ok(Json(InspectFileResponse::from_result(
        req.file_path,
        language_name,
        result,
    )))
}

impl InspectFileResponse {
    /// Create from file inspection result with request context
    fn from_result(file_path: String, language: String, result: FileInspectionResult) -> Self {
        match result {
            FileInspectionResult::LineInspection(r) => {
                Self::from_line_inspection(file_path, language, r)
            }
            FileInspectionResult::VariableSearch(r) => {
                Self::from_variable_search(file_path, language, r)
            }
            FileInspectionResult::Overview(r) => Self::from_overview(file_path, language, r),
        }
    }

    fn from_line_inspection(file_path: String, language: String, r: LineInspectionResult) -> Self {
        Self {
            file_path,
            language,
            target_line: Some(r.target_line),
            code_at_line: Some(r.code_at_line),
            available_variables: Some(r.available_variables),
            variable_definitions: None,
            suggested_lines: None,
            similar_variables: None,
            context_lines: r.context.lines.into_iter().map(CodeLineDto::from).collect(),
            total_lines: None,
        }
    }

    fn from_variable_search(file_path: String, language: String, r: VariableSearchResult) -> Self {
        Self {
            file_path,
            language,
            target_line: None,
            code_at_line: None,
            available_variables: None,
            variable_definitions: Some(
                r.definitions
                    .into_iter()
                    .map(VariableDefinitionDto::from)
                    .collect(),
            ),
            suggested_lines: Some(r.suggested_lines),
            similar_variables: Some(r.similar_variables),
            context_lines: r
                .context
                .map(|ctx| ctx.lines.into_iter().map(CodeLineDto::from).collect())
                .unwrap_or_default(),
            total_lines: None,
        }
    }

    fn from_overview(file_path: String, language: String, r: FileOverview) -> Self {
        Self {
            file_path,
            language,
            target_line: None,
            code_at_line: None,
            available_variables: None,
            variable_definitions: None,
            suggested_lines: None,
            similar_variables: None,
            context_lines: r.preview_lines.into_iter().map(CodeLineDto::from).collect(),
            total_lines: Some(r.total_lines),
        }
    }
}

// ============================================================================
// MCP Usage Statistics - Uses proto types
// ============================================================================

use crate::types::{McpErrorCount, McpUsageSnapshot, McpWorkflowStats};

impl From<WorkflowStats> for McpWorkflowStats {
    fn from(stats: WorkflowStats) -> Self {
        Self {
            correct_workflows: stats.correct_workflows,
            skipped_inspect: stats.skipped_inspect,
            no_connection: stats.no_connection,
            adherence_rate: stats.adherence_rate,
        }
    }
}

impl From<ErrorCount> for McpErrorCount {
    fn from(ec: ErrorCount) -> Self {
        Self {
            code: ec.code,
            count: ec.count,
        }
    }
}

impl From<UsageSnapshot> for McpUsageSnapshot {
    fn from(snapshot: UsageSnapshot) -> Self {
        Self {
            total_calls: snapshot.total_calls,
            total_success: snapshot.total_success,
            total_errors: snapshot.total_errors,
            success_rate: snapshot.success_rate,
            avg_latency_ms: snapshot.avg_latency_ms,
            top_errors: snapshot
                .top_errors
                .into_iter()
                .map(McpErrorCount::from)
                .collect(),
            workflow: Some(McpWorkflowStats::from(snapshot.workflow)),
        }
    }
}

/// Get MCP tool usage statistics.
///
/// Returns statistics about how MCP tools are being used, including
/// success rates, common errors, and workflow adherence. Useful for
/// diagnosing issues with LLM agent interactions and optimizing prompts.
///
/// # Response
/// Returns `McpUsageSnapshot` (proto) with:
/// - `totalCalls`: Total number of MCP tool calls
/// - `totalSuccess`/`totalErrors`: Success and error counts
/// - `successRate`: Ratio of successful calls (0.0-1.0)
/// - `avgLatencyMs`: Average latency of successful calls
/// - `topErrors`: Most common error types
/// - `workflow`: Statistics about workflow adherence (inspect before add, etc.)
pub async fn get_mcp_usage(State(state): State<Arc<ApiState>>) -> Json<McpUsageSnapshot> {
    info!("REST: get_mcp_usage");

    Json(McpUsageSnapshot::from(state.context.mcp_usage.snapshot()))
}
