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
    use std::fmt::Write;

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

    // Runtime info
    let _ = writeln!(output, "# HELP detrix_info Detrix server information");
    let _ = writeln!(output, "# TYPE detrix_info gauge");
    let _ = writeln!(
        output,
        "detrix_info{{version=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION")
    );

    // Uptime
    let _ = writeln!(
        output,
        "# HELP detrix_uptime_seconds Time since server start"
    );
    let _ = writeln!(output, "# TYPE detrix_uptime_seconds counter");
    let _ = writeln!(output, "detrix_uptime_seconds {}", uptime_seconds);

    // Active metrics
    let _ = writeln!(
        output,
        "# HELP detrix_active_metrics_total Number of currently enabled metrics"
    );
    let _ = writeln!(output, "# TYPE detrix_active_metrics_total gauge");
    let _ = writeln!(
        output,
        "detrix_active_metrics_total {}",
        active_metrics_count
    );

    // Total metrics
    let _ = writeln!(
        output,
        "# HELP detrix_metrics_total Total number of configured metrics"
    );
    let _ = writeln!(output, "# TYPE detrix_metrics_total gauge");
    let _ = writeln!(output, "detrix_metrics_total {}", total_metrics_count);

    // Adapter connection status
    let _ = writeln!(
        output,
        "# HELP detrix_adapter_connected Whether the DAP adapter is connected"
    );
    let _ = writeln!(output, "# TYPE detrix_adapter_connected gauge");
    let _ = writeln!(
        output,
        "detrix_adapter_connected {}",
        if adapter_connected { 1 } else { 0 }
    );

    // Process CPU usage
    let _ = writeln!(
        output,
        "# HELP detrix_process_cpu_percent Current CPU usage percentage"
    );
    let _ = writeln!(output, "# TYPE detrix_process_cpu_percent gauge");
    let _ = writeln!(
        output,
        "detrix_process_cpu_percent {:.2}",
        process_metrics.cpu_usage_percent
    );

    // Process memory usage
    let _ = writeln!(
        output,
        "# HELP detrix_process_memory_bytes Current memory usage in bytes"
    );
    let _ = writeln!(output, "# TYPE detrix_process_memory_bytes gauge");
    let _ = writeln!(
        output,
        "detrix_process_memory_bytes {}",
        process_metrics.memory_usage_bytes
    );

    // Degraded components count (0 = healthy)
    let _ = writeln!(
        output,
        "# HELP detrix_degraded_components Number of components that failed to report status"
    );
    let _ = writeln!(output, "# TYPE detrix_degraded_components gauge");
    let _ = writeln!(output, "detrix_degraded_components {}", degraded_components);

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
