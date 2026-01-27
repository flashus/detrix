//! Metric operation handlers
//!
//! Endpoints for enabling, disabling, and updating metrics.
//!
//! Uses From trait for clean conversion from service types to proto DTOs.

use super::{metric_to_rest_response, parse_metric_mode, parse_safety_level};
use crate::http::error::{HttpError, ToHttpOption, ToHttpResult};
use crate::state::ApiState;
use crate::types::{MetricInfo, ToggleMetricResponse};
use axum::{
    extract::{Path, State},
    Json,
};
use detrix_application::ToggleMetricResult as ServiceToggleResult;
use detrix_core::MetricId;
use serde::Deserialize;
use std::sync::Arc;
use tracing::info;

/// Create ToggleMetricResponse from service result with metric context
fn toggle_response_from_result(
    metric_id: u64,
    name: String,
    enabled: bool,
    result: ServiceToggleResult,
) -> ToggleMetricResponse {
    ToggleMetricResponse {
        metric_id,
        name,
        enabled,
        status: if enabled { "enabled" } else { "disabled" }.to_string(),
        storage_updated: result.storage_updated,
        dap_confirmed: result.dap_confirmed,
        actual_line: result.actual_line,
        dap_message: result.dap_message,
        metadata: None,
    }
}

/// Enable a metric by ID.
///
/// Sets the metric's logpoint in the target process via DAP. The metric
/// will start capturing events at the specified location.
///
/// # Path Parameters
/// - `id`: Metric ID (integer)
///
/// # Response
/// Returns `ToggleMetricResponse` (proto) with confirmation status including:
/// - `storageUpdated`: Whether the database was updated
/// - `dapConfirmed`: Whether DAP confirmed the logpoint was set
/// - `actualLine`: The actual line where the breakpoint was set (may differ)
///
/// # Errors
/// - 404 Not Found: Metric with given ID does not exist
pub async fn enable_metric(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<u64>,
) -> Result<Json<ToggleMetricResponse>, HttpError> {
    info!("REST: enable_metric (id={})", id);

    let result = state
        .context
        .metric_service
        .toggle_metric(MetricId(id), true)
        .await
        .http_context("Failed to enable metric")?;

    // Get metric to include name in response (parity with gRPC)
    let metric = state
        .context
        .metric_service
        .get_metric(MetricId(id))
        .await
        .http_context("Failed to get metric")?
        .http_not_found(&format!("Metric {}", id))?;

    Ok(Json(toggle_response_from_result(
        id,
        metric.name,
        true,
        result,
    )))
}

/// Disable a metric by ID.
///
/// Removes the metric's logpoint from the target process via DAP. The metric
/// configuration is preserved and can be re-enabled later.
///
/// # Path Parameters
/// - `id`: Metric ID (integer)
///
/// # Response
/// Returns `ToggleMetricResponse` (proto) with confirmation status.
///
/// # Errors
/// - 404 Not Found: Metric with given ID does not exist
pub async fn disable_metric(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<u64>,
) -> Result<Json<ToggleMetricResponse>, HttpError> {
    info!("REST: disable_metric (id={})", id);

    let result = state
        .context
        .metric_service
        .toggle_metric(MetricId(id), false)
        .await
        .http_context("Failed to disable metric")?;

    // Get metric to include name in response (parity with gRPC)
    let metric = state
        .context
        .metric_service
        .get_metric(MetricId(id))
        .await
        .http_context("Failed to get metric")?
        .http_not_found(&format!("Metric {}", id))?;

    Ok(Json(toggle_response_from_result(
        id,
        metric.name,
        false,
        result,
    )))
}

/// Update metric request DTO
/// Uses camelCase to match proto JSON conventions
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMetricRequest {
    pub name: Option<String>,
    pub group: Option<String>,
    pub expression: Option<String>,
    pub enabled: Option<bool>,
    pub mode: Option<String>,
    pub safety_level: Option<String>,
}

/// Update a metric's configuration.
///
/// Partially updates a metric with the provided fields. Omitted fields
/// remain unchanged.
///
/// # Path Parameters
/// - `id`: Metric ID (integer)
///
/// # Request Body (all fields optional)
/// - `name`: New metric name
/// - `group`: New group name
/// - `expression`: New expression to evaluate
/// - `enabled`: Enable/disable the metric
/// - `mode`: Capture mode ('stream', 'first', 'throttle', 'sample')
/// - `safetyLevel`: Safety level ('strict', 'trusted')
///
/// # Response
/// Returns updated `MetricInfo` with all metric details.
///
/// # Errors
/// - 400 Bad Request: Invalid mode or safety level
/// - 404 Not Found: Metric with given ID does not exist
pub async fn update_metric(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<u64>,
    Json(payload): Json<UpdateMetricRequest>,
) -> Result<Json<MetricInfo>, HttpError> {
    info!("REST: update_metric (id={})", id);

    // Get existing metric
    let mut metric = state
        .context
        .metric_service
        .get_metric(MetricId(id))
        .await
        .http_context("Failed to get metric")?
        .http_not_found(&format!("Metric {}", id))?;

    // Apply updates
    if let Some(name) = payload.name {
        metric.name = name;
    }
    if let Some(group) = payload.group {
        metric.group = Some(group);
    }
    if let Some(expression) = payload.expression {
        metric.expression = expression;
    }
    if let Some(enabled) = payload.enabled {
        metric.enabled = enabled;
    }
    if let Some(mode) = payload.mode {
        metric.mode = parse_metric_mode(&mode).http_bad_request()?;
    }
    if let Some(safety_level) = payload.safety_level {
        metric.safety_level = parse_safety_level(&safety_level).http_bad_request()?;
    }

    // Update via service
    state
        .context
        .metric_service
        .update_metric(&metric)
        .await
        .http_context("Failed to update metric")?;

    // Updated metric should have ID - error if missing (database integrity issue)
    Ok(Json(metric_to_rest_response(&metric).http_err()?))
}
