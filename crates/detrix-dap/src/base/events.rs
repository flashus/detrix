//! Event Handlers
//!
//! This module contains the event handling logic for DAP events,
//! including output events (logpoints) and stopped events (breakpoints).

use super::introspection::{
    capture_memory_snapshot, capture_stack_trace, evaluate_expression, find_metrics_at_location,
};
use super::traits::OutputParser;
use crate::{
    constants::{defaults, events, requests, stop_reasons},
    ext::DebugResult,
    AdapterProcess, DapBroker, OutputEventBody, StackTraceArguments, StackTraceResponseBody,
    StoppedEventBody,
};
use detrix_config::constants::DEFAULT_DAP_VALUE_DISPLAY_LIMIT;
use detrix_core::{expression_contains_function_call, Metric, MetricEvent};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;
use tokio::sync::RwLock;
use tracing::{debug, error, info, trace, warn};

// ============================================================================
// Event Handlers - Extracted from subscribe_events for clarity
// ============================================================================

/// Main event handling loop that processes DAP events.
pub async fn handle_events<P: OutputParser>(
    mut event_rx: Receiver<crate::Event>,
    active_metrics: Arc<RwLock<HashMap<String, Metric>>>,
    adapter: Arc<AdapterProcess>,
    tx: tokio::sync::mpsc::Sender<MetricEvent>,
    lang: &'static str,
) {
    debug!("[{}] Event parsing task started", lang);

    while let Some(event) = event_rx.recv().await {
        trace!("[{}] Received DAP event: {}", lang, event.event);

        let should_exit = match event.event.as_str() {
            events::OUTPUT => {
                handle_output_event::<P>(event.body, &active_metrics, &tx, lang).await
            }
            events::STOPPED => {
                handle_stopped_event(event.body, &active_metrics, &adapter, &tx, lang).await
            }
            _ => {
                trace!("[{}] Ignoring event: {}", lang, event.event);
                false
            }
        };

        if should_exit {
            break;
        }
    }

    debug!("[{}] Event parsing task ended", lang);
}

/// Handle DAP "output" events (logpoints without introspection).
/// Returns `true` if the event loop should exit (channel closed).
pub async fn handle_output_event<P: OutputParser>(
    body: Option<serde_json::Value>,
    active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    tx: &tokio::sync::mpsc::Sender<MetricEvent>,
    lang: &'static str,
) -> bool {
    let Some(body) = body else {
        return false;
    };

    let Ok(output) = serde_json::from_value::<OutputEventBody>(body) else {
        return false;
    };

    trace!("[{}] Output: {}", lang, output.output);

    let Some(metric_event) = P::parse_output(&output, active_metrics).await else {
        return false;
    };

    // Look up metric to get location for logging
    let location_str = {
        let metrics = active_metrics.read().await;
        metrics
            .values()
            .find(|m| m.name == metric_event.metric_name)
            .map(|m| format!("{}:{}", m.location.file, m.location.line))
            .unwrap_or_else(|| "unknown".to_string())
    };

    // Truncate value for display
    let primary_value = metric_event.value_json();
    let value_display = if primary_value.len() > DEFAULT_DAP_VALUE_DISPLAY_LIMIT {
        format!("{}...", &primary_value[..DEFAULT_DAP_VALUE_DISPLAY_LIMIT])
    } else {
        primary_value.to_string()
    };

    info!(
        "[{}] {} @ {} = {}",
        lang, metric_event.metric_name, location_str, value_display
    );

    debug!(
        "[{}] Parsed metric event: stack_trace={}, memory_snapshot={}",
        lang,
        metric_event.stack_trace.is_some(),
        metric_event.memory_snapshot.is_some()
    );

    match tx.try_send(metric_event) {
        Ok(()) => false,
        Err(tokio::sync::mpsc::error::TrySendError::Full(evt)) => {
            warn!(
                "[{}] Event channel full, dropping metric event (id={}) - slow consumer",
                lang, evt.metric_id.0
            );
            false
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            // Receiver dropped - signal exit
            true
        }
    }
}

