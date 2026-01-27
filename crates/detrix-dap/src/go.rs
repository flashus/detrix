//! Go Adapter Integration (Delve)
//!
//! Provides Go-specific functionality for working with Delve:
//! - Setting metrics as logpoints via DAP
//! - Parsing output events (with goroutine ID extraction)
//! - Managing Go-specific configuration
//!
//! # Architecture
//!
//! This module uses `BaseAdapter<GoOutputParser>` for common DAP operations.
//! Go-specific behavior is implemented in `GoOutputParser`:
//! - Output parsing with Delve's `> [Go N]: DETRICS:` format
//! - Goroutine ID extraction for thread tracking
//! - Requires explicit continue after connection (Delve starts paused)
//!
//! Delve DAP mode: `dlv dap --listen=127.0.0.1:38697`

use crate::{
    base::{
        create_metric_event_from_logpoint, parse_logpoint_core, BaseAdapter, OutputParser,
        ThreadExtractor, ThreadInfo,
    },
    error_detection::{parse_error_output_common, GO_ERROR_PATTERNS},
    AdapterConfig, OutputEventBody,
};
use async_trait::async_trait;
use detrix_config::AdapterConnectionConfig;
use detrix_core::{Metric, MetricEvent};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::trace;

// ============================================================================
// GoOutputParser - Go-specific output parsing
// ============================================================================

/// Go-specific output parser for Delve.
///
/// Handles:
/// - Delve's output format: `> [Go N]: DETRICS:metric_name=value`
/// - Goroutine ID extraction for thread tracking
/// - Go/Delve error detection
pub struct GoOutputParser;

/// Go/Delve thread extractor.
///
/// Extracts goroutine ID from Delve's output format: `> [Go N]: message`
#[derive(Debug, Default, Clone, Copy)]
pub struct GoThreadExtractor;

impl ThreadExtractor for GoThreadExtractor {
    fn extract_thread_info(&self, output: &str) -> ThreadInfo {
        // Delve prefixes logpoint messages with "> [Go N]: "
        // Example: "> [Go 1]: DETRICS:order_placed=BTCUSD"
        if let Some(rest) = output.strip_prefix("> [Go ") {
            // Find the closing bracket and extract goroutine ID
            if let Some(bracket_pos) = rest.find("]: ") {
                let goroutine_str = &rest[..bracket_pos];
                if let Ok(goroutine_id) = goroutine_str.parse::<i64>() {
                    let message = &rest[bracket_pos + 3..]; // Skip "]: "
                    return ThreadInfo::with_goroutine(goroutine_id, message);
                }
            }
        }

        // Fallback: no thread info extracted
        ThreadInfo::without_thread(output)
    }
}

impl GoOutputParser {
    /// Parse error output from Delve into MetricEvent.
    ///
    /// Uses shared error detection patterns from `error_detection.rs` to identify
    /// Go/Delve errors like:
    /// - "could not evaluate expression: ..."
    /// - "panic: runtime error: index out of range"
    /// - "nil pointer dereference"
    async fn parse_error_output(
        output: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent> {
        parse_error_output_common(output, active_metrics, GO_ERROR_PATTERNS).await
    }
}

#[async_trait]
impl OutputParser for GoOutputParser {
    async fn parse_output(
        body: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent> {
        let output = body.output.trim();

        // Use shared parsing with Go-specific thread extraction
        let extractor = GoThreadExtractor;
        if let Some(parse_result) = parse_logpoint_core(output, &extractor) {
            trace!(
                "Parsing Delve DETRICS logpoint: {} (goroutine={:?})",
                parse_result.metric_name,
                parse_result.thread_info.thread_id
            );
            return create_metric_event_from_logpoint(&parse_result, active_metrics).await;
        }

        // Try error detection as fallback
        Self::parse_error_output(body, active_metrics).await
    }

    fn needs_continue_after_connect() -> bool {
        // Delve in headless mode starts paused
        true
    }

    fn language_name() -> &'static str {
        "go"
    }
}

// ============================================================================
// GoAdapter - Type alias for BaseAdapter<GoOutputParser>
// ============================================================================

/// Go adapter for Delve.
///
/// This is a type alias for `BaseAdapter<GoOutputParser>`, providing
/// Go-specific output parsing while reusing common DAP logic.
pub type GoAdapter = BaseAdapter<GoOutputParser>;

// ============================================================================
// GoAdapter Configuration Methods
// ============================================================================

impl GoAdapter {
    /// Create default Delve configuration for AttachRemote mode using config.
    ///
    /// This is used when connecting to `dlv exec --headless` or `dlv debug --headless`.
    /// The server already has a target loaded, so we use "attach remote" mode.
    pub fn default_config_with_connection(
        connection_config: &AdapterConnectionConfig,
    ) -> AdapterConfig {
        AdapterConfig::new("go", "dlv").attach_remote(
            &connection_config.default_host,
            connection_config.default_port,
        )
    }

    /// Create default Delve configuration for AttachRemote mode with explicit port.
    ///
    /// This is used when connecting to `dlv exec --headless` or `dlv debug --headless`.
    pub fn default_config(port: u16) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        AdapterConfig::new("go", "dlv").attach_remote(&config.default_host, port)
    }

