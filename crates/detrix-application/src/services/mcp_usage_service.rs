//! MCP usage tracking service for prompt optimization
//!
//! Tracks tool invocations, error patterns, and workflow adherence
//! to enable data-driven improvements to MCP tool descriptions.

use detrix_config::constants::DEFAULT_MCP_USAGE_HISTORY;
pub use detrix_core::{McpErrorCode, McpUsageEvent};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Instant;

/// Session context for workflow analysis
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// List of tools called in this session (most recent last)
    pub tools_called: Vec<String>,
    /// Whether a connection has been created in this session
    pub has_connection: bool,
    /// Whether inspect_file was called before add_metric
    pub inspect_called: bool,
    /// Maximum history size
    max_history: usize,
}

impl Default for SessionContext {
    fn default() -> Self {
        Self {
            tools_called: Vec::new(),
            has_connection: false,
            inspect_called: false,
            max_history: DEFAULT_MCP_USAGE_HISTORY,
        }
    }
}

impl SessionContext {
    /// Create a new session context with custom history size
    pub fn with_history_size(max_history: usize) -> Self {
        Self {
            tools_called: Vec::new(),
            has_connection: false,
            inspect_called: false,
            max_history,
        }
    }
}

impl SessionContext {
    /// Record a tool call in the session
    pub fn record_tool(&mut self, tool_name: &str) {
        // Track specific tools for workflow analysis
        match tool_name {
            "create_connection" => self.has_connection = true,
            "inspect_file" => self.inspect_called = true,
            // Note: add_metric does NOT reset inspect_called here
            // It's reset in check_workflow_for_add_metric after checking
            _ => {}
        }

        // Maintain bounded history
        if self.tools_called.len() >= self.max_history {
            self.tools_called.remove(0);
        }
        self.tools_called.push(tool_name.to_string());
    }

    /// Check if workflow is correct for add_metric
    /// Returns Some(error) if workflow violation detected
    pub fn check_add_metric_workflow(&self) -> Option<McpErrorCode> {
        if !self.has_connection {
            Some(McpErrorCode::MetricBeforeConnection)
        } else {
            None // Note: We don't enforce inspect_file, just track it
        }
    }

    /// Get the prior N tools for context
    pub fn prior_tools(&self, n: usize) -> Vec<String> {
        let len = self.tools_called.len();
        if len <= n {
            self.tools_called.clone()
        } else {
            self.tools_called[len - n..].to_vec()
        }
    }

    /// Reset session state (for new MCP connection)
    pub fn reset(&mut self) {
        self.tools_called.clear();
        self.has_connection = false;
        self.inspect_called = false;
    }
}

/// In-memory atomic counters for zero-overhead tracking
#[derive(Debug, Default)]
pub struct McpUsageCounters {
    // Overall counters
    pub total_calls: AtomicU64,
    pub total_success: AtomicU64,
    pub total_errors: AtomicU64,

    // Per-error-code counters
    pub error_location_missing_at: AtomicU64,
    pub error_location_missing_line: AtomicU64,
    pub error_location_invalid_line: AtomicU64,
    pub error_connection_not_found: AtomicU64,
    pub error_connection_disconnected: AtomicU64,
    pub error_query_missing_param: AtomicU64,
    pub error_unsafe_expression: AtomicU64,
    pub error_other: AtomicU64,

    // Workflow tracking
    pub workflow_correct: AtomicU64,
    pub workflow_skip_inspect: AtomicU64,
    pub workflow_no_connection: AtomicU64,

    // Latency tracking (sum of ms for average calculation)
    pub total_latency_ms: AtomicU64,
}

