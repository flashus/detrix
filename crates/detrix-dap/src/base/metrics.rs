//! Metric Management
//!
//! This module contains the metric management functionality for setting,
//! removing, and clearing metrics as logpoints/breakpoints.

use crate::{
    constants::requests, AdapterProcess, Error, Result, SetBreakpointsArguments, Source,
    SourceBreakpoint,
};
use detrix_application::{RemoveMetricResult, SetMetricResult};
use detrix_core::{expression_contains_function_call, Metric, SourceLanguage};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// ============================================================================
// MetricManager Trait
// ============================================================================

/// Trait for metric management operations.
///
/// This trait defines the interface for setting, removing, and clearing metrics.
/// It is implemented by BaseAdapter to provide metric management functionality.
#[allow(async_fn_in_trait)]
pub trait MetricManager: Send + Sync {
    /// Get the underlying adapter process.
    fn adapter(&self) -> &Arc<AdapterProcess>;

    /// Get active metrics map.
    fn active_metrics(&self) -> &Arc<RwLock<HashMap<String, Metric>>>;

    /// Get file locks map.
    #[allow(clippy::type_complexity)]
    fn file_locks(&self) -> &Arc<RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;

    /// Resolve a file path to an absolute path.
    fn resolve_path(&self, file: &str) -> PathBuf;

    /// Build a logpoint message for the debugger.
    fn build_logpoint_message(metric: &Metric) -> String;

    /// Whether this adapter supports logpoint introspection.
    fn supports_logpoint_introspection() -> bool;

    /// Get the language name for logging.
    fn language_name() -> &'static str;

    /// Parse the setBreakpoints response body.
    fn parse_breakpoint_response(
        body: &Option<serde_json::Value>,
        default_line: u32,
    ) -> (bool, u32, Option<String>);

