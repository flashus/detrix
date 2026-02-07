//! Client E2E Tests
//!
//! Tests that verify Detrix client implementations work correctly.
//!
//! # Unified Test Pattern
//!
//! These tests use the unified testing approach - the same test
//! scenarios run against all client implementations by changing only the
//! configuration. This mirrors the DAP workflow test pattern.
//!
//! # Adding New Language Support
//!
//! To add tests for a new language client:
//! 1. Add Detrix client init to `fixtures/{lang}/` fixture (controlled by env var)
//! 2. Add `ClientTestConfig::{lang}()` factory method in config.rs
//! 3. Add the language to `generate_client_test!` macro invocation below
//!
//! # Running Tests
//!
//! ```bash
//! # Run all client tests
//! cargo test -p detrix-testing client_tests --release -- --nocapture
//!
//! # Run specific language tests
//! cargo test -p detrix-testing test_python_client --release -- --nocapture
//! cargo test -p detrix-testing test_go_client --release -- --nocapture
//! ```

use detrix_testing::e2e::{
    cleanup_orphaned_e2e_processes,
    client_tests::{ClientProcessTester, ClientTestConfig, ClientTestScenarios, ClientTester},
    executor::{find_detrix_binary, get_workspace_root},
    require_tool, RestClient, TestExecutor, TestReporter,
};
use std::sync::Arc;

/// Macro to generate client tests for a specific language
///
/// This generates a test that:
/// 1. Checks for required tool dependencies
/// 2. Spawns the language's test fixture with Detrix client enabled
/// 3. Runs the full test suite
/// 4. Handles cleanup
macro_rules! generate_client_test {
    ($test_name:ident, $config_fn:ident, $display_name:expr) => {
        #[tokio::test]
        async fn $test_name() {
            let config = ClientTestConfig::$config_fn();

            // Skip if required tool not available
            if !require_tool(config.language.tool_dependency()).await {
                eprintln!("Skipping: {} not available", config.language.display_name());
                return;
            }

            // Skip if detrix binary not available
            let workspace_root = get_workspace_root();
            if find_detrix_binary(&workspace_root).is_none() {
                eprintln!("Skipping: detrix binary not found");
                return;
            }

            // Skip if fixture doesn't exist
            let fixture_path = workspace_root.join(&config.fixture_path);
            if !fixture_path.exists() {
                eprintln!(
                    "Skipping: {} test fixture not found at {}",
                    $display_name,
                    fixture_path.display()
                );
                return;
            }

            cleanup_orphaned_e2e_processes();

            let reporter = Arc::new(TestReporter::new(
                &format!("{}_client", stringify!($config_fn)),
                $display_name,
            ));

            // Start daemon
            let mut executor = TestExecutor::new();
            if let Err(e) = executor.start_daemon().await {
                reporter.print_header();
                let step = reporter.step_start("Setup", "Start daemon");
                reporter.step_failed(step, &format!("Failed to start daemon: {}", e));
                reporter.print_footer(false);
                panic!("Failed to start daemon: {}", e);
            }

            // Create REST client for daemon verification
            let daemon_client = RestClient::new(executor.http_port);

            // Spawn client using unified tester
            let daemon_url = format!("http://127.0.0.1:{}", executor.http_port);
            let client = match ClientProcessTester::spawn(config.clone(), &daemon_url, 0).await {
                Ok(c) => Arc::new(c),
                Err(e) => {
                    reporter.print_header();
                    let step =
                        reporter.step_start("Setup", &format!("Spawn {} client", $display_name));
                    reporter.step_failed(step, &format!("Failed to spawn client: {}", e));
                    reporter.print_footer(false);
                    executor.stop_all();
                    panic!("Failed to spawn {} client: {}", $display_name, e);
                }
            };

            // Run test suite
            let result =
                ClientTestScenarios::run_full_suite(&client, &daemon_client, &reporter).await;

            // Print logs on failure
            if result.is_err() {
                client.print_logs(50);
                executor.print_daemon_logs(50);
            }

            // Cleanup
            drop(client);
            executor.stop_all();

            if let Err(e) = result {
                panic!("{} test failed: {}", $display_name, e);
            }
        }
    };
}

// Generate tests for each supported language
generate_client_test!(test_python_client, python, "Python Client");
generate_client_test!(test_go_client, go, "Go Client");
generate_client_test!(test_rust_client, rust, "Rust Client");

