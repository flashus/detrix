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
    let mut req = request.into_inner();

    // If safety_level is not provided, use the default from config
    if req.safety_level.is_empty() {
        let config = state.config_service.get_config().await;
        req.safety_level = config.safety.default_safety_level().as_str().to_string();
    }

    // Convert proto DTO to domain type
    let metric =
        add_request_to_metric(&req).map_err(|e| Status::invalid_argument(e.to_string()))?;

    // Call service (ALL business logic happens here)
    // Pass replace flag (default to false if not specified)
    let replace = req.replace.unwrap_or(false);
    let outcome = state
        .context
        .metric_service
        .add_metric(metric.clone(), replace)
        .await
        .to_status()?;

    // Log any warnings to tracing (presentation layer responsibility)
    for warning in &outcome.warnings {
        tracing::warn!("{}", warning);
    }

    // Return proto DTO
    Ok(Response::new(MetricResponse {
        metric_id: outcome.value.0,
        name: metric.name,
        status: status::CREATED.to_string(),
        location: Some(core_to_proto_location(&metric.location)),
        metadata: None,
    }))
}

/// Handle remove_metric request
pub async fn handle_remove_metric(
    state: &Arc<ApiState>,
    request: Request<RemoveMetricRequest>,
) -> Result<Response<RemoveMetricResponse>, Status> {
    let req = request.into_inner();

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
        .remove_metric(metric_id)
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
    let req = request.into_inner();
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
    if let Some(expression) = req.expression {
        metric.expression = expression;
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
        .update_metric(&metric)
        .await
        .to_status()?;

    // Log any warnings to tracing (presentation layer responsibility)
    for warning in &outcome.warnings {
        tracing::warn!("{}", warning);
    }

    // Metric from service should always have ID - error if missing (database integrity issue)
    let metric_id = metric
        .id
        .map(|id| id.0)
        .ok_or_else(|| Status::internal("Updated metric missing ID - database integrity issue"))?;

    // Return proto DTO
    Ok(Response::new(MetricResponse {
        metric_id,
        name: metric.name,
        status: status::UPDATED.to_string(),
        location: Some(core_to_proto_location(&metric.location)),
        metadata: None,
    }))
}

/// Handle get_metric request
pub async fn handle_get_metric(
    state: &Arc<ApiState>,
    request: Request<GetMetricRequest>,
) -> Result<Response<MetricResponse>, Status> {
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

    // Metric from storage should always have ID - error if missing (database integrity issue)
    let metric_id = metric
        .id
        .map(|id| id.0)
        .ok_or_else(|| Status::internal("Found metric missing ID - database integrity issue"))?;

    // Return proto DTO
    Ok(Response::new(MetricResponse {
        metric_id,
        name: metric.name.clone(),
        status: status::FOUND.to_string(),
        location: Some(core_to_proto_location(&metric.location)),
        metadata: None,
    }))
}