/// Handle DAP "stopped" events (breakpoints with introspection).
/// Returns `true` if the event loop should exit (channel closed).
pub async fn handle_stopped_event(
    body: Option<serde_json::Value>,
    active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    adapter: &Arc<AdapterProcess>,
    tx: &tokio::sync::mpsc::Sender<MetricEvent>,
    lang: &'static str,
) -> bool {
    warn!("[{}] Received stopped event", lang);

    let Some(body) = body else {
        return false;
    };

    let Ok(stopped) = serde_json::from_value::<StoppedEventBody>(body) else {
        return false;
    };

    warn!("[{}] Stopped reason: {}", lang, stopped.reason);

    // Only handle breakpoint stops
    if stopped.reason != stop_reasons::BREAKPOINT {
        return false;
    }

    // Use default thread ID if not specified (see constants::defaults::THREAD_ID for rationale)
    let thread_id = stopped.thread_id.unwrap_or(defaults::THREAD_ID);
    debug!("[{}] Stopped at breakpoint, thread_id={}", lang, thread_id);

    let Ok(broker) = adapter.broker().await else {
        return false;
    };

    // Get stopped location from stack trace
    let Some((file, line, frame_id)) = get_stopped_location(&broker, thread_id).await else {
        // Still need to continue even if we can't get location
        send_continue(&broker, thread_id, lang).await;
        return false;
    };

    // Find and process metrics at this location
    let metrics_at_location = find_metrics_at_location(active_metrics, &file, line).await;

    if metrics_at_location.is_empty() {
        warn!(
            "[{}] No metric found at {}:{}, may be external breakpoint",
            lang, file, line
        );
    } else {
        warn!(
            "[{}] Found {} metric(s) at {}:{}: {:?}",
            lang,
            metrics_at_location.len(),
            file,
            line,
            metrics_at_location
                .iter()
                .map(|m| &m.name)
                .collect::<Vec<_>>()
        );

        for metric in metrics_at_location {
            process_stopped_metric(&broker, &metric, thread_id, frame_id, tx, lang).await;
        }
    }

    // Always continue execution after processing
    send_continue(&broker, thread_id, lang).await;
    false
}

/// Get the file, line, and frame ID where execution stopped.
pub async fn get_stopped_location(
    broker: &Arc<DapBroker>,
    thread_id: i64,
) -> Option<(String, u32, i64)> {
    let stack_args = StackTraceArguments {
        thread_id,
        start_frame: Some(0),
        levels: Some(1),
        format: None,
    };

    let args_json =
        serde_json::to_value(&stack_args).debug_ok("Failed to serialize stack trace args")?;

    let stack_resp = broker
        .send_request(requests::STACK_TRACE, Some(args_json))
        .await
        .debug_ok("Stack trace request failed")?;

    if !stack_resp.success {
        debug!("Stack trace request unsuccessful");
        return None;
    }

    let body = stack_resp.body?;
    let stack_body: StackTraceResponseBody =
        serde_json::from_value(body).debug_ok("Failed to parse stack trace response")?;
    let top_frame = stack_body.stack_frames.first()?;

    let file = top_frame
        .source
        .as_ref()
        .and_then(|s| s.path.clone())
        .unwrap_or_default();
    let line = top_frame.line as u32;
    let frame_id = top_frame.id;

    Some((file, line, frame_id))
}

