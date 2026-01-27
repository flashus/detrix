//! Timeout middleware for EventOutput
//!
//! Wraps any EventOutput with configurable send timeouts.
//! This provides a consistent timeout behavior across all output types.
//!
//! # Example
//!
//! ```rust,ignore
//! use detrix_output::middleware::TimeoutOutput;
//! use std::time::Duration;
//!
//! let tcp = GelfTcpBuilder::new("graylog", 12201).build();
//! let with_timeout = TimeoutOutput::with_timeout(tcp, Duration::from_secs(10));
//!
//! // Will fail if send takes longer than 10 seconds
//! with_timeout.send(&event).await?;
//! ```

use async_trait::async_trait;
use detrix_core::{MetricEvent, Result};
use detrix_ports::{EventOutput, OutputStats};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::warn;

use detrix_config::constants::DEFAULT_OUTPUT_SEND_TIMEOUT_SECS;

/// Timeout configuration
#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    /// Timeout for send operations
    pub send_timeout: Duration,
    /// Optional name for logging
    pub name: Option<String>,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            send_timeout: Duration::from_secs(DEFAULT_OUTPUT_SEND_TIMEOUT_SECS),
            name: None,
        }
    }
}

impl TimeoutConfig {
    /// Create config with specified timeout
    pub fn new(send_timeout: Duration) -> Self {
        Self {
            send_timeout,
            name: None,
        }
    }

    /// Set a name for better logging
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Timeout wrapper for any EventOutput
///
/// Wraps send operations with configurable timeout.
/// If the inner send operation exceeds the timeout, an error is returned.
pub struct TimeoutOutput<T: EventOutput> {
    inner: T,
    config: TimeoutConfig,
    timeouts: AtomicU64,
}

impl<T: EventOutput> TimeoutOutput<T> {
    /// Create a new timeout wrapper with full config
    pub fn new(inner: T, config: TimeoutConfig) -> Self {
        Self {
            inner,
            config,
            timeouts: AtomicU64::new(0),
        }
    }

    /// Create with simple timeout duration
    pub fn with_timeout(inner: T, timeout: Duration) -> Self {
        Self::new(inner, TimeoutConfig::new(timeout))
    }

    /// Get number of timeout events
    pub fn timeout_count(&self) -> u64 {
        self.timeouts.load(Ordering::Relaxed)
    }

