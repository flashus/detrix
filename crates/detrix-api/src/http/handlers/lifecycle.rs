//! Lifecycle handlers
//!
//! Endpoints for wake, sleep, and status operations.

use super::connection_to_rest_response;
use crate::constants::status;
use crate::http::error::{HttpError, ToHttpResult};
use crate::mcp_client_tracker::McpClientSummary;
use crate::state::{ApiState, DaemonInfo};
use crate::system_metrics::get_process_metrics;
use crate::types::{ConnectionInfo, SleepResponse};
use axum::{extract::State, Json};
use detrix_core::format_uptime;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, instrument, warn};

/// Wake response DTO
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeResponse {
    pub status: String,
    pub message: String,
    pub connections: Vec<ConnectionInfo>,
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

/// Wake endpoint - check status and list active connections
///
/// Adapters are started automatically when connections are created,
/// so this endpoint primarily reports current status.
#[instrument(skip(state))]
pub async fn wake(State(state): State<Arc<ApiState>>) -> Result<Json<WakeResponse>, HttpError> {
    info!("REST: wake");

    // Get active connections
    let connections = state
        .context
        .connection_service
        .list_active_connections()
        .await
        .http_context("Failed to list connections")?;

    // Convert to proto ConnectionInfo
    let connection_dtos: Vec<ConnectionInfo> = connections
        .iter()
        .map(connection_to_rest_response)
        .collect();
    let adapter_connected = state
        .context
        .adapter_lifecycle_manager
        .has_connected_adapters()
        .await;

    let status = if adapter_connected { "awake" } else { "idle" };
    let message = if connection_dtos.is_empty() {
        "No active connections. Create a connection to start observing.".to_string()
    } else {
        format!("{} active connection(s)", connection_dtos.len())
    };

    Ok(Json(WakeResponse {
        status: status.to_string(),
        message,
        connections: connection_dtos,
    }))
}

/// Stop all active DAP adapters and enter sleep mode.
///
/// Gracefully disconnects from all debug adapters. Metrics remain configured
/// and will be re-enabled when connections are re-established.
///
/// # Response
/// Returns `SleepResponse` (proto) with status "sleeping" on success.
#[instrument(skip(state))]
pub async fn sleep(State(state): State<Arc<ApiState>>) -> Result<Json<SleepResponse>, HttpError> {
    info!("REST: sleep");

    // Stop all adapters
    state
        .context
        .adapter_lifecycle_manager
        .stop_all()
        .await
        .http_context("Failed to sleep")?;

    Ok(Json(SleepResponse {
        status: status::SLEEPING.to_string(),
        message: "All adapters stopped".to_string(),
        metrics_saved: 0, // REST doesn't track this
        metadata: None,
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

    let mut degraded = Vec::new();

    let uptime_seconds = state.start_time.elapsed().as_secs();
    let adapter_connected = state
        .context
        .adapter_lifecycle_manager
        .has_connected_adapters()
        .await;

    // Get metrics counts
    let (active_metrics, total_metrics) = match state.context.metric_service.list_metrics().await {
        Ok(metrics) => {
            let active = metrics.iter().filter(|m| m.enabled).count();
            (active, metrics.len())
        }
        Err(e) => {
            warn!(error = %e, "Failed to list metrics for status endpoint");
            degraded.push("metrics".to_string());
            (0, 0)
        }
    };

    // Get connection counts
    let total_connections = match state.context.connection_service.list_connections().await {
        Ok(connections) => connections.len(),
        Err(e) => {
            warn!(error = %e, "Failed to list connections for status endpoint");
            degraded.push("connections".to_string());
            0
        }
    };

    let active_connections = match state
        .context
        .connection_service
        .list_active_connections()
        .await
    {
        Ok(connections) => connections.len(),
        Err(e) => {
            warn!(error = %e, "Failed to list active connections for status endpoint");
            if !degraded.contains(&"connections".to_string()) {
                degraded.push("connections".to_string());
            }
            0
        }
    };

    // Get total events count
    let total_events = match state.event_repository.count_all().await {
        Ok(count) => count,
        Err(e) => {
            warn!(error = %e, "Failed to count events for status endpoint");
            degraded.push("events".to_string());
            0
        }
    };

    // Get process metrics
    let process_metrics = get_process_metrics();

    // Get MCP clients
    let mcp_clients = state.get_mcp_clients().await;

    // Get daemon info
    let daemon = state.get_daemon_info();

    // Get started timestamp
    let started_at = state.started_at();

    Json(StatusResponse {
        status: if adapter_connected {
            status::ACTIVE.to_string()
        } else {
            "idle".to_string()
        },
        uptime_seconds,
        uptime_formatted: format_uptime(uptime_seconds),
        started_at,
        adapter_connected,
        active_metrics,
        total_metrics,
        total_events,
        active_connections,
        total_connections,
        cpu_usage_percent: process_metrics.cpu_usage_percent,
        memory_usage_bytes: process_metrics.memory_usage_bytes,
        daemon,
        mcp_clients,
        degraded,
    })
}
