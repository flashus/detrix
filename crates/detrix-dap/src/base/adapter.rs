//! BaseAdapter - Generic adapter with shared logic
//!
//! This module contains the core BaseAdapter struct and its lifecycle management
//! methods (start, stop, ensure_connected).

use super::events::handle_events;
use super::metrics::MetricManager;
use super::traits::OutputParser;
use crate::{constants::requests, AdapterConfig, AdapterProcess, AdapterState, Error, Result};
use async_trait::async_trait;
use detrix_application::{DapAdapter, RemoveMetricResult, SetMetricResult};
use detrix_config::AdapterConnectionConfig;
use detrix_core::{Metric, MetricEvent};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, trace, warn};

// ============================================================================
// BaseAdapter - Generic adapter with shared logic
// ============================================================================

/// Generic base adapter that handles common DAP operations.
///
/// Language-specific behavior is provided via the `OutputParser` type parameter.
///
/// # Type Parameters
/// * `P` - The output parser implementation for the specific language
pub struct BaseAdapter<P: OutputParser> {
    /// Underlying adapter process managing DAP communication
    pub(crate) adapter: Arc<AdapterProcess>,

    /// Active metrics mapped by location key (file:line)
    pub(crate) active_metrics: Arc<RwLock<HashMap<String, Metric>>>,

    /// Base path for resolving relative file paths
    pub(crate) base_path: PathBuf,

    /// JoinHandles for spawned event parsing tasks (for cleanup on stop)
    pub(crate) event_tasks: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,

    /// Per-file locks to serialize setBreakpoints calls.
    /// DAP's setBreakpoints replaces ALL breakpoints in a file, so concurrent calls
    /// for the same file can race and drop breakpoints. This lock serializes them.
    pub(crate) file_locks: Arc<RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,

    /// Phantom data to track the parser type
    pub(crate) _parser: PhantomData<P>,
}

impl<P: OutputParser> std::fmt::Debug for BaseAdapter<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BaseAdapter")
            .field("language", &P::language_name())
            .field("adapter", &self.adapter)
            .field("base_path", &self.base_path)
            .finish()
    }
}

impl<P: OutputParser> BaseAdapter<P> {
    /// Create a new base adapter with connection config.
    pub fn new_with_config(
        config: AdapterConfig,
        base_path: impl Into<PathBuf>,
        connection_config: AdapterConnectionConfig,
    ) -> Self {
        Self {
            adapter: Arc::new(AdapterProcess::new_with_config(config, connection_config)),
            active_metrics: Arc::new(RwLock::new(HashMap::new())),
            base_path: base_path.into(),
            event_tasks: Arc::new(RwLock::new(Vec::new())),
            file_locks: Arc::new(RwLock::new(HashMap::new())),
            _parser: PhantomData,
        }
    }

    /// Create a new base adapter with default connection config.
    pub fn new(config: AdapterConfig, base_path: impl Into<PathBuf>) -> Self {
        Self::new_with_config(config, base_path, AdapterConnectionConfig::default())
    }

    /// Get the underlying adapter process.
    pub fn adapter(&self) -> &AdapterProcess {
        &self.adapter
    }

    /// Get active metrics (for testing/inspection).
    pub fn active_metrics(&self) -> &Arc<RwLock<HashMap<String, Metric>>> {
        &self.active_metrics
    }

    /// Get the base path.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Check if adapter is ready (non-blocking).
    pub fn is_ready(&self) -> bool {
        self.adapter.is_ready()
    }

    // ========================================================================
    // Connection Management
    // ========================================================================

    /// Start the adapter (connects to the debugger).
    pub async fn start(&self) -> Result<()> {
        self.ensure_connected().await
    }

