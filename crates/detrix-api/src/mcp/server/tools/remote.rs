//! Remote app control and adapter disconnect tools.
//!
//! - wake/sleep: proxy to remote app's control plane via RemoteAppService
//! - disconnect_all: stop all local adapters

use crate::constants::status;
use crate::mcp::error::ToMcpResult;
use crate::mcp::params::{DisconnectAllParams, SleepParams, WakeParams};
use crate::state::ApiState;
use rmcp::ErrorData as McpError;
use std::sync::Arc;
use tracing::info;

/// Result of wake operation
pub struct WakeResult {
    pub app_url: String,
    pub status: String,
    pub connection_id: Option<String>,
    pub debug_port: Option<i32>,
    /// Daemon URL advertised by the app (from daemon's advertise_url).
    /// The MCP bridge uses this to auto-switch to the correct daemon.
    pub daemon_url: Option<String>,
}

impl WakeResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        let mut msg = match &self.connection_id {
            Some(conn_id) => format!(
                "App at {} woke successfully (connection_id: {}, debug_port: {})",
                self.app_url,
                conn_id,
                self.debug_port.unwrap_or(0)
            ),
            None => format!(
                "Wake request sent to {} (status: {})",
                self.app_url, self.status
            ),
        };
        if let Some(ref daemon_url) = self.daemon_url {
            msg.push_str(&format!(", daemon_url: {}", daemon_url));
        }
        msg
    }

    /// Build JSON response for MCP
    pub fn build_json(&self) -> serde_json::Value {
        serde_json::json!({
            "app_url": self.app_url,
            "status": self.status,
            "connection_id": self.connection_id,
            "debug_port": self.debug_port,
            "daemon_url": self.daemon_url,
        })
    }
}

/// Core wake implementation — delegates to RemoteAppService.
pub async fn wake_impl(state: &Arc<ApiState>, params: WakeParams) -> Result<WakeResult, McpError> {
    let app_url = params.app_url.trim_end_matches('/').to_string();

    let remote_app_service = state.context.remote_app_service.as_ref().ok_or_else(|| {
        McpError::internal_error("Remote app control not configured".to_string(), None)
    })?;

    info!("wake: Sending wake request to {}", app_url);

    let result = remote_app_service
        .wake_app(&app_url, params.daemon_url.as_deref())
        .await
        .mcp_context("Remote app operation failed")?;

    // Set control_plane_url on the connection so VFS can fetch files from the app
    if let Some(ref conn_id) = result.connection_id {
        let _ = state
            .context
            .connection_service
            .set_control_plane_url(&detrix_core::ConnectionId::new(conn_id), app_url.clone())
            .await;
    }

    Ok(WakeResult {
        app_url: result.app_url,
        status: result.status,
        connection_id: result.connection_id,
        debug_port: result.debug_port,
        daemon_url: result.daemon_url,
    })
}

// ============================================================================
// Sleep Implementation
// ============================================================================

/// Result of sleep operation
pub struct SleepResult {
    pub app_url: String,
    pub status: String,
}

impl SleepResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "App at {} is now sleeping (status: {})",
            self.app_url, self.status
        )
    }

    /// Build JSON response for MCP
    pub fn build_json(&self) -> serde_json::Value {
        serde_json::json!({
            "app_url": self.app_url,
            "status": self.status,
        })
    }
}

/// Core sleep implementation — delegates to RemoteAppService.
pub async fn sleep_impl(
    state: &Arc<ApiState>,
    params: SleepParams,
) -> Result<SleepResult, McpError> {
    let app_url = params.app_url.trim_end_matches('/').to_string();

    let remote_app_service = state.context.remote_app_service.as_ref().ok_or_else(|| {
        McpError::internal_error("Remote app control not configured".to_string(), None)
    })?;

    info!("sleep: Sending sleep request to {}", app_url);

    let result = remote_app_service
        .sleep_app(&app_url)
        .await
        .mcp_context("Remote app operation failed")?;

    Ok(SleepResult {
        app_url: result.app_url,
        status: result.status,
    })
}

// ============================================================================
// Disconnect All Implementation
// ============================================================================

/// Result of disconnect_all operation
pub struct DisconnectAllResult {
    pub status: String,
    pub adapters_stopped: u32,
}

impl DisconnectAllResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "All adapters stopped (status: {}, adapters_stopped: {})",
            self.status, self.adapters_stopped
        )
    }

    /// Build JSON response for MCP
    pub fn build_json(&self) -> serde_json::Value {
        serde_json::json!({
            "status": self.status,
            "adapters_stopped": self.adapters_stopped,
        })
    }
}

/// Disconnect all local adapters.
pub async fn disconnect_all_impl(
    state: &Arc<ApiState>,
    _params: DisconnectAllParams,
) -> Result<DisconnectAllResult, McpError> {
    let result = state
        .context
        .adapter_lifecycle_manager
        .disconnect_all()
        .await;

    Ok(DisconnectAllResult {
        status: if result.partial_failure {
            status::PARTIAL_FAILURE
        } else {
            status::DISCONNECTED
        }
        .to_string(),
        adapters_stopped: result.adapters_stopped as u32,
    })
}
