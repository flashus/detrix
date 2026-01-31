//! Mock DAP adapter implementations for testing
//!
//! - [`MockDapAdapter`] - Simple mock that always succeeds, tracks call counts
//! - [`StatefulMockDapAdapter`] - Full-featured mock with event sending and state tracking
//! - [`MockDapAdapterFactory`] - Factory for creating mock adapters

use async_trait::async_trait;
use detrix_core::{Metric, MetricEvent, Result};
use detrix_ports::{
    DapAdapter, DapAdapterFactory, DapAdapterRef, RemoveMetricResult, SetMetricResult,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

// ============================================================================
// Mock DAP Adapter (Simple)
// ============================================================================

/// A simple mock DAP adapter for testing purposes
///
/// This adapter doesn't connect to any real debugger - it just tracks
/// operations and always succeeds. For tests that need event simulation
/// or state tracking, use [`StatefulMockDapAdapter`] instead.
///
/// # Example
///
/// ```no_run
/// use detrix_testing::MockDapAdapter;
/// use detrix_ports::DapAdapter;
/// use detrix_testing::fixtures;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let adapter = MockDapAdapter::new();
/// let metric = fixtures::sample_metric("test_metric");
/// adapter.set_metric(&metric).await?;
/// assert_eq!(adapter.set_count(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct MockDapAdapter {
    /// Count of set_metric calls
    set_count: AtomicUsize,
    /// Count of remove_metric calls
    remove_count: AtomicUsize,
}

impl MockDapAdapter {
    /// Create a new mock adapter
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the number of times set_metric was called
    pub fn set_count(&self) -> usize {
        self.set_count.load(Ordering::SeqCst)
    }

    /// Get the number of times remove_metric was called
    pub fn remove_count(&self) -> usize {
        self.remove_count.load(Ordering::SeqCst)
    }

    /// Reset all counters
    pub fn reset(&self) {
        self.set_count.store(0, Ordering::SeqCst);
        self.remove_count.store(0, Ordering::SeqCst);
    }
}

#[async_trait]
impl DapAdapter for MockDapAdapter {
    async fn start(&self) -> Result<()> {
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        Ok(())
    }

    async fn ensure_connected(&self) -> Result<()> {
        // Mock is always "connected"
        Ok(())
    }

    fn is_connected(&self) -> bool {
        // Mock is always "connected"
        true
    }

    async fn set_metric(&self, metric: &Metric) -> Result<SetMetricResult> {
        self.set_count.fetch_add(1, Ordering::SeqCst);
        Ok(SetMetricResult {
            verified: true,
            line: metric.location.line,
            message: None,
        })
    }

    async fn remove_metric(&self, _metric: &Metric) -> Result<RemoveMetricResult> {
        self.remove_count.fetch_add(1, Ordering::SeqCst);
        Ok(RemoveMetricResult::success())
    }

    async fn subscribe_events(&self) -> Result<mpsc::Receiver<MetricEvent>> {
        let (_tx, rx) = mpsc::channel(1);
        Ok(rx)
    }
}

// ============================================================================
// Stateful Mock DAP Adapter (Full-featured)
// ============================================================================

/// A full-featured mock DAP adapter with state tracking and event simulation
///
/// Use this adapter when tests need to:
/// - Verify start/stop was called
/// - Track connection state
/// - Simulate events from a debugger
/// - Subscribe multiple times
///
/// # Example
///
/// ```no_run
/// use detrix_testing::StatefulMockDapAdapter;
/// use detrix_ports::DapAdapter;
/// use detrix_testing::fixtures;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let adapter = StatefulMockDapAdapter::new();
/// adapter.start().await?;
/// assert!(adapter.was_started());
///
/// // Simulate an event from the debugger
/// let event = fixtures::sample_event_for_metric(1, "test");
/// adapter.send_event(event).await;
/// # Ok(())
/// # }
/// ```
pub struct StatefulMockDapAdapter {
    /// Whether the adapter is "connected"
    connected: AtomicBool,
    /// Whether start() was called
    started: AtomicBool,
    /// Whether stop() was called
    stopped: AtomicBool,
    /// Event sender - used to simulate events from debugger (bounded for backpressure)
    event_tx: Mutex<Option<mpsc::Sender<MetricEvent>>>,
    /// How many times subscribe_events was called
    subscribe_count: AtomicUsize,
    /// Count of set_metric calls
    set_count: AtomicUsize,
    /// Count of remove_metric calls
    remove_count: AtomicUsize,
}

