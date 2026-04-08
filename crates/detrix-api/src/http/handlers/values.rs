//! Metric value and history handlers
//!
//! Endpoints for getting latest metric values and historical data.

use crate::grpc::conversions::core_event_to_proto;
use crate::http::error::{HttpError, ToHttpOption, ToHttpResult};
use crate::http::middleware::AuthenticatedUser;
use crate::state::ApiState;
use crate::types::ProtoMetricEvent;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Extension, Json,
};
use detrix_core::MetricId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::info;

/// Latest metric value response
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricValueResponse {
    pub metric_id: u64,
    pub metric_name: String,
    pub value: serde_json::Value,
    pub timestamp: i64,
    pub is_error: bool,
}

/// Get the most recent captured value for a metric.
///
/// Returns the latest event/value captured for the specified metric.
/// Useful for polling-style integrations or quick value checks.
///
/// # Path Parameters
/// - `id`: Metric ID (integer)
///
/// # Response
/// Returns `MetricValueResponse` with the latest value and metadata.
///
/// # Errors
/// - 404 Not Found: Metric or events not found
pub async fn get_metric_value(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<AuthenticatedUser>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Result<Json<MetricValueResponse>, HttpError> {
    info!("REST: get_metric_value (id={})", id);

    let client_id = super::extract_client_id(&headers)?;
    let scope = user.scope(client_id);

    // Get metric first for its name
    let metric = state
        .context
        .metric_service
        .get_metric(MetricId(id))
        .await
        .http_context("Failed to get metric")?
        .http_not_found(&format!("Metric {}", id))?;

    // Enforce read scope — return not_found to avoid leaking existence
    if !scope.can_read(&metric) {
        return Err(HttpError::not_found(format!("Metric {}", id)));
    }

    // Query latest event
    let events = state
        .context
        .streaming_service
        .query_metric_events(MetricId(id), Some(1), None)
        .await
        .http_context("Failed to query events")?;

    let event = events
        .into_iter()
        .next()
        .http_not_found(&format!("Events for metric {}", id))?;

    // Parse value_json to serde_json::Value
    let value: serde_json::Value =
        serde_json::from_str(event.value_json()).unwrap_or(serde_json::Value::Null);

    Ok(Json(MetricValueResponse {
        metric_id: id,
        metric_name: metric.name,
        value,
        timestamp: event.timestamp,
        is_error: event.is_error,
    }))
}

/// Query parameters for metric history
#[derive(Debug, Deserialize)]
pub struct MetricHistoryParams {
    #[serde(default = "default_history_limit")]
    pub limit: i64,
    pub offset: Option<i64>,
    pub since: Option<i64>,
    pub until: Option<i64>,
}

const DEFAULT_HISTORY_LIMIT: i64 = 100;

fn default_history_limit() -> i64 {
    DEFAULT_HISTORY_LIMIT
}

/// Metric history response
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricHistoryResponse {
    pub metric_id: u64,
    pub metric_name: String,
    pub events: Vec<ProtoMetricEvent>,
    pub total_count: usize,
    pub has_more: bool,
}

/// Get historical events for a metric with pagination.
///
/// Retrieves captured events/values over time for analysis and debugging.
/// Results include introspection data (stack traces, memory snapshots) if captured.
///
/// # Path Parameters
/// - `id`: Metric ID (integer)
///
/// # Query Parameters
/// - `limit`: Max events to return (default: 100)
/// - `offset`: Skip first N events (for pagination)
/// - `since`: Filter events after this timestamp (microseconds since epoch)
/// - `until`: Filter events before this timestamp (microseconds since epoch)
///
/// # Response
/// Returns `MetricHistoryResponse` with events array and pagination info.
///
/// # Errors
/// - 404 Not Found: Metric with given ID does not exist
pub async fn get_metric_history(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<AuthenticatedUser>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Query(params): Query<MetricHistoryParams>,
) -> Result<Json<MetricHistoryResponse>, HttpError> {
    info!(
        "REST: get_metric_history (id={}, limit={}, offset={:?})",
        id, params.limit, params.offset
    );

    let client_id = super::extract_client_id(&headers)?;
    let scope = user.scope(client_id);

    // Get metric first for its name
    let metric = state
        .context
        .metric_service
        .get_metric(MetricId(id))
        .await
        .http_context("Failed to get metric")?
        .http_not_found(&format!("Metric {}", id))?;

    // Enforce read scope — return not_found to avoid leaking existence
    if !scope.can_read(&metric) {
        return Err(HttpError::not_found(format!("Metric {}", id)));
    }

    // Query events with limit + 1 to check has_more
    let limit_plus_one = params.limit + 1;
    let events = state
        .context
        .streaming_service
        .query_metric_events(MetricId(id), Some(limit_plus_one), None)
        .await
        .http_context("Failed to query events")?;

    let has_more = events.len() as i64 > params.limit;

    // Convert to proto MetricEvent
    let events_to_return: Vec<ProtoMetricEvent> = events
        .iter()
        .take(params.limit as usize)
        .map(core_event_to_proto)
        .collect();

    Ok(Json(MetricHistoryResponse {
        metric_id: id,
        metric_name: metric.name,
        total_count: events_to_return.len(),
        events: events_to_return,
        has_more,
    }))
}

/// Drop count response for eBPF ring buffer monitoring
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricDropCountResponse {
    pub metric_id: u64,
    pub metric_name: String,
    pub drop_count: u64,
    /// Human-readable explanation of the drop count
    pub explanation: String,
}

/// Get the ring buffer drop count for an eBPF-based metric.
///
/// Returns the number of events dropped due to ring buffer overflow
/// for the specified metric. Only applicable to eBPF-based adapters
/// (Go on Linux); returns 0 for DAP-based adapters.
///
/// # Path Parameters
/// - `id`: Metric ID (integer)
///
/// # Response
/// Returns `MetricDropCountResponse` with drop count and explanation.
///
/// # Errors
/// - 404 Not Found: Metric not found
pub async fn get_metric_drop_count(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<AuthenticatedUser>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Result<Json<MetricDropCountResponse>, HttpError> {
    info!("REST: get_metric_drop_count (id={})", id);

    // Get metric first
    let metric = state
        .context
        .metric_service
        .get_metric(MetricId(id))
        .await
        .http_context("Failed to get metric")?
        .http_not_found(&format!("Metric {}", id))?;

    // Enforce read scope
    let client_id = super::extract_client_id(&headers)?;
    let scope = user.scope(client_id);
    if !scope.can_read(&metric) {
        return Err(HttpError::not_found(format!("Metric {}", id)));
    }

    // Get drop count from adapter lifecycle manager
    let drop_count = state
        .context
        .adapter_lifecycle_manager
        .get_metric_drop_count(&metric.name)
        .await;

    let explanation = if drop_count == 0 {
        "No events dropped — ring buffer is keeping up".to_string()
    } else {
        format!(
            "{} events dropped due to ring buffer overflow. Consider reducing active logpoints or moving them off hot paths.",
            drop_count
        )
    };

    Ok(Json(MetricDropCountResponse {
        metric_id: id,
        metric_name: metric.name,
        drop_count,
        explanation,
    }))
}
