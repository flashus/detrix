//! MCP HTTP handler for JSON-RPC over HTTP
//!
//! Following Clean Architecture:
//! - Parse JSON-RPC requests from HTTP POST body
//! - Delegate to MCP server tools
//! - Return JSON-RPC responses
//!
//! NO business logic here - just protocol translation!

use crate::mcp::{
    AcknowledgeEventsParams, AddMetricParams, CloseConnectionParams, CreateConnectionParams,
    DetrixServer, EnableFromDiffParams, GetConfigParams, GetConnectionParams, GetMetricParams,
    GroupParams, InspectFileParams, ListConnectionsParams, ListGroupsParams, ListMetricsParams,
    ObserveParams, QueryMetricsParams, QuerySystemEventsParams, RemoveMetricParams,
    ToggleMetricParams, UpdateConfigParams, UpdateMetricParams, ValidateConfigParams,
    ValidateExpressionParams,
};
use crate::mcp_client_tracker::{McpClientId, ParentProcessInfo};
use crate::state::ApiState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use rmcp::ServerHandler;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, error, info};

/// Extract parent process info from HTTP headers
///
/// Headers used:
/// - `X-Detrix-Parent-Pid`: Parent process ID (the IDE/editor)
/// - `X-Detrix-Parent-Name`: Parent process name (windsurf, claude, cursor, etc.)
/// - `X-Detrix-Bridge-Pid`: Bridge process ID (the MCP bridge process)
fn extract_parent_process_info(headers: &HeaderMap) -> Option<ParentProcessInfo> {
    let parent_pid = headers
        .get("X-Detrix-Parent-Pid")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())?;

    let parent_name = headers
        .get("X-Detrix-Parent-Name")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())?;

    let bridge_pid = headers
        .get("X-Detrix-Bridge-Pid")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())?;

    Some(ParentProcessInfo {
        pid: parent_pid,
        name: parent_name,
        bridge_pid,
    })
}

/// Macro for generating HTTP tool router from tool definitions.
///
/// Generates match arms for `handle_tools_call` based on a single list of tool definitions,
/// eliminating the need to maintain tool lists in multiple places. When adding a new MCP tool:
/// 1. Add the `#[tool]` method in `mcp/server.rs`
/// 2. Add the HTTP bridge method (`call_*`) in `mcp/server.rs`
/// 3. Add the tool to this macro invocation (ONE place for HTTP routing!)
///
/// # Compile-Time Safety
///
/// - **Forward check**: If you add a tool here but no `call_*` method exists → compile error
/// - **Reverse check**: Test `test_http_router_covers_all_mcp_tools` verifies all MCP tools are routed
///
/// # Usage
///
/// ```ignore
/// http_tool_router!(mcp_server, arguments, tool_name,
///     // Tools without parameters
///     { wake, sleep, get_status },
///     // Tools with parameters: tool_name => ParamType
///     { add_metric => AddMetricParams, remove_metric => RemoveMetricParams }
/// )
/// ```
macro_rules! http_tool_router {
    (
        $server:expr,
        $arguments:expr,
        $tool_name:expr,
        // Group 1: No-param tools in braces
        { $( $no_param:ident ),* $(,)? },
        // Group 2: Param tools in braces: tool => Type
        { $( $param_tool:ident => $param_type:ty ),* $(,)? }
    ) => {
        paste::paste! {
            match $tool_name {
                // No-param tools: "wake" => mcp_server.call_wake().await
                $(
                    stringify!($no_param) => $server.[<call_ $no_param>]().await,
                )*
                // Param tools: "add_metric" => { deserialize then call }
                $(
                    stringify!($param_tool) => match serde_json::from_value::<$param_type>($arguments.clone()) {
                        Ok(params) => $server.[<call_ $param_tool>](params).await,
                        Err(e) => Err(format!("Invalid parameters: {}", e)),
                    },
                )*
                _ => Err(format!("Unknown tool: {}", $tool_name)),
            }
        }
    };
}

