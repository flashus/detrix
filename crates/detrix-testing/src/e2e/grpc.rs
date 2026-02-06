//! gRPC client implementation for E2E tests
//!
//! This module provides a gRPC client that implements the `ApiClient` trait,
//! enabling the same test scenarios to run against both MCP and gRPC APIs.

use async_trait::async_trait;
use detrix_api::generated::detrix::v1::{
    connection_service_client::ConnectionServiceClient, metric_event, metric_mode::Mode,
    metrics_service_client::MetricsServiceClient, streaming_service_client::StreamingServiceClient,
    AddMetricRequest as ProtoAddMetricRequest, CreateConnectionRequest, GetMcpUsageRequest,
    GetMetricRequest, GroupRequest, ListConnectionsRequest, ListMetricsRequest, Location,
    MetricMode, QueryRequest, RemoveMetricRequest, StackTraceSlice, StatusRequest,
    StreamAllRequest, StreamMode, ToggleMetricRequest, WakeRequest,
};
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::transport::Channel;

use super::client::{
    AddMetricRequest, ApiClient, ApiError, ApiResponse, ApiResult, ConnectionInfo, EventInfo,
    GroupInfo, McpErrorCountInfo, McpUsageInfo, McpWorkflowInfo, MetricInfo, StatusInfo,
    SystemEventInfo, ValidationResult,
};
use detrix_api::grpc::{proto_to_core_memory_snapshot, proto_to_core_stack_trace};
use detrix_config::constants::{DEFAULT_API_HOST, DEFAULT_REST_PORT};

/// gRPC client for Detrix E2E tests
pub struct GrpcClient {
    grpc_port: u16,
    http_port: u16,
    metrics_client: Option<MetricsServiceClient<Channel>>,
    connection_client: Option<ConnectionServiceClient<Channel>>,
    streaming_client: Option<StreamingServiceClient<Channel>>,
}

impl GrpcClient {
    /// Create a new gRPC client for the given gRPC and HTTP ports
    pub fn new(grpc_port: u16) -> Self {
        Self::with_http_port(grpc_port, DEFAULT_REST_PORT)
    }

    /// Create a new gRPC client with explicit HTTP port for health checks
    pub fn with_http_port(grpc_port: u16, http_port: u16) -> Self {
        Self {
            grpc_port,
            http_port,
            metrics_client: None,
            connection_client: None,
            streaming_client: None,
        }
    }

    /// Get or create a connection to the gRPC server
    async fn connect(&mut self) -> Result<(), ApiError> {
        if self.metrics_client.is_some() {
            return Ok(());
        }

        let addr = format!("http://{}:{}", DEFAULT_API_HOST, self.grpc_port);
        let channel = Channel::from_shared(addr.clone())
            .map_err(|e| ApiError::new(format!("Invalid URI: {}", e)))?
            .connect()
            .await
            .map_err(|e| ApiError::new(format!("Failed to connect to gRPC server: {}", e)))?;

        self.metrics_client = Some(MetricsServiceClient::new(channel.clone()));
        self.connection_client = Some(ConnectionServiceClient::new(channel.clone()));
        self.streaming_client = Some(StreamingServiceClient::new(channel));

        Ok(())
    }

    fn metrics_client(&mut self) -> Result<&mut MetricsServiceClient<Channel>, ApiError> {
        self.metrics_client
            .as_mut()
            .ok_or_else(|| ApiError::new("Not connected"))
    }

    fn connection_client(&mut self) -> Result<&mut ConnectionServiceClient<Channel>, ApiError> {
        self.connection_client
            .as_mut()
            .ok_or_else(|| ApiError::new("Not connected"))
    }

    fn streaming_client(&mut self) -> Result<&mut StreamingServiceClient<Channel>, ApiError> {
        self.streaming_client
            .as_mut()
            .ok_or_else(|| ApiError::new("Not connected"))
    }

