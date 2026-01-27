//! Batching middleware for EventOutput
//!
//! Buffers events and flushes them in batches for improved throughput.
//! Useful for outputs where batching is more efficient (e.g., HTTP).
//!
//! # Example
//!
//! ```rust,ignore
//! use detrix_output::middleware::{BatchingConfig, BatchingOutput};
//! use std::time::Duration;
//!
//! let http = GelfHttpBuilder::new("http://graylog:12201/gelf").build();
//! let batched = BatchingOutput::new(http, BatchingConfig::new(100)
//!     .with_flush_interval(Duration::from_secs(1)));
//!
//! // Events are buffered until batch_size reached or flush_interval elapsed
//! batched.send(&event).await?;
//!
//! // Or flush manually
//! batched.flush().await?;
//! ```

use async_trait::async_trait;
use detrix_core::{MetricEvent, Result};
use detrix_ports::{EventOutput, OutputStats};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::debug;

/// Batching configuration
#[derive(Debug, Clone)]
pub struct BatchingConfig {
    /// Maximum events to buffer before flushing
    pub batch_size: usize,
    /// Maximum time to buffer events (None = no time-based flush)
    pub flush_interval: Option<Duration>,
}

impl Default for BatchingConfig {
    fn default() -> Self {
        Self {
            batch_size: 100,
            flush_interval: Some(Duration::from_secs(1)),
        }
    }
}

impl BatchingConfig {
    /// Create config with specified batch size
    pub fn new(batch_size: usize) -> Self {
        Self {
            batch_size,
            flush_interval: None,
        }
    }

    /// Set flush interval for time-based flushing
    pub fn with_flush_interval(mut self, interval: Duration) -> Self {
        self.flush_interval = Some(interval);
        self
    }
}

/// Internal buffer state
struct BatchBuffer {
    events: Vec<MetricEvent>,
    last_flush: Instant,
}

impl BatchBuffer {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            last_flush: Instant::now(),
        }
    }
}

/// Batching wrapper for any EventOutput
///
/// Buffers events and flushes when batch_size reached or flush_interval exceeded.
/// Events are sent via `send_batch()` to the inner output.
pub struct BatchingOutput<T: EventOutput> {
    inner: T,
    config: BatchingConfig,
    buffer: Mutex<BatchBuffer>,
    batches_sent: AtomicU64,
    events_buffered: AtomicU64,
}

impl<T: EventOutput> BatchingOutput<T> {
    /// Create a new batching wrapper
    pub fn new(inner: T, config: BatchingConfig) -> Self {
        Self {
            inner,
            config,
            buffer: Mutex::new(BatchBuffer::new()),
            batches_sent: AtomicU64::new(0),
            events_buffered: AtomicU64::new(0),
        }
    }

    /// Create with simple batch size
    pub fn with_batch_size(inner: T, batch_size: usize) -> Self {
        Self::new(inner, BatchingConfig::new(batch_size))
    }

    /// Get number of batches sent
    pub fn batches_sent(&self) -> u64 {
        self.batches_sent.load(Ordering::Relaxed)
    }

    /// Get total events buffered (including flushed)
    pub fn events_buffered(&self) -> u64 {
        self.events_buffered.load(Ordering::Relaxed)
    }

    /// Get reference to inner output
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Get mutable reference to inner output
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Get current buffer size (approximate - lock not held)
    pub async fn current_buffer_size(&self) -> usize {
        self.buffer.lock().await.events.len()
    }

    /// Check if flush is needed based on size or time
    fn should_flush(&self, buffer: &BatchBuffer) -> bool {
        if buffer.events.len() >= self.config.batch_size {
            return true;
        }
        if let Some(interval) = self.config.flush_interval {
            if buffer.last_flush.elapsed() >= interval && !buffer.events.is_empty() {
                return true;
            }
        }
        false
    }

