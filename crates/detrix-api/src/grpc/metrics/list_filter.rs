//! List and filter handlers: list_metrics, list_groups

use crate::error::ToStatusResult;
use crate::generated::detrix::v1::{
    ListGroupsRequest, ListGroupsResponse, ListMetricsRequest, ListMetricsResponse,
};
use crate::grpc::conversions::metric_to_info;
use crate::state::ApiState;
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Handle list_metrics request
pub async fn handle_list_metrics(
    state: &Arc<ApiState>,
    request: Request<ListMetricsRequest>,
) -> Result<Response<ListMetricsResponse>, Status> {
    let req = request.into_inner();

    // Call service (filter by group if provided)
    let metrics = if let Some(group) = req.group {
        state
            .context
            .metric_service
            .list_metrics_by_group(&group)
            .await
            .to_status()?
    } else {
        state
            .context
            .metric_service
            .list_metrics()
            .await
            .to_status()?
    };

    // Filter by enabled_only if requested
    let filtered_metrics: Vec<_> = if req.enabled_only.unwrap_or(false) {
        metrics.into_iter().filter(|m| m.enabled).collect()
    } else {
        metrics
    };

    // Convert to proto DTOs - skip metrics with missing IDs (shouldn't happen, but be resilient)
    let metric_infos: Vec<_> = filtered_metrics
        .iter()
        .filter_map(|m| {
            metric_to_info(m)
                .inspect_err(|e| {
                    tracing::warn!(metric_name = %m.name, "Skipping metric: {}", e);
                })
                .ok()
        })
        .collect();

    Ok(Response::new(ListMetricsResponse {
        metrics: metric_infos,
        metadata: None,
    }))
}

/// Handle list_groups request
pub async fn handle_list_groups(
    state: &Arc<ApiState>,
    _request: Request<ListGroupsRequest>,
) -> Result<Response<ListGroupsResponse>, Status> {
    // Call service
    let groups = state
        .context
        .metric_service
        .list_groups()
        .await
        .to_status()?;

    // Convert to proto DTOs
    let group_infos = groups
        .iter()
        .map(crate::grpc::conversions::core_group_info_to_proto)
        .collect();

    Ok(Response::new(ListGroupsResponse {
        groups: group_infos,
        metadata: None,
    }))
}
