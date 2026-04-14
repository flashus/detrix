//! Protocol conversions — proto ↔ domain type conversions.

use detrix_api::generated::detrix::v1::SerializedMetricEvent;
use detrix_core::MetricEvent;

/// Convert a domain MetricEvent to a proto SerializedMetricEvent.
pub fn metric_event_to_proto(event: &MetricEvent) -> SerializedMetricEvent {
    let values_json = serde_json::to_string(&event.values).unwrap_or_default();
    SerializedMetricEvent {
        metric_id: event.metric_id.0,
        metric_name: event.metric_name.clone(),
        timestamp_ns: event.timestamp * 1_000, // micros → nanos
        thread_name: event.thread_name.clone().unwrap_or_default(),
        thread_id: event.thread_id.unwrap_or(0),
        values_json,
        is_error: event.is_error,
        error_message: event.error_message.clone().unwrap_or_default(),
    }
}

/// Truncate values_json if it exceeds 64 KB.
pub fn truncate_values_json(values_json: &str) -> String {
    const MAX_SIZE: usize = 65_536; // 64 KB
    if values_json.len() > MAX_SIZE {
        let original_size = values_json.len();
        format!(r#"{{"_truncated":true,"_size_bytes":{original_size}}}"#)
    } else {
        values_json.to_string()
    }
}
