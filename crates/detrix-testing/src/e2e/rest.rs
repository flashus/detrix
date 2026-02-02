//! REST Client Implementation
//!
//! Implements the ApiClient trait for the REST API (HTTP/JSON).
//!
//! Uses proto-generated types from detrix-api wherever possible to avoid duplication.

use super::client::{
    AddMetricRequest, ApiClient, ApiError, ApiResponse, ApiResult, ConnectionInfo, EventInfo,
    GroupInfo, McpUsageInfo, MetricInfo, StatusInfo, SystemEventInfo, ValidationResult,
};
use async_trait::async_trait;
use std::time::Duration;

// Import proto types, conversions, and REST DTOs from detrix-api
use detrix_api::{
    grpc::{proto_mode_to_string, proto_to_core_memory_snapshot, proto_to_core_stack_trace},
    // REST handler DTOs (now have both Serialize and Deserialize)
    http::handlers::{
        ConfigResponse, CreateMetricRequest as RestCreateMetricRequest, CreateMetricResponse,
        GroupOperationResponse, HealthResponse, PaginatedMetricsResponse,
        StackTraceSliceDto as RestStackTraceSliceDto, StatusResponse, WakeResponse,
    },
    // ErrorResponse for parsing API errors
    http::ErrorResponse,
    // Proto types (for requests/responses that match proto format)
    ConnectionInfo as ProtoConnectionInfo,
    CreateConnectionRequest as ProtoCreateConnectionRequest,
    CreateConnectionResponse as ProtoCreateConnectionResponse,
    InspectFileRequest as ProtoInspectFileRequest,
    Location as ProtoLocation,
    MetricEvent as ProtoMetricEvent,
    MetricInfo as ProtoMetricInfo,
    ReloadConfigRequest as ProtoReloadConfigRequest,
    SleepResponse,
    UpdateConfigRequest as ProtoUpdateConfigRequest,
    ValidateConfigRequest as ProtoValidateConfigRequest,
    ValidateExpressionRequest as ProtoValidateExpressionRequest,
    ValidateExpressionResponse as ProtoValidateExpressionResponse,
    ValidationResponse as ProtoValidationResponse,
};

/// Convert proto ConnectionStatus enum (i32) to string representation
/// Proto enum values: 0=UNSPECIFIED, 1=DISCONNECTED, 2=CONNECTING, 3=CONNECTED, 4=FAILED
fn status_i32_to_string(status: i32) -> String {
    match status {
        0 => "unspecified".to_string(),
        1 => "disconnected".to_string(),
        2 => "connecting".to_string(),
        3 => "connected".to_string(),
        4 => "failed".to_string(),
        _ => format!("unknown({})", status),
    }
}

// proto_mode_to_string is imported from detrix_api::grpc (centralized conversions)

/// Convert proto MetricInfo to client MetricInfo
fn proto_metric_to_client_metric(m: ProtoMetricInfo) -> MetricInfo {
    let location = m
        .location
        .map(|l| format!("{}#{}", l.file, l.line))
        .unwrap_or_default();
    MetricInfo {
        name: m.name,
        location,
        expression: m.expression,
        group: m.group,
        enabled: m.enabled,
        mode: proto_mode_to_string(&m.mode),
    }
}

/// REST API client (HTTP/JSON)
pub struct RestClient {
    base_url: String,
    client: reqwest::Client,
}

impl RestClient {
    /// Create a new REST client
    pub fn new(http_port: u16) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", http_port),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Create a new REST client with custom base URL
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Query events with timestamp filter
    ///
    /// # Arguments
    /// * `metric_name` - Name of the metric to query events for
    /// * `limit` - Maximum number of events to return
    /// * `since_micros` - Only return events with timestamp >= this value (microseconds since epoch)
    pub async fn query_events_since(
        &self,
        metric_name: &str,
        limit: u32,
        since_micros: i64,
    ) -> ApiResult<Vec<EventInfo>> {
        // Use camelCase for query params (as expected by API with serde rename_all = "camelCase")
        let response = self
            .client
            .get(format!(
                "{}/api/v1/events?metricName={}&limit={}&since={}",
                self.base_url, metric_name, limit, since_micros
            ))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Query events failed: {}", e)))?;

        // Check for HTTP errors before parsing
        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            return Err(ApiError::new(format!("HTTP {} - {}", status, raw)).with_raw(raw));
        }

