//! Shared Logpoint Parsing
//!
//! This module contains common parsing logic used by all language adapters
//! for parsing logpoint output and creating metric events.

use super::traits::{ThreadExtractor, ThreadInfo, DETRICS_PREFIX, MULTI_EXPR_DELIMITER};
use crate::OutputEventBody;
use detrix_core::{ExpressionValue, Metric, MetricEvent, TypedValue};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ============================================================================
// Shared Logpoint Parsing
// ============================================================================

/// Core DETRICS logpoint parsing result.
///
/// Contains parsed values and thread information for creating a MetricEvent.
#[derive(Debug)]
pub struct LogpointParseResult {
    /// Metric name from the logpoint
    pub metric_name: String,
    /// All value parts split by MULTI_EXPR_DELIMITER (single expr = 1 element)
    pub value_parts: Vec<String>,
    /// Thread information extracted from output
    pub thread_info: ThreadInfo,
}

/// Parse the core DETRICS logpoint format with thread extraction.
///
/// This handles the common flow for all languages:
/// 1. Extract thread info (via ThreadExtractor)
/// 2. Find DETRICS: prefix
/// 3. Parse metric_name=value format
///
/// # Arguments
/// * `output` - Raw debugger output text
/// * `extractor` - Language-specific thread extractor
///
/// # Returns
/// `Some(LogpointParseResult)` if a valid DETRICS logpoint was found, `None` otherwise.
pub fn parse_logpoint_core<E: ThreadExtractor>(
    output: &str,
    extractor: &E,
) -> Option<LogpointParseResult> {
    let output = output.trim();

    // 1. Extract thread info and get cleaned text
    let thread_info = extractor.extract_thread_info(output);
    let text = &thread_info.cleaned_text;

    // 2. Find DETRICS: prefix
    if !text.starts_with(DETRICS_PREFIX) {
        return None;
    }

    let rest = text.strip_prefix(DETRICS_PREFIX)?;

    // 3. Parse metric_name=value format
    // For Python with introspection: "name=value|ST:...|MS:..."
    // For Go/Rust: "name=value"
    let main_content = rest.split('|').next()?;
    let parts: Vec<&str> = main_content.splitn(2, '=').collect();
    if parts.len() != 2 {
        return None;
    }

    let metric_name = parts[0].to_string();

    // Split by multi-expression delimiter (single expr = 1 element)
    let value_parts: Vec<String> = parts[1]
        .split(MULTI_EXPR_DELIMITER)
        .map(|s| s.to_string())
        .collect();

    Some(LogpointParseResult {
        metric_name,
        value_parts,
        thread_info,
    })
}

/// Create a MetricEvent from parsed logpoint data.
///
/// This is the final step of logpoint parsing - creating the domain event
/// from parsed components.
///
/// # Arguments
/// * `parse_result` - Parsed logpoint data from `parse_logpoint_core`
/// * `active_metrics` - Map of active metrics to match against
///
/// # Returns
/// `Some(MetricEvent)` if the metric was found and event created, `None` otherwise.
pub async fn create_metric_event_from_logpoint(
    parse_result: &LogpointParseResult,
    active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
) -> Option<MetricEvent> {
    // Find the metric by name
    let metrics = active_metrics.read().await;
    let metric = metrics
        .values()
        .find(|m| m.name == parse_result.metric_name);
    let metric = match metric {
        Some(m) => m,
        None => {
            tracing::debug!(
                "No active metric found for logpoint name '{}'",
                parse_result.metric_name
            );
            return None;
        }
    };
    let metric_id = match metric.id {
        Some(id) => id,
        None => {
            tracing::debug!(
                "Metric '{}' has no ID assigned, skipping event",
                parse_result.metric_name
            );
            return None;
        }
    };
    let metric_name = metric.name.clone();
    let connection_id = metric.connection_id.clone();
    let expressions = metric.expressions.clone();
    drop(metrics);

    // Warn if expression count doesn't match parsed value count (zip will truncate)
    if expressions.len() != parse_result.value_parts.len() {
        tracing::warn!(
            "Expression/value count mismatch for metric '{}': {} expressions, {} values",
            parse_result.metric_name,
            expressions.len(),
            parse_result.value_parts.len(),
        );
    }

    // Zip metric expressions with parsed value parts to create ExpressionValues
    let expression_values: Vec<ExpressionValue> = expressions
        .iter()
        .zip(parse_result.value_parts.iter())
        .map(|(expr, val)| {
            let (vj, typed) = parse_value(val);
            ExpressionValue {
                expression: expr.clone(),
                value_json: vj,
                typed_value: typed,
            }
        })
        .collect();

    Some(MetricEvent {
        id: None,
        metric_id,
        metric_name,
        connection_id,
        timestamp: MetricEvent::now_micros(),
        thread_name: parse_result.thread_info.thread_name.clone(),
        thread_id: parse_result.thread_info.thread_id,
        values: expression_values,
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    })
}

// ============================================================================
// Shared Error Handling Utilities
// ============================================================================

/// Parse a value string into JSON and a typed value with JSON type detection.
///
/// This is shared logic used by all language adapters for value parsing.
/// It attempts to parse the value as JSON to extract a typed value.
///
/// # Returns
/// A tuple of (value_json, typed_value)
pub fn parse_value(value_str: &str) -> (String, Option<TypedValue>) {
    // Try to parse as JSON
    if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(value_str) {
        let value_json =
            serde_json::to_string(&json_value).unwrap_or_else(|_| value_str.to_string());

        let typed = if let Some(n) = json_value.as_f64() {
            Some(TypedValue::Numeric(n))
        } else if let Some(s) = json_value.as_str() {
            Some(TypedValue::Text(s.to_string()))
        } else {
            json_value.as_bool().map(TypedValue::Boolean)
        };

        (value_json, typed)
    } else {
        // Fallback: treat as plain string, try numeric parse
        let typed = if let Ok(n) = value_str.parse::<f64>() {
            Some(TypedValue::Numeric(n))
        } else {
            Some(TypedValue::Text(value_str.to_string()))
        };
        (value_str.to_string(), typed)
    }
}

/// Find metric by DAP output location, with fallback to first active metric.
///
/// This is shared logic used by all language adapters for error event creation.
/// It first tries to find a metric by exact location (file:line), then falls back
/// to the first active metric if no location match is found.
///
/// # Arguments
/// * `output` - The DAP output event containing source/line information
/// * `metrics` - Map of active metrics keyed by location (file:line)
///
/// # Returns
/// * `Some((metric_id, metric_name, connection_id))` if a metric is found
/// * `None` if no metrics are active or none have IDs
pub fn find_metric_for_error(
    output: &OutputEventBody,
    metrics: &HashMap<String, Metric>,
) -> Option<(detrix_core::MetricId, String, detrix_core::ConnectionId)> {
    // Try to find the metric by location first (most accurate)
    let metric_by_location = if let (Some(source), Some(line)) = (&output.source, output.line) {
        if let Some(path) = &source.path {
            let filename = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path);
            let location_key = format!("{}:{}", filename, line);
            metrics.get(&location_key)
        } else {
            None
        }
    } else {
        None
    };

    // Determine which metric this error belongs to
    if let Some(metric) = metric_by_location {
        metric
            .id
            .map(|id| (id, metric.name.clone(), metric.connection_id.clone()))
    } else {
        // Fall back to first active metric (may not be accurate)
        metrics.values().next().and_then(|metric| {
            metric
                .id
                .map(|id| (id, metric.name.clone(), metric.connection_id.clone()))
        })
    }
}
