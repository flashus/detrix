//! Common test infrastructure for DAP adapter integration tests
//!
//! This module provides:
//! - `DapTestFixture` trait for language-specific test fixtures
//! - `dap_standard_tests!` macro for generating standard test suite
//! - Common utilities (port allocation, wait helpers)
//! - Test process cleanup to handle orphaned debugger processes
//!
//! # Adding a New Language Adapter
//!
//! 1. Create `{language}_tests.rs` in the tests directory
//! 2. Implement `DapTestFixture` trait for your language
//! 3. Use `dap_standard_tests!(YourFixture);` to generate standard tests
//! 4. Add language-specific tests as regular test functions
//!
//! # Example
//!
//! ```ignore
//! mod common;
//! use common::{DapTestFixture, dap_standard_tests};
//!
//! struct RustFixture;
//!
//! #[async_trait::async_trait]
//! impl DapTestFixture for RustFixture {
//!     fn adapter_name() -> &'static str { "rust" }
//!     // ... implement other methods
//! }
//!
//! dap_standard_tests!(RustFixture);
//!
//! // Language-specific tests
//! #[tokio::test]
//! async fn test_rust_specific_feature() { ... }
//! ```

use async_trait::async_trait;
use detrix_core::entities::{Location, MetricId, MetricMode, SafetyLevel};
use detrix_core::{Metric, SourceLanguage};
use detrix_dap::AdapterConfig;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tokio::process::Child;

// Re-export cleanup functions from detrix-testing to avoid duplication
pub use detrix_testing::e2e::{
    cleanup_orphaned_e2e_processes as cleanup_orphaned_processes,
    register_e2e_process as register_test_process,
    unregister_e2e_process as unregister_test_process,
};

// Use atomic counter to avoid port conflicts between tests
// DAP tests use port range 15000+ to avoid conflicts with E2E tests
static PORT_COUNTER: AtomicU16 = AtomicU16::new(15000);