    /// Flush buffer to inner output
    async fn do_flush(&self, buffer: &mut BatchBuffer) -> Result<()> {
        if buffer.events.is_empty() {
            return Ok(());
        }

        let events = std::mem::take(&mut buffer.events);
        let count = events.len();
        buffer.last_flush = Instant::now();

        debug!(count, "Flushing batch to output");
        self.inner.send_batch(&events).await?;
        self.batches_sent.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[async_trait]
impl<T: EventOutput + Send + Sync> EventOutput for BatchingOutput<T> {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        let mut buffer = self.buffer.lock().await;
        buffer.events.push(event.clone());
        self.events_buffered.fetch_add(1, Ordering::Relaxed);

        if self.should_flush(&buffer) {
            self.do_flush(&mut buffer).await?;
        }
        Ok(())
    }

    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        let mut buffer = self.buffer.lock().await;
        buffer.events.extend(events.iter().cloned());
        self.events_buffered
            .fetch_add(events.len() as u64, Ordering::Relaxed);

        if self.should_flush(&buffer) {
            self.do_flush(&mut buffer).await?;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        let mut buffer = self.buffer.lock().await;
        self.do_flush(&mut buffer).await?;
        self.inner.flush().await
    }

    fn stats(&self) -> OutputStats {
        self.inner.stats()
    }

    fn is_healthy(&self) -> bool {
        self.inner.is_healthy()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }
}

impl<T: EventOutput + std::fmt::Debug> std::fmt::Debug for BatchingOutput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchingOutput")
            .field("inner", &self.inner)
            .field("batch_size", &self.config.batch_size)
            .field("flush_interval", &self.config.flush_interval)
            .field("batches_sent", &self.batches_sent())
            .finish()
    }
}

