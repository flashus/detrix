//! MCP usage instrumentation for tracking tool calls
//!
//! This module provides instrumentation for MCP tool invocations,
//! tracking success/failure rates, error types, and performance metrics.

use detrix_application::{CallTimer, McpUsageEvent, McpUsageRepository, McpUsageService};
use rmcp::ErrorData as McpError;
use std::sync::Arc;

/// MCP instrumentation wrapper for tracking tool usage
#[derive(Clone)]
pub struct McpInstrumentation {
    mcp_usage: Arc<McpUsageService>,
    repository: Option<Arc<dyn McpUsageRepository + Send + Sync>>,
}

impl McpInstrumentation {
    /// Create a new instrumentation instance
    pub fn new(
        mcp_usage: Arc<McpUsageService>,
        repository: Option<Arc<dyn McpUsageRepository + Send + Sync>>,
    ) -> Self {
        Self {
            mcp_usage,
            repository,
        }
    }

    /// Start tracking a tool call, returns a timer
    pub fn start_tool_call(&self, tool_name: &str) -> CallTimer {
        self.mcp_usage.start_call(tool_name)
    }

    /// Finish tracking a successful tool call and persist if repository available
    pub fn finish_tool_success(&self, timer: CallTimer) {
        let event = self.mcp_usage.record_success(timer);
        self.persist_event(event);
    }

    /// Finish tracking a failed tool call with error classification and persist
    pub fn finish_tool_error(&self, timer: CallTimer, error: &McpError) {
        let event = self.mcp_usage.record_error_message(timer, &error.message);
        self.persist_event(event);
    }

    /// Persist an event to the repository (if available)
    fn persist_event(&self, event: McpUsageEvent) {
        if let Some(repo) = &self.repository {
            let repo = repo.clone();
            tokio::spawn(async move {
                if let Err(e) = repo.save_event(&event).await {
                    tracing::warn!("Failed to persist MCP usage event: {}", e);
                }
            });
        }
    }
}
