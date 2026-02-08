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
            values: vec![detrix_core::ExpressionValue {
                expression: String::new(),
                value_json: value_str.to_string(),
                typed_value: value_str
                    .parse::<f64>()
                    .ok()
                    .map(detrix_core::TypedValue::Numeric)
                    .or_else(|| Some(detrix_core::TypedValue::Text(value_str.to_string()))),
            }],
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
    metric.expressions = vec!["x + y".to_string()];
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
    assert_eq!(event.value_json(), "42");
    assert_eq!(event.value_numeric(), Some(42.0));
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

use detrix_core::expression_contains_function_call;

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

// ============================================================================
// subscribe_events idempotency tests
// ============================================================================

/// Test that finished event handler tasks are cleaned up
#[tokio::test]
async fn test_event_tasks_finished_are_cleaned_up() {
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // Create a Vec to simulate event_tasks
    let event_tasks: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(RwLock::new(Vec::new()));

    // Spawn a task that finishes immediately
    let handle1 = tokio::spawn(async {});
    event_tasks.write().await.push(handle1);

    // Spawn another task that finishes immediately
    let handle2 = tokio::spawn(async {});
    event_tasks.write().await.push(handle2);

    // Wait for tasks to finish
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Simulate the cleanup logic from subscribe_events
    {
        let mut tasks = event_tasks.write().await;
        let (finished, active): (Vec<_>, Vec<_>) = tasks.drain(..).partition(|h| h.is_finished());

        assert_eq!(finished.len(), 2, "Both tasks should be finished");
        assert_eq!(active.len(), 0, "No tasks should be active");
    }
}

/// Test that active event handler tasks are detected and can be aborted
#[tokio::test]
async fn test_event_tasks_active_are_detected() {
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let event_tasks: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(RwLock::new(Vec::new()));

    // Spawn a task that runs for a long time
    let handle = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });
    event_tasks.write().await.push(handle);

    // The task should not be finished yet
    {
        let tasks = event_tasks.read().await;
        let active_count = tasks.iter().filter(|h| !h.is_finished()).count();
        assert_eq!(active_count, 1, "Task should still be active");
    }

    // Abort and cleanup
    {
        let mut tasks = event_tasks.write().await;
        for handle in tasks.drain(..) {
            if !handle.is_finished() {
                handle.abort();
            }
        }
    }

    // Give time for abort
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let tasks = event_tasks.read().await;
    assert!(tasks.is_empty(), "Tasks should be cleared after abort");
}

// ============================================================================
// find_metrics_at_location tests
// ============================================================================

use super::introspection::find_metrics_at_location;
use detrix_core::Location;

/// Helper: create a metric at a specific file and line
fn metric_at(name: &str, file: &str, line: u32) -> Metric {
    let mut metric = sample_metric_for_language(name, SourceLanguage::Go);
    metric.id = Some(MetricId(line as u64));
    metric.location = Location {
        file: file.to_string(),
        line,
    };
    metric
}

/// Helper: insert metrics into active_metrics map keyed by location
async fn make_active_metrics(metrics: Vec<Metric>) -> Arc<RwLock<HashMap<String, Metric>>> {
    let map = Arc::new(RwLock::new(HashMap::new()));
    let mut guard = map.write().await;
    for m in metrics {
        guard.insert(format!("{}#{}", m.location.file, m.location.line), m);
    }
    drop(guard);
    map
}

#[tokio::test]
async fn test_find_metrics_exact_match_only() {
    // Metrics at lines 122 and 126 (4 lines apart, within old tolerance of 5)
    // Stopped at line 122 → should ONLY return metric at 122
    let metrics = make_active_metrics(vec![
        metric_at("symbol_length", "main.go", 122),
        metric_at("entry_price", "main.go", 126),
    ])
    .await;

    let result = find_metrics_at_location(&metrics, "main.go", 122).await;
    assert_eq!(
        result.len(),
        1,
        "Should match only the exact metric at line 122"
    );
    assert_eq!(result[0].name, "symbol_length");
}

#[tokio::test]
async fn test_find_metrics_exact_match_other_line() {
    // Stopped at line 126 → should ONLY return metric at 126
    let metrics = make_active_metrics(vec![
        metric_at("symbol_length", "main.go", 122),
        metric_at("entry_price", "main.go", 126),
    ])
    .await;

    let result = find_metrics_at_location(&metrics, "main.go", 126).await;
    assert_eq!(
        result.len(),
        1,
        "Should match only the exact metric at line 126"
    );
    assert_eq!(result[0].name, "entry_price");
}

