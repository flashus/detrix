//! CRUD handlers: add_metric, remove_metric, update_metric, get_metric

use crate::constants::status;
use crate::error::ToStatusResult;
use crate::generated::detrix::v1::{
    get_metric_request, remove_metric_request, AddMetricRequest, GetMetricRequest, MetricResponse,
    RemoveMetricRequest, RemoveMetricResponse, UpdateMetricRequest,
};
use crate::grpc::conversions::{add_request_to_metric, core_to_proto_location};
use crate::state::ApiState;
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Handle add_metric request
pub async fn handle_add_metric(
    state: &Arc<ApiState>,
    request: Request<AddMetricRequest>,
) -> Result<Response<MetricResponse>, Status> {
    let user = crate::grpc::extract_user(&request)?;
    let client_id = crate::grpc::extract_client_id(&request)?;
    let mut req = request.into_inner();

    // If safety_level is not provided, use the default from config
    if req.safety_level.is_empty() {
        let config = state.config_service.get_config().await;
        req.safety_level = config.safety.default_safety_level().as_str().to_string();
    }

    // Fetch connection for language derivation and path resolution
    let conn_id = detrix_core::ConnectionId::from(req.connection_id.as_str());
    let connection = state
        .context
        .connection_service
        .get_connection(&conn_id)
        .await
        .to_status()?
        .ok_or_else(|| {
            Status::not_found(format!("Connection '{}' not found", req.connection_id))
        })?;

    // Resolve relative file path against connection's workspace_root
    if let Some(ref mut location) = req.location {
        location.file = detrix_application::resolve_file_path(
            &location.file,
            connection.valid_workspace_root(),
        );

        // Pre-fetch file into VFS cache (transparent remote file fetching for cloud mode)
        if let Err(e) = state
            .context
            .file_source_chain
            .ensure_available(&connection, &location.file)
            .await
        {
            tracing::debug!("Pre-fetch skipped for '{}': {e}", location.file);
        }
    }

    // If language is not provided, derive it from the connection (same pattern as MCP/REST)
    if req.language.as_ref().is_none_or(|l| l.is_empty()) {
        req.language = Some(connection.language.to_string());
    }

    // Convert proto DTO to domain type
    let mut metric =
        add_request_to_metric(&req).map_err(|e| Status::invalid_argument(e.to_string()))?;
    // Stamp identity from authenticated user + client header
    metric.user_id = Some(user.user_id.clone());
    metric.agent_id = client_id;

    // Call service (ALL business logic happens here)
    // Pass replace flag (default to false if not specified)
    let replace = req.replace.unwrap_or(false);
    let outcome = state
        .context
        .metric_service
        .add_metric(metric.clone(), replace, None)
        .await
        .to_status()?;

    // Log warnings to tracing and collect them for the response
    for warning in &outcome.warnings {
        tracing::warn!("{}", warning);
    }
    let warnings: Vec<String> = outcome.warnings.iter().map(|w| w.to_string()).collect();

    // Return proto DTO
    Ok(Response::new(MetricResponse {
        metric_id: outcome.value.0,
        name: metric.name.clone(),
        status: status::CREATED.to_string(),
        location: Some(core_to_proto_location(&metric.location)),
        metadata: None,
        expressions: metric.expressions,
        warnings,
    }))
}

/// Handle remove_metric request
pub async fn handle_remove_metric(
    state: &Arc<ApiState>,
    request: Request<RemoveMetricRequest>,
) -> Result<Response<RemoveMetricResponse>, Status> {
    let user = crate::grpc::extract_user(&request)?;
    let client_id = crate::grpc::extract_client_id(&request)?;
    let scope = crate::common::build_scope(&user.user_id, &user.role, client_id.clone());
    let req = request.into_inner();
    tracing::info!(?client_id, "gRPC: remove_metric");

    // Extract metric_id from oneof identifier
    let metric_id = match req.identifier {
        Some(remove_metric_request::Identifier::MetricId(id)) => detrix_core::MetricId(id),
        Some(remove_metric_request::Identifier::Name(name)) => {
            // Look up by name first
            let metric = state
                .context
                .metric_service
                .get_metric_by_name(&name)
                .await
                .to_status()?
                .ok_or_else(|| Status::not_found(format!("Metric '{}' not found", name)))?;
            metric
                .id
                .ok_or_else(|| Status::internal("Metric has no ID"))?
        }
        None => return Err(Status::invalid_argument("Missing metric identifier")),
    };

    // Call service
    state
        .context
        .metric_service
        .remove_metric(metric_id, &scope)
        .await
        .to_status()?;

    // Return proto DTO
    Ok(Response::new(RemoveMetricResponse {
        success: true,
        metadata: None,
    }))
}