        // Use proto MetricEvent type
        let events: Vec<ProtoMetricEvent> = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        let infos: Vec<EventInfo> = events
            .into_iter()
            .map(|e| {
                // Convert timestamp to ISO string
                let timestamp_iso = chrono::DateTime::from_timestamp_micros(e.timestamp)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|| e.timestamp.to_string());

                // Calculate age in seconds
                let now = chrono::Utc::now().timestamp_micros();
                let age_seconds = (now - e.timestamp) / 1_000_000;

                // Check result oneof for value or error
                let (value, is_error) = match e.result {
                    Some(detrix_api::metric_event::Result::ValueJson(v)) => (v, false),
                    Some(detrix_api::metric_event::Result::Error(_)) => (String::new(), true),
                    None => (String::new(), false),
                };

                // Convert introspection data using proto types
                let stack_trace = e.stack_trace.as_ref().map(proto_to_core_stack_trace);
                let memory_snapshot = e
                    .memory_snapshot
                    .as_ref()
                    .map(proto_to_core_memory_snapshot);

                EventInfo {
                    metric_name: e.metric_name,
                    value,
                    timestamp_iso,
                    age_seconds,
                    is_error,
                    stack_trace,
                    memory_snapshot,
                    extra: std::collections::HashMap::new(),
                }
            })
            .collect();

        Ok(ApiResponse::new(infos).with_raw(raw))
    }
}

// ============================================================================
// All DTOs are now imported from production crates:
// - RestCreateMetricRequest, RestStackTraceSliceDto from detrix_api::http::handlers
// - ErrorResponse from detrix_api::http
// - Proto types (ValidateExpressionRequest, etc.) from detrix_api
// ============================================================================

// ============================================================================
// ApiClient Implementation
// ============================================================================

