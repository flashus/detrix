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

    /// Configuration error (invalid paths, missing files, etc.).
    #[error("configuration error: {0}")]
    ConfigError(String),
}

/// Result type alias for Detrix client operations.
pub type Result<T> = std::result::Result<T, Error>;

impl<T> From<std::sync::PoisonError<T>> for Error {
    fn from(_: std::sync::PoisonError<T>) -> Self {
        Error::ControlPlaneError("lock poisoned".to_string())
    }
}

/// Extension trait for adding context to any `Result<T, E: Display>`.
pub(crate) trait ResultExt<T> {
    fn control_plane(self, msg: &str) -> Result<T>;
    fn lldb(self, msg: &str) -> Result<T>;
    fn port_bind(self, msg: &str) -> Result<T>;
    fn config(self, msg: &str) -> Result<T>;
    fn registration(self, msg: &str) -> Result<T>;
}

impl<T, E: std::fmt::Display> ResultExt<T> for std::result::Result<T, E> {
    fn control_plane(self, msg: &str) -> Result<T> {
        self.map_err(|e| Error::ControlPlaneError(format!("{}: {}", msg, e)))
    }

    fn lldb(self, msg: &str) -> Result<T> {
        self.map_err(|e| Error::LldbStartFailed(format!("{}: {}", msg, e)))
    }

    fn port_bind(self, msg: &str) -> Result<T> {
        self.map_err(|e| Error::PortBindError(format!("{}: {}", msg, e)))
    }

    fn config(self, msg: &str) -> Result<T> {
        self.map_err(|e| Error::ConfigError(format!("{}: {}", msg, e)))
    }

    fn registration(self, msg: &str) -> Result<T> {
        self.map_err(|e| Error::RegistrationFailed(format!("{}: {}", msg, e)))
    }
}

/// Extension trait for reqwest errors needing structured error construction.
pub(crate) trait ReqwestResultExt<T> {
    fn daemon_unreachable(self, url: &str) -> Result<T>;
    fn registration_context(self) -> Result<T>;
}

impl<T> ReqwestResultExt<T> for std::result::Result<T, reqwest::Error> {
    fn daemon_unreachable(self, url: &str) -> Result<T> {
        self.map_err(|e| Error::DaemonUnreachable {
            url: url.to_string(),
            message: e.to_string(),
        })
    }

    fn registration_context(self) -> Result<T> {
        self.map_err(|e| {
            use std::error::Error as StdError;
            let mut details = format!("{}", e);
            let err: &dyn StdError = &e;
            if let Some(source) = err.source() {
                details.push_str(&format!(" (caused by: {})", source));
                if let Some(inner) = source.source() {
                    details.push_str(&format!(" (inner: {})", inner));
                }
            }
            Error::RegistrationFailed(details)
        })
    }
}