impl<T: EventOutput> Drop for BatchingOutput<T> {
    fn drop(&mut self) {
        // Note: Can't flush async in drop.
        // Caller should call flush() before dropping.
        let buffer = self.buffer.get_mut();
        if !buffer.events.is_empty() {
            tracing::warn!(
                count = buffer.events.len(),
                "BatchingOutput dropped with unflushed events - call flush() before dropping"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_testing::fixtures::sample_test_event as sample_event;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    /// Mock output that tracks batches received
    struct BatchTrackingOutput {
        batches: Arc<TokioMutex<Vec<Vec<MetricEvent>>>>,
        flush_count: AtomicU64,
    }

    impl BatchTrackingOutput {
        fn new() -> Self {
            Self {
                batches: Arc::new(TokioMutex::new(Vec::new())),
                flush_count: AtomicU64::new(0),
            }
        }

        async fn batch_count(&self) -> usize {
            self.batches.lock().await.len()
        }

        async fn total_events(&self) -> usize {
            self.batches.lock().await.iter().map(|b| b.len()).sum()
        }
    }

    #[async_trait]
    impl EventOutput for BatchTrackingOutput {
        async fn send(&self, event: &MetricEvent) -> Result<()> {
            self.batches.lock().await.push(vec![event.clone()]);
            Ok(())
        }

        async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
            if !events.is_empty() {
                self.batches.lock().await.push(events.to_vec());
            }
            Ok(())
        }

        async fn flush(&self) -> Result<()> {
            self.flush_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn stats(&self) -> OutputStats {
            OutputStats {
                events_sent: 0,
                events_failed: 0,
                bytes_sent: 0,
                reconnections: 0,
                connected: true,
            }
        }

        fn name(&self) -> &str {
            "batch-tracking"
        }
    }

    #[tokio::test]
    async fn test_batching_buffers_until_size_reached() {
        let mock = BatchTrackingOutput::new();
        let batched = BatchingOutput::new(mock, BatchingConfig::new(3));

        let event = sample_event();

        // Send 2 events - should stay buffered
        batched.send(&event).await.unwrap();
        batched.send(&event).await.unwrap();

        assert_eq!(batched.current_buffer_size().await, 2);
        assert_eq!(batched.inner().batch_count().await, 0);

        // Send 3rd event - should trigger flush
        batched.send(&event).await.unwrap();

        assert_eq!(batched.current_buffer_size().await, 0);
        assert_eq!(batched.inner().batch_count().await, 1);
        assert_eq!(batched.inner().total_events().await, 3);
        assert_eq!(batched.batches_sent(), 1);
    }

    #[tokio::test]
    async fn test_batching_explicit_flush() {
        let mock = BatchTrackingOutput::new();
        let batched = BatchingOutput::new(mock, BatchingConfig::new(100));

        let event = sample_event();

        // Send events (below batch size)
        batched.send(&event).await.unwrap();
        batched.send(&event).await.unwrap();

        assert_eq!(batched.current_buffer_size().await, 2);

        // Explicit flush
        batched.flush().await.unwrap();

        assert_eq!(batched.current_buffer_size().await, 0);
        assert_eq!(batched.inner().batch_count().await, 1);
        assert_eq!(batched.inner().total_events().await, 2);
    }

    #[tokio::test]
    async fn test_batching_send_batch_accumulates() {
        let mock = BatchTrackingOutput::new();
        let batched = BatchingOutput::new(mock, BatchingConfig::new(10));

        let events: Vec<MetricEvent> = (0..4).map(|_| sample_event()).collect();

        // Send batch of 4
        batched.send_batch(&events).await.unwrap();
        assert_eq!(batched.current_buffer_size().await, 4);

        // Send another batch of 4
        batched.send_batch(&events).await.unwrap();
        assert_eq!(batched.current_buffer_size().await, 8);

        // Send another batch - triggers flush (8 + 4 > 10)
        batched.send_batch(&events).await.unwrap();

        // Should have flushed all 12
        assert_eq!(batched.current_buffer_size().await, 0);
        assert_eq!(batched.inner().total_events().await, 12);
    }

    #[tokio::test]
    async fn test_batching_empty_flush_is_noop() {
        let mock = BatchTrackingOutput::new();
        let batched = BatchingOutput::new(mock, BatchingConfig::new(10));

        // Flush empty buffer
        batched.flush().await.unwrap();

        assert_eq!(batched.batches_sent(), 0);
        assert_eq!(batched.inner().batch_count().await, 0);
    }

    #[tokio::test]
    async fn test_batching_time_based_flush() {
        let mock = BatchTrackingOutput::new();
        let config = BatchingConfig::new(100).with_flush_interval(Duration::from_millis(50));
        let batched = BatchingOutput::new(mock, config);

        let event = sample_event();

        // Send event
        batched.send(&event).await.unwrap();
        assert_eq!(batched.current_buffer_size().await, 1);

        // Wait for flush interval
        tokio::time::sleep(Duration::from_millis(60)).await;

        // Send another event - should trigger time-based flush
        batched.send(&event).await.unwrap();

        // Buffer should be empty (flushed 2 events)
        // Note: The first send triggers flush check after adding
        assert!(batched.current_buffer_size().await <= 1);
    }

    #[test]
    fn test_batching_config() {
        let config = BatchingConfig::new(50).with_flush_interval(Duration::from_secs(2));

        assert_eq!(config.batch_size, 50);
        assert_eq!(config.flush_interval, Some(Duration::from_secs(2)));
    }

    #[test]
    fn test_batching_config_default() {
        let config = BatchingConfig::default();

        assert_eq!(config.batch_size, 100);
        assert_eq!(config.flush_interval, Some(Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn test_batching_accessors() {
        let mock = BatchTrackingOutput::new();
        let batched = BatchingOutput::new(mock, BatchingConfig::new(10));

        assert_eq!(batched.name(), "batch-tracking");
        assert_eq!(batched.events_buffered(), 0);
        assert_eq!(batched.batches_sent(), 0);
    }

    #[tokio::test]
    async fn test_batching_stats_pass_through() {
        let mock = BatchTrackingOutput::new();
        let batched = BatchingOutput::new(mock, BatchingConfig::new(10));

        let stats = batched.stats();
        assert!(stats.connected);
    }

    #[tokio::test]
    async fn test_batching_is_healthy() {
        let mock = BatchTrackingOutput::new();
        let batched = BatchingOutput::new(mock, BatchingConfig::new(10));

        assert!(batched.is_healthy());
    }

    #[tokio::test]
    async fn test_batching_multiple_flushes() {
        let mock = BatchTrackingOutput::new();
        let batched = BatchingOutput::new(mock, BatchingConfig::new(2));

        let event = sample_event();

        // First batch
        batched.send(&event).await.unwrap();
        batched.send(&event).await.unwrap();

        // Second batch
        batched.send(&event).await.unwrap();
        batched.send(&event).await.unwrap();

        // Third batch
        batched.send(&event).await.unwrap();
        batched.send(&event).await.unwrap();

        assert_eq!(batched.batches_sent(), 3);
        assert_eq!(batched.events_buffered(), 6);
        assert_eq!(batched.inner().total_events().await, 6);
    }
}
