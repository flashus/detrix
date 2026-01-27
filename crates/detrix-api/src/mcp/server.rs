//! MCP server implementation for Detrix
//!
//! This server exposes Detrix functionality to LLM agents via the Model Context Protocol.
//!
//! # Module Structure
//!
//! Parameter types have been extracted to `params.rs` for maintainability.
//! This file contains:
//! - `DetrixServer` struct
//! - Tool implementations (via `#[tool_router]` macro)
//! - `ServerHandler` implementation

use crate::mcp::error::ToMcpResult;
use crate::mcp::instrumentation::McpInstrumentation;
use crate::mcp::params::{
    AcknowledgeEventsParams, AddMetricParams, CloseConnectionParams, CreateConnectionParams,
    EnableFromDiffParams, GetConfigParams, GetConnectionParams, GetMetricParams, GroupParams,
    InspectFileParams, ListConnectionsParams, ListGroupsParams, ListMetricsParams, ObserveParams,
    QueryMetricsParams, QuerySystemEventsParams, RemoveMetricParams, ToggleMetricParams,
    UpdateConfigParams, UpdateMetricParams, ValidateConfigParams, ValidateExpressionParams,
};
use crate::state::ApiState;
// McpErrorCode and McpUsageEvent used internally by McpUsageService
#[allow(unused_imports)]
use detrix_application::{CallTimer, McpErrorCode, McpUsageEvent};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router, ErrorData as McpError,
};
use std::sync::Arc;

// ============================================================================
// MCP Server
// ============================================================================

/// Detrix MCP server for multi-language applications
///
/// **CRITICAL FOR AI AGENTS:**
/// This server lets you observe running code WITHOUT adding print() or log.debug().
/// Use these tools to inspect running processes dynamically instead of modifying source code.
///
/// **Supported Languages:** Python (debugpy), Go (delve), Rust (lldb-dap)
/// **Coming Soon:** Node.js
///
/// **Key Principle:** Observe, don't modify. Add metrics to running code with zero downtime.
#[derive(Clone)]
pub struct DetrixServer {
    state: Arc<ApiState>,
    tool_router: ToolRouter<Self>,
    instrumentation: McpInstrumentation,
}

#[allow(unused_imports)]
use detrix_application::DapAdapterRef;

#[tool_router]
impl DetrixServer {
    /// Create a new MCP server with the given API state
    pub fn new(state: Arc<ApiState>) -> Self {
        let instrumentation = McpInstrumentation::new(
            state.context.mcp_usage.clone(),
            state.mcp_usage_repository.clone(),
        );

        Self {
            state,
            tool_router: Self::tool_router(),
            instrumentation,
        }
    }

    // =========================================================================
    // MCP Usage Instrumentation (delegated to instrumentation module)
    // =========================================================================

    /// Start tracking a tool call, returns a timer
    fn start_tool_call(&self, tool_name: &str) -> detrix_application::CallTimer {
        self.instrumentation.start_tool_call(tool_name)
    }

    /// Finish tracking a successful tool call and persist if repository available
    fn finish_tool_success(&self, timer: detrix_application::CallTimer) {
        self.instrumentation.finish_tool_success(timer)
    }

    /// Finish tracking a failed tool call with error classification and persist
    fn finish_tool_error(&self, timer: detrix_application::CallTimer, error: &McpError) {
        self.instrumentation.finish_tool_error(timer, error)
    }

    // =========================================================================
    // MCP Tools
    // =========================================================================