    /// Get reference to inner output
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Get mutable reference to inner output
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Consume wrapper and return inner output
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Get configured timeout duration
    pub fn timeout_duration(&self) -> Duration {
        self.config.send_timeout
    }
}

#[async_trait]
impl<T: EventOutput + Send + Sync> EventOutput for TimeoutOutput<T> {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        match tokio::time::timeout(self.config.send_timeout, self.inner.send(event)).await {
            Ok(result) => result,
            Err(_) => {
                self.timeouts.fetch_add(1, Ordering::Relaxed);
                warn!(
                    output = self.config.name.as_deref().unwrap_or(self.inner.name()),
                    timeout_ms = self.config.send_timeout.as_millis(),
                    "Send operation timed out"
                );
                Err(detrix_core::Error::Output("Send timeout".to_string()))
            }
        }
    }

    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        match tokio::time::timeout(self.config.send_timeout, self.inner.send_batch(events)).await {
            Ok(result) => result,
            Err(_) => {
                self.timeouts.fetch_add(1, Ordering::Relaxed);
                warn!(
                    output = self.config.name.as_deref().unwrap_or(self.inner.name()),
                    timeout_ms = self.config.send_timeout.as_millis(),
                    count = events.len(),
                    "Batch send operation timed out"
                );
                Err(detrix_core::Error::Output("Batch send timeout".to_string()))
            }
        }
    }

    async fn flush(&self) -> Result<()> {
        // Flush uses same timeout - could be configurable separately
        match tokio::time::timeout(self.config.send_timeout, self.inner.flush()).await {
            Ok(result) => result,
            Err(_) => {
                warn!(
                    output = self.config.name.as_deref().unwrap_or(self.inner.name()),
                    timeout_ms = self.config.send_timeout.as_millis(),
                    "Flush operation timed out"
                );
                Err(detrix_core::Error::Output("Flush timeout".to_string()))
            }
        }
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

impl<T: EventOutput + std::fmt::Debug> std::fmt::Debug for TimeoutOutput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimeoutOutput")
            .field("inner", &self.inner)
            .field("timeout", &self.config.send_timeout)
            .field("timeouts", &self.timeout_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_testing::fixtures::sample_test_event as sample_event;
    use std::sync::atomic::AtomicU64;

    /// Mock output that can delay responses
    struct DelayedOutput {
        delay: Duration,
        send_count: AtomicU64,
    }

    impl DelayedOutput {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                send_count: AtomicU64::new(0),
            }
        }

        fn send_count(&self) -> u64 {
            self.send_count.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl EventOutput for DelayedOutput {
        async fn send(&self, _event: &MetricEvent) -> Result<()> {
            self.send_count.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(self.delay).await;
            Ok(())
        }

        async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
            self.send_count
                .fetch_add(events.len() as u64, Ordering::Relaxed);
            tokio::time::sleep(self.delay).await;
            Ok(())
        }

        async fn flush(&self) -> Result<()> {
            tokio::time::sleep(self.delay).await;
            Ok(())
        }

        fn stats(&self) -> OutputStats {
            OutputStats {
                events_sent: self.send_count.load(Ordering::Relaxed),
                events_failed: 0,
                bytes_sent: 0,
                reconnections: 0,
                connected: true,
            }
        }

        fn name(&self) -> &str {
            "delayed"
        }
    }

    #[tokio::test]
    async fn test_timeout_passes_through_fast_send() {
        let mock = DelayedOutput::new(Duration::from_millis(10));
        let timeout = TimeoutOutput::with_timeout(mock, Duration::from_secs(1));

        let event = sample_event();
        timeout.send(&event).await.unwrap();

        assert_eq!(timeout.inner().send_count(), 1);
        assert_eq!(timeout.timeout_count(), 0);
    }

    #[tokio::test]
    async fn test_timeout_fails_on_slow_send() {
        let mock = DelayedOutput::new(Duration::from_millis(200));
        let timeout = TimeoutOutput::with_timeout(mock, Duration::from_millis(50));

        let event = sample_event();
        let result = timeout.send(&event).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
        assert_eq!(timeout.timeout_count(), 1);
    }

    #[tokio::test]
    async fn test_timeout_batch() {
        let mock = DelayedOutput::new(Duration::from_millis(10));
        let timeout = TimeoutOutput::with_timeout(mock, Duration::from_secs(1));

        let events: Vec<MetricEvent> = (0..3).map(|_| sample_event()).collect();
        timeout.send_batch(&events).await.unwrap();

        assert_eq!(timeout.inner().send_count(), 3);
        assert_eq!(timeout.timeout_count(), 0);
    }

    #[tokio::test]
    async fn test_timeout_batch_fails_slow() {
        let mock = DelayedOutput::new(Duration::from_millis(200));
        let timeout = TimeoutOutput::with_timeout(mock, Duration::from_millis(50));

        let events: Vec<MetricEvent> = (0..3).map(|_| sample_event()).collect();
        let result = timeout.send_batch(&events).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[tokio::test]
    async fn test_timeout_flush() {
        let mock = DelayedOutput::new(Duration::from_millis(10));
        let timeout = TimeoutOutput::with_timeout(mock, Duration::from_secs(1));

        timeout.flush().await.unwrap();
    }

    #[tokio::test]
    async fn test_timeout_flush_fails_slow() {
        let mock = DelayedOutput::new(Duration::from_millis(200));
        let timeout = TimeoutOutput::with_timeout(mock, Duration::from_millis(50));

        let result = timeout.flush().await;
        assert!(result.is_err());
    }

    #[test]
    fn test_timeout_config() {
        let config = TimeoutConfig::new(Duration::from_secs(5)).with_name("my-output");

        assert_eq!(config.send_timeout, Duration::from_secs(5));
        assert_eq!(config.name, Some("my-output".to_string()));
    }

    #[test]
    fn test_timeout_accessors() {
        let mock = DelayedOutput::new(Duration::from_millis(10));
        let timeout = TimeoutOutput::with_timeout(mock, Duration::from_secs(5));

        assert_eq!(timeout.name(), "delayed");
        assert_eq!(timeout.timeout_duration(), Duration::from_secs(5));
    }

    #[test]
    fn test_timeout_into_inner() {
        let mock = DelayedOutput::new(Duration::from_millis(10));
        let timeout = TimeoutOutput::with_timeout(mock, Duration::from_secs(5));

        let inner = timeout.into_inner();
        assert_eq!(inner.delay, Duration::from_millis(10));
    }
}
