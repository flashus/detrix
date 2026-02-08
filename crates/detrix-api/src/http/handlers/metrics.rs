//! Metric CRUD handlers
//!
//! Endpoints for creating, reading, updating, and deleting metrics.

use super::{default_mode, metric_to_rest_response};
use crate::constants::status;
use crate::generated::detrix::v1::Location;
use crate::grpc::conversions::core_event_to_proto;
use crate::http::error::{HttpError, ToHttpOption, ToHttpResult};
use crate::state::ApiState;
use crate::types::{MetricInfo, ProtoMetricEvent};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use detrix_application::MetricFilter;
use detrix_config::DEFAULT_QUERY_LIMIT;
use detrix_core::{ConnectionId, MetricId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

/// Query parameters for listing metrics
/// Uses camelCase to match proto JSON conventions
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListMetricsQuery {
    pub connection_id: Option<String>,
    pub enabled: Option<bool>,
    /// Maximum number of metrics to return (default: 100, max: 1000)
    pub limit: Option<usize>,
    /// Number of metrics to skip for pagination (default: 0)
    pub offset: Option<usize>,
}

/// Stack trace slice configuration DTO
/// Can be specified as [head, tail] array or {"full": true}
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StackTraceSliceDto {
    /// [head, tail] - number of frames from top and bottom
    HeadTail(u32, u32),
    /// Full stack trace
    Full { full: bool },
}

/// Request DTO for creating a metric
///
/// NOTE: This is a REST-specific DTO with convenience features like defaults.
/// It must be kept in sync with proto AddMetricRequest.
/// Uses camelCase to match proto JSON conventions.
/// See introspection fields at the end of this struct.
/// Has both Serialize and Deserialize for use in tests.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMetricRequest {
    pub name: String,
    /// Required: Which connection this metric belongs to (no default)
    pub connection_id: String,
    pub group: Option<String>,
    pub location: Location,
    pub expressions: Vec<String>,
    /// Language/adapter type. DEPRECATED: Now derived from connection.
    /// This field is ignored - language is always taken from the connection configuration.
    #[serde(default)]
    pub language: Option<String>,
    /// Whether the metric is enabled. Required - explicitly specify true or false.
    pub enabled: bool,
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Safety level for expression evaluation ('strict', 'trusted').
    /// If not provided, uses the default from SafetyConfig.
    pub safety_level: Option<String>,
    /// If true, replace any existing metric at the same location (file:line).
    /// If false (default), return error when metric already exists at this location.
    #[serde(default)]
    pub replace: bool,

    // Mode-specific configuration (optional, with defaults from detrix-config)
    /// For "sample" mode: capture every Nth hit (default: 10)
    pub sample_rate: Option<u32>,
    /// For "sample_interval" mode: capture every N seconds (default: 30)
    pub sample_interval_seconds: Option<u32>,
    /// For "throttle" mode: max captures per second (default: 100)
    pub max_per_second: Option<u32>,

    // Introspection fields
    /// Enable stack trace capture at this metric point
    #[serde(default)]
    pub capture_stack_trace: bool,
    /// Stack trace TTL in seconds (auto-disable after this time)
    pub stack_trace_ttl: Option<u64>,
    /// Stack trace slice: [head, tail] or "full"
    pub stack_trace_slice: Option<StackTraceSliceDto>,
    /// Enable memory snapshot capture at this metric point
    #[serde(default)]
    pub capture_memory_snapshot: bool,
    /// Scope for memory snapshot: "local", "global", or "both"
    pub snapshot_scope: Option<String>,
    /// Memory snapshot TTL in seconds (auto-disable after this time)
    pub snapshot_ttl: Option<u64>,
}

/// Response DTO for creating a metric
/// Uses proto MetricInfo for the metric field to ensure all fields are included
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMetricResponse {
    pub metric_id: u64,
    pub name: String,
    pub status: String,
    pub metric: MetricInfo,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
}

/// Paginated response for listing metrics
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaginatedMetricsResponse {
    pub metrics: Vec<MetricInfo>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
}

/// Query parameters for listing events
/// Uses camelCase to match proto JSON conventions
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryEventsParams {
    pub metric_id: Option<u64>,
    pub metric_name: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Optional timestamp filter: only return events with timestamp >= this value
    /// Value is in microseconds since Unix epoch
    pub since: Option<i64>,
}

fn default_limit() -> i64 {
    DEFAULT_QUERY_LIMIT
}

