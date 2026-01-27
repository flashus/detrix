//! HTTP Bridge Methods for DetrixServer
//!
//! These public methods allow HTTP handlers to call MCP tools directly,
//! providing a bridge between the HTTP API and MCP tool implementations.
//!
//! Each method wraps the corresponding MCP tool, converting the McpError
//! to a simple String for HTTP response handling.

use super::DetrixServer;
use crate::mcp::error::HttpBridgeResultExt;
use crate::mcp::params::{
    AcknowledgeEventsParams, AddMetricParams, CloseConnectionParams, CreateConnectionParams,
    EnableFromDiffParams, GetConfigParams, GetConnectionParams, GetMetricParams, GroupParams,
    InspectFileParams, ListConnectionsParams, ListGroupsParams, ListMetricsParams, ObserveParams,
    QueryMetricsParams, QuerySystemEventsParams, RemoveMetricParams, ToggleMetricParams,
    UpdateConfigParams, UpdateMetricParams, ValidateConfigParams, ValidateExpressionParams,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

impl DetrixServer {
    /// Get all registered tools from the tool router.
    /// This is used by the HTTP MCP handler to dynamically list tools
    /// instead of maintaining a separate hardcoded list.
    pub fn get_tools(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
    }

    /// Call wake tool via HTTP bridge
    pub async fn call_wake(&self) -> Result<CallToolResult, String> {
        self.wake().await.http_bridge()
    }

    /// Call sleep tool via HTTP bridge
    pub async fn call_sleep(&self) -> Result<CallToolResult, String> {
        self.sleep().await.http_bridge()
    }

    /// Call add_metric tool via HTTP bridge
    pub async fn call_add_metric(&self, params: AddMetricParams) -> Result<CallToolResult, String> {
        self.add_metric(Parameters(params)).await.http_bridge()
    }

    /// Call observe tool via HTTP bridge
    pub async fn call_observe(&self, params: ObserveParams) -> Result<CallToolResult, String> {
        self.observe(Parameters(params)).await.http_bridge()
    }

    /// Call enable_from_diff tool via HTTP bridge
    pub async fn call_enable_from_diff(
        &self,
        params: EnableFromDiffParams,
    ) -> Result<CallToolResult, String> {
        self.enable_from_diff(Parameters(params))
            .await
            .http_bridge()
    }

    /// Call remove_metric tool via HTTP bridge
    pub async fn call_remove_metric(
        &self,
        params: RemoveMetricParams,
    ) -> Result<CallToolResult, String> {
        self.remove_metric(Parameters(params)).await.http_bridge()
    }

    /// Call toggle_metric tool via HTTP bridge
    pub async fn call_toggle_metric(
        &self,
        params: ToggleMetricParams,
    ) -> Result<CallToolResult, String> {
        self.toggle_metric(Parameters(params)).await.http_bridge()
    }

    /// Call list_metrics tool via HTTP bridge
    pub async fn call_list_metrics(
        &self,
        params: ListMetricsParams,
    ) -> Result<CallToolResult, String> {
        self.list_metrics(Parameters(params)).await.http_bridge()
    }

    /// Call query_metrics tool via HTTP bridge
    pub async fn call_query_metrics(
        &self,
        params: QueryMetricsParams,
    ) -> Result<CallToolResult, String> {
        self.query_metrics(Parameters(params)).await.http_bridge()
    }

    /// Call validate_expression tool via HTTP bridge
    pub async fn call_validate_expression(
        &self,
        params: ValidateExpressionParams,
    ) -> Result<CallToolResult, String> {
        self.validate_expression(Parameters(params))
            .await
            .http_bridge()
    }

    /// Call inspect_file tool via HTTP bridge
    pub async fn call_inspect_file(
        &self,
        params: InspectFileParams,
    ) -> Result<CallToolResult, String> {
        self.inspect_file(Parameters(params)).await.http_bridge()
    }

    /// Call create_connection tool via HTTP bridge
    pub async fn call_create_connection(
        &self,
        params: CreateConnectionParams,
    ) -> Result<CallToolResult, String> {
        self.create_connection(Parameters(params))
            .await
            .http_bridge()
    }

    /// Call close_connection tool via HTTP bridge
    pub async fn call_close_connection(
        &self,
        params: CloseConnectionParams,
    ) -> Result<CallToolResult, String> {
        self.close_connection(Parameters(params))
            .await
            .http_bridge()
    }

    /// Call get_connection tool via HTTP bridge
    pub async fn call_get_connection(
        &self,
        params: GetConnectionParams,
    ) -> Result<CallToolResult, String> {
        self.get_connection(Parameters(params)).await.http_bridge()
    }

    /// Call list_connections tool via HTTP bridge
    pub async fn call_list_connections(
        &self,
        params: ListConnectionsParams,
    ) -> Result<CallToolResult, String> {
        self.list_connections(Parameters(params))
            .await
            .http_bridge()
    }

    /// Call get_status tool via HTTP bridge
    pub async fn call_get_status(&self) -> Result<CallToolResult, String> {
        self.get_status().await.http_bridge()
    }

    /// Call get_metric tool via HTTP bridge
    pub async fn call_get_metric(&self, params: GetMetricParams) -> Result<CallToolResult, String> {
        self.get_metric(Parameters(params)).await.http_bridge()
    }

    /// Call update_metric tool via HTTP bridge
    pub async fn call_update_metric(
        &self,
        params: UpdateMetricParams,
    ) -> Result<CallToolResult, String> {
        self.update_metric(Parameters(params)).await.http_bridge()
    }

    /// Call enable_group tool via HTTP bridge
    pub async fn call_enable_group(&self, params: GroupParams) -> Result<CallToolResult, String> {
        self.enable_group(Parameters(params)).await.http_bridge()
    }

    /// Call disable_group tool via HTTP bridge
    pub async fn call_disable_group(&self, params: GroupParams) -> Result<CallToolResult, String> {
        self.disable_group(Parameters(params)).await.http_bridge()
    }

    /// Call list_groups tool via HTTP bridge
    pub async fn call_list_groups(
        &self,
        params: ListGroupsParams,
    ) -> Result<CallToolResult, String> {
        self.list_groups(Parameters(params)).await.http_bridge()
    }

    /// Call get_config tool via HTTP bridge
    pub async fn call_get_config(&self, params: GetConfigParams) -> Result<CallToolResult, String> {
        self.get_config(Parameters(params)).await.http_bridge()
    }

    /// Call update_config tool via HTTP bridge
    pub async fn call_update_config(
        &self,
        params: UpdateConfigParams,
    ) -> Result<CallToolResult, String> {
        self.update_config(Parameters(params)).await.http_bridge()
    }

    /// Call validate_config tool via HTTP bridge
    pub async fn call_validate_config(
        &self,
        params: ValidateConfigParams,
    ) -> Result<CallToolResult, String> {
        self.validate_config(Parameters(params)).await.http_bridge()
    }

    /// Call reload_config tool via HTTP bridge
    pub async fn call_reload_config(&self) -> Result<CallToolResult, String> {
        self.reload_config().await.http_bridge()
    }

    /// Call query_system_events tool via HTTP bridge
    pub async fn call_query_system_events(
        &self,
        params: QuerySystemEventsParams,
    ) -> Result<CallToolResult, String> {
        self.query_system_events(Parameters(params))
            .await
            .http_bridge()
    }

    /// Call acknowledge_events tool via HTTP bridge
    pub async fn call_acknowledge_events(
        &self,
        params: AcknowledgeEventsParams,
    ) -> Result<CallToolResult, String> {
        self.acknowledge_events(Parameters(params))
            .await
            .http_bridge()
    }

    /// Call get_mcp_usage tool via HTTP bridge
    pub async fn call_get_mcp_usage(&self) -> Result<CallToolResult, String> {
        self.get_mcp_usage().await.http_bridge()
    }
}