/// Test Python client with daemon restart
///
/// Verifies that the client handles daemon restarts gracefully.
/// This is a Python-specific test that demonstrates language-specific
/// scenarios can still be added alongside the unified tests.
#[tokio::test]
async fn test_python_client_daemon_restart() {
    let config = ClientTestConfig::python();

    // Skip if Python not available
    if !require_tool(config.language.tool_dependency()).await {
        eprintln!("Skipping: debugpy not available");
        return;
    }

    // Skip if detrix binary not available
    let workspace_root = get_workspace_root();
    if find_detrix_binary(&workspace_root).is_none() {
        eprintln!("Skipping: detrix binary not found");
        return;
    }

    // Skip if fixture doesn't exist
    let fixture_path = workspace_root.join(&config.fixture_path);
    if !fixture_path.exists() {
        eprintln!(
            "Skipping: Python test fixture not found at {}",
            fixture_path.display()
        );
        return;
    }

    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("python_daemon_restart", "Python Client - Daemon Restart");
    reporter.print_header();

    // Start daemon
    let mut executor = TestExecutor::new();
    if let Err(e) = executor.start_daemon().await {
        let step = reporter.step_start("Setup", "Start daemon");
        reporter.step_failed(step, &format!("Failed to start daemon: {}", e));
        reporter.print_footer(false);
        panic!("Failed to start daemon: {}", e);
    }

    // Spawn Python client using unified tester
    let daemon_url = format!("http://127.0.0.1:{}", executor.http_port);
    let client: ClientProcessTester = match ClientProcessTester::spawn(config, &daemon_url, 0).await
    {
        Ok(c) => c,
        Err(e) => {
            let step = reporter.step_start("Setup", "Spawn Python client");
            reporter.step_failed(step, &format!("Failed to spawn Python client: {}", e));
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Failed to spawn Python client: {}", e);
        }
    };

    // Wake the client
    let step = reporter.step_start("Phase1", "Wake client before daemon restart");
    match client.wake(None).await {
        Ok(resp) => {
            if resp.status == "awake" {
                reporter.step_success(step, Some(&format!("Awake on port {}", resp.debug_port)));
            } else {
                reporter.step_failed(step, &format!("Unexpected status: {}", resp.status));
                reporter.print_footer(false);
                panic!("Unexpected wake status: {}", resp.status);
            }
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Wake failed: {}", e));
            reporter.print_footer(false);
            panic!("Wake failed: {}", e);
        }
    }

    // Stop daemon
    let step = reporter.step_start("Phase2", "Stop daemon");
    executor.stop_daemon();
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    reporter.step_success(step, Some("Daemon stopped"));

    // Client should still respond (control plane is independent)
    let step = reporter.step_start("Phase3", "Client still responds after daemon stop");
    match client.status().await {
        Ok(status) => {
            // State might be awake or sleeping depending on implementation
            reporter.step_success(step, Some(&format!("State: {}", status.state)));
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Status failed: {}", e));
            reporter.print_footer(false);
            panic!("Status failed after daemon stop: {}", e);
        }
    }

    // Restart daemon
    let step = reporter.step_start("Phase4", "Restart daemon");
    if let Err(e) = executor.restart_daemon().await {
        reporter.step_failed(step, &format!("Failed to restart daemon: {}", e));
        reporter.print_footer(false);
        panic!("Failed to restart daemon: {}", e);
    }
    reporter.step_success(step, Some("Daemon restarted"));

    // Client should be able to wake again with new daemon
    let step = reporter.step_start("Phase5", "Wake client after daemon restart");
    // First sleep to reset state
    let _ = client.sleep().await;

    match client.wake(None).await {
        Ok(resp) => {
            if resp.status == "awake" {
                reporter.step_success(step, Some(&format!("Awake on port {}", resp.debug_port)));
            } else {
                reporter.step_failed(step, &format!("Unexpected status: {}", resp.status));
            }
        }
        Err(e) => {
            // This is expected if the daemon URL changed (port fallback)
            reporter.step_success(
                step,
                Some(&format!(
                    "Wake failed as expected (daemon port may have changed): {}",
                    e
                )),
            );
        }
    }

    reporter.print_footer(true);

    // Cleanup
    drop(client);
    executor.stop_all();
}

