//! Helper functions for MCP tool implementations
//!
//! This module provides reusable helpers for MCP tools, organized by domain:
//! - `connection` - Connection management and validation
//! - `metrics` - Metric location and naming utilities

use crate::state::ApiState;
use detrix_application::{FileInspectionRequest, FileInspectionResult, FileInspectionService};
use detrix_core::ConnectionId;
use rmcp::ErrorData as McpError;

// ============================================================================
// Connection Management Helpers
// ============================================================================

pub mod connection {
    use super::*;
    use crate::mcp::error::ToMcpResult;

    /// Check if any debugger adapter is currently connected via the lifecycle manager
    pub async fn is_debugger_connected(state: &ApiState) -> bool {
        state
            .context
            .adapter_lifecycle_manager
            .has_connected_adapters()
            .await
    }

    /// Get connection status message for responses
    pub async fn connection_status_message(state: &ApiState) -> String {
        if is_debugger_connected(state).await {
            "Debugger: connected".to_string()
        } else {
            "⚠️ Debugger: NOT connected (use create_connection to connect to debugpy first)"
                .to_string()
        }
    }

    /// Require debugger connection for write operations
    ///
    /// Returns an error if the debugger is not connected, with a helpful message.
    pub async fn require_debugger_connected(state: &ApiState) -> Result<(), McpError> {
        if !is_debugger_connected(state).await {
            return Err(McpError::internal_error(
                "Debugger not connected. Use 'create_connection' to connect to debugpy first, then retry this operation.".to_string(),
                None,
            ));
        }
        Ok(())
    }

    /// Resolve connection ID - use provided, or auto-select if only one exists.
    ///
    /// Returns the connection ID to use, or an error with helpful guidance.
    pub async fn resolve_connection(
        state: &ApiState,
        connection_id: Option<String>,
    ) -> Result<ConnectionId, McpError> {
        if let Some(id) = connection_id {
            // Validate the provided connection exists
            let conn_id = ConnectionId::new(&id);
            let exists = state
                .context
                .connection_service
                .get_connection(&conn_id)
                .await
                .mcp_context("Failed to check connection")?
                .is_some();

            if !exists {
                return Err(McpError::invalid_params(
                    format!(
                        "Connection '{}' not found.\n\
                         Use list_connections to see available connections.",
                        id
                    ),
                    None,
                ));
            }
            return Ok(conn_id);
        }

        // Auto-select if only one connection exists
        let connections = state
            .context
            .connection_service
            .list_connections()
            .await
            .mcp_context("Failed to list connections")?;

        match connections.len() {
            0 => Err(McpError::invalid_params(
                "No connections available.\n\
                 Use create_connection first:\n\
                 create_connection(host='127.0.0.1', port=5678, language='python')",
                None,
            )),
            1 => {
                let conn = &connections[0];
                Ok(conn.id.clone())
            }
            n => {
                let ids: Vec<_> = connections.iter().map(|c| c.id.to_string()).collect();
                Err(McpError::invalid_params(
                    format!(
                        "Multiple connections exist ({}). Specify connection_id.\n\
                         Available: {}",
                        n,
                        ids.join(", ")
                    ),
                    None,
                ))
            }
        }
    }
}

// ============================================================================
// Metric Location and Naming Helpers
// ============================================================================

pub mod metrics {
    use super::*;
    use crate::common::extract_variable_names;

    /// Find the best line for an expression in a file.
    ///
    /// Uses the file inspection service to find where a variable is defined/used.
    /// Returns (line, line_content, alternatives).
    #[allow(clippy::type_complexity)]
    pub fn find_best_line(
        file: &str,
        expression: &str,
        find_variable: Option<&str>,
    ) -> Result<(u32, String, Vec<(u32, String)>), McpError> {
        // Determine what to search for
        let search_var = find_variable
            .map(|s| s.to_string())
            .or_else(|| extract_variable_names(expression).into_iter().next());

        let search_var = match search_var {
            Some(v) => v,
            None => {
                return Err(McpError::invalid_params(
                    format!(
                        "Could not determine variable to search for in expression '{}'.\n\
                         Hint: Specify line= parameter explicitly, or use find_variable= to search for a specific variable.",
                        expression
                    ),
                    None,
                ));
            }
        };

        // Use file inspection service to find the variable
        let service = FileInspectionService::new();
        let request = FileInspectionRequest {
            file_path: file.to_string(),
            line: None,
            find_variable: Some(search_var.clone()),
        };

        let (_lang, result) = service.inspect(request).map_err(|e| {
            McpError::internal_error(format!("Failed to inspect file: {}", e), None)
        })?;

        // Extract variable definitions from the result
        let definitions = match result {
            FileInspectionResult::VariableSearch(vs) => vs.definitions,
            _ => Vec::new(),
        };

        if definitions.is_empty() {
            return Err(McpError::invalid_params(
                format!(
                    "Could not find variable '{}' in file '{}'.\n\
                     Hint: Specify line= parameter explicitly.",
                    search_var, file
                ),
                None,
            ));
        }

        // First definition is primary, rest are alternatives
        let primary = &definitions[0];
        let primary_line = primary.line;
        let primary_content = primary.code.clone();

        let alternatives: Vec<_> = definitions
            .iter()
            .skip(1)
            .take(5) // Limit to 5 alternatives
            .map(|d| (d.line, d.code.clone()))
            .collect();

        Ok((primary_line, primary_content, alternatives))
    }

    /// Generate a metric name from file and line context.
    ///
    /// Delegates to shared `generate_metric_name` from `common::parsing`.
    /// Format: `filename_lineNumber` (e.g., `auth_py_42`)
    ///
    /// Note: The `_expression` parameter is kept for API compatibility but unused.
    /// The metric name is derived from file location only for consistency.
    pub fn generate_metric_name(_expression: &str, file: &str, line: u32) -> String {
        crate::common::generate_metric_name(file, line)
    }
}
