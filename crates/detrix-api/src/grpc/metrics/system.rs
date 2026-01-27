//! System state handlers: wake, sleep, get_status

use crate::constants::status;
use crate::error::ToStatusResult;
use crate::generated::detrix::v1::{
    DaemonInfo, McpClientInfo, ParentProcessInfo, SleepRequest, SleepResponse, StatusRequest,
    StatusResponse, WakeRequest, WakeResponse,
};
use crate::state::ApiState;
use detrix_core::format_uptime;
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Handle wake request
pub async fn handle_wake(
    state: &Arc<ApiState>,
    _request: Request<WakeRequest>,
) -> Result<Response<WakeResponse>, Status> {
    // With connection-based approach, wake is a no-op at the global level.
    // Metrics are synced when connections are created via ConnectionService.
    // This endpoint returns current status for backwards compatibility.
    let adapter_count = state
        .context
        .adapter_lifecycle_manager
        .adapter_count()
        .await;

    let metrics = state
        .context
        .metric_service
        .list_metrics()
        .await
        .to_status()?;

    let enabled_count = metrics.iter().filter(|m| m.enabled).count();

    Ok(Response::new(WakeResponse {
        status: if adapter_count > 0 {
            "active"
        } else {
            "no_connections"
        }
        .to_string(),
        metrics_loaded: enabled_count as u32,
        metadata: None,
    }))
}

/// Handle sleep request
pub async fn handle_sleep(
    state: &Arc<ApiState>,
    _request: Request<SleepRequest>,
) -> Result<Response<SleepResponse>, Status> {
    // With connection-based approach, sleep stops all adapters via lifecycle manager.
    // This provides a way to cleanly disconnect all debugger connections.
    let adapter_count = state
        .context
        .adapter_lifecycle_manager
        .adapter_count()
        .await;

    // Stop all adapters - track partial failure for status
    let partial_failure = match state.context.adapter_lifecycle_manager.stop_all().await {
        Ok(_) => false,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to stop all adapters during sleep");
            true
        }
    };

    // Indicate partial failure in status if some adapters failed to stop
    let status_str = if partial_failure {
        "sleeping_partial_failure".to_string()
    } else {
        "sleeping".to_string()
    };

    let message = if partial_failure {
        "Some adapters failed to stop".to_string()
    } else {
        "All adapters stopped".to_string()
    };

    Ok(Response::new(SleepResponse {
        status: status_str,
        metrics_saved: adapter_count as u32,
        metadata: None,
        message,
    }))
}

/// Handle get_status request
pub async fn handle_get_status(
    state: &Arc<ApiState>,
    _request: Request<StatusRequest>,
) -> Result<Response<StatusResponse>, Status> {
    let mut degraded = Vec::new();

    // Query all metrics to count active/total
    let (enabled_metrics, total_metrics) = match state.context.metric_service.list_metrics().await {
        Ok(metrics) => {
            let enabled = metrics.iter().filter(|m| m.enabled).count() as u32;
            (enabled, metrics.len() as u32)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list metrics for status endpoint");
            degraded.push("metrics".to_string());
            (0, 0)
        }
    };

    // Query total events count from event repository
    let total_events = match state.event_repository.count_all().await {
        Ok(count) => count as u64,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count events for status endpoint");
            degraded.push("events".to_string());
            0
        }
    };

    // Get connection counts
    let total_connections = match state.context.connection_service.list_connections().await {
        Ok(connections) => connections.len() as u32,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list connections for status endpoint");
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
        Ok(connections) => connections.len() as u32,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list active connections for status endpoint");
            if !degraded.contains(&"connections".to_string()) {
                degraded.push("connections".to_string());
            }
            0
        }
    };

    // Get uptime
    let uptime_seconds = state.uptime_seconds() as i64;
    let uptime_formatted = format_uptime(uptime_seconds as u64);

    // Determine mode based on whether any adapters are connected
    let adapter_connected = state
        .context
        .adapter_lifecycle_manager
        .has_connected_adapters()
        .await;
    let mode = if adapter_connected {
        status::ACTIVE.to_string()
    } else {
        "idle".to_string()
    };

    // Get system metrics (CPU and memory for current process)
    let process_metrics = crate::system_metrics::get_process_metrics();

    // Get MCP clients
    let mcp_clients: Vec<McpClientInfo> = state
        .get_mcp_clients()
        .await
        .into_iter()
        .map(|c| McpClientInfo {
            id: c.id,
            connected_at: c.connected_at,
            connected_duration_secs: c.connected_duration_secs,
            last_activity: c.last_activity,
            last_activity_ago_secs: c.last_activity_ago_secs,
            parent_process: c.parent_process.map(|pp| ParentProcessInfo {
                pid: pp.pid,
                name: pp.name,
                bridge_pid: pp.bridge_pid,
            }),
        })
        .collect();

    // Get daemon info
    let daemon_info = state.get_daemon_info();

    // Get started timestamp
    let started_at = state.started_at();

    // Get config path
    let config_path = state
        .config_service
        .config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    Ok(Response::new(StatusResponse {
        mode,
        enabled_metrics,
        uptime_seconds: uptime_seconds as u64,
        total_events,
        cpu_usage_percent: process_metrics.cpu_usage_percent as f64,
        memory_usage_bytes: process_metrics.memory_usage_bytes,
        metadata: None,
        // Extended fields
        uptime_formatted,
        started_at,
        total_metrics,
        active_connections,
        total_connections,
        adapter_connected,
        daemon: Some(DaemonInfo {
            pid: daemon_info.pid,
            ppid: daemon_info.ppid,
        }),
        mcp_clients,
        degraded,
        config_path,
    }))
}