impl McpUsageCounters {
    /// Increment the appropriate error counter
    pub fn increment_error(&self, code: McpErrorCode) {
        self.total_errors.fetch_add(1, Ordering::Relaxed);
        match code {
            McpErrorCode::LocationMissingAt => {
                self.error_location_missing_at
                    .fetch_add(1, Ordering::Relaxed);
            }
            McpErrorCode::LocationMissingLine => {
                self.error_location_missing_line
                    .fetch_add(1, Ordering::Relaxed);
            }
            McpErrorCode::LocationInvalidLine => {
                self.error_location_invalid_line
                    .fetch_add(1, Ordering::Relaxed);
            }
            McpErrorCode::MetricBeforeConnection => {
                self.workflow_no_connection.fetch_add(1, Ordering::Relaxed);
            }
            McpErrorCode::ConnectionNotFound => {
                self.error_connection_not_found
                    .fetch_add(1, Ordering::Relaxed);
            }
            McpErrorCode::ConnectionDisconnected => {
                self.error_connection_disconnected
                    .fetch_add(1, Ordering::Relaxed);
            }
            McpErrorCode::QueryMissingRequiredParam => {
                self.error_query_missing_param
                    .fetch_add(1, Ordering::Relaxed);
            }
            McpErrorCode::UnsafeExpression => {
                self.error_unsafe_expression.fetch_add(1, Ordering::Relaxed);
            }
            McpErrorCode::Other => {
                self.error_other.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Get all error counts as a sorted vector (highest count first)
    pub fn error_breakdown(&self) -> Vec<(McpErrorCode, u64)> {
        let mut errors = vec![
            (
                McpErrorCode::LocationMissingAt,
                self.error_location_missing_at.load(Ordering::Relaxed),
            ),
            (
                McpErrorCode::LocationMissingLine,
                self.error_location_missing_line.load(Ordering::Relaxed),
            ),
            (
                McpErrorCode::LocationInvalidLine,
                self.error_location_invalid_line.load(Ordering::Relaxed),
            ),
            (
                McpErrorCode::ConnectionNotFound,
                self.error_connection_not_found.load(Ordering::Relaxed),
            ),
            (
                McpErrorCode::ConnectionDisconnected,
                self.error_connection_disconnected.load(Ordering::Relaxed),
            ),
            (
                McpErrorCode::QueryMissingRequiredParam,
                self.error_query_missing_param.load(Ordering::Relaxed),
            ),
            (
                McpErrorCode::UnsafeExpression,
                self.error_unsafe_expression.load(Ordering::Relaxed),
            ),
            (
                McpErrorCode::Other,
                self.error_other.load(Ordering::Relaxed),
            ),
        ];
        // Filter zero counts and sort by count descending
        errors.retain(|(_, count)| *count > 0);
        errors.sort_by(|a, b| b.1.cmp(&a.1));
        errors
    }
}

/// Usage snapshot for API responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSnapshot {
    /// Total tool calls
    pub total_calls: u64,
    /// Total successful calls
    pub total_success: u64,
    /// Total error calls
    pub total_errors: u64,
    /// Success rate (0.0-1.0)
    pub success_rate: f64,
    /// Average latency in milliseconds
    pub avg_latency_ms: f64,
    /// Top errors by count
    pub top_errors: Vec<ErrorCount>,
    /// Workflow statistics
    pub workflow: WorkflowStats,
}

/// Error count for snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorCount {
    pub code: String,
    pub count: u64,
}

/// Workflow statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStats {
    pub correct_workflows: u64,
    pub skipped_inspect: u64,
    pub no_connection: u64,
    /// Adherence rate (correct / total add_metric calls)
    pub adherence_rate: f64,
}

/// MCP usage tracking service
///
/// Provides in-memory counters for fast tracking and
/// optional persistence for historical analysis.
pub struct McpUsageService {
    counters: McpUsageCounters,
    session: RwLock<SessionContext>,
}

impl Default for McpUsageService {
    fn default() -> Self {
        Self::new()
    }
}

impl McpUsageService {
    /// Create a new usage service
    pub fn new() -> Self {
        Self {
            counters: McpUsageCounters::default(),
            session: RwLock::new(SessionContext::default()),
        }
    }

