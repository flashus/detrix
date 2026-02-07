//! Introspection Capture
//!
//! This module contains helper functions for capturing introspection data
//! including stack traces, memory snapshots (variables), and expression evaluation.

use crate::{
    constants::requests, ext::DebugResult, DapBroker, ScopesArguments, ScopesResponseBody,
    StackTraceArguments, StackTraceResponseBody, VariablesArguments, VariablesResponseBody,
};
use detrix_config::constants::{
    DAP_LINE_TOLERANCE, DEFAULT_MAX_STACK_FRAMES, FULL_STACK_TRACE_MAX_FRAMES,
    MAX_VARIABLE_CAPTURE_COUNT,
};
use detrix_core::{
    CapturedStackTrace, CapturedVariable, MemorySnapshot, Metric, SnapshotScope, StackFrame,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, trace, warn};

// ============================================================================
// Introspection Capture - Helper functions for stack trace and variable capture
// ============================================================================

/// Capture stack trace from DAP adapter
pub async fn capture_stack_trace(
    broker: &Arc<DapBroker>,
    thread_id: i64,
    metric: &Metric,
) -> Option<CapturedStackTrace> {
    // Get stack trace slice configuration
    let slice_config = metric.stack_trace_slice.as_ref();
    let is_full = slice_config.map(|s| s.full).unwrap_or(false);

    let max_frames = if is_full {
        FULL_STACK_TRACE_MAX_FRAMES // Capture up to max frames for "full" trace
    } else if slice_config.is_some() {
        // For slicing, we need all frames to slice correctly
        FULL_STACK_TRACE_MAX_FRAMES
    } else {
        DEFAULT_MAX_STACK_FRAMES // Default reasonable limit
    };

    let args = StackTraceArguments {
        thread_id,
        start_frame: Some(0),
        levels: Some(max_frames as i64), // Convert to i64 for DAP protocol
        format: None,
    };

    let args_json = serde_json::to_value(&args).debug_ok("Failed to serialize stack trace args")?;

    let response = broker
        .send_request(requests::STACK_TRACE, Some(args_json))
        .await
        .debug_ok("Stack trace request failed")?;

    if !response.success {
        warn!("stackTrace request failed: {:?}", response.message);
        return None;
    }

    let body: StackTraceResponseBody =
        serde_json::from_value(response.body?).debug_ok("Failed to parse stack trace response")?;

    let total_frames = body.total_frames.unwrap_or(body.stack_frames.len() as i64) as u32;

    // Convert DAP stack frames to domain stack frames
    let mut frames: Vec<StackFrame> = body
        .stack_frames
        .iter()
        .enumerate()
        .map(|(idx, dap_frame)| {
            let file = dap_frame
                .source
                .as_ref()
                .and_then(|s| s.path.clone().or_else(|| s.name.clone()));
            StackFrame {
                index: idx as u32,
                name: dap_frame.name.clone(),
                file,
                line: Some(dap_frame.line as u32),
                column: dap_frame.column.map(|c| c as u32),
                module: dap_frame
                    .module_id
                    .as_ref()
                    .and_then(|m| m.as_str())
                    .map(String::from),
            }
        })
        .collect();

    // Apply slicing if configured (and not full trace)
    let truncated = if !is_full {
        if let Some(slice) = slice_config {
            match (slice.head, slice.tail) {
                (Some(head), Some(tail)) => {
                    let head = head as usize;
                    let tail = tail as usize;
                    if frames.len() > head + tail {
                        let mut sliced = frames[..head].to_vec();
                        sliced.extend_from_slice(&frames[frames.len() - tail..]);
                        frames = sliced;
                        true
                    } else {
                        false
                    }
                }
                (Some(head), None) => {
                    let head = head as usize;
                    if frames.len() > head {
                        frames.truncate(head);
                        true
                    } else {
                        false
                    }
                }
                (None, Some(tail)) => {
                    let tail = tail as usize;
                    if frames.len() > tail {
                        let start = frames.len() - tail;
                        frames = frames[start..].to_vec();
                        true
                    } else {
                        false
                    }
                }
                (None, None) => false,
            }
        } else {
            false
        }
    } else {
        false
    };

    Some(CapturedStackTrace {
        frames,
        total_frames,
        truncated,
    })
}

