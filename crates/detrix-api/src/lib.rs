#![allow(clippy::result_large_err)]
#![recursion_limit = "256"]

// Generated protobuf code
pub mod generated {
    // Include generated code from build.rs
    #![allow(clippy::all)]
    #![allow(warnings)]

    // Include the generated proto modules
    pub mod detrix {
        pub mod v1 {
            // Common types
            include!("generated/detrix.v1.rs");
        }
    }
}

// Re-export generated types for convenience
pub use generated::detrix::v1::*;

// Shared types module - dto's from .proto
// Use this module for type conversions across all API layers
pub mod types;

// Re-export services from detrix-application (Use Cases)
pub use detrix_application::{MetricService, StreamingService};

// gRPC service implementations (DTO mapping only)
pub mod grpc;

// HTTP server with REST, WebSocket, and MCP HTTP (DTO mapping only)
pub mod http;

// MCP (Model Context Protocol) handlers (DTO mapping only)
pub mod mcp;

// Common utilities shared across API layers
pub mod common;

// API response constants (status strings, etc.)
pub mod constants;

// Error types
pub mod error;

// Shared state
pub mod state;

// Supervisor for event listener lifecycle
pub mod supervisor;

// MCP client tracking for daemon lifecycle
pub mod mcp_client_tracker;

// System metrics (CPU, memory)
pub mod system_metrics;

// Shared test helpers (only compiled for tests)
#[cfg(test)]
pub mod test_support;

pub use error::{Error, Result};
pub use mcp_client_tracker::{McpClientSummary, ParentProcessInfo};
pub use state::{ApiState, ApiStateBuilder, DaemonInfo};

// Re-export tonic for downstream crates
pub use tonic;
