//! Metric event entity - runtime events captured from target applications

use super::memory::{CapturedStackTrace, MemorySnapshot};
use super::metric::MetricId;
use crate::connection::ConnectionId;
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Typed value extracted from a debugger expression evaluation.
///
/// Enforces mutual exclusivity at the type level — exactly one variant is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TypedValue {
    Numeric(f64),
    Text(String),
    Boolean(bool),
}

/// Serde proxy for backward-compatible JSON serialization of `ExpressionValue`.
///
/// Maps the `TypedValue` enum to/from flat optional fields so that existing
/// stored JSON (SQLite, REST API, WebSocket) continues to work unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionValueSerde {
    expression: String,
    value_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value_numeric: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value_string: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value_boolean: Option<bool>,
}

impl From<ExpressionValue> for ExpressionValueSerde {
    fn from(ev: ExpressionValue) -> Self {
        let (value_numeric, value_string, value_boolean) = match &ev.typed_value {
            Some(TypedValue::Numeric(n)) => (Some(*n), None, None),
            Some(TypedValue::Text(s)) => (None, Some(s.clone()), None),
            Some(TypedValue::Boolean(b)) => (None, None, Some(*b)),
            None => (None, None, None),
        };
        ExpressionValueSerde {
            expression: ev.expression,
            value_json: ev.value_json,
            value_numeric,
            value_string,
            value_boolean,
        }
    }
}

impl From<ExpressionValueSerde> for ExpressionValue {
    fn from(s: ExpressionValueSerde) -> Self {
        // Priority: numeric > string > boolean (deterministic if multiple fields set).
        // Multiple fields being set indicates malformed data — our serializer only ever
        // sets one. Debug-assert to catch this during development.
        debug_assert!(
            [
                s.value_numeric.is_some(),
                s.value_string.is_some(),
                s.value_boolean.is_some()
            ]
            .iter()
            .filter(|&&v| v)
            .count()
                <= 1,
            "ExpressionValueSerde has multiple typed value fields set"
        );
        let typed_value = if let Some(n) = s.value_numeric {
            Some(TypedValue::Numeric(n))
        } else if let Some(t) = s.value_string {
            Some(TypedValue::Text(t))
        } else {
            s.value_boolean.map(TypedValue::Boolean)
        };
        ExpressionValue {
            expression: s.expression,
            value_json: s.value_json,
            typed_value,
        }
    }
}

/// A single expression's evaluated value within a metric event.
///
/// Each metric can have multiple expressions, and each expression produces
/// one `ExpressionValue` when the logpoint fires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(into = "ExpressionValueSerde", from = "ExpressionValueSerde")]
pub struct ExpressionValue {
    pub expression: String,
    pub value_json: String,
    pub typed_value: Option<TypedValue>,
}

impl ExpressionValue {
    /// Create a new ExpressionValue with just expression and JSON value
    pub fn new(expression: impl Into<String>, value_json: impl Into<String>) -> Self {
        Self {
            expression: expression.into(),
            value_json: value_json.into(),
            typed_value: None,
        }
    }

    /// Create an ExpressionValue with a numeric typed value
    pub fn with_numeric(
        expression: impl Into<String>,
        value_json: impl Into<String>,
        n: f64,
    ) -> Self {
        Self {
            expression: expression.into(),
            value_json: value_json.into(),
            typed_value: Some(TypedValue::Numeric(n)),
        }
    }

    /// Create an ExpressionValue with a text typed value
    pub fn with_text(
        expression: impl Into<String>,
        value_json: impl Into<String>,
        s: impl Into<String>,
    ) -> Self {
        Self {
            expression: expression.into(),
            value_json: value_json.into(),
            typed_value: Some(TypedValue::Text(s.into())),
        }
    }

    /// Create an ExpressionValue with a boolean typed value
    pub fn with_boolean(
        expression: impl Into<String>,
        value_json: impl Into<String>,
        b: bool,
    ) -> Self {
        Self {
            expression: expression.into(),
            value_json: value_json.into(),
            typed_value: Some(TypedValue::Boolean(b)),
        }
    }

    /// Get the numeric value if this is a numeric typed value
    pub fn numeric(&self) -> Option<f64> {
        match &self.typed_value {
            Some(TypedValue::Numeric(n)) => Some(*n),
            _ => None,
        }
    }

