//! GELF Message Format
//!
//! Converts MetricEvent to GELF (Graylog Extended Log Format) JSON messages.
//! See: <https://go2docs.graylog.org/current/getting_in_log_data/gelf.html>

use detrix_config::constants::DEFAULT_GELF_SOURCE_HOST;
use detrix_core::MetricEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// GELF log levels (syslog severity)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum GelfLevel {
    Emergency = 0,
    Alert = 1,
    Critical = 2,
    Error = 3,
    Warning = 4,
    Notice = 5,
    #[default]
    Informational = 6,
    Debug = 7,
}

/// GELF message structure
///
/// All custom fields must be prefixed with underscore (_).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GelfMessage {
    /// GELF spec version (always "1.1")
    pub version: String,

    /// Hostname of the source (Detrix daemon)
    pub host: String,

    /// Short description (metric name)
    pub short_message: String,

    /// Long description (full context)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_message: Option<String>,

    /// Unix timestamp with optional decimal places for milliseconds
    pub timestamp: f64,

    /// Syslog level (0-7)
    pub level: u8,

    // --- Custom fields (GELF requires underscore prefix) ---
    /// Metric ID
    #[serde(rename = "_metric_id")]
    pub metric_id: u64,

    /// Metric name
    #[serde(rename = "_metric_name")]
    pub metric_name: String,

    /// Connection ID (which debugged process)
    #[serde(rename = "_connection_id")]
    pub connection_id: String,

    /// JSON value captured
    #[serde(rename = "_value_json")]
    pub value_json: String,

    /// Thread name
    #[serde(rename = "_thread_name", skip_serializing_if = "Option::is_none")]
    pub thread_name: Option<String>,

    /// Thread ID
    #[serde(rename = "_thread_id", skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,

    /// Error flag
    #[serde(rename = "_is_error")]
    pub is_error: bool,

    /// Error type (e.g., "NameError")
    #[serde(rename = "_error_type", skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,

    /// Error message
    #[serde(rename = "_error_message", skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,

    /// Request ID for correlation
    #[serde(rename = "_request_id", skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    /// Additional custom fields
    #[serde(flatten)]
    pub extra_fields: HashMap<String, serde_json::Value>,
}

impl GelfMessage {
    /// GELF version
    pub const VERSION: &'static str = "1.1";

    /// Create a GELF message from a MetricEvent
    pub fn from_event(event: &MetricEvent, host: Option<&str>) -> Self {
        let level = if event.is_error {
            GelfLevel::Error
        } else {
            GelfLevel::Informational
        };

        // Convert microseconds timestamp to seconds with fractional part
        let timestamp = event.timestamp as f64 / 1_000_000.0;

        Self {
            version: Self::VERSION.to_string(),
            host: host.unwrap_or(DEFAULT_GELF_SOURCE_HOST).to_string(),
            short_message: format!("Metric: {}", event.metric_name),
            full_message: Some(format!(
                "{} (connection: {})",
                event.metric_name, event.connection_id.0
            )),
            timestamp,
            level: level as u8,
            metric_id: event.metric_id.0,
            metric_name: event.metric_name.clone(),
            connection_id: event.connection_id.0.clone(),
            value_json: event.value_json.clone(),
            thread_name: event.thread_name.clone(),
            thread_id: event.thread_id,
            is_error: event.is_error,
            error_type: event.error_type.clone(),
            error_message: event.error_message.clone(),
            request_id: event.request_id.clone(),
            extra_fields: HashMap::new(),
        }
    }

    /// Add a custom field (will be prefixed with _ if not already)
    pub fn add_field(&mut self, key: &str, value: serde_json::Value) {
        let key = if key.starts_with('_') {
            key.to_string()
        } else {
            format!("_{}", key)
        };
        self.extra_fields.insert(key, value);
    }