    /// Wake Detrix - check status and auto-start daemon if needed
    ///
    /// This is the recommended first tool to call. The MCP bridge automatically
    /// starts the daemon if it's not running, so calling wake() ensures Detrix
    /// is ready to accept connections.
    #[tool(
        description = "Check Detrix status. Auto-starts daemon if not running. Call this first to ensure Detrix is ready."
    )]
    async fn wake(&self) -> Result<CallToolResult, McpError> {
        let result = tools::wake_impl(&self.state).await?;
        Ok(CallToolResult::success(vec![Content::text(
            result.build_message(),
        )]))
    }

    /// Put Detrix to sleep (stop observing Python code, zero overhead)
    #[tool(description = "Stop all connections. Zero overhead when sleeping.")]
    async fn sleep(&self) -> Result<CallToolResult, McpError> {
        let result = tools::sleep_impl(&self.state).await;
        Ok(CallToolResult::success(vec![Content::text(
            result.build_message(),
        )]))
    }

    /// Add a new metric - USE THIS INSTEAD OF print() or log.debug()
    #[tool(
        description = "Add observation point at @file:line. USE INSTEAD of print(). Requires connection_id from create_connection."
    )]
    async fn add_metric(
        &self,
        Parameters(params): Parameters<AddMetricParams>,
    ) -> Result<CallToolResult, McpError> {
        let timer = self.start_tool_call("add_metric");

        match tools::add_metric_impl(&self.state, params).await {
            Ok(result) => {
                self.finish_tool_success(timer);
                let message = result.build_message();
                let json = result.build_json();
                Ok(CallToolResult::success(vec![
                    Content::text(&message),
                    Content::text(
                        serde_json::to_string_pretty(&json).unwrap_or_else(|_| {
                            format!("{{\"metric_id\": {}}}", result.metric_id.0)
                        }),
                    ),
                ]))
            }
            Err(e) => {
                self.finish_tool_error(timer, &e);
                Err(e)
            }
        }
    }

    /// Observe a value in code - simplified one-call workflow.
    ///
    /// This tool combines connection selection, file inspection, and metric creation
    /// into a single call. It's the recommended way to add metrics for most use cases.
    ///
    /// # Examples
    /// ```ignore
    /// // Simplest usage - just file and expression
    /// observe(file="payment.py", expression="amount")
    ///
    /// // With explicit line
    /// observe(file="payment.py", expression="amount", line=127)
    ///
    /// // With TTL for temporary debugging
    /// observe(file="auth.py", expression="user.session", ttl_seconds=300)
    /// ```
    #[tool(
        description = "Observe a value in code. Simplest way to add metrics - auto-finds line if not specified."
    )]
    async fn observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, McpError> {
        let timer = self.start_tool_call("observe");

        match tools::observe_impl(&self.state, params, None).await {
            Ok(result) => {
                self.finish_tool_success(timer);
                let message = result.build_message();
                let json = result.build_json();
                Ok(CallToolResult::success(vec![
                    Content::text(&message),
                    Content::text(
                        serde_json::to_string_pretty(&json).unwrap_or_else(|_| {
                            format!("{{\"metric_id\": {}}}", result.metric_id.0)
                        }),
                    ),
                ]))
            }
            Err(e) => {
                self.finish_tool_error(timer, &e);
                Err(e)
            }
        }
    }

    /// Enable metrics from git diff containing print/log statements.
    #[tool(
        description = "Enable metrics from git diff. Parses print/log statements and creates observation points automatically."
    )]
    async fn enable_from_diff(
        &self,
        Parameters(params): Parameters<EnableFromDiffParams>,
    ) -> Result<CallToolResult, McpError> {
        let timer = self.start_tool_call("enable_from_diff");

        match tools::enable_from_diff_impl(&self.state, params).await {
            Ok(Ok(result)) => {
                self.finish_tool_success(timer);
                let message = result.build_message();
                let json = result.build_json();
                Ok(CallToolResult::success(vec![
                    Content::text(&message),
                    Content::text(serde_json::to_string_pretty(&json).unwrap_or_else(|_| {
                        "{\"error\": \"failed to serialize response\"}".to_string()
                    })),
                ]))
            }
            Ok(Err(tools::NoDebugStatementsFound)) => {
                self.finish_tool_success(timer);
                Ok(CallToolResult::success(vec![Content::text(
                    "No debug print/log statements found in diff.\n\
                     \n\
                     Supported patterns:\n\
                     - Python: print(f\"{x}\"), logger.debug(f\"{x}\")\n\
                     - Go: fmt.Printf(\"%v\", x), log.Printf(\"%v\", x)\n\
                     - Rust: dbg!(x), println!(\"{:?}\", x)",
                )]))
            }
            Err(e) => {
                self.finish_tool_error(timer, &e);
                Err(e)
            }
        }
    }

    /// Remove a debugging metric from Python code observation
    #[tool(description = "Remove metric by name. Deletes metric and historical data.")]
    async fn remove_metric(
        &self,
        Parameters(params): Parameters<RemoveMetricParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::remove_metric_impl(&self.state, params).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(
                result.build_message(),
            )])),
            Err(e) => Err(e),
        }
    }

    /// Pause/resume observing a specific point in Python code
    #[tool(description = "Enable/disable metric without removing. Keeps historical data.")]
    async fn toggle_metric(
        &self,
        Parameters(params): Parameters<ToggleMetricParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::toggle_metric_impl(&self.state, params).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(
                result.build_message(),
            )])),
            Err(e) => Err(e),
        }
    }

    /// List all Python debugging metrics (active observation points)
    #[tool(description = "List all metrics. Filter by group or enabled status.")]
    async fn list_metrics(
        &self,
        Parameters(params): Parameters<ListMetricsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::list_metrics_impl(&self.state, params).await {
            Ok(result) => Ok(CallToolResult::success(vec![
                Content::text(result.build_message()),
                Content::text(result.formatted_output),
            ])),
            Err(e) => Err(e),
        }
    }

    /// Review captured values from Python code observation
    #[tool(
        description = "Query captured values by metric name or group. Returns events with timestamps."
    )]
    async fn query_metrics(
        &self,
        Parameters(params): Parameters<QueryMetricsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::query_metrics_impl(&self.state, params).await {
            Ok(result) => Ok(CallToolResult::success(vec![
                Content::text(result.build_message()),
                Content::text(result.formatted_output),
            ])),
            Err(e) => Err(e),
        }
    }

    /// Validate a Language expression for safety before observing
    #[tool(description = "Check if expression is safe. Prevents eval/exec/file I/O.")]
    async fn validate_expression(
        &self,
        Parameters(params): Parameters<ValidateExpressionParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = tools::validate_expression_impl(&self.state, params)?;
        Ok(CallToolResult::success(vec![
            Content::text(result.build_message()),
            Content::text(result.build_json()),
        ]))
    }

    /// Inspect a source file to find correct line placement for metrics
    ///
    /// For Python files: Full AST analysis (variable scope, definitions)
    /// For other languages: Code context extraction around specified line
    #[tool(
        description = "Find correct line for metric. Returns variables at line. USE BEFORE add_metric."
    )]
    async fn inspect_file(
        &self,
        Parameters(params): Parameters<InspectFileParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = tools::inspect_file_impl(params)?;
        Ok(CallToolResult::success(result.messages))
    }

    // ========================================================================
    // Connection Management Tools
    // ========================================================================

    /// Create a new debugger connection
    #[tool(
        description = "Connect to debugger at host:port. Returns connection_id. CALL FIRST after starting debugger.