/// Handle update_metric request
pub async fn handle_update_metric(
    state: &Arc<ApiState>,
    request: Request<UpdateMetricRequest>,
) -> Result<Response<MetricResponse>, Status> {
    let user = crate::grpc::extract_user(&request)?;
    let client_id = crate::grpc::extract_client_id(&request)?;
    let scope = crate::common::build_scope(&user.user_id, &user.role, client_id.clone());
    let req = request.into_inner();
    tracing::info!(metric_id = req.metric_id, ?client_id, "gRPC: update_metric");
    let metric_id = detrix_core::MetricId(req.metric_id);

    // Get existing metric
    let mut metric = state
        .context
        .metric_service
        .get_metric(metric_id)
        .await
        .to_status()?
        .ok_or_else(|| Status::not_found(format!("Metric {} not found", metric_id)))?;

    // Apply updates
    if !req.expressions.is_empty() {
        metric.expressions = req.expressions;
    }
    if let Some(enabled) = req.enabled {
        metric.enabled = enabled;
    }
    if let Some(mode) = req.mode {
        metric.mode = crate::grpc::conversions::proto_to_core_mode(&mode)?;
    }
    if req.condition.is_some() {
        metric.condition = req.condition;
    }

    // Call service
    let outcome = state
        .context
        .metric_service
        .update_metric(&metric, &scope)
        .await
        .to_status()?;

    // Log warnings and collect for response
    for warning in &outcome.warnings {
        tracing::warn!("{}", warning);
    }
    let warnings: Vec<String> = outcome.warnings.iter().map(|w| w.to_string()).collect();

    // Metric from service should always have ID - error if missing (database integrity issue)
    let metric_id = metric
        .id
        .map(|id| id.0)
        .ok_or_else(|| Status::internal("Updated metric missing ID - database integrity issue"))?;

    // Return proto DTO
    Ok(Response::new(MetricResponse {
        metric_id,
        name: metric.name.clone(),
        status: status::UPDATED.to_string(),
        location: Some(core_to_proto_location(&metric.location)),
        metadata: None,
        expressions: metric.expressions,
        warnings,
    }))
}

/// Handle get_metric request
pub async fn handle_get_metric(
    state: &Arc<ApiState>,
    request: Request<GetMetricRequest>,
) -> Result<Response<MetricResponse>, Status> {
    let user = crate::grpc::extract_user(&request)?;
    let client_id = crate::grpc::extract_client_id(&request)?;
    let scope = crate::common::build_scope(&user.user_id, &user.role, client_id);
    let req = request.into_inner();

    // Extract metric from oneof identifier
    let metric = match req.identifier {
        Some(get_metric_request::Identifier::MetricId(id)) => {
            let metric_id = detrix_core::MetricId(id);
            state
                .context
                .metric_service
                .get_metric(metric_id)
                .await
                .to_status()?
                .ok_or_else(|| Status::not_found(format!("Metric {} not found", metric_id)))?
        }
        Some(get_metric_request::Identifier::Name(name)) => state
            .context
            .metric_service
            .get_metric_by_name(&name)
            .await
            .to_status()?
            .ok_or_else(|| Status::not_found(format!("Metric '{}' not found", name)))?,
        None => return Err(Status::invalid_argument("Missing metric identifier")),
    };

    // Enforce read scope — return not_found to avoid leaking existence
    if !scope.can_read(&metric) {
        return Err(Status::not_found("Metric not found"));
    }

    // Metric from storage should always have ID - error if missing (database integrity issue)
    let metric_id = metric
        .id
        .map(|id| id.0)
        .ok_or_else(|| Status::internal("Found metric missing ID - database integrity issue"))?;

    // Return proto DTO (get is a read — no warnings)
    Ok(Response::new(MetricResponse {
        metric_id,
        name: metric.name.clone(),
        status: status::FOUND.to_string(),
        location: Some(core_to_proto_location(&metric.location)),
        metadata: None,
        expressions: metric.expressions,
        warnings: vec![],
    }))
}
