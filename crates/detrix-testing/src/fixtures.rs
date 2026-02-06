//! Test fixtures and sample data factories
//!
//! Provides convenient functions to create sample domain objects for testing.
//!
//! ## Fixture Files
//!
//! This module provides access to real Python/Go/Rust fixture files that exist on disk.
//! Tests should use these files so that file validation passes correctly.
//!
//! ```rust,ignore
//! use detrix_testing::fixtures;
//!
//! // Get path to test.py fixture
//! let path = fixtures::test_py_path();
//!
//! // Create a metric pointing to a real file
//! let metric = fixtures::sample_metric("my_metric");
//! ```

use detrix_core::{
    system_event::{SystemEvent, SystemEventType},
    ConnectionId, ConnectionIdentity, Location, Metric, MetricEvent, MetricId, MetricMode,
    SafetyLevel, SourceLanguage,
};
use std::path::PathBuf;

/// Get the path to the fixtures directory
///
/// Returns the absolute path to `crates/detrix-testing/fixtures/`
pub fn fixtures_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("fixtures")
}

/// Get the path to test.py fixture file
///
/// This file contains sample Python code with variables and classes
/// that can be used in metric expressions for testing.
pub fn test_py_path() -> String {
    fixtures_dir().join("test.py").to_string_lossy().to_string()
}

/// Get the path to app.py fixture file
pub fn app_py_path() -> String {
    fixtures_dir().join("app.py").to_string_lossy().to_string()
}

/// Get the path to auth.py fixture file
pub fn auth_py_path() -> String {
    fixtures_dir().join("auth.py").to_string_lossy().to_string()
}

/// Create a sample metric with the given name
///
/// Uses sensible defaults for all other fields.
/// Points to a real fixture file (test.py) so file validation passes.
pub fn sample_metric(name: &str) -> Metric {
    Metric {
        id: None,
        name: name.to_string(),
        connection_id: ConnectionId::from("test"),
        group: Some("test_group".to_string()),
        location: Location {
            file: test_py_path(),
            line: 30, // Line with x = Value()
        },
        expression: "x.value".to_string(), // Complex expression to skip scope validation
        language: SourceLanguage::Python,
        mode: MetricMode::Stream,
        enabled: true,
        condition: None,
        safety_level: SafetyLevel::Strict,
        created_at: None,
        // Default values for introspection fields
        capture_stack_trace: false,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: false,
        snapshot_scope: None,
        snapshot_ttl: None,
        // Anchor tracking defaults
        anchor: None,
        anchor_status: Default::default(),
    }
}

/// Create a sample metric with a specific ID
pub fn sample_metric_with_id(id: u64, name: &str) -> Metric {
    let mut metric = sample_metric(name);
    metric.id = Some(MetricId(id));
    metric
}

/// Create a sample metric in a specific group
pub fn sample_metric_in_group(name: &str, group: &str) -> Metric {
    let mut metric = sample_metric(name);
    metric.group = Some(group.to_string());
    metric
}

/// Create a sample metric at a specific location
pub fn sample_metric_at(name: &str, file: &str, line: u32) -> Metric {
    let mut metric = sample_metric(name);
    metric.location = Location {
        file: file.to_string(),
        line,
    };
    metric
}

/// Create a sample metric with a specific expression
pub fn sample_metric_with_expression(name: &str, expression: &str) -> Metric {
    let mut metric = sample_metric(name);
    metric.expression = expression.to_string();
    metric
}

/// Create a sample metric with a specific connection ID
pub fn sample_metric_with_connection(name: &str, connection_id: &str) -> Metric {
    let mut metric = sample_metric(name);
    metric.connection_id = ConnectionId::new(connection_id);
    metric
}

/// Create a sample metric for a specific language with appropriate defaults
pub fn sample_metric_for_language(name: &str, language: SourceLanguage) -> Metric {
    let (file, line, expression) = match language {
        SourceLanguage::Python => ("test.py", 42, "x.value"),
        SourceLanguage::Go => ("main.go", 42, "user.Name"),
        SourceLanguage::Rust => ("src/main.rs", 42, "user.name"),
        SourceLanguage::JavaScript => ("index.js", 42, "user.name"),
        SourceLanguage::TypeScript => ("index.ts", 42, "user.name"),
        SourceLanguage::Java => ("Main.java", 42, "user.getName()"),
        SourceLanguage::Cpp => ("main.cpp", 42, "user.name"),
        SourceLanguage::C => ("main.c", 42, "user->name"),
        SourceLanguage::Ruby => ("main.rb", 42, "user.name"),
        SourceLanguage::Php => ("index.php", 42, "$user->name"),
        SourceLanguage::Unknown => ("unknown.txt", 42, "value"),
    };

    let mut metric = sample_metric(name);
    metric.language = language;
    metric.location = Location {
        file: file.to_string(),
        line,
    };
    metric.expression = expression.to_string();
    metric
}

