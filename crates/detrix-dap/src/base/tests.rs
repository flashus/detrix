//! Tests for BaseAdapter
//!
//! This module contains tests for the BaseAdapter and its components.

use super::adapter::BaseAdapter;
use super::traits::{OutputParser, DETRICS_PREFIX};
use crate::OutputEventBody;
use async_trait::async_trait;
use detrix_core::{Metric, MetricEvent, MetricId, SourceLanguage};
use detrix_testing::fixtures::sample_metric_for_language;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// Test parser implementation
struct TestParser;

#[async_trait]
impl OutputParser for TestParser {
    async fn parse_output(
        body: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent> {
        let text = body.output.trim();
        if !text.starts_with(DETRICS_PREFIX) {
            return None;
        }

        let content = text.trim_start_matches(DETRICS_PREFIX);
        let parts: Vec<&str> = content.splitn(2, '=').collect();
        if parts.len() != 2 {
            return None;
        }

        let metric_name = parts[0];
        let value_str = parts[1];

        let metrics = active_metrics.read().await;
        let metric = metrics.values().find(|m| m.name == metric_name)?;
        let metric_id = metric.id?;

        Some(MetricEvent {
            id: None,
            metric_id,
            metric_name: metric.name.clone(),
            connection_id: metric.connection_id.clone(),
            timestamp: MetricEvent::now_micros(),
            thread_name: None,
            thread_id: None,
            value_json: value_str.to_string(),
            value_numeric: value_str.parse().ok(),
            value_string: Some(value_str.to_string()),
            value_boolean: None,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        })
    }

    fn language_name() -> &'static str {
        "test"
    }
}

fn sample_metric() -> Metric {
    let mut metric = sample_metric_for_language("test_metric", SourceLanguage::Python);
    metric.id = Some(MetricId(1));
    metric.expression = "x + y".to_string();
    metric
}

#[test]
fn test_location_key() {
    let key = BaseAdapter::<TestParser>::location_key("main.py", 42);
    assert_eq!(key, "main.py#42");
}

#[test]
fn test_default_logpoint_message() {
    let metric = sample_metric();
    let message = TestParser::build_logpoint_message(&metric);
    assert_eq!(message, "DETRICS:test_metric={x + y}");
}

#[test]
fn test_parse_breakpoint_response_success() {
    let body = serde_json::json!({
        "breakpoints": [{
            "verified": true,
            "line": 45,
            "message": "Breakpoint set"
        }]
    });

    let (verified, line, message) =
        BaseAdapter::<TestParser>::parse_breakpoint_response(&Some(body), 42);

    assert!(verified);
    assert_eq!(line, 45);
    assert_eq!(message, Some("Breakpoint set".to_string()));
}

#[test]
fn test_parse_breakpoint_response_not_verified() {
    let body = serde_json::json!({
        "breakpoints": [{
            "verified": false,
            "line": 42
        }]
    });

    let (verified, line, message) =
        BaseAdapter::<TestParser>::parse_breakpoint_response(&Some(body), 42);

    assert!(!verified);
    assert_eq!(line, 42);
    assert!(message.is_none());
}

#[test]
fn test_parse_breakpoint_response_not_verified_with_message() {
    // Test case for DAP error like "breakpoint already exists" from Delve
    let body = serde_json::json!({
        "breakpoints": [{
            "verified": false,
            "line": 42,
            "message": "breakpoint already exists"
        }]
    });

    let (verified, line, message) =
        BaseAdapter::<TestParser>::parse_breakpoint_response(&Some(body), 42);

    assert!(!verified);
    assert_eq!(line, 42);
    assert_eq!(message, Some("breakpoint already exists".to_string()));
}

#[test]
fn test_parse_breakpoint_response_empty() {
    let (verified, line, message) = BaseAdapter::<TestParser>::parse_breakpoint_response(&None, 42);

    assert!(!verified);
    assert_eq!(line, 42);
    assert!(message.is_none());
}

#[test]
fn test_needs_continue_default() {
    assert!(!TestParser::needs_continue_after_connect());
}

