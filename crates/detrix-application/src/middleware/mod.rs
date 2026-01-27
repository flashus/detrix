//! Middleware components for the application layer
//!
//! Middleware in this context refers to decorator/wrapper patterns that
//! can be composed around core adapters to add cross-cutting concerns:
//!
//! - **ReconnectingAdapter**: Adds automatic reconnection with exponential backoff
//! - **ReconnectingAdapterFactory**: Factory decorator for automatic middleware wrapping
//! - TODO: **CircuitBreakerAdapter**: Fail-fast on repeated failures
//! - TODO: **MetricsAdapter**: Collect performance metrics
//! - TODO: **LoggingAdapter**: Enhanced logging for debugging

mod reconnecting_adapter;
mod reconnecting_factory;

pub use reconnecting_adapter::{ConnectionState, ReconnectStats, ReconnectingAdapter};
pub use reconnecting_factory::ReconnectingAdapterFactory;
