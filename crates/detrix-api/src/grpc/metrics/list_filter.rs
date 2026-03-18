//! List and filter handlers: list_metrics, list_groups

use crate::error::ToStatusResult;
use crate::generated::detrix::v1::{
    ListGroupsRequest, ListGroupsResponse, ListMetricsRequest, ListMetricsResponse,
};
use crate::grpc::conversions::metric_to_info;
use crate::state::ApiState;
use detrix_application::MetricFilter;
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Handle list_metrics request
pub async fn handle_list_metrics(
    state: &Arc<ApiState>,
    request: Request<ListMetricsRequest>,
) -> Result<Response<ListMetricsResponse>, Status> {
    let user = crate::grpc::extract_user(&request)?;
    let client_id = crate::grpc::extract_client_id(&request)?;
    let scope = detrix_application::extract_scope(&user.user_id, &user.role, client_id);
    let req = request.into_inner();

    // Build filter with user scope and optional group
    let filter = MetricFilter {
        user_id: scope.user_id().map(|s| s.to_string()),
        group: req.group,
        enabled: if req.enabled_only.unwrap_or(false) {
            Some(true)
        } else {
            None
        },
        ..Default::default()
    };

    let (metrics, _) = state
        .context
        .metric_service
        .list_metrics_filtered(&filter, usize::MAX, 0)
        .await
        .to_status()?;

    // Convert to proto DTOs - skip metrics with missing IDs (shouldn't happen, but be resilient)
    let metric_infos: Vec<_> = metrics
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
    request: Request<ListGroupsRequest>,
) -> Result<Response<ListGroupsResponse>, Status> {
    let user = crate::grpc::extract_user(&request)?;
    let client_id = crate::grpc::extract_client_id(&request)?;
    let scope = detrix_application::extract_scope(&user.user_id, &user.role, client_id);

    let summaries = state
        .context
        .metric_service
        .list_group_summaries_scoped(&scope)
        .await
        .to_status()?;

    let group_infos = summaries
        .into_iter()
        .map(|s| crate::generated::detrix::v1::GroupInfo {
            name: s
                .name
                .unwrap_or_else(|| detrix_core::DEFAULT_GROUP_NAME.to_string()),
            metric_count: s.metric_count as u32,
            enabled_count: s.enabled_count as u32,
        })
        .collect();

    Ok(Response::new(ListGroupsResponse {
        groups: group_infos,
        metadata: None,
    }))
}