    /// Get or create a lock for a specific file path.
    /// This serializes setBreakpoints calls for the same file to prevent race conditions.
    async fn get_file_lock(&self, file_path: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.file_locks().write().await;
        locks
            .entry(file_path.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Set a metric as a logpoint in the target process.
    ///
    /// IMPORTANT: DAP `setBreakpoints` command replaces ALL breakpoints in a source file.
    /// To avoid race conditions when adding multiple metrics to the same file, we:
    /// 1. Acquire per-file lock to serialize setBreakpoints calls
    /// 2. Add the new metric to our tracking map
    /// 3. Collect ALL breakpoints for the file (including existing ones)
    /// 4. Send a single `setBreakpoints` with ALL breakpoints
    /// 5. Release lock
    async fn set_metric(&self, metric: &Metric) -> Result<SetMetricResult> {
        let file_path = &metric.location.file;
        let line = metric.location.line;
        let lang = Self::language_name();

        info!(
            "[{}] Setting metric '{}' at {}:{} (introspection: stack={}, memory={})",
            lang,
            metric.name,
            file_path,
            line,
            metric.capture_stack_trace,
            metric.capture_memory_snapshot
        );

        // Acquire per-file lock to prevent race conditions with concurrent set/remove calls
        // for the same file. DAP setBreakpoints replaces ALL breakpoints, so we must serialize.
        let file_lock = self.get_file_lock(file_path).await;
        let _guard = file_lock.lock().await;

        // Store the new metric FIRST (before sending to DAP)
        // Key by metric name to allow multiple metrics on the same line
        self.active_metrics()
            .write()
            .await
            .insert(metric.name.clone(), metric.clone());

        // Collect ALL breakpoints for this file (including the new one we just added)
        // This is critical because DAP `setBreakpoints` REPLACES all breakpoints in a file
        let active = self.active_metrics().read().await;
        let all_breakpoints: Vec<SourceBreakpoint> = active
            .values()
            .filter(|m| m.location.file == *file_path)
            .map(|m| {
                // Determine if we need breakpoint mode (pauses execution):
                // 1. Introspection (stack trace or memory snapshot) - needs pause to capture context
                // 2. Go function calls - Delve requires "call" prefix which only works via evaluate
                //
                // WARNING: Go function calls BLOCK the target process while executing!
                // Use simple variable expressions when possible for non-blocking observability.
                let needs_introspection = m.capture_stack_trace || m.capture_memory_snapshot;
                let is_go_function_call = m.language == SourceLanguage::Go
                    && expression_contains_function_call(&m.expression);

                let use_breakpoint = (needs_introspection && !Self::supports_logpoint_introspection())
                    || is_go_function_call;

                if use_breakpoint {
                    if is_go_function_call {
                        warn!(
                            "[{}] Using BLOCKING breakpoint for '{}' (Go function call: '{}') - consider using simple variables",
                            Self::language_name(), m.name, m.expression
                        );
                    } else {
                        debug!(
                            "[{}] Using breakpoint for '{}' (introspection enabled: stack={}, memory={})",
                            Self::language_name(), m.name, m.capture_stack_trace, m.capture_memory_snapshot
                        );
                    }
                    SourceBreakpoint::at_line(m.location.line)
                } else {
                    // Use logpoint - adapter handles introspection in logpoint message if needed
                    if needs_introspection {
                        debug!(
                            "[{}] Using logpoint with introspection for '{}' (stack={}, memory={})",
                            Self::language_name(), m.name, m.capture_stack_trace, m.capture_memory_snapshot
                        );
                    }
                    SourceBreakpoint::logpoint(m.location.line, Self::build_logpoint_message(m))
                }
            })
            .collect();
        drop(active);

        debug!(
            "[{}] Sending {} breakpoints for file {} (including new metric '{}')",
            lang,
            all_breakpoints.len(),
            file_path,
            metric.name
        );

        // Resolve file path
        let abs_path = self.resolve_path(file_path);
        info!(
            "[{}] Resolved file_path='{}' -> abs_path='{}'",
            lang,
            file_path,
            abs_path.display()
        );

        // Create DAP request with ALL breakpoints for this file
        let source = Source {
            path: Some(abs_path.to_string_lossy().to_string()),
            name: Some(file_path.to_string()),
            source_reference: None,
        };

        let args = SetBreakpointsArguments {
            source,
            breakpoints: Some(all_breakpoints),
            source_modified: Some(false),
        };

        // Send request
        let broker = self.adapter().broker().await?;
        let response = broker
            .send_request(requests::SET_BREAKPOINTS, Some(serde_json::to_value(args)?))
            .await?;

        if !response.success {
            // Remove the metric from tracking since DAP rejected it (keyed by name)
            self.active_metrics().write().await.remove(&metric.name);
            return Err(Error::Protocol(format!(
                "Failed to set breakpoint: {}",
                response
                    .message
                    .unwrap_or_else(|| "Unknown error".to_string())
            )));
        }

        // Parse response to find our specific breakpoint
        let (verified, actual_line, message) =
            Self::parse_breakpoint_response(&response.body, line);

        if verified {
            debug!(
                "[{}] Logpoint set for '{}' (verified, line={})",
                lang, metric.name, actual_line
            );
        } else {
            // Include the DAP error message if available (e.g., "breakpoint already exists")
            if let Some(ref msg) = message {
                warn!(
                    "[{}] Logpoint not verified for '{}' at {}:{} - {}",
                    lang, metric.name, file_path, line, msg
                );
            } else {
                warn!(
                    "[{}] Logpoint not verified for '{}' at {}:{}",
                    lang, metric.name, file_path, line
                );
            }
        }

        Ok(SetMetricResult {
            verified,
            line: actual_line,
            message,
        })
    }

    /// Remove a metric (logpoint) from the target process.
    ///
    /// Returns confirmation from DAP about whether the removal was successful.
    /// Uses per-file lock to prevent race conditions with concurrent set/remove calls.
    async fn remove_metric(&self, metric: &Metric) -> Result<RemoveMetricResult> {
        let file_path = &metric.location.file;
        let lang = Self::language_name();

        info!(
            "[{}] Removing metric '{}' at {}:{}",
            lang, metric.name, file_path, metric.location.line
        );

        // Acquire per-file lock to prevent race conditions with concurrent set/remove calls
        let file_lock = self.get_file_lock(file_path).await;
        let _guard = file_lock.lock().await;

        // Remove from tracking first (keyed by metric name)
        self.active_metrics().write().await.remove(&metric.name);

        // Get remaining breakpoints for this file
        let active = self.active_metrics().read().await;
        let remaining_breakpoints: Vec<SourceBreakpoint> = active
            .values()
            .filter(|m| m.location.file == *file_path)
            .map(|m| SourceBreakpoint::logpoint(m.location.line, Self::build_logpoint_message(m)))
            .collect();
        drop(active);

        // Send updated breakpoints (without the removed one)
        let abs_path = self.resolve_path(file_path);
        let source = Source {
            path: Some(abs_path.to_string_lossy().to_string()),
            name: Some(file_path.to_string()),
            source_reference: None,
        };

        let args = SetBreakpointsArguments {
            source,
            breakpoints: Some(remaining_breakpoints),
            source_modified: Some(false),
        };

        let broker = self.adapter().broker().await?;
        let response = broker
            .send_request(requests::SET_BREAKPOINTS, Some(serde_json::to_value(args)?))
            .await?;

        let confirmed = response.success;
        let message = response.message.clone();

        if confirmed {
            debug!(
                "[{}] Metric '{}' removed (DAP confirmed)",
                lang, metric.name
            );
        } else {
            warn!(
                "[{}] Metric '{}' removal not confirmed by DAP: {:?}",
                lang, metric.name, message
            );
        }

        Ok(RemoveMetricResult::new(confirmed, message))
    }

    /// Clear all metrics (remove all logpoints).
    #[allow(dead_code)]
    async fn clear_metrics(&self) -> Result<()> {
        let lang = Self::language_name();

        // Collect unique file paths under lock, then drop immediately
        let files: Vec<String> = {
            let active_metrics = self.active_metrics().read().await;
            active_metrics
                .values()
                .map(|m| m.location.file.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect()
        };

        let broker = self.adapter().broker().await?;

        // Clear breakpoints for each file (lock is not held during I/O)
        for file in files {
            let abs_path = self.resolve_path(&file);

            let source = Source {
                path: Some(abs_path.to_string_lossy().to_string()),
                name: Some(file.clone()),
                source_reference: None,
            };

            let args = SetBreakpointsArguments {
                source,
                breakpoints: Some(vec![]), // Empty = clear all
                source_modified: Some(false),
            };

            broker
                .send_request(requests::SET_BREAKPOINTS, Some(serde_json::to_value(args)?))
                .await?;
        }

        self.active_metrics().write().await.clear();

        info!("[{}] All metrics cleared", lang);
        Ok(())
    }
}
