//! Fluent middleware builder for composing EventOutput wrappers
//!
//! Provides a clean, chainable API for wrapping outputs with middleware.
//!
//! # Example
//!
//! ```rust,ignore
//! use detrix_output::{GelfTcpBuilder, CircuitBreakerConfig};
//! use detrix_output::middleware::OutputBuilder;
//! use std::time::Duration;
//!
//! let output = OutputBuilder::new(GelfTcpBuilder::new("graylog", 12201).build())
//!     .with_circuit_breaker(CircuitBreakerConfig::new(5, 30_000))
//!     .with_timeout(Duration::from_secs(10))
//!     .build();
//! ```

use super::{BatchingConfig, BatchingOutput, CircuitBreakerOutput, TimeoutConfig, TimeoutOutput};
use crate::circuit_breaker::CircuitBreakerConfig;
use detrix_ports::EventOutput;
use std::time::Duration;

/// Builder for composing middleware around an EventOutput
///
/// Provides a fluent API for wrapping outputs with middleware.
/// Middleware is applied in the order specified (inner to outer).
pub struct OutputBuilder<T: EventOutput> {
    inner: T,
}

impl<T: EventOutput> OutputBuilder<T> {
    /// Create a new middleware builder wrapping the given output
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    /// Consume the builder and return the wrapped output
    pub fn build(self) -> T {
        self.inner
    }

    /// Wrap with circuit breaker middleware
    ///
    /// Protects against cascading failures by blocking requests when
    /// the circuit is open.
    pub fn with_circuit_breaker(
        self,
        config: CircuitBreakerConfig,
    ) -> OutputBuilder<CircuitBreakerOutput<T>>
    where
        T: Send + Sync,
    {
        OutputBuilder {
            inner: CircuitBreakerOutput::new(self.inner, config),
        }
    }

    /// Wrap with timeout middleware using full config
    pub fn with_timeout_config(self, config: TimeoutConfig) -> OutputBuilder<TimeoutOutput<T>>
    where
        T: Send + Sync,
    {
        OutputBuilder {
            inner: TimeoutOutput::new(self.inner, config),
        }
    }

    /// Wrap with timeout middleware using a simple duration
    ///
    /// Fails requests that take longer than the specified duration.
    pub fn with_timeout(self, timeout: Duration) -> OutputBuilder<TimeoutOutput<T>>
    where
        T: Send + Sync,
    {
        OutputBuilder {
            inner: TimeoutOutput::with_timeout(self.inner, timeout),
        }
    }

    /// Wrap with batching middleware using full config
    pub fn with_batching_config(self, config: BatchingConfig) -> OutputBuilder<BatchingOutput<T>>
    where
        T: Send + Sync,
    {
        OutputBuilder {
            inner: BatchingOutput::new(self.inner, config),
        }
    }

    /// Wrap with batching middleware using a simple batch size
    ///
    /// Buffers events and flushes when batch size is reached.
    pub fn with_batching(self, batch_size: usize) -> OutputBuilder<BatchingOutput<T>>
    where
        T: Send + Sync,
    {
        OutputBuilder {
            inner: BatchingOutput::with_batch_size(self.inner, batch_size),
        }
    }
}

/// Extension trait for EventOutput to enable fluent middleware wrapping
pub trait EventOutputExt: EventOutput + Sized {
    /// Start building a middleware chain for this output
    fn with_middleware(self) -> OutputBuilder<Self> {
        OutputBuilder::new(self)
    }
}

/// Blanket implementation for all EventOutput types
impl<T: EventOutput> EventOutputExt for T {}

#[cfg(test)]
mod tests {
    use super::super::test_support::MockOutput;
    use super::*;
    use detrix_testing::fixtures::sample_test_event as sample_event;

    #[tokio::test]
    async fn test_builder_with_circuit_breaker() {
        let mock = MockOutput::new();
        let output = OutputBuilder::new(mock)
            .with_circuit_breaker(CircuitBreakerConfig::new(5, 30_000))
            .build();

        let event = sample_event();
        output.send(&event).await.unwrap();

        assert_eq!(output.circuit_stats().total_successes, 1);
    }

    #[tokio::test]
    async fn test_builder_with_timeout() {
        let mock = MockOutput::new();
        let output = OutputBuilder::new(mock)
            .with_timeout(Duration::from_secs(10))
            .build();

        let event = sample_event();
        output.send(&event).await.unwrap();

        assert_eq!(output.timeout_count(), 0);
    }

    #[tokio::test]
    async fn test_builder_with_batching() {
        let mock = MockOutput::new();
        let output = OutputBuilder::new(mock).with_batching(10).build();

        let event = sample_event();
        output.send(&event).await.unwrap();

        // Not flushed yet (batch size not reached)
        assert_eq!(output.current_buffer_size().await, 1);
    }

    #[tokio::test]
    async fn test_builder_chain_multiple_middleware() {
        let mock = MockOutput::new();
        let output = OutputBuilder::new(mock)
            .with_timeout(Duration::from_secs(10))
            .with_circuit_breaker(CircuitBreakerConfig::new(5, 30_000))
            .build();

        let event = sample_event();
        output.send(&event).await.unwrap();

        // Circuit breaker recorded success
        assert_eq!(output.circuit_stats().total_successes, 1);
        // Inner timeout wrapper is accessible
        assert_eq!(output.inner().timeout_count(), 0);
    }

    #[tokio::test]
    async fn test_extension_trait() {
        let mock = MockOutput::new();
        let output = mock
            .with_middleware()
            .with_circuit_breaker(CircuitBreakerConfig::new(5, 30_000))
            .build();

        let event = sample_event();
        output.send(&event).await.unwrap();

        assert_eq!(output.circuit_stats().total_successes, 1);
    }
}
