//! Circuit breaker middleware for EventOutput
//!
//! Wraps any EventOutput with circuit breaker protection to prevent cascading
//! failures when the downstream service is unavailable.
//!
//! # Circuit Breaker States
//!
//! - **Closed**: Normal operation, requests flow through
//! - **Open**: Too many failures, requests are rejected immediately
//! - **HalfOpen**: Testing if service recovered, limited requests allowed
//!
//! # Example
//!
//! ```rust,ignore
//! use detrix_output::middleware::CircuitBreakerOutput;
//! use detrix_output::{GelfTcpBuilder, CircuitBreakerConfig};
//!
//! let tcp = GelfTcpBuilder::new("graylog", 12201).build();
//! let protected = CircuitBreakerOutput::new(tcp, CircuitBreakerConfig::new(5, 30_000));
//!
//! // When circuit is open, requests fail fast without hitting the inner output
//! protected.send(&event).await?;
//! ```

use crate::circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerStats, CircuitState,
};
use async_trait::async_trait;
use detrix_core::{MetricEvent, Result};
use detrix_ports::{EventOutput, OutputStats};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::debug;

/// Circuit breaker wrapper for any EventOutput
///
/// Wraps an inner output and protects it with circuit breaker logic.
/// When the circuit opens, requests are rejected without hitting the inner output.
pub struct CircuitBreakerOutput<T: EventOutput> {
    inner: T,
    circuit_breaker: CircuitBreaker,
    events_rejected: AtomicU64,
}

impl<T: EventOutput> CircuitBreakerOutput<T> {
    /// Create a new circuit breaker wrapper
    pub fn new(inner: T, config: CircuitBreakerConfig) -> Self {
        Self {
            circuit_breaker: CircuitBreaker::new(config),
            inner,
            events_rejected: AtomicU64::new(0),
        }
    }

    /// Create with a named circuit breaker for better logging
    pub fn with_name(inner: T, config: CircuitBreakerConfig, name: impl Into<String>) -> Self {
        Self {
            circuit_breaker: CircuitBreaker::new(config.with_name(name)),
            inner,
            events_rejected: AtomicU64::new(0),
        }
    }

    /// Get the number of events rejected by circuit breaker
    pub fn events_rejected(&self) -> u64 {
        self.events_rejected.load(Ordering::Relaxed)
    }

    /// Get circuit breaker state
    pub fn circuit_state(&self) -> CircuitState {
        self.circuit_breaker.state()
    }

    /// Get circuit breaker statistics
    pub fn circuit_stats(&self) -> CircuitBreakerStats {
        self.circuit_breaker.stats()
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

    /// Reset circuit breaker to closed state
    pub fn reset_circuit(&self) {
        self.circuit_breaker.reset();
    }
}

#[async_trait]
impl<T: EventOutput + Send + Sync> EventOutput for CircuitBreakerOutput<T> {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        // Gate: Check if circuit allows request
        if !self.circuit_breaker.allow_request() {
            self.events_rejected.fetch_add(1, Ordering::Relaxed);
            debug!(
                state = %self.circuit_breaker.state(),
                "Send blocked by circuit breaker"
            );
            return Err(detrix_core::Error::Output(
                "Circuit breaker is open - request blocked".to_string(),
            ));
        }

        // Execute: Send to inner output
        match self.inner.send(event).await {
            Ok(()) => {
                self.circuit_breaker.record_success();
                Ok(())
            }
            Err(e) => {
                self.circuit_breaker.record_failure();
                Err(e)
            }
        }
    }

    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        // Gate: Check if circuit allows request
        if !self.circuit_breaker.allow_request() {
            self.events_rejected
                .fetch_add(events.len() as u64, Ordering::Relaxed);
            debug!(
                state = %self.circuit_breaker.state(),
                count = events.len(),
                "Batch blocked by circuit breaker"
            );
            return Err(detrix_core::Error::Output(
                "Circuit breaker is open - batch blocked".to_string(),
            ));
        }

        // Execute: Send batch to inner output
        match self.inner.send_batch(events).await {
            Ok(()) => {
                self.circuit_breaker.record_success();
                Ok(())
            }
            Err(e) => {
                self.circuit_breaker.record_failure();
                Err(e)
            }
        }
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }

    fn stats(&self) -> OutputStats {
        self.inner.stats()
    }

    fn is_healthy(&self) -> bool {
        self.circuit_breaker.state() != CircuitState::Open && self.inner.is_healthy()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }
}

