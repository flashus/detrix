//! Metric operations: remove, toggle, get, update, list, query
//!
//! These are the metric management tools - CRUD operations on existing metrics.

use crate::grpc::conversions::metric_to_info;
use crate::mcp::conversions::events_to_display;
use crate::mcp::error::{OptionToMcpResult, ToMcpResult};
use crate::mcp::format::{format_response, ResponseFormat};
use crate::mcp::helpers;
use crate::mcp::params::{
    GetMetricParams, ListMetricsParams, QueryMetricsParams, RemoveMetricParams, ToggleMetricParams,
    UpdateMetricParams,
};
use crate::state::ApiState;
use rmcp::ErrorData as McpError;
use std::sync::Arc;
use tracing::debug;

// ============================================================================
// remove_metric Implementation
// ============================================================================

/// Result of remove_metric operation
pub struct RemoveMetricResult {
    pub name: String,
}

impl RemoveMetricResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!("Metric '{}' removed successfully", self.name)
    }
}

/// Core remove_metric implementation
pub async fn remove_metric_impl(
    state: &Arc<ApiState>,
    params: RemoveMetricParams,
) -> Result<RemoveMetricResult, McpError> {
    // First check if metric exists
    let metric = match state
        .context
        .metric_service
        .get_metric_by_name(&params.name)
        .await
    {
        Ok(Some(m)) => m,
        Ok(None) => {
            return Err(McpError::invalid_params(
                format!("Metric '{}' not found", params.name),
                None,
            ))
        }
        Err(e) => {
            return Err(McpError::internal_error(
                format!("Failed to find metric: {}", e),
                None,
            ))
        }
    };

    // Require debugger connection for actually removing the metric
    helpers::connection::require_debugger_connected(state).await?;

    if let Some(metric_id) = metric.id {
        match state.context.metric_service.remove_metric(metric_id).await {
            Ok(_) => Ok(RemoveMetricResult { name: params.name }),
            Err(e) => Err(McpError::internal_error(
                format!("Failed to remove metric: {}", e),
                None,
            )),
        }
    } else {
        Err(McpError::invalid_params(
            "Metric has no ID (should not happen)".to_string(),
            None,
        ))
    }
}

// ============================================================================
// toggle_metric Implementation
// ============================================================================

/// Result of toggle_metric operation
pub struct ToggleMetricResult {
    pub name: String,
    pub enabled: bool,
    pub storage_updated: bool,
    pub dap_confirmed: bool,
    pub actual_line: Option<u32>,
    pub dap_message: Option<String>,
}

impl ToggleMetricResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        let status = if self.enabled { "enabled" } else { "disabled" };

        let mut response = format!(
            "Metric '{}' {}\n\nDAP Status:\n- Storage updated: {}\n- DAP confirmed: {}",
            self.name, status, self.storage_updated, self.dap_confirmed
        );

        if let Some(line) = self.actual_line {
            response.push_str(&format!("\n- Actual line: {}", line));
        }

        if let Some(ref msg) = self.dap_message {
            response.push_str(&format!("\n- DAP message: {}", msg));
        }

        response
    }
}

/// Core toggle_metric implementation
pub async fn toggle_metric_impl(
    state: &Arc<ApiState>,
    params: ToggleMetricParams,
) -> Result<ToggleMetricResult, McpError> {
    // First check if metric exists
    let metric = match state
        .context
        .metric_service
        .get_metric_by_name(&params.name)
        .await
    {
        Ok(Some(m)) => m,
        Ok(None) => {
            return Err(McpError::invalid_params(
                format!("Metric '{}' not found", params.name),
                None,
            ))
        }
        Err(e) => {
            return Err(McpError::internal_error(
                format!("Failed to find metric: {}", e),
                None,
            ))
        }
    };

    // Require debugger connection for actually toggling the metric
    helpers::connection::require_debugger_connected(state).await?;

    if let Some(metric_id) = metric.id {
        match state
            .context
            .metric_service
            .toggle_metric(metric_id, params.enabled)
            .await
        {
            Ok(result) => Ok(ToggleMetricResult {
                name: params.name,
                enabled: params.enabled,
                storage_updated: result.storage_updated,
                dap_confirmed: result.dap_confirmed,
                actual_line: result.actual_line,
                dap_message: result.dap_message,
            }),
            Err(e) => Err(McpError::internal_error(
                format!("Failed to toggle metric: {}", e),
                None,
            )),
        }
    } else {
        Err(McpError::invalid_params(
            "Metric has no ID (should not happen)".to_string(),
            None,
        ))
    }
}