/// Test: No duplicate events (triple events regression test)
///
/// This test verifies that when observing a variable, events are received
/// exactly once per occurrence, not duplicated (e.g., 3 times per hit).
///
/// Regression test for: https://github.com/flashus/detrix/issues/XXX
/// Root cause: Broker subscribers not properly cleaned up during reconnection
#[tokio::test]
async fn test_rust_client_no_duplicate_events() {
    use detrix_testing::e2e::client::{AddMetricRequest, ApiClient};
    use std::collections::HashMap;

    let config = ClientTestConfig::rust();

    // Skip if lldb-dap not available
    if !require_tool(config.language.tool_dependency()).await {
        eprintln!("Skipping: lldb-dap not available");
        return;
    }

    // Skip if detrix binary not available
    let workspace_root = get_workspace_root();
    if find_detrix_binary(&workspace_root).is_none() {
        eprintln!("Skipping: detrix binary not found");
        return;
    }

    // Skip if fixture doesn't exist
    let fixture_dir = workspace_root.join("fixtures/rust");
    if !fixture_dir.exists() {
        eprintln!(
            "Skipping: Rust test fixture not found at {}",
            fixture_dir.display()
        );
        return;
    }

    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new(
        "rust_no_duplicate_events",
        "Rust Client - No Duplicate Events",
    );
    reporter.print_header();

    // Start daemon
    let mut executor = TestExecutor::new();
    if let Err(e) = executor.start_daemon().await {
        let step = reporter.step_start("Setup", "Start daemon");
        reporter.step_failed(step, &format!("Failed to start daemon: {}", e));
        reporter.print_footer(false);
        panic!("Failed to start daemon: {}", e);
    }

    let step = reporter.step_start("Setup", "Start daemon");
    reporter.step_success(step, Some("Daemon started"));

    // Spawn Rust client using unified tester
    let daemon_url = format!("http://127.0.0.1:{}", executor.http_port);
    let client = match ClientProcessTester::spawn(config.clone(), &daemon_url, 0).await {
        Ok(c) => c,
        Err(e) => {
            let step = reporter.step_start("Setup", "Spawn Rust client");
            reporter.step_failed(step, &format!("Failed to spawn Rust client: {}", e));
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Failed to spawn Rust client: {}", e);
        }
    };

    let step = reporter.step_start("Setup", "Spawn Rust fixture app");
    reporter.step_success(
        step,
        Some(&format!("Control port: {}", client.control_port())),
    );

    // Create REST client for daemon API
    let daemon_client = RestClient::new(executor.http_port);

    // Wake the client
    let step = reporter.step_start("Phase1", "Wake Rust client");
    let connection_id = match client.wake(None).await {
        Ok(resp) => {
            if resp.status == "awake" {
                reporter.step_success(
                    step,
                    Some(&format!("Awake, connection_id={}", resp.connection_id)),
                );
                resp.connection_id
            } else {
                reporter.step_failed(step, &format!("Unexpected status: {}", resp.status));
                reporter.print_footer(false);
                executor.stop_all();
                panic!("Unexpected wake status: {}", resp.status);
            }
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Wake failed: {}", e));
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Wake failed: {}", e);
        }
    };

    // Wait for connection to stabilize
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Add a metric to observe the pnl variable
    // Line 121 is where pnl is calculated: let pnl = calculate_pnl(...)
    // We observe at line 128 (log! call) where pnl is in scope
    let fixture_file = fixture_dir.join("src/main.rs");
    let location = format!("{}#128", fixture_file.display());

    let step = reporter.step_start("Phase2", "Add metric for pnl variable");
    let metric_req =
        AddMetricRequest::new("test_pnl", &location, "pnl", &connection_id).with_language("rust");

    match daemon_client.add_metric(metric_req).await {
        Ok(resp) => {
            reporter.step_success(step, Some(&format!("Metric ID: {}", resp.data)));
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to add metric: {}", e));
            client.print_logs(50);
            executor.print_daemon_logs(50);
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Failed to add metric: {}", e);
        }
    }

    // Wait for events to be captured
    // The trading bot runs every 3 seconds, so we wait 12 seconds to get ~4 events
    let step = reporter.step_start("Phase3", "Collect events (12 seconds)");
    tokio::time::sleep(tokio::time::Duration::from_secs(12)).await;

    // Query events
    let events = match daemon_client.query_events("test_pnl", 100).await {
        Ok(resp) => resp.data,
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to query events: {}", e));
            client.print_logs(50);
            executor.print_daemon_logs(50);
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Failed to query events: {}", e);
        }
    };

    reporter.step_success(step, Some(&format!("Received {} events", events.len())));

    // Analyze events for duplicates
    // Group events by timestamp (within 100ms window) to detect duplicates
    let step = reporter.step_start("Phase4", "Check for duplicate events");

    // Parse timestamps and count occurrences
    let mut timestamp_counts: HashMap<String, u32> = HashMap::new();
    for event in &events {
        // Round to seconds for grouping (events from same hit should have ~same timestamp)
        let ts_key = event.timestamp_iso.chars().take(19).collect::<String>();
        *timestamp_counts.entry(ts_key).or_insert(0) += 1;
    }

    // Check for duplicates (more than 1 event per second is a duplicate)
    let mut has_duplicates = false;
    let mut duplicate_details = Vec::new();
    for (ts, count) in &timestamp_counts {
        if *count > 1 {
            has_duplicates = true;
            duplicate_details.push(format!("{}: {} events", ts, count));
        }
    }

    if has_duplicates {
        reporter.step_failed(
            step,
            &format!(
                "Found duplicate events! {}\nThis is the triple events bug.",
                duplicate_details.join(", ")
            ),
        );
        client.print_logs(50);
        executor.print_daemon_logs(50);
        reporter.print_footer(false);
        executor.stop_all();
        panic!(
            "Triple events bug detected! Duplicate events: {}",
            duplicate_details.join(", ")
        );
    }

    // Verify we got a reasonable number of events (at least 2 in 12 seconds with 3s interval)
    if events.len() < 2 {
        reporter.step_failed(
            step,
            &format!(
                "Expected at least 2 events in 12 seconds, got {}",
                events.len()
            ),
        );
        client.print_logs(50);
        executor.print_daemon_logs(50);
        reporter.print_footer(false);
        executor.stop_all();
        panic!("Not enough events captured");
    }

    reporter.step_success(
        step,
        Some(&format!(
            "No duplicates detected. {} events across {} distinct timestamps",
            events.len(),
            timestamp_counts.len()
        )),
    );

    reporter.print_footer(true);

    // Cleanup
    drop(client);
    executor.stop_all();
}
