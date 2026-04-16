//! Protocol conversions — proto ↔ domain type conversions.

use detrix_api::generated::detrix::v1::{SerializedMetricEvent, SetMetric};
use detrix_core::{Metric, MetricEvent};

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

/// Convert a proto SetMetric to a domain Metric.
///
/// Metric does NOT implement Default (confirmed via crates/detrix-core/src/entities/metric.rs).
/// MetricId and ConnectionId also have no Default. expressions is Vec<String> (not Vec<Expression>).
/// id is Option<MetricId>. All 21 fields must be listed explicitly:
pub fn proto_set_metric_to_metric(msg: &SetMetric) -> detrix_core::Result<Metric> {
    Ok(detrix_core::Metric {
        id: Some(detrix_core::MetricId(msg.metric_id)),
        name: msg.metric_name.clone(),
        connection_id: detrix_core::ConnectionId(msg.connection_id.clone()),
        group: None,
        location: detrix_core::Location {
            file: msg.file.clone(),
            line: msg.line,
        },
        expressions: msg.expressions.clone(), // Vec<String> directly
        language: detrix_core::SourceLanguage::Go,
        enabled: msg.enabled,
        mode: detrix_core::MetricMode::default(),
        condition: None,
        safety_level: detrix_core::SafetyLevel::default(),
        created_at: None,
        user_id: None,
        agent_id: None,
        capture_stack_trace: false,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: false,
        snapshot_scope: None,
        snapshot_ttl: None,
        anchor: None,
        anchor_status: detrix_core::AnchorStatus::default(),
    })
}
