//! Shared system status gathering logic.
//!
//! Provides a protocol-agnostic `SystemStatus` struct and `gather_system_status()` function
//! used by REST, gRPC, and MCP handlers to avoid duplicating status-collection logic.

use crate::mcp_client_tracker::McpClientSummary;
use crate::state::{ApiState, DaemonInfo};
use crate::system_metrics::{get_process_metrics, ProcessMetrics};
use detrix_core::format_uptime;
use std::sync::Arc;
use tracing::warn;

/// Protocol-agnostic system status, gathered once and mapped to protocol-specific DTOs.
#[derive(Debug, Clone)]
pub struct SystemStatus {
    /// "active" or "idle"
    pub mode: &'static str,
    pub uptime_seconds: u64,
    pub uptime_formatted: String,
    pub started_at: String,
    pub adapter_connected: bool,
    pub active_metrics: usize,
    pub total_metrics: usize,
    pub total_events: i64,
    pub active_connections: usize,
    pub total_connections: usize,
    pub process_metrics: ProcessMetrics,
    pub daemon: DaemonInfo,
    pub mcp_clients: Vec<McpClientSummary>,
    /// Components that failed to respond (empty if all healthy)
    pub degraded: Vec<String>,
}

/// Gather comprehensive system status from all services.
pub async fn gather_system_status(state: &Arc<ApiState>) -> SystemStatus {
    use crate::constants::status;

    let mut degraded = Vec::new();

    let uptime_seconds = state.uptime_seconds();

    // Run independent queries concurrently
    let (adapter_connected, metrics_result, connections_result, active_conns_result, events_result) = tokio::join!(
        state
            .context
            .adapter_lifecycle_manager
            .has_connected_adapters(),
        state.context.metric_service.list_metrics(),
        state.context.connection_service.list_connections(),
        state.context.connection_service.list_active_connections(),
        state.event_repository.count_all(),
    );

    let mode = if adapter_connected {
        status::ACTIVE
    } else {
        status::IDLE
    };

    // Process metrics counts
    let (active_metrics, total_metrics) = match metrics_result {
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

    // Process connection counts
    let total_connections = match connections_result {
        Ok(connections) => connections.len(),
        Err(e) => {
            warn!(error = %e, "Failed to list connections for status endpoint");
            degraded.push("connections".to_string());
            0
        }
    };

    let active_connections = match active_conns_result {
        Ok(connections) => connections.len(),
        Err(e) => {
            warn!(error = %e, "Failed to list active connections for status endpoint");
            if !degraded.iter().any(|d| d == "connections") {
                degraded.push("connections".to_string());
            }
            0
        }
    };

    // Process total events count
    let total_events = match events_result {
        Ok(count) => count,
        Err(e) => {
            warn!(error = %e, "Failed to count events for status endpoint");
            degraded.push("events".to_string());
            0
        }
    };

    let process_metrics = get_process_metrics();
    let mcp_clients = state.get_mcp_clients().await;
    let daemon = state.get_daemon_info();
    let started_at = state.started_at();
    let uptime_formatted = format_uptime(uptime_seconds);

    SystemStatus {
        mode,
        uptime_seconds,
        uptime_formatted,
        started_at,
        adapter_connected,
        active_metrics,
        total_metrics,
        total_events,
        active_connections,
        total_connections,
        process_metrics,
        daemon,
        mcp_clients,
        degraded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SystemStatus struct construction test
    #[test]
    fn test_system_status_default_fields() {
        let status = SystemStatus {
            mode: "idle",
            uptime_seconds: 0,
            uptime_formatted: "00:00:00".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            adapter_connected: false,
            active_metrics: 0,
            total_metrics: 0,
            total_events: 0,
            active_connections: 0,
            total_connections: 0,
            process_metrics: ProcessMetrics::default(),
            daemon: DaemonInfo::current(),
            mcp_clients: vec![],
            degraded: vec![],
        };
        assert_eq!(status.mode, "idle");
        assert!(!status.adapter_connected);
        assert!(status.degraded.is_empty());
    }
}
