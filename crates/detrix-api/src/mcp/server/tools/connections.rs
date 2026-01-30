//! Connection management: create, close, get, list
//!
//! These tools manage debugger connections (DAP adapters).

use crate::grpc::conversions::connection_to_info;
use crate::mcp::error::{OptionToMcpResult, ToMcpResult};
use crate::mcp::format::{format_response, ResponseFormat};
use crate::mcp::params::{
    CloseConnectionParams, CreateConnectionParams, GetConnectionParams, ListConnectionsParams,
};
use crate::state::ApiState;
use detrix_core::ConnectionId;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

// ============================================================================
// create_connection Implementation
// ============================================================================

/// Result of create_connection operation
pub struct CreateConnectionResult {
    pub connection_id: String,
    pub host: String,
    pub port: u16,
    pub language: String,
    pub status: String,
}

impl CreateConnectionResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "✅ Connected to {} debugger at {}:{} (connection_id: {})",
            self.language, self.host, self.port, self.connection_id
        )
    }

    /// Build JSON response for MCP
    pub fn build_json(&self) -> serde_json::Value {
        serde_json::json!({
            "connection_id": self.connection_id,
            "host": self.host,
            "port": self.port,
            "language": self.language,
            "status": self.status,
        })
    }
}

/// Core create_connection implementation
pub async fn create_connection_impl(
    state: &Arc<ApiState>,
    params: CreateConnectionParams,
) -> Result<CreateConnectionResult, McpError> {
    let connection_service = &state.context.connection_service;

    match connection_service
        .create_connection(
            params.host.clone(),
            params.port as u16,
            params.language.clone(),
            params.connection_id,
            params.program,   // Optional program path for Rust direct lldb-dap
            params.safe_mode, // SafeMode: only allow logpoints
        )
        .await
    {
        Ok(connection_id) => {
            // Fetch the created connection for full response
            let connection = connection_service
                .get_connection(&connection_id)
                .await
                .mcp_context("Failed to fetch connection details")?
                .mcp_ok_or("Connection not found after creation")?;

            Ok(CreateConnectionResult {
                connection_id: connection.id.0.clone(),
                host: connection.host.clone(),
                port: connection.port,
                language: connection.language.to_string(),
                status: connection.status.to_status_string(),
            })
        }
        Err(e) => {
            let error_msg = format!(
                "Failed to connect to {} debugger at {}:{}: {}\n\n\
                Troubleshooting:\n\
                - Ensure the debugger is running (e.g., `python -m debugpy --listen {}:{} --wait-for-client your_script.py`)\n\
                - Check that the port {} is not blocked by a firewall\n\
                - Verify no other process is using port {}",
                params.language, params.host, params.port, e,
                params.port, params.port, params.port, params.port
            );
            Err(McpError::internal_error(error_msg, None))
        }
    }
}

// ============================================================================
// close_connection Implementation
// ============================================================================

/// Result of close_connection operation
pub struct CloseConnectionResult {
    pub connection_id: String,
}

impl CloseConnectionResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!("✅ Connection '{}' closed successfully", self.connection_id)
    }
}

/// Core close_connection implementation
pub async fn close_connection_impl(
    state: &Arc<ApiState>,
    params: CloseConnectionParams,
) -> Result<CloseConnectionResult, McpError> {
    let connection_service = &state.context.connection_service;
    let connection_id = ConnectionId::new(&params.connection_id);

    match connection_service.disconnect(&connection_id).await {
        Ok(_) => Ok(CloseConnectionResult {
            connection_id: params.connection_id,
        }),
        Err(e) => Err(McpError::internal_error(
            format!("Failed to close connection: {}", e),
            None,
        )),
    }
}

// ============================================================================
// get_connection Implementation
// ============================================================================

/// Result of get_connection operation
pub struct GetConnectionResult {
    pub connection_id: String,
    pub host: String,
    pub port: u16,
    pub language: String,
    pub status: String,
    pub created_at: i64,
    pub last_connected_at: Option<i64>,
    pub last_active: i64,
    pub auto_reconnect: bool,
}

impl GetConnectionResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!("Connection '{}': {}", self.connection_id, self.status)
    }

    /// Build JSON response for MCP
    pub fn build_json(&self) -> serde_json::Value {
        serde_json::json!({
            "connection_id": self.connection_id,
            "host": self.host,
            "port": self.port,
            "language": self.language,
            "status": self.status,
            "created_at": self.created_at,
            "last_connected_at": self.last_connected_at,
            "last_active": self.last_active,
            "auto_reconnect": self.auto_reconnect,
        })
    }
}

/// Core get_connection implementation
pub async fn get_connection_impl(
    state: &Arc<ApiState>,
    params: GetConnectionParams,
) -> Result<GetConnectionResult, McpError> {
    let connection_service = &state.context.connection_service;
    let connection_id = ConnectionId::new(&params.connection_id);

    match connection_service.get_connection(&connection_id).await {
        Ok(Some(connection)) => Ok(GetConnectionResult {
            connection_id: connection.id.0.clone(),
            host: connection.host.clone(),
            port: connection.port,
            language: connection.language.to_string(),
            status: connection.status.to_status_string(),
            created_at: connection.created_at,
            last_connected_at: connection.last_connected_at,
            last_active: connection.last_active,
            auto_reconnect: connection.auto_reconnect,
        }),
        Ok(None) => Err(McpError::invalid_params(
            format!("Connection '{}' not found", params.connection_id),
            None,
        )),
        Err(e) => Err(McpError::internal_error(
            format!("Failed to get connection: {}", e),
            None,
        )),
    }
}

// ============================================================================
// list_connections Implementation
// ============================================================================

/// Result of list_connections operation
pub struct ListConnectionsResult {
    pub count: usize,
    pub active_only: bool,
    pub formatted_output: String,
}

impl ListConnectionsResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        let filter_msg = if self.active_only {
            " (active only)"
        } else {
            ""
        };
        format!("Found {} connections{}", self.count, filter_msg)
    }
}

/// Core list_connections implementation
pub async fn list_connections_impl(
    state: &Arc<ApiState>,
    params: ListConnectionsParams,
) -> Result<ListConnectionsResult, McpError> {
    let connection_service = &state.context.connection_service;
    let active_only = params.active_only.unwrap_or(false);

    let connections = if active_only {
        connection_service
            .list_active_connections()
            .await
            .mcp_context("Failed to list connections")?
    } else {
        connection_service
            .list_connections()
            .await
            .mcp_context("Failed to list connections")?
    };

    // Convert to proto ConnectionInfo for consistent response format
    let proto_connections: Vec<_> = connections.iter().map(connection_to_info).collect();

    // Use TOON format by default for LLM efficiency
    let format = ResponseFormat::from_str_opt(params.format.as_deref());
    let output = format_response(&proto_connections, format);

    Ok(ListConnectionsResult {
        count: connections.len(),
        active_only,
        formatted_output: output,
    })
}
