//! gRPC client for communicating with the Detrix daemon
//!
//! Uses shared authentication and conversion utilities from detrix-api.

use color_eyre::eyre::{Context, Result};
use detrix_api::generated::detrix::v1::{
    connection_service_client::ConnectionServiceClient,
    metrics_service_client::MetricsServiceClient, streaming_service_client::StreamingServiceClient,
    CloseConnectionRequest, ListConnectionsRequest, ListMetricsRequest, StreamAllRequest,
};
use detrix_api::grpc::{
    connect_to_daemon_grpc, proto_to_core_connection, proto_to_core_event, proto_to_core_metric,
    AuthChannel, DaemonEndpoints,
};
use detrix_core::{Connection, Metric, MetricEvent};
use detrix_logging::warn;
use serde::Deserialize;
use std::pin::Pin;
use tokio_stream::Stream;

/// Daemon status information from HTTP API
#[derive(Debug, Clone, Default)]
pub struct DaemonStatus {
    pub status: String,
    pub uptime_seconds: u64,
    pub uptime_formatted: String,
    pub started_at: String,
    pub active_metrics: usize,
    pub total_metrics: usize,
    pub active_connections: usize,
    pub daemon_pid: u32,
    pub daemon_ppid: u32,
    pub mcp_clients: Vec<McpClientInfo>,
}

/// Parent process info (from MCP client tracking)
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentProcessInfo {
    pub pid: u32,
    pub name: String,
    pub bridge_pid: u32,
}

/// MCP client information
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpClientInfo {
    pub id: String,
    pub connected_at: String,
    pub connected_duration_secs: u64,
    pub last_activity: String,
    pub last_activity_ago_secs: u64,
    #[serde(default)]
    pub parent_process: Option<ParentProcessInfo>,
}

/// HTTP status response (matches REST API which uses camelCase)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpStatusResponse {
    status: String,
    uptime_seconds: u64,
    uptime_formatted: String,
    started_at: String,
    active_metrics: usize,
    total_metrics: usize,
    active_connections: usize,
    #[allow(dead_code)]
    total_connections: usize,
    daemon: DaemonInfoResponse,
    #[serde(default)]
    mcp_clients: Vec<McpClientInfo>,
}

/// Daemon process info (PID, parent PID)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonInfoResponse {
    pid: u32,
    ppid: u32,
}

/// Client for communicating with the Detrix daemon
#[derive(Clone)]
pub struct DetrixClient {
    metrics_client: MetricsServiceClient<AuthChannel>,
    streaming_client: StreamingServiceClient<AuthChannel>,
    connections_client: ConnectionServiceClient<AuthChannel>,
    http_base_url: String,
    http_client: reqwest::Client,
}

impl DetrixClient {
    /// Connect to the Detrix daemon using discovered endpoints
    ///
    /// Uses the shared daemon connection logic from detrix-api.
    pub async fn connect_with_endpoints(endpoints: &DaemonEndpoints) -> Result<Self> {
        let auth_channel = connect_to_daemon_grpc(endpoints)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("{}", e))?;

