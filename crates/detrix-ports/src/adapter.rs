//! Debug Adapter Protocol (DAP) Adapter Interface
//!
//! Defines the trait that all DAP adapters must implement.
//! This enables swapping implementations (Python, Go, Rust, Node.js)
//! and testing with mocks.
//!
//! Per Clean Architecture, this output port belongs in the Application layer.
//! Infrastructure adapters (detrix-dap) implement this trait.

use async_trait::async_trait;
use detrix_core::{Metric, MetricEvent, MetricId, Result};
use futures::{stream, StreamExt};
use std::sync::Arc;

/// Thread-safe reference to a DAP adapter
pub type DapAdapterRef = Arc<dyn DapAdapter + Send + Sync>;

/// Thread-safe reference to a DAP adapter factory
pub type DapAdapterFactoryRef = Arc<dyn DapAdapterFactory + Send + Sync>;

/// Result of setting a metric as a logpoint
#[derive(Debug, Clone)]
pub struct SetMetricResult {
    /// Whether the breakpoint was verified by the debugger
    pub verified: bool,
    /// The actual line where the breakpoint was set (may differ from requested)
    pub line: u32,
    /// Optional message from the debugger
    pub message: Option<String>,
}

/// Result of removing a metric (logpoint)
#[derive(Debug, Clone, Default)]
pub struct RemoveMetricResult {
    /// Whether DAP confirmed the removal was successful
    pub confirmed: bool,
    /// Optional message from the debugger
    pub message: Option<String>,
}

impl RemoveMetricResult {
    /// Create a successful removal result
    pub fn success() -> Self {
        Self {
            confirmed: true,
            message: None,
        }
    }

    /// Create a removal result with confirmation and message
    pub fn new(confirmed: bool, message: Option<String>) -> Self {
        Self { confirmed, message }
    }
}

/// Result of toggling a metric's enabled state
///
/// Provides visibility into both storage and DAP operations
#[derive(Debug, Clone)]
pub struct ToggleMetricResult {
    /// Storage was successfully updated
    pub storage_updated: bool,
    /// DAP confirmed the operation (breakpoint set/removed)
    pub dap_confirmed: bool,
    /// Actual line where breakpoint was set (only for enable, may differ from requested)
    pub actual_line: Option<u32>,
    /// Message from DAP (e.g., verification status, errors)
    pub dap_message: Option<String>,
}

impl ToggleMetricResult {
    /// Create result for a successful enable operation
    pub fn enabled(set_result: &SetMetricResult) -> Self {
        Self {
            storage_updated: true,
            dap_confirmed: set_result.verified,
            actual_line: Some(set_result.line),
            dap_message: set_result.message.clone(),
        }
    }

    /// Create result for a successful disable operation
    pub fn disabled(remove_result: &RemoveMetricResult) -> Self {
        Self {
            storage_updated: true,
            dap_confirmed: remove_result.confirmed,
            actual_line: None,
            dap_message: remove_result.message.clone(),
        }
    }

    /// Create result when no change was needed (already in desired state)
    pub fn no_change() -> Self {
        Self {
            storage_updated: false,
            dap_confirmed: true,
            actual_line: None,
            dap_message: Some("No change needed".to_string()),
        }
    }
}

/// Information about a failed metric operation.
/// Uses metric name for identification since ID may not be available for unsaved metrics.
#[derive(Debug, Clone)]
pub struct FailedMetric {
    /// Metric name (always available)
    pub name: String,
    /// Metric ID if available (None for unsaved metrics)
    pub id: Option<MetricId>,
    /// Error message describing the failure
    pub error: String,
}

impl FailedMetric {
    /// Create a new FailedMetric from a metric and error
    pub fn from_metric(metric: &Metric, error: impl Into<String>) -> Self {
        Self {
            name: metric.name.clone(),
            id: metric.id,
            error: error.into(),
        }
    }
}

/// Result of a batch set operation
#[derive(Debug, Clone, Default)]
pub struct BatchSetResult {
    /// Metrics that were successfully set
    pub succeeded: Vec<SetMetricResult>,
    /// Metrics that failed with their error messages
    pub failed: Vec<FailedMetric>,
}

impl BatchSetResult {
    /// Create a new empty batch result
    pub fn new() -> Self {
        Self::default()
    }