    /// Stream all metric events via gRPC StreamAll RPC.
    ///
    /// Returns a channel receiver and a handle to close the stream.
    /// Events are converted to `EventInfo` for compatibility with WebSocket streaming tests.
    pub async fn stream_events(
        &mut self,
    ) -> Result<(mpsc::Receiver<EventInfo>, GrpcStreamHandle), ApiError> {
        self.connect().await?;

        let request = StreamAllRequest {
            thread_filter: None,
            metadata: None,
        };

        let response = self
            .streaming_client()?
            .stream_all(request)
            .await
            .map_err(|e| ApiError::new(format!("gRPC stream error: {}", e)))?;

        let mut stream = response.into_inner();

        let (tx, rx) = mpsc::channel(100);
        let handle = GrpcStreamHandle::new();
        let close_flag = handle.close_flag.clone();

        // Spawn a task to read from the stream and send to the channel
        tokio::spawn(async move {
            loop {
                if close_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }

                match tokio::time::timeout(Duration::from_millis(100), stream.message()).await {
                    Ok(Ok(Some(event))) => {
                        let (value, is_error) = match &event.result {
                            Some(metric_event::Result::ValueJson(val)) => (val.clone(), false),
                            Some(metric_event::Result::Error(err)) => {
                                (err.error_message.clone(), true)
                            }
                            None => ("".to_string(), false),
                        };

                        let timestamp_iso =
                            chrono::DateTime::from_timestamp_micros(event.timestamp)
                                .map(|dt| dt.to_rfc3339())
                                .unwrap_or_else(|| {
                                    eprintln!(
                                        "[WARN] Failed to parse timestamp {} for metric {}",
                                        event.timestamp, event.metric_name
                                    );
                                    String::new()
                                });

                        // Convert introspection data from proto
                        let stack_trace = event.stack_trace.as_ref().map(proto_to_core_stack_trace);
                        let memory_snapshot = event
                            .memory_snapshot
                            .as_ref()
                            .map(proto_to_core_memory_snapshot);

                        let event_info = EventInfo {
                            metric_name: event.metric_name,
                            value,
                            timestamp_iso,
                            age_seconds: (chrono::Utc::now().timestamp_micros() - event.timestamp)
                                / 1_000_000,
                            is_error,
                            stack_trace,
                            memory_snapshot,
                            extra: std::collections::HashMap::new(),
                        };

                        if tx.send(event_info).await.is_err() {
                            break;
                        }
                    }
                    Ok(Ok(None)) => {
                        // Stream ended
                        break;
                    }
                    Ok(Err(_)) => {
                        // Stream error
                        break;
                    }
                    Err(_) => {
                        // Timeout - check close flag and continue
                        continue;
                    }
                }
            }
        });

        Ok((rx, handle))
    }

    /// Receive events for a limited duration, collecting up to max_events.
    pub async fn receive_events(
        &mut self,
        duration: Duration,
        max_events: Option<usize>,
    ) -> Result<Vec<EventInfo>, ApiError> {
        let (mut rx, handle) = self.stream_events().await?;

        let events = tokio::time::timeout(duration, async {
            let mut collected = Vec::new();
            let limit = max_events.unwrap_or(usize::MAX);

            while let Some(event) = rx.recv().await {
                collected.push(event);
                if collected.len() >= limit {
                    break;
                }
            }
            collected
        })
        .await
        .unwrap_or_else(|_| {
            // Timeout is expected behavior when collecting events for a fixed duration
            Vec::new()
        });

        handle.close().await;
        Ok(events)
    }
}