    /// Get the text value if this is a text typed value
    pub fn text(&self) -> Option<&str> {
        match &self.typed_value {
            Some(TypedValue::Text(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Get the boolean value if this is a boolean typed value
    pub fn boolean(&self) -> Option<bool> {
        match &self.typed_value {
            Some(TypedValue::Boolean(b)) => Some(*b),
            _ => None,
        }
    }
}

/// Runtime metric event captured from target application
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub metric_id: MetricId,
    pub metric_name: String, // Name of the metric that generated this event
    pub connection_id: ConnectionId, // Which connection produced this event
    pub timestamp: i64,      // Microseconds since epoch
    pub thread_name: Option<String>,
    pub thread_id: Option<i64>,
    pub values: Vec<ExpressionValue>,
    pub is_error: bool,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub request_id: Option<String>,
    /// Session identifier for grouping related events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    // Introspection data
    /// Captured stack trace (if metric.capture_stack_trace was true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<CapturedStackTrace>,
    /// Captured memory snapshot (if metric.capture_memory_snapshot was true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_snapshot: Option<MemorySnapshot>,
}

impl MetricEvent {
    /// Get current timestamp in microseconds
    pub fn now_micros() -> i64 {
        Utc::now().timestamp_micros()
    }

    /// Create a new event with a single value
    pub fn new(
        metric_id: MetricId,
        metric_name: String,
        connection_id: ConnectionId,
        value_json: String,
    ) -> Self {
        MetricEvent {
            id: None,
            metric_id,
            metric_name,
            connection_id,
            timestamp: Self::now_micros(),
            thread_name: None,
            thread_id: None,
            values: vec![ExpressionValue::new("", value_json)],
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        }
    }

    /// Create an error event
    pub fn error(
        metric_id: MetricId,
        metric_name: String,
        connection_id: ConnectionId,
        error_type: String,
        error_message: String,
    ) -> Self {
        MetricEvent {
            id: None,
            metric_id,
            metric_name,
            connection_id,
            timestamp: Self::now_micros(),
            thread_name: None,
            thread_id: None,
            values: vec![],
            is_error: true,
            error_type: Some(error_type),
            error_message: Some(error_message),
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        }
    }

    /// Convenience: get the first value's JSON string (or empty for errors)
    pub fn value_json(&self) -> &str {
        self.values
            .first()
            .map(|v| v.value_json.as_str())
            .unwrap_or("")
    }

    /// Convenience: get the first value's numeric value
    pub fn value_numeric(&self) -> Option<f64> {
        self.values.first().and_then(|v| v.numeric())
    }

    /// Convenience: get the first value's string value
    pub fn value_string(&self) -> Option<&str> {
        self.values.first().and_then(|v| v.text())
    }

    /// Convenience: get the first value's boolean value
    pub fn value_boolean(&self) -> Option<bool> {
        self.values.first().and_then(|v| v.boolean())
    }

    /// Set stack trace on this event
    pub fn with_stack_trace(mut self, stack_trace: CapturedStackTrace) -> Self {
        self.stack_trace = Some(stack_trace);
        self
    }

    /// Set memory snapshot on this event
    pub fn with_memory_snapshot(mut self, memory_snapshot: MemorySnapshot) -> Self {
        self.memory_snapshot = Some(memory_snapshot);
        self
    }
}

/// Group information with metrics count
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupInfo {
    pub name: String,
    pub metric_count: u32,
    pub enabled_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metric_event_new() {
        let event = MetricEvent::new(
            MetricId(1),
            "test_metric".to_string(),
            ConnectionId::from("default"),
            "{\"user_id\":123}".to_string(),
        );

        assert_eq!(event.metric_id, MetricId(1));
        assert_eq!(event.metric_name, "test_metric");
        assert!(event.timestamp > 0);
        assert_eq!(event.value_json(), "{\"user_id\":123}");
        assert_eq!(event.values.len(), 1);
        assert!(!event.is_error);
    }

    #[test]
    fn test_metric_event_error() {
        let event = MetricEvent::error(
            MetricId(1),
            "test_metric".to_string(),
            ConnectionId::from("default"),
            "NameError".to_string(),
            "name 'x' is not defined".to_string(),
        );

        assert_eq!(event.metric_id, MetricId(1));
        assert_eq!(event.metric_name, "test_metric");
        assert!(event.is_error);
        assert_eq!(event.error_type, Some("NameError".to_string()));
        assert_eq!(
            event.error_message,
            Some("name 'x' is not defined".to_string())
        );
        assert!(event.values.is_empty());
        assert!(event.value_json().is_empty());
    }

    #[test]
    fn test_expression_value_new() {
        let ev = ExpressionValue::new("x.value", "42");
        assert_eq!(ev.expression, "x.value");
        assert_eq!(ev.value_json, "42");
        assert!(ev.typed_value.is_none());
    }

    #[test]
    fn test_expression_value_factory_methods() {
        let num = ExpressionValue::with_numeric("x", "42", 42.0);
        assert_eq!(num.numeric(), Some(42.0));
        assert!(num.text().is_none());

        let txt = ExpressionValue::with_text("s", "\"hello\"", "hello");
        assert_eq!(txt.text(), Some("hello"));
        assert!(txt.numeric().is_none());

        let b = ExpressionValue::with_boolean("flag", "true", true);
        assert_eq!(b.boolean(), Some(true));
        assert!(b.text().is_none());
    }

    #[test]
    fn test_metric_event_values_vec() {
        let mut event = MetricEvent::new(
            MetricId(1),
            "multi".to_string(),
            ConnectionId::from("default"),
            "ignored".to_string(),
        );
        event.values = vec![
            ExpressionValue::with_text("symbol", "\"BTCUSD\"", "BTCUSD"),
            ExpressionValue::with_numeric("quantity", "10", 10.0),
            ExpressionValue::with_numeric("price", "100.5", 100.5),
        ];

        assert_eq!(event.values.len(), 3);
        assert_eq!(event.value_json(), "\"BTCUSD\"");
        assert!(event.value_numeric().is_none()); // first value is text
        assert_eq!(event.value_string(), Some("BTCUSD"));
    }

    #[test]
    fn test_metric_event_convenience_methods() {
        let mut event = MetricEvent::new(
            MetricId(1),
            "test".to_string(),
            ConnectionId::from("default"),
            "".to_string(),
        );
        event.values = vec![ExpressionValue::with_numeric("x", "42", 42.0)];

        assert_eq!(event.value_json(), "42");
        assert_eq!(event.value_numeric(), Some(42.0));
        assert!(event.value_string().is_none());
        assert!(event.value_boolean().is_none());
    }

    #[test]
    fn test_serde_round_trip_backward_compat() {
        // Verify the JSON format hasn't changed (flat optional fields)
        let ev = ExpressionValue::with_numeric("x", "42", 42.0);
        let json = serde_json::to_value(&ev).unwrap();

        // Must serialize to flat fields, not a "typedValue" wrapper
        assert_eq!(json["expression"], "x");
        assert_eq!(json["valueJson"], "42");
        assert_eq!(json["valueNumeric"], 42.0);
        assert!(json.get("valueString").is_none());
        assert!(json.get("valueBoolean").is_none());

        // Round-trip: deserialize back
        let ev2: ExpressionValue = serde_json::from_value(json).unwrap();
        assert_eq!(ev2.numeric(), Some(42.0));
        assert!(ev2.text().is_none());

        // Test text variant
        let ev_text = ExpressionValue::with_text("s", "\"hello\"", "hello");
        let json_text = serde_json::to_value(&ev_text).unwrap();
        assert_eq!(json_text["valueString"], "hello");
        assert!(json_text.get("valueNumeric").is_none());
        let ev_text2: ExpressionValue = serde_json::from_value(json_text).unwrap();
        assert_eq!(ev_text2.text(), Some("hello"));

        // Test boolean variant
        let ev_bool = ExpressionValue::with_boolean("flag", "true", true);
        let json_bool = serde_json::to_value(&ev_bool).unwrap();
        assert_eq!(json_bool["valueBoolean"], true);
        let ev_bool2: ExpressionValue = serde_json::from_value(json_bool).unwrap();
        assert_eq!(ev_bool2.boolean(), Some(true));

        // Test None variant
        let ev_none = ExpressionValue::new("raw", "{}");
        let json_none = serde_json::to_value(&ev_none).unwrap();
        assert!(json_none.get("valueNumeric").is_none());
        assert!(json_none.get("valueString").is_none());
        assert!(json_none.get("valueBoolean").is_none());
        let ev_none2: ExpressionValue = serde_json::from_value(json_none).unwrap();
        assert!(ev_none2.typed_value.is_none());
    }
}
