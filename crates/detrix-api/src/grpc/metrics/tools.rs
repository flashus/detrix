//! Tool handlers: validate_expression, inspect_file, get_mcp_usage

use crate::generated::detrix::v1::{
    GetMcpUsageRequest, GetMcpUsageResponse, InspectFileRequest, InspectFileResponse,
    McpErrorCount, McpUsageSnapshot, McpWorkflowStats, ValidateExpressionRequest,
    ValidateExpressionResponse,
};
use crate::state::ApiState;
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Handle validate_expression request
pub async fn handle_validate_expression(
    state: &Arc<ApiState>,
    request: Request<ValidateExpressionRequest>,
) -> Result<Response<ValidateExpressionResponse>, Status> {
    let req = request.into_inner();

    // Language is required - no default
    if req.language.is_empty() {
        return Err(Status::invalid_argument(
            detrix_core::SourceLanguage::language_error(""),
        ));
    }

    // Use default safety level (strict)
    let safety_level = detrix_core::SafetyLevel::Strict;

    let result = state.context.metric_service.validate_expression(
        &req.expression,
        &req.language,
        safety_level,
    );

    match result {
        Ok(validation) => Ok(Response::new(ValidateExpressionResponse {
            is_safe: validation.is_safe,
            expression: req.expression,
            metadata: None,
            language: req.language,
            violations: validation.errors,
            warnings: validation.warnings,
        })),
        Err(e) => Ok(Response::new(ValidateExpressionResponse {
            is_safe: false,
            expression: req.expression,
            metadata: None,
            language: req.language,
            violations: vec![e.to_string()],
            warnings: vec![],
        })),
    }
}

/// Handle inspect_file request
pub async fn handle_inspect_file(
    _state: &Arc<ApiState>,
    request: Request<InspectFileRequest>,
) -> Result<Response<InspectFileResponse>, Status> {
    use detrix_application::{FileInspectionRequest, FileInspectionService};

    let req = request.into_inner();

    // Validate file exists
    let path = std::path::Path::new(&req.file_path);
    if !path.exists() {
        return Err(Status::not_found(format!(
            "File not found: {}",
            req.file_path
        )));
    }

    // Create inspection request
    let inspection_request = FileInspectionRequest {
        file_path: req.file_path.clone(),
        line: req.line,
        find_variable: req.find_variable,
    };

    // Use FileInspectionService from application layer
    let file_inspection = FileInspectionService::new();
    let (_detected_language, result) = file_inspection
        .inspect(inspection_request)
        .map_err(|e| Status::internal(format!("File inspection failed: {}", e)))?;

    // Serialize result to JSON for gRPC response
    let content = serde_json::to_string(&result)
        .map_err(|e| Status::internal(format!("Failed to serialize result: {}", e)))?;

    Ok(Response::new(InspectFileResponse {
        success: true,
        content,
        error: None,
        metadata: None,
    }))
}

/// Handle get_mcp_usage request
pub async fn handle_get_mcp_usage(
    state: &Arc<ApiState>,
    _request: Request<GetMcpUsageRequest>,
) -> Result<Response<GetMcpUsageResponse>, Status> {
    // Get snapshot from MCP usage service
    let snapshot = state.context.mcp_usage.snapshot();

    // Convert application layer types to proto types
    let proto_snapshot = McpUsageSnapshot {
        total_calls: snapshot.total_calls,
        total_success: snapshot.total_success,
        total_errors: snapshot.total_errors,
        success_rate: snapshot.success_rate,
        avg_latency_ms: snapshot.avg_latency_ms,
        top_errors: snapshot
            .top_errors
            .into_iter()
            .map(|ec| McpErrorCount {
                code: ec.code,
                count: ec.count,
            })
            .collect(),
        workflow: Some(McpWorkflowStats {
            correct_workflows: snapshot.workflow.correct_workflows,
            skipped_inspect: snapshot.workflow.skipped_inspect,
            no_connection: snapshot.workflow.no_connection,
            adherence_rate: snapshot.workflow.adherence_rate,
        }),
    };

    Ok(Response::new(GetMcpUsageResponse {
        usage: Some(proto_snapshot),
        metadata: None,
    }))
}
