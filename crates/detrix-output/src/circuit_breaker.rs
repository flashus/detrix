//! Circuit Breaker Pattern Implementation
//!
//! Implements the circuit breaker pattern to prevent cascading failures
//! and reduce load on failing external services like Graylog.
//!
//! # States
//!
//! - **Closed**: Normal operation, requests flow through
//! - **Open**: Failure threshold exceeded, requests are blocked
//! - **Half-Open**: Testing if service recovered, single probe allowed
//!
//! # Example
//!
//! ```ignore
//! use detrix_output::CircuitBreaker;
//!
//! let breaker = CircuitBreaker::new(CircuitBreakerConfig {
//!     failure_threshold: 5,
//!     reset_timeout_ms: 30000,
//!     half_open_max_calls: 1,
//! });
//!
//! if breaker.allow_request() {
//!     match do_operation().await {
//!         Ok(_) => breaker.record_success(),
//!         Err(_) => breaker.record_failure(),
//!     }
//! } else {
//!     // Circuit is open, skip request
//! }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Circuit breaker state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation - requests flow through
    Closed,
    /// Failure threshold exceeded - requests are blocked
    Open,
    /// Testing recovery - limited requests allowed
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "closed"),
            Self::Open => write!(f, "open"),
            Self::HalfOpen => write!(f, "half-open"),
        }
    }
}

/// Configuration for the circuit breaker
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before opening the circuit
    pub failure_threshold: u32,
    /// Time to wait before transitioning from Open to Half-Open (ms)
    pub reset_timeout_ms: u64,
    /// Number of successful calls in Half-Open to close the circuit
    pub success_threshold: u32,
    /// Optional name for logging
    pub name: Option<String>,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            reset_timeout_ms: 30_000, // 30 seconds
            success_threshold: 2,
            name: None,
        }
    }
}

impl CircuitBreakerConfig {
    /// Create config with specified thresholds
    pub fn new(failure_threshold: u32, reset_timeout_ms: u64) -> Self {
        Self {
            failure_threshold,
            reset_timeout_ms,
            ..Default::default()
        }
    }

    /// Set a name for logging purposes
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the success threshold for half-open state
    pub fn with_success_threshold(mut self, threshold: u32) -> Self {
        self.success_threshold = threshold;
        self
    }
}

/// Internal state for the circuit breaker
struct InternalState {
    state: CircuitState,
    consecutive_failures: u32,
    consecutive_successes: u32,
    last_failure_time: Option<Instant>,
}

