//! Status tools: wake, sleep
//!
//! These tools manage Detrix daemon lifecycle and status.

use crate::mcp::error::ToMcpResult;
use crate::state::ApiState;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

// ============================================================================
// wake Implementation
// ============================================================================

/// Result of wake operation
pub struct WakeResult {
    pub adapter_count: usize,
    pub enabled_count: usize,
    pub status: WakeStatus,
}

/// Wake status classification
pub enum WakeStatus {
    Active,
    NoConnections,
}

impl WakeResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        let status_str = match self.status {
            WakeStatus::Active => "active",
            WakeStatus::NoConnections => "no_connections",
        };

        format!(
            "✓ Detrix daemon is running\n\
             Status: {} | {} active connection(s) | {} enabled metric(s)\n\
             Tip: Use 'create_connection' to connect to a debugger process.",
            status_str, self.adapter_count, self.enabled_count
        )
    }
}

/// Core wake implementation
pub async fn wake_impl(state: &Arc<ApiState>) -> Result<WakeResult, McpError> {
    // With connection-based approach, wake is informational - shows current status
    // Note: The MCP bridge handles daemon auto-start before this tool is even called
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
        .mcp_context("Failed to list metrics")?;

    let enabled_count = metrics.iter().filter(|m| m.enabled).count();

    let status = if adapter_count > 0 {
        WakeStatus::Active
    } else {
        WakeStatus::NoConnections
    };

    Ok(WakeResult {
        adapter_count,
        enabled_count,
        status,
    })
}

// ============================================================================
// sleep Implementation
// ============================================================================

/// Result of sleep operation
pub struct SleepResult {
    pub closed_connections: usize,
}

impl SleepResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "😴 Detrix is now sleeping. Closed {} connection(s). Zero overhead until create_connection() is called.",
            self.closed_connections
        )
    }
}

/// Core sleep implementation
pub async fn sleep_impl(state: &Arc<ApiState>) -> SleepResult {
    // Get current adapter count before stopping
    let adapter_count = state
        .context
        .adapter_lifecycle_manager
        .adapter_count()
        .await;

    // Stop all adapters via lifecycle manager
    if let Err(e) = state.context.adapter_lifecycle_manager.stop_all().await {
        tracing::warn!("Failed to stop all adapters during sleep: {}", e);
    }

    // Update MCP tracker - no active connections after sleep
    state.mcp_client_tracker.update_connection_count(0);

    SleepResult {
        closed_connections: adapter_count,
    }
}
