//! Common helpers shared across tool implementations

use detrix_config::DefaultsConfig;
use detrix_core::{ConnectionId, Metric, MetricMode, SafetyLevel, SourceLanguage};

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
    pub expressions: Vec<String>,
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
        expressions: Vec<String>,
        language: SourceLanguage,
    ) -> Self {
        Self {
            name,
            connection_id,
            file,
            line,
            expressions,
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
            expressions: self.expressions,
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

/// Parameters for building an observe response
pub struct ObserveResponseParams<'a> {
    pub metric_id: u64,
    pub metric_name: &'a str,
    pub file: &'a str,
    pub line: u32,
    pub expressions: &'a [String],
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
            "expressions": params.expressions,
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