/// Process a metric at a stopped breakpoint location.
///
/// Evaluates the metric's expression via DAP evaluate and captures optional
/// introspection data (stack trace, memory snapshot).
///
/// Only metrics set as true breakpoints (not logpoints) receive `stopped` events:
/// - Introspection metrics (stack trace and/or memory snapshot capture)
/// - Go function call metrics (require "call" prefix via DAP evaluate)
///
/// Simple variable metrics without introspection are set as logpoints and handled
/// by `handle_output_event` instead (via DAP `output` events).
///
/// NOTE: Go/Delve supports function calls via DAP evaluate requests using the
/// `call` prefix (e.g., `call len(x)`). Detrix auto-detects function calls and
/// adds this prefix automatically. However, **variadic functions are NOT supported**
/// (e.g., fmt.Sprintf, fmt.Println) - they will return an error.
pub async fn process_stopped_metric(
    broker: &Arc<DapBroker>,
    metric: &Metric,
    thread_id: i64,
    frame_id: i64,
    tx: &tokio::sync::mpsc::Sender<MetricEvent>,
    lang: &'static str,
) {
    let needs_introspection = metric.capture_stack_trace || metric.capture_memory_snapshot;
    let is_go_function_call = metric.language == detrix_core::SourceLanguage::Go
        && metric
            .expressions
            .iter()
            .any(|e| expression_contains_function_call(e));

    if is_go_function_call {
        debug!(
            "[{}] Processing Go function call metric '{}' (expressions: {:?})",
            lang, metric.name, metric.expressions
        );
    } else if needs_introspection {
        debug!(
            "[{}] Processing introspection metric '{}' (stack={}, memory={})",
            lang, metric.name, metric.capture_stack_trace, metric.capture_memory_snapshot
        );
    } else {
        debug!(
            "[{}] Processing metric '{}' at breakpoint (no introspection requested)",
            lang, metric.name
        );
    }

    // Capture introspection data
    let stack_trace = if metric.capture_stack_trace {
        capture_stack_trace(broker, thread_id, metric).await
    } else {
        None
    };

    let memory_snapshot = if metric.capture_memory_snapshot {
        capture_memory_snapshot(broker, thread_id, metric).await
    } else {
        None
    };

    // Create metric event - metric must have an ID (saved in storage before evaluation)
    let Some(metric_id) = metric.id else {
        error!(
            "[{}] Metric '{}' missing ID - skipping event (storage integrity issue)",
            lang, metric.name
        );
        return;
    };

    // NOTE: Expressions are evaluated sequentially — DAP requires serialized
    // evaluate requests per-thread. Breakpoint pause duration scales linearly
    // with expression count. For time-sensitive code, prefer logpoint mode.
    let mut expression_values = Vec::new();
    for expr in &metric.expressions {
        let (eval_expr, eval_context) = if metric.language == detrix_core::SourceLanguage::Go
            && expression_contains_function_call(expr)
        {
            warn!(
                "[{}] Using blocking 'call' for function expression '{}' - consider using simple variables",
                lang, expr
            );
            (format!("call {}", expr), "repl")
        } else {
            (expr.clone(), "watch")
        };

        let value = evaluate_expression(broker, &eval_expr, frame_id, eval_context)
            .await
            .unwrap_or_else(|err| format!("<evaluation failed: {}>", err));

        let (vj, typed) = super::parsing::parse_value(&value);
        expression_values.push(detrix_core::ExpressionValue {
            expression: expr.clone(),
            value_json: vj,
            typed_value: typed,
        });
    }

    let metric_event = MetricEvent {
        id: None,
        metric_id,
        metric_name: metric.name.clone(),
        connection_id: metric.connection_id.clone(),
        timestamp: MetricEvent::now_micros(),
        thread_name: None,
        thread_id: None,
        values: expression_values,
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace,
        memory_snapshot,
    };

    warn!(
        "[{}] INTROSPECTION EVENT for '{}': value={} (stack={}, memory={})",
        lang,
        metric.name,
        metric_event.value_json(),
        metric_event.stack_trace.is_some(),
        metric_event.memory_snapshot.is_some()
    );

    // Send event (ignore errors - channel may be full or closed)
    if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send(metric_event) {
        warn!(
            "[{}] Event channel full, dropping introspection event for '{}' - slow consumer",
            lang, metric.name
        );
    }
}

/// Send continue command to resume execution.
pub async fn send_continue(broker: &Arc<DapBroker>, thread_id: i64, lang: &'static str) {
    let continue_args = serde_json::json!({ "threadId": thread_id });
    if let Err(e) = broker
        .send_request(requests::CONTINUE, Some(continue_args))
        .await
    {
        error!("[{}] Failed to continue after introspection: {}", lang, e);
    } else {
        debug!("[{}] Continued execution after introspection", lang);
    }
}
