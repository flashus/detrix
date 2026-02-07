//! Client Tests Module
//!
//! This module provides infrastructure for testing Detrix client implementations.
//! It follows the same unified pattern as DAP workflow tests - a single test
//! framework that works with any language client by changing only the configuration.
//!
//! # Architecture
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │              ClientTestScenarios                            │
//! │  (Language-agnostic test scenarios)                        │
//! └────────────────────────────┬───────────────────────────────┘
//!                              │
//!                     ┌────────┴────────┐
//!                     │ ClientTester    │  (trait)
//!                     └────────┬────────┘
//!                              │
//!                     ┌────────┴────────┐
//!                     │ClientProcessTest│  (unified implementation)
//!                     └────────┬────────┘
//!                              │ uses
//!              ┌───────────────┼───────────────┐
//!              │               │               │
//!       ┌──────▼──────┐ ┌─────▼─────┐ ┌──────▼──────┐
//!       │Config::     │ │Config::   │ │Config::     │
//!       │  python()   │ │  go()     │ │  rust()     │
//!       └─────────────┘ └───────────┘ └─────────────┘
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use detrix_testing::e2e::client_tests::{
//!     ClientTestScenarios, ClientTestConfig, ClientProcessTester, ClientTester,
//! };
//! use detrix_testing::e2e::{TestReporter, RestClient};
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let reporter = TestReporter::new("python_client", "Python");
//! let daemon = RestClient::new(8090);
//!
//! // Spawn Python client using unified tester
//! let config = ClientTestConfig::python();
//! let client = ClientProcessTester::spawn(
//!     config,
//!     "http://127.0.0.1:8090",
//!     0,  // auto-assign port
//! ).await?;
//!
//! // Run test scenarios
//! ClientTestScenarios::initial_state_sleeping(&client, &reporter).await?;
//!
//! // For Go client - just change the config:
//! // let config = ClientTestConfig::go();
//! // let client = ClientProcessTester::spawn(config, ...).await?;
//! # Ok(())
//! # }
//! ```

mod config;
mod process_tester;
mod python_tester;
mod scenarios;

// Unified exports
pub use config::{ClientLanguage, ClientTestConfig};
pub use process_tester::ClientProcessTester;
pub use scenarios::ClientTestScenarios;

// Legacy exports for backward compatibility
pub use config::ClientTestConfig as ClientConformanceConfig;
pub use python_tester::PythonClientTester;
pub use scenarios::ClientTestScenarios as ClientConformanceScenarios;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Client status response from the control plane
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientStatus {
    /// Current state: "sleeping", "waking", or "awake"
    pub state: String,
    /// Connection name
    pub name: String,
    /// Host the control plane is bound to
    pub control_host: String,
    /// Actual port the control plane is listening on
    pub control_port: u16,
    /// Debug adapter port (0 if not started)
    pub debug_port: u16,
    /// Whether the debug port is actually accepting connections
    pub debug_port_active: bool,
    /// URL of the Detrix daemon
    pub daemon_url: String,
    /// Connection ID from daemon registration (null if not registered)
    pub connection_id: Option<String>,
}

/// Response from wake operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeResponse {
    /// "awake" or "already_awake"
    pub status: String,
    /// Debug adapter port
    pub debug_port: u16,
    /// Connection ID from daemon
    pub connection_id: String,
}

/// Response from sleep operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleepResponse {
    /// "sleeping" or "already_sleeping"
    pub status: String,
}

/// Health check response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Should be "ok"
    pub status: String,
    /// Should be "detrix-client"
    pub service: String,
}

/// Trait for testing client implementations
///
/// Any client tester (unified or language-specific) must implement this trait
/// to be testable with the conformance suite.
#[async_trait]
pub trait ClientTester: Send + Sync {
    /// Get client status via control plane
    async fn status(&self) -> Result<ClientStatus, String>;

    /// Call wake on the client
    async fn wake(&self, daemon_url: Option<&str>) -> Result<WakeResponse, String>;

    /// Call sleep on the client
    async fn sleep(&self) -> Result<SleepResponse, String>;

    /// Get client health
    async fn health(&self) -> Result<HealthResponse, String>;

    /// Shutdown the client process
    async fn shutdown(&self) -> Result<(), String>;

    /// Get the control plane port
    fn control_port(&self) -> u16;

    /// Get the configured daemon URL
    fn daemon_url(&self) -> &str;
}