/// Create a sample metric with full customization
///
/// This is the most flexible fixture factory for tests that need
/// specific combinations of settings.
pub fn sample_metric_custom(
    name: &str,
    file: &str,
    line: u32,
    expression: &str,
    language: SourceLanguage,
) -> Metric {
    let mut metric = sample_metric(name);
    metric.location = Location {
        file: file.to_string(),
        line,
    };
    metric.expression = expression.to_string();
    metric.language = language;
    metric
}

/// Create a simple sample metric event with fixed test values.
///
/// This is useful for tests that just need any valid MetricEvent
/// without caring about specific field values. Uses deterministic
/// values for predictable test behavior.
///
/// For more control over field values, use `sample_event()` or
/// other specialized constructors.
pub fn sample_test_event() -> MetricEvent {
    MetricEvent {
        id: Some(1),
        metric_id: MetricId(42),
        metric_name: "test_metric".to_string(),
        connection_id: ConnectionId::new("test-conn"),
        timestamp: 1_733_590_800_123_456, // Fixed timestamp for deterministic tests
        thread_name: Some("MainThread".to_string()),
        thread_id: Some(12345),
        value_json: r#"{"value": 42}"#.to_string(),
        value_numeric: Some(42.0),
        value_string: None,
        value_boolean: None,
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    }
}

/// Create a sample metric event
pub fn sample_event(metric_id: u64, value: f64) -> MetricEvent {
    MetricEvent {
        id: None,
        metric_id: MetricId(metric_id),
        metric_name: format!("metric_{}", metric_id),
        connection_id: ConnectionId::from("test"),
        timestamp: chrono::Utc::now().timestamp_micros(),
        value_json: value.to_string(),
        value_numeric: Some(value),
        value_string: None,
        value_boolean: None,
        thread_id: None,
        thread_name: None,
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    }
}

/// Create a sample metric event with a string value
pub fn sample_string_event(metric_id: u64, value: &str) -> MetricEvent {
    MetricEvent {
        id: None,
        metric_id: MetricId(metric_id),
        metric_name: format!("metric_{}", metric_id),
        connection_id: ConnectionId::from("test"),
        timestamp: chrono::Utc::now().timestamp_micros(),
        value_json: format!("\"{}\"", value),
        value_numeric: None,
        value_string: Some(value.to_string()),
        value_boolean: None,
        thread_id: None,
        thread_name: None,
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    }
}

/// Create a sample metric event with a specific timestamp
pub fn sample_event_at(metric_id: u64, value: f64, timestamp: i64) -> MetricEvent {
    MetricEvent {
        id: None,
        metric_id: MetricId(metric_id),
        metric_name: format!("metric_{}", metric_id),
        connection_id: ConnectionId::from("test"),
        timestamp,
        value_json: value.to_string(),
        value_numeric: Some(value),
        value_string: None,
        value_boolean: None,
        thread_id: None,
        thread_name: None,
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    }
}

/// Create a sample metric event for a specific metric (with custom name)
pub fn sample_event_for_metric(metric_id: u64, name: &str) -> MetricEvent {
    MetricEvent {
        id: None,
        metric_id: MetricId(metric_id),
        metric_name: name.to_string(),
        connection_id: ConnectionId::from("test"),
        timestamp: chrono::Utc::now().timestamp_micros(),
        value_json: "42".to_string(),
        value_numeric: Some(42.0),
        value_string: None,
        value_boolean: None,
        thread_id: None,
        thread_name: None,
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    }
}

/// Create a sample connection identity with sensible test defaults
pub fn sample_connection_identity() -> ConnectionIdentity {
    ConnectionIdentity::new(
        "test-app",
        SourceLanguage::Python,
        "/workspace",
        "test-host",
    )
}

/// Create a sample system event for testing
pub fn sample_system_event(event_type: SystemEventType) -> SystemEvent {
    SystemEvent {
        id: None,
        event_type,
        connection_id: Some(ConnectionId::from("test-conn-001")),
        metric_id: Some(1),
        metric_name: Some("test_metric".to_string()),
        timestamp: chrono::Utc::now().timestamp_micros(),
        details_json: None,
        acknowledged: false,
        // Audit fields (optional)
        actor: None,
        resource_type: None,
        resource_id: None,
        old_value: None,
        new_value: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_metric() {
        let metric = sample_metric("test");
        assert_eq!(metric.name, "test");
        assert!(metric.id.is_none());
        assert!(metric.enabled);
    }

    #[test]
    fn test_sample_metric_with_id() {
        let metric = sample_metric_with_id(42, "test");
        assert_eq!(metric.id, Some(MetricId(42)));
    }

    #[test]
    fn test_sample_event() {
        let event = sample_event(1, 42.0);
        assert_eq!(event.metric_id, MetricId(1));
        assert_eq!(event.value_numeric, Some(42.0));
    }
}