impl<T: EventOutput + std::fmt::Debug> std::fmt::Debug for CircuitBreakerOutput<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircuitBreakerOutput")
            .field("inner", &self.inner)
            .field("circuit_state", &self.circuit_state())
            .field("events_rejected", &self.events_rejected())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::MockOutput;
    use super::*;
    use detrix_testing::fixtures::sample_test_event as sample_event;

    #[tokio::test]
    async fn test_circuit_breaker_passes_through_on_success() {
        let mock = MockOutput::new_with_failure(false);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(3, 30_000));

        let event = sample_event();
        cb.send(&event).await.unwrap();
        cb.send(&event).await.unwrap();

        assert_eq!(cb.inner().send_count(), 2);
        assert_eq!(cb.events_rejected(), 0);
        assert_eq!(cb.circuit_state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_circuit_breaker_opens_on_failures() {
        let mock = MockOutput::new_with_failure(true);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(2, 60_000));

        let event = sample_event();

        // First two failures should open the circuit
        let _ = cb.send(&event).await;
        let _ = cb.send(&event).await;

        assert_eq!(cb.circuit_state(), CircuitState::Open);

        // Third request should be rejected without hitting mock
        let result = cb.send(&event).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Circuit breaker is open"));

        // Mock was only called twice (not three times)
        assert_eq!(cb.inner().send_count(), 2);
        assert_eq!(cb.events_rejected(), 1);
    }

    #[tokio::test]
    async fn test_circuit_breaker_records_success() {
        let mock = MockOutput::new_with_failure(false);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(3, 30_000));

        let event = sample_event();
        cb.send(&event).await.unwrap();

        let stats = cb.circuit_stats();
        assert_eq!(stats.total_successes, 1);
        assert_eq!(stats.total_failures, 0);
    }

    #[tokio::test]
    async fn test_circuit_breaker_records_failure() {
        let mock = MockOutput::new_with_failure(true);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(5, 30_000));

        let event = sample_event();
        let _ = cb.send(&event).await;

        let stats = cb.circuit_stats();
        assert_eq!(stats.total_successes, 0);
        assert_eq!(stats.total_failures, 1);
    }

    #[tokio::test]
    async fn test_circuit_breaker_send_batch() {
        let mock = MockOutput::new_with_failure(false);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(3, 30_000));

        let events: Vec<MetricEvent> = (0..5).map(|_| sample_event()).collect();
        cb.send_batch(&events).await.unwrap();

        assert_eq!(cb.inner().batch_count(), 5);
        assert_eq!(cb.circuit_stats().total_successes, 1);
    }

    #[tokio::test]
    async fn test_circuit_breaker_batch_rejected_when_open() {
        let mock = MockOutput::new_with_failure(true);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(2, 60_000));

        let event = sample_event();
        let events: Vec<MetricEvent> = (0..3).map(|_| sample_event()).collect();

        // Open the circuit
        let _ = cb.send(&event).await;
        let _ = cb.send(&event).await;

        assert_eq!(cb.circuit_state(), CircuitState::Open);

        // Batch should be rejected
        let result = cb.send_batch(&events).await;
        assert!(result.is_err());
        assert_eq!(cb.events_rejected(), 3); // All events in batch counted
    }

    #[tokio::test]
    async fn test_circuit_breaker_is_healthy() {
        let mock = MockOutput::new_with_failure(true);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(2, 60_000));

        assert!(cb.is_healthy()); // Circuit closed = healthy

        let event = sample_event();
        let _ = cb.send(&event).await;
        let _ = cb.send(&event).await;

        assert!(!cb.is_healthy()); // Circuit open = unhealthy
    }

    #[tokio::test]
    async fn test_circuit_breaker_reset() {
        let mock = MockOutput::new_with_failure(true);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(2, 60_000));

        let event = sample_event();
        let _ = cb.send(&event).await;
        let _ = cb.send(&event).await;

        assert_eq!(cb.circuit_state(), CircuitState::Open);

        cb.reset_circuit();
        assert_eq!(cb.circuit_state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_accessors() {
        let mock = MockOutput::new_with_failure(false);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(3, 30_000));

        assert_eq!(cb.name(), "mock");
        assert_eq!(cb.inner().name(), "mock");
    }

    #[test]
    fn test_circuit_breaker_into_inner() {
        let mock = MockOutput::new_with_failure(false);
        let cb = CircuitBreakerOutput::new(mock, CircuitBreakerConfig::new(3, 30_000));

        let inner = cb.into_inner();
        assert_eq!(inner.name(), "mock");
    }

    #[tokio::test]
    async fn test_circuit_breaker_with_arc() {
        let mock = MockOutput::new_with_failure(false);
        let cb = std::sync::Arc::new(CircuitBreakerOutput::new(
            mock,
            CircuitBreakerConfig::new(3, 30_000),
        ));

        let event = sample_event();
        cb.send(&event).await.unwrap();

        assert_eq!(cb.events_rejected(), 0);
    }
}
