//! System state handlers: wake, sleep, disconnect_all, get_status

use crate::constants::status;
use crate::error::ToStatusResult;
use crate::generated::detrix::v1::{
    DaemonInfo, DisconnectAllRequest, DisconnectAllResponse, McpClientInfo, ParentProcessInfo,
    SleepRequest, SleepResponse, StatusRequest, StatusResponse, WakeRequest, WakeResponse,
};
use crate::state::ApiState;
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Handle wake request — proxy to remote app's control plane
pub async fn handle_wake(
    state: &Arc<ApiState>,
    request: Request<WakeRequest>,
) -> Result<Response<WakeResponse>, Status> {
    let client_id = crate::grpc::extract_client_id(&request)?;
    let req = request.into_inner();
    tracing::info!(app_url = %req.app_url, ?client_id, "gRPC: wake");
    let app_url = req.app_url;
    let daemon_url = if req.daemon_url.is_empty() {
        None
    } else {
        Some(req.daemon_url.as_str())
    };

    let remote_app_service = state
        .context
        .remote_app_service
        .as_ref()
        .ok_or_else(|| Status::unavailable("Remote app control not configured"))?;

    let result = remote_app_service
        .wake_app(&app_url, daemon_url)
        .await
        .to_status()?;

    Ok(Response::new(WakeResponse {
        status: result.status,
        app_url: result.app_url,
        connection_id: result.connection_id.unwrap_or_default(),
        debug_port: result.debug_port.unwrap_or(0),
        message: format!("Wake request sent to {}", app_url),
        metadata: None,
        daemon_url: None, // TODO: Will be populated in Phase 5 from config
    }))
}

/// Handle sleep request — proxy to remote app's control plane
pub async fn handle_sleep(
    state: &Arc<ApiState>,
    request: Request<SleepRequest>,
) -> Result<Response<SleepResponse>, Status> {
    let client_id = crate::grpc::extract_client_id(&request)?;
    let req = request.into_inner();
    tracing::info!(app_url = %req.app_url, ?client_id, "gRPC: sleep");
    let app_url = req.app_url;

    let remote_app_service = state
        .context
        .remote_app_service
        .as_ref()
        .ok_or_else(|| Status::unavailable("Remote app control not configured"))?;

    let result = remote_app_service.sleep_app(&app_url).await.to_status()?;

    Ok(Response::new(SleepResponse {
        status: result.status,
        app_url: result.app_url,
        message: format!("Sleep request sent to {}", app_url),
        metadata: None,
    }))
}

/// Handle disconnect_all request — stop all local adapters
pub async fn handle_disconnect_all(
    state: &Arc<ApiState>,
    request: Request<DisconnectAllRequest>,
) -> Result<Response<DisconnectAllResponse>, Status> {
    let client_id = crate::grpc::extract_client_id(&request)?;
    let _req = request.into_inner();
    tracing::info!(?client_id, "gRPC: disconnect_all");
    let result = state
        .context
        .adapter_lifecycle_manager
        .disconnect_all()
        .await;

    Ok(Response::new(DisconnectAllResponse {
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
        metadata: None,
    }))
}

/// Handle get_status request
pub async fn handle_get_status(
    state: &Arc<ApiState>,
    _request: Request<StatusRequest>,
) -> Result<Response<StatusResponse>, Status> {
    let s = crate::system_status::gather_system_status(state).await;

    // Map shared McpClientSummary → proto McpClientInfo
    let mcp_clients: Vec<McpClientInfo> = s
        .mcp_clients
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

    let config_path = state
        .config_service
        .config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    Ok(Response::new(StatusResponse {
        mode: s.mode.to_string(),
        enabled_metrics: s.active_metrics as u32,
        uptime_seconds: s.uptime_seconds,
        total_events: s.total_events as u64,
        cpu_usage_percent: s.process_metrics.cpu_usage_percent as f64,
        memory_usage_bytes: s.process_metrics.memory_usage_bytes,
        metadata: None,
        uptime_formatted: s.uptime_formatted,
        started_at: s.started_at,
        total_metrics: s.total_metrics as u32,
        active_connections: s.active_connections as u32,
        total_connections: s.total_connections as u32,
        adapter_connected: s.adapter_connected,
        daemon: Some(DaemonInfo {
            pid: s.daemon.pid,
            ppid: s.daemon.ppid,
        }),
        mcp_clients,
        degraded: s.degraded,
        config_path,
    }))
}
