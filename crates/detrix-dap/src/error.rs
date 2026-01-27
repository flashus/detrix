//! Error types for DAP operations

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug, Clone, PartialEq)]
pub enum Error {
    /// DAP protocol state violations and message format expectations
    ///
    /// Use for: sequence number mismatches, unexpected message types,
    /// invalid state transitions in the DAP protocol.
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// JSON parsing and deserialization failures
    ///
    /// Automatically converted from `serde_json::Error` via `From` impl.
    #[error("Invalid message format: {0}")]
    InvalidMessage(String),

    /// Adapter lookup failures
    #[error("Adapter not found: {0}")]
    AdapterNotFound(String),

    /// Process I/O, channel operations, and adapter lifecycle failures
    ///
    /// Use for: socket errors, channel send failures, process spawn errors,
    /// stream read/write errors.
    #[error("Adapter communication error: {0}")]
    Communication(String),

    /// Adapter initialization failures
    #[error("Initialization failed: {0}")]
    InitializationFailed(String),

    /// Request timeout
    #[error("Request timed out after {0}ms")]
    Timeout(u64),
}

// Implement From for common error types
impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::InvalidMessage(err.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Communication(err.to_string())
    }
}

impl From<std::string::FromUtf8Error> for Error {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Error::Protocol(format!("Invalid UTF-8: {}", err))
    }
}

// Convert DAP errors to core errors (enables ? operator in DapAdapter trait impls)
impl From<Error> for detrix_core::Error {
    fn from(err: Error) -> Self {
        detrix_core::Error::Adapter(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = Error::Protocol("test error".to_string());
        assert_eq!(err.to_string(), "Protocol error: test error");
    }

    #[test]
    fn test_error_from_serde_json() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json");
        assert!(json_err.is_err());

        let err: Error = json_err.unwrap_err().into();
        match err {
            Error::InvalidMessage(_) => (),
            _ => panic!("Expected InvalidMessage error"),
        }
    }
}