    /// Stop the adapter and cleanup all resources.
    ///
    /// IMPORTANT: This method waits for aborted event tasks to fully complete.
    /// This ensures their receivers are dropped, which allows the broker to
    /// clean up stale subscribers. Without this wait, rapid stop/start cycles
    /// can result in multiple subscribers receiving the same events (triple events bug).
    pub async fn stop(&self) -> Result<()> {
        let lang = P::language_name();

        // Abort all event parsing tasks and WAIT for them to complete
        // This is critical to prevent triple events - we must ensure receivers
        // are dropped before any new subscription can be created
        let tasks: Vec<_> = {
            let mut tasks_guard = self.event_tasks.write().await;
            tasks_guard.drain(..).collect()
        };

        if !tasks.is_empty() {
            debug!("[{}] Stopping {} event handler task(s)", lang, tasks.len());
            for mut handle in tasks {
                handle.abort();
                // Wait for task to finish with timeout
                match tokio::time::timeout(std::time::Duration::from_millis(200), &mut handle).await
                {
                    Ok(_) => {
                        trace!("[{}] Event handler task stopped", lang);
                    }
                    Err(_) => {
                        // Timeout - task will be cleaned up eventually
                        debug!(
                            "[{}] Event handler task stop timeout, continuing anyway",
                            lang
                        );
                    }
                }
            }
        }

        // Clear active metrics
        self.active_metrics.write().await.clear();

        // Clear file locks
        self.file_locks.write().await.clear();

        // Stop the adapter process
        self.adapter.stop().await
    }

    /// Ensure the adapter is connected, handling state transitions.
    ///
    /// Uses configurable timeouts from AdapterConnectionConfig:
    /// - `connection_timeout_ms`: Maximum time to wait for adapter to reach Ready state
    /// - `shutdown_timeout_ms`: Maximum time to wait for adapter shutdown
    /// - `retry_interval_ms`: Interval between state checks
    #[tracing::instrument(skip(self), fields(lang = P::language_name()))]
    pub async fn ensure_connected(&self) -> Result<()> {
        use tokio::time::{sleep, Duration, Instant};

        let config = self.adapter.connection_config();
        let connection_timeout = Duration::from_millis(config.connection_timeout_ms);
        let shutdown_timeout = Duration::from_millis(config.shutdown_timeout_ms);
        let retry_interval = Duration::from_millis(config.retry_interval_ms.max(100)); // Min 100ms

        let state = self.adapter.state().await;
        let lang = P::language_name();
        info!(
            "[{}] ensure_connected: current state is {:?}, connection_timeout={}ms",
            lang, state, config.connection_timeout_ms
        );

        match state {
            AdapterState::Ready => {
                info!(
                    "[{}] Adapter in Ready state, verifying with health check...",
                    lang
                );
                if self.adapter.health_check().await.is_ok() {
                    info!("[{}] Health check passed, adapter is ready", lang);
                    return Ok(());
                }
                info!("[{}] Health check failed, restarting...", lang);
                self.adapter.stop().await?;
            }
            AdapterState::Starting => {
                info!(
                    "[{}] Adapter already starting, waiting for Ready state (timeout: {}ms)...",
                    lang, config.connection_timeout_ms
                );
                let start = Instant::now();
                let mut iteration = 0u32;
                while start.elapsed() < connection_timeout {
                    sleep(retry_interval).await;
                    let current_state = self.adapter.state().await;
                    info!(
                        "[{}] Waiting for Ready, iteration {}, elapsed: {}ms, state: {:?}",
                        lang,
                        iteration,
                        start.elapsed().as_millis(),
                        current_state
                    );
                    if matches!(current_state, AdapterState::Ready) {
                        info!("[{}] Adapter reached Ready state!", lang);
                        return Ok(());
                    }
                    if matches!(current_state, AdapterState::Failed | AdapterState::Stopped) {
                        info!("[{}] State is {:?}, will try to start", lang, current_state);
                        break;
                    }
                    iteration += 1;
                }
                if start.elapsed() >= connection_timeout {
                    return Err(Error::Protocol(format!(
                        "Timeout ({}ms) waiting for adapter to reach Ready state",
                        config.connection_timeout_ms
                    )));
                }
            }
            AdapterState::ShuttingDown => {
                debug!(
                    "[{}] Adapter shutting down, waiting (timeout: {}ms)...",
                    lang, config.shutdown_timeout_ms
                );
                let start = Instant::now();
                while start.elapsed() < shutdown_timeout {
                    sleep(retry_interval).await;
                    if matches!(self.adapter.state().await, AdapterState::Stopped) {
                        break;
                    }
                }
            }
            AdapterState::Failed => {
                debug!(
                    "[{}] Adapter in Failed state, stopping before restart...",
                    lang
                );
                if let Err(e) = self.adapter.stop().await {
                    debug!(
                        "[{}] Failed to stop (will attempt restart anyway): {}",
                        lang, e
                    );
                }
            }
            AdapterState::Stopped => {
                info!("[{}] Adapter in Stopped state, will start it", lang);
            }
        }

        info!("[{}] Starting adapter connection...", lang);
        self.adapter.start().await?;
        info!("[{}] Adapter connected successfully", lang);
        Ok(())
    }