/// Check if a port is available for binding
fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Get a unique port for testing that is actually available.
/// Skips ports that are in use (e.g., from orphaned debugger processes).
/// Also triggers cleanup of orphaned processes on first call.
pub fn get_test_port() -> u16 {
    // Ensure orphaned processes are cleaned up
    cleanup_orphaned_processes();

    const MAX_ATTEMPTS: u16 = 100;

    for _ in 0..MAX_ATTEMPTS {
        let port = PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
        if is_port_available(port) {
            return port;
        }
        // Port in use, try next one
    }

    // Fallback: return next port anyway and let the test fail with a clear error
    PORT_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Wait for a port to be listening
/// Uses lsof instead of TcpStream::connect to avoid consuming single-connection debuggers
pub async fn wait_for_port(port: u16, timeout_secs: u64) -> bool {
    let start = std::time::Instant::now();
    let timeout_duration = Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout_duration {
        let output = std::process::Command::new("lsof")
            .args(["-i", &format!(":{}", port), "-sTCP:LISTEN"])
            .output();

        if let Ok(out) = output {
            if out.status.success() && !out.stdout.is_empty() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// Trait for language-specific DAP test fixtures
///
/// Implement this trait to enable parameterized testing for a new language adapter.
/// The `dap_standard_tests!` macro will generate a standard test suite using this fixture.
#[allow(dead_code)]
#[async_trait]
pub trait DapTestFixture: Send + Sync + 'static {
    /// Name of the adapter (e.g., "python", "go", "rust")
    fn adapter_name() -> &'static str;

    /// Language of the adapter as SourceLanguage enum
    fn adapter_language() -> SourceLanguage;

    /// Check if the debugger is available on the system
    async fn is_available() -> bool;

    /// Start the debug server on the given port
    /// Returns the child process handle
    async fn start_debug_server(port: u16, script_path: &str) -> Result<Child, std::io::Error>;

    /// Create adapter configuration for the given port
    fn create_adapter_config(port: u16) -> AdapterConfig;

    /// Get sample script content for testing
    /// The script should:
    /// - Have a function that can be logpointed
    /// - Run long enough for tests to complete
    fn sample_script() -> &'static str;

    /// Get the file extension for this language (e.g., "py", "go", "rs")
    fn file_extension() -> &'static str;

    /// Get the line number where a logpoint should be set in the sample script
    fn logpoint_line() -> u32;

    /// Get a test expression for this language
    fn test_expression() -> &'static str;

    /// Create a test metric for this language
    fn create_test_metric(name: &str, line: u32, expression: &str) -> Metric {
        Metric {
            id: Some(MetricId(1)),
            name: name.to_string(),
            connection_id: detrix_core::ConnectionId::from("test"),
            group: None,
            location: Location {
                file: format!("test_script.{}", Self::file_extension()),
                line,
            },
            expressions: vec![expression.to_string()],
            language: Self::adapter_language(),
            enabled: true,
            mode: MetricMode::Stream,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: Default::default(),
        }
    }

    /// Get timeout for waiting for debug server to start (seconds)
    fn server_start_timeout() -> u64 {
        15
    }

    /// Get timeout for connection attempts (seconds)
    fn connection_timeout() -> u64 {
        5
    }

    /// Whether the adapter supports the standard connection test
    ///
    /// Some adapters (like Delve for Go) require a launch request to start a debug session,
    /// which isn't fully implemented yet. Override this to return false to skip the
    /// standard connection test.
    fn supports_standard_connection_test() -> bool {
        true
    }

    /// Cleanup after test (remove temp files, etc.)
    fn cleanup(script_path: &PathBuf) {
        std::fs::remove_file(script_path).ok();
    }
}

/// Macro to generate standard DAP adapter tests
///
/// This macro generates a comprehensive test suite for any adapter that
/// implements the `DapTestFixture` trait.
///
/// # Generated Tests
///
/// - `test_connection` - Connect to running debugger
/// - `test_connection_timeout` - Fail gracefully on unused port
/// - `test_adapter_state_transitions` - Verify state machine works
/// - `test_adapter_stop_idempotent` - Stop can be called multiple times
/// - `test_is_ready_state` - is_ready() returns correct state
///
/// # Usage
///
/// ```ignore
/// dap_standard_tests!(PythonFixture);
/// ```
/// Internal module for macro-generated tests
/// This avoids import conflicts when tests use their own imports
#[allow(unused_imports)]
pub mod generated_tests {
    pub use super::{
        get_test_port, register_test_process, unregister_test_process, wait_for_port,
        DapTestFixture,
    };
    pub use detrix_dap::{AdapterProcess, AdapterState};
    pub use detrix_testing::e2e::record_missing;
    pub use std::time::Duration;
    pub use tokio::time::timeout;
}

#[macro_export]
macro_rules! dap_standard_tests {
    ($fixture:ty) => {
        /// Test: Connection to running debugger (generated by dap_standard_tests!)
        #[tokio::test]
        async fn test_std_connection() {
            use $crate::common::generated_tests::*;

            if !<$fixture as DapTestFixture>::supports_standard_connection_test() {
                eprintln!(
                    "Skipping test: {} does not support standard connection test (requires launch mode)",
                    <$fixture as DapTestFixture>::adapter_name()
                );
                return;
            }

            if !<$fixture as DapTestFixture>::is_available().await {
                let name = <$fixture as DapTestFixture>::adapter_name();
                record_missing(name);
                eprintln!(
                    "\n\x1b[33m⚠ SKIPPING TEST: {} not available\x1b[0m\n",
                    name
                );
                return;
            }

            let port = get_test_port();
            eprintln!(
                "Using port {} for {} test",
                port,
                <$fixture as DapTestFixture>::adapter_name()
            );

            // Create temp script
            let script_path = std::env::temp_dir().join(format!(
                "detrix_test_{}.{}",
                port,
                <$fixture as DapTestFixture>::file_extension()
            ));
            std::fs::write(&script_path, <$fixture as DapTestFixture>::sample_script()).unwrap();
            eprintln!("Created test script at {:?}", script_path);

            // Start debug server
            let mut debug_process =
                match <$fixture as DapTestFixture>::start_debug_server(port, script_path.to_str().unwrap()).await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("Failed to start debug server: {:?}", e);
                        <$fixture as DapTestFixture>::cleanup(&script_path);
                        return;
                    }
                };

            // Register process for cleanup tracking
            let debug_pid = debug_process.id().unwrap_or(0);
            let adapter_name = <$fixture as DapTestFixture>::adapter_name();
            if debug_pid > 0 {
                register_test_process(adapter_name, debug_pid);
            }

            // Wait for server to be ready
            if !wait_for_port(port, <$fixture as DapTestFixture>::server_start_timeout()).await {
                if let Ok(Some(status)) = debug_process.try_wait() {
                    eprintln!("Debug server exited with status: {:?}", status);
                }
                debug_process.kill().await.ok();
                if debug_pid > 0 {
                    unregister_test_process(adapter_name, debug_pid);
                }
                <$fixture as DapTestFixture>::cleanup(&script_path);
                panic!(
                    "{} did not start listening on port {}",
                    <$fixture as DapTestFixture>::adapter_name(),
                    port
                );
            }
            eprintln!("{} is listening on port {}", <$fixture as DapTestFixture>::adapter_name(), port);

            // Connect using adapter
            let config = <$fixture as DapTestFixture>::create_adapter_config(port);
            let adapter = AdapterProcess::new(config);

            let result = adapter.start().await;

            // Cleanup
            adapter.stop().await.ok();
            debug_process.kill().await.ok();
            if debug_pid > 0 {
                unregister_test_process(adapter_name, debug_pid);
            }
            <$fixture as DapTestFixture>::cleanup(&script_path);

            assert!(
                result.is_ok(),
                "Failed to connect to {}: {:?}",
                <$fixture as DapTestFixture>::adapter_name(),
                result
            );

            // Verify capabilities
            let caps = adapter.capabilities().await;
            assert!(caps.is_some(), "Should have received capabilities");
            let caps = caps.unwrap();
            assert!(
                caps.supports_log_points.unwrap_or(false),
                "{} should support logpoints",
                <$fixture as DapTestFixture>::adapter_name()
            );

            eprintln!("{} connection test passed!", <$fixture as DapTestFixture>::adapter_name());
        }

        /// Test: Connection timeout on unused port (generated by dap_standard_tests!)
        #[tokio::test]
        async fn test_std_connection_timeout() {
            use $crate::common::generated_tests::*;

            // Use a high port that's unlikely to have anything listening
            let port = 65400 + (get_test_port() % 100);

            let config = <$fixture as DapTestFixture>::create_adapter_config(port);
            let adapter = AdapterProcess::new(config);

            let result = timeout(
                Duration::from_secs(<$fixture as DapTestFixture>::connection_timeout()),
                adapter.start(),
            )
            .await;

            match result {
                Ok(inner_result) => {
                    assert!(
                        inner_result.is_err(),
                        "Expected connection to fail on unused port"
                    );
                }
                Err(_) => {
                    // Timeout is also acceptable
                }
            }
        }

        /// Test: Adapter state transitions (generated by dap_standard_tests!)
        #[tokio::test]
        async fn test_std_adapter_state_transitions() {
            use $crate::common::generated_tests::*;

            let config = <$fixture as DapTestFixture>::create_adapter_config(65499);
            let adapter = AdapterProcess::new(config);

            // Initial state
            assert_eq!(adapter.state().await, AdapterState::Stopped);

            // After failed start (no server listening)
            let _ = adapter.start().await;
            let state = adapter.state().await;
            assert!(
                state == AdapterState::Failed || state == AdapterState::Stopped,
                "State should be Failed or Stopped after failed connection"
            );
        }

        /// Test: Stop is idempotent (generated by dap_standard_tests!)
        #[tokio::test]
        async fn test_std_adapter_stop_idempotent() {
            use $crate::common::generated_tests::*;

            let config = <$fixture as DapTestFixture>::create_adapter_config(65498);
            let adapter = AdapterProcess::new(config);

            // Stop when already stopped should be ok
            assert!(adapter.stop().await.is_ok());
            assert!(adapter.stop().await.is_ok());
            assert!(adapter.stop().await.is_ok());

            assert_eq!(adapter.state().await, AdapterState::Stopped);
        }

        /// Test: is_ready() returns correct state (generated by dap_standard_tests!)
        #[tokio::test]
        async fn test_std_is_ready_state() {
            use $crate::common::generated_tests::*;

            let config = <$fixture as DapTestFixture>::create_adapter_config(65497);
            let adapter = AdapterProcess::new(config);

            assert!(!adapter.is_ready(), "Should not be ready when stopped");
        }
    };
}
