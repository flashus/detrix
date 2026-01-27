//! Python Adapter Integration (debugpy)
//!
//! Provides Python-specific functionality for working with debugpy:
//! - Setting metrics as logpoints
//! - Parsing output events (including Python error detection)
//! - Managing Python-specific configuration
//!
//! # Architecture
//!
//! This module uses `BaseAdapter<PythonOutputParser>` for common DAP operations.
//! Python-specific behavior is implemented in `PythonOutputParser`:
//! - Output parsing with DETRICS: prefix
//! - Python error detection (NameError, AttributeError, etc.)
//! - JSON/type-aware value parsing

use crate::{
    base::{
        parse_logpoint_core, parse_value, BaseAdapter, NoThreadExtractor, OutputParser,
        DETRICS_PREFIX,
    },
    error_detection::{parse_error_output_common, PYTHON_ERROR_PATTERNS},
    ext::DebugResult,
    AdapterConfig, OutputEventBody,
};
use async_trait::async_trait;
use detrix_config::AdapterConnectionConfig;
use detrix_core::{
    CapturedStackTrace, CapturedVariable, MemorySnapshot, Metric, MetricEvent, SnapshotScope,
    StackFrame,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ============================================================================
// PythonOutputParser - Python-specific output parsing
// ============================================================================

/// Python-specific output parser for debugpy.
///
/// Handles:
/// - DETRICS logpoint parsing
/// - Python error detection (NameError, TypeError, etc.)
/// - JSON-aware value parsing
pub struct PythonOutputParser;

#[async_trait]
impl OutputParser for PythonOutputParser {
    async fn parse_output(
        body: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent> {
        let text = body.output.trim();

        // Try DETRICS logpoint first
        if text.starts_with(DETRICS_PREFIX) {
            return Self::parse_logpoint_output(text, active_metrics).await;
        }

        // Try Python error detection
        Self::parse_error_output(body, active_metrics).await
    }

    /// Python/debugpy supports capturing introspection data via logpoints.
    ///
    /// The `build_logpoint_message` method builds Python code that captures
    /// stack traces and memory snapshots directly in the logpoint output,
    /// which is more efficient than stopping at a breakpoint.
    fn supports_logpoint_introspection() -> bool {
        true
    }

    /// Build a logpoint message for Python/debugpy with optional introspection.
    ///
    /// When introspection is enabled, the logpoint captures additional data:
    /// - Stack trace: `|ST:[{frame_json}]`
    /// - Memory snapshot: `|MS:{vars_json}`
    ///
    /// Format: `DETRICS:name={expression}|ST:[...]|MS:{...}`
    fn build_logpoint_message(metric: &Metric) -> String {
        let mut message = format!(
            "{}{}={{{}}}",
            DETRICS_PREFIX, metric.name, metric.expression
        );

        // Add stack trace capture if requested
        if metric.capture_stack_trace {
            // Get stack trace slice config (default: last 10 frames)
            let max_frames = match &metric.stack_trace_slice {
                Some(slice) => {
                    if slice.full {
                        100
                    } else if let Some(tail) = slice.tail {
                        tail as usize
                    } else if let Some(head) = slice.head {
                        head as usize
                    } else {
                        10
                    }
                }
                None => 10,
            };

            // Python expression to capture stack trace as JSON
            // Uses traceback.extract_stack() which gives us FrameSummary objects
            let stack_expr = format!(
                r#"__import__('json').dumps([{{'name':f.name,'file':f.filename,'line':f.lineno}} for f in __import__('traceback').extract_stack()[-{}:]])"#,
                max_frames
            );
            message.push_str(&format!("|ST:{{{}}}", stack_expr));
        }

        // Add memory snapshot capture if requested
        if metric.capture_memory_snapshot {
            let scope = metric.snapshot_scope.unwrap_or_default();

            // Python expression to capture variables as JSON
            // Truncate values to 100 chars and exclude private vars
            let vars_expr = match scope {
                SnapshotScope::Local => {
                    r#"__import__('json').dumps({'locals': {k:str(v)[:100] for k,v in locals().items() if not k.startswith('_')}})"#
                }
                SnapshotScope::Global => {
                    r#"__import__('json').dumps({'globals': {k:str(v)[:100] for k,v in globals().items() if not k.startswith('_') and not callable(v)}})"#
                }
                SnapshotScope::Both => {
                    r#"__import__('json').dumps({'locals': {k:str(v)[:100] for k,v in locals().items() if not k.startswith('_')}, 'globals': {k:str(v)[:100] for k,v in globals().items() if not k.startswith('_') and not callable(v)}})"#
                }
            };
            message.push_str(&format!("|MS:{{{}}}", vars_expr));
        }

        message
    }

    fn language_name() -> &'static str {
        "python"
    }
}

impl PythonOutputParser {
    /// Parse DETRICS logpoint output into MetricEvent.
    ///
    /// Formats:
    /// - Basic: `DETRICS:metric_name=value`
    /// - With introspection: `DETRICS:metric_name=value|ST:[...]|MS:{...}`
    async fn parse_logpoint_output(
        text: &str,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent> {
        // Use shared core parsing (Python doesn't extract thread info)
        let parse_result = parse_logpoint_core(text, &NoThreadExtractor)?;

        // Find the metric
        let metrics = active_metrics.read().await;
        let metric = metrics
            .values()
            .find(|m| m.name == parse_result.metric_name)?;
        let metric_id = metric.id?;
        let metric_name = metric.name.clone();
        let connection_id = metric.connection_id.clone();
        let snapshot_scope = metric.snapshot_scope.unwrap_or_default();
        drop(metrics);

        // Parse value with type detection (using shared parser from base)
        let (value_json, value_numeric, value_string, value_boolean) =
            parse_value(&parse_result.value_str);

        // Parse introspection data from remaining sections (Python-specific)
        // The raw text after DETRICS: contains: "name=value|ST:[...]|MS:{...}"
        let content = text.trim_start_matches(DETRICS_PREFIX);
        let sections: Vec<&str> = content.split('|').collect();

        let mut stack_trace = None;
        let mut memory_snapshot = None;

        for section in sections.iter().skip(1) {
            if let Some(st_data) = section.strip_prefix("ST:") {
                stack_trace = Self::parse_stack_trace_json(st_data);
            } else if let Some(ms_data) = section.strip_prefix("MS:") {
                memory_snapshot = Self::parse_memory_snapshot_json(ms_data, &snapshot_scope);
            }
        }

        Some(MetricEvent {
            id: None,
            metric_id,
            metric_name,
            connection_id,
            timestamp: MetricEvent::now_micros(),
            thread_name: None,
            thread_id: None,
            value_json,
            value_numeric,
            value_string,
            value_boolean,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace,
            memory_snapshot,
        })
    }

    /// Parse stack trace JSON from logpoint output.
    ///
    /// Expected format: `[{"name":"func","file":"path","line":10},...]`
    fn parse_stack_trace_json(json_str: &str) -> Option<CapturedStackTrace> {
        #[derive(serde::Deserialize)]
        struct RawFrame {
            name: String,
            file: String,
            line: u32,
        }

        let frames: Vec<RawFrame> = serde_json::from_str(json_str).debug_ok(&format!(
            "Failed to parse stack trace JSON - input: {}",
            json_str
        ))?;
        let total_frames = frames.len() as u32;

        let stack_frames: Vec<StackFrame> = frames
            .into_iter()
            .enumerate()
            .map(|(idx, f)| StackFrame {
                index: idx as u32,
                name: f.name,
                file: Some(f.file),
                line: Some(f.line),
                column: None,
                module: None,
            })
            .collect();

        Some(CapturedStackTrace {
            frames: stack_frames,
            total_frames,
            truncated: false,
        })
    }

    /// Parse memory snapshot JSON from logpoint output.
    ///
    /// Expected format: `{"locals":{"var":"value",...},"globals":{...}}`
    fn parse_memory_snapshot_json(json_str: &str, scope: &SnapshotScope) -> Option<MemorySnapshot> {
        #[derive(serde::Deserialize, Default)]
        struct RawSnapshot {
            #[serde(default)]
            locals: HashMap<String, String>,
            #[serde(default)]
            globals: HashMap<String, String>,
        }

        let raw: RawSnapshot = serde_json::from_str(json_str).debug_ok(&format!(
            "Failed to parse memory snapshot JSON - input: {}",
            json_str
        ))?;

        let locals: Vec<CapturedVariable> = raw
            .locals
            .into_iter()
            .map(|(name, value)| CapturedVariable {
                name,
                value,
                var_type: None,
                scope: Some("local".to_string()),
            })
            .collect();

        let globals: Vec<CapturedVariable> = raw
            .globals
            .into_iter()
            .map(|(name, value)| CapturedVariable {
                name,
                value,
                var_type: None,
                scope: Some("global".to_string()),
            })
            .collect();

        Some(MemorySnapshot {
            locals,
            globals,
            scope: *scope,
        })
    }

    /// Parse error output from debugpy into MetricEvent.
    ///
    /// Uses shared error detection patterns from `error_detection.rs` to identify
    /// Python errors like NameError, AttributeError, TypeError, etc.
    async fn parse_error_output(
        output: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent> {
        parse_error_output_common(output, active_metrics, PYTHON_ERROR_PATTERNS).await
    }
}

// ============================================================================
// PythonAdapter - Type alias for BaseAdapter<PythonOutputParser>
// ============================================================================

/// Python adapter for debugpy.
///
/// This is a type alias for `BaseAdapter<PythonOutputParser>`, providing
/// Python-specific output parsing while reusing common DAP logic.
pub type PythonAdapter = BaseAdapter<PythonOutputParser>;

// ============================================================================
// PythonAdapter Configuration Methods
// ============================================================================

impl PythonAdapter {
    /// Create default debugpy configuration for Attach mode using config.
    pub fn default_config_with_connection(
        connection_config: &AdapterConnectionConfig,
    ) -> AdapterConfig {
        AdapterConfig::new("python", "python").attach(
            &connection_config.default_host,
            connection_config.default_port,
        )
    }

    /// Create default debugpy configuration for Attach mode with explicit port.
    pub fn default_config(port: u16) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        AdapterConfig::new("python", "python").attach(&config.default_host, port)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OutputCategory, Source};
    use detrix_core::entities::{Location, MetricId, MetricMode, SafetyLevel};
    use detrix_core::SourceLanguage;

    fn create_test_metric(name: &str, file: &str, line: u32, expression: &str) -> Metric {
        Metric {
            id: Some(MetricId(1)),
            name: name.to_string(),
            connection_id: detrix_core::ConnectionId::from("default"),
            group: None,
            location: Location {
                file: file.to_string(),
                line,
            },
            expression: expression.to_string(),
            language: SourceLanguage::Python,
            enabled: true,
            mode: MetricMode::Stream,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: Default::default(),
        }
    }

    #[test]
    fn test_python_adapter_creation() {
        let config = PythonAdapter::default_config(5678);
        let adapter = PythonAdapter::new(config, "/app");

        assert_eq!(adapter.base_path(), std::path::Path::new("/app"));
    }

    #[test]
    fn test_default_config() {
        let config = PythonAdapter::default_config(5678);

        assert_eq!(config.adapter_id, "python");
        assert_eq!(config.command, "python");

        // Verify Attach mode configuration
        match &config.connection_mode {
            crate::adapter::ConnectionMode::Attach { host, port } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(*port, 5678);
            }
            _ => panic!("Expected Attach mode"),
        }
    }

    #[tokio::test]
    async fn test_parse_logpoint_output_valid() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = create_test_metric("order_placed", "orders.py", 127, "order.symbol");

        let mut metrics = active_metrics.write().await;
        metrics.insert("orders.py#127".to_string(), metric.clone());
        drop(metrics);

        let output = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "DETRICS:order_placed=\"BTCUSD\"".to_string(),
            source: Some(Source {
                path: Some("/app/orders.py".to_string()),
                name: Some("orders.py".to_string()),
                source_reference: None,
            }),
            line: Some(127),
            column: None,
            variables_reference: None,
        };

        let event = PythonOutputParser::parse_output(&output, &active_metrics)
            .await
            .expect("Should parse event");

        assert_eq!(event.metric_id, MetricId(1));
        assert_eq!(event.value_string, Some("BTCUSD".to_string()));
    }

    #[tokio::test]
    async fn test_parse_logpoint_output_json_value() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = create_test_metric(
            "order_placed",
            "orders.py",
            127,
            "{\"symbol\": order.symbol}",
        );

        let mut metrics = active_metrics.write().await;
        metrics.insert("orders.py#127".to_string(), metric.clone());
        drop(metrics);

        let output = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: r#"DETRICS:order_placed={"symbol":"BTCUSD","qty":10}"#.to_string(),
            source: Some(Source {
                path: Some("/app/orders.py".to_string()),
                name: Some("orders.py".to_string()),
                source_reference: None,
            }),
            line: Some(127),
            column: None,
            variables_reference: None,
        };

        let event = PythonOutputParser::parse_output(&output, &active_metrics)
            .await
            .expect("Should parse event");

        assert_eq!(event.metric_id, MetricId(1));
        // JSON value should be stored in value_json
        assert!(event.value_json.contains("symbol"));
        assert!(event.value_json.contains("BTCUSD"));
    }

    #[tokio::test]
    async fn test_parse_logpoint_output_not_detrix() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));

        let output = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "Regular log message".to_string(),
            source: None,
            line: None,
            column: None,
            variables_reference: None,
        };

        let event = PythonOutputParser::parse_output(&output, &active_metrics).await;
        assert!(event.is_none());
    }

    #[tokio::test]
    async fn test_parse_logpoint_output_invalid_format() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));

        let output = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "DETRICS:invalid_format".to_string(),
            source: None,
            line: None,
            column: None,
            variables_reference: None,
        };

        let event = PythonOutputParser::parse_output(&output, &active_metrics).await;
        assert!(event.is_none());
    }

    #[tokio::test]
    async fn test_parse_error_output_name_error() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = create_test_metric("test_metric", "test.py", 10, "f'x={x}'");

        let mut metrics = active_metrics.write().await;
        metrics.insert("test.py#10".to_string(), metric.clone());
        drop(metrics);

        let output = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "name 'x' is not defined\n".to_string(),
            source: None,
            line: None,
            column: None,
            variables_reference: None,
        };

        let event = PythonOutputParser::parse_output(&output, &active_metrics)
            .await
            .expect("Should parse error event");

        assert_eq!(event.metric_id, MetricId(1));
        assert_eq!(event.metric_name, "test_metric");
        assert!(event.is_error);
        assert_eq!(event.error_type, Some("NameError".to_string()));
        assert!(event
            .error_message
            .as_ref()
            .unwrap()
            .contains("not defined"));
    }

    #[tokio::test]
    async fn test_parse_error_output_attribute_error() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = create_test_metric("test_metric", "test.py", 10, "obj.foo");

        let mut metrics = active_metrics.write().await;
        metrics.insert("test.py#10".to_string(), metric.clone());
        drop(metrics);

        let output = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "AttributeError: 'NoneType' object has no attribute 'foo'\n".to_string(),
            source: None,
            line: None,
            column: None,
            variables_reference: None,
        };

        let event = PythonOutputParser::parse_output(&output, &active_metrics)
            .await
            .expect("Should parse error event");

        assert!(event.is_error);
        assert_eq!(event.error_type, Some("AttributeError".to_string()));
    }

    #[tokio::test]
    async fn test_parse_error_output_type_error() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = create_test_metric("test_metric", "test.py", 10, "a + b");

        let mut metrics = active_metrics.write().await;
        metrics.insert("test.py#10".to_string(), metric.clone());
        drop(metrics);

        let output = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "TypeError: unsupported operand type(s) for +: 'int' and 'str'\n".to_string(),
            source: None,
            line: None,
            column: None,
            variables_reference: None,
        };

        let event = PythonOutputParser::parse_output(&output, &active_metrics)
            .await
            .expect("Should parse error event");

        assert!(event.is_error);
        assert_eq!(event.error_type, Some("TypeError".to_string()));
    }

    #[tokio::test]
    async fn test_parse_error_output_not_error() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));

        let output = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "Regular output message\n".to_string(),
            source: None,
            line: None,
            column: None,
            variables_reference: None,
        };

        let event = PythonOutputParser::parse_output(&output, &active_metrics).await;
        assert!(event.is_none());
    }

    #[test]
    fn test_parse_value_numeric() {
        let (json, numeric, string, boolean) = parse_value("42");
        assert_eq!(json, "42");
        assert_eq!(numeric, Some(42.0));
        assert!(string.is_none());
        assert!(boolean.is_none());
    }

    #[test]
    fn test_parse_value_string() {
        let (json, numeric, string, boolean) = parse_value("\"hello\"");
        assert_eq!(json, "\"hello\"");
        assert!(numeric.is_none());
        assert_eq!(string, Some("hello".to_string()));
        assert!(boolean.is_none());
    }

    #[test]
    fn test_parse_value_boolean() {
        let (json, numeric, string, boolean) = parse_value("true");
        assert_eq!(json, "true");
        assert!(numeric.is_none());
        assert!(string.is_none());
        assert_eq!(boolean, Some(true));
    }

    #[test]
    fn test_parse_value_non_json() {
        let (json, numeric, string, boolean) = parse_value("not json value");
        assert_eq!(json, "not json value");
        // The shared parse_value also tries numeric parsing
        assert!(numeric.is_none());
        assert_eq!(string, Some("not json value".to_string()));
        assert!(boolean.is_none());
    }

    // ========================================================================
    // Introspection Parsing Tests
    // ========================================================================

    #[test]
    fn test_parse_stack_trace_json() {
        let json_str = r#"[{"name":"main","file":"/app/main.py","line":42},{"name":"process","file":"/app/process.py","line":10}]"#;
        let result = PythonOutputParser::parse_stack_trace_json(json_str);

        assert!(result.is_some());
        let trace = result.unwrap();
        assert_eq!(trace.frames.len(), 2);
        assert_eq!(trace.total_frames, 2);
        assert!(!trace.truncated);

        assert_eq!(trace.frames[0].name, "main");
        assert_eq!(trace.frames[0].file, Some("/app/main.py".to_string()));
        assert_eq!(trace.frames[0].line, Some(42));

        assert_eq!(trace.frames[1].name, "process");
        assert_eq!(trace.frames[1].file, Some("/app/process.py".to_string()));
        assert_eq!(trace.frames[1].line, Some(10));
    }

    #[test]
    fn test_parse_stack_trace_json_empty() {
        let json_str = r#"[]"#;
        let result = PythonOutputParser::parse_stack_trace_json(json_str);

        assert!(result.is_some());
        let trace = result.unwrap();
        assert_eq!(trace.frames.len(), 0);
        assert_eq!(trace.total_frames, 0);
    }

    #[test]
    fn test_parse_stack_trace_json_invalid() {
        let json_str = "not valid json";
        let result = PythonOutputParser::parse_stack_trace_json(json_str);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_memory_snapshot_json_locals() {
        let json_str = r#"{"locals":{"x":"42","name":"test"}}"#;
        let result = PythonOutputParser::parse_memory_snapshot_json(
            json_str,
            &detrix_core::SnapshotScope::Local,
        );

        assert!(result.is_some());
        let snapshot = result.unwrap();
        assert_eq!(snapshot.locals.len(), 2);
        assert!(snapshot.globals.is_empty());

        // Find the 'x' variable
        let x_var = snapshot.locals.iter().find(|v| v.name == "x");
        assert!(x_var.is_some());
        assert_eq!(x_var.unwrap().value, "42");
    }

    #[test]
    fn test_parse_memory_snapshot_json_both_scopes() {
        let json_str = r#"{"locals":{"x":"1"},"globals":{"CONFIG":"settings"}}"#;
        let result = PythonOutputParser::parse_memory_snapshot_json(
            json_str,
            &detrix_core::SnapshotScope::Both,
        );

        assert!(result.is_some());
        let snapshot = result.unwrap();
        assert_eq!(snapshot.locals.len(), 1);
        assert_eq!(snapshot.globals.len(), 1);

        assert_eq!(snapshot.locals[0].name, "x");
        assert_eq!(snapshot.globals[0].name, "CONFIG");
    }

    #[test]
    fn test_parse_memory_snapshot_json_invalid() {
        let json_str = "not valid json";
        let result = PythonOutputParser::parse_memory_snapshot_json(
            json_str,
            &detrix_core::SnapshotScope::Local,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_build_logpoint_message_basic() {
        let metric = create_test_metric("my_metric", "app.py", 10, "x + y");
        let message = PythonOutputParser::build_logpoint_message(&metric);
        assert_eq!(message, "DETRICS:my_metric={x + y}");
    }

    #[test]
    fn test_build_logpoint_message_with_stack_trace() {
        let mut metric = create_test_metric("my_metric", "app.py", 10, "x");
        metric.capture_stack_trace = true;
        let message = PythonOutputParser::build_logpoint_message(&metric);

        assert!(message.starts_with("DETRICS:my_metric={x}"));
        assert!(message.contains("|ST:{"));
        assert!(message.contains("traceback"));
        assert!(message.contains("extract_stack"));
    }

    #[test]
    fn test_build_logpoint_message_with_memory_snapshot() {
        let mut metric = create_test_metric("my_metric", "app.py", 10, "x");
        metric.capture_memory_snapshot = true;
        metric.snapshot_scope = Some(detrix_core::SnapshotScope::Local);
        let message = PythonOutputParser::build_logpoint_message(&metric);

        assert!(message.starts_with("DETRICS:my_metric={x}"));
        assert!(message.contains("|MS:{"));
        assert!(message.contains("locals()"));
    }

    #[test]
    fn test_build_logpoint_message_with_both() {
        let mut metric = create_test_metric("my_metric", "app.py", 10, "x");
        metric.capture_stack_trace = true;
        metric.capture_memory_snapshot = true;
        metric.snapshot_scope = Some(detrix_core::SnapshotScope::Both);
        let message = PythonOutputParser::build_logpoint_message(&metric);

        assert!(message.starts_with("DETRICS:my_metric={x}"));
        assert!(message.contains("|ST:{"));
        assert!(message.contains("|MS:{"));
        assert!(message.contains("locals()"));
        assert!(message.contains("globals()"));
    }

    #[tokio::test]
    async fn test_parse_logpoint_with_introspection() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let mut metric = create_test_metric("my_metric", "app.py", 10, "x");
        metric.capture_stack_trace = true;
        metric.capture_memory_snapshot = true;
        metric.snapshot_scope = Some(detrix_core::SnapshotScope::Local);

        let mut metrics = active_metrics.write().await;
        metrics.insert("app.py#10".to_string(), metric.clone());
        drop(metrics);

        // Simulate the output from debugpy with introspection data
        let output = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: r#"DETRICS:my_metric=42|ST:[{"name":"main","file":"app.py","line":10}]|MS:{"locals":{"x":"42"}}"#.to_string(),
            source: None,
            line: None,
            column: None,
            variables_reference: None,
        };

        let event = PythonOutputParser::parse_output(&output, &active_metrics)
            .await
            .expect("Should parse event with introspection");

        assert_eq!(event.value_numeric, Some(42.0));

        // Verify stack trace was parsed
        assert!(event.stack_trace.is_some());
        let trace = event.stack_trace.unwrap();
        assert_eq!(trace.frames.len(), 1);
        assert_eq!(trace.frames[0].name, "main");

        // Verify memory snapshot was parsed
        assert!(event.memory_snapshot.is_some());
        let snapshot = event.memory_snapshot.unwrap();
        assert_eq!(snapshot.locals.len(), 1);
        assert_eq!(snapshot.locals[0].name, "x");
        assert_eq!(snapshot.locals[0].value, "42");
    }

    // Note: Full integration tests with real debugpy will be added in integration tests
    // These unit tests verify the parsing and configuration logic
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use crate::{OutputCategory, Source};
    use detrix_core::entities::{Location, MetricId, MetricMode, SafetyLevel};
    use detrix_core::SourceLanguage;
    use proptest::prelude::*;

    // === Test Fixtures ===

    fn create_test_metric_with_id(name: &str, id: u64) -> Metric {
        Metric {
            id: Some(MetricId(id)),
            name: name.to_string(),
            connection_id: detrix_core::ConnectionId::from("default"),
            group: None,
            location: Location {
                file: "test.py".to_string(),
                line: 10,
            },
            expression: "x".to_string(),
            language: SourceLanguage::Python,
            enabled: true,
            mode: MetricMode::Stream,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: Default::default(),
        }
    }

    // === Generators ===

    /// Generate valid metric names
    fn valid_metric_name() -> impl Strategy<Value = String> {
        "[a-zA-Z_][a-zA-Z0-9_]{0,20}"
            .prop_filter("no consecutive underscores", |s: &String| !s.contains("__"))
    }

    /// Generate arbitrary OutputEventBody for fuzzing
    fn arb_output_body() -> impl Strategy<Value = OutputEventBody> {
        (
            ".*{0,200}", // Any output text up to 200 chars
            prop::option::of(Just(OutputCategory::Console)),
            prop::option::of("[a-zA-Z0-9_/\\.]{1,30}".prop_map(|p| Source {
                path: Some(p),
                name: None,
                source_reference: None,
            })),
            prop::option::of(1u32..10000),
        )
            .prop_map(|(output, category, source, line)| OutputEventBody {
                output,
                category,
                source,
                line,
                column: None,
                variables_reference: None,
            })
    }

    // === Properties ===

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// No panics: parse_output never panics for any input
        #[test]
        fn proptest_parse_output_never_panics(body in arb_output_body()) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let metrics = Arc::new(RwLock::new(HashMap::new()));
                // Should not panic, may return None
                let _ = PythonOutputParser::parse_output(&body, &metrics).await;
            });
        }

        /// Metric attribution: if output contains metric name, event uses that metric
        #[test]
        fn proptest_correct_metric_attribution(name in valid_metric_name()) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let result: Result<(), TestCaseError> = rt.block_on(async {
                let mut map = HashMap::new();
                let metric = create_test_metric_with_id(&name, 42);
                map.insert(
                    format!("{}:{}", metric.location.file, metric.location.line),
                    metric,
                );
                let metrics = Arc::new(RwLock::new(map));

                let body = OutputEventBody {
                    output: format!("DETRICS:{}=123", name),
                    category: None,
                    source: None,
                    line: None,
                    column: None,
                    variables_reference: None,
                };

                if let Some(event) = PythonOutputParser::parse_output(&body, &metrics).await {
                    if event.metric_name != name {
                        return Err(TestCaseError::fail(format!(
                            "Expected metric_name {}, got {}",
                            name, event.metric_name
                        )));
                    }
                    if event.metric_id != MetricId(42) {
                        return Err(TestCaseError::fail(format!(
                            "Expected metric_id 42, got {:?}",
                            event.metric_id
                        )));
                    }
                }
                Ok(())
            });
            result?;
        }

        /// Type detection: JSON integers produce value_numeric
        #[test]
        fn proptest_numeric_value_detection(n in -1000000i64..1000000) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let result: Result<(), TestCaseError> = rt.block_on(async {
                let name = "test_metric";
                let mut map = HashMap::new();
                let metric = create_test_metric_with_id(name, 1);
                map.insert(
                    format!("{}:{}", metric.location.file, metric.location.line),
                    metric,
                );
                let metrics = Arc::new(RwLock::new(map));

                let body = OutputEventBody {
                    output: format!("DETRICS:{}={}", name, n),
                    category: None,
                    source: None,
                    line: None,
                    column: None,
                    variables_reference: None,
                };

                if let Some(event) = PythonOutputParser::parse_output(&body, &metrics).await {
                    if event.value_numeric.is_none() {
                        return Err(TestCaseError::fail("Should detect numeric value"));
                    }
                    if event.value_numeric.unwrap() as i64 != n {
                        return Err(TestCaseError::fail(format!(
                            "Expected {}, got {}",
                            n,
                            event.value_numeric.unwrap()
                        )));
                    }
                }
                Ok(())
            });
            result?;
        }

        /// Type detection: true/false produce value_boolean
        #[test]
        fn proptest_boolean_value_detection(b in prop::bool::ANY) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let result: Result<(), TestCaseError> = rt.block_on(async {
                let name = "test_metric";
                let mut map = HashMap::new();
                let metric = create_test_metric_with_id(name, 1);
                map.insert(
                    format!("{}:{}", metric.location.file, metric.location.line),
                    metric,
                );
                let metrics = Arc::new(RwLock::new(map));

                let body = OutputEventBody {
                    output: format!("DETRICS:{}={}", name, b),
                    category: None,
                    source: None,
                    line: None,
                    column: None,
                    variables_reference: None,
                };

                if let Some(event) = PythonOutputParser::parse_output(&body, &metrics).await {
                    if event.value_boolean != Some(b) {
                        return Err(TestCaseError::fail(format!(
                            "Expected {:?}, got {:?}",
                            Some(b),
                            event.value_boolean
                        )));
                    }
                }
                Ok(())
            });
            result?;
        }

        /// parse_value never panics for any input
        #[test]
        fn proptest_parse_value_never_panics(s in "\\PC{0,500}") {
            let _ = parse_value(&s);
        }

        /// parse_value: valid JSON numbers produce numeric field
        #[test]
        fn proptest_parse_value_valid_numbers(n in -1e10f64..1e10f64) {
            // Use a bounded range to avoid floating point precision issues with very large numbers
            let s = format!("{}", n);
            let (_, numeric, _, _) = parse_value(&s);
            if let Some(parsed) = numeric {
                // Allow some floating point tolerance (relative for larger numbers)
                let tolerance = (n.abs() * 1e-10).max(1e-10);
                prop_assert!((parsed - n).abs() < tolerance, "Parsed {} but expected {}", parsed, n);
            }
        }

        /// parse_stack_trace_json never panics
        #[test]
        fn proptest_parse_stack_trace_never_panics(s in "\\PC{0,500}") {
            let _ = PythonOutputParser::parse_stack_trace_json(&s);
        }

        /// parse_memory_snapshot_json never panics
        #[test]
        fn proptest_parse_memory_snapshot_never_panics(
            s in "\\PC{0,500}",
            scope in prop_oneof![
                Just(detrix_core::SnapshotScope::Local),
                Just(detrix_core::SnapshotScope::Global),
                Just(detrix_core::SnapshotScope::Both),
            ]
        ) {
            let _ = PythonOutputParser::parse_memory_snapshot_json(&s, &scope);
        }

        /// Valid stack trace JSON roundtrips correctly
        #[test]
        fn proptest_stack_trace_json_valid_structure(
            func_name in "[a-zA-Z_][a-zA-Z0-9_]{0,20}",
            file_name in "[a-zA-Z0-9_/\\.]{1,30}",
            line in 1u32..10000
        ) {
            let json = format!(
                r#"[{{"name":"{}","file":"{}","line":{}}}]"#,
                func_name, file_name, line
            );
            let result = PythonOutputParser::parse_stack_trace_json(&json);

            prop_assert!(result.is_some(), "Should parse valid stack trace JSON");
            let trace = result.unwrap();
            prop_assert_eq!(trace.frames.len(), 1);
            prop_assert_eq!(&trace.frames[0].name, &func_name);
            prop_assert_eq!(&trace.frames[0].file, &Some(file_name));
            prop_assert_eq!(trace.frames[0].line, Some(line));
        }

        /// Valid memory snapshot JSON roundtrips correctly
        #[test]
        fn proptest_memory_snapshot_json_valid_structure(
            var_name in "[a-zA-Z_][a-zA-Z0-9_]{0,20}",
            var_value in "[a-zA-Z0-9_]{0,50}"
        ) {
            let json = format!(r#"{{"locals":{{"{}":"{}"}}}}"#, var_name, var_value);
            let result = PythonOutputParser::parse_memory_snapshot_json(
                &json,
                &detrix_core::SnapshotScope::Local,
            );

            prop_assert!(result.is_some(), "Should parse valid memory snapshot JSON");
            let snapshot = result.unwrap();
            prop_assert_eq!(snapshot.locals.len(), 1);
            prop_assert_eq!(&snapshot.locals[0].name, &var_name);
            prop_assert_eq!(&snapshot.locals[0].value, &var_value);
        }
    }
}
