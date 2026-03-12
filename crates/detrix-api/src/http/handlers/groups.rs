//! Group operation handlers
//!
//! Endpoints for listing groups and batch operations on metrics by group.
//!
//! Uses From trait for clean conversion from service types to proto DTOs.

use super::metric_to_rest_response;
use crate::http::error::{HttpError, ToHttpResult};
use crate::http::middleware::AuthenticatedUser;
use crate::state::ApiState;
use crate::types::{GroupInfo, MetricInfo};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Extension, Json,
};
use detrix_application::{GroupOperationResult, GroupSummary, MetricFilter};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

impl From<GroupSummary> for GroupInfo {
    fn from(summary: GroupSummary) -> Self {
        Self {
            name: summary.name.unwrap_or_else(|| "default".to_string()),
            metric_count: summary.metric_count as u32,
            enabled_count: summary.enabled_count as u32,
        }
    }
}

/// Group operation response DTO
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupOperationResponse {
    pub group: String,
    pub operation: String,
    pub affected_metrics: usize,
    pub success_count: usize,
    pub failure_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub errors: Vec<String>,
}

impl GroupOperationResponse {
    /// Create from operation result with context
    pub fn from_result(group: String, operation: &str, result: GroupOperationResult) -> Self {
        let total = result.succeeded + result.failed.len();
        let errors: Vec<String> = result
            .failed
            .iter()
            .map(|(name, err)| format!("{}: {}", name, err))
            .collect();

        Self {
            group,
            operation: operation.to_string(),
            affected_metrics: total,
            success_count: result.succeeded,
            failure_count: result.failed.len(),
            errors,
        }
    }
}

/// List all metric groups with summary statistics.
///
/// Groups are derived from metrics' `group` field. Metrics without a group
/// are assigned to "default".
///
/// # Response
/// Returns JSON array of `GroupInfo` objects (proto) with:
/// - `name`: Group name
/// - `metricCount`: Total metrics in the group
/// - `enabledCount`: Enabled metrics in the group
pub async fn list_groups(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<AuthenticatedUser>,
    headers: HeaderMap,
) -> Result<Json<Vec<GroupInfo>>, HttpError> {
    info!("REST: list_groups");

    let client_id = super::extract_client_id(&headers)?;
    let scope = crate::common::build_scope(&user.user_id, &user.role, client_id);

    // Admin scope: use efficient GROUP BY query from storage.
    // User/Agent scope: fetch user's metrics and compute summaries in-memory
    // (avoids adding user_id filter to the storage GROUP BY query for now).
    if scope.user_id().is_none() {
        let summaries = state
            .context
            .metric_service
            .list_group_summaries()
            .await
            .http_context("Failed to list group summaries")?;
        return Ok(Json(summaries.into_iter().map(GroupInfo::from).collect()));
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
        .http_context("Failed to list metrics for groups")?;

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
    let summaries: Vec<GroupInfo> = group_map
        .into_iter()
        .map(|(name, (metric_count, enabled_count))| GroupInfo {
            name: name.unwrap_or_else(|| "default".to_string()),
            metric_count: metric_count as u32,
            enabled_count: enabled_count as u32,
        })
        .collect();

    Ok(Json(summaries))
}

/// List all metrics belonging to a specific group.
///
/// # Path Parameters
/// - `name`: Group name (use "default" for ungrouped metrics)
///
/// # Response
/// Returns JSON array of `MetricInfo` objects for metrics in the group.
/// Returns empty array if no metrics in the group.
pub async fn list_group_metrics(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<AuthenticatedUser>,
    headers: HeaderMap,
    Path(group_name): Path<String>,
) -> Result<Json<Vec<MetricInfo>>, HttpError> {
    info!("REST: list_group_metrics (group={})", group_name);

    let client_id = super::extract_client_id(&headers)?;
    let scope = crate::common::build_scope(&user.user_id, &user.role, client_id);

    let filter = MetricFilter {
        user_id: scope.user_id().map(|s| s.to_string()),
        ..Default::default()
    };
    let (metrics, _) = state
        .context
        .metric_service
        .list_metrics_filtered(&filter, usize::MAX, 0)
        .await
        .http_context("Failed to list metrics")?;

    // Filter by group and convert to proto DTOs
    // Caller decides: skip invalid metrics with logging
    let filtered: Vec<MetricInfo> = metrics
        .iter()
        .filter(|m| {
            let metric_group = m.group.clone().unwrap_or_else(|| "default".to_string());
            metric_group == group_name
        })
        .filter_map(|m| {
            metric_to_rest_response(m)
                .inspect_err(|e| warn!(metric_name = %m.name, "Skipping metric: {}", e))
                .ok()
        })
        .collect();

    Ok(Json(filtered))
}

/// Enable all metrics in a group.
///
/// Enables all metrics belonging to the specified group. Each metric's
/// logpoint is set in the target process via DAP.
///
/// # Path Parameters
/// - `name`: Group name
///
/// # Response
/// Returns `GroupOperationResponse` with success/failure counts and any errors.
pub async fn enable_group(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<AuthenticatedUser>,
    headers: HeaderMap,
    Path(group_name): Path<String>,
) -> Result<Json<GroupOperationResponse>, HttpError> {
    let client_id = super::extract_client_id(&headers)?;
    let scope = crate::common::build_scope(&user.user_id, &user.role, client_id.clone());
    info!(
        "REST: enable_group (group={}, client_id={:?})",
        group_name, client_id
    );

    let result = state
        .context
        .metric_service
        .enable_group(&group_name, &scope)
        .await
        .http_context("Failed to enable group")?;

    Ok(Json(GroupOperationResponse::from_result(
        group_name, "enable", result,
    )))
}

/// Disable all metrics in a group.
///
/// Disables all metrics belonging to the specified group. Each metric's
/// logpoint is removed from the target process via DAP.
///
/// # Path Parameters
/// - `name`: Group name
///
/// # Response
/// Returns `GroupOperationResponse` with success/failure counts and any errors.
pub async fn disable_group(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<AuthenticatedUser>,
    headers: HeaderMap,
    Path(group_name): Path<String>,
) -> Result<Json<GroupOperationResponse>, HttpError> {
    let client_id = super::extract_client_id(&headers)?;
    let scope = crate::common::build_scope(&user.user_id, &user.role, client_id.clone());
    info!(
        "REST: disable_group (group={}, client_id={:?})",
        group_name, client_id
    );

    let result = state
        .context
        .metric_service
        .disable_group(&group_name, &scope)
        .await
        .http_context("Failed to disable group")?;

    Ok(Json(GroupOperationResponse::from_result(
        group_name, "disable", result,
    )))
}
