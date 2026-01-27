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

/// Evaluate an expression in the context of a stack frame
pub async fn evaluate_expression(
    broker: &Arc<DapBroker>,
    expression: &str,
    frame_id: i64,
) -> Option<String> {
    let args = serde_json::json!({
        "expression": expression,
        "frameId": frame_id,
        "context": "watch"  // "watch" context for evaluating expressions
    });

    let response = broker
        .send_request("evaluate", Some(args))
        .await
        .debug_ok(&format!("Evaluate request failed for '{}'", expression))?;

    if !response.success {
        debug!("Evaluate request unsuccessful for '{}'", expression);
        return None;
    }

    response
        .body?
        .get("result")
        .and_then(|r| r.as_str())
        .map(String::from)
}

/// Find ALL metrics at a stopped location (there may be multiple metrics on the same line)
///
/// For compiled languages (Rust, Go), the debugger might resolve breakpoints to nearby lines
/// due to compiler optimizations. We use a tolerance of +/- 5 lines for matching.
pub async fn find_metrics_at_location(
    active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    file: &str,
    line: u32,
) -> Vec<Metric> {
    let metrics = active_metrics.read().await;
    let mut result = Vec::new();

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
        // Check if metric is at this location
        let metric_filename = std::path::Path::new(&metric.location.file)
            .file_name()
            .and_then(|n| n.to_str());

        // Match by full path or just filename
        let file_matches =
            metric.location.file == file || (filename.is_some() && metric_filename == filename);

        // Allow some tolerance in line matching for compiled languages
        // (LLDB/Delve may resolve breakpoints to nearby lines)
        let metric_line = metric.location.line;
        let line_matches = metric_line == line
            || (line > metric_line && line - metric_line <= DAP_LINE_TOLERANCE)
            || (metric_line > line && metric_line - line <= DAP_LINE_TOLERANCE);

        trace!(
            "  checking metric '{}' at {}:{} (metric_filename={:?}): file_matches={}, line_matches={} (stopped at line {})",
            metric.name,
            metric.location.file,
            metric.location.line,
            metric_filename,
            file_matches,
            line_matches,
            line
        );

        if file_matches && line_matches {
            result.push(metric.clone());
        }
    }

    trace!(
        "find_metrics_at_location: found {} metrics near {}:{} (tolerance={})",
        result.len(),
        file,
        line,
        DAP_LINE_TOLERANCE
    );

    result
}
