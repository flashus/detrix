//! gRPC client for metrics operations

use super::helpers::{metric_info_from_proto, parse_location};
use super::types::{
    AddMetricParams, ConfigValidationResult, DaemonInfoDto, ExpressionValidationResult, GroupInfo,
    InspectFileInfo, McpClientDto, McpErrorCount, McpUsageInfo, McpWorkflowStats, MetricInfo,
    ParentProcessDto, StatusInfo, WakeInfo,
};
use anyhow::{Context, Result};
use detrix_api::generated::detrix::v1::{
    metric_mode::Mode, metrics_service_client::MetricsServiceClient, AddMetricRequest,
    GetMcpUsageRequest, GetMetricRequest, GroupRequest, InspectFileRequest, ListGroupsRequest,
    ListMetricsRequest, Location, MetricMode, ReloadConfigRequest, RemoveMetricRequest,
    RequestMetadata, SleepRequest, StatusRequest, StreamMode, UpdateConfigRequest,
    UpdateMetricRequest, ValidateConfigRequest, ValidateExpressionRequest, WakeRequest,
};
use detrix_api::grpc::AuthChannel;
use detrix_core::SAFETY_STRICT;

/// gRPC client for metrics operations
pub struct MetricsClient {
    client: MetricsServiceClient<AuthChannel>,
}

impl MetricsClient {
    /// Create a new metrics client from an existing channel
    pub fn new(channel: AuthChannel) -> Self {
        Self {
            client: MetricsServiceClient::new(channel),
        }
    }

    /// Create a new metrics client using pre-discovered endpoints
    ///
    /// Use this when you have `DaemonEndpoints` from `ClientContext`
    /// to avoid redundant endpoint discovery.
    pub async fn with_endpoints(endpoints: &detrix_api::grpc::DaemonEndpoints) -> Result<Self> {
        let channel = super::connect_with_endpoints(endpoints).await?;
        Ok(Self::new(channel))
    }