        let http_base_url = endpoints.http_endpoint();

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            metrics_client: MetricsServiceClient::new(auth_channel.clone()),
            streaming_client: StreamingServiceClient::new(auth_channel.clone()),
            connections_client: ConnectionServiceClient::new(auth_channel),
            http_base_url,
            http_client,
        })
    }

    /// Connect to the Detrix daemon with explicit HTTP configuration
    ///
    /// # Arguments
    /// * `grpc_endpoint` - Full gRPC endpoint URL (e.g., "http://127.0.0.1:50051")
    /// * `http_host` - HTTP API host (from config.api.http.host)
    /// * `http_port` - HTTP API port (from config.api.http.port)
    ///
    /// Note: Use `connect_with_endpoints()` with pre-discovered endpoints for PID file discovery.
    pub async fn connect_with_http_config(
        grpc_endpoint: &str,
        http_host: &str,
        http_port: u16,
    ) -> Result<Self> {
        use detrix_api::grpc::{read_auth_token, AuthInterceptor};
        use tonic::service::interceptor::InterceptedService;
        use tonic::transport::Channel;

        let channel = Channel::from_shared(grpc_endpoint.to_string())
            .context("Invalid endpoint")?
            .connect()
            .await
            .context("Failed to connect to daemon")?;

        // Read auth token and create interceptor (shared with CLI)
        let token = read_auth_token();
        let interceptor = AuthInterceptor::new(token);

        let auth_channel = InterceptedService::new(channel, interceptor);

        // Build HTTP base URL from host and port config
        let http_base_url = format!("http://{}:{}", http_host, http_port);

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            metrics_client: MetricsServiceClient::new(auth_channel.clone()),
            streaming_client: StreamingServiceClient::new(auth_channel.clone()),
            connections_client: ConnectionServiceClient::new(auth_channel),
            http_base_url,
            http_client,
        })
    }

    /// Stream all metric events
    pub async fn stream_all(
        &mut self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<MetricEvent, tonic::Status>> + Send>>> {
        let request = StreamAllRequest::default();
        let response = self
            .streaming_client
            .stream_all(request)
            .await
            .context("Failed to start stream")?;

        let stream = response.into_inner();

        // Map proto events to core events using shared conversion
        let mapped = tokio_stream::StreamExt::map(stream, |result| {
            result.map(|proto_event| proto_to_core_event(&proto_event))
        });

        Ok(Box::pin(mapped))
    }

    /// List all metrics
    pub async fn list_metrics(&mut self) -> Result<Vec<Metric>> {
        let request = ListMetricsRequest::default();
        let response = self
            .metrics_client
            .list_metrics(request)
            .await
            .context("Failed to list metrics")?;

        // Caller decides: skip invalid metrics with logging
        let metrics = response
            .into_inner()
            .metrics
            .into_iter()
            .filter_map(|m| {
                proto_to_core_metric(&m)
                    .inspect_err(|e| {
                        warn!(
                            metric_name = %m.name,
                            "Skipping metric with conversion error: {}", e
                        );
                    })
                    .ok()
            })
            .collect();

        Ok(metrics)
    }

    /// List all connections
    pub async fn list_connections(&mut self) -> Result<Vec<Connection>> {
        let request = ListConnectionsRequest::default();
        let response = self
            .connections_client
            .list_connections(request)
            .await
            .context("Failed to list connections")?;

        // Caller decides: skip invalid connections with logging
        let connections = response
            .into_inner()
            .connections
            .into_iter()
            .filter_map(|c| {
                proto_to_core_connection(&c)
                    .inspect_err(|e| {
                        warn!(
                            connection_id = %c.connection_id,
                            "Skipping connection with conversion error: {}", e
                        );
                    })
                    .ok()
            })
            .collect();

        Ok(connections)
    }

    /// Close a connection by ID
    pub async fn close_connection(&mut self, connection_id: &str) -> Result<bool> {
        let request = CloseConnectionRequest {
            connection_id: connection_id.to_string(),
            metadata: None,
        };
        let response = self
            .connections_client
            .close_connection(request)
            .await
            .context("Failed to close connection")?;
        Ok(response.into_inner().success)
    }

    /// Get daemon status via HTTP API
    pub async fn get_status(&self) -> Result<DaemonStatus> {
        let url = format!("{}/status", self.http_base_url);

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to daemon HTTP API")?;

        if !response.status().is_success() {
            color_eyre::eyre::bail!("HTTP status error: {}", response.status());
        }

        let http_resp: HttpStatusResponse = response
            .json()
            .await
            .context("Failed to parse status response")?;

        Ok(DaemonStatus {
            status: http_resp.status,
            uptime_seconds: http_resp.uptime_seconds,
            uptime_formatted: http_resp.uptime_formatted,
            started_at: http_resp.started_at,
            active_metrics: http_resp.active_metrics,
            total_metrics: http_resp.total_metrics,
            active_connections: http_resp.active_connections,
            daemon_pid: http_resp.daemon.pid,
            daemon_ppid: http_resp.daemon.ppid,
            mcp_clients: http_resp.mcp_clients,
        })
    }
}
