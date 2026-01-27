//! MCP error handling utilities
//!
//! This module provides extension traits for ergonomic error conversion
//! to MCP errors, reducing boilerplate in tool handlers.

use rmcp::ErrorData as McpError;

/// Extension trait for converting Results to MCP errors with context
///
/// This trait reduces boilerplate when converting application errors to MCP errors:
///
/// # Before
/// ```ignore
/// let metrics = self.metric_service
///     .list_metrics()
///     .await
///     .map_err(|e| McpError::internal_error(format!("Failed to list metrics: {}", e), None))?;
/// ```
///
/// # After
/// ```ignore
/// let metrics = self.metric_service
///     .list_metrics()
///     .await
///     .mcp_context("Failed to list metrics")?;
/// ```
pub trait ToMcpResult<T> {
    /// Convert a Result to an MCP internal error with context message
    fn mcp_context(self, context: &str) -> Result<T, McpError>;

    /// Convert a Result to an MCP invalid_params error with context message
    fn mcp_invalid_params(self, context: &str) -> Result<T, McpError>;
}

impl<T, E: std::fmt::Display> ToMcpResult<T> for Result<T, E> {
    fn mcp_context(self, context: &str) -> Result<T, McpError> {
        self.map_err(|e| McpError::internal_error(format!("{}: {}", context, e), None))
    }

    fn mcp_invalid_params(self, context: &str) -> Result<T, McpError> {
        self.map_err(|e| McpError::invalid_params(format!("{}: {}", context, e), None))
    }
}

/// Extension trait for converting Options to MCP errors
pub trait OptionToMcpResult<T> {
    /// Convert an Option to an MCP error if None
    fn mcp_ok_or(self, context: &str) -> Result<T, McpError>;

    /// Convert an Option to an MCP invalid_params error if None
    fn mcp_ok_or_invalid(self, context: &str) -> Result<T, McpError>;
}

impl<T> OptionToMcpResult<T> for Option<T> {
    fn mcp_ok_or(self, context: &str) -> Result<T, McpError> {
        self.ok_or_else(|| McpError::internal_error(context.to_string(), None))
    }

    fn mcp_ok_or_invalid(self, context: &str) -> Result<T, McpError> {
        self.ok_or_else(|| McpError::invalid_params(context.to_string(), None))
    }
}

/// Extension trait for converting MCP Results to HTTP bridge Results
///
/// The HTTP bridge uses `Result<T, String>` for its return types,
/// while MCP tools return `Result<T, McpError>`. This trait provides
/// ergonomic conversion:
///
/// # Before
/// ```ignore
/// self.wake().await.map_err(|e| e.message.to_string())
/// ```
///
/// # After
/// ```ignore
/// self.wake().await.http_bridge()
/// ```
pub trait HttpBridgeResultExt<T> {
    /// Convert an MCP Result to an HTTP bridge Result
    fn http_bridge(self) -> Result<T, String>;
}

impl<T> HttpBridgeResultExt<T> for Result<T, McpError> {
    fn http_bridge(self) -> Result<T, String> {
        self.map_err(|e| e.message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_context_ok() {
        let result: Result<i32, &str> = Ok(42);
        let mcp_result = result.mcp_context("Test context");
        assert!(mcp_result.is_ok());
        assert_eq!(mcp_result.unwrap(), 42);
    }

    #[test]
    fn test_mcp_context_err() {
        let result: Result<i32, &str> = Err("original error");
        let mcp_result = result.mcp_context("Failed to do thing");
        assert!(mcp_result.is_err());
        // McpError doesn't implement PartialEq, so just verify it's an error
    }

    #[test]
    fn test_mcp_invalid_params_err() {
        let result: Result<i32, &str> = Err("bad input");
        let mcp_result = result.mcp_invalid_params("Invalid parameter");
        assert!(mcp_result.is_err());
    }

    #[test]
    fn test_option_mcp_ok_or_some() {
        let opt: Option<i32> = Some(42);
        let result = opt.mcp_ok_or("Not found");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_option_mcp_ok_or_none() {
        let opt: Option<i32> = None;
        let result = opt.mcp_ok_or("Resource not found");
        assert!(result.is_err());
    }

    #[test]
    fn test_option_mcp_ok_or_invalid_none() {
        let opt: Option<i32> = None;
        let result = opt.mcp_ok_or_invalid("Missing required parameter");
        assert!(result.is_err());
    }
}