    // ========================================================================
    // Event Subscription
    // ========================================================================

    /// Subscribe to metric events from the debugger output.
    ///
    /// Handles two types of events:
    /// 1. "output" events - for logpoints (metrics without introspection)
    /// 2. "stopped" events - for breakpoints (metrics with introspection enabled)
    ///
    /// When a stopped event is received for a metric with introspection:
    /// - Captures stack trace via DAP stackTrace request
    /// - Captures variables via DAP scopes/variables requests
    /// - Sends continue to resume execution
    ///
    /// NOTE: This method cleans up finished event handler tasks. If there are
    /// still active handlers (unexpected), it aborts them and WAITS for them
    /// to finish to ensure their receivers are dropped before creating new
    /// subscriptions. This prevents duplicate events from multiple subscribers.
    pub async fn subscribe_events(&self) -> Result<tokio::sync::mpsc::Receiver<MetricEvent>> {
        let lang = P::language_name();

        // Clean up event handler tasks to prevent duplicate events
        // IMPORTANT: We must wait for aborted tasks to actually finish so their
        // receivers are dropped and the broker's sender cleanup works correctly.
        {
            let mut tasks = self.event_tasks.write().await;
            if !tasks.is_empty() {
                // Separate finished tasks from active ones
                let (finished, mut active): (Vec<_>, Vec<_>) =
                    tasks.drain(..).partition(|h| h.is_finished());

                if !finished.is_empty() {
                    debug!(
                        "[{}] Cleaned up {} finished event handler task(s)",
                        lang,
                        finished.len()
                    );
                }

                if !active.is_empty() {
                    // Active handlers shouldn't exist on reconnect (they should have
                    // exited when the old connection died). This indicates a bug or
                    // accidental double-call. Abort and WAIT to prevent duplicate events.
                    warn!(
                        "[{}] Found {} unexpectedly active event handler task(s) - aborting and waiting",
                        lang,
                        active.len()
                    );
                    for handle in &mut active {
                        handle.abort();
                    }
                    // Wait for all aborted tasks to actually finish (with timeout)
                    // This ensures their receivers are dropped before we create new ones
                    for handle in active {
                        match tokio::time::timeout(std::time::Duration::from_millis(100), handle)
                            .await
                        {
                            Ok(_) => {
                                debug!("[{}] Aborted task finished", lang);
                            }
                            Err(_) => {
                                // Task didn't finish in time, but abort was requested
                                // It will be cleaned up eventually
                                debug!("[{}] Aborted task timeout, continuing anyway", lang);
                            }
                        }
                    }
                }
            }
        }

        let broker = self.adapter.broker().await?;
        let event_rx = broker.subscribe_events().await;

        // Use bounded channel with configurable capacity for backpressure
        let capacity = self.adapter.connection_config().event_channel_capacity;
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        let active_metrics = Arc::clone(&self.active_metrics);
        let adapter = Arc::clone(&self.adapter);

        let handle = tokio::spawn(handle_events::<P>(
            event_rx,
            active_metrics,
            adapter,
            tx,
            lang,
        ));

        self.event_tasks.write().await.push(handle);
        Ok(rx)
    }