    /// Record the start of a tool call, returns a timer for latency tracking
    pub fn start_call(&self, tool_name: &str) -> CallTimer {
        self.counters.total_calls.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut session) = self.session.write() {
            session.record_tool(tool_name);
        }
        CallTimer::new(tool_name.to_string())
    }

    /// Record a successful tool call
    pub fn record_success(&self, timer: CallTimer) -> McpUsageEvent {
        let latency_ms = timer.elapsed_ms();
        self.counters.total_success.fetch_add(1, Ordering::Relaxed);
        self.counters
            .total_latency_ms
            .fetch_add(latency_ms, Ordering::Relaxed);

        let prior_tools = self
            .session
            .read()
            .map(|s| s.prior_tools(3))
            .unwrap_or_default();
        McpUsageEvent::success(timer.tool_name, latency_ms, prior_tools)
    }

    /// Record a failed tool call
    pub fn record_error(&self, timer: CallTimer, error_code: McpErrorCode) -> McpUsageEvent {
        let latency_ms = timer.elapsed_ms();
        self.counters.increment_error(error_code);
        self.counters
            .total_latency_ms
            .fetch_add(latency_ms, Ordering::Relaxed);

        let prior_tools = self
            .session
            .read()
            .map(|s| s.prior_tools(3))
            .unwrap_or_default();
        McpUsageEvent::error(timer.tool_name, error_code, latency_ms, prior_tools)
    }

    /// Record error from an error message string
    pub fn record_error_message(&self, timer: CallTimer, error_message: &str) -> McpUsageEvent {
        let error_code = McpErrorCode::classify_error_message(error_message);
        self.record_error(timer, error_code)
    }

    /// Check workflow before add_metric and record workflow stats
    pub fn check_workflow_for_add_metric(&self) {
        // First check the workflow state
        let (no_connection, skipped_inspect) = {
            if let Ok(session) = self.session.read() {
                (
                    !session.has_connection,
                    session.has_connection && !session.inspect_called,
                )
            } else {
                (false, false)
            }
        };

        // Update counters based on workflow state
        if no_connection {
            self.counters
                .workflow_no_connection
                .fetch_add(1, Ordering::Relaxed);
        } else if skipped_inspect {
            self.counters
                .workflow_skip_inspect
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.counters
                .workflow_correct
                .fetch_add(1, Ordering::Relaxed);
        }

        // Reset inspect_called for the next metric
        if let Ok(mut session) = self.session.write() {
            session.inspect_called = false;
        }
    }

    /// Get a snapshot of current usage statistics
    pub fn snapshot(&self) -> UsageSnapshot {
        let total_calls = self.counters.total_calls.load(Ordering::Relaxed);
        let total_success = self.counters.total_success.load(Ordering::Relaxed);
        let total_errors = self.counters.total_errors.load(Ordering::Relaxed);
        let total_latency = self.counters.total_latency_ms.load(Ordering::Relaxed);

        let success_rate = if total_calls > 0 {
            total_success as f64 / total_calls as f64
        } else {
            1.0
        };

        let avg_latency_ms = if total_calls > 0 {
            total_latency as f64 / total_calls as f64
        } else {
            0.0
        };

        let workflow_correct = self.counters.workflow_correct.load(Ordering::Relaxed);
        let workflow_skip = self.counters.workflow_skip_inspect.load(Ordering::Relaxed);
        let workflow_no_conn = self.counters.workflow_no_connection.load(Ordering::Relaxed);
        let total_add_metric = workflow_correct + workflow_skip + workflow_no_conn;

        let adherence_rate = if total_add_metric > 0 {
            workflow_correct as f64 / total_add_metric as f64
        } else {
            1.0
        };

        let top_errors = self
            .counters
            .error_breakdown()
            .into_iter()
            .take(5)
            .map(|(code, count)| ErrorCount {
                code: code.as_str().to_string(),
                count,
            })
            .collect();

        UsageSnapshot {
            total_calls,
            total_success,
            total_errors,
            success_rate,
            avg_latency_ms,
            top_errors,
            workflow: WorkflowStats {
                correct_workflows: workflow_correct,
                skipped_inspect: workflow_skip,
                no_connection: workflow_no_conn,
                adherence_rate,
            },
        }
    }

    /// Reset session context (for new MCP connection)
    pub fn reset_session(&self) {
        if let Ok(mut session) = self.session.write() {
            session.reset();
        }
    }

    /// Get session context for debugging
    pub fn session_info(&self) -> SessionContext {
        self.session.read().map(|s| s.clone()).unwrap_or_default()
    }
}

/// Timer for tracking call latency
pub struct CallTimer {
    pub tool_name: String,
    start: Instant,
}

impl CallTimer {
    fn new(tool_name: String) -> Self {
        Self {
            tool_name,
            start: Instant::now(),
        }
    }