/// Macro to generate a const array of all HTTP-routed tool names.
/// Used by tests to verify all MCP tools are covered by the HTTP router.
macro_rules! http_router_tool_names {
    (
        { $( $no_param:ident ),* $(,)? },
        { $( $param_tool:ident => $param_type:ty ),* $(,)? }
    ) => {
        &[
            $( stringify!($no_param), )*
            $( stringify!($param_tool), )*
        ]
    };
}

/// List of all tools handled by the HTTP router.
/// This MUST match the tools in `http_tool_router!` invocation.
/// Test `test_http_router_covers_all_mcp_tools` verifies this matches MCP server tools.
pub const HTTP_ROUTER_TOOLS: &[&str] = http_router_tool_names!(
    // Tools without parameters (must match http_tool_router! below)
    {
        wake,
        sleep,
        get_status,
        reload_config,
        get_mcp_usage
    },
    // Tools with parameters (must match http_tool_router! below)
    {
        add_metric => (),
        remove_metric => (),
        toggle_metric => (),
        list_metrics => (),
        query_metrics => (),
        validate_expression => (),
        inspect_file => (),
        create_connection => (),
        close_connection => (),
        get_connection => (),
        list_connections => (),
        get_metric => (),
        update_metric => (),
        enable_group => (),
        disable_group => (),
        list_groups => (),
        get_config => (),
        update_config => (),
        validate_config => (),
        query_system_events => (),
        acknowledge_events => (),
        observe => (),
        enable_from_diff => ()
    }
);

/// MCP HTTP handler
///
/// Handles JSON-RPC 2.0 requests over HTTP POST for MCP protocol
///
/// Tracks MCP bridge clients via X-Detrix-Client-Id header for daemon lifecycle management.
/// When daemon is spawned by MCP (--mcp-spawned), it auto-shutdowns when all clients disconnect.
pub async fn mcp_handler(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, McpHttpError> {
    info!("MCP HTTP: Received request");
    debug!("MCP HTTP: Request payload: {:?}", payload);

    // Track MCP client activity for daemon lifecycle management
    if let Some(client_id_header) = headers.get("X-Detrix-Client-Id") {
        if let Ok(client_id_str) = client_id_header.to_str() {
            let client_id = McpClientId::from_string(client_id_str);

            // Check if we have parent process info from headers
            let parent_info = extract_parent_process_info(&headers);

            // Touch the client (implicit heartbeat)
            state.mcp_client_tracker.touch(&client_id).await;

            // Update parent process info if provided (only updates if client exists)
            if let Some(info) = parent_info {
                state
                    .mcp_client_tracker
                    .update_parent_process(&client_id, info)
                    .await;
            }

            debug!("MCP HTTP: Client activity tracked: {}", client_id_str);
        }
    }

    // Validate JSON-RPC 2.0 format
    let jsonrpc_version = payload.get("jsonrpc").and_then(|v| v.as_str());
    if jsonrpc_version != Some("2.0") {
        error!("MCP HTTP: Invalid JSON-RPC version: {:?}", jsonrpc_version);
        return Err(McpHttpError::InvalidJsonRpc(
            "Invalid JSON-RPC version (must be '2.0')".to_string(),
        ));
    }

    // Create MCP server instance
    let mcp_server = DetrixServer::new(Arc::clone(&state));

    // Parse the method from the request
    let method = payload.get("method").and_then(|v| v.as_str());
    let id = payload.get("id").cloned();
    let params = payload.get("params").cloned().unwrap_or(Value::Null);

    let response_value = match method {
        Some("initialize") => {
            info!("MCP HTTP: Handling initialize request");
            let server_info = mcp_server.get_info();
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": server_info.capabilities,
                    "serverInfo": {
                        "name": "detrix",
                        "version": "1.0.0"
                    },
                    "instructions": server_info.instructions
                }
            })
        }
        Some("notifications/initialized") => {
            // No-op for initialized notification
            info!("MCP HTTP: Client initialized notification received");
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            })
        }
        Some("tools/list") => {
            info!("MCP HTTP: Handling tools/list request");
            handle_tools_list(&mcp_server, id)
        }
        Some("tools/call") => {
            info!("MCP HTTP: Handling tools/call request");
            handle_tools_call(mcp_server, id, params).await
        }
        Some(_method_name) => {
            error!("MCP HTTP: Method not yet implemented: {}", _method_name);
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("Method not implemented: {}", _method_name)
                }
            })
        }
        None => {
            error!("MCP HTTP: Missing method in request");
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32600,
                    "message": "Invalid Request: missing method"
                }
            })
        }
    };

    debug!("MCP HTTP: Response: {:?}", response_value);
    Ok(Json(response_value))
}

