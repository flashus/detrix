//! Base Adapter - Shared logic for all DAP language adapters
//!
//! This module provides a generic `BaseAdapter<P>` that handles common DAP operations:
//! - Connection management (start, stop, ensure_connected)
//! - Setting/removing metrics as logpoints
//! - Event subscription and parsing
//!
//! Language-specific adapters implement the `OutputParser` trait to customize:
//! - Output format parsing (e.g., Python vs Delve prefixes)
//! - Logpoint message format
//! - Post-connection behavior (e.g., Delve needs explicit continue)
//!
//! # Clean Architecture
//!
//! This is INFRASTRUCTURE code - it implements the `DapAdapter` trait defined in
//! the application layer (`detrix-application::ports::adapter`).

mod adapter;
mod events;
mod introspection;
pub mod metrics;
mod parsing;
mod traits;

#[cfg(test)]
mod tests;

// Re-export public API
pub use adapter::BaseAdapter;
pub use metrics::{build_source_breakpoint, MetricManager};
pub use parsing::{find_metric_for_error, parse_value, LogpointParseResult};
pub use traits::{NoThreadExtractor, OutputParser, ThreadExtractor, ThreadInfo, DETRICS_PREFIX};

// Re-export for internal use within the module
pub(crate) use parsing::{create_metric_event_from_logpoint, parse_logpoint_core};