    /// Serialize to JSON bytes
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Serialize to JSON string (for debugging)
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Serialize to JSON bytes with null terminator (for GELF TCP)
    pub fn to_json_null_terminated(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut bytes = self.to_json()?;
        bytes.push(0);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_core::{ConnectionId, MetricId};

    fn sample_event() -> MetricEvent {
        MetricEvent {
            id: Some(1),
            metric_id: MetricId(42),
            metric_name: "order_placed".to_string(),
            connection_id: ConnectionId::new("trading-bot-1"),
            timestamp: 1733590800_123456, // 2024-12-07 some time, in microseconds
            thread_name: Some("MainThread".to_string()),
            thread_id: Some(12345),
            value_json: r#"{"symbol": "AAPL", "qty": 100, "price": 150.25}"#.to_string(),
            value_numeric: Some(150.25),
            value_string: None,
            value_boolean: None,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: Some("req-123".to_string()),
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        }
    }

    fn sample_error_event() -> MetricEvent {
        MetricEvent {
            id: Some(2),
            metric_id: MetricId(43),
            metric_name: "error_context".to_string(),
            connection_id: ConnectionId::new("api-server"),
            timestamp: 1733590801_000000,
            thread_name: Some("worker-1".to_string()),
            thread_id: Some(9999),
            value_json: r#"{"error": "division by zero"}"#.to_string(),
            value_numeric: None,
            value_string: None,
            value_boolean: None,
            is_error: true,
            error_type: Some("ZeroDivisionError".to_string()),
            error_message: Some("division by zero".to_string()),
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        }
    }

    #[test]
    fn test_gelf_message_from_event() {
        let event = sample_event();
        let msg = GelfMessage::from_event(&event, None);

        assert_eq!(msg.version, "1.1");
        assert_eq!(msg.host, "detrix");
        assert_eq!(msg.short_message, "Metric: order_placed");
        assert_eq!(msg.level, GelfLevel::Informational as u8);
        assert_eq!(msg.metric_id, 42);
        assert_eq!(msg.metric_name, "order_placed");
        assert_eq!(msg.connection_id, "trading-bot-1");
        assert!(!msg.is_error);
        assert_eq!(msg.thread_name, Some("MainThread".to_string()));
        assert_eq!(msg.thread_id, Some(12345));
        assert_eq!(msg.request_id, Some("req-123".to_string()));
    }

    #[test]
    fn test_gelf_message_from_error_event() {
        let event = sample_error_event();
        let msg = GelfMessage::from_event(&event, Some("my-host"));

        assert_eq!(msg.host, "my-host");
        assert_eq!(msg.level, GelfLevel::Error as u8);
        assert!(msg.is_error);
        assert_eq!(msg.error_type, Some("ZeroDivisionError".to_string()));
        assert_eq!(msg.error_message, Some("division by zero".to_string()));
    }

    #[test]
    fn test_gelf_message_timestamp_conversion() {
        let event = sample_event();
        let msg = GelfMessage::from_event(&event, None);

        // 1733590800_123456 microseconds = 1733590800.123456 seconds
        let expected = 1733590800.123456;
        assert!((msg.timestamp - expected).abs() < 0.000001);
    }

    #[test]
    fn test_gelf_message_add_field() {
        let event = sample_event();
        let mut msg = GelfMessage::from_event(&event, None);

        // With underscore prefix
        msg.add_field("_environment", serde_json::json!("production"));
        assert_eq!(
            msg.extra_fields.get("_environment"),
            Some(&serde_json::json!("production"))
        );

        // Without underscore prefix (should be added)
        msg.add_field("datacenter", serde_json::json!("us-east-1"));
        assert_eq!(
            msg.extra_fields.get("_datacenter"),
            Some(&serde_json::json!("us-east-1"))
        );
    }

    #[test]
    fn test_gelf_message_to_json() {
        let event = sample_event();
        let msg = GelfMessage::from_event(&event, None);

        let json = msg.to_json_string().expect("Failed to serialize");

        // Verify required GELF fields are present
        assert!(json.contains("\"version\":\"1.1\""));
        assert!(json.contains("\"host\":\"detrix\""));
        assert!(json.contains("\"short_message\":"));
        assert!(json.contains("\"timestamp\":"));
        assert!(json.contains("\"level\":"));

        // Verify custom fields have underscore prefix
        assert!(json.contains("\"_metric_id\":"));
        assert!(json.contains("\"_metric_name\":"));
        assert!(json.contains("\"_connection_id\":"));
        assert!(json.contains("\"_value_json\":"));
    }

    #[test]
    fn test_gelf_message_null_terminated() {
        let event = sample_event();
        let msg = GelfMessage::from_event(&event, None);

        let bytes = msg.to_json_null_terminated().expect("Failed to serialize");

        // Should end with null byte
        assert_eq!(bytes.last(), Some(&0u8));

        // Should be valid JSON before null byte
        let json_bytes = &bytes[..bytes.len() - 1];
        let _: serde_json::Value =
            serde_json::from_slice(json_bytes).expect("Invalid JSON before null");
    }

    #[test]
    fn test_gelf_level_values() {
        assert_eq!(GelfLevel::Emergency as u8, 0);
        assert_eq!(GelfLevel::Alert as u8, 1);
        assert_eq!(GelfLevel::Critical as u8, 2);
        assert_eq!(GelfLevel::Error as u8, 3);
        assert_eq!(GelfLevel::Warning as u8, 4);
        assert_eq!(GelfLevel::Notice as u8, 5);
        assert_eq!(GelfLevel::Informational as u8, 6);
        assert_eq!(GelfLevel::Debug as u8, 7);
    }
}
