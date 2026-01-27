//! Metric event entity - runtime events captured from target applications

use super::memory::{CapturedStackTrace, MemorySnapshot};
use super::metric::MetricId;
use crate::connection::ConnectionId;
use chrono::Utc;
use serde::{Deserialize, Serialize};

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
    pub value_json: String,
    pub value_numeric: Option<f64>,
    pub value_string: Option<String>,
    pub value_boolean: Option<bool>,
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

    /// Create a new event
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
            value_json,
            value_numeric: None,
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
            value_json: String::new(),
            value_numeric: None,
            value_string: None,
            value_boolean: None,
            is_error: true,
            error_type: Some(error_type),
            error_message: Some(error_message),
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        }
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
        assert_eq!(event.value_json, "{\"user_id\":123}");
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
        assert!(event.value_json.is_empty());
    }
}