/// Capture memory snapshot (variables) from DAP adapter
pub async fn capture_memory_snapshot(
    broker: &Arc<DapBroker>,
    thread_id: i64,
    metric: &Metric,
) -> Option<MemorySnapshot> {
    // First get the top stack frame to get scopes
    let stack_args = StackTraceArguments {
        thread_id,
        start_frame: Some(0),
        levels: Some(1),
        format: None,
    };

    let stack_args_json = serde_json::to_value(&stack_args)
        .debug_ok("Failed to serialize stack args for memory snapshot")?;

    let stack_response = broker
        .send_request(requests::STACK_TRACE, Some(stack_args_json))
        .await
        .debug_ok("Stack trace request for memory snapshot failed")?;

    if !stack_response.success {
        debug!("Stack trace request for memory snapshot unsuccessful");
        return None;
    }

    let stack_body: StackTraceResponseBody = serde_json::from_value(stack_response.body?)
        .debug_ok("Failed to parse stack response for memory snapshot")?;
    let frame_id = stack_body.stack_frames.first()?.id;

    // Get scopes for the frame
    let scopes_args = ScopesArguments { frame_id };
    let scopes_args_json =
        serde_json::to_value(&scopes_args).debug_ok("Failed to serialize scopes args")?;

    let scopes_response = broker
        .send_request("scopes", Some(scopes_args_json))
        .await
        .debug_ok("Scopes request failed")?;

    if !scopes_response.success {
        debug!("Scopes request unsuccessful");
        return None;
    }

    let scopes_body: ScopesResponseBody = serde_json::from_value(scopes_response.body?)
        .debug_ok("Failed to parse scopes response")?;

    let snapshot_scope = metric.snapshot_scope.unwrap_or_default();

    let mut locals = Vec::new();
    let mut globals = Vec::new();

    for scope in scopes_body.scopes {
        // Skip expensive scopes (usually contain many system variables)
        if scope.expensive == Some(true) && snapshot_scope != SnapshotScope::Both {
            continue;
        }

        let is_local = scope.name.to_lowercase().contains("local")
            || scope.presentation_hint.as_deref() == Some("locals");
        let is_global = scope.name.to_lowercase().contains("global")
            || scope.presentation_hint.as_deref() == Some("globals");

        // Filter based on requested scope
        let should_capture = match snapshot_scope {
            SnapshotScope::Local => is_local,
            SnapshotScope::Global => is_global,
            SnapshotScope::Both => true,
        };

        if !should_capture {
            continue;
        }

        // Get variables for this scope
        let vars_args = VariablesArguments {
            variables_reference: scope.variables_reference,
            filter: None,
            start: None,
            count: Some(MAX_VARIABLE_CAPTURE_COUNT as i64), // Limit to prevent huge captures
            format: None,
        };

        let Some(vars_args_json) = serde_json::to_value(&vars_args).debug_ok(&format!(
            "Failed to serialize variables args for scope '{}'",
            scope.name
        )) else {
            continue;
        };

        if let Ok(vars_response) = broker.send_request("variables", Some(vars_args_json)).await {
            if vars_response.success {
                if let Some(body) = vars_response.body {
                    if let Ok(vars_body) = serde_json::from_value::<VariablesResponseBody>(body) {
                        for var in vars_body.variables {
                            // Skip internal/special variables
                            if var.name.starts_with("__") && var.name.ends_with("__") {
                                continue;
                            }

                            let captured_var = CapturedVariable {
                                name: var.name,
                                value: var.value,
                                var_type: var.var_type,
                                scope: Some(scope.name.clone()),
                            };

                            if is_local {
                                locals.push(captured_var);
                            } else {
                                globals.push(captured_var);
                            }
                        }
                    }
                }
            }
        }
    }

    Some(MemorySnapshot {
        locals,
        globals,
        scope: snapshot_scope,
    })
}

