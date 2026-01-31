//! Error types for the Detrix client.

use thiserror::Error;

/// Error type for Detrix client operations.
#[derive(Debug, Error)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    /// Client has already been initialized.
    #[error("detrix client already initialized")]
    AlreadyInitialized,

    /// Client has not been initialized.
    #[error("detrix client not initialized")]
    NotInitialized,

    /// Wake operation is already in progress.
    #[error("wake operation already in progress")]
    WakeInProgress,

    /// lldb-dap binary not found.
    #[error("lldb-dap not found: {0}")]
    LldbNotFound(String),

    /// Failed to start lldb-dap process.
    #[error("failed to start lldb-dap: {0}")]
    LldbStartFailed(String),

    /// lldb-dap process exited unexpectedly.
    #[error("lldb-dap process exited unexpectedly")]
    LldbProcessDied,

    /// Daemon is not reachable.
    #[error("daemon not reachable at {url}: {message}")]
    DaemonUnreachable { url: String, message: String },

    /// Failed to register with daemon.
    #[error("failed to register with daemon: {0}")]
    RegistrationFailed(String),

    /// Failed to start control plane server.
    #[error("failed to start control plane: {0}")]
    ControlPlaneError(String),

    /// HTTP request failed.
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    /// IO error.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// Timeout waiting for operation.
    #[error("timeout: {0}")]
    Timeout(String),

    /// Port bind error (address already in use).
    #[error("port bind failed: {0}")]
    PortBindError(String),
}

/// Result type alias for Detrix client operations.
pub type Result<T> = std::result::Result<T, Error>;
