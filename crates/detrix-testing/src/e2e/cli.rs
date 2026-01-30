//! CLI Client Implementation
//!
//! Implements the ApiClient trait for the CLI interface.
//! Runs detrix CLI commands and parses JSON output.

use super::client::{
    AddMetricRequest, ApiClient, ApiError, ApiResponse, ApiResult, ConnectionInfo, EventInfo,
    GroupInfo, McpErrorCountInfo, McpUsageInfo, McpWorkflowInfo, MetricInfo, StatusInfo,
    SystemEventInfo, ValidationResult,
};
use super::executor::{find_detrix_binary, get_workspace_root};
use async_trait::async_trait;
use detrix_config::constants::DEFAULT_GRPC_PORT;
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

/// CLI client that executes detrix commands
pub struct CliClient {
    /// Path to the detrix binary
    detrix_binary: PathBuf,
    /// HTTP port for daemon communication (stored for reference but not directly used)
    #[allow(dead_code)]
    http_port: u16,
    /// gRPC port for daemon communication
    grpc_port: u16,
}

impl CliClient {
    /// Create a new CLI client using the release binary
    ///
    /// Note: For testing, use `with_grpc_port` to specify the daemon's gRPC port
    pub fn new(http_port: u16) -> Self {
        // Find the detrix binary using shared utility
        let workspace_root = get_workspace_root();
        let detrix_binary = find_detrix_binary(&workspace_root).expect(
            "Could not find detrix binary. Build with: cargo build --release -p detrix-cli",
        );

        Self {
            detrix_binary,
            http_port,
            // Note: Default port matches detrix-config::ApiConfig::default().grpc.port
            // For tests, use with_grpc_port() to specify the actual port
            grpc_port: DEFAULT_GRPC_PORT,
        }
    }

    /// Create a CLI client with explicit gRPC port for testing
    pub fn with_grpc_port(http_port: u16, grpc_port: u16) -> Self {
        let workspace_root = get_workspace_root();
        let detrix_binary = find_detrix_binary(&workspace_root).expect(
            "Could not find detrix binary. Build with: cargo build --release -p detrix-cli",
        );

        Self {
            detrix_binary,
            http_port,
            grpc_port,
        }
    }

    /// Create a CLI client with explicit binary path
    pub fn with_binary(binary_path: PathBuf, http_port: u16) -> Self {
        Self {
            detrix_binary: binary_path,
            http_port,
            // Note: Default port matches detrix-config::ApiConfig::default().grpc.port
            grpc_port: DEFAULT_GRPC_PORT,
        }
    }