/// Thread-safe circuit breaker implementation
///
/// Protects external services from being overwhelmed when they're failing.
/// Automatically attempts recovery after the reset timeout.
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    internal: RwLock<InternalState>,
    // Statistics
    total_requests: AtomicU64,
    rejected_requests: AtomicU64,
    total_failures: AtomicU64,
    total_successes: AtomicU64,
    state_transitions: AtomicU64,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given configuration
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            internal: RwLock::new(InternalState {
                state: CircuitState::Closed,
                consecutive_failures: 0,
                consecutive_successes: 0,
                last_failure_time: None,
            }),
            total_requests: AtomicU64::new(0),
            rejected_requests: AtomicU64::new(0),
            total_failures: AtomicU64::new(0),
            total_successes: AtomicU64::new(0),
            state_transitions: AtomicU64::new(0),
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }

    /// Acquire read lock, recovering from poison if necessary
    fn read_internal(&self) -> RwLockReadGuard<'_, InternalState> {
        match self.internal.read() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!(
                    breaker = self.name(),
                    "Circuit breaker read lock poisoned, recovering"
                );
                poisoned.into_inner()
            }
        }
    }

    /// Acquire write lock, recovering from poison if necessary
    fn write_internal(&self) -> RwLockWriteGuard<'_, InternalState> {
        match self.internal.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!(
                    breaker = self.name(),
                    "Circuit breaker write lock poisoned, recovering"
                );
                poisoned.into_inner()
            }
        }
    }

    /// Get the current circuit state
    pub fn state(&self) -> CircuitState {
        let internal = self.read_internal();
        self.compute_effective_state(&internal)
    }

    /// Check if a request should be allowed through
    ///
    /// Returns true if the circuit is closed or half-open (allowing probe).
    /// Returns false if the circuit is open.
    pub fn allow_request(&self) -> bool {
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        let mut internal = self.write_internal();
        let effective_state = self.compute_effective_state(&internal);

        match effective_state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                self.rejected_requests.fetch_add(1, Ordering::Relaxed);
                false
            }
            CircuitState::HalfOpen => {
                // Transition to half-open if we were open
                if internal.state == CircuitState::Open {
                    self.transition_to(&mut internal, CircuitState::HalfOpen);
                }
                true
            }
        }
    }

    /// Record a successful operation
    ///
    /// In half-open state, enough successes will close the circuit.
    /// In closed state, resets the failure counter.
    pub fn record_success(&self) {
        self.total_successes.fetch_add(1, Ordering::Relaxed);

        let mut internal = self.write_internal();

        match internal.state {
            CircuitState::HalfOpen => {
                internal.consecutive_successes += 1;
                internal.consecutive_failures = 0;

                if internal.consecutive_successes >= self.config.success_threshold {
                    self.transition_to(&mut internal, CircuitState::Closed);
                }
            }
            CircuitState::Closed => {
                // Reset failure counter on success
                internal.consecutive_failures = 0;
                internal.consecutive_successes += 1;
            }
            CircuitState::Open => {
                // Shouldn't happen - request was blocked
                debug!(
                    breaker = self.name(),
                    "Success recorded while circuit is open (unexpected)"
                );
            }
        }
    }

    /// Record a failed operation
    ///
    /// Increments failure counter. If threshold is exceeded, opens the circuit.
    pub fn record_failure(&self) {
        self.total_failures.fetch_add(1, Ordering::Relaxed);

        let mut internal = self.write_internal();

        internal.consecutive_failures += 1;
        internal.consecutive_successes = 0;
        internal.last_failure_time = Some(Instant::now());

        match internal.state {
            CircuitState::Closed => {
                if internal.consecutive_failures >= self.config.failure_threshold {
                    self.transition_to(&mut internal, CircuitState::Open);
                }
            }
            CircuitState::HalfOpen => {
                // Failed probe - go back to open
                self.transition_to(&mut internal, CircuitState::Open);
            }
            CircuitState::Open => {
                // Already open, just update failure time
            }
        }
    }

    /// Get statistics about the circuit breaker
    pub fn stats(&self) -> CircuitBreakerStats {
        let internal = self.read_internal();

        CircuitBreakerStats {
            state: self.compute_effective_state(&internal),
            consecutive_failures: internal.consecutive_failures,
            consecutive_successes: internal.consecutive_successes,
            total_requests: self.total_requests.load(Ordering::Relaxed),
            rejected_requests: self.rejected_requests.load(Ordering::Relaxed),
            total_failures: self.total_failures.load(Ordering::Relaxed),
            total_successes: self.total_successes.load(Ordering::Relaxed),
            state_transitions: self.state_transitions.load(Ordering::Relaxed),
        }
    }

    /// Force the circuit to a specific state (for testing or manual intervention)
    pub fn force_state(&self, state: CircuitState) {
        let mut internal = self.write_internal();
        self.transition_to(&mut internal, state);
    }

    /// Reset the circuit breaker to initial state
    pub fn reset(&self) {
        let mut internal = self.write_internal();
        internal.state = CircuitState::Closed;
        internal.consecutive_failures = 0;
        internal.consecutive_successes = 0;
        internal.last_failure_time = None;

        info!(
            breaker = self.name(),
            "Circuit breaker reset to closed state"
        );
    }

    /// Get the name of this circuit breaker
    fn name(&self) -> &str {
        self.config.name.as_deref().unwrap_or("default")
    }

    /// Compute the effective state, considering timeout for Open -> HalfOpen
    fn compute_effective_state(&self, internal: &InternalState) -> CircuitState {
        if internal.state == CircuitState::Open {
            if let Some(last_failure) = internal.last_failure_time {
                let elapsed = last_failure.elapsed();
                let timeout = Duration::from_millis(self.config.reset_timeout_ms);

                if elapsed >= timeout {
                    // Timeout expired, should transition to half-open
                    return CircuitState::HalfOpen;
                }
            }
        }
        internal.state
    }

    /// Transition to a new state with logging
    fn transition_to(&self, internal: &mut InternalState, new_state: CircuitState) {
        let old_state = internal.state;
        if old_state != new_state {
            internal.state = new_state;
            internal.consecutive_successes = 0;
            self.state_transitions.fetch_add(1, Ordering::Relaxed);

            match new_state {
                CircuitState::Open => {
                    warn!(
                        breaker = self.name(),
                        from = %old_state,
                        to = %new_state,
                        consecutive_failures = internal.consecutive_failures,
                        reset_timeout_ms = self.config.reset_timeout_ms,
                        "Circuit breaker opened - requests will be blocked"
                    );
                }
                CircuitState::HalfOpen => {
                    info!(
                        breaker = self.name(),
                        from = %old_state,
                        to = %new_state,
                        "Circuit breaker entering half-open state - testing recovery"
                    );
                }
                CircuitState::Closed => {
                    info!(
                        breaker = self.name(),
                        from = %old_state,
                        to = %new_state,
                        "Circuit breaker closed - normal operation resumed"
                    );
                }
            }
        }
    }
}

impl std::fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stats = self.stats();
        f.debug_struct("CircuitBreaker")
            .field("name", &self.name())
            .field("state", &stats.state)
            .field("consecutive_failures", &stats.consecutive_failures)
            .field("config", &self.config)
            .finish()
    }
}