/// List all configured metrics with optional filtering and pagination.
///
/// # Query Parameters
/// - `connectionId`: Optional filter by connection ID
/// - `enabled`: Optional filter by enabled status (true/false)
/// - `limit`: Maximum number of metrics to return (default: 100, max: 1000)
/// - `offset`: Number of metrics to skip for pagination (default: 0)
///
/// # Response
/// Returns `PaginatedMetricsResponse` with metrics array and pagination metadata.
/// Metrics that fail conversion are silently skipped with a warning logged.
pub async fn list_metrics(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<ListMetricsQuery>,
) -> Result<Json<PaginatedMetricsResponse>, HttpError> {
    // Use configured defaults for pagination (from ConfigService for hot-reload support)
    let config = state.config_service.get_config().await;
    let default_limit = config.api.default_query_limit as usize;
    let max_limit = config.api.max_query_limit as usize;

    let limit = params.limit.unwrap_or(default_limit).min(max_limit);
    let offset = params.offset.unwrap_or(0);

    info!(
        "REST: list_metrics (connection_id={:?}, enabled={:?}, limit={}, offset={})",
        params.connection_id, params.enabled, limit, offset
    );

    // Build filter for DB-level filtering (PERF-02 N+1 fix)
    let filter = MetricFilter {
        connection_id: params.connection_id.map(ConnectionId),
        enabled: params.enabled,
        group: None,
    };

    // Use DB-level filtering for efficient queries
    let (metrics, total) = state
        .context
        .metric_service
        .list_metrics_filtered(&filter, limit, offset)
        .await
        .http_context("Failed to list metrics")?;

    // Convert to DTOs
    let dtos: Vec<MetricInfo> = metrics
        .iter()
        .filter_map(|m| {
            metric_to_rest_response(m)
                .inspect_err(|e| warn!(metric_name = %m.name, "Skipping metric: {}", e))
                .ok()
        })
        .collect();

    info!(
        "Returning {} of {} metrics (offset={})",
        dtos.len(),
        total,
        offset
    );

    Ok(Json(PaginatedMetricsResponse {
        metrics: dtos,
        total,
        limit,
        offset,
    }))
}

/// Get a single metric by its numeric ID.
///
/// # Path Parameters
/// - `id`: Metric ID (integer)
///
/// # Response
/// Returns `MetricInfo` with full metric details including location, expression,
/// mode, and introspection settings.
///
/// # Errors
/// - 404 Not Found: Metric with given ID does not exist
pub async fn get_metric(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<u64>,
) -> Result<Json<MetricInfo>, HttpError> {
    info!("REST: get_metric (id={})", id);

    let metric = state
        .context
        .metric_service
        .get_metric(MetricId(id))
        .await
        .http_context("Failed to get metric")?
        .http_not_found(&format!("Metric {}", id))?;

    // Single metric should always have ID - error if missing (database integrity issue)
    Ok(Json(metric_to_rest_response(&metric).http_err()?))
}