    /// Run a detrix CLI command and return JSON output
    async fn run_command(&self, args: &[&str]) -> Result<String, ApiError> {
        let mut cmd = Command::new(&self.detrix_binary);
        cmd.args(["--format", "json"])
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Set the gRPC port override so CLI connects to the test daemon
            // (bypasses PID file lookup which might point to a global daemon)
            .env(
                detrix_config::constants::ENV_DETRIX_GRPC_PORT_OVERRIDE,
                self.grpc_port.to_string(),
            );

        let output = cmd
            .output()
            .await
            .map_err(|e| ApiError::new(format!("Failed to execute detrix command: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            // Try to extract error message from stderr or stdout
            let error_msg = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("Command failed with exit code: {:?}", output.status.code())
            };
            return Err(ApiError::new(error_msg));
        }

        Ok(stdout)
    }

    /// Parse JSON output into a typed response
    fn parse_json<T: for<'de> Deserialize<'de>>(&self, json: &str) -> Result<T, ApiError> {
        serde_json::from_str(json).map_err(|e| {
            ApiError::new(format!(
                "Failed to parse JSON response: {} (input: {})",
                e, json
            ))
        })
    }

    /// Parse count from CLI text message like "Enabled 3 metrics in group..."
    fn parse_count_from_message(message: &str) -> usize {
        // Look for patterns like "Enabled N metrics" or "Disabled N metrics"
        for word in message.split_whitespace() {
            if let Ok(n) = word.parse::<usize>() {
                return n;
            }
        }
        0
    }
}

// ============================================================================
// Response DTOs for CLI output
// ============================================================================

#[derive(Debug, Deserialize)]
struct CliStatusResponse {
    mode: String,
    uptime_seconds: u64,
    active_connections: usize,
    total_metrics: usize,
    enabled_metrics: usize,
}

#[derive(Debug, Deserialize)]
struct CliConnectionResponse {
    connection_id: String,
    host: String,
    port: u32, // API returns u32 (proto), convert to u16
    language: String,
    status: String,
    #[serde(default)]
    safe_mode: bool,
}

/// Response structure matching the CLI's actual JSON output
/// CLI outputs location_file and location_line separately
#[derive(Debug, Deserialize)]
struct CliMetricResponse {
    name: String,
    /// Some CLI outputs use "location" as combined string
    #[serde(default)]
    location: Option<String>,
    /// Some CLI outputs use "location_file" separately
    #[serde(default)]
    location_file: Option<String>,
    /// Some CLI outputs use "location_line" separately
    #[serde(default)]
    location_line: Option<u32>,
    expression: String,
    group: Option<String>,
    enabled: bool,
    #[serde(default)]
    mode: Option<String>,
}

impl CliMetricResponse {
    /// Get the combined location string
    fn get_location(&self) -> String {
        if let Some(loc) = &self.location {
            loc.clone()
        } else if let (Some(file), Some(line)) = (&self.location_file, &self.location_line) {
            format!("{}#{}", file, line)
        } else if let Some(file) = &self.location_file {
            file.clone()
        } else {
            String::new()
        }
    }
}

/// Event response matching CLI's actual JSON output
/// CLI outputs: metric_id, metric_name, timestamp, value_json, thread_name, thread_id
#[derive(Debug, Deserialize)]
struct CliEventResponse {
    metric_name: String,
    /// CLI uses "value_json" field
    #[serde(alias = "value")]
    #[serde(default)]
    value_json: Option<String>,
    /// Timestamp in microseconds
    #[serde(default)]
    timestamp: i64,
    #[serde(default)]
    timestamp_iso: String,
    #[serde(default)]
    age_seconds: i64,
    #[serde(default)]
    is_error: bool,
}

impl CliEventResponse {
    fn get_value(&self) -> String {
        self.value_json.clone().unwrap_or_default()
    }