    /// Create Delve configuration for DAP mode.
    ///
    /// This starts Delve in DAP server mode: `dlv dap --listen=host:port`
    pub fn dap_server_config(host: &str, port: u16) -> AdapterConfig {
        AdapterConfig::new("go", "dlv")
            .arg("dap")
            .arg("--listen")
            .arg(format!("{}:{}", host, port))
    }

    /// Build a logpoint message for Go expressions.
    ///
    /// Delve supports logpoints similar to debugpy, but with Go syntax.
    /// Format: `{expression}` evaluates Go expressions
    pub fn build_logpoint_message(metric: &Metric) -> String {
        // Uses default implementation from OutputParser trait
        GoOutputParser::build_logpoint_message(metric)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::OutputCategory;
    use detrix_core::entities::MetricId;
    use detrix_core::SourceLanguage;
    use detrix_testing::fixtures::sample_metric_for_language;
    use std::path::PathBuf;

    fn sample_metric() -> Metric {
        let mut metric = sample_metric_for_language("test_metric", SourceLanguage::Go);
        metric.id = Some(MetricId(1));
        metric
    }

    #[test]
    fn test_default_config() {
        let config = GoAdapter::default_config(38697);
        assert_eq!(config.adapter_id, "go");
        assert_eq!(config.command, "dlv");
        match config.connection_mode {
            crate::ConnectionMode::AttachRemote { port, .. } => assert_eq!(port, 38697),
            _ => panic!("Expected AttachRemote mode"),
        }
    }

    #[test]
    fn test_dap_server_config() {
        let config = GoAdapter::dap_server_config("127.0.0.1", 38697);
        assert_eq!(config.adapter_id, "go");
        assert_eq!(config.command, "dlv");
        assert!(config.args.contains(&"dap".to_string()));
        assert!(config.args.contains(&"--listen".to_string()));
    }

    #[test]
    fn test_build_logpoint_message() {
        let metric = sample_metric();
        let message = GoAdapter::build_logpoint_message(&metric);
        assert_eq!(message, "DETRICS:test_metric={user.Name}");
    }

    #[test]
    fn test_go_adapter_creation() {
        let config = GoAdapter::default_config(38697);
        let adapter = GoAdapter::new(config, PathBuf::from("/tmp"));

        assert!(!adapter.is_ready());
    }

    #[test]
    fn test_location_key() {
        let key = GoAdapter::location_key("main.go", 42);
        assert_eq!(key, "main.go#42");
    }

    #[test]
    fn test_needs_continue() {
        assert!(GoOutputParser::needs_continue_after_connect());
    }

    #[tokio::test]
    async fn test_parse_delve_output_valid() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("main.go#42".to_string(), metric);

        // Test with Delve's actual output format: "> [Go N]: DETRICS:..."
        let body = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "> [Go 1]: DETRICS:test_metric=42".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = GoOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.metric_name, "test_metric");
        assert_eq!(event.value_json, "42");
        assert_eq!(event.value_numeric, Some(42.0));
        assert_eq!(event.thread_id, Some(1));
        assert_eq!(event.thread_name, Some("goroutine-1".to_string()));
    }

    #[tokio::test]
    async fn test_parse_delve_output_direct_format() {
        // Test backward compatibility with direct DETRICS: format
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("main.go#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "DETRICS:test_metric=42".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = GoOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.metric_name, "test_metric");
        assert_eq!(event.value_json, "42");
        // No goroutine ID in direct format
        assert_eq!(event.thread_id, None);
        assert_eq!(event.thread_name, None);
    }

    #[tokio::test]
    async fn test_parse_delve_output_non_detrix() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));

        let body = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "some other output".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        assert!(GoOutputParser::parse_output(&body, &active_metrics)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn test_parse_delve_output_unknown_metric() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));

        let body = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "DETRICS:unknown_metric=42".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        // Should return None because metric isn't tracked
        assert!(GoOutputParser::parse_output(&body, &active_metrics)
            .await
            .is_none());
    }

    // =========================================================================
    // Error detection tests
    // =========================================================================
    //
    // Note: Comprehensive error detection tests are in error_detection.rs.
    // These tests verify the Go adapter integration with error detection.

    use crate::error_detection::detect_error;

    #[test]
    fn test_detect_error_eval_error() {
        let result = detect_error(
            "could not evaluate expression: undefined: foo",
            GO_ERROR_PATTERNS,
        );
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "EvalError");
    }

    #[test]
    fn test_detect_error_nil_pointer() {
        let result = detect_error("runtime error: nil pointer dereference", GO_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "NilPointerError");
    }

    #[test]
    fn test_detect_error_no_error() {
        let result = detect_error("DETRICS:test_metric=42", GO_ERROR_PATTERNS);
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_error_normal_output() {
        let result = detect_error("some normal console output", GO_ERROR_PATTERNS);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_parse_error_output_nil_pointer() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("main.go#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Stderr),
            output: "runtime error: nil pointer dereference".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = GoOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert!(event.is_error);
        assert_eq!(event.error_type, Some("NilPointerError".to_string()));
    }

    #[tokio::test]
    async fn test_parse_error_output_index_error() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("main.go#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Stderr),
            output: "runtime error: index out of range [10] with length 5".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = GoOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert!(event.is_error);
        assert_eq!(event.error_type, Some("IndexError".to_string()));
    }

    #[tokio::test]
    async fn test_parse_error_output_not_error() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("main.go#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "Hello, world!".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = GoOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_none());
    }
}