/// Statistics about circuit breaker operation
#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub total_requests: u64,
    pub rejected_requests: u64,
    pub total_failures: u64,
    pub total_successes: u64,
    pub state_transitions: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_initial_state() {
        let breaker = CircuitBreaker::with_defaults();
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_allows_requests_when_closed() {
        let breaker = CircuitBreaker::with_defaults();

        for _ in 0..10 {
            assert!(breaker.allow_request());
            breaker.record_success();
        }

        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_opens_after_failures() {
        let config = CircuitBreakerConfig::new(3, 1000);
        let breaker = CircuitBreaker::new(config);

        // 3 failures should open the circuit
        for _ in 0..3 {
            assert!(breaker.allow_request());
            breaker.record_failure();
        }

        assert_eq!(breaker.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_blocks_when_open() {
        let config = CircuitBreakerConfig::new(2, 60_000); // Long timeout
        let breaker = CircuitBreaker::new(config);

        // Open the circuit
        breaker.allow_request();
        breaker.record_failure();
        breaker.allow_request();
        breaker.record_failure();

        assert_eq!(breaker.state(), CircuitState::Open);

        // Requests should be blocked
        assert!(!breaker.allow_request());
        assert!(!breaker.allow_request());

        let stats = breaker.stats();
        assert_eq!(stats.rejected_requests, 2);
    }

    #[test]
    fn test_circuit_breaker_half_open_after_timeout() {
        let config = CircuitBreakerConfig::new(2, 10); // 10ms timeout
        let breaker = CircuitBreaker::new(config);

        // Open the circuit
        breaker.allow_request();
        breaker.record_failure();
        breaker.allow_request();
        breaker.record_failure();

        assert_eq!(breaker.state(), CircuitState::Open);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(20));

        // Should be half-open now
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // Should allow one request
        assert!(breaker.allow_request());
    }

    #[test]
    fn test_circuit_breaker_closes_on_half_open_success() {
        let config = CircuitBreakerConfig::new(2, 10).with_success_threshold(2);
        let breaker = CircuitBreaker::new(config);

        // Open the circuit
        breaker.allow_request();
        breaker.record_failure();
        breaker.allow_request();
        breaker.record_failure();

        // Wait for half-open
        std::thread::sleep(Duration::from_millis(20));

        // First success
        assert!(breaker.allow_request());
        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);

        // Second success should close
        assert!(breaker.allow_request());
        breaker.record_success();
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_reopens_on_half_open_failure() {
        let config = CircuitBreakerConfig::new(2, 10);
        let breaker = CircuitBreaker::new(config);

        // Open the circuit
        breaker.allow_request();
        breaker.record_failure();
        breaker.allow_request();
        breaker.record_failure();

        // Wait for half-open
        std::thread::sleep(Duration::from_millis(20));

        // Probe request
        assert!(breaker.allow_request());
        breaker.record_failure(); // Probe failed

        assert_eq!(breaker.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let config = CircuitBreakerConfig::new(2, 60_000);
        let breaker = CircuitBreaker::new(config);

        // Open the circuit
        breaker.allow_request();
        breaker.record_failure();
        breaker.allow_request();
        breaker.record_failure();

        assert_eq!(breaker.state(), CircuitState::Open);

        // Reset
        breaker.reset();

        assert_eq!(breaker.state(), CircuitState::Closed);
        assert!(breaker.allow_request());
    }

    #[test]
    fn test_circuit_breaker_force_state() {
        let breaker = CircuitBreaker::with_defaults();

        breaker.force_state(CircuitState::Open);
        assert_eq!(breaker.state(), CircuitState::Open);

        breaker.force_state(CircuitState::Closed);
        assert_eq!(breaker.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_stats() {
        let config = CircuitBreakerConfig::new(5, 1000);
        let breaker = CircuitBreaker::new(config);

        // Some operations
        breaker.allow_request();
        breaker.record_success();
        breaker.allow_request();
        breaker.record_failure();
        breaker.allow_request();
        breaker.record_success();

        let stats = breaker.stats();
        assert_eq!(stats.total_requests, 3);
        assert_eq!(stats.total_successes, 2);
        assert_eq!(stats.total_failures, 1);
        assert_eq!(stats.rejected_requests, 0);
        assert_eq!(stats.state, CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_with_name() {
        let config = CircuitBreakerConfig::new(3, 1000).with_name("gelf-tcp");
        let breaker = CircuitBreaker::new(config);

        assert_eq!(breaker.name(), "gelf-tcp");
    }

    #[test]
    fn test_success_resets_failure_counter() {
        let config = CircuitBreakerConfig::new(3, 1000);
        let breaker = CircuitBreaker::new(config);

        // Two failures
        breaker.allow_request();
        breaker.record_failure();
        breaker.allow_request();
        breaker.record_failure();

        // One success should reset counter
        breaker.allow_request();
        breaker.record_success();

        // Two more failures shouldn't open (counter was reset)
        breaker.allow_request();
        breaker.record_failure();
        breaker.allow_request();
        breaker.record_failure();

        // Still closed (only 2 consecutive failures)
        assert_eq!(breaker.state(), CircuitState::Closed);

        // Third failure opens it
        breaker.allow_request();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
    }
}
