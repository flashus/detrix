//! Agent-specific error types.

use thiserror::Error;

/// Errors that can occur in the agent.
#[derive(Error, Debug)]
pub enum AgentError {
    #[error("gRPC connection failed: {0}")]
    Connection(String),

    #[error("Agent ID error: {0}")]
    AgentId(String),

    #[error("Registration rejected: {reason}")]
    RegistrationRejected { reason: String },

    #[error("Adapter error for connection {connection_id}: {source}")]
    Adapter {
        connection_id: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Scanner error: {0}")]
    Scanner(String),

    #[error("Metrics server error: {0}")]
    MetricsServer(String),

    #[error("Protocol conversion error: {0}")]
    ProtoConversion(String),

    #[error("Configuration error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, AgentError>;
