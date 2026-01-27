//! Test utilities for Detrix
//!
//! This crate provides mocks, fixtures, and helpers for testing Detrix components.
//!
//! # Mocks
//!
//! - [`MockDapAdapter`] - A simple mock DAP adapter that tracks operations
//! - [`StatefulMockDapAdapter`] - Full-featured mock with event sending and state tracking
//! - [`MockDapAdapterFactory`] - A mock factory for creating MockDapAdapter instances
//! - [`MockMetricRepository`] - A mock metric repository (in-memory)
//! - [`MockEventRepository`] - A mock event repository (in-memory)
//! - [`MockSystemEventRepository`] - A mock system event repository (in-memory)
//! - [`MockConnectionRepository`] - A mock connection repository (in-memory)
//! - [`MockDlqRepository`] - A mock dead-letter queue repository (in-memory)
//!
//! # E2E Testing Framework
//!
//! The `e2e` module provides a unified framework for end-to-end API testing:
//!
//! - [`e2e::ApiClient`] - Abstract trait for MCP/gRPC/REST API clients
//! - [`e2e::McpClient`] - MCP (JSON-RPC) client implementation
//! - [`e2e::TestReporter`] - Verbose step-by-step progress reporting
//! - [`e2e::TestExecutor`] - Process management (daemon, debugpy)
//! - [`e2e::TestScenarios`] - Reusable test scenarios
//!
//! # Fixtures
//!
//! - [`fixtures::sample_metric`] - Create a sample metric for testing
//! - [`fixtures::sample_event`] - Create a sample metric event for testing
//!
//! # Usage
//!
//! ```no_run
//! use detrix_testing::{MockDapAdapter, StatefulMockDapAdapter, MockDapAdapterFactory, fixtures};
//! use detrix_ports::DapAdapter;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Simple mock for basic tests
//! let adapter = MockDapAdapter::new();
//!
//! // Full-featured mock for event simulation
//! let stateful = StatefulMockDapAdapter::new();
//! stateful.start().await?;
//! assert!(stateful.was_started());
//!
//! let factory = MockDapAdapterFactory::new();
//! let metric = fixtures::sample_metric("test_metric");
//! # Ok(())
//! # }
//! ```

pub mod e2e;
pub mod fixtures;
mod mocks;
pub mod proptest_config;

pub use mocks::{
    MockConnectionRepository, MockDapAdapter, MockDapAdapterFactory, MockDlqRepository,
    MockEventRepository, MockMetricRepository, MockSystemEventRepository, StatefulMockDapAdapter,
};