    /// List all metrics
    pub async fn list_metrics(
        &mut self,
        group: Option<String>,
        enabled_only: bool,
    ) -> Result<Vec<MetricInfo>> {
        let request = ListMetricsRequest {
            group,
            enabled_only: Some(enabled_only),
            name_pattern: None,
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .list_metrics(request)
            .await
            .context("Failed to list metrics via gRPC")?
            .into_inner();

        Ok(response
            .metrics
            .into_iter()
            .map(metric_info_from_proto)
            .collect())
    }

    /// Get a metric by name
    pub async fn get_metric(&mut self, name: &str) -> Result<MetricInfo> {
        use detrix_api::generated::detrix::v1::get_metric_request::Identifier;

        let request = GetMetricRequest {
            identifier: Some(Identifier::Name(name.to_string())),
            metadata: Some(RequestMetadata::default()),
        };

        let _response = self
            .client
            .get_metric(request)
            .await
            .context("Failed to get metric via gRPC")?
            .into_inner();

        // MetricResponse has metric_id, name, status, location fields directly
        // We need to fetch the full metric info using list_metrics
        let metrics = self.list_metrics(None, false).await?;
        metrics
            .into_iter()
            .find(|m| m.name == name)
            .ok_or_else(|| anyhow::anyhow!("Metric '{}' not found", name))
    }

    /// Add a new metric
    pub async fn add_metric(&mut self, params: AddMetricParams) -> Result<MetricInfo> {
        // Parse location string (file.py#123 or just file.py)
        let (file, line) = parse_location(&params.location, params.line)?;

        // Detect language from file extension using shared utility
        let language = detrix_api::common::detect_language(&file)
            .unwrap_or("python")
            .to_string();

        // Create default stream mode
        let default_mode = MetricMode {
            mode: Some(Mode::Stream(StreamMode {})),
        };

        let request = AddMetricRequest {
            name: params.name.clone(),
            group: params.group,
            location: Some(Location {
                file: file.clone(),
                line,
            }),
            expression: params.expression,
            language,
            enabled: params.enabled,
            mode: Some(default_mode),
            condition: None,
            safety_level: SAFETY_STRICT.to_string(),
            connection_id: params.connection_id,
            replace: Some(params.replace),
            capture_stack_trace: None,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: None,
            snapshot_scope: None,
            snapshot_ttl: None,
            metadata: Some(RequestMetadata::default()),
        };

        let _response = self
            .client
            .add_metric(request)
            .await
            .context("Failed to add metric via gRPC")?
            .into_inner();

        // MetricResponse returns metric_id, name, status, location
        // Fetch the full metric info
        self.get_metric(&params.name).await
    }

    /// Remove a metric by name
    pub async fn remove_metric(&mut self, name: &str) -> Result<()> {
        use detrix_api::generated::detrix::v1::remove_metric_request::Identifier;

        let request = RemoveMetricRequest {
            identifier: Some(Identifier::Name(name.to_string())),
            metadata: Some(RequestMetadata::default()),
        };

        self.client
            .remove_metric(request)
            .await
            .context("Failed to remove metric via gRPC")?;

        Ok(())
    }

    /// Toggle a metric (enable/disable)
    pub async fn toggle_metric(&mut self, name: &str, enabled: bool) -> Result<()> {
        use detrix_api::generated::detrix::v1::ToggleMetricRequest;

        // First get the metric to find its ID
        let metric = self.get_metric(name).await?;

        let request = ToggleMetricRequest {
            metric_id: metric.metric_id,
            enabled,
            metadata: Some(RequestMetadata::default()),
        };

        self.client
            .toggle_metric(request)
            .await
            .context("Failed to toggle metric via gRPC")?;

        Ok(())
    }

    /// Update a metric
    #[allow(dead_code)]
    pub async fn update_metric(
        &mut self,
        name: &str,
        expression: Option<String>,
        enabled: Option<bool>,
    ) -> Result<MetricInfo> {
        // First get the metric to find its ID
        let metric = self.get_metric(name).await?;

        let request = UpdateMetricRequest {
            metric_id: metric.metric_id,
            expression,
            enabled,
            mode: None,
            condition: None,
            metadata: Some(RequestMetadata::default()),
        };

        self.client
            .update_metric(request)
            .await
            .context("Failed to update metric via gRPC")?;

        // Fetch the updated metric info
        self.get_metric(name).await
    }

    // ========================================================================
    // Group Operations
    // ========================================================================

    /// List all groups
    pub async fn list_groups(&mut self) -> Result<Vec<GroupInfo>> {
        let request = ListGroupsRequest {
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .list_groups(request)
            .await
            .context("Failed to list groups via gRPC")?
            .into_inner();

        Ok(response
            .groups
            .into_iter()
            .map(|g| GroupInfo {
                name: g.name,
                metric_count: g.metric_count,
                enabled_count: g.enabled_count,
            })
            .collect())
    }

    /// Enable all metrics in a group
    pub async fn enable_group(&mut self, group: &str) -> Result<u32> {
        let request = GroupRequest {
            group_name: group.to_string(),
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .enable_group(request)
            .await
            .context("Failed to enable group via gRPC")?
            .into_inner();

        Ok(response.metrics_affected as u32)
    }

    /// Disable all metrics in a group
    pub async fn disable_group(&mut self, group: &str) -> Result<u32> {
        let request = GroupRequest {
            group_name: group.to_string(),
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .disable_group(request)
            .await
            .context("Failed to disable group via gRPC")?
            .into_inner();

        Ok(response.metrics_affected as u32)
    }

    // ========================================================================
    // System Control
    // ========================================================================

    /// Get system status
    pub async fn get_status(&mut self) -> Result<StatusInfo> {
        let request = StatusRequest {
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .get_status(request)
            .await
            .context("Failed to get status via gRPC")?
            .into_inner();

        // Convert proto types to CLI DTOs
        let daemon = response.daemon.map(|d| DaemonInfoDto {
            pid: d.pid,
            ppid: d.ppid,
        });

        let mcp_clients: Vec<McpClientDto> = response
            .mcp_clients
            .into_iter()
            .map(|c| McpClientDto {
                id: c.id,
                connected_at: c.connected_at,
                connected_duration_secs: c.connected_duration_secs,
                last_activity: c.last_activity,
                last_activity_ago_secs: c.last_activity_ago_secs,
                parent_process: c.parent_process.map(|pp| ParentProcessDto {
                    pid: pp.pid,
                    name: pp.name,
                    bridge_pid: pp.bridge_pid,
                }),
            })
            .collect();

        Ok(StatusInfo {
            mode: response.mode,
            uptime_seconds: response.uptime_seconds as u64,
            uptime_formatted: response.uptime_formatted,
            started_at: response.started_at,
            active_connections: response.active_connections as u32,
            total_metrics: response.total_metrics as u32,
            enabled_metrics: response.enabled_metrics as u32,
            total_events: response.total_events as u64,
            daemon,
            mcp_clients,
            config_path: response.config_path,
        })
    }

    /// Wake the system
    pub async fn wake(&mut self) -> Result<WakeInfo> {
        let request = WakeRequest {
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .wake(request)
            .await
            .context("Failed to wake via gRPC")?
            .into_inner();

        Ok(WakeInfo {
            status: response.status,
            metrics_loaded: response.metrics_loaded,
        })
    }

    /// Sleep the system
    pub async fn sleep(&mut self) -> Result<()> {
        let request = SleepRequest {
            metadata: Some(RequestMetadata::default()),
        };

        self.client
            .sleep(request)
            .await
            .context("Failed to sleep via gRPC")?;

        Ok(())
    }

    // ========================================================================
    // Config
    // ========================================================================

    /// Get current config
    pub async fn get_config(&mut self) -> Result<String> {
        use detrix_api::generated::detrix::v1::GetConfigRequest;

        let request = GetConfigRequest {
            path: None,
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .get_config(request)
            .await
            .context("Failed to get config via gRPC")?
            .into_inner();

        Ok(response.config_toml)
    }

    /// Validate config TOML content via daemon
    pub async fn validate_config(&mut self, config_toml: &str) -> Result<ConfigValidationResult> {
        let request = ValidateConfigRequest {
            config_toml: config_toml.to_string(),
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .validate_config(request)
            .await
            .context("Failed to validate config via gRPC")?
            .into_inner();

        Ok(ConfigValidationResult {
            valid: response.valid,
            errors: response.errors,
            warnings: response.warnings,
        })
    }

    /// Reload config from disk
    pub async fn reload_config(&mut self) -> Result<()> {
        let request = ReloadConfigRequest {
            config_path: None,
            metadata: Some(RequestMetadata::default()),
        };

        self.client
            .reload_config(request)
            .await
            .context("Failed to reload config via gRPC")?;

        Ok(())
    }

    // ========================================================================
    // Validation
    // ========================================================================

    /// Validate an expression
    pub async fn validate_expression(
        &mut self,
        expression: &str,
        language: &str,
    ) -> Result<ExpressionValidationResult> {
        let request = ValidateExpressionRequest {
            expression: expression.to_string(),
            language: language.to_string(),
            metadata: Some(RequestMetadata::default()),
            safety_level: None, // Use default from config
        };

        let response = self
            .client
            .validate_expression(request)
            .await
            .context("Failed to validate expression via gRPC")?
            .into_inner();

        Ok(ExpressionValidationResult {
            is_safe: response.is_safe,
            expression: response.expression,
            language: response.language,
            violations: response.violations,
            warnings: response.warnings,
        })
    }

    // ========================================================================
    // MCP Usage
    // ========================================================================

    /// Get MCP tool usage statistics
    pub async fn get_mcp_usage(&mut self) -> Result<McpUsageInfo> {
        let request = GetMcpUsageRequest {
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .get_mcp_usage(request)
            .await
            .context("Failed to get MCP usage via gRPC")?
            .into_inner();

        let usage = response.usage.unwrap_or_default();

        Ok(McpUsageInfo {
            total_calls: usage.total_calls,
            total_success: usage.total_success,
            total_errors: usage.total_errors,
            success_rate: usage.success_rate,
            avg_latency_ms: usage.avg_latency_ms,
            top_errors: usage
                .top_errors
                .into_iter()
                .map(|e| McpErrorCount {
                    code: e.code,
                    count: e.count,
                })
                .collect(),
            workflow: usage
                .workflow
                .map(|w| McpWorkflowStats {
                    correct_workflows: w.correct_workflows,
                    skipped_inspect: w.skipped_inspect,
                    no_connection: w.no_connection,
                    adherence_rate: w.adherence_rate,
                })
                .unwrap_or_default(),
        })
    }

    // ========================================================================
    // File Inspection
    // ========================================================================

    /// Inspect a file to discover observable variables
    pub async fn inspect_file(
        &mut self,
        file_path: &str,
        line: Option<u32>,
        find_variable: Option<&str>,
    ) -> Result<InspectFileInfo> {
        let request = InspectFileRequest {
            file_path: file_path.to_string(),
            line,
            find_variable: find_variable.map(|v| v.to_string()),
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .inspect_file(request)
            .await
            .context("Failed to inspect file via gRPC")?
            .into_inner();

        if !response.success {
            if let Some(error) = response.error {
                anyhow::bail!("Inspect file failed: {}", error);
            }
            anyhow::bail!("Inspect file failed with unknown error");
        }

        Ok(InspectFileInfo {
            success: response.success,
            content: response.content,
        })
    }

    // ========================================================================
    // Config Update
    // ========================================================================

    /// Update configuration
    pub async fn update_config(
        &mut self,
        config_toml: &str,
        merge: bool,
        persist: bool,
    ) -> Result<()> {
        let request = UpdateConfigRequest {
            config_toml: config_toml.to_string(),
            merge,
            persist,
            metadata: Some(RequestMetadata::default()),
        };

        self.client
            .update_config(request)
            .await
            .context("Failed to update config via gRPC")?;

        Ok(())
    }
}
