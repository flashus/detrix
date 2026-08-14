//! Prometheus metrics HTTP server.

use axum::{extract::State, http::StatusCode, response::Html, routing::get, Router};
use detrix_logging::info;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Shared metrics state exposed via /metrics endpoint.
#[derive(Clone)]
pub struct MetricsState {
    pub events_received: Arc<AtomicU64>,
    pub events_in_flight: Arc<AtomicU64>,
    pub events_forwarded: Arc<AtomicU64>,
    pub events_dropped: Arc<AtomicU64>,
    pub events_decoded: Arc<AtomicU64>,
    pub kernel_events_dropped: Arc<AtomicU64>,
    pub decode_events_dropped: Arc<AtomicU64>,
    pub active_connections: Arc<AtomicU32>,
    pub uptime_secs: Arc<AtomicU64>,
}

impl MetricsState {
    pub fn new() -> Self {
        Self {
            events_received: Arc::new(AtomicU64::new(0)),
            events_in_flight: Arc::new(AtomicU64::new(0)),
            events_forwarded: Arc::new(AtomicU64::new(0)),
            events_dropped: Arc::new(AtomicU64::new(0)),
            events_decoded: Arc::new(AtomicU64::new(0)),
            kernel_events_dropped: Arc::new(AtomicU64::new(0)),
            decode_events_dropped: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicU32::new(0)),
            uptime_secs: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Default for MetricsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Start the metrics HTTP server.
pub async fn start(host: &str, port: u16, state: MetricsState) -> crate::error::Result<()> {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/health", get(health_handler))
        .with_state(state);

    let addr: SocketAddr = format!("{host}:{port}").parse().map_err(|e| {
        crate::error::AgentError::MetricsServer(format!(
            "Invalid bind address '{host}:{port}': {e}"
        ))
    })?;
    info!("Metrics server listening on http://{addr}");

    axum::serve(
        tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| crate::error::AgentError::MetricsServer(format!("Bind failed: {e}")))?,
        app,
    )
    .await
    .map_err(|e| crate::error::AgentError::MetricsServer(format!("Server failed: {e}")))?;

    Ok(())
}

async fn metrics_handler(State(state): State<MetricsState>) -> Html<String> {
    let received = state.events_received.load(Ordering::Relaxed);
    let in_flight = state.events_in_flight.load(Ordering::Relaxed);
    let forwarded = state.events_forwarded.load(Ordering::Relaxed);
    let dropped = state.events_dropped.load(Ordering::Relaxed);
    let decoded = state.events_decoded.load(Ordering::Relaxed);
    let kernel_dropped = state.kernel_events_dropped.load(Ordering::Relaxed);
    let decode_dropped = state.decode_events_dropped.load(Ordering::Relaxed);
    let connections = state.active_connections.load(Ordering::Relaxed);
    let uptime = state.uptime_secs.load(Ordering::Relaxed);

    let body = format!(
        "# HELP detrix_agent_events_received_total Events received by forwarding stage\n\
         # TYPE detrix_agent_events_received_total counter\n\
         detrix_agent_events_received_total {received}\n\
         # HELP detrix_agent_events_in_flight Events queued in forwarding stage\n\
         # TYPE detrix_agent_events_in_flight gauge\n\
         detrix_agent_events_in_flight {in_flight}\n\
         # HELP detrix_agent_events_forwarded_total Cumulative events forwarded\n\
         # TYPE detrix_agent_events_forwarded_total counter\n\
         detrix_agent_events_forwarded_total {forwarded}\n\
         # HELP detrix_agent_events_dropped_total Cumulative events dropped\n\
         # TYPE detrix_agent_events_dropped_total counter\n\
         detrix_agent_events_dropped_total {dropped}\n\
         # HELP detrix_agent_events_decoded_total Cumulative decoded events\n\
         # TYPE detrix_agent_events_decoded_total counter\n\
         detrix_agent_events_decoded_total {decoded}\n\
         # HELP detrix_agent_kernel_events_dropped_total Cumulative kernel ring-buffer drops\n\
         # TYPE detrix_agent_kernel_events_dropped_total counter\n\
         detrix_agent_kernel_events_dropped_total {kernel_dropped}\n\
         # HELP detrix_agent_decode_events_dropped_total Cumulative decoder drops\n\
         # TYPE detrix_agent_decode_events_dropped_total counter\n\
         detrix_agent_decode_events_dropped_total {decode_dropped}\n\
         # HELP detrix_agent_active_connections Active connections\n\
         # TYPE detrix_agent_active_connections gauge\n\
         detrix_agent_active_connections {connections}\n\
         # HELP detrix_agent_uptime_seconds Uptime in seconds\n\
         # TYPE detrix_agent_uptime_seconds counter\n\
         detrix_agent_uptime_seconds {uptime}\n",
    );

    Html(body)
}

async fn health_handler() -> StatusCode {
    StatusCode::OK
}