#[tokio::test]
async fn test_parse_output_valid() {
    let active_metrics = Arc::new(RwLock::new(HashMap::new()));
    let metric = sample_metric();
    active_metrics
        .write()
        .await
        .insert("test.py#42".to_string(), metric);

    let body = OutputEventBody {
        category: Some(crate::OutputCategory::Console),
        output: "DETRICS:test_metric=42".to_string(),
        source: None,
        line: None,
        column: None,
        variables_reference: None,
    };

    let result = TestParser::parse_output(&body, &active_metrics).await;
    assert!(result.is_some());
    let event = result.unwrap();
    assert_eq!(event.metric_name, "test_metric");
    assert_eq!(event.value_json, "42");
    assert_eq!(event.value_numeric, Some(42.0));
}

#[tokio::test]
async fn test_parse_output_not_detrix() {
    let active_metrics = Arc::new(RwLock::new(HashMap::new()));

    let body = OutputEventBody {
        category: Some(crate::OutputCategory::Console),
        output: "Regular log message".to_string(),
        source: None,
        line: None,
        column: None,
        variables_reference: None,
    };

    let result = TestParser::parse_output(&body, &active_metrics).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_parse_output_unknown_metric() {
    let active_metrics = Arc::new(RwLock::new(HashMap::new()));

    let body = OutputEventBody {
        category: Some(crate::OutputCategory::Console),
        output: "DETRICS:unknown_metric=42".to_string(),
        source: None,
        line: None,
        column: None,
        variables_reference: None,
    };

    // Should return None because metric isn't tracked
    let result = TestParser::parse_output(&body, &active_metrics).await;
    assert!(result.is_none());
}

// ============================================================================
// expression_contains_function_call tests
// ============================================================================

use super::events::expression_contains_function_call;

#[test]
fn test_expression_contains_function_call_simple_function() {
    // Standard function calls
    assert!(expression_contains_function_call("len(x)"));
    assert!(expression_contains_function_call("fmt.Sprintf(\"test\")"));
    assert!(expression_contains_function_call("user.GetName()"));
    assert!(expression_contains_function_call("calculate(a, b, c)"));
}

#[test]
fn test_expression_contains_function_call_method_calls() {
    // Method calls on objects
    assert!(expression_contains_function_call("obj.method()"));
    assert!(expression_contains_function_call("user.GetBalance()"));
    assert!(expression_contains_function_call("list.append(item)"));
    assert!(expression_contains_function_call("string.format(x)"));
}

#[test]
fn test_expression_contains_function_call_nested() {
    // Nested function calls
    assert!(expression_contains_function_call(
        "fmt.Sprintf(\"%s\", user.GetName())"
    ));
    assert!(expression_contains_function_call(
        "len(strings.Split(s, \",\"))"
    ));
}

#[test]
fn test_expression_contains_function_call_not_function() {
    // Simple variables - should NOT be detected as function calls
    assert!(!expression_contains_function_call("x"));
    assert!(!expression_contains_function_call("user.name"));
    assert!(!expression_contains_function_call("order.Price"));
    assert!(!expression_contains_function_call("arr[0]"));
    assert!(!expression_contains_function_call("map[\"key\"]"));
}

#[test]
fn test_expression_contains_function_call_arithmetic() {
    // Arithmetic expressions with parentheses - NOT function calls
    assert!(!expression_contains_function_call("(x + y)"));
    assert!(!expression_contains_function_call("(a + b) * c"));
    assert!(!expression_contains_function_call("((x))"));
}

#[test]
fn test_expression_contains_function_call_type_cast() {
    // Type casts look like function calls - detected as function call
    // This is acceptable because Go type casts like int(x) are handled similarly
    assert!(expression_contains_function_call("int(x)"));
    assert!(expression_contains_function_call("float64(value)"));
}

#[test]
fn test_expression_contains_function_call_empty() {
    assert!(!expression_contains_function_call(""));
    assert!(!expression_contains_function_call("   "));
}
