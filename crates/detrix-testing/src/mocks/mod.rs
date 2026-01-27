//! Mock implementations for testing
//!
//! These mocks implement the port traits from `detrix-ports` and can be used
//! in unit and integration tests.
//!
//! # Adapter Mocks
//!
//! - [`MockDapAdapter`] - Simple mock that always succeeds, tracks call counts
//! - [`StatefulMockDapAdapter`] - Full-featured mock with event sending and state tracking
//! - [`MockDapAdapterFactory`] - Factory for creating mock adapters
//!
//! # Repository Mocks
//!
//! - [`MockMetricRepository`] - In-memory metric storage
//! - [`MockEventRepository`] - In-memory event storage
//! - [`MockSystemEventRepository`] - In-memory system event storage
//! - [`MockConnectionRepository`] - In-memory connection storage
//! - [`MockDlqRepository`] - In-memory dead-letter queue storage

mod adapters;
mod repositories;

pub use adapters::{MockDapAdapter, MockDapAdapterFactory, StatefulMockDapAdapter};
pub use repositories::{
    MockConnectionRepository, MockDlqRepository, MockEventRepository, MockMetricRepository,
    MockSystemEventRepository,
};