    /// Count of successful operations
    pub fn success_count(&self) -> usize {
        self.succeeded.len()
    }

    /// Count of failed operations
    pub fn failure_count(&self) -> usize {
        self.failed.len()
    }

    /// Check if all operations succeeded
    pub fn all_succeeded(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Result of a batch remove operation
#[derive(Debug, Clone, Default)]
pub struct BatchRemoveResult {
    /// Count of metrics successfully removed
    pub succeeded: usize,
    /// Metrics that failed with their error messages
    pub failed: Vec<FailedMetric>,
}

impl BatchRemoveResult {
    /// Create a new empty batch result
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if all operations succeeded
    pub fn all_succeeded(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Trait for Debug Adapter Protocol (DAP) adapters
///
/// This trait abstracts the interaction with language-specific debuggers
/// (debugpy for Python, delve for Go, lldb-dap for Rust)
#[async_trait]
pub trait DapAdapter: Send + Sync {
    /// Start the debug adapter and connect to the target process
    async fn start(&self) -> Result<()>;

    /// Stop the debug adapter
    async fn stop(&self) -> Result<()>;

    /// Ensure the adapter is connected, starting if necessary
    ///
    /// This method handles connection lifecycle:
    /// - If already connected and healthy, returns Ok immediately
    /// - If disconnected or unhealthy, attempts to (re)connect
    /// - Returns error if connection cannot be established
    ///
    /// Use this before operations that require an active connection.
    async fn ensure_connected(&self) -> Result<()>;

    /// Check if the adapter is currently connected and healthy
    ///
    /// Returns true if the adapter is ready to handle requests.
    /// This is a non-blocking check that doesn't attempt reconnection.
    fn is_connected(&self) -> bool;

    /// Set a metric as a logpoint in the target process
    ///
    /// Returns information about whether the breakpoint was verified
    /// and the actual line where it was set.
    async fn set_metric(&self, metric: &Metric) -> Result<SetMetricResult>;

    /// Remove a metric (logpoint) from the target process
    ///
    /// This will remove the breakpoint for this specific metric.
    /// Returns confirmation from DAP about the removal.
    async fn remove_metric(&self, metric: &Metric) -> Result<RemoveMetricResult>;

    /// Subscribe to metric events from the debug adapter
    ///
    /// Returns a bounded receiver that will receive MetricEvents as they are captured
    /// by the debugger. The channel has bounded capacity for backpressure - if the
    /// consumer is slow, events will be dropped with a warning to prevent memory growth.
    async fn subscribe_events(&self) -> Result<tokio::sync::mpsc::Receiver<MetricEvent>>;

    /// Continue/resume program execution
    ///
    /// For debuggers that start paused (like Delve in headless mode), this
    /// method resumes execution. Should be called after setting logpoints.
    ///
    /// Returns Ok(true) if execution was resumed, Ok(false) if already running.
    async fn continue_execution(&self) -> Result<bool> {
        // Default implementation: do nothing (for debuggers that auto-continue)
        Ok(true)
    }

    /// Set multiple metrics as logpoints in a batch operation.
    ///
    /// This is an optimization for bulk operations:
    /// - When `metrics.len() < threshold`: executes sequentially (low overhead)
    /// - When `metrics.len() >= threshold`: executes concurrently with `concurrency` limit
    ///
    /// Adapters may override this to use more efficient DAP batching
    /// (multiple breakpoints per setBreakpoints request).
    ///
    /// # Arguments
    /// * `metrics` - Metrics to set
    /// * `threshold` - Minimum count to trigger concurrent execution (default: 5)
    /// * `concurrency` - Maximum concurrent operations (default: 4)
    ///
    /// Returns `BatchSetResult` containing successful and failed operations.
    async fn set_metrics_batch(
        &self,
        metrics: &[Metric],
        threshold: usize,
        concurrency: usize,
    ) -> BatchSetResult {
        if metrics.len() < threshold {
            // Sequential execution for small batches (less overhead)
            let mut result = BatchSetResult::new();
            for metric in metrics {
                match self.set_metric(metric).await {
                    Ok(set_result) => result.succeeded.push(set_result),
                    Err(e) => {
                        result
                            .failed
                            .push(FailedMetric::from_metric(metric, e.to_string()));
                    }
                }
            }
            result
        } else {
            // Concurrent execution for larger batches
            let results: Vec<_> = stream::iter(metrics.iter().cloned())
                .map(|metric| async move {
                    match self.set_metric(&metric).await {
                        Ok(set_result) => Ok(set_result),
                        Err(e) => Err(FailedMetric::from_metric(&metric, e.to_string())),
                    }
                })
                .buffer_unordered(concurrency)
                .collect()
                .await;

            let mut batch_result = BatchSetResult::new();
            for result in results {
                match result {
                    Ok(set_result) => batch_result.succeeded.push(set_result),
                    Err(failed) => batch_result.failed.push(failed),
                }
            }
            batch_result
        }
    }

    /// Remove multiple metrics in a batch operation.
    ///
    /// This is an optimization for bulk operations:
    /// - When `metrics.len() < threshold`: executes sequentially (low overhead)
    /// - When `metrics.len() >= threshold`: executes concurrently with `concurrency` limit
    ///
    /// Adapters may override this to use more efficient DAP batching.
    ///
    /// # Arguments
    /// * `metrics` - Metrics to remove
    /// * `threshold` - Minimum count to trigger concurrent execution (default: 5)
    /// * `concurrency` - Maximum concurrent operations (default: 4)
    ///
    /// Returns `BatchRemoveResult` containing successful and failed operations.
    async fn remove_metrics_batch(
        &self,
        metrics: &[Metric],
        threshold: usize,
        concurrency: usize,
    ) -> BatchRemoveResult {
        if metrics.len() < threshold {
            // Sequential execution for small batches (less overhead)
            let mut result = BatchRemoveResult::new();
            for metric in metrics {
                match self.remove_metric(metric).await {
                    Ok(_) => result.succeeded += 1,
                    Err(e) => {
                        result
                            .failed
                            .push(FailedMetric::from_metric(metric, e.to_string()));
                    }
                }
            }
            result
        } else {
            // Concurrent execution for larger batches
            let results: Vec<_> = stream::iter(metrics.iter().cloned())
                .map(|metric| async move {
                    match self.remove_metric(&metric).await {
                        Ok(_) => Ok(()),
                        Err(e) => Err(FailedMetric::from_metric(&metric, e.to_string())),
                    }
                })
                .buffer_unordered(concurrency)
                .collect()
                .await;

            let mut batch_result = BatchRemoveResult::new();
            for result in results {
                match result {
                    Ok(()) => batch_result.succeeded += 1,
                    Err(failed) => batch_result.failed.push(failed),
                }
            }
            batch_result
        }
    }

    /// Check if this adapter supports native batch operations.
    ///
    /// Adapters that can efficiently process multiple breakpoints in a single
    /// DAP request should return `true` and override `set_metrics_batch`.
    fn supports_batch_operations(&self) -> bool {
        false
    }
}

// ============================================================================
// Adapter Factory
// ============================================================================

/// Factory trait for creating DAP adapters
///
/// This trait abstracts the creation of language-specific DAP adapters.
/// It follows the Factory pattern to decouple adapter creation from usage.
///
/// Implementations are provided in the `detrix-dap` infrastructure crate.
#[async_trait]
pub trait DapAdapterFactory: Send + Sync {
    /// Create a Python DAP adapter (debugpy)
    ///
    /// # Arguments
    /// * `host` - Host address (e.g., "127.0.0.1", "localhost")
    /// * `port` - Port number where debugpy is listening
    ///
    /// # Returns
    /// A configured Python adapter ready to connect
    async fn create_python_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef>;

    /// Create a Go DAP adapter (Delve)
    ///
    /// # Arguments
    /// * `host` - Host address (e.g., "127.0.0.1", "localhost")
    /// * `port` - Port number where Delve is listening
    ///
    /// # Returns
    /// A configured Go adapter ready to connect
    async fn create_go_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef>;

    /// Create a Rust DAP adapter (CodeLLDB / lldb-dap)
    ///
    /// # Arguments
    /// * `host` - Host address (e.g., "127.0.0.1", "localhost")
    /// * `port` - Port number where lldb-dap is listening
    /// * `program` - Optional program path for direct launch mode
    ///
    /// # Returns
    /// A configured Rust adapter ready to connect
    async fn create_rust_adapter(
        &self,
        host: &str,
        port: u16,
        program: Option<&str>,
    ) -> Result<DapAdapterRef>;
}
