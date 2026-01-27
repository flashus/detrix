//! Detrix Output Adapters
//!
//! Implements event output destinations (Graylog/GELF, etc.)
//! following Clean Architecture - these adapters implement
//! the `EventOutput` port trait from `detrix-ports`.
//!
//! # Features
//!
//! - **GELF Output**: TCP, UDP, and HTTP transports for Graylog
//! - **Circuit Breaker**: Prevents cascading failures
//! - **Middleware**: Composable wrappers (circuit breaker, timeout, batching)
//! - **Composite**: Fan-out to multiple outputs

pub mod circuit_breaker;
pub mod composite;
pub mod error;
pub mod gelf;
pub mod middleware;

pub use circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerStats, CircuitState,
};
pub use composite::CompositeOutput;
pub use gelf::{
    GelfHttpBuilder, GelfHttpConfig, GelfHttpOutput, GelfLevel, GelfMessage, GelfMessageExt,
    GelfTcpBuilder, GelfTcpConfig, GelfTcpOutput, GelfUdpBuilder, GelfUdpConfig, GelfUdpOutput,
    RoutedGelfBuilder, RoutedGelfOutput, RoutedGelfTcpOutput, RouterConfig, StreamRule,
};
pub use middleware::{
    BatchingConfig, BatchingOutput, CircuitBreakerOutput, EventOutputExt, OutputBuilder,
    TimeoutConfig, TimeoutOutput,
};