#[tokio::test]
async fn test_find_metrics_tolerance_when_no_exact_match() {
    // Metric at line 122, stopped at line 124 (compiler moved breakpoint)
    // No exact match → should fall back to closest within tolerance
    let metrics = make_active_metrics(vec![metric_at("symbol_length", "main.go", 122)]).await;

    let result = find_metrics_at_location(&metrics, "main.go", 124).await;
    assert_eq!(
        result.len(),
        1,
        "Should find closest metric within tolerance"
    );
    assert_eq!(result[0].name, "symbol_length");
}

#[tokio::test]
async fn test_find_metrics_closest_when_no_exact_match() {
    // Two metrics at 122 and 130, stopped at 124 (no exact match)
    // Should prefer the closest: 122 (distance 2) over 130 (distance 6, outside tolerance)
    let metrics = make_active_metrics(vec![
        metric_at("symbol_length", "main.go", 122),
        metric_at("other_metric", "main.go", 130),
    ])
    .await;

    let result = find_metrics_at_location(&metrics, "main.go", 124).await;
    assert_eq!(result.len(), 1, "Should find only the closest metric");
    assert_eq!(result[0].name, "symbol_length");
}

#[tokio::test]
async fn test_find_metrics_multiple_exact_matches_same_line() {
    // Two metrics on the same line → should return both
    let metrics = make_active_metrics(vec![
        metric_at("metric_a", "main.go", 100),
        metric_at("metric_b", "main.go", 100),
    ])
    .await;

    // Need to insert with different keys since both are on same line
    {
        let mut guard = metrics.write().await;
        guard.clear();
        let mut m1 = metric_at("metric_a", "main.go", 100);
        m1.id = Some(MetricId(1));
        guard.insert("metric_a".to_string(), m1);
        let mut m2 = metric_at("metric_b", "main.go", 100);
        m2.id = Some(MetricId(2));
        guard.insert("metric_b".to_string(), m2);
    }

    let result = find_metrics_at_location(&metrics, "main.go", 100).await;
    assert_eq!(
        result.len(),
        2,
        "Should return both metrics on the same line"
    );
}

#[tokio::test]
async fn test_find_metrics_outside_tolerance() {
    // Metric at line 100, stopped at line 110 (distance 10, outside tolerance of 5)
    let metrics = make_active_metrics(vec![metric_at("far_metric", "main.go", 100)]).await;

    let result = find_metrics_at_location(&metrics, "main.go", 110).await;
    assert!(
        result.is_empty(),
        "Should not match metrics outside tolerance"
    );
}

#[tokio::test]
async fn test_find_metrics_different_file() {
    let metrics = make_active_metrics(vec![metric_at("metric_a", "main.go", 100)]).await;

    let result = find_metrics_at_location(&metrics, "other.go", 100).await;
    assert!(
        result.is_empty(),
        "Should not match metrics in different files"
    );
}

/// Test the partition logic used in subscribe_events
#[tokio::test]
async fn test_event_tasks_partition_mixed() {
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let event_tasks: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(RwLock::new(Vec::new()));

    // Spawn a task that finishes immediately
    let finished_handle = tokio::spawn(async {});

    // Spawn a task that runs for a long time
    let active_handle = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });

    event_tasks.write().await.push(finished_handle);
    event_tasks.write().await.push(active_handle);

    // Wait for the first task to finish
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Partition and verify
    {
        let mut tasks = event_tasks.write().await;
        let (finished, active): (Vec<_>, Vec<_>) = tasks.drain(..).partition(|h| h.is_finished());

        assert_eq!(finished.len(), 1, "One task should be finished");
        assert_eq!(active.len(), 1, "One task should still be active");

        // Abort the active one to clean up
        for handle in active {
            handle.abort();
        }
    }
}

// ============================================================================
// build_source_breakpoint tests
// ============================================================================

use super::metrics::build_source_breakpoint;

/// Helper: create a metric with specific configuration for breakpoint mode testing
fn metric_with_config(
    name: &str,
    line: u32,
    expression: &str,
    language: SourceLanguage,
    capture_stack_trace: bool,
    capture_memory_snapshot: bool,
) -> Metric {
    let mut metric = sample_metric_for_language(name, language);
    metric.id = Some(MetricId(line as u64));
    metric.location = Location {
        file: "main.go".to_string(),
        line,
    };
    metric.expressions = vec![expression.to_string()];
    metric.capture_stack_trace = capture_stack_trace;
    metric.capture_memory_snapshot = capture_memory_snapshot;
    metric
}

#[test]
fn test_build_source_breakpoint_simple_variable_is_logpoint() {
    // Simple variable, no introspection → logpoint (non-blocking)
    let metric = metric_with_config(
        "order_symbol",
        118,
        "symbol",
        SourceLanguage::Go,
        false,
        false,
    );
    let bp = build_source_breakpoint(&metric, false, "DETRICS:order_symbol={symbol}");

    assert!(
        bp.log_message.is_some(),
        "Simple variable should produce a logpoint"
    );
    assert_eq!(bp.line, 118);
    assert_eq!(bp.log_message.unwrap(), "DETRICS:order_symbol={symbol}");
}

