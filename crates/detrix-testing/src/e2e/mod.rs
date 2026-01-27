//! Unified E2E Test Framework for Detrix APIs
//!
//! This module provides a reusable test framework that works across MCP, gRPC, and REST APIs.
//! Test scenarios are written once and run against all API backends.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                    Test Scenarios                       │
//! │  (Reusable test logic: add_metric, query, etc.)        │
//! └───────────────────────────┬─────────────────────────────┘
//!                             │
//!                    ┌────────┴────────┐
//!                    │   ApiClient     │  (trait)
//!                    └────────┬────────┘
//!            ┌────────────────┼────────────────┐
//!            │                │                │
//!     ┌──────▼─────┐   ┌──────▼─────┐   ┌──────▼─────┐
//!     │ McpClient  │   │ GrpcClient │   │ RestClient │
//!     └────────────┘   └────────────┘   └────────────┘
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use detrix_testing::e2e::{McpClient, TestReporter, ApiClient};
//! use detrix_testing::e2e::client::AddMetricRequest;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let client = McpClient::new(8080);
//! let reporter = TestReporter::new("test", "MCP");
//!
//! // Test add metric
//! let request = AddMetricRequest::new("test", "file.py#10", "x", "conn");
//! let response = client.add_metric(request).await?;
//! println!("Created metric: {}", response.data);
//! # Ok(())
//! # }
//! ```

pub mod availability;
pub mod client;
pub mod daemon_discovery;
pub mod dap_scenarios;
pub mod executor;
pub mod jwt;
pub mod reporter;
pub mod scenarios;
pub mod unified;

// Re-exports
pub use availability::{
    // Dependency tracking
    get_missing_dependencies,
    // Availability check functions
    is_debugpy_available,
    is_delve_available,
    is_go_available,
    is_gopls_available,
    is_lldb_available,
    is_pylsp_available,
    is_pyright_available,
    is_python_lsp_available,
    is_rust_analyzer_available,
    is_rustc_available,
    print_missing_deps_warning,
    record_missing,
    require_tool,
    ToolDependency,
};
pub use client::{ApiClient, ApiResponse, ConnectionInfo, EventInfo, MetricInfo};
pub use dap_scenarios::{
    DapLanguageExt, DapWorkflowConfig, DapWorkflowScenarios, MetricPoint, ReconnectionResult,
    RestartDebuggerFn, WorkflowResult,
};
pub use executor::{
    cleanup_orphaned_e2e_processes, find_detrix_binary, get_target_candidates, get_workspace_root,
    kill_and_unregister_process, kill_and_unregister_process_async, register_e2e_process,
    restart_delve, restart_lldb, unregister_e2e_process, DebuggerLanguage, TestExecutor,
};
pub use reporter::{TestReporter, TestStep};
pub use scenarios::TestScenarios;

// Backend implementations
pub mod cli;
pub mod grpc;
pub mod mcp;
pub mod rest;
pub mod websocket;

pub use cli::CliClient;
pub use grpc::{GrpcClient, GrpcStreamHandle};
pub use mcp::McpClient;
pub use rest::RestClient;
pub use websocket::{EventMessage, WebSocketClient, WebSocketError, WebSocketHandle};

// JWT testing utilities
pub use jwt::{JwtBuilder, JwtKeyPair, MockJwksServer, TestClaims};