/// MCP heartbeat handler
///
/// Lightweight endpoint for MCP bridge clients to send periodic heartbeats
/// to keep their registration alive on the daemon. This ensures clients
/// remain registered even during idle periods (no JSON-RPC requests).
///
/// The X-Detrix-Client-Id header is required to identify the client.
pub async fn mcp_heartbeat_handler(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
) -> StatusCode {
    // Track MCP client activity
    if let Some(client_id_header) = headers.get("X-Detrix-Client-Id") {
        if let Ok(client_id_str) = client_id_header.to_str() {
            let client_id = McpClientId::from_string(client_id_str);
            state.mcp_client_tracker.touch(&client_id).await;
            debug!("MCP heartbeat: Client activity tracked: {}", client_id_str);
            return StatusCode::OK;
        }
    }

    // No client ID provided
    debug!("MCP heartbeat: Missing X-Detrix-Client-Id header");
    StatusCode::BAD_REQUEST
}

/// MCP disconnect handler
///
/// Called by MCP bridge clients when they are shutting down gracefully.
/// This immediately unregisters the client from the daemon, allowing
/// the daemon to trigger auto-shutdown if no other clients are connected.
///
/// The X-Detrix-Client-Id header is required to identify the client.
pub async fn mcp_disconnect_handler(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
) -> StatusCode {
    // Unregister MCP client
    if let Some(client_id_header) = headers.get("X-Detrix-Client-Id") {
        if let Ok(client_id_str) = client_id_header.to_str() {
            let client_id = McpClientId::from_string(client_id_str);
            state.mcp_client_tracker.unregister(&client_id).await;
            info!("MCP disconnect: Client unregistered: {}", client_id_str);
            return StatusCode::OK;
        }
    }

    // No client ID provided
    debug!("MCP disconnect: Missing X-Detrix-Client-Id header");
    StatusCode::BAD_REQUEST
}

/// Handle tools/list request - returns all available tools dynamically from the tool router
///
/// This uses the rmcp tool_router to get tool definitions, ensuring the HTTP MCP handler
/// always reflects the same tools as the native MCP server. No more hardcoded tool lists!
fn handle_tools_list(mcp_server: &DetrixServer, id: Option<Value>) -> Value {
    // Get tools dynamically from the tool router
    let tools = mcp_server.get_tools();

    // Serialize tools to JSON - rmcp::model::Tool is already serializable
    let tools_json = serde_json::to_value(&tools).unwrap_or_else(|e| {
        error!("Failed to serialize tools: {}", e);
        serde_json::json!([])
    });

    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": tools_json
        }
    })
}

