//! Traits for language-specific customization
//!
//! This module defines the traits that language adapters implement to customize
//! output parsing, logpoint formatting, and thread extraction.

use crate::OutputEventBody;
use async_trait::async_trait;
use detrix_core::{Metric, MetricEvent};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ============================================================================
// Constants
// ============================================================================

/// Prefix used in logpoint output to identify Detrix metrics.
/// Format: `DETRICS:metric_name={expression}` for logpoints
/// Format: `DETRICS:metric_name=value` in output
pub const DETRICS_PREFIX: &str = "DETRICS:";

// Re-export delimiter constants from detrix-core (single source of truth)
pub use detrix_core::{MULTI_EXPR_DELIMITER, MULTI_EXPR_DELIMITER_STR};

// ============================================================================
// OutputParser Trait - Language-specific customization point
// ============================================================================

/// Trait for language-specific output parsing and logpoint formatting.
///
/// Each language adapter implements this trait to handle:
/// - Parsing DAP output events into MetricEvents
/// - Building logpoint messages in the debugger's expected format
/// - Any post-connection setup (e.g., Delve needs explicit continue)
#[async_trait]
pub trait OutputParser: Send + Sync + 'static {
    /// Parse a DAP output event body into a MetricEvent.
    ///
    /// Returns `None` if the output is not a DETRICS logpoint or cannot be parsed.
    ///
    /// # Arguments
    /// * `body` - The output event body from DAP
    /// * `active_metrics` - Map of active metrics by location key
    async fn parse_output(
        body: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent>;

    /// Build a logpoint message for the debugger.
    ///
    /// The message format varies by debugger:
    /// - debugpy: `DETRICS:name={expression}`
    /// - Delve: `DETRICS:name={expression}`
    /// - CodeLLDB: `DETRICS:name={expression}` (with LLDB expression syntax)
    fn build_logpoint_message(metric: &Metric) -> String {
        // Default implementation - works for debugpy and Delve
        if metric.expressions.len() == 1 {
            format!(
                "{}{}={{{}}}",
                DETRICS_PREFIX, metric.name, metric.expressions[0]
            )
        } else {
            let blocks: Vec<String> = metric
                .expressions
                .iter()
                .map(|e| format!("{{{}}}", e))
                .collect();
            format!(
                "{}{}={}",
                DETRICS_PREFIX,
                metric.name,
                blocks.join(MULTI_EXPR_DELIMITER_STR)
            )
        }
    }

    /// Whether this debugger requires explicit continue after connection.
    ///
    /// Some debuggers (like Delve in headless mode) start paused and need
    /// an explicit continue command to resume execution.
    fn needs_continue_after_connect() -> bool {
        false
    }

    /// Whether this adapter supports capturing introspection data via logpoints.
    ///
    /// When `true`, the adapter's `build_logpoint_message` includes introspection
    /// data (stack traces, memory snapshots) in the logpoint message itself,
    /// which is more efficient than stopping the program at a breakpoint.
    ///
    /// When `false` (default), introspection-enabled metrics use breakpoints
    /// and capture data via DAP stack trace and variables requests.
    fn supports_logpoint_introspection() -> bool {
        false
    }

    /// Get the language identifier for logging.
    fn language_name() -> &'static str;
}

// ============================================================================
// Thread Extraction - Language-specific thread/goroutine ID extraction
// ============================================================================

/// Information extracted about the thread/goroutine context.
///
/// Different debuggers report thread information differently:
/// - Go/Delve: `> [Go N]: ` prefix for goroutine ID
/// - Rust/LLDB: `thread #N:` or `[thread N]` prefix
/// - Python/debugpy: No thread prefix in logpoint output
#[derive(Debug, Clone, Default)]
pub struct ThreadInfo {
    /// Thread or goroutine ID (if extracted)
    pub thread_id: Option<i64>,
    /// Thread name (e.g., "goroutine-1", "thread-5")
    pub thread_name: Option<String>,
    /// The cleaned text with thread prefix removed
    pub cleaned_text: String,
}

impl ThreadInfo {
    /// Create ThreadInfo with no thread information
    pub fn without_thread(text: &str) -> Self {
        Self {
            thread_id: None,
            thread_name: None,
            cleaned_text: text.to_string(),
        }
    }

    /// Create ThreadInfo with goroutine information (Go/Delve)
    pub fn with_goroutine(id: i64, cleaned_text: &str) -> Self {
        Self {
            thread_id: Some(id),
            thread_name: Some(format!("goroutine-{}", id)),
            cleaned_text: cleaned_text.to_string(),
        }
    }

    /// Create ThreadInfo with thread information (Rust/LLDB)
    pub fn with_thread(id: i64, cleaned_text: &str) -> Self {
        Self {
            thread_id: Some(id),
            thread_name: Some(format!("thread-{}", id)),
            cleaned_text: cleaned_text.to_string(),
        }
    }
}

/// Trait for language-specific thread/goroutine ID extraction.
///
/// Each language debugger formats output differently, so thread extraction
/// is language-specific. Implementing this trait allows shared parsing logic
/// to work with different debugger output formats.
pub trait ThreadExtractor: Send + Sync {
    /// Extract thread information from debugger output.
    ///
    /// Returns `ThreadInfo` containing:
    /// - Thread/goroutine ID if found
    /// - Thread name if found
    /// - Cleaned text with thread prefix removed
    fn extract_thread_info(&self, output: &str) -> ThreadInfo;
}

/// Default thread extractor that doesn't extract thread info.
///
/// Used for debuggers like Python/debugpy that don't include thread
/// information in logpoint output.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoThreadExtractor;

impl ThreadExtractor for NoThreadExtractor {
    fn extract_thread_info(&self, output: &str) -> ThreadInfo {
        ThreadInfo::without_thread(output)
    }
}