/// Create a new metric and set it as a logpoint in the target process.
///
/// # Request Body
/// - `name`: Unique metric name
/// - `connectionId`: Connection ID for the target process (required)
/// - `location`: `{ file, line }` - Source file and line number
/// - `expression`: Expression to evaluate at the observation point
/// - `language`: DEPRECATED - Language is now derived from the connection
/// - `enabled`: Whether to enable immediately (required, no default)
/// - `mode`: Capture mode ('stream', 'first', 'throttle', 'sample') - default: 'stream'
/// - `safetyLevel`: Expression safety level ('strict', 'trusted') - default: from SafetyConfig
/// - `replace`: If true, replace existing metric at same location - default: false
/// - `captureStackTrace`: Enable stack trace capture - default: false
/// - `captureMemorySnapshot`: Enable variable snapshot capture - default: false
///
/// # Response
/// Returns 201 Created with `CreateMetricResponse` containing the new metric ID,
/// full metric details, and any warnings.
///
/// # Errors
/// - 400 Bad Request: Invalid metric configuration or expression
/// - 409 Conflict: Metric already exists at location (when `replace=false`)
pub async fn add_metric(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<CreateMetricRequest>,
) -> Result<(StatusCode, Json<CreateMetricResponse>), HttpError> {
    use crate::grpc::conversions::add_request_to_metric;
    use crate::http::proto_adapters::rest_request_to_add_metric_request;

    info!(
        "REST: add_metric (name={}, file={}:{})",
        payload.name, payload.location.file, payload.location.line
    );

    // Get connection to derive language (same pattern as MCP)
    let conn_id = detrix_core::ConnectionId(payload.connection_id.clone());
    let connection = state
        .context
        .connection_service
        .get_connection(&conn_id)
        .await
        .http_context("Failed to get connection")?
        .http_not_found("Connection")?;

    // Resolve safety level: use provided value or default from config
    let safety_level = match &payload.safety_level {
        Some(level) => level.clone(),
        None => {
            let config = state.config_service.get_config().await;
            config.safety.default_safety_level().as_str().to_string()
        }
    };

    // 1. Convert REST DTO → Proto AddMetricRequest (thin adapter)
    // Use connection's language (ignore payload.language for consistency with MCP)
    let proto_request =
        rest_request_to_add_metric_request(&payload, &safety_level, connection.language);

    // 2. Convert Proto → Core Metric (shared conversion from grpc/conversions.rs)
    let metric =
        add_request_to_metric(&proto_request).map_err(|e| HttpError::bad_request(e.to_string()))?;

    // Call service (ALL business logic happens here)
    let outcome = state
        .context
        .metric_service
        .add_metric(metric.clone(), payload.replace)
        .await
        .http_context("Failed to add metric")?;

    // Log any warnings
    for warning in &outcome.warnings {
        warn!("{}", warning);
    }

    // Fetch the created metric to return full details
    let created_metric = state
        .context
        .metric_service
        .get_metric(outcome.value)
        .await
        .http_context("Metric created but could not be retrieved")?
        .http_not_found("Metric after creation")?;

    // Just created metric should have ID - error if missing (database integrity issue)
    Ok((
        StatusCode::CREATED,
        Json(CreateMetricResponse {
            metric_id: outcome.value.0,
            name: payload.name,
            status: status::CREATED.to_string(),
            metric: metric_to_rest_response(&created_metric).http_err()?,
            warnings: outcome.warnings.iter().map(|w| w.to_string()).collect(),
        }),
    ))
}

/// Delete a metric by ID.
///
/// Removes the metric from storage and clears the associated logpoint
/// from the target process if one was set.
///
/// # Path Parameters
/// - `id`: Metric ID (integer)
///
/// # Response
/// Returns 204 No Content on success.
///
/// # Errors
/// - 404 Not Found: Metric with given ID does not exist
pub async fn delete_metric(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<u64>,
) -> Result<StatusCode, HttpError> {
    info!("REST: delete_metric (id={})", id);

    state
        .context
        .metric_service
        .remove_metric(MetricId(id))
        .await
        .http_context("Failed to delete metric")?;

    Ok(StatusCode::NO_CONTENT)
}

/// Query events endpoint
///
/// Query captured metric events. Filter by metric_id or metric_name.
/// Returns recent events if no filter specified.
/// Returns proto MetricEvent which includes introspection data (stack_trace, memory_snapshot).
///
/// When `since` is not provided and a specific metric is queried, events are
/// automatically scoped to the metric's creation time to avoid returning stale
/// data from previous sessions. Pass `since=0` to explicitly request all events.
pub async fn query_events(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<QueryEventsParams>,
) -> Result<Json<Vec<ProtoMetricEvent>>, HttpError> {
    info!(
        "REST: query_events (metric_id={:?}, metric_name={:?}, limit={}, since={:?})",
        params.metric_id, params.metric_name, params.limit, params.since
    );

    // If metric_name provided, look up the metric ID first
    let metric_id = match (&params.metric_id, &params.metric_name) {
        (Some(id), _) => Some(MetricId(*id)),
        (None, Some(name)) => {
            // Look up by name via MetricService
            let metric = state
                .context
                .metric_service
                .get_metric_by_name(name)
                .await
                .http_context("Failed to look up metric")?
                .http_not_found(&format!("Metric {}", name))?;
            metric.id
        }
        (None, None) => None,
    };

    // Query events via StreamingService (Clean Architecture - delegate to application layer)
    let streaming_service = &state.context.streaming_service;
    let events = match metric_id {
        Some(id) => streaming_service
            .query_metric_events(id, Some(params.limit), params.since)
            .await
            .http_context("Failed to query events")?,
        None => {
            // Return recent events from all metrics
            // Note: since filter not applied to "all events" query for simplicity
            streaming_service
                .query_all_events(Some(params.limit))
                .await
                .http_context("Failed to query recent events")?
        }
    };

    // Convert to proto MetricEvent
    let dtos: Vec<ProtoMetricEvent> = events.iter().map(core_event_to_proto).collect();

    info!("Returning {} events", dtos.len());
    Ok(Json(dtos))
}
