//! Adapter Process Management
//!
//! Manages debug adapter processes (debugpy, delve, CodeLLDB, etc.)
//! - Process spawning and lifecycle
//! - DAP connection management
//! - Health checking
//! - Graceful shutdown

mod config;
mod connection;
mod initialization;
mod process;

#[cfg(test)]
mod tests;

// Re-export public API
pub use config::{AdapterConfig, AdapterSubprocessConfig, ConnectionMode};
pub use process::{AdapterProcess, AdapterState};
