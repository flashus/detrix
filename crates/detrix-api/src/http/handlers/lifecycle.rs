//! Lifecycle handlers
//!
//! Endpoints for wake, sleep, disconnect_all, and status operations.
//!
//! - `wake(app_url, daemon_url?)` — proxy to remote app's control plane
//! - `sleep(app_url)` — proxy to remote app's control plane
//! - `disconnect_all()` — stop all local adapters
//! - `status()` — comprehensive system health

use crate::constants::status;
use crate::http::error::{HttpError, ToHttpResult};
use crate::mcp_client_tracker::McpClientSummary;
use crate::state::{ApiState, DaemonInfo};
use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, instrument};

/// Wake request DTO (REST)
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeRequest {
    pub app_url: String,
    pub daemon_url: Option<String>,
}

/// Wake response DTO (REST)
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeResponse {
    pub status: String,
    pub app_url: String,
    pub connection_id: Option<String>,
    pub debug_port: Option<i32>,
    pub message: String,
}

/// Sleep request DTO (REST)
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SleepRequest {
    pub app_url: String,
}

/// Sleep response DTO (REST)
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SleepResponse {
    pub status: String,
    pub app_url: String,
    pub message: String,
}

/// Disconnect all response DTO (REST)
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisconnectAllResponse {
    pub status: String,
    pub adapters_stopped: u32,
    pub message: String,
}

/// Status response DTO
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub status: String,
    pub uptime_seconds: u64,
    /// Human-readable uptime (e.g., "1d 02:30:45" or "02:30:45")
    pub uptime_formatted: String,
    /// Started timestamp (ISO 8601)
    pub started_at: String,
    pub adapter_connected: bool,
    pub active_metrics: usize,
    pub total_metrics: usize,
    /// Total number of events captured
    pub total_events: i64,
    pub active_connections: usize,
    pub total_connections: usize,
    pub cpu_usage_percent: f32,
    pub memory_usage_bytes: u64,
    /// Daemon process info (PID, parent PID)
    pub daemon: DaemonInfo,
    /// Connected MCP clients
    pub mcp_clients: Vec<McpClientSummary>,
    /// List of degraded components (empty if all healthy)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub degraded: Vec<String>,
}

/// Wake an app's Detrix client via its control plane.
///
/// Sends HTTP POST to `{app_url}/detrix/wake` to start the app's debugger.
#[instrument(skip(state))]
pub async fn wake(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<WakeRequest>,
) -> Result<Json<WakeResponse>, HttpError> {
    info!("REST: wake app_url={}", req.app_url);

    let remote_app_service = state.context.remote_app_service.as_ref().ok_or_else(|| {
        HttpError::with_code(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Remote app control not configured",
            detrix_core::ErrorCode::RemoteAppError,
        )
    })?;

    let result = remote_app_service
        .wake_app(&req.app_url, req.daemon_url.as_deref())
        .await
        .http_err()?;

    Ok(Json(WakeResponse {
        status: result.status,
        app_url: result.app_url,
        connection_id: result.connection_id,
        debug_port: result.debug_port,
        message: format!("Wake request sent to {}", req.app_url),
    }))
}

/// Sleep an app's Detrix client via its control plane.
///
/// Sends HTTP POST to `{app_url}/detrix/sleep` to stop the app's debugger.
#[instrument(skip(state))]
pub async fn sleep(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<SleepRequest>,
) -> Result<Json<SleepResponse>, HttpError> {
    info!("REST: sleep app_url={}", req.app_url);

    let remote_app_service = state.context.remote_app_service.as_ref().ok_or_else(|| {
        HttpError::with_code(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Remote app control not configured",
            detrix_core::ErrorCode::RemoteAppError,
        )
    })?;

    let result = remote_app_service
        .sleep_app(&req.app_url)
        .await
        .http_err()?;

    Ok(Json(SleepResponse {
        status: result.status,
        app_url: result.app_url,
        message: format!("Sleep request sent to {}", req.app_url),
    }))
}

/// Stop all active DAP adapters.
///
/// Gracefully disconnects from all debug adapters. Metrics remain configured
/// and will be re-enabled when connections are re-established.
#[instrument(skip(state))]
pub async fn disconnect_all(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<DisconnectAllResponse>, HttpError> {
    info!("REST: disconnect_all");

    let result = state
        .context
        .adapter_lifecycle_manager
        .disconnect_all()
        .await;

    Ok(Json(DisconnectAllResponse {
        status: if result.partial_failure {
            status::PARTIAL_FAILURE
        } else {
            status::DISCONNECTED
        }
        .to_string(),
        adapters_stopped: result.adapters_stopped as u32,
        message: if result.partial_failure {
            "Some adapters failed to stop"
        } else {
            "All adapters stopped"
        }
        .to_string(),
    }))
}

/// Get comprehensive system status and health information.
///
/// Provides detailed status including resource usage, adapter connectivity,
/// and metric/connection counts. Use this for monitoring dashboards.
///
/// # Response
/// Returns `StatusResponse` with:
/// - `status`: "active" if adapters connected, "idle" otherwise
/// - `uptime_seconds`: Server uptime
/// - `adapter_connected`: Whether any DAP adapter is connected
/// - `active_metrics`/`total_metrics`: Metric counts
/// - `active_connections`/`total_connections`: Connection counts
/// - `cpu_usage_percent`/`memory_usage_bytes`: Process resource usage
/// - `degraded`: List of components that failed to report status
#[instrument(skip(state))]
pub async fn status(State(state): State<Arc<ApiState>>) -> Json<StatusResponse> {
    info!("REST: status");

    let s = crate::system_status::gather_system_status(&state).await;

    Json(StatusResponse {
        status: s.mode.to_string(),
        uptime_seconds: s.uptime_seconds,
        uptime_formatted: s.uptime_formatted,
        started_at: s.started_at,
        adapter_connected: s.adapter_connected,
        active_metrics: s.active_metrics,
        total_metrics: s.total_metrics,
        total_events: s.total_events,
        active_connections: s.active_connections,
        total_connections: s.total_connections,
        cpu_usage_percent: s.process_metrics.cpu_usage_percent,
        memory_usage_bytes: s.process_metrics.memory_usage_bytes,
        daemon: s.daemon,
        mcp_clients: s.mcp_clients,
        degraded: s.degraded,
    })
}