    /// Get elapsed time in milliseconds
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code_classification() {
        assert_eq!(
            McpErrorCode::classify_error_message("Location must start with '@'"),
            McpErrorCode::LocationMissingAt
        );
        assert_eq!(
            McpErrorCode::classify_error_message("Expected @file#line format"),
            McpErrorCode::LocationMissingLine
        );
        assert_eq!(
            McpErrorCode::classify_error_message("Invalid line number: abc"),
            McpErrorCode::LocationInvalidLine
        );
        assert_eq!(
            McpErrorCode::classify_error_message("Connection 'abc' not found"),
            McpErrorCode::ConnectionNotFound
        );
        assert_eq!(
            McpErrorCode::classify_error_message("Either 'name' or 'group' must be specified"),
            McpErrorCode::QueryMissingRequiredParam
        );
        assert_eq!(
            McpErrorCode::classify_error_message("Expression contains unsafe function: eval"),
            McpErrorCode::UnsafeExpression
        );
        assert_eq!(
            McpErrorCode::classify_error_message("Unknown error"),
            McpErrorCode::Other
        );
    }

    #[test]
    fn test_session_context() {
        let mut session = SessionContext::default();

        // Initially no connection
        assert!(!session.has_connection);
        assert_eq!(
            session.check_add_metric_workflow(),
            Some(McpErrorCode::MetricBeforeConnection)
        );

        // After create_connection
        session.record_tool("create_connection");
        assert!(session.has_connection);
        assert_eq!(session.check_add_metric_workflow(), None);

        // After inspect_file
        session.record_tool("inspect_file");
        assert!(session.inspect_called);

        // record_tool for add_metric does NOT reset inspect_called
        // (reset happens in check_workflow_for_add_metric service method)
        session.record_tool("add_metric");
        assert!(session.inspect_called); // Still true after record_tool

        // Session reset
        session.reset();
        assert!(!session.has_connection);
        assert!(session.tools_called.is_empty());
    }

    #[test]
    fn test_counter_increments() {
        let service = McpUsageService::new();

        // Simulate a successful call
        let timer = service.start_call("list_metrics");
        std::thread::sleep(std::time::Duration::from_millis(1));
        let _event = service.record_success(timer);

        let snapshot = service.snapshot();
        assert_eq!(snapshot.total_calls, 1);
        assert_eq!(snapshot.total_success, 1);
        assert_eq!(snapshot.total_errors, 0);
        assert!(snapshot.avg_latency_ms >= 1.0);
    }

    #[test]
    fn test_error_tracking() {
        let service = McpUsageService::new();

        // Simulate error calls
        let timer = service.start_call("add_metric");
        let _event = service.record_error(timer, McpErrorCode::LocationMissingAt);

        let timer = service.start_call("add_metric");
        let _event = service.record_error(timer, McpErrorCode::LocationMissingAt);

        let timer = service.start_call("query_metrics");
        let _event = service.record_error(timer, McpErrorCode::QueryMissingRequiredParam);

        let snapshot = service.snapshot();
        assert_eq!(snapshot.total_calls, 3);
        assert_eq!(snapshot.total_errors, 3);
        assert_eq!(snapshot.top_errors.len(), 2);
        assert_eq!(snapshot.top_errors[0].code, "location_missing_at");
        assert_eq!(snapshot.top_errors[0].count, 2);
    }

    #[test]
    fn test_workflow_tracking() {
        let service = McpUsageService::new();

        // Bad workflow: add_metric without connection
        let _timer = service.start_call("add_metric");
        service.check_workflow_for_add_metric();

        // Good workflow: connection → inspect → add
        let _timer = service.start_call("create_connection");
        let _timer = service.start_call("inspect_file");
        let _timer = service.start_call("add_metric");
        service.check_workflow_for_add_metric();

        // Partial workflow: connection → add (no inspect)
        let _timer = service.start_call("create_connection");
        let _timer = service.start_call("add_metric");
        service.check_workflow_for_add_metric();

        let snapshot = service.snapshot();
        assert_eq!(snapshot.workflow.no_connection, 1);
        assert_eq!(snapshot.workflow.correct_workflows, 1);
        assert_eq!(snapshot.workflow.skipped_inspect, 1);
    }
}