Language-specific setup:
- Python: `python -m debugpy --listen HOST:PORT --wait-for-client script.py`
- Go: `dlv debug --headless --listen=:PORT --api-version=2`
- Rust: `detrix lldb-serve ./target/debug/binary --listen 127.0.0.1:PORT`
  Binary MUST be built with debug symbols (cargo build, NOT --release).
  The lldb-serve wrapper handles program launch and type formatters automatically."
    )]
    async fn create_connection(
        &self,
        Parameters(params): Parameters<CreateConnectionParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::create_connection_impl(&self.state, params).await {
            Ok(result) => {
                // Update MCP tracker with new connection count
                let adapter_count = self
                    .state
                    .context
                    .adapter_lifecycle_manager
                    .adapter_count()
                    .await;
                self.state
                    .mcp_client_tracker
                    .update_connection_count(adapter_count);

                let json = serde_json::to_string_pretty(&result.build_json())
                    .unwrap_or_else(|_| "Error serializing connection".to_string());

                Ok(CallToolResult::success(vec![
                    Content::text(result.build_message()),
                    Content::text(json),
                ]))
            }
            Err(e) => Err(e),
        }
    }

    /// Close a debugger connection
    #[tool(
        description = "Disconnect from a debugpy process. Use when you're done debugging or want to release the connection."
    )]
    async fn close_connection(
        &self,
        Parameters(params): Parameters<CloseConnectionParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::close_connection_impl(&self.state, params).await {
            Ok(result) => {
                // Update MCP tracker with new connection count
                let adapter_count = self
                    .state
                    .context
                    .adapter_lifecycle_manager
                    .adapter_count()
                    .await;
                self.state
                    .mcp_client_tracker
                    .update_connection_count(adapter_count);

                Ok(CallToolResult::success(vec![Content::text(
                    result.build_message(),
                )]))
            }
            Err(e) => Err(e),
        }
    }

    /// Get details about a specific connection
    #[tool(description = "Get detailed information about a specific debugger connection by ID.")]
    async fn get_connection(
        &self,
        Parameters(params): Parameters<GetConnectionParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::get_connection_impl(&self.state, params).await {
            Ok(result) => {
                let json = serde_json::to_string_pretty(&result.build_json())
                    .unwrap_or_else(|_| "Error serializing connection".to_string());

                Ok(CallToolResult::success(vec![
                    Content::text(result.build_message()),
                    Content::text(json),
                ]))
            }
            Err(e) => Err(e),
        }
    }

    /// List all debugger connections
    #[tool(
        description = "List all debugger connections. Optionally filter to show only active (connected) connections."
    )]
    async fn list_connections(
        &self,
        Parameters(params): Parameters<ListConnectionsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::list_connections_impl(&self.state, params).await {
            Ok(result) => Ok(CallToolResult::success(vec![
                Content::text(result.build_message()),
                Content::text(result.formatted_output),
            ])),
            Err(e) => Err(e),
        }
    }

    // ========================================================================
    // Status and System Tools
    // ========================================================================

    /// Get system status including uptime, connections, and metrics count
    #[tool(
        description = "Get Detrix system status including uptime, active connections, and metrics count."
    )]
    async fn get_status(&self) -> Result<CallToolResult, McpError> {
        let adapter_count = self
            .state
            .context
            .adapter_lifecycle_manager
            .adapter_count()
            .await;

        let metrics = self
            .state
            .context
            .metric_service
            .list_metrics()
            .await
            .mcp_context("Failed to list metrics")?;

        let enabled_count = metrics.iter().filter(|m| m.enabled).count();
        let uptime_secs = self.state.uptime_seconds();

        let status_json = serde_json::to_string_pretty(&serde_json::json!({
            "mode": if adapter_count > 0 { "active" } else { "sleeping" },
            "uptime_seconds": uptime_secs,
            "active_connections": adapter_count,
            "total_metrics": metrics.len(),
            "enabled_metrics": enabled_count,
        }))
        .unwrap_or_else(|_| "Error serializing status".to_string());

        Ok(CallToolResult::success(vec![
            Content::text(format!(
                "Status: {} | Uptime: {}s | {} connection(s) | {}/{} metrics enabled",
                if adapter_count > 0 {
                    "active"
                } else {
                    "sleeping"
                },
                uptime_secs,
                adapter_count,
                enabled_count,
                metrics.len()
            )),
            Content::text(status_json),
        ]))
    }

    // ========================================================================
    // Metric Management Tools
    // ========================================================================

    /// Get a specific metric by name
    #[tool(description = "Get details about a specific metric by name.")]
    async fn get_metric(
        &self,
        Parameters(params): Parameters<GetMetricParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::get_metric_impl(&self.state, params).await {
            Ok(result) => {
                let json = serde_json::to_string_pretty(&result.build_json())
                    .unwrap_or_else(|_| "Error serializing metric".to_string());

                Ok(CallToolResult::success(vec![
                    Content::text(result.build_message()),
                    Content::text(json),
                ]))
            }
            Err(e) => Err(e),
        }
    }

    /// Update an existing metric
    #[tool(
        description = "Update an existing metric's properties (expression, group, enabled status)."
    )]
    async fn update_metric(
        &self,
        Parameters(params): Parameters<UpdateMetricParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::update_metric_impl(&self.state, params).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(
                result.build_message(),
            )])),
            Err(e) => Err(e),
        }
    }

    // ========================================================================
    // Group Management Tools
    // ========================================================================

    /// Enable all metrics in a group
    #[tool(description = "Enable all metrics in a specific group.")]
    async fn enable_group(
        &self,
        Parameters(params): Parameters<GroupParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::enable_group_impl(&self.state, params).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(
                result.build_message(),
            )])),
            Err(e) => Err(e),
        }
    }

    /// Disable all metrics in a group
    #[tool(description = "Disable all metrics in a specific group.")]
    async fn disable_group(
        &self,
        Parameters(params): Parameters<GroupParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::disable_group_impl(&self.state, params).await {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(
                result.build_message(),
            )])),
            Err(e) => Err(e),
        }
    }

    /// List all metric groups
    #[tool(description = "List all metric groups with their metric counts.")]
    async fn list_groups(
        &self,
        Parameters(params): Parameters<ListGroupsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::list_groups_impl(&self.state, params).await {
            Ok(result) => Ok(CallToolResult::success(vec![
                Content::text(result.build_message()),
                Content::text(result.formatted_output),
            ])),
            Err(e) => Err(e),
        }
    }

    // ========================================================================
    // Configuration Tools
    // ========================================================================

    /// Get current configuration
    #[tool(description = "Get the current Detrix configuration as TOML.")]
    async fn get_config(
        &self,
        Parameters(params): Parameters<GetConfigParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = tools::get_config_impl(&self.state, params).await?;
        Ok(CallToolResult::success(vec![
            Content::text(result.build_message()),
            Content::text(result.config_toml),
        ]))
    }

    /// Update configuration with partial TOML
    #[tool(
        description = "Update Detrix configuration with partial TOML. Only runtime-safe settings can be changed (no port/storage path changes)."
    )]
    async fn update_config(
        &self,
        Parameters(params): Parameters<UpdateConfigParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = tools::update_config_impl(&self.state, params).await?;
        let mut messages = vec![Content::text(result.build_message())];
        if let Some(warnings) = result.build_warnings() {
            messages.push(Content::text(warnings));
        }
        Ok(CallToolResult::success(messages))
    }

    /// Validate configuration without applying it
    #[tool(
        description = "Validate TOML configuration without applying it. Returns any errors found."
    )]
    async fn validate_config(
        &self,
        Parameters(params): Parameters<ValidateConfigParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = tools::validate_config_impl(&self.state, params);
        Ok(CallToolResult::success(vec![Content::text(
            result.build_message(),
        )]))
    }

    /// Reload configuration from file
    #[tool(description = "Reload configuration from the config file on disk.")]
    async fn reload_config(&self) -> Result<CallToolResult, McpError> {
        let result = tools::reload_config_impl(&self.state).await?;
        Ok(CallToolResult::success(vec![Content::text(
            result.build_message(),
        )]))
    }

    // ========================================================================
    // System Event Tools
    // ========================================================================

    /// Query system events (crashes, connections, metric changes)
    #[tool(
        description = "Query system events like debugger crashes, connection changes, and metric lifecycle events. Use for catch-up after disconnect or to check what happened. Events are persisted in database for reliable retrieval."
    )]
    async fn query_system_events(
        &self,
        Parameters(params): Parameters<QuerySystemEventsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::query_system_events_impl(&self.state, params).await? {
            Ok(result) => Ok(CallToolResult::success(vec![
                Content::text(result.build_message()),
                Content::text(result.formatted_output),
            ])),
            Err(not_configured) => Ok(CallToolResult::success(vec![Content::text(
                not_configured.build_message(),
            )])),
        }
    }

    /// Acknowledge system events (mark as read)
    #[tool(
        description = "Mark system events as acknowledged/read. Use this after processing events to avoid seeing them again in unacknowledged_only queries."
    )]
    async fn acknowledge_events(
        &self,
        Parameters(params): Parameters<AcknowledgeEventsParams>,
    ) -> Result<CallToolResult, McpError> {
        match tools::acknowledge_events_impl(&self.state, params).await? {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(
                result.build_message(),
            )])),
            Err(Ok(not_configured)) => Ok(CallToolResult::success(vec![Content::text(
                not_configured.build_message(),
            )])),
            Err(Err(no_ids)) => Ok(CallToolResult::success(vec![Content::text(
                no_ids.build_message(),
            )])),
        }
    }

    /// Get MCP usage metrics for prompt tuning and error analysis
    #[tool(
        description = "Get MCP tool usage statistics. Shows success rates, common errors, and workflow patterns. Use for debugging agent behavior and tuning prompts."
    )]
    async fn get_mcp_usage(&self) -> Result<CallToolResult, McpError> {
        let result = tools::get_mcp_usage_impl(&self.state);
        Ok(CallToolResult::success(vec![
            Content::text(result.build_message()),
            Content::text(result.build_json()),
        ]))
    }
}