impl StatefulMockDapAdapter {
    /// Create a new stateful mock adapter
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            event_tx: Mutex::new(None),
            subscribe_count: AtomicUsize::new(0),
            set_count: AtomicUsize::new(0),
            remove_count: AtomicUsize::new(0),
        }
    }

    /// Send a test event (simulates debugger output)
    pub async fn send_event(&self, event: MetricEvent) {
        if let Some(tx) = self.event_tx.lock().await.as_ref() {
            let _ = tx.try_send(event);
        }
    }

    /// Check if start was called
    pub fn was_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Check if stop was called
    pub fn was_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Get the number of times subscribe_events was called
    pub fn subscribe_count(&self) -> usize {
        self.subscribe_count.load(Ordering::SeqCst)
    }

    /// Get the number of times set_metric was called
    pub fn set_count(&self) -> usize {
        self.set_count.load(Ordering::SeqCst)
    }

    /// Get the number of times remove_metric was called
    pub fn remove_count(&self) -> usize {
        self.remove_count.load(Ordering::SeqCst)
    }

    /// Reset all state
    pub fn reset(&self) {
        self.connected.store(false, Ordering::SeqCst);
        self.started.store(false, Ordering::SeqCst);
        self.stopped.store(false, Ordering::SeqCst);
        self.subscribe_count.store(0, Ordering::SeqCst);
        self.set_count.store(0, Ordering::SeqCst);
        self.remove_count.store(0, Ordering::SeqCst);
    }
}

impl Default for StatefulMockDapAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DapAdapter for StatefulMockDapAdapter {
    async fn start(&self) -> Result<()> {
        self.started.store(true, Ordering::SeqCst);
        self.connected.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.stopped.store(true, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn ensure_connected(&self) -> Result<()> {
        if self.connected.load(Ordering::SeqCst) {
            Ok(())
        } else {
            self.start().await
        }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    async fn set_metric(&self, metric: &Metric) -> Result<SetMetricResult> {
        self.set_count.fetch_add(1, Ordering::SeqCst);
        Ok(SetMetricResult {
            verified: true,
            line: metric.location.line,
            message: None,
        })
    }

    async fn remove_metric(&self, _metric: &Metric) -> Result<RemoveMetricResult> {
        self.remove_count.fetch_add(1, Ordering::SeqCst);
        Ok(RemoveMetricResult::success())
    }

    async fn subscribe_events(&self) -> Result<mpsc::Receiver<MetricEvent>> {
        self.subscribe_count.fetch_add(1, Ordering::SeqCst);
        // Use reasonable default capacity for testing
        let (tx, rx) = mpsc::channel(1000);
        *self.event_tx.lock().await = Some(tx);
        Ok(rx)
    }
}

// ============================================================================
// Mock DAP Adapter Factory
// ============================================================================

/// A mock DAP adapter factory for testing purposes
///
/// Creates MockDapAdapter instances. Useful for testing code that needs to
/// create adapters via the DapAdapterFactory trait.
#[derive(Debug, Default)]
pub struct MockDapAdapterFactory;

impl MockDapAdapterFactory {
    /// Create a new mock factory
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DapAdapterFactory for MockDapAdapterFactory {
    async fn create_python_adapter(&self, _host: &str, _port: u16) -> Result<DapAdapterRef> {
        Ok(Arc::new(MockDapAdapter::new()) as DapAdapterRef)
    }

    async fn create_go_adapter(&self, _host: &str, _port: u16) -> Result<DapAdapterRef> {
        Ok(Arc::new(MockDapAdapter::new()) as DapAdapterRef)
    }

    async fn create_rust_adapter(
        &self,
        _host: &str,
        _port: u16,
        _program: Option<&str>,
        _pid: Option<u32>,
    ) -> Result<DapAdapterRef> {
        Ok(Arc::new(MockDapAdapter::new()) as DapAdapterRef)
    }
}
