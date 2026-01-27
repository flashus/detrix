//! Output middleware components
//!
//! Provides composable wrappers for EventOutput implementations.
//! These middleware follow the decorator pattern and can be stacked
//! to add cross-cutting concerns like circuit breaking, timeouts, and batching.
//!
//! # Usage with Builder (Recommended)
//!
//! ```rust,ignore
//! use detrix_output::middleware::{OutputBuilder, EventOutputExt};
//! use detrix_output::{GelfTcpBuilder, CircuitBreakerConfig};
//! use std::time::Duration;
//!
//! // Fluent builder API
//! let output = OutputBuilder::new(GelfTcpBuilder::new("graylog", 12201).build())
//!     .with_timeout(Duration::from_secs(10))
//!     .with_circuit_breaker(CircuitBreakerConfig::new(5, 30_000))
//!     .build();
//!
//! // Or using extension trait
//! let output = GelfTcpBuilder::new("graylog", 12201).build()
//!     .with_middleware()
//!     .with_circuit_breaker(CircuitBreakerConfig::new(5, 30_000))
//!     .build();
//! ```
//!
//! # Manual Composition
//!
//! ```rust,ignore
//! use detrix_output::middleware::{CircuitBreakerOutput, TimeoutOutput};
//!
//! let tcp = GelfTcpBuilder::new("graylog", 12201).build();
//! let with_timeout = TimeoutOutput::with_timeout(tcp, Duration::from_secs(10));
//! let with_cb = CircuitBreakerOutput::new(with_timeout, CircuitBreakerConfig::new(5, 30_000));
//! ```

mod batching;
mod builder;
mod circuit_breaker;
mod timeout;

#[cfg(test)]
mod test_support;

pub use batching::{BatchingConfig, BatchingOutput};
pub use builder::{EventOutputExt, OutputBuilder};
pub use circuit_breaker::CircuitBreakerOutput;
pub use timeout::{TimeoutConfig, TimeoutOutput};
