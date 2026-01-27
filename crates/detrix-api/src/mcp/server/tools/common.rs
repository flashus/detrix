//! Common helpers shared across tool implementations

use crate::mcp::error::ToMcpResult;
use crate::state::ApiState;
use detrix_config::DefaultsConfig;
use detrix_core::{Connection, ConnectionId, Metric, MetricMode, SafetyLevel, SourceLanguage};
use rmcp::ErrorData as McpError;
use std::sync::Arc;

// ============================================================================
// Metric Building Helpers
// ============================================================================

/// Builder for creating Metric structs with configurable defaults
///
/// Defaults are applied from `DefaultsConfig` when `with_defaults()` is called,
/// but can be overridden by explicit method calls.
pub struct MetricBuilder {
    pub name: String,
    pub connection_id: ConnectionId,
    pub file: String,
    pub line: u32,
    pub expression: String,
    pub language: SourceLanguage,
    pub group: Option<String>,
    pub capture_stack_trace: bool,
    pub capture_memory_snapshot: bool,
    pub ttl_seconds: Option<i64>,
    // Configurable via DefaultsConfig
    pub mode: MetricMode,
    pub enabled: bool,
    pub safety_level: SafetyLevel,
}

impl MetricBuilder {
    pub fn new(
        name: String,
        connection_id: ConnectionId,
        file: String,
        line: u32,
        expression: String,
        language: SourceLanguage,
    ) -> Self {
        Self {
            name,
            connection_id,
            file,
            line,
            expression,
            language,
            group: None,
            capture_stack_trace: false,
            capture_memory_snapshot: false,
            ttl_seconds: None,
            // Hardcoded defaults (overridden by with_defaults())
            mode: MetricMode::Stream,
            enabled: true,
            safety_level: SafetyLevel::Strict,
        }
    }

    /// Apply defaults from configuration
    ///
    /// This sets mode, enabled, and safety_level from the DefaultsConfig.
    /// Call this early in the builder chain, then override specific values as needed.
    pub fn with_defaults(mut self, defaults: &DefaultsConfig) -> Self {
        self.mode = defaults.mode.clone();
        self.enabled = defaults.enabled;
        self.safety_level = defaults.safety_level;
        self
    }

    pub fn with_group(mut self, group: Option<String>) -> Self {
        self.group = group;
        self
    }

    pub fn with_stack_trace(mut self, capture: bool) -> Self {
        self.capture_stack_trace = capture;
        self
    }

    pub fn with_memory_snapshot(mut self, capture: bool) -> Self {
        self.capture_memory_snapshot = capture;
        self
    }

    pub fn with_ttl(mut self, ttl_seconds: Option<i64>) -> Self {
        self.ttl_seconds = ttl_seconds;
        self
    }

    #[allow(dead_code)]
    pub fn with_mode(mut self, mode: MetricMode) -> Self {
        self.mode = mode;
        self
    }

    #[allow(dead_code)]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    #[allow(dead_code)]
    pub fn with_safety_level(mut self, safety_level: SafetyLevel) -> Self {
        self.safety_level = safety_level;
        self
    }

    pub fn build(self) -> Metric {
        let ttl = self.ttl_seconds.map(|t| t as u64);
        Metric {
            id: None,
            name: self.name,
            connection_id: self.connection_id,
            group: self.group,
            location: detrix_core::Location {
                file: self.file,
                line: self.line,
            },
            expression: self.expression,
            language: self.language,
            mode: self.mode,
            enabled: self.enabled,
            condition: None,
            safety_level: self.safety_level,
            created_at: None,
            capture_stack_trace: self.capture_stack_trace,
            stack_trace_ttl: ttl,
            stack_trace_slice: None,
            capture_memory_snapshot: self.capture_memory_snapshot,
            snapshot_scope: None,
            snapshot_ttl: ttl,
            anchor: None,
            anchor_status: Default::default(),
        }
    }
}

// ============================================================================
// Connection Validation Helpers
// ============================================================================

/// Get and validate a connection, ensuring it has a running adapter
#[allow(dead_code)] // Prepared for future tool logic extraction
pub async fn get_validated_connection(
    state: &Arc<ApiState>,
    connection_id: &ConnectionId,
) -> Result<Connection, McpError> {
    let connection = state
        .context
        .connection_service
        .get_connection(connection_id)
        .await
        .mcp_context("Failed to get connection")?
        .ok_or_else(|| {
            McpError::invalid_params(
                format!(
                    "Connection '{}' not found. Use list_connections to see available connections.",
                    connection_id
                ),
                None,
            )
        })?;

    // Check connection has a running adapter
    if !state
        .context
        .adapter_lifecycle_manager
        .has_adapter(connection_id)
        .await
    {
        return Err(McpError::invalid_params(
            format!(
                "Connection '{}' is not connected (status: {:?}). Reconnect or use a different connection.",
                connection_id, connection.status
            ),
            None,
        ));
    }

    Ok(connection)
}

// ============================================================================
// Response Building Helpers
// ============================================================================

/// Parameters for building a metric response
#[allow(dead_code)] // Prepared for future tool logic extraction
pub struct MetricResponseParams<'a> {
    pub success: bool,
    pub metric_id: i64,
    pub metric_name: &'a str,
    pub file: &'a str,
    pub line: u32,
    pub expression: &'a str,
    pub connection_id: &'a str,
    pub warnings: &'a [String],
    pub additional_context: Option<serde_json::Value>,
}

/// Build a JSON response for metric operations
#[allow(dead_code)] // Prepared for future tool logic extraction
pub fn build_metric_response(params: MetricResponseParams<'_>) -> serde_json::Value {
    let mut response = serde_json::json!({
        "success": params.success,
        "metric_id": params.metric_id,
        "name": params.metric_name,
        "location": format!("@{}#{}", params.file, params.line),
        "parsed": {
            "file": params.file,
            "line": params.line,
            "expression": params.expression,
            "connection_id": params.connection_id,
        },
        "warnings": params.warnings,
    });

    if let Some(ctx) = params.additional_context {
        if let Some(obj) = response.as_object_mut() {
            if let Some(ctx_obj) = ctx.as_object() {
                for (k, v) in ctx_obj {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
    }

    response
}

/// Parameters for building an observe response
pub struct ObserveResponseParams<'a> {
    pub metric_id: u64,
    pub metric_name: &'a str,
    pub file: &'a str,
    pub line: u32,
    pub expression: &'a str,
    pub connection_id: &'a str,
    pub line_content: &'a str,
    pub line_source: &'a str,
    pub alternatives: &'a [(u32, String)],
    pub warnings: &'a [String],
}

/// Build an observe response with line context
pub fn build_observe_response(params: ObserveResponseParams<'_>) -> serde_json::Value {
    serde_json::json!({
        "success": true,
        "metric": {
            "id": params.metric_id,
            "name": params.metric_name,
            "location": format!("@{}#{}", params.file, params.line),
            "expression": params.expression,
            "enabled": true,
        },
        "context": {
            "line_content": params.line_content,
            "line_source": params.line_source,
            "connection_id": params.connection_id,
            "alternatives": params.alternatives.iter().map(|(l, c)| {
                serde_json::json!({
                    "line": l,
                    "content": c.trim(),
                })
            }).collect::<Vec<_>>(),
        },
        "warnings": params.warnings,
    })
}