    fn get_timestamp_iso(&self) -> String {
        if !self.timestamp_iso.is_empty() {
            self.timestamp_iso.clone()
        } else if self.timestamp > 0 {
            // Convert microseconds to ISO format
            use chrono::{TimeZone, Utc};
            let secs = self.timestamp / 1_000_000;
            let nanos = ((self.timestamp % 1_000_000) * 1000) as u32;
            if let Some(dt) = Utc.timestamp_opt(secs, nanos).single() {
                dt.to_rfc3339()
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    }
}

#[derive(Debug, Deserialize)]
struct CliGroupResponse {
    name: String,
    metric_count: u32,  // Proto uses uint32
    enabled_count: u32, // Proto uses uint32
}

/// Response structure matching the CLI's actual JSON output for validation
/// CLI outputs `{"valid":true/false,"message":"..."}` format
#[derive(Debug, Deserialize)]
struct CliValidationResponse {
    /// CLI uses "valid" field
    #[serde(alias = "is_safe")]
    valid: bool,
    /// CLI uses "message" field for validation details
    #[serde(default)]
    message: Option<String>,
    /// Some outputs may use "issues" array
    #[serde(default)]
    issues: Vec<String>,
}

impl CliValidationResponse {
    /// Convert to the common ValidationResult format
    fn to_validation_result(&self) -> ValidationResult {
        let issues = if !self.issues.is_empty() {
            self.issues.clone()
        } else if let Some(msg) = &self.message {
            if !self.valid && !msg.is_empty() {
                vec![msg.clone()]
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        ValidationResult {
            is_safe: self.valid,
            issues,
        }
    }
}

#[derive(Debug, Deserialize)]
struct CliUsageResponse {
    total_calls: u64,
    total_success: u64,
    total_errors: u64,
    success_rate: f64,
    avg_latency_ms: f64,
    #[serde(default)]
    top_errors: Vec<CliErrorCount>,
    workflow: CliWorkflowStats,
}

#[derive(Debug, Deserialize)]
struct CliErrorCount {
    code: String,
    count: u64,
}

#[derive(Debug, Deserialize)]
struct CliWorkflowStats {
    correct_workflows: u64,
    skipped_inspect: u64,
    no_connection: u64,
    adherence_rate: f64,
}

#[derive(Debug, Deserialize)]
struct CliSystemEventResponse {
    #[serde(default)]
    id: Option<i64>,
    event_type: String,
    #[serde(default)]
    timestamp_iso: String,
    #[serde(default)]
    age_seconds: i64,
    #[serde(default)]
    connection_id: Option<String>,
    #[serde(default)]
    metric_id: Option<i64>,
    #[serde(default)]
    metric_name: Option<String>,
    #[serde(default)]
    details: Option<serde_json::Value>,
    #[serde(default)]
    acknowledged: bool,
    #[serde(default)]
    timestamp_us: i64,
}

// ============================================================================
// ApiClient Implementation
// ============================================================================

#[async_trait]
impl ApiClient for CliClient {
    fn api_type(&self) -> &'static str {
        "CLI"
    }

    // ========================================================================
    // Health & Status
    // ========================================================================

    async fn health(&self) -> Result<bool, ApiError> {
        // Use status command to check health
        match self.run_command(&["status"]).await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    async fn get_status(&self) -> ApiResult<StatusInfo> {
        let output = self.run_command(&["status"]).await?;
        let status: CliStatusResponse = self.parse_json(&output)?;

        Ok(ApiResponse::new(StatusInfo {
            mode: status.mode,
            uptime_seconds: status.uptime_seconds,
            active_connections: status.active_connections,
            total_metrics: status.total_metrics,
            enabled_metrics: status.enabled_metrics,
        }))
    }

    async fn wake(&self) -> ApiResult<String> {
        let output = self.run_command(&["wake"]).await?;
        Ok(ApiResponse::new(output.trim().to_string()))
    }

    async fn sleep(&self) -> ApiResult<String> {
        let output = self.run_command(&["sleep"]).await?;
        Ok(ApiResponse::new(output.trim().to_string()))
    }

    // ========================================================================
    // Connection Management
    // ========================================================================

    async fn create_connection(
        &self,
        host: &str,
        port: u16,
        language: &str,
    ) -> ApiResult<ConnectionInfo> {
        let output = self
            .run_command(&[
                "connection",
                "create",
                "--host",
                host,
                "--port",
                &port.to_string(),
                "--language",
                language,
            ])
            .await?;

        let conn: CliConnectionResponse = self.parse_json(&output)?;

        Ok(ApiResponse::new(ConnectionInfo {
            connection_id: conn.connection_id,
            host: conn.host,
            port: conn.port as u16, // Convert from proto u32
            language: conn.language,
            status: conn.status,
            safe_mode: conn.safe_mode,
        }))
    }

    async fn create_connection_with_program(
        &self,
        host: &str,
        port: u16,
        language: &str,
        program: Option<&str>,
    ) -> ApiResult<ConnectionInfo> {
        // Convert port to string for lifetime purposes
        let port_str = port.to_string();
        let args_with_port: Vec<&str> = vec![
            "connection",
            "create",
            "--host",
            host,
            "--port",
            &port_str,
            "--language",
            language,
        ];

        let output = if let Some(prog) = program {
            let mut full_args = args_with_port.clone();
            full_args.push("--program");
            full_args.push(prog);
            self.run_command(&full_args).await?
        } else {
            self.run_command(&args_with_port).await?
        };

        let conn: CliConnectionResponse = self.parse_json(&output)?;

        Ok(ApiResponse::new(ConnectionInfo {
            connection_id: conn.connection_id,
            host: conn.host,
            port: conn.port as u16,
            language: conn.language,
            status: conn.status,
            safe_mode: conn.safe_mode,
        }))
    }

    async fn close_connection(&self, connection_id: &str) -> ApiResult<()> {
        self.run_command(&["connection", "close", connection_id])
            .await?;
        Ok(ApiResponse::new(()))
    }

    async fn get_connection(&self, connection_id: &str) -> ApiResult<ConnectionInfo> {
        let output = self
            .run_command(&["connection", "get", connection_id])
            .await?;

        let conn: CliConnectionResponse = self.parse_json(&output)?;

        Ok(ApiResponse::new(ConnectionInfo {
            connection_id: conn.connection_id,
            host: conn.host,
            port: conn.port as u16,
            language: conn.language,
            status: conn.status,
            safe_mode: conn.safe_mode,
        }))
    }

    async fn list_connections(&self) -> ApiResult<Vec<ConnectionInfo>> {
        let output = self.run_command(&["connection", "list"]).await?;

        // Handle empty list case
        if output.trim().is_empty() || output.trim() == "[]" {
            return Ok(ApiResponse::new(vec![]));
        }

        let conns: Vec<CliConnectionResponse> = self.parse_json(&output)?;

        Ok(ApiResponse::new(
            conns
                .into_iter()
                .map(|c| ConnectionInfo {
                    connection_id: c.connection_id,
                    host: c.host,
                    port: c.port as u16,
                    language: c.language,
                    status: c.status,
                    safe_mode: c.safe_mode,
                })
                .collect(),
        ))
    }

    // ========================================================================
    // Metric Management
    // ========================================================================

    async fn add_metric(&self, request: AddMetricRequest) -> ApiResult<String> {
        let mut args = vec![
            "metric".to_string(),
            "add".to_string(),
            request.name.clone(),
            "--location".to_string(),
            request.location.clone(),
            "--expression".to_string(),
            request.expression.clone(),
            "--connection".to_string(),
            request.connection_id.clone(),
        ];

        if let Some(group) = &request.group {
            args.push("--group".to_string());
            args.push(group.clone());
        }

        // Note: CLI doesn't support --disabled flag, metrics are enabled by default
        // We'll disable after creation if needed

        if let Some(capture_stack_trace) = request.capture_stack_trace {
            if capture_stack_trace {
                args.push("--capture-stack-trace".to_string());
            }
        }

        if let Some(capture_memory_snapshot) = request.capture_memory_snapshot {
            if capture_memory_snapshot {
                args.push("--capture-memory-snapshot".to_string());
            }
        }

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let _output = self.run_command(&args_refs).await?;

        // If metric should be disabled, disable it now
        if let Some(enabled) = request.enabled {
            if !enabled {
                self.run_command(&["metric", "disable", &request.name])
                    .await?;
            }
        }

        // Return the metric name
        Ok(ApiResponse::new(request.name))
    }

    async fn remove_metric(&self, name: &str) -> ApiResult<()> {
        self.run_command(&["metric", "remove", name]).await?;
        Ok(ApiResponse::new(()))
    }

    async fn toggle_metric(&self, name: &str, enabled: bool) -> ApiResult<()> {
        let action = if enabled { "enable" } else { "disable" };
        self.run_command(&["metric", action, name]).await?;
        Ok(ApiResponse::new(()))
    }

    async fn get_metric(&self, name: &str) -> ApiResult<MetricInfo> {
        let output = self.run_command(&["metric", "get", name]).await?;
        let metric: CliMetricResponse = self.parse_json(&output)?;

        let location = metric.get_location();
        Ok(ApiResponse::new(MetricInfo {
            name: metric.name,
            location,
            expression: metric.expression,
            group: metric.group,
            enabled: metric.enabled,
            mode: metric.mode,
        }))
    }

    async fn list_metrics(&self) -> ApiResult<Vec<MetricInfo>> {
        let output = self.run_command(&["metric", "list"]).await?;

        // Handle empty list case
        if output.trim().is_empty() || output.trim() == "[]" {
            return Ok(ApiResponse::new(vec![]));
        }

        let metrics: Vec<CliMetricResponse> = self.parse_json(&output)?;

        Ok(ApiResponse::new(
            metrics
                .into_iter()
                .map(|m| {
                    let location = m.get_location();
                    MetricInfo {
                        name: m.name,
                        location,
                        expression: m.expression,
                        group: m.group,
                        enabled: m.enabled,
                        mode: m.mode,
                    }
                })
                .collect(),
        ))
    }

    async fn query_events(&self, metric_name: &str, limit: u32) -> ApiResult<Vec<EventInfo>> {
        let output = self
            .run_command(&[
                "event",
                "query",
                "--metric",
                metric_name,
                "--limit",
                &limit.to_string(),
            ])
            .await?;

        // Handle empty list case
        if output.trim().is_empty() || output.trim() == "[]" {
            return Ok(ApiResponse::new(vec![]));
        }

        let events: Vec<CliEventResponse> = self.parse_json(&output)?;

        Ok(ApiResponse::new(
            events
                .into_iter()
                .map(|e| {
                    let value = e.get_value();
                    let timestamp_iso = e.get_timestamp_iso();
                    EventInfo {
                        metric_name: e.metric_name,
                        value,
                        timestamp_iso,
                        age_seconds: e.age_seconds,
                        is_error: e.is_error,
                        stack_trace: None,
                        memory_snapshot: None,
                        extra: Default::default(),
                    }
                })
                .collect(),
        ))
    }

    // ========================================================================
    // Group Management
    // ========================================================================

    async fn enable_group(&self, group: &str) -> ApiResult<usize> {
        let output = self.run_command(&["group", "enable", group]).await?;
        // CLI outputs text: "Enabled N metrics in group 'name'"
        // Try to parse count from message
        let count = Self::parse_count_from_message(&output);
        Ok(ApiResponse::new(count))
    }

    async fn disable_group(&self, group: &str) -> ApiResult<usize> {
        let output = self.run_command(&["group", "disable", group]).await?;
        // CLI outputs text: "Disabled N metrics in group 'name'"
        let count = Self::parse_count_from_message(&output);
        Ok(ApiResponse::new(count))
    }

    async fn list_groups(&self) -> ApiResult<Vec<GroupInfo>> {
        let output = self.run_command(&["group", "list"]).await?;

        // Handle empty list case
        if output.trim().is_empty() || output.trim() == "[]" {
            return Ok(ApiResponse::new(vec![]));
        }

        let groups: Vec<CliGroupResponse> = self.parse_json(&output)?;

        Ok(ApiResponse::new(
            groups
                .into_iter()
                .map(|g| GroupInfo {
                    name: g.name,
                    metric_count: g.metric_count,
                    enabled_count: g.enabled_count,
                })
                .collect(),
        ))
    }

    // ========================================================================
    // Validation
    // ========================================================================

    async fn validate_expression(
        &self,
        expression: &str,
        language: &str,
    ) -> ApiResult<ValidationResult> {
        let output = self
            .run_command(&["validate", expression, "--language", language])
            .await?;

        let result: CliValidationResponse = self.parse_json(&output)?;

        Ok(ApiResponse::new(result.to_validation_result()))
    }

    async fn inspect_file(
        &self,
        file_path: &str,
        line: Option<u32>,
        find_variable: Option<&str>,
    ) -> ApiResult<String> {
        let mut args = vec!["inspect".to_string(), file_path.to_string()];

        if let Some(l) = line {
            args.push("--line".to_string());
            args.push(l.to_string());
        }

        if let Some(var) = find_variable {
            args.push("--find-variable".to_string());
            args.push(var.to_string());
        }

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.run_command(&args_refs).await?;
        Ok(ApiResponse::new(output))
    }

    // ========================================================================
    // Configuration
    // ========================================================================

    async fn get_config(&self) -> ApiResult<String> {
        let output = self.run_command(&["config", "get"]).await?;
        Ok(ApiResponse::new(output))
    }

    async fn update_config(&self, config_toml: &str, persist: bool) -> ApiResult<()> {
        // Write config to temp file
        let temp_path = std::env::temp_dir().join("detrix_test_update_config.toml");
        std::fs::write(&temp_path, config_toml)
            .map_err(|e| ApiError::new(format!("Failed to write temp config: {}", e)))?;

        let mut args = vec![
            "config".to_string(),
            "update".to_string(),
            "--file".to_string(),
            temp_path.to_str().unwrap().to_string(),
        ];

        if persist {
            args.push("--persist".to_string());
        }

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_command(&args_refs).await?;

        // Clean up
        let _ = std::fs::remove_file(&temp_path);

        Ok(ApiResponse::new(()))
    }

    async fn validate_config(&self, config_toml: &str) -> ApiResult<bool> {
        // Write config to temp file
        let temp_path = std::env::temp_dir().join("detrix_test_config.toml");
        std::fs::write(&temp_path, config_toml)
            .map_err(|e| ApiError::new(format!("Failed to write temp config: {}", e)))?;

        // CLI expects --file flag for file path
        let output = self
            .run_command(&["config", "validate", "--file", temp_path.to_str().unwrap()])
            .await?;

        // Clean up
        let _ = std::fs::remove_file(&temp_path);

        // Check if valid
        Ok(ApiResponse::new(
            output.contains("valid") || output.contains("OK"),
        ))
    }

    async fn reload_config(&self) -> ApiResult<()> {
        self.run_command(&["config", "reload"]).await?;
        Ok(ApiResponse::new(()))
    }

    // ========================================================================
    // System Events
    // ========================================================================

    async fn query_system_events(
        &self,
        limit: u32,
        unacknowledged_only: bool,
        event_types: Option<Vec<&str>>,
    ) -> ApiResult<Vec<SystemEventInfo>> {
        let mut args = vec![
            "system-event".to_string(),
            "query".to_string(),
            "--limit".to_string(),
            limit.to_string(),
        ];

        if unacknowledged_only {
            args.push("--unacked".to_string());
        }

        if let Some(types) = event_types {
            if !types.is_empty() {
                args.push("--event-type".to_string());
                args.push(types.join(","));
            }
        }

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.run_command(&args_refs).await?;

        // Handle empty list case
        if output.trim().is_empty() || output.trim() == "[]" {
            return Ok(ApiResponse::new(vec![]));
        }

        let events: Vec<CliSystemEventResponse> = self.parse_json(&output)?;

        Ok(ApiResponse::new(
            events
                .into_iter()
                .map(|e| SystemEventInfo {
                    id: e.id,
                    event_type: e.event_type,
                    timestamp_iso: e.timestamp_iso,
                    age_seconds: e.age_seconds,
                    connection_id: e.connection_id,
                    metric_id: e.metric_id,
                    metric_name: e.metric_name,
                    details: e.details,
                    acknowledged: e.acknowledged,
                    timestamp_us: e.timestamp_us,
                })
                .collect(),
        ))
    }

    async fn acknowledge_system_events(&self, _event_ids: Vec<i64>) -> ApiResult<u64> {
        // ack command not yet implemented in CLI
        Err(ApiError::new(
            "acknowledge_system_events not yet implemented in CLI",
        ))
    }

    // ========================================================================
    // MCP Usage Statistics
    // ========================================================================

    async fn get_mcp_usage(&self) -> ApiResult<McpUsageInfo> {
        let output = self.run_command(&["usage"]).await?;
        let usage: CliUsageResponse = self.parse_json(&output)?;

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
                correct_workflows: usage.workflow.correct_workflows,
                skipped_inspect: usage.workflow.skipped_inspect,
                no_connection: usage.workflow.no_connection,
                adherence_rate: usage.workflow.adherence_rate,
            },
        }))
    }
}
