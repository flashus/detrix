//! Control handlers: toggle_metric, enable_group, disable_group

use crate::error::ToStatusResult;
use crate::generated::detrix::v1::{
    GroupRequest, GroupResponse, ToggleMetricRequest, ToggleMetricResponse,
};
use crate::state::ApiState;
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Handle toggle_metric request
pub async fn handle_toggle_metric(
    state: &Arc<ApiState>,
    request: Request<ToggleMetricRequest>,
) -> Result<Response<ToggleMetricResponse>, Status> {
    let req = request.into_inner();
    let metric_id = detrix_core::MetricId(req.metric_id);

    // Get metric for response
    let metric = state
        .context
        .metric_service
        .get_metric(metric_id)
        .await
        .to_status()?
        .ok_or_else(|| Status::not_found(format!("Metric {} not found", metric_id)))?;

    // Call service - now returns rich result with DAP confirmation
    let result = state
        .context
        .metric_service
        .toggle_metric(metric_id, req.enabled)
        .await
        .to_status()?;

    Ok(Response::new(ToggleMetricResponse {
        metric_id: metric_id.0,
        name: metric.name,
        enabled: req.enabled,
        storage_updated: result.storage_updated,
        dap_confirmed: result.dap_confirmed,
        actual_line: result.actual_line,
        dap_message: result.dap_message,
        metadata: None,
        status: if req.enabled { "enabled" } else { "disabled" }.to_string(),
    }))
}

/// Handle enable_group request
pub async fn handle_enable_group(
    state: &Arc<ApiState>,
    request: Request<GroupRequest>,
) -> Result<Response<GroupResponse>, Status> {
    let req = request.into_inner();

    // Call service
    let result = state
        .context
        .metric_service
        .enable_group(&req.group_name)
        .await
        .to_status()?;

    // Log failures if any
    if result.has_failures() {
        tracing::warn!(
            "Partial failure enabling group '{}': {} succeeded, {} failed: {:?}",
            req.group_name,
            result.succeeded,
            result.failed.len(),
            result.failed
        );
    }

    Ok(Response::new(GroupResponse {
        group_name: req.group_name,
        metrics_affected: result.succeeded as u32,
        metadata: None,
    }))
}

/// Handle disable_group request
pub async fn handle_disable_group(
    state: &Arc<ApiState>,
    request: Request<GroupRequest>,
) -> Result<Response<GroupResponse>, Status> {
    let req = request.into_inner();

    // Call service
    let result = state
        .context
        .metric_service
        .disable_group(&req.group_name)
        .await
        .to_status()?;

    // Log failures if any
    if result.has_failures() {
        tracing::warn!(
            "Partial failure disabling group '{}': {} succeeded, {} failed: {:?}",
            req.group_name,
            result.succeeded,
            result.failed.len(),
            result.failed
        );
    }

    Ok(Response::new(GroupResponse {
        group_name: req.group_name,
        metrics_affected: result.succeeded as u32,
        metadata: None,
    }))
}