/// Handle tools/call request - execute the requested tool
async fn handle_tools_call(mcp_server: DetrixServer, id: Option<Value>, params: Value) -> Value {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    info!(
        "MCP HTTP: Calling tool '{}' with arguments: {:?}",
        tool_name, arguments
    );

    // HTTP tool router generated from macro - add new tools here!
    // When adding a new MCP tool:
    // 1. Add the #[tool] method in mcp/server.rs
    // 2. Add the HTTP bridge method (call_*) in mcp/server.rs
    // 3. Add the tool below (no-param tools go in first group, param tools in second)
    let result = http_tool_router!(mcp_server, arguments, tool_name,
        // Tools without parameters
        {
            wake,
            sleep,
            get_status,
            reload_config,
            get_mcp_usage
        },
        // Tools with parameters: tool_name => ParamType
        {
            add_metric => AddMetricParams,
            remove_metric => RemoveMetricParams,
            toggle_metric => ToggleMetricParams,
            list_metrics => ListMetricsParams,
            query_metrics => QueryMetricsParams,
            validate_expression => ValidateExpressionParams,
            inspect_file => InspectFileParams,
            create_connection => CreateConnectionParams,
            close_connection => CloseConnectionParams,
            get_connection => GetConnectionParams,
            list_connections => ListConnectionsParams,
            get_metric => GetMetricParams,
            update_metric => UpdateMetricParams,
            enable_group => GroupParams,
            disable_group => GroupParams,
            list_groups => ListGroupsParams,
            get_config => GetConfigParams,
            update_config => UpdateConfigParams,
            validate_config => ValidateConfigParams,
            query_system_events => QuerySystemEventsParams,
            acknowledge_events => AcknowledgeEventsParams,
            observe => ObserveParams,
            enable_from_diff => EnableFromDiffParams
        }
    );

    match result {
        Ok(call_result) => {
            // Convert CallToolResult to JSON response
            // Note: In rmcp 0.12, call_result.content is Vec<Content>, not Option<Vec<Content>>
            let content: Vec<Value> = call_result
                .content
                .into_iter()
                .map(|c| {
                    // Serialize each Content item; Content serializes to {"type": "text", "text": "..."}
                    serde_json::to_value(&c)
                        .unwrap_or_else(|_| serde_json::json!({"type": "unknown"}))
                })
                .collect();

            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": content,
                    "isError": call_result.is_error.unwrap_or(false)
                }
            })
        }
        Err(e) => {
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32603,
                    "message": e
                }
            })
        }
    }
}

/// MCP HTTP error type
#[derive(Debug)]
pub enum McpHttpError {
    InvalidJsonRpc(String),
    Internal(String),
}

