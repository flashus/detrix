//! Tool Implementation Helpers
//!
//! This module contains implementation logic extracted from MCP tool methods
//! to reduce the size of server.rs while working within the `#[tool_router]` macro constraints.
//!
//! The `#[tool_router]` macro requires all `#[tool]` methods to be in one impl block,
//! so we extract the heavy logic here and have thin wrapper methods in server.rs.
//!
//! # Module Organization
//!
//! - `common` - Shared builders and response helpers
//! - `metrics` - observe, add_metric, enable_from_diff logic
//! - `metric_ops` - remove, toggle, get, update, list, query metrics
//! - `connections` - create, close, get, list connections
//! - `groups` - enable, disable, list groups
//! - `config` - get, update, validate, reload config
//! - `system` - query events, acknowledge, usage, validate, inspect
//! - `status` - wake, sleep

pub mod common;
pub mod config;
pub mod connections;
pub mod groups;
pub mod metric_ops;
pub mod metrics;
pub mod status;
pub mod system;

// Re-export what server.rs needs
pub use config::{get_config_impl, reload_config_impl, update_config_impl, validate_config_impl};
pub use connections::{
    close_connection_impl, create_connection_impl, get_connection_impl, list_connections_impl,
};
pub use groups::{disable_group_impl, enable_group_impl, list_groups_impl};
pub use metric_ops::{
    get_metric_impl, list_metrics_impl, query_metrics_impl, remove_metric_impl, toggle_metric_impl,
    update_metric_impl,
};
pub use metrics::{add_metric_impl, enable_from_diff_impl, observe_impl, NoDebugStatementsFound};
pub use status::{sleep_impl, wake_impl};
pub use system::{
    acknowledge_events_impl, get_mcp_usage_impl, inspect_file_impl, query_system_events_impl,
    validate_expression_impl,
};