// ============================================================================
// get_metric Implementation
// ============================================================================

/// Result of get_metric operation
pub struct GetMetricResult {
    pub name: String,
    pub location: String,
    pub expression: String,
    pub group: Option<String>,
    pub enabled: bool,
    pub connection_id: String,
    pub mode: String,
}

impl GetMetricResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "Metric '{}': {} ({})",
            self.name,
            if self.enabled { "enabled" } else { "disabled" },
            self.location
        )
    }

    /// Build JSON response for MCP
    pub fn build_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "location": self.location,
            "expression": self.expression,
            "group": self.group,
            "enabled": self.enabled,
            "connection_id": self.connection_id,
            "mode": self.mode,
        })
    }
}

/// Core get_metric implementation
pub async fn get_metric_impl(
    state: &Arc<ApiState>,
    params: GetMetricParams,
) -> Result<GetMetricResult, McpError> {
    let metric = state
        .context
        .metric_service
        .get_metric_by_name(&params.name)
        .await
        .mcp_context("Failed to get metric")?
        .mcp_ok_or(&format!("Metric '{}' not found", params.name))?;

    Ok(GetMetricResult {
        name: metric.name.clone(),
        location: metric.location.to_string_format(),
        expression: metric.expression.clone(),
        group: metric.group.clone(),
        enabled: metric.enabled,
        connection_id: metric.connection_id.0.clone(),
        mode: format!("{:?}", metric.mode),
    })
}

// ============================================================================
// update_metric Implementation
// ============================================================================

/// Result of update_metric operation
pub struct UpdateMetricResult {
    pub name: String,
}

impl UpdateMetricResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!("Metric '{}' updated successfully", self.name)
    }
}

/// Core update_metric implementation
pub async fn update_metric_impl(
    state: &Arc<ApiState>,
    params: UpdateMetricParams,
) -> Result<UpdateMetricResult, McpError> {
    // Get the current metric first
    let mut metric = state
        .context
        .metric_service
        .get_metric_by_name(&params.name)
        .await
        .mcp_context("Failed to get metric")?
        .mcp_ok_or(&format!("Metric '{}' not found", params.name))?;

    // Apply updates
    if let Some(expression) = params.expression {
        metric.expression = expression;
    }
    if let Some(group) = params.group {
        metric.group = Some(group);
    }
    if let Some(enabled) = params.enabled {
        metric.enabled = enabled;
    }

    // Save updated metric
    state
        .context
        .metric_service
        .update_metric(&metric)
        .await
        .mcp_context("Failed to update metric")?;

    Ok(UpdateMetricResult { name: params.name })
}

// ============================================================================
// list_metrics Implementation
// ============================================================================

/// Result of list_metrics operation
pub struct ListMetricsResult {
    pub count: usize,
    pub formatted_output: String,
    pub connection_status: String,
}

impl ListMetricsResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!("Found {} metrics | {}", self.count, self.connection_status)
    }
}

