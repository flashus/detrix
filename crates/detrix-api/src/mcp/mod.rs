//! MCP (Model Context Protocol) server implementation
//!
//! This module provides MCP integration for Detrix, enabling LLM agents
//! to interact with metrics dynamically. MCP is a protocol that allows AI
//! models to access tools, resources, and prompts.
//!
//! # Architecture
//!
//! The MCP server follows the same Clean Architecture pattern as gRPC/REST:
//! - Parse MCP requests (JSON-RPC 2.0)
//! - Convert to domain types
//! - Call application services
//! - Convert results to MCP responses
//!
//! # Components
//!
//! - **Tools**: Actions that modify state (add_metric, remove_metric, etc.)
//! - **Resources**: Read-only data access (status, metrics list, etc.)
//! - **Prompts**: Pre-built templates for common debugging scenarios
//!
//! # Module Structure
//!
//! - `params` - Parameter types for all MCP tools
//! - `server` - DetrixServer implementation with tool handlers
//! - `format` - Response formatting (TOON/JSON)
//! - `error` - Error handling utilities
//! - `conversions` - Type conversions
//! - `proto_adapters` - MCP params → Proto types adapters (DRY)

pub mod conversions;
pub mod error;
pub mod format;
pub mod helpers;
pub mod instrumentation;
pub mod params;
pub mod prompts;
pub mod proto_adapters;
pub mod resources;
pub mod server;

pub use error::{OptionToMcpResult, ToMcpResult};

pub use format::{format_response, ResponseFormat};

// Re-export params for convenience
pub use params::{
    AcknowledgeEventsParams, AddMetricParams, CloseConnectionParams, CreateConnectionParams,
    EnableFromDiffParams, GetConfigParams, GetConnectionParams, GetMetricParams, GroupParams,
    InspectFileParams, ListConnectionsParams, ListGroupsParams, ListMetricsParams, ObserveParams,
    QueryMetricsParams, QuerySystemEventsParams, RemoveMetricParams, ToggleMetricParams,
    UpdateConfigParams, UpdateMetricParams, ValidateConfigParams, ValidateExpressionParams,
};

pub use server::DetrixServer;
