//! Group management: enable, disable, list
//!
//! These tools manage metric groups for batch operations.

use crate::mcp::error::ToMcpResult;
use crate::mcp::format::{format_response, ResponseFormat};
use crate::mcp::params::{GroupParams, ListGroupsParams};
use crate::state::ApiState;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

// ============================================================================
// enable_group Implementation
// ============================================================================

/// Result of enable_group operation
pub struct EnableGroupResult {
    pub group: String,
    pub succeeded: usize,
}

impl EnableGroupResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "✅ Enabled {} metrics in group '{}'",
            self.succeeded, self.group
        )
    }
}

/// Core enable_group implementation
pub async fn enable_group_impl(
    state: &Arc<ApiState>,
    params: GroupParams,
) -> Result<EnableGroupResult, McpError> {
    let result = state
        .context
        .metric_service
        .enable_group(&params.group)
        .await
        .mcp_context("Failed to enable group")?;

    Ok(EnableGroupResult {
        group: params.group,
        succeeded: result.succeeded,
    })
}

// ============================================================================
// disable_group Implementation
// ============================================================================

/// Result of disable_group operation
pub struct DisableGroupResult {
    pub group: String,
    pub succeeded: usize,
}

impl DisableGroupResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "✅ Disabled {} metrics in group '{}'",
            self.succeeded, self.group
        )
    }
}

/// Core disable_group implementation
pub async fn disable_group_impl(
    state: &Arc<ApiState>,
    params: GroupParams,
) -> Result<DisableGroupResult, McpError> {
    let result = state
        .context
        .metric_service
        .disable_group(&params.group)
        .await
        .mcp_context("Failed to disable group")?;

    Ok(DisableGroupResult {
        group: params.group,
        succeeded: result.succeeded,
    })
}

// ============================================================================
// list_groups Implementation
// ============================================================================

/// Result of list_groups operation
pub struct ListGroupsResult {
    pub count: usize,
    pub formatted_output: String,
}

impl ListGroupsResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!("Found {} groups", self.count)
    }
}

/// Core list_groups implementation
pub async fn list_groups_impl(
    state: &Arc<ApiState>,
    params: ListGroupsParams,
) -> Result<ListGroupsResult, McpError> {
    let groups = state
        .context
        .metric_service
        .list_groups()
        .await
        .mcp_context("Failed to list groups")?;

    let group_infos: Vec<_> = groups
        .iter()
        .map(|g| {
            serde_json::json!({
                "name": g.name,
                "metric_count": g.metric_count,
                "enabled_count": g.enabled_count,
            })
        })
        .collect();

    let format = ResponseFormat::from_str_opt(params.format.as_deref());
    let output = format_response(&group_infos, format);

    Ok(ListGroupsResult {
        count: groups.len(),
        formatted_output: output,
    })
}
