//! JSON serialization/deserialization helpers for storage repositories
//!
//! Provides consistent patterns for handling optional JSON fields in database operations.

use serde::{de::DeserializeOwned, Serialize};
use tracing::warn;

/// Serialize an optional value to JSON string, returning None on failure.
///
/// Logs a warning if serialization fails.
///
/// # Example
/// ```ignore
/// let stack_trace_json = serialize_optional(
///     &event.stack_trace,
///     "stack_trace",
///     &format!("metric_id={}", event.metric_id.0),
/// );
/// ```
pub fn serialize_optional<T: Serialize>(
    value: &Option<T>,
    field_name: &str,
    context: &str,
) -> Option<String> {
    value.as_ref().and_then(|v| {
        serde_json::to_string(v)
            .map_err(|e| {
                warn!(
                    field = field_name,
                    context = context,
                    error = %e,
                    "Failed to serialize {}, storing as NULL", field_name
                );
            })
            .ok()
    })
}

/// Deserialize an optional JSON string to a typed value, returning None on failure.
///
/// Logs a warning if deserialization fails.
///
/// # Example
/// ```ignore
/// let stack_trace: Option<StackTrace> = deserialize_optional(
///     row_json.as_deref(),
///     "stack_trace",
///     &format!("event_id={}", id),
/// );
/// ```
pub fn deserialize_optional<T: DeserializeOwned>(
    json: Option<&str>,
    field_name: &str,
    context: &str,
) -> Option<T> {
    json.and_then(|s| {
        serde_json::from_str(s)
            .map_err(|e| {
                warn!(
                    field = field_name,
                    context = context,
                    error = %e,
                    "Failed to deserialize {}", field_name
                );
            })
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestData {
        value: i32,
        name: String,
    }

    #[test]
    fn test_serialize_optional_some() {
        let data = Some(TestData {
            value: 42,
            name: "test".to_string(),
        });
        let result = serialize_optional(&data, "test_field", "test_context");
        assert!(result.is_some());
        assert!(result.unwrap().contains("42"));
    }

    #[test]
    fn test_serialize_optional_none() {
        let data: Option<TestData> = None;
        let result = serialize_optional(&data, "test_field", "test_context");
        assert!(result.is_none());
    }

    #[test]
    fn test_deserialize_optional_valid() {
        let json = Some(r#"{"value": 42, "name": "test"}"#);
        let result: Option<TestData> = deserialize_optional(json, "test_field", "test_context");
        assert!(result.is_some());
        assert_eq!(result.unwrap().value, 42);
    }

    #[test]
    fn test_deserialize_optional_none() {
        let result: Option<TestData> = deserialize_optional(None, "test_field", "test_context");
        assert!(result.is_none());
    }

    #[test]
    fn test_deserialize_optional_invalid() {
        let json = Some("invalid json");
        let result: Option<TestData> = deserialize_optional(json, "test_field", "test_context");
        assert!(result.is_none());
    }
}