#[test]
fn test_build_source_breakpoint_introspection_is_breakpoint() {
    // Introspection enabled, adapter doesn't support logpoint introspection → breakpoint
    let metric = metric_with_config(
        "entry_price_with_stack",
        126,
        "entryPrice",
        SourceLanguage::Go,
        true,  // capture_stack_trace
        false, // capture_memory_snapshot
    );
    let bp = build_source_breakpoint(
        &metric,
        false,
        "DETRICS:entry_price_with_stack={entryPrice}",
    );

    assert!(
        bp.log_message.is_none(),
        "Introspection metric should produce a breakpoint (not logpoint)"
    );
    assert_eq!(bp.line, 126);
}

#[test]
fn test_build_source_breakpoint_memory_snapshot_is_breakpoint() {
    // Memory snapshot enabled → breakpoint
    let metric = metric_with_config(
        "memory_metric",
        130,
        "pnl",
        SourceLanguage::Go,
        false, // no stack trace
        true,  // capture_memory_snapshot
    );
    let bp = build_source_breakpoint(&metric, false, "DETRICS:memory_metric={pnl}");

    assert!(
        bp.log_message.is_none(),
        "Memory snapshot metric should produce a breakpoint"
    );
}

#[test]
fn test_build_source_breakpoint_go_function_call_is_breakpoint() {
    // Go function call → breakpoint (needs DAP evaluate with "call" prefix)
    let metric = metric_with_config(
        "symbol_length",
        122,
        "len(symbol)",
        SourceLanguage::Go,
        false,
        false,
    );
    let bp = build_source_breakpoint(&metric, false, "DETRICS:symbol_length={len(symbol)}");

    assert!(
        bp.log_message.is_none(),
        "Go function call should produce a breakpoint"
    );
    assert_eq!(bp.line, 122);
}

#[test]
fn test_build_source_breakpoint_python_function_call_is_logpoint() {
    // Python function call → logpoint (Python/debugpy supports function calls in logpoints)
    let metric = metric_with_config(
        "length_metric",
        42,
        "len(items)",
        SourceLanguage::Python,
        false,
        false,
    );
    let bp = build_source_breakpoint(&metric, false, "DETRICS:length_metric={len(items)}");

    assert!(
        bp.log_message.is_some(),
        "Python function call without introspection should produce a logpoint"
    );
}

#[test]
fn test_build_source_breakpoint_introspection_with_logpoint_support() {
    // Introspection enabled BUT adapter supports logpoint introspection → logpoint
    let metric = metric_with_config(
        "intro_metric",
        42,
        "value",
        SourceLanguage::Python,
        true,
        true,
    );
    let bp = build_source_breakpoint(
        &metric,
        true, // supports_logpoint_introspection
        "DETRICS:intro_metric={value}|ST:...|MS:...",
    );

    assert!(
        bp.log_message.is_some(),
        "Introspection with logpoint support should produce a logpoint"
    );
}

#[test]
fn test_build_source_breakpoint_mixed_mode_consistency() {
    // Simulate test_wake scenario: 4 metrics on same file, 2 logpoints + 2 breakpoints
    // Verify each metric independently produces the correct breakpoint type
    let logpoint_1 = metric_with_config(
        "order_symbol",
        118,
        "symbol",
        SourceLanguage::Go,
        false,
        false,
    );
    let breakpoint_1 = metric_with_config(
        "symbol_length",
        122,
        "len(symbol)",
        SourceLanguage::Go,
        false,
        false,
    );
    let breakpoint_2 = metric_with_config(
        "entry_price_with_stack",
        126,
        "entryPrice",
        SourceLanguage::Go,
        true,
        false,
    );
    let logpoint_2 = metric_with_config("pnl_value", 130, "pnl", SourceLanguage::Go, false, false);

    let bp1 = build_source_breakpoint(&logpoint_1, false, "DETRICS:order_symbol={symbol}");
    let bp2 = build_source_breakpoint(&breakpoint_1, false, "DETRICS:symbol_length={len(symbol)}");
    let bp3 = build_source_breakpoint(
        &breakpoint_2,
        false,
        "DETRICS:entry_price_with_stack={entryPrice}",
    );
    let bp4 = build_source_breakpoint(&logpoint_2, false, "DETRICS:pnl_value={pnl}");

    assert!(bp1.log_message.is_some(), "order_symbol should be logpoint");
    assert!(
        bp2.log_message.is_none(),
        "symbol_length should be breakpoint (Go function call)"
    );
    assert!(
        bp3.log_message.is_none(),
        "entry_price_with_stack should be breakpoint (introspection)"
    );
    assert!(bp4.log_message.is_some(), "pnl_value should be logpoint");
}
