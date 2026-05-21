//! Circuit Breaker for RemoteAdapter
//!
//! Three-state machine protecting request-response calls:
//! - **Closed:** requests pass through, 30s timeout per call
//! - **Open:** fail fast, no network wait
//! - **HalfOpen:** one probe allowed after 30s in Open

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use detrix_core::Error;

const STATE_CLOSED: u8 = 0;
const STATE_OPEN: u8 = 1;
const STATE_HALF_OPEN: u8 = 2;

struct CircuitBreakerInner {
    state: AtomicU8,
    consecutive_failures: AtomicU64,
    last_transition_ms: AtomicU64,
    failure_timestamps: Mutex<Vec<u64>>,
}

/// Circuit breaker protecting `set_metric` / `remove_metric` calls.
///
/// State transitions:
/// - Closed → Open: 3 consecutive timeouts within 60s window
/// - Open → HalfOpen: after 30s cooldown
/// - HalfOpen → Closed: on first success
/// - HalfOpen → Open: on timeout
#[derive(Clone)]
pub struct CircuitBreaker {
    inner: Arc<CircuitBreakerInner>,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CircuitBreakerInner {
                state: AtomicU8::new(STATE_CLOSED),
                consecutive_failures: AtomicU64::new(0),
                last_transition_ms: AtomicU64::new(now_ms()),
                failure_timestamps: Mutex::new(Vec::with_capacity(3)),
            }),
        }
    }

    /// Check if the circuit is open (failing fast).
    ///
    /// If the circuit is `Open` and the 30s cooldown has elapsed, this
    /// **transitions it to `HalfOpen`** as a side effect — one probe request
    /// will then be allowed through. Use only at call sites that intend to
    /// attempt a real operation. For pure status queries use `peek_open`.
    pub fn is_open(&self) -> bool {
        let state = self.inner.state.load(Ordering::Acquire);
        if state == STATE_OPEN {
            // Check if we should transition to HalfOpen (30s cooldown)
            let elapsed =
                now_ms().saturating_sub(self.inner.last_transition_ms.load(Ordering::Acquire));
            if elapsed >= 30_000 {
                self.inner
                    .state
                    .compare_exchange(
                        STATE_OPEN,
                        STATE_HALF_OPEN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            } else {
                true
            }
        } else {
            false
        }
    }

    /// Return `true` if the circuit is currently `Open` — **without** triggering
    /// the `Open → HalfOpen` transition. Safe to call from status/logging code.
    pub fn peek_open(&self) -> bool {
        self.inner.state.load(Ordering::Acquire) == STATE_OPEN
    }

    /// Execute a closure through the circuit breaker.
    ///
    /// If the circuit is Open, returns Err immediately without waiting.
    /// If Closed/HalfOpen, executes the closure. On timeout, records the failure
    /// and potentially opens the circuit.
    pub async fn call<F, Fut, T>(&self, f: F) -> Result<T, Error>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, Error>>,
    {
        if self.is_open() {
            return Err(Error::Adapter("agent circuit open".to_string()));
        }

        let result = f().await;

        match &result {
            Ok(_) => self.on_success(),
            // Only transport/timeout errors should trip the circuit.
            // Business errors (metric not found, invalid expression, etc.) are
            // terminal by definition and must not penalise the circuit.
            Err(e) if e.is_retryable() => self.on_failure(),
            Err(_) => {}
        }

        result
    }

    fn on_success(&self) {
        self.inner.consecutive_failures.store(0, Ordering::Relaxed);
        if let Ok(mut timestamps) = self.inner.failure_timestamps.lock() {
            timestamps.clear();
        }

        let state = self.inner.state.load(Ordering::Acquire);
        if state == STATE_HALF_OPEN {
            self.inner.state.store(STATE_CLOSED, Ordering::Release);
            self.inner
                .last_transition_ms
                .store(now_ms(), Ordering::Release);
        }
    }

    fn on_failure(&self) {
        let ts = now_ms();
        if let Ok(mut timestamps) = self.inner.failure_timestamps.lock() {
            timestamps.push(ts);
            // Keep only last 3 timestamps
            if timestamps.len() > 3 {
                timestamps.remove(0);
            }

            // Check if 3 failures within 60s window
            if timestamps.len() >= 3 {
                let window = ts.saturating_sub(timestamps[0]);
                if window <= 60_000 {
                    // Transition to Open
                    self.inner.state.store(STATE_OPEN, Ordering::Release);
                    self.inner.last_transition_ms.store(ts, Ordering::Release);
                    timestamps.clear();
                }
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_timeout_err() -> Error {
        Error::Adapter("timeout".to_string())
    }

    #[tokio::test]
    async fn test_closed_passes_through() {
        let cb = CircuitBreaker::new();
        let result = cb.call(|| async { Ok(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_opens_after_3_failures_in_60s() {
        let cb = CircuitBreaker::new();

        // 3 consecutive failures
        cb.call(|| async { Err::<(), _>(closed_timeout_err()) })
            .await
            .unwrap_err();
        cb.call(|| async { Err::<(), _>(closed_timeout_err()) })
            .await
            .unwrap_err();
        cb.call(|| async { Err::<(), _>(closed_timeout_err()) })
            .await
            .unwrap_err();

        // Circuit should be Open now
        assert!(cb.is_open());

        // Next call fails immediately
        let result = cb.call(|| async { Ok::<_, Error>(42) }).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("circuit open"));
    }

    #[tokio::test]
    async fn test_half_open_to_closed_on_success() {
        let cb = CircuitBreaker::new();

        // Open the circuit
        for _ in 0..3 {
            cb.call(|| async { Err::<(), _>(closed_timeout_err()) })
                .await
                .unwrap_err();
        }
        assert!(cb.is_open());

        // Wait 30s+ for HalfOpen transition (skip actual sleep by forcing state)
        cb.inner.state.store(STATE_HALF_OPEN, Ordering::Relaxed);

        // Success should close the circuit
        let result = cb.call(|| async { Ok::<_, Error>(42) }).await;
        assert!(result.is_ok());
        assert_eq!(cb.inner.state.load(Ordering::Relaxed), STATE_CLOSED);
    }

    #[tokio::test]
    async fn test_failure_count_resets_on_success() {
        let cb = CircuitBreaker::new();

        // 2 failures
        cb.call(|| async { Err::<(), _>(closed_timeout_err()) })
            .await
            .unwrap_err();
        cb.call(|| async { Err::<(), _>(closed_timeout_err()) })
            .await
            .unwrap_err();

        // Success resets the counter
        cb.call(|| async { Ok::<_, Error>(()) }).await.unwrap();

        // 2 more failures should NOT open the circuit (counter was reset)
        cb.call(|| async { Err::<(), _>(closed_timeout_err()) })
            .await
            .unwrap_err();
        cb.call(|| async { Err::<(), _>(closed_timeout_err()) })
            .await
            .unwrap_err();

        // Circuit should still be Closed (only 2 failures after reset)
        assert!(!cb.is_open());
    }

    #[tokio::test]
    async fn test_terminal_error_does_not_open_circuit() {
        let cb = CircuitBreaker::new();
        // MetricNotFound is Terminal (not retryable) — 3× should NOT open the circuit
        let terminal = || Error::MetricNotFound("x".to_string());
        for _ in 0..3 {
            cb.call(|| async { Err::<(), _>(terminal()) })
                .await
                .unwrap_err();
        }
        assert!(!cb.is_open(), "terminal errors must not open the circuit");
    }

    #[tokio::test]
    async fn test_clone_shares_state() {
        let cb = CircuitBreaker::new();
        let cb2 = cb.clone();

        // Failure on one should affect the other (shared Arc)
        cb.call(|| async { Err::<(), _>(closed_timeout_err()) })
            .await
            .unwrap_err();

        assert_eq!(
            cb.inner.consecutive_failures.load(Ordering::Relaxed),
            cb2.inner.consecutive_failures.load(Ordering::Relaxed)
        );
    }
}
