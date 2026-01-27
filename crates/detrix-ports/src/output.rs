//! Event Output Port
//!
//! Defines the interface for streaming events to external systems (Graylog, etc.)

use async_trait::async_trait;
use detrix_core::{MetricEvent, Result};
use std::sync::Arc;

/// Statistics for an event output
#[derive(Debug, Clone, Default)]
pub struct OutputStats {
    /// Total events sent
    pub events_sent: u64,
    /// Total events failed
    pub events_failed: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Number of reconnections
    pub reconnections: u64,
    /// Whether the output is currently connected
    pub connected: bool,
}

/// Port trait for event output destinations (Graylog, etc.)
///
/// Implementations should handle:
/// - Connection management
/// - Retry logic
/// - Buffering/batching
#[async_trait]
pub trait EventOutput: Send + Sync {
    /// Send a single event to the output
    ///
    /// Returns Ok(()) if the event was accepted (may be buffered).
    /// Returns Err if the event could not be accepted.
    async fn send(&self, event: &MetricEvent) -> Result<()>;

    /// Send multiple events in a batch
    ///
    /// Default implementation calls send() for each event.
    /// Implementations may override for better efficiency.
    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        for event in events {
            self.send(event).await?;
        }
        Ok(())
    }

    /// Flush any buffered events
    ///
    /// Ensures all buffered events are sent to the destination.
    async fn flush(&self) -> Result<()>;

    /// Get output statistics
    fn stats(&self) -> OutputStats;

    /// Check if the output is healthy/connected
    fn is_healthy(&self) -> bool {
        self.stats().connected
    }

    /// Get the name of this output (for logging/identification)
    fn name(&self) -> &str;
}

/// No-op output for testing or when no output is configured
pub struct NullOutput;

#[async_trait]
impl EventOutput for NullOutput {
    async fn send(&self, _event: &MetricEvent) -> Result<()> {
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn stats(&self) -> OutputStats {
        OutputStats {
            connected: true,
            ..Default::default()
        }
    }

    fn name(&self) -> &str {
        "null"
    }
}

/// Blanket implementation for Arc-wrapped outputs
///
/// Enables middleware composition with `Arc<T>` types (e.g., `Arc<GelfHttpOutput>`).
#[async_trait]
impl<T: EventOutput + ?Sized> EventOutput for Arc<T> {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        (**self).send(event).await
    }

    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        (**self).send_batch(events).await
    }

    async fn flush(&self) -> Result<()> {
        (**self).flush().await
    }

    fn stats(&self) -> OutputStats {
        (**self).stats()
    }

    fn is_healthy(&self) -> bool {
        (**self).is_healthy()
    }

    fn name(&self) -> &str {
        (**self).name()
    }
}