    /// Continue/resume program execution (for debuggers that start paused).
    pub async fn continue_execution(&self) -> Result<bool> {
        if !P::needs_continue_after_connect() {
            return Ok(true);
        }

        let broker = self.adapter.broker().await?;
        let lang = P::language_name();

        debug!("[{}] Sending continue request to resume execution", lang);
        let continue_args = serde_json::json!({
            "threadId": 1  // Use main thread
        });

        let response = broker
            .send_request(requests::CONTINUE, Some(continue_args))
            .await?;

        if !response.success {
            debug!(
                "[{}] Continue request failed (may already be running): {:?}",
                lang, response.message
            );
            Ok(false)
        } else {
            info!("[{}] Program execution resumed", lang);
            Ok(true)
        }
    }

    // ========================================================================
    // Helper Methods
    // ========================================================================

    /// Resolve a file path to an absolute path.
    pub(crate) fn resolve_path(&self, file: &str) -> PathBuf {
        let path = PathBuf::from(file);
        if path.is_absolute() {
            path
        } else {
            self.base_path.join(file)
        }
    }

    /// Create a location key for metrics tracking.
    pub fn location_key(file: &str, line: u32) -> String {
        format!("{}#{}", file, line)
    }

    /// Parse the setBreakpoints response body to extract breakpoint info.
    pub(crate) fn parse_breakpoint_response(
        body: &Option<serde_json::Value>,
        default_line: u32,
    ) -> (bool, u32, Option<String>) {
        if let Some(body) = body {
            let breakpoints = body.get("breakpoints").and_then(|b| b.as_array());
            if let Some(bps) = breakpoints {
                if let Some(bp) = bps.first() {
                    let verified = bp
                        .get("verified")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let actual_line =
                        bp.get("line")
                            .and_then(|l| l.as_u64())
                            .unwrap_or(default_line as u64) as u32;
                    let message = bp
                        .get("message")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string());
                    return (verified, actual_line, message);
                }
            }
        }
        (false, default_line, None)
    }
}

// Delegate metric management to MetricManager trait
impl<P: OutputParser> MetricManager for BaseAdapter<P> {
    fn adapter(&self) -> &Arc<AdapterProcess> {
        &self.adapter
    }

    fn active_metrics(&self) -> &Arc<RwLock<HashMap<String, Metric>>> {
        &self.active_metrics
    }

    fn file_locks(&self) -> &Arc<RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> {
        &self.file_locks
    }

    fn resolve_path(&self, file: &str) -> PathBuf {
        BaseAdapter::<P>::resolve_path(self, file)
    }

    fn build_logpoint_message(metric: &Metric) -> String {
        P::build_logpoint_message(metric)
    }

    fn supports_logpoint_introspection() -> bool {
        P::supports_logpoint_introspection()
    }

    fn language_name() -> &'static str {
        P::language_name()
    }

    fn parse_breakpoint_response(
        body: &Option<serde_json::Value>,
        default_line: u32,
    ) -> (bool, u32, Option<String>) {
        BaseAdapter::<P>::parse_breakpoint_response(body, default_line)
    }
}

// ============================================================================
// DapAdapter Trait Implementation
// ============================================================================

#[async_trait]
impl<P: OutputParser> DapAdapter for BaseAdapter<P> {
    async fn start(&self) -> detrix_core::Result<()> {
        Ok(self.start().await?)
    }

    async fn stop(&self) -> detrix_core::Result<()> {
        Ok(self.stop().await?)
    }

    async fn ensure_connected(&self) -> detrix_core::Result<()> {
        Ok(self.ensure_connected().await?)
    }

    fn is_connected(&self) -> bool {
        self.is_ready()
    }

    async fn set_metric(&self, metric: &Metric) -> detrix_core::Result<SetMetricResult> {
        Ok(<Self as MetricManager>::set_metric(self, metric).await?)
    }

    async fn remove_metric(&self, metric: &Metric) -> detrix_core::Result<RemoveMetricResult> {
        Ok(<Self as MetricManager>::remove_metric(self, metric).await?)
    }

    async fn subscribe_events(
        &self,
    ) -> detrix_core::Result<tokio::sync::mpsc::Receiver<MetricEvent>> {
        Ok(self.subscribe_events().await?)
    }

    async fn continue_execution(&self) -> detrix_core::Result<bool> {
        Ok(self.continue_execution().await?)
    }
}