impl IntoResponse for McpHttpError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            McpHttpError::InvalidJsonRpc(msg) => (StatusCode::BAD_REQUEST, msg),
            McpHttpError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };

        let body = Json(serde_json::json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -32603,
                "message": error_message
            },
            "id": null
        }));

        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::create_test_state;

    #[tokio::test]
    async fn test_mcp_handler_initialize() {
        let (state, _temp_dir) = create_test_state().await;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "id": 1
        });

        let headers = HeaderMap::new();
        let result = mcp_handler(State(state), headers, Json(request)).await;
        assert!(result.is_ok(), "initialize should succeed");

        let response = result.unwrap().0;
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert!(response["result"]["serverInfo"]["name"] == "detrix");
    }

    #[tokio::test]
    async fn test_mcp_handler_tools_list() {
        let (state, _temp_dir) = create_test_state().await;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        });

        let headers = HeaderMap::new();
        let result = mcp_handler(State(state), headers, Json(request)).await;
        assert!(result.is_ok(), "tools/list should succeed");

        let response = result.unwrap().0;
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);

        let tools = response["result"]["tools"].as_array().unwrap();
        assert!(!tools.is_empty(), "Should return tools");

        // Check for specific tools
        let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(tool_names.contains(&"add_metric"));
        assert!(tool_names.contains(&"list_metrics"));
        assert!(tool_names.contains(&"wake"));
        assert!(tool_names.contains(&"create_connection"));
    }

    #[tokio::test]
    async fn test_mcp_handler_tools_call_list_metrics() {
        let (state, _temp_dir) = create_test_state().await;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "list_metrics",
                "arguments": {}
            },
            "id": 1
        });

        let headers = HeaderMap::new();
        let result = mcp_handler(State(state), headers, Json(request)).await;
        assert!(result.is_ok(), "tools/call list_metrics should succeed");

        let response = result.unwrap().0;
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert!(response["result"]["content"].is_array());
    }

    #[tokio::test]
    async fn test_mcp_handler_tools_call_wake() {
        let (state, _temp_dir) = create_test_state().await;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "wake",
                "arguments": {}
            },
            "id": 1
        });

        let headers = HeaderMap::new();
        let result = mcp_handler(State(state), headers, Json(request)).await;
        assert!(result.is_ok(), "tools/call wake should succeed");

        let response = result.unwrap().0;
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
    }

    #[tokio::test]
    async fn test_mcp_handler_invalid_jsonrpc() {
        let (state, _temp_dir) = create_test_state().await;

        let request = serde_json::json!({
            "method": "tools/list",
            "id": 1
        });

        let headers = HeaderMap::new();
        let result = mcp_handler(State(state), headers, Json(request)).await;
        assert!(result.is_err(), "Should fail with invalid JSON-RPC");
    }

    #[tokio::test]
    async fn test_mcp_handler_unknown_tool() {
        let (state, _temp_dir) = create_test_state().await;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "nonexistent_tool",
                "arguments": {}
            },
            "id": 1
        });

        let headers = HeaderMap::new();
        let result = mcp_handler(State(state), headers, Json(request)).await;
        assert!(result.is_ok());

        let response = result.unwrap().0;
        assert!(
            response["error"].is_object(),
            "Should return error for unknown tool"
        );
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unknown tool"));
    }

    #[tokio::test]
    async fn test_mcp_handler_tracks_client_activity() {
        let (state, _temp_dir) = create_test_state().await;

        // Verify no clients initially
        assert_eq!(state.mcp_client_tracker.active_client_count().await, 0);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "id": 1
        });

        // Create headers with client ID
        let mut headers = HeaderMap::new();
        headers.insert("X-Detrix-Client-Id", "test-client-123".parse().unwrap());

        let result = mcp_handler(State(Arc::clone(&state)), headers, Json(request)).await;
        assert!(result.is_ok());

        // Client should have been auto-registered via touch()
        assert_eq!(state.mcp_client_tracker.active_client_count().await, 1);
    }

    /// Test that MCP tools/call response content is correctly formatted
    /// Each content item should be an object, not nested arrays
    /// See: https://spec.modelcontextprotocol.io/specification/server/tools/#calling-tools
    #[tokio::test]
    async fn test_mcp_content_format_is_flat_array() {
        let (state, _temp_dir) = create_test_state().await;

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "list_metrics",
                "arguments": {}
            },
            "id": 1
        });

        let headers = HeaderMap::new();
        let result = mcp_handler(State(state), headers, Json(request)).await;
        assert!(result.is_ok(), "tools/call list_metrics should succeed");

        let response = result.unwrap().0;
        let content = response["result"]["content"]
            .as_array()
            .expect("content should be an array");

        // Each item in content should be an object (not a nested array)
        for (i, item) in content.iter().enumerate() {
            assert!(
                item.is_object(),
                "content[{}] should be an object with 'type' and 'text', got: {}",
                i,
                item
            );
            assert!(
                item.get("type").is_some(),
                "content[{}] should have 'type' field, got: {}",
                i,
                item
            );
        }

        // If there are items, verify they have correct structure
        if !content.is_empty() {
            let first_item = &content[0];
            assert_eq!(
                first_item.get("type").and_then(|v| v.as_str()),
                Some("text"),
                "content[0].type should be 'text'"
            );
            assert!(
                first_item.get("text").is_some(),
                "content[0] should have 'text' field"
            );
        }
    }

    /// Test Content serialization format directly
    #[test]
    fn test_content_serializes_to_object() {
        use rmcp::model::Content;

        let content = Content::text("Hello World");
        let json = serde_json::to_value(&content).unwrap();

        // Content should serialize to an object, not an array
        assert!(
            json.is_object(),
            "Content should serialize to object, got: {}",
            json
        );
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some("text"));
        assert_eq!(
            json.get("text").and_then(|v| v.as_str()),
            Some("Hello World")
        );
    }

    /// Test that Vec<Content> serializes correctly when flattened
    #[test]
    fn test_vec_content_serializes_correctly() {
        use rmcp::model::Content;

        let contents: Vec<Content> = vec![Content::text("First"), Content::text("Second")];

        // Serialize each content item individually (correct approach)
        let json_vec: Vec<serde_json::Value> = contents
            .into_iter()
            .map(|c| serde_json::to_value(&c).unwrap())
            .collect();

        assert_eq!(json_vec.len(), 2);
        assert!(json_vec[0].is_object());
        assert!(json_vec[1].is_object());
        assert_eq!(json_vec[0]["text"], "First");
        assert_eq!(json_vec[1]["text"], "Second");
    }

    /// **CRITICAL TEST**: Verifies HTTP router covers ALL MCP tools.
    ///
    /// This test ensures that when you add a new `#[tool]` in `mcp/server.rs`,
    /// you MUST also add it to `HTTP_ROUTER_TOOLS` (and `http_tool_router!`).
    ///
    /// If this test fails:
    /// 1. Check which tools are missing from the router
    /// 2. Add them to `HTTP_ROUTER_TOOLS` const
    /// 3. Add them to `http_tool_router!` invocation in `handle_tools_call`
    #[tokio::test]
    async fn test_http_router_covers_all_mcp_tools() {
        use crate::mcp::DetrixServer;
        use std::collections::HashSet;

        let (state, _temp_dir) = create_test_state().await;
        let mcp_server = DetrixServer::new(Arc::clone(&state));

        // Get all tools from MCP server (defined via #[tool] attributes)
        let tools = mcp_server.get_tools();
        let mcp_tools: HashSet<String> = tools.iter().map(|t| t.name.to_string()).collect();

        // Get all tools from HTTP router (defined in HTTP_ROUTER_TOOLS)
        let router_tools: HashSet<String> = super::HTTP_ROUTER_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Find mismatches
        let missing_from_router: Vec<_> = mcp_tools.difference(&router_tools).collect();
        let extra_in_router: Vec<_> = router_tools.difference(&mcp_tools).collect();

        // Provide helpful error message
        if !missing_from_router.is_empty() || !extra_in_router.is_empty() {
            panic!(
                "\n\
                ╔══════════════════════════════════════════════════════════════════╗\n\
                ║  HTTP ROUTER / MCP TOOLS MISMATCH DETECTED!                      ║\n\
                ╠══════════════════════════════════════════════════════════════════╣\n\
                ║  Missing from HTTP router (add to HTTP_ROUTER_TOOLS):            ║\n\
                ║    {:?}\n\
                ║                                                                  ║\n\
                ║  Extra in HTTP router (remove or add #[tool] in server.rs):      ║\n\
                ║    {:?}\n\
                ╠══════════════════════════════════════════════════════════════════╣\n\
                ║  To fix: Update HTTP_ROUTER_TOOLS and http_tool_router! macro    ║\n\
                ║  in crates/detrix-api/src/http/mcp.rs                            ║\n\
                ╚══════════════════════════════════════════════════════════════════╝\n",
                missing_from_router, extra_in_router
            );
        }

        // Sanity check: we should have tools
        assert!(
            !mcp_tools.is_empty(),
            "MCP server should have at least one tool"
        );
        assert!(
            !router_tools.is_empty(),
            "HTTP router should have at least one tool"
        );
    }
}