/// Core list_metrics implementation
pub async fn list_metrics_impl(
    state: &Arc<ApiState>,
    params: ListMetricsParams,
) -> Result<ListMetricsResult, McpError> {
    let metrics = if let Some(ref group_name) = params.group {
        state
            .context
            .metric_service
            .list_metrics_by_group(group_name)
            .await
            .mcp_context("Failed to list metrics")?
    } else {
        state
            .context
            .metric_service
            .list_metrics()
            .await
            .mcp_context("Failed to list metrics")?
    };

    debug!("list_metrics: found {} metrics in storage", metrics.len());
    for m in &metrics {
        debug!(
            "  - metric: name={}, id={:?}, location={}:{}",
            m.name, m.id, m.location.file, m.location.line
        );
    }

    let filtered: Vec<_> = if params.enabled_only.unwrap_or(false) {
        metrics.into_iter().filter(|m| m.enabled).collect()
    } else {
        metrics
    };

    // Convert to proto MetricInfo for consistent response format
    let proto_metrics: Vec<_> = filtered
        .iter()
        .filter_map(|m| {
            metric_to_info(m)
                .inspect_err(|e| tracing::warn!(metric_name = %m.name, "Skipping metric: {}", e))
                .ok()
        })
        .collect();

    // Use TOON format by default for LLM efficiency
    let format = ResponseFormat::from_str_opt(params.format.as_deref());
    let output = format_response(&proto_metrics, format);

    let connection_status = helpers::connection::connection_status_message(state).await;

    Ok(ListMetricsResult {
        count: filtered.len(),
        formatted_output: output,
        connection_status,
    })
}

// ============================================================================
// query_metrics Implementation
// ============================================================================

/// Result of query_metrics operation
pub struct QueryMetricsResult {
    pub count: usize,
    pub query_type: String, // "metric" or "group"
    pub query_name: String,
    pub formatted_output: String,
    pub connection_status: String,
}

impl QueryMetricsResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "Found {} events for {} '{}' | {}",
            self.count, self.query_type, self.query_name, self.connection_status
        )
    }
}

/// Core query_metrics implementation
pub async fn query_metrics_impl(
    state: &Arc<ApiState>,
    params: QueryMetricsParams,
) -> Result<QueryMetricsResult, McpError> {
    // Convert u64 to i64 for repository (capped, always fits)
    let limit = params
        .limit
        .unwrap_or(detrix_config::DEFAULT_QUERY_LIMIT as u64)
        .min(detrix_config::DEFAULT_MAX_QUERY_LIMIT as u64) as i64;
    let format = ResponseFormat::from_str_opt(params.format.as_deref());

    if let Some(ref metric_name) = params.name {
        // Query by metric name
        match state
            .context
            .metric_service
            .get_metric_by_name(metric_name)
            .await
        {
            Ok(Some(metric)) => {
                if let Some(metric_id) = metric.id {
                    let events = state
                        .event_repository
                        .find_by_metric_id(metric_id, limit)
                        .await
                        .mcp_context("Failed to query events")?;

                    let display_events = events_to_display(&events);
                    let output = format_response(&display_events, format);
                    let connection_status =
                        helpers::connection::connection_status_message(state).await;

                    Ok(QueryMetricsResult {
                        count: events.len(),
                        query_type: "metric".to_string(),
                        query_name: metric_name.clone(),
                        formatted_output: output,
                        connection_status,
                    })
                } else {
                    Err(McpError::invalid_params(
                        "Metric has no ID".to_string(),
                        None,
                    ))
                }
            }
            Ok(None) => Err(McpError::invalid_params(
                format!("Metric '{}' not found", metric_name),
                None,
            )),
            Err(e) => Err(McpError::internal_error(
                format!("Failed to find metric: {}", e),
                None,
            )),
        }
    } else if let Some(ref group_name) = params.group {
        // Query by group
        let metrics = state
            .context
            .metric_service
            .list_metrics_by_group(group_name)
            .await
            .mcp_context("Failed to list metrics")?;

        let metric_ids: Vec<_> = metrics.iter().filter_map(|m| m.id).collect();

        let events = state
            .event_repository
            .find_by_metric_ids(&metric_ids, limit)
            .await
            .mcp_context("Failed to query events")?;

        let display_events = events_to_display(&events);
        let output = format_response(&display_events, format);
        let connection_status = helpers::connection::connection_status_message(state).await;

        Ok(QueryMetricsResult {
            count: events.len(),
            query_type: "group".to_string(),
            query_name: group_name.clone(),
            formatted_output: output,
            connection_status,
        })
    } else {
        Err(McpError::invalid_params(
            "Either 'name' or 'group' must be specified".to_string(),
            None,
        ))
    }
}