/// Handle to manage a gRPC streaming connection
pub struct GrpcStreamHandle {
    close_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl GrpcStreamHandle {
    fn new() -> Self {
        Self {
            close_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Close the stream gracefully
    pub async fn close(&self) {
        self.close_flag
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // Give the spawned task time to notice and exit
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[async_trait]
impl ApiClient for GrpcClient {
    fn api_type(&self) -> &'static str {
        "gRPC"
    }

    // ========================================================================
    // Health & Status
    // ========================================================================

    async fn health(&self) -> Result<bool, ApiError> {
        // Health check via HTTP endpoint - uses the stored http_port
        let url = format!("http://{}:{}/health", DEFAULT_API_HOST, self.http_port);
        let client = reqwest::Client::new();
        match client.get(&url).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    async fn get_status(&self) -> ApiResult<StatusInfo> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .get_status(StatusRequest { metadata: None })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        Ok(ApiResponse::new(StatusInfo {
            mode: response.mode,
            uptime_seconds: response.uptime_seconds as u64,
            active_connections: response.active_connections as usize,
            total_metrics: response.total_metrics as usize,
            enabled_metrics: response.enabled_metrics as usize,
        }))
    }

    async fn wake(&self) -> ApiResult<String> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .wake(WakeRequest { metadata: None })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        Ok(ApiResponse::new(response.status))
    }

    async fn sleep(&self) -> ApiResult<String> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .sleep(detrix_api::SleepRequest { metadata: None })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        Ok(ApiResponse::new(response.status))
    }

    // ========================================================================
    // Connection Management
    // ========================================================================

    async fn create_connection_with_program(
        &self,
        host: &str,
        port: u16,
        language: &str,
        program: Option<&str>,
    ) -> ApiResult<ConnectionInfo> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .connection_client()?
            .create_connection(CreateConnectionRequest {
                host: host.to_string(),
                port: port.into(), // Convert u16 to u32 for proto
                language: language.to_string(),
                // Identity fields for UUID-based connection tracking
                name: format!("e2e-test-{}-{}", language, port),
                workspace_root: "/e2e-test".to_string(),
                hostname: detrix_api::common::resolve_hostname(),
                metadata: None,
                program: program.map(|s| s.to_string()), // Optional program path for launch mode (Rust direct lldb-dap)
                safe_mode: false,                        // Default to false for tests
                pid: None,                               // Tests don't use AttachPid mode
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        let conn = response.connection.unwrap_or_default();
        Ok(ApiResponse::new(ConnectionInfo {
            connection_id: response.connection_id,
            host: conn.host,
            port: conn.port as u16, // Convert from proto u32
            language: conn.language,
            status: response.status,
            safe_mode: conn.safe_mode,
        }))
    }

    async fn close_connection(&self, connection_id: &str) -> ApiResult<()> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        client
            .connection_client()?
            .close_connection(detrix_api::CloseConnectionRequest {
                connection_id: connection_id.to_string(),
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?;

        Ok(ApiResponse::new(()))
    }

    async fn get_connection(&self, connection_id: &str) -> ApiResult<ConnectionInfo> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .connection_client()?
            .get_connection(detrix_api::GetConnectionRequest {
                connection_id: connection_id.to_string(),
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        let conn = response.connection.unwrap_or_default();
        Ok(ApiResponse::new(ConnectionInfo {
            connection_id: conn.connection_id,
            host: conn.host,
            port: conn.port as u16,
            language: conn.language,
            status: format!("{:?}", conn.status),
            safe_mode: conn.safe_mode,
        }))
    }

    async fn list_connections(&self) -> ApiResult<Vec<ConnectionInfo>> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .connection_client()?
            .list_connections(ListConnectionsRequest { metadata: None })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        let connections = response
            .connections
            .into_iter()
            .map(|conn| ConnectionInfo {
                connection_id: conn.connection_id,
                host: conn.host,
                port: conn.port as u16,
                language: conn.language,
                status: format!("{:?}", conn.status),
                safe_mode: conn.safe_mode,
            })
            .collect();

        Ok(ApiResponse::new(connections))
    }

    // ========================================================================
    // Metric Management
    // ========================================================================

    async fn add_metric(&self, request: AddMetricRequest) -> ApiResult<String> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        // Parse location from "@file.py#123" format
        let (file, line) = parse_location(&request.location)?;

        // connection_id is required - enforced by AddMetricRequest structure
        let connection_id = request.connection_id.clone();

        // Build stack trace slice using helper function
        let stack_trace_slice = build_stack_trace_slice(&request);

        let response = client
            .metrics_client()?
            .add_metric(ProtoAddMetricRequest {
                name: request.name,
                group: request.group,
                location: Some(Location { file, line }),
                expression: request.expression,
                language: request.language.clone(), // Optional - will be derived from connection if None
                enabled: request.enabled.unwrap_or(true), // Default to enabled if not specified
                mode: Some(MetricMode {
                    mode: Some(Mode::Stream(StreamMode {})),
                }),
                condition: None,
                safety_level: "strict".to_string(),
                metadata: None,
                connection_id,
                replace: None, // Default: don't replace existing metrics
                // Introspection fields from request
                capture_stack_trace: request.capture_stack_trace,
                stack_trace_ttl: request.stack_trace_ttl,
                stack_trace_slice,
                capture_memory_snapshot: request.capture_memory_snapshot,
                snapshot_scope: request.snapshot_scope.clone(),
                snapshot_ttl: request.snapshot_ttl,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        Ok(ApiResponse::new(response.metric_id.to_string()))
    }

    async fn remove_metric(&self, name: &str) -> ApiResult<()> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        client
            .metrics_client()?
            .remove_metric(RemoveMetricRequest {
                identifier: Some(
                    detrix_api::generated::detrix::v1::remove_metric_request::Identifier::Name(
                        name.to_string(),
                    ),
                ),
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?;

        Ok(ApiResponse::new(()))
    }

    async fn toggle_metric(&self, name: &str, enabled: bool) -> ApiResult<()> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        // First, get the metric to find its ID
        let metric = self.get_metric(name).await?;
        let metric_id: i64 = metric
            .raw_response
            .as_ref()
            .and_then(|r| r.parse().ok())
            .unwrap_or(0);

        client
            .metrics_client()?
            .toggle_metric(ToggleMetricRequest {
                metric_id: metric_id as u64,
                enabled,
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?;

        Ok(ApiResponse::new(()))
    }

    async fn get_metric(&self, name: &str) -> ApiResult<MetricInfo> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .get_metric(GetMetricRequest {
                identifier: Some(
                    detrix_api::generated::detrix::v1::get_metric_request::Identifier::Name(
                        name.to_string(),
                    ),
                ),
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        let location = response
            .location
            .map(|l| format!("@{}#{}", l.file, l.line))
            .unwrap_or_default();

        Ok(ApiResponse::new(MetricInfo {
            name: response.name,
            location,
            expression: String::new(), // Not provided in MetricResponse
            group: None,
            enabled: true, // Not directly provided
            mode: None,
        })
        .with_raw(response.metric_id.to_string()))
    }

    async fn list_metrics(&self) -> ApiResult<Vec<MetricInfo>> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .list_metrics(ListMetricsRequest {
                group: None,
                enabled_only: None,
                name_pattern: None,
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        let metrics = response
            .metrics
            .into_iter()
            .map(|m| MetricInfo {
                name: m.name,
                location: m
                    .location
                    .map(|l| format!("@{}#{}", l.file, l.line))
                    .unwrap_or_default(),
                expression: m.expression,
                group: m.group,
                enabled: m.enabled,
                mode: proto_mode_to_string(&m.mode),
            })
            .collect();

        Ok(ApiResponse::new(metrics))
    }

    async fn query_events(&self, metric_name: &str, limit: u32) -> ApiResult<Vec<EventInfo>> {
        // First, get the metric to find its ID (gRPC API requires metric_ids, not names)
        let metric = self.get_metric(metric_name).await?;
        let metric_id: i64 = metric
            .raw_response
            .as_ref()
            .and_then(|r| r.parse().ok())
            .unwrap_or(0);

        if metric_id == 0 {
            // No metric found or couldn't parse ID - return empty
            return Ok(ApiResponse::new(vec![]));
        }

        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .streaming_client()?
            .query_metrics(QueryRequest {
                metric_ids: vec![metric_id as u64],
                time_range: None,
                limit: Some(limit),
                offset: None,
                cursor: None,
                order: None,
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        let events = response
            .events
            .into_iter()
            .map(|e| {
                // Extract value from the MetricEvent result
                let (value, is_error) = match &e.result {
                    Some(detrix_api::generated::detrix::v1::metric_event::Result::ValueJson(v)) => {
                        (v.clone(), false)
                    }
                    Some(detrix_api::generated::detrix::v1::metric_event::Result::Error(err)) => {
                        (err.error_message.clone(), true)
                    }
                    None => (String::new(), false),
                };

                // Convert introspection data from proto
                let stack_trace = e.stack_trace.as_ref().map(proto_to_core_stack_trace);
                let memory_snapshot = e
                    .memory_snapshot
                    .as_ref()
                    .map(proto_to_core_memory_snapshot);

                EventInfo {
                    metric_name: e.metric_name,
                    value,
                    timestamp_iso: chrono::DateTime::from_timestamp_micros(e.timestamp)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default(),
                    age_seconds: (chrono::Utc::now().timestamp_micros() - e.timestamp) / 1_000_000,
                    is_error,
                    stack_trace,
                    memory_snapshot,
                    extra: std::collections::HashMap::new(),
                }
            })
            .collect();

        Ok(ApiResponse::new(events))
    }

    // ========================================================================
    // Group Management
    // ========================================================================

    async fn enable_group(&self, group: &str) -> ApiResult<usize> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .enable_group(GroupRequest {
                group_name: group.to_string(),
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        Ok(ApiResponse::new(response.metrics_affected as usize))
    }

    async fn disable_group(&self, group: &str) -> ApiResult<usize> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .disable_group(GroupRequest {
                group_name: group.to_string(),
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        Ok(ApiResponse::new(response.metrics_affected as usize))
    }

    async fn list_groups(&self) -> ApiResult<Vec<GroupInfo>> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .list_groups(detrix_api::ListGroupsRequest { metadata: None })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        let groups = response
            .groups
            .into_iter()
            .map(|g| GroupInfo {
                name: g.name,
                metric_count: g.metric_count,
                enabled_count: g.enabled_count,
            })
            .collect();

        Ok(ApiResponse::new(groups))
    }

    // ========================================================================
    // Validation
    // ========================================================================

    async fn validate_expression(
        &self,
        expression: &str,
        language: &str,
    ) -> ApiResult<ValidationResult> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .validate_expression(detrix_api::ValidateExpressionRequest {
                expression: expression.to_string(),
                language: language.to_string(),
                metadata: None,
                safety_level: None, // Use default from config
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        // Use violations from proto response (reason field was removed)
        Ok(ApiResponse::new(ValidationResult {
            is_safe: response.is_safe,
            issues: response.violations,
        }))
    }

    async fn inspect_file(
        &self,
        file_path: &str,
        line: Option<u32>,
        find_variable: Option<&str>,
    ) -> ApiResult<String> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .inspect_file(detrix_api::InspectFileRequest {
                file_path: file_path.to_string(),
                line,
                find_variable: find_variable.map(|s| s.to_string()),
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        if response.success {
            Ok(ApiResponse::new(response.content))
        } else {
            Err(ApiError::new(
                response
                    .error
                    .unwrap_or_else(|| "Unknown error".to_string()),
            ))
        }
    }

    // ========================================================================
    // Configuration
    // ========================================================================

    async fn get_config(&self) -> ApiResult<String> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .get_config(detrix_api::GetConfigRequest {
                path: None,
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        Ok(ApiResponse::new(response.config_toml))
    }

    async fn update_config(&self, config_toml: &str, persist: bool) -> ApiResult<()> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        client
            .metrics_client()?
            .update_config(detrix_api::UpdateConfigRequest {
                config_toml: config_toml.to_string(),
                merge: false,
                persist,
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?;

        Ok(ApiResponse::new(()))
    }

    async fn validate_config(&self, config_toml: &str) -> ApiResult<bool> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .validate_config(detrix_api::ValidateConfigRequest {
                config_toml: config_toml.to_string(),
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?
            .into_inner();

        Ok(ApiResponse::new(response.valid))
    }

    async fn reload_config(&self) -> ApiResult<()> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        client
            .metrics_client()?
            .reload_config(detrix_api::ReloadConfigRequest {
                config_path: None,
                metadata: None,
            })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?;

        Ok(ApiResponse::new(()))
    }

    // ========================================================================
    // System Events (not implemented for gRPC - MCP only)
    // ========================================================================

    async fn query_system_events(
        &self,
        _limit: u32,
        _unacknowledged_only: bool,
        _event_types: Option<Vec<&str>>,
    ) -> ApiResult<Vec<SystemEventInfo>> {
        // System events are MCP-only feature
        Err(ApiError::new(
            "System events are not available via gRPC API. Use MCP instead.",
        ))
    }

    async fn acknowledge_system_events(&self, _event_ids: Vec<i64>) -> ApiResult<u64> {
        // System events are MCP-only feature
        Err(ApiError::new(
            "System events are not available via gRPC API. Use MCP instead.",
        ))
    }

    async fn get_mcp_usage(&self) -> ApiResult<McpUsageInfo> {
        let mut client = GrpcClient::new(self.grpc_port);
        client.connect().await?;

        let response = client
            .metrics_client()?
            .get_mcp_usage(GetMcpUsageRequest { metadata: None })
            .await
            .map_err(|e| ApiError::new(format!("gRPC error: {}", e)))?;

        let inner = response.into_inner();
        let usage = inner.usage.ok_or_else(|| ApiError::new("No usage data"))?;
        let workflow = usage
            .workflow
            .ok_or_else(|| ApiError::new("No workflow data"))?;

        Ok(ApiResponse::new(McpUsageInfo {
            total_calls: usage.total_calls,
            total_success: usage.total_success,
            total_errors: usage.total_errors,
            success_rate: usage.success_rate,
            avg_latency_ms: usage.avg_latency_ms,
            top_errors: usage
                .top_errors
                .into_iter()
                .map(|e| McpErrorCountInfo {
                    code: e.code,
                    count: e.count,
                })
                .collect(),
            workflow: McpWorkflowInfo {
                correct_workflows: workflow.correct_workflows,
                skipped_inspect: workflow.skipped_inspect,
                no_connection: workflow.no_connection,
                adherence_rate: workflow.adherence_rate,
            },
        }))
    }
}

/// Build StackTraceSlice from AddMetricRequest fields
///
/// Logic:
/// 1. If any slice options are specified (full/head/tail), create slice with those values
/// 2. If only capture_stack_trace is true, default to full stack trace
/// 3. Otherwise, return None
fn build_stack_trace_slice(request: &AddMetricRequest) -> Option<StackTraceSlice> {
    if request.stack_trace_full.is_some()
        || request.stack_trace_head.is_some()
        || request.stack_trace_tail.is_some()
    {
        Some(StackTraceSlice {
            full: request.stack_trace_full.unwrap_or(false),
            head: request.stack_trace_head,
            tail: request.stack_trace_tail,
        })
    } else if request.capture_stack_trace == Some(true) {
        // Default to full stack trace if capture is enabled but no slice specified
        Some(StackTraceSlice {
            full: true,
            head: None,
            tail: None,
        })
    } else {
        None
    }
}

// Parse a location string like "@file.py#123" into (file, line)
fn parse_location(location: &str) -> Result<(String, u32), ApiError> {
    let location = location.strip_prefix('@').unwrap_or(location);
    let parts: Vec<&str> = location.rsplitn(2, '#').collect();
    if parts.len() != 2 {
        return Err(ApiError::new(format!(
            "Invalid location format: {}",
            location
        )));
    }

    let line: u32 = parts[0]
        .parse()
        .map_err(|_| ApiError::new(format!("Invalid line number: {}", parts[0])))?;
    let file = parts[1].to_string();

    Ok((file, line))
}

/// Convert proto MetricMode to string representation
///
/// Maps the proto enum variants to their string equivalents:
/// - Stream → "stream"
/// - Sample → "sample"
/// - First → "first"
/// - Throttle → "throttle"
/// - SampleInterval → "sample_interval"
fn proto_mode_to_string(mode: &Option<MetricMode>) -> Option<String> {
    mode.as_ref()?.mode.as_ref().map(|m| match m {
        Mode::Stream(_) => "stream".to_string(),
        Mode::Sample(_) => "sample".to_string(),
        Mode::First(_) => "first".to_string(),
        Mode::Throttle(_) => "throttle".to_string(),
        Mode::SampleInterval(_) => "sample_interval".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_api::generated::detrix::v1::{
        FirstMode, SampleIntervalMode, SampleMode, StreamMode, ThrottleMode,
    };

    #[test]
    fn test_parse_location() {
        let (file, line) = parse_location("@fixtures/python/detrix_example_app.py#42").unwrap();
        assert_eq!(file, "fixtures/python/detrix_example_app.py");
        assert_eq!(line, 42);

        let (file, line) = parse_location("fixtures/python/detrix_example_app.py#42").unwrap();
        assert_eq!(file, "fixtures/python/detrix_example_app.py");
        assert_eq!(line, 42);

        // Windows paths work naturally with # separator
        let (file, line) = parse_location("C:/Users/test.py#10").unwrap();
        assert_eq!(file, "C:/Users/test.py");
        assert_eq!(line, 10);
    }

    #[test]
    fn test_build_stack_trace_slice_with_full_option() {
        // Use with_full_stack_trace() which sets capture_stack_trace=true and stack_trace_full=true
        let request =
            AddMetricRequest::new("test", "test.py#10", "x", "conn1").with_full_stack_trace();

        let slice = build_stack_trace_slice(&request);
        assert!(slice.is_some());
        let slice = slice.unwrap();
        assert!(slice.full);
        assert!(slice.head.is_none());
        assert!(slice.tail.is_none());
    }

    #[test]
    fn test_build_stack_trace_slice_with_head_tail() {
        let mut request = AddMetricRequest::new("test", "test.py#10", "x", "conn1");
        request.stack_trace_head = Some(5);
        request.stack_trace_tail = Some(3);

        let slice = build_stack_trace_slice(&request);
        assert!(slice.is_some());
        let slice = slice.unwrap();
        assert!(!slice.full);
        assert_eq!(slice.head, Some(5));
        assert_eq!(slice.tail, Some(3));
    }

    #[test]
    fn test_build_stack_trace_slice_with_capture_stack_trace_only() {
        // Use with_stack_trace() which sets capture_stack_trace=true
        let request = AddMetricRequest::new("test", "test.py#10", "x", "conn1").with_stack_trace();

        let slice = build_stack_trace_slice(&request);
        assert!(slice.is_some());
        let slice = slice.unwrap();
        // Default to full stack trace when capture_stack_trace is true but no slice options
        assert!(slice.full);
        assert!(slice.head.is_none());
        assert!(slice.tail.is_none());
    }

    #[test]
    fn test_build_stack_trace_slice_no_introspection() {
        let request = AddMetricRequest::new("test", "test.py#10", "x", "conn1");

        let slice = build_stack_trace_slice(&request);
        assert!(slice.is_none());
    }

    #[test]
    fn test_build_stack_trace_slice_explicit_full_false() {
        // When full=false is explicitly set with head/tail, use those values
        let mut request = AddMetricRequest::new("test", "test.py#10", "x", "conn1");
        request.stack_trace_full = Some(false);
        request.stack_trace_head = Some(10);

        let slice = build_stack_trace_slice(&request);
        assert!(slice.is_some());
        let slice = slice.unwrap();
        assert!(!slice.full);
        assert_eq!(slice.head, Some(10));
    }

    // ========================================================================
    // Mode Conversion Tests
    // ========================================================================

    #[test]
    fn test_proto_mode_to_string_stream() {
        let mode = MetricMode {
            mode: Some(Mode::Stream(StreamMode {})),
        };
        assert_eq!(
            proto_mode_to_string(&Some(mode)),
            Some("stream".to_string())
        );
    }

    #[test]
    fn test_proto_mode_to_string_sample() {
        let mode = MetricMode {
            mode: Some(Mode::Sample(SampleMode { rate: 50 })),
        };
        assert_eq!(
            proto_mode_to_string(&Some(mode)),
            Some("sample".to_string())
        );
    }

    #[test]
    fn test_proto_mode_to_string_first() {
        let mode = MetricMode {
            mode: Some(Mode::First(FirstMode {})),
        };
        assert_eq!(proto_mode_to_string(&Some(mode)), Some("first".to_string()));
    }

    #[test]
    fn test_proto_mode_to_string_throttle() {
        let mode = MetricMode {
            mode: Some(Mode::Throttle(ThrottleMode { max_per_second: 10 })),
        };
        assert_eq!(
            proto_mode_to_string(&Some(mode)),
            Some("throttle".to_string())
        );
    }

    #[test]
    fn test_proto_mode_to_string_sample_interval() {
        let mode = MetricMode {
            mode: Some(Mode::SampleInterval(SampleIntervalMode { seconds: 30 })),
        };
        assert_eq!(
            proto_mode_to_string(&Some(mode)),
            Some("sample_interval".to_string())
        );
    }

    #[test]
    fn test_proto_mode_to_string_none() {
        assert_eq!(proto_mode_to_string(&None), None);
    }

    #[test]
    fn test_proto_mode_to_string_empty_mode() {
        let mode = MetricMode { mode: None };
        assert_eq!(proto_mode_to_string(&Some(mode)), None);
    }
}