/// Evaluate an expression in the context of a stack frame.
///
/// The `context` parameter determines the evaluation mode:
/// - `"watch"` - Standard watch expression evaluation (default for most languages)
/// - `"repl"` - REPL context, enables function calls (used by Go/Delve, in Go, only works in breakpoint mode and doesnt support variadic args functions)
/// - `"hover"` - Hover context for quick evaluation
///
/// Returns `Ok(value)` on success, `Err(error_message)` on failure.
/// The error message contains the actual debugger error for user feedback.
pub async fn evaluate_expression(
    broker: &Arc<DapBroker>,
    expression: &str,
    frame_id: i64,
    context: &str,
) -> Result<String, String> {
    let args = serde_json::json!({
        "expression": expression,
        "frameId": frame_id,
        "context": context
    });

    debug!(
        "Evaluating expression: '{}' in context '{}' (frame_id={})",
        expression, context, frame_id
    );

    let response = match broker.send_request("evaluate", Some(args)).await {
        Ok(r) => r,
        Err(e) => {
            let err = format!("DAP request failed: {}", e);
            warn!("Evaluate request failed for '{}': {}", expression, err);
            return Err(err);
        }
    };

    if !response.success {
        // Extract detailed error message from response body if available
        let error_detail = response
            .body
            .as_ref()
            .and_then(|b| b.get("error"))
            .and_then(|e| e.get("format"))
            .and_then(|f| f.as_str())
            .map(String::from)
            .or_else(|| response.message.clone())
            .unwrap_or_else(|| "Unknown error".to_string());

        warn!(
            "Evaluate request unsuccessful for '{}': {}",
            expression, error_detail
        );
        return Err(error_detail);
    }

    let body = response.body.ok_or_else(|| {
        let err = "Response missing body".to_string();
        warn!("Evaluate response missing body for '{}'", expression);
        err
    })?;

    debug!("Evaluate response body: {:?}", body);

    // Try to get result as string, or convert from other types
    body.get("result")
        .map(|r| {
            if let Some(s) = r.as_str() {
                s.to_string()
            } else {
                // For non-string results, convert to string representation
                r.to_string()
            }
        })
        .ok_or_else(|| {
            let err = "Response missing 'result' field".to_string();
            warn!(
                "Evaluate response missing 'result' field for '{}': body={:?}",
                expression, body
            );
            err
        })
}

/// Find metrics at a stopped location, preferring exact line matches.
///
/// Strategy (prevents cross-evaluation between nearby breakpoints):
/// 1. Find all metrics matching the file with exact line match (distance 0)
/// 2. If exact matches found, return only those
/// 3. If no exact match, fall back to the closest metric(s) within ±DAP_LINE_TOLERANCE
///    (handles compiler-adjusted breakpoint lines in Go/Rust)
pub async fn find_metrics_at_location(
    active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    file: &str,
    line: u32,
) -> Vec<Metric> {
    let metrics = active_metrics.read().await;
    let mut exact_matches = Vec::new();
    let mut closest_distance = u32::MAX;
    let mut tolerance_matches = Vec::new();

    // Get filename for flexible matching
    let filename = std::path::Path::new(file)
        .file_name()
        .and_then(|n| n.to_str());

    trace!(
        "find_metrics_at_location: searching for file='{}' (filename={:?}) line={} in {} active metrics",
        file,
        filename,
        line,
        metrics.len()
    );

    for metric in metrics.values() {
        let metric_filename = std::path::Path::new(&metric.location.file)
            .file_name()
            .and_then(|n| n.to_str());

        // Match by full path or just filename
        let file_matches =
            metric.location.file == file || (filename.is_some() && metric_filename == filename);

        // Allow some tolerance in line matching for compiled languages
        // (LLDB/Delve may resolve breakpoints to nearby lines)
        if !file_matches {
            continue;
        }

        let metric_line = metric.location.line;
        let distance = line.abs_diff(metric_line);

        trace!(
            "  checking metric '{}' at {}:{}: distance={} (stopped at line {})",
            metric.name,
            metric.location.file,
            metric.location.line,
            distance,
            line
        );

        if distance == 0 {
            exact_matches.push(metric.clone());
        } else if distance <= DAP_LINE_TOLERANCE {
            if distance < closest_distance {
                closest_distance = distance;
                tolerance_matches.clear();
                tolerance_matches.push(metric.clone());
            } else if distance == closest_distance {
                tolerance_matches.push(metric.clone());
            }
        }
    }

    // Prefer exact matches; only fall back to closest tolerance matches
    let result = if !exact_matches.is_empty() {
        trace!(
            "find_metrics_at_location: {} exact match(es) at {}:{}",
            exact_matches.len(),
            file,
            line,
        );
        exact_matches
    } else if !tolerance_matches.is_empty() {
        trace!(
            "find_metrics_at_location: no exact match, using {} closest metric(s) at distance {} from {}:{}",
            tolerance_matches.len(),
            closest_distance,
            file,
            line,
        );
        tolerance_matches
    } else {
        trace!(
            "find_metrics_at_location: no metrics found near {}:{} (tolerance={})",
            file,
            line,
            DAP_LINE_TOLERANCE
        );
        Vec::new()
    };

    result
}
