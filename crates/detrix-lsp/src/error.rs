//! Error types for the LSP integration

use thiserror::Error;

/// Result type for LSP operations
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during LSP operations
#[derive(Debug, Error)]
pub enum Error {
    /// LSP server failed to start
    #[error("Failed to start LSP server: {0}")]
    ServerStartFailed(String),

    /// LSP server is not available (not started or crashed)
    #[error("LSP server not available: {0}")]
    ServerNotAvailable(String),

    /// Connection to LSP server failed
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// LSP request timed out
    #[error("Request timed out after {0}ms")]
    Timeout(u64),

    /// LSP protocol error (invalid message format)
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// JSON-RPC error from LSP server
    #[error("LSP error {code}: {message}")]
    Rpc { code: i32, message: String },

    /// Call hierarchy feature not supported
    #[error("Call hierarchy not supported by LSP server")]
    CallHierarchyNotSupported,

    /// File not found for analysis
    #[error("File not found: {0}")]
    FileNotFound(String),

    /// Function not found in file
    #[error("Function '{function}' not found in {file}")]
    FunctionNotFound { function: String, file: String },

    /// Maximum call depth exceeded
    #[error("Maximum call depth ({0}) exceeded")]
    MaxDepthExceeded(usize),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// UTF-8 decoding error
    #[error("Invalid UTF-8 in response: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

// Conversion from detrix-core errors
impl From<detrix_core::Error> for Error {
    fn from(err: detrix_core::Error) -> Self {
        Error::Protocol(err.to_string())
    }
}

// Conversion to detrix-core errors for the PurityAnalyzer trait
impl From<Error> for detrix_core::Error {
    fn from(err: Error) -> Self {
        detrix_core::Error::Safety(err.to_string())
    }
}

/// Extension trait for converting Results to LSP errors with context
///
/// # Example
/// ```ignore
/// use crate::error::ToLspResult;
/// let output = Command::new("pylsp").spawn().lsp_server_start("pylsp")?;
/// ```
#[allow(dead_code)]
pub trait ToLspResult<T> {
    /// Convert to ServerStartFailed error with context
    fn lsp_server_start(self, command: &str) -> Result<T>;

    /// Convert to FileNotFound error with context
    fn lsp_file_not_found(self, path: &std::path::Path) -> Result<T>;

    /// Convert to ConnectionFailed error with context
    fn lsp_connection_failed(self, context: &str) -> Result<T>;

    /// Convert to Protocol error with context
    fn lsp_protocol(self, context: &str) -> Result<T>;
}

impl<T, E: std::fmt::Display> ToLspResult<T> for std::result::Result<T, E> {
    fn lsp_server_start(self, command: &str) -> Result<T> {
        self.map_err(|e| Error::ServerStartFailed(format!("{}: {}", command, e)))
    }

    fn lsp_file_not_found(self, path: &std::path::Path) -> Result<T> {
        self.map_err(|e| Error::FileNotFound(format!("{}: {}", path.display(), e)))
    }

    fn lsp_connection_failed(self, context: &str) -> Result<T> {
        self.map_err(|e| Error::ConnectionFailed(format!("{}: {}", context, e)))
    }

    fn lsp_protocol(self, context: &str) -> Result<T> {
        self.map_err(|e| Error::Protocol(format!("{}: {}", context, e)))
    }
}

/// Extension trait for converting Options to LSP errors
#[allow(dead_code)]
pub trait OptionToLspResult<T> {
    /// Convert None to ConnectionFailed error
    fn lsp_connection_ok_or(self, message: &str) -> Result<T>;

    /// Convert None to Protocol error
    fn lsp_protocol_ok_or(self, message: &str) -> Result<T>;
}

impl<T> OptionToLspResult<T> for Option<T> {
    fn lsp_connection_ok_or(self, message: &str) -> Result<T> {
        self.ok_or_else(|| Error::ConnectionFailed(message.to_string()))
    }

    fn lsp_protocol_ok_or(self, message: &str) -> Result<T> {
        self.ok_or_else(|| Error::Protocol(message.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = Error::ServerStartFailed("command not found".into());
        assert!(err.to_string().contains("Failed to start LSP server"));
    }

    #[test]
    fn test_timeout_error() {
        let err = Error::Timeout(5000);
        assert!(err.to_string().contains("5000ms"));
    }

    #[test]
    fn test_lsp_error() {
        let err = Error::Rpc {
            code: -32600,
            message: "Invalid request".into(),
        };
        assert!(err.to_string().contains("-32600"));
        assert!(err.to_string().contains("Invalid request"));
    }

    #[test]
    fn test_function_not_found() {
        let err = Error::FunctionNotFound {
            function: "get_data".into(),
            file: "api.py".into(),
        };
        assert!(err.to_string().contains("get_data"));
        assert!(err.to_string().contains("api.py"));
    }
}