#[async_trait]
impl ApiClient for RestClient {
    fn api_type(&self) -> &'static str {
        "REST"
    }

    // ========================================================================
    // Health & Status
    // ========================================================================

    async fn health(&self) -> Result<bool, ApiError> {
        let response = self
            .client
            .get(format!("{}/health", self.base_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Health check failed: {}", e)))?;

        if !response.status().is_success() {
            return Ok(false);
        }

        let health: HealthResponse = response
            .json()
            .await
            .map_err(|e| ApiError::new(format!("Failed to parse health response: {}", e)))?;

        Ok(health.status == "ok")
    }

    async fn get_status(&self) -> ApiResult<StatusInfo> {
        let response = self
            .client
            .get(format!("{}/api/v1/status", self.base_url))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Status request failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        // REST API returns different format than proto - use REST-specific DTO
        let status: StatusResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse status: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(StatusInfo {
            mode: status.status, // REST uses `status`, proto uses `mode`
            uptime_seconds: status.uptime_seconds,
            active_connections: status.active_connections,
            total_metrics: status.total_metrics,
            enabled_metrics: status.active_metrics, // REST uses `active_metrics`, proto uses `enabled_metrics`
        })
        .with_raw(raw))
    }

    async fn wake(&self) -> ApiResult<String> {
        let response = self
            .client
            .post(format!("{}/api/v1/wake", self.base_url))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Wake request failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        // REST API returns different format than proto - use REST-specific DTO
        let wake: WakeResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse wake response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(format!("{}: {}", wake.status, wake.message)).with_raw(raw))
    }

    async fn sleep(&self) -> ApiResult<String> {
        let response = self
            .client
            .post(format!("{}/api/v1/sleep", self.base_url))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Sleep request failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        // SleepResponse is proto-generated with both Serialize and Deserialize
        let sleep: SleepResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse sleep response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(format!("{}: {}", sleep.status, sleep.message)).with_raw(raw))
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
        // Use proto type directly
        let request = ProtoCreateConnectionRequest {
            host: host.to_string(),
            port: port.into(), // Convert u16 to u32 for proto
            language: language.to_string(),
            connection_id: None,
            metadata: None,
            program: program.map(|s| s.to_string()), // Optional program path for launch mode (Rust direct lldb-dap)
            safe_mode: false,                        // Default to false for tests
            pid: None,                               // Tests don't use AttachPid mode
        };

        let response = self
            .client
            .post(format!("{}/api/v1/connections", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Create connection failed: {}", e)))?;

        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&raw) {
                return Err(ApiError::new(err.error).with_raw(raw));
            }
            return Err(ApiError::new(format!("HTTP {}: {}", status, raw)).with_raw(raw));
        }

        let resp: ProtoCreateConnectionResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        // Connection is optional in proto response
        let conn = resp.connection.ok_or_else(|| {
            ApiError::new("Response missing connection details").with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(ConnectionInfo {
            connection_id: resp.connection_id,
            host: conn.host,
            port: conn.port as u16, // Convert from proto u32
            language: conn.language,
            status: status_i32_to_string(conn.status),
            safe_mode: conn.safe_mode,
        })
        .with_raw(raw))
    }

    async fn close_connection(&self, connection_id: &str) -> ApiResult<()> {
        let response = self
            .client
            .delete(format!(
                "{}/api/v1/connections/{}",
                self.base_url, connection_id
            ))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Close connection failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let raw = response.text().await.unwrap_or_default();
            return Err(ApiError::new(format!("HTTP {}", status)).with_raw(raw));
        }

        Ok(ApiResponse::new(()))
    }

    async fn get_connection(&self, connection_id: &str) -> ApiResult<ConnectionInfo> {
        let response = self
            .client
            .get(format!(
                "{}/api/v1/connections/{}",
                self.base_url, connection_id
            ))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Get connection failed: {}", e)))?;

        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            return Err(ApiError::new(format!("HTTP {}", status)).with_raw(raw));
        }

        let conn: ProtoConnectionInfo = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(ConnectionInfo {
            connection_id: conn.connection_id,
            host: conn.host,
            port: conn.port as u16,
            language: conn.language,
            status: status_i32_to_string(conn.status),
            safe_mode: conn.safe_mode,
        })
        .with_raw(raw))
    }

    async fn list_connections(&self) -> ApiResult<Vec<ConnectionInfo>> {
        let response = self
            .client
            .get(format!("{}/api/v1/connections", self.base_url))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("List connections failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        let connections: Vec<ProtoConnectionInfo> = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        let infos: Vec<ConnectionInfo> = connections
            .into_iter()
            .map(|c| ConnectionInfo {
                connection_id: c.connection_id,
                host: c.host,
                port: c.port as u16,
                language: c.language,
                status: status_i32_to_string(c.status),
                safe_mode: c.safe_mode,
            })
            .collect();

        Ok(ApiResponse::new(infos).with_raw(raw))
    }

    // ========================================================================
    // Metric Management
    // ========================================================================

    async fn add_metric(&self, request: AddMetricRequest) -> ApiResult<String> {
        // Parse location "file.py#123" format
        let (file, line) = parse_location(&request.location)?;

        // Mode as simple string (REST handler expects string, not proto object)
        let mode = request.mode.clone().unwrap_or_else(|| "stream".to_string());

        // Build stack trace slice using production enum type
        // RestStackTraceSliceDto is an enum: HeadTail(u32, u32) or Full { full: bool }
        let stack_trace_slice = if let (Some(head), Some(tail)) =
            (request.stack_trace_head, request.stack_trace_tail)
        {
            Some(RestStackTraceSliceDto::HeadTail(head, tail))
        } else if request.stack_trace_full == Some(true)
            || request.capture_stack_trace == Some(true)
        {
            // Default to full stack trace if capture is enabled
            Some(RestStackTraceSliceDto::Full { full: true })
        } else {
            None
        };

        // Use production RestCreateMetricRequest type
        let dto = RestCreateMetricRequest {
            name: request.name,
            connection_id: request.connection_id,
            group: request.group,
            location: ProtoLocation { file, line },
            expression: request.expression,
            language: "python".to_string(), // Default to python
            enabled: request.enabled.unwrap_or(true), // Default to enabled if not specified
            mode,
            safety_level: Some("strict".to_string()),
            replace: false,
            // Mode-specific configuration (use defaults)
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            // Introspection options
            capture_stack_trace: request.capture_stack_trace.unwrap_or(false),
            stack_trace_ttl: request.stack_trace_ttl,
            stack_trace_slice,
            capture_memory_snapshot: request.capture_memory_snapshot.unwrap_or(false),
            snapshot_scope: request.snapshot_scope,
            snapshot_ttl: request.snapshot_ttl,
        };

        let response = self
            .client
            .post(format!("{}/api/v1/metrics", self.base_url))
            .json(&dto)
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Add metric failed: {}", e)))?;

        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&raw) {
                return Err(ApiError::new(err.error).with_raw(raw));
            }
            return Err(ApiError::new(format!("HTTP {}: {}", status, raw)).with_raw(raw));
        }

        let resp: CreateMetricResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(resp.metric_id.to_string()).with_raw(raw))
    }

    async fn remove_metric(&self, name: &str) -> ApiResult<()> {
        // First, find the metric by name to get its ID
        let metrics = self.list_metrics().await?;
        let metric = metrics
            .data
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| ApiError::new(format!("Metric '{}' not found", name)))?;

        // Get metric ID from listing
        let response = self
            .client
            .get(format!("{}/api/v1/metrics", self.base_url))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("List metrics failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        let paginated: PaginatedMetricsResponse = serde_json::from_str(&raw)
            .map_err(|e| ApiError::new(format!("Failed to parse metrics: {}", e)))?;

        let metric_dto = paginated
            .metrics
            .iter()
            .find(|m| m.name == metric.name)
            .ok_or_else(|| ApiError::new(format!("Metric '{}' not found", name)))?;

        // Proto MetricInfo always has metric_id (u64, not optional)
        let metric_id = metric_dto.metric_id;

        let response = self
            .client
            .delete(format!("{}/api/v1/metrics/{}", self.base_url, metric_id))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Delete metric failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let raw = response.text().await.unwrap_or_default();
            return Err(ApiError::new(format!("HTTP {}", status)).with_raw(raw));
        }

        Ok(ApiResponse::new(()))
    }

    async fn toggle_metric(&self, name: &str, enabled: bool) -> ApiResult<()> {
        // Find metric ID by name
        let response = self
            .client
            .get(format!("{}/api/v1/metrics", self.base_url))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("List metrics failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        let paginated: PaginatedMetricsResponse = serde_json::from_str(&raw)
            .map_err(|e| ApiError::new(format!("Failed to parse metrics: {}", e)))?;

        let metric = paginated
            .metrics
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| ApiError::new(format!("Metric '{}' not found", name)))?;

        // Proto MetricInfo always has metric_id (u64, not optional)
        let metric_id = metric.metric_id;

        let endpoint = if enabled { "enable" } else { "disable" };
        let response = self
            .client
            .post(format!(
                "{}/api/v1/metrics/{}/{}",
                self.base_url, metric_id, endpoint
            ))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Toggle metric failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let raw = response.text().await.unwrap_or_default();
            return Err(ApiError::new(format!("HTTP {}", status)).with_raw(raw));
        }

        Ok(ApiResponse::new(()))
    }

    async fn get_metric(&self, name: &str) -> ApiResult<MetricInfo> {
        // REST API uses IDs, so we need to find by name
        let metrics = self.list_metrics().await?;
        let metric = metrics
            .data
            .into_iter()
            .find(|m| m.name == name)
            .ok_or_else(|| ApiError::new(format!("Metric '{}' not found", name)))?;

        Ok(ApiResponse::new(metric))
    }

    async fn list_metrics(&self) -> ApiResult<Vec<MetricInfo>> {
        let response = self
            .client
            .get(format!("{}/api/v1/metrics", self.base_url))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("List metrics failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        // Use production PaginatedMetricsResponse which contains proto MetricInfo
        let paginated: PaginatedMetricsResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        // Convert proto MetricInfo to client MetricInfo
        let infos: Vec<MetricInfo> = paginated
            .metrics
            .into_iter()
            .map(proto_metric_to_client_metric)
            .collect();

        Ok(ApiResponse::new(infos).with_raw(raw))
    }

    async fn query_events(&self, metric_name: &str, limit: u32) -> ApiResult<Vec<EventInfo>> {
        let response = self
            .client
            .get(format!(
                "{}/api/v1/events?metric_name={}&limit={}",
                self.base_url, metric_name, limit
            ))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Query events failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        // Use proto MetricEvent type
        let events: Vec<ProtoMetricEvent> = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        let infos: Vec<EventInfo> = events
            .into_iter()
            .map(|e| {
                // Convert timestamp to ISO string
                let timestamp_iso = chrono::DateTime::from_timestamp_micros(e.timestamp)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|| e.timestamp.to_string());

                // Calculate age in seconds
                let now = chrono::Utc::now().timestamp_micros();
                let age_seconds = (now - e.timestamp) / 1_000_000;

                // Check result oneof for value or error
                let (value, is_error) = match e.result {
                    Some(detrix_api::metric_event::Result::ValueJson(v)) => (v, false),
                    Some(detrix_api::metric_event::Result::Error(_)) => (String::new(), true),
                    None => (String::new(), false),
                };

                // Convert introspection data using proto types
                let stack_trace = e.stack_trace.as_ref().map(proto_to_core_stack_trace);
                let memory_snapshot = e
                    .memory_snapshot
                    .as_ref()
                    .map(proto_to_core_memory_snapshot);

                EventInfo {
                    metric_name: e.metric_name,
                    value,
                    timestamp_iso,
                    age_seconds,
                    is_error,
                    stack_trace,
                    memory_snapshot,
                    extra: std::collections::HashMap::new(),
                }
            })
            .collect();

        Ok(ApiResponse::new(infos).with_raw(raw))
    }

    // ========================================================================
    // Group Management
    // ========================================================================

    async fn enable_group(&self, group: &str) -> ApiResult<usize> {
        let response = self
            .client
            .post(format!("{}/api/v1/groups/{}/enable", self.base_url, group))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Enable group failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        let resp: GroupOperationResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(resp.success_count).with_raw(raw))
    }

    async fn disable_group(&self, group: &str) -> ApiResult<usize> {
        let response = self
            .client
            .post(format!("{}/api/v1/groups/{}/disable", self.base_url, group))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Disable group failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        let resp: GroupOperationResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(resp.success_count).with_raw(raw))
    }

    async fn list_groups(&self) -> ApiResult<Vec<GroupInfo>> {
        let response = self
            .client
            .get(format!("{}/api/v1/groups", self.base_url))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("List groups failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        // GroupInfo is detrix_api::GroupInfo (re-exported from client)
        let groups: Vec<GroupInfo> = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(groups).with_raw(raw))
    }

    // ========================================================================
    // Validation
    // ========================================================================

    async fn validate_expression(
        &self,
        expression: &str,
        language: &str,
    ) -> ApiResult<ValidationResult> {
        // Use proto type with metadata set to None
        let request = ProtoValidateExpressionRequest {
            expression: expression.to_string(),
            language: language.to_string(),
            safety_level: Some("strict".to_string()),
            metadata: None,
        };

        let response = self
            .client
            .post(format!("{}/api/v1/validate_expression", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Validate expression failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        let resp: ProtoValidateExpressionResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(ValidationResult {
            is_safe: resp.is_safe,
            issues: resp.violations,
        })
        .with_raw(raw))
    }

    async fn inspect_file(
        &self,
        file_path: &str,
        line: Option<u32>,
        find_variable: Option<&str>,
    ) -> ApiResult<String> {
        // Use proto type with metadata set to None
        let request = ProtoInspectFileRequest {
            file_path: file_path.to_string(),
            line,
            find_variable: find_variable.map(String::from),
            metadata: None,
        };

        let response = self
            .client
            .post(format!("{}/api/v1/inspect_file", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Inspect file failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        // Return the raw JSON as a string
        Ok(ApiResponse::new(raw.clone()).with_raw(raw))
    }

    // ========================================================================
    // Configuration
    // ========================================================================

    async fn get_config(&self) -> ApiResult<String> {
        let response = self
            .client
            .get(format!("{}/api/v1/config", self.base_url))
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Get config failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        // REST API returns different format than proto - use REST-specific DTO
        let resp: ConfigResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(resp.config_toml).with_raw(raw))
    }

    async fn update_config(&self, config_toml: &str, persist: bool) -> ApiResult<()> {
        // Use proto type with metadata set to None, merge defaults to false
        let request = ProtoUpdateConfigRequest {
            config_toml: config_toml.to_string(),
            merge: false, // REST default behavior
            persist,
            metadata: None,
        };

        let response = self
            .client
            .put(format!("{}/api/v1/config", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Update config failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let raw = response.text().await.unwrap_or_default();
            return Err(ApiError::new(format!("HTTP {}", status)).with_raw(raw));
        }

        Ok(ApiResponse::new(()))
    }

    async fn validate_config(&self, config_toml: &str) -> ApiResult<bool> {
        // Use proto type with metadata set to None
        let request = ProtoValidateConfigRequest {
            config_toml: config_toml.to_string(),
            metadata: None,
        };

        let response = self
            .client
            .post(format!("{}/api/v1/config/validate", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Validate config failed: {}", e)))?;

        let raw = response
            .text()
            .await
            .map_err(|e| ApiError::new(format!("Failed to read response: {}", e)))?;

        let resp: ProtoValidationResponse = serde_json::from_str(&raw).map_err(|e| {
            ApiError::new(format!("Failed to parse response: {}", e)).with_raw(raw.clone())
        })?;

        Ok(ApiResponse::new(resp.valid).with_raw(raw))
    }

    async fn reload_config(&self) -> ApiResult<()> {
        // Use proto type with metadata set to None
        let request = ProtoReloadConfigRequest {
            config_path: None,
            metadata: None,
        };

        let response = self
            .client
            .post(format!("{}/api/v1/config/reload", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(|e| ApiError::new(format!("Reload config failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let raw = response.text().await.unwrap_or_default();
            return Err(ApiError::new(format!("HTTP {}", status)).with_raw(raw));
        }

        Ok(ApiResponse::new(()))
    }

    // ========================================================================
    // System Events (not implemented for REST - MCP only)
    // ========================================================================

    async fn query_system_events(
        &self,
        _limit: u32,
        _unacknowledged_only: bool,
        _event_types: Option<Vec<&str>>,
    ) -> ApiResult<Vec<SystemEventInfo>> {
        Err(ApiError::new(
            "System events are not available via REST API. Use MCP instead.",
        ))
    }

    async fn acknowledge_system_events(&self, _event_ids: Vec<i64>) -> ApiResult<u64> {
        Err(ApiError::new(
            "System events are not available via REST API. Use MCP instead.",
        ))
    }

    async fn get_mcp_usage(&self) -> ApiResult<McpUsageInfo> {
        let url = format!("{}/api/v1/mcp/usage", self.base_url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ApiError::new(format!("HTTP error: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::new(format!("HTTP {} - {}", status, body)).with_raw(body.clone()));
        }

        let body = response.text().await.unwrap_or_default();
        let usage: McpUsageInfo = serde_json::from_str(&body)
            .map_err(|e| ApiError::new(format!("Failed to parse response: {}", e)))?;

        Ok(ApiResponse::new(usage).with_raw(body))
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Parse location string in format "file.py#123" to (file, line)
fn parse_location(location: &str) -> Result<(String, u32), ApiError> {
    // Handle format: "file.py#123" or "@file.py#123"
    let location = location.strip_prefix('@').unwrap_or(location);

    let parts: Vec<&str> = location.rsplitn(2, '#').collect();
    if parts.len() != 2 {
        return Err(ApiError::new(format!(
            "Invalid location format '{}'. Expected 'file#line'",
            location
        )));
    }

    let line: u32 = parts[0]
        .parse()
        .map_err(|_| ApiError::new(format!("Invalid line number: {}", parts[0])))?;
    let file = parts[1].to_string();

    Ok((file, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_location() {
        let (file, line) = parse_location("test.py#123").unwrap();
        assert_eq!(file, "test.py");
        assert_eq!(line, 123);

        let (file, line) = parse_location("@test.py#456").unwrap();
        assert_eq!(file, "test.py");
        assert_eq!(line, 456);

        let (file, line) = parse_location("/path/to/file.py#789").unwrap();
        assert_eq!(file, "/path/to/file.py");
        assert_eq!(line, 789);

        // Windows paths work naturally with # separator
        let (file, line) = parse_location("C:/Users/test.py#10").unwrap();
        assert_eq!(file, "C:/Users/test.py");
        assert_eq!(line, 10);

        assert!(parse_location("invalid").is_err());
        assert!(parse_location("file.py#abc").is_err());
    }

    #[test]
    fn test_create_metric_request_serializes_introspection_fields() {
        // Using production RestCreateMetricRequest type
        let dto = RestCreateMetricRequest {
            name: "test_metric".to_string(),
            connection_id: "conn1".to_string(),
            group: Some("group1".to_string()),
            location: ProtoLocation {
                file: "test.py".to_string(),
                line: 42,
            },
            expression: "x + y".to_string(),
            language: "python".to_string(),
            enabled: true,
            mode: "stream".to_string(),
            safety_level: Some("strict".to_string()),
            replace: false,
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            capture_stack_trace: true,
            stack_trace_ttl: Some(600),
            stack_trace_slice: Some(RestStackTraceSliceDto::Full { full: true }),
            capture_memory_snapshot: true,
            snapshot_scope: Some("local".to_string()),
            snapshot_ttl: Some(300),
        };

        let json = serde_json::to_string(&dto).unwrap();

        // Verify camelCase serialization
        assert!(json.contains("\"connectionId\":"));
        assert!(json.contains("\"mode\":\"stream\"")); // Mode is a string
        assert!(json.contains("\"captureStackTrace\":true"));
        assert!(json.contains("\"stackTraceTtl\":600"));
        assert!(json.contains("\"stackTraceSlice\":"));
        assert!(json.contains("\"captureMemorySnapshot\":true"));
        assert!(json.contains("\"snapshotScope\":\"local\""));
        assert!(json.contains("\"snapshotTtl\":300"));
    }

    #[test]
    fn test_create_metric_request_with_defaults() {
        // Using production RestCreateMetricRequest with minimal fields
        let dto = RestCreateMetricRequest {
            name: "test_metric".to_string(),
            connection_id: "conn1".to_string(),
            group: None,
            location: ProtoLocation {
                file: "test.py".to_string(),
                line: 42,
            },
            expression: "x".to_string(),
            language: "python".to_string(),
            enabled: true,
            mode: "stream".to_string(),
            safety_level: None,
            replace: false,
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
        };

        let json = serde_json::to_string(&dto).unwrap();

        // Required fields should be present
        assert!(json.contains("\"name\":\"test_metric\""));
        assert!(json.contains("\"connectionId\":\"conn1\""));
        // Boolean fields should be present (not Option)
        assert!(json.contains("\"captureStackTrace\":false"));
        assert!(json.contains("\"captureMemorySnapshot\":false"));
    }

    #[test]
    fn test_stack_trace_slice_dto_serialization() {
        // Full stack trace (using production enum)
        let full_slice = RestStackTraceSliceDto::Full { full: true };
        let json = serde_json::to_string(&full_slice).unwrap();
        assert!(json.contains("\"full\":true"));

        // Head + tail slice (using production enum)
        let partial_slice = RestStackTraceSliceDto::HeadTail(5, 3);
        let json = serde_json::to_string(&partial_slice).unwrap();
        // HeadTail serializes as [5, 3] array (untagged enum)
        assert!(json.contains("[5,3]"));
    }
}
