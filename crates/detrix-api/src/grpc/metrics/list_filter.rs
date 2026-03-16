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
    let scope = crate::common::build_scope(&user.user_id, &user.role, client_id);
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
    let scope = crate::common::build_scope(&user.user_id, &user.role, client_id);

    // Admin scope: use efficient GROUP BY query from storage.
    // User/Agent scope: fetch user's metrics and compute summaries in-memory.
    if scope.user_id().is_none() {
        let groups = state
            .context
            .metric_service
            .list_groups()
            .await
            .to_status()?;

        let group_infos = groups
            .iter()
            .map(crate::grpc::conversions::core_group_info_to_proto)
            .collect();

        return Ok(Response::new(ListGroupsResponse {
            groups: group_infos,
            metadata: None,
        }));
    }

    // User/Agent: filter metrics by user_id then build summaries
    let filter = MetricFilter {
        user_id: scope.user_id().map(|s| s.to_string()),
        ..Default::default()
    };
    let (metrics, _) = state
        .context
        .metric_service
        .list_metrics_filtered(&filter, usize::MAX, 0)
        .await
        .to_status()?;

    // Build summaries from filtered metrics
    let mut group_map: std::collections::HashMap<Option<String>, (u64, u64)> =
        std::collections::HashMap::new();
    for m in &metrics {
        let entry = group_map.entry(m.group.clone()).or_insert((0, 0));
        entry.0 += 1;
        if m.enabled {
            entry.1 += 1;
        }
    }

    let group_infos = group_map
        .into_iter()
        .map(
            |(name, (metric_count, enabled_count))| crate::generated::detrix::v1::GroupInfo {
                name: name.unwrap_or_else(|| "default".to_string()),
                metric_count: metric_count as u32,
                enabled_count: enabled_count as u32,
            },
        )
        .collect();

    Ok(Response::new(ListGroupsResponse {
        groups: group_infos,
        metadata: None,
    }))
}