mod http_bridge;

// Tool implementation helpers for reducing server.rs complexity
// Contains MetricBuilder, response builders, and validation helpers
mod tools;

// Implement the ServerHandler trait to define server capabilities
#[tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for DetrixServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "# Detrix - Observe Code WITHOUT Modifications\n\n\
                 **USE INSTEAD of print()/log.debug() statements**\n\n\
                 ## Quick Start\n\
                 1. Start debugger (in terminal):\n\
                    - Python: `python -m debugpy --listen 5678 --wait-for-client script.py`\n\
                    - Go: `dlv debug --headless --listen=:2345 --api-version=2`\n\
                    - Rust: `detrix lldb-serve ./target/debug/binary --listen 127.0.0.1:4711`\n\
                 2. create_connection(host, port, language)\n\
                 3. observe(file, expression) or add_metric(...)\n\n\
                 ## Simplified Workflow\n\
                 ```\n\
                 observe(file='auth.py', expression='user.session_id')\n\
                 ```\n\
                 The `observe` tool auto-finds the line and creates the metric!\n\n\
                 ## Location Format (for add_metric)\n\
                 `file.py#line` or `@file.py#line` (@ optional)\n\n\
                 ## Auto Features\n\
                 - Single connection? Automatically selected!\n\
                 - No line specified? Auto-found from expression!\n\
                 - No metric name? Auto-generated!\n\n\
                 ## Supported Languages\n\
                 Python (debugpy), Go (delve), Rust (lldb-serve)\n\n\
                 ## Tips\n\
                 - Use `observe` for simplest workflow\n\
                 - Use inspect_file to explore files first\n\
                 - Use get_mcp_usage to see error patterns"
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

// ============================================================================
// Internal Unit Tests
// ============================================================================
// These tests have access to private methods and test tool logic directly.

#[cfg(test)]
mod tests;
