//! Health and monitoring handlers
//!
//! Endpoints for health checks, liveness probes, and Prometheus metrics.

use crate::constants::status;
use crate::state::ApiState;
use crate::system_metrics::get_process_metrics;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::warn;

/// Health check response (REST-specific, not in proto)
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    /// Service identifier - always "detrix" for this daemon
    pub service: String,
    pub status: String,
    pub uptime_seconds: u64,
    pub adapter_connected: bool,
    /// Daemon's external advertise URL (if configured).
    /// Used by clients to discover how to reach this daemon externally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advertise_url: Option<String>,
}

/// Health check endpoint for monitoring and load balancer probes.
///
/// Returns server health status including uptime and adapter connectivity.
/// Use this endpoint for liveness/readiness checks.
///
/// # Response
/// - `status`: Always "ok" if server is responding
/// - `uptime_seconds`: Time since server started
/// - `adapter_connected`: Whether any DAP adapter is currently connected
pub async fn health_check(State(state): State<Arc<ApiState>>) -> Json<HealthResponse> {
    let uptime = state.start_time.elapsed().as_secs();
    // Check if any adapters are connected via the lifecycle manager
    let adapter_connected = state
        .context
        .adapter_lifecycle_manager
        .has_connected_adapters()
        .await;

    Json(HealthResponse {
        service: "detrix".to_string(),
        status: status::OK.to_string(),
        uptime_seconds: uptime,
        adapter_connected,
        advertise_url: state.advertise_url.clone(),
    })
}

/// Prometheus metrics endpoint
///
/// Exports metrics in Prometheus exposition format for scraping.
/// Endpoint: GET /metrics
pub async fn prometheus_metrics(
    State(state): State<Arc<ApiState>>,
) -> Result<
    (
        StatusCode,
        [(axum::http::header::HeaderName, &'static str); 1],
        String,
    ),
    StatusCode,
> {
    let mut output = String::with_capacity(4096);

    // Get uptime
    let uptime_seconds = state.start_time.elapsed().as_secs();

    // Get process metrics
    let process_metrics = get_process_metrics();

    // Get counts from application (single query, compute both counts)
    let mut degraded_components = 0;
    let (total_metrics_count, active_metrics_count) =
        match state.context.metric_service.list_metrics().await {
            Ok(metrics) => {
                let active = metrics.iter().filter(|m| m.enabled).count();
                (metrics.len(), active)
            }
            Err(e) => {
                warn!(error = %e, "Failed to list metrics for prometheus endpoint");
                degraded_components += 1;
                (0, 0)
            }
        };

    // Get adapter connection status - check if any adapters are connected
    let adapter_connected = state
        .context
        .adapter_lifecycle_manager
        .has_connected_adapters()
        .await;

    // Build Prometheus exposition format output
    // See: https://prometheus.io/docs/instrumenting/exposition_formats/
    // Note: push_str used instead of writeln! to avoid a must_use Result —
    // fmt::Write on String is infallible by definition.

    // Runtime info
    output.push_str("# HELP detrix_info Detrix server information\n");
    output.push_str("# TYPE detrix_info gauge\n");
    output.push_str(&format!(
        "detrix_info{{version=\"{}\"}} 1\n",
        env!("CARGO_PKG_VERSION")
    ));

    // Uptime
    output.push_str("# HELP detrix_uptime_seconds Time since server start\n");
    output.push_str("# TYPE detrix_uptime_seconds counter\n");
    output.push_str(&format!("detrix_uptime_seconds {uptime_seconds}\n"));

    // Active metrics
    output.push_str("# HELP detrix_active_metrics_total Number of currently enabled metrics\n");
    output.push_str("# TYPE detrix_active_metrics_total gauge\n");
    output.push_str(&format!(
        "detrix_active_metrics_total {active_metrics_count}\n"
    ));

    // Total metrics
    output.push_str("# HELP detrix_metrics_total Total number of configured metrics\n");
    output.push_str("# TYPE detrix_metrics_total gauge\n");
    output.push_str(&format!("detrix_metrics_total {total_metrics_count}\n"));

    // Adapter connection status
    output.push_str("# HELP detrix_adapter_connected Whether the DAP adapter is connected\n");
    output.push_str("# TYPE detrix_adapter_connected gauge\n");
    output.push_str(&format!(
        "detrix_adapter_connected {}\n",
        if adapter_connected { 1 } else { 0 }
    ));

    // Process CPU usage
    output.push_str("# HELP detrix_process_cpu_percent Current CPU usage percentage\n");
    output.push_str("# TYPE detrix_process_cpu_percent gauge\n");
    output.push_str(&format!(
        "detrix_process_cpu_percent {:.2}\n",
        process_metrics.cpu_usage_percent
    ));

    // Process memory usage
    output.push_str("# HELP detrix_process_memory_bytes Current memory usage in bytes\n");
    output.push_str("# TYPE detrix_process_memory_bytes gauge\n");
    output.push_str(&format!(
        "detrix_process_memory_bytes {}\n",
        process_metrics.memory_usage_bytes
    ));

    // Degraded components count (0 = healthy)
    output.push_str(
        "# HELP detrix_degraded_components Number of components that failed to report status\n",
    );
    output.push_str("# TYPE detrix_degraded_components gauge\n");
    output.push_str(&format!(
        "detrix_degraded_components {degraded_components}\n"
    ));

    // Return with Prometheus content type
    Ok((
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        output,
    ))
}
