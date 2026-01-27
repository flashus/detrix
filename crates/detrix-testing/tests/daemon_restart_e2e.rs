//! Daemon Restart E2E Tests
//!
//! Tests the daemon's ability to automatically reconnect to debuggers
//! after being restarted. This verifies the `reconnect_stored_connections()`
//! functionality.
//!
//! Test flow:
//! 1. Start debugpy with a long-running Python script
//! 2. Start daemon
//! 3. Connect to debugpy
//! 4. Add metrics and capture events
//! 5. Stop daemon (kill process)
//! 6. Wait (debugpy continues running)
//! 7. Start daemon again (new process, same database)
//! 8. Verify daemon auto-reconnects to debugpy
//! 9. Verify events are still being captured

use detrix_testing::e2e::{
    availability::{require_tool, ToolDependency},
    client::{AddMetricRequest, ApiClient},
    executor::TestExecutor,
    reporter::TestReporter,
    rest::RestClient,
};
use std::time::Duration;
use tokio::time::sleep;

/// Test daemon restart with auto-reconnect to debugpy
///
/// This test verifies that when a daemon is restarted:
/// 1. It reconnects to previously connected debuggers automatically
/// 2. Metrics are preserved in the database
/// 3. Events continue to be captured after reconnection
#[tokio::test]
async fn test_daemon_restart_auto_reconnect_python() {
    // Check if debugpy is available
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let reporter = TestReporter::new("daemon_restart_python", "REST");
    reporter.section("DAEMON RESTART AUTO-RECONNECT TEST");
    reporter.info("Testing daemon restart with Python/debugpy");

    let mut executor = TestExecutor::new();

    // ========================================================================
    // PHASE 1: Start debugpy
    // ========================================================================
    reporter.section("PHASE 1: START DEBUGPY");

    // Use trade_bot_forever.py - it runs continuously which is needed for restart testing
    let script_path = executor
        .workspace_root
        .join("fixtures/python/trade_bot_forever.py");
    if !script_path.exists() {
        reporter.error("trade_bot_forever.py not found");
        panic!("Missing Python test fixture: trade_bot_forever.py");
    }

    let step = reporter.step_start("Start debugpy", "Launch Python debugger with test script");
    reporter.info(&format!("Script: {}", script_path.display()));
    reporter.info(&format!("Port: {}", executor.debugpy_port));

    if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
        reporter.step_failed(step, &e);
        executor.print_debugpy_logs(50);
        panic!("Failed to start debugpy: {}", e);
    }
    reporter.step_success(step, Some("debugpy started"));

    // ========================================================================
    // PHASE 2: Start daemon (first time)
    // ========================================================================
    reporter.section("PHASE 2: START DAEMON (FIRST TIME)");

    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    reporter.info(&format!("HTTP port: {}", executor.http_port));
    reporter.info(&format!("gRPC port: {}", executor.grpc_port));

    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e);
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some("Daemon started"));

    // Create REST client
    let client = RestClient::new(executor.http_port);

    // Verify health
    let step = reporter.step_start("Health check", "Verify daemon is responding");
    match client.health().await {
        Ok(true) => reporter.step_success(step, Some("Daemon healthy")),
        Ok(false) => {
            reporter.step_failed(step, "Daemon unhealthy");
            panic!("Daemon unhealthy");
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            panic!("Health check failed: {}", e);
        }
    }

    // ========================================================================
    // PHASE 3: Connect to debugpy
    // ========================================================================
    reporter.section("PHASE 3: CONNECT TO DEBUGPY");

    let step = reporter.step_start(
        "Create connection",
        &format!("Connect to debugpy on port {}", executor.debugpy_port),
    );

    let connection_result = client
        .create_connection("127.0.0.1", executor.debugpy_port, "python")
        .await;

    let connection_id = match connection_result {
        Ok(r) => {
            reporter.step_success(
                step,
                Some(&format!("Connection ID: {}", r.data.connection_id)),
            );
            r.data.connection_id
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            executor.print_daemon_logs(100);
            panic!("Failed to create connection: {}", e);
        }
    };

    // ========================================================================
    // PHASE 4: Add metrics and capture initial events
    // ========================================================================
    reporter.section("PHASE 4: ADD METRICS & CAPTURE EVENTS");

    // Add a metric - use line 59 for trade_bot_forever.py (after +13 lines Detrix client init)
    // At line 59: `order_id = place_order(symbol, quantity, price)`
    // Variables price, quantity, symbol are in scope from lines 54-56
    let metric_line = 59;

    let location = format!("@{}#{}", script_path.display(), metric_line);
    let step = reporter.step_start(
        "Add metric",
        &format!("Add test metric at line {}", metric_line),
    );

    let mut request =
        AddMetricRequest::new("restart_test_metric", &location, "price", &connection_id);
    request.language = Some("python".to_string());
    request.enabled = Some(true);

    let _metric_id = match client.add_metric(request).await {
        Ok(r) => {
            reporter.step_success(step, Some(&format!("Metric ID: {}", r.data)));
            r.data
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            executor.print_daemon_logs(100);
            panic!("Failed to add metric: {}", e);
        }
    };

    // Wait for initial events
    let step = reporter.step_start(
        "Wait for events",
        "Wait for initial event capture (15s max)",
    );
    let mut initial_event_count = 0;
    let start = std::time::Instant::now();

    while start.elapsed() < Duration::from_secs(15) {
        match client.query_events("restart_test_metric", 100).await {
            Ok(r) => {
                if !r.data.is_empty() {
                    initial_event_count = r.data.len();
                    reporter.info(&format!("Captured {} event(s)", initial_event_count));
                    break;
                }
            }
            Err(e) => {
                reporter.warn(&format!("Query failed: {}", e));
            }
        }
        sleep(Duration::from_secs(2)).await;
    }

    if initial_event_count == 0 {
        reporter.step_failed(step, "No events captured before daemon restart");
        executor.print_daemon_logs(100);
        executor.print_debugpy_logs(50);
        panic!("Failed to capture initial events");
    }
    reporter.step_success(
        step,
        Some(&format!("{} events captured", initial_event_count)),
    );

    // ========================================================================
    // PHASE 5: Stop daemon
    // ========================================================================
    reporter.section("PHASE 5: STOP DAEMON");

    let step = reporter.step_start("Stop daemon", "Kill daemon process (debugpy continues)");
    executor.stop_daemon();
    reporter.step_success(step, Some("Daemon stopped"));

    // Verify daemon is actually down
    let step = reporter.step_start("Verify daemon down", "Confirm daemon is not responding");
    sleep(Duration::from_secs(2)).await;
    match client.health().await {
        Ok(_) => {
            reporter.step_failed(step, "Daemon still responding after stop");
            panic!("Daemon should not be responding after stop");
        }
        Err(_) => {
            reporter.step_success(step, Some("Daemon is down (connection refused)"));
        }
    }

    // ========================================================================
    // PHASE 6: Wait period
    // ========================================================================
    reporter.section("PHASE 6: WAIT PERIOD");

    let wait_seconds = 10;
    let step = reporter.step_start(
        "Wait",
        &format!("Wait {}s (debugpy continues running)", wait_seconds),
    );
    reporter.info("debugpy should continue running during this time...");

    for i in 1..=wait_seconds {
        sleep(Duration::from_secs(1)).await;
        if i % 2 == 0 {
            reporter.info(&format!("  {}s elapsed...", i));
        }
    }
    reporter.step_success(step, Some(&format!("Waited {}s", wait_seconds)));

    // ========================================================================
    // PHASE 7: Restart daemon
    // ========================================================================
    reporter.section("PHASE 7: RESTART DAEMON");

    let step = reporter.step_start(
        "Restart daemon",
        "Start daemon again (same database, should auto-reconnect)",
    );

    if let Err(e) = executor.restart_daemon().await {
        reporter.step_failed(step, &e);
        executor.print_daemon_logs(100);
        panic!("Failed to restart daemon: {}", e);
    }
    reporter.step_success(step, Some("Daemon restarted"));

    // Create new client (same port)
    let client = RestClient::new(executor.http_port);

    // Verify health
    let step = reporter.step_start("Health check", "Verify restarted daemon is healthy");
    match client.health().await {
        Ok(true) => reporter.step_success(step, Some("Daemon healthy")),
        Ok(false) => {
            reporter.step_failed(step, "Daemon unhealthy");
            panic!("Restarted daemon unhealthy");
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            panic!("Health check failed: {}", e);
        }
    }

    // ========================================================================
    // PHASE 8: Verify auto-reconnect
    // ========================================================================
    reporter.section("PHASE 8: VERIFY AUTO-RECONNECT");

    // Poll for connection to reach Connected status (reconnection is async)
    let step = reporter.step_start(
        "Check connections",
        "Verify daemon auto-reconnected to debugpy (polling up to 30s)",
    );

    let mut reconnected = false;
    let mut last_status = String::new();
    let start = std::time::Instant::now();
    // Allow 30s for auto-reconnect because debugpy needs time to detect dead connections
    // and our retry logic uses exponential backoff (3s timeout × up to 5 retries)
    let timeout = Duration::from_secs(30);

    while start.elapsed() < timeout {
        match client.list_connections().await {
            Ok(r) => {
                for conn in &r.data {
                    if conn.port == executor.debugpy_port {
                        last_status = conn.status.clone();
                        if conn.status.to_lowercase().contains("connected")
                            && !conn.status.to_lowercase().contains("disconnected")
                        {
                            reporter.info(&format!(
                                "Connection {} reached status: {}",
                                conn.connection_id, conn.status
                            ));
                            reconnected = true;
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                reporter.warn(&format!("List connections failed: {}", e));
            }
        }

        if reconnected {
            break;
        }

        sleep(Duration::from_millis(500)).await;
    }

    // Final check - list connections for logging
    let connections = match client.list_connections().await {
        Ok(r) => r.data,
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            executor.print_daemon_logs(100);
            panic!("Failed to list connections: {}", e);
        }
    };

    reporter.info(&format!("Found {} connection(s)", connections.len()));
    for conn in &connections {
        reporter.info(&format!(
            "  - {}: {} ({}:{})",
            conn.connection_id, conn.status, conn.host, conn.port
        ));
    }

    if reconnected {
        reporter.step_success(step, Some("Auto-reconnect successful"));
    } else {
        reporter.step_failed(step, "Connection not restored");
        executor.print_daemon_logs(100);

        // Check if connection exists but in different state
        let exists = connections.iter().any(|c| c.port == executor.debugpy_port);
        if exists {
            reporter.error(&format!(
                "Connection exists but not in Connected state (last status: {})",
                last_status
            ));
        } else {
            reporter.error("Connection not found at all");
        }

        panic!("Auto-reconnect failed");
    }

    // ========================================================================
    // PHASE 9: Verify events after reconnect
    // ========================================================================
    reporter.section("PHASE 9: VERIFY EVENTS AFTER RECONNECT");

    // Check metrics are preserved
    let step = reporter.step_start("Check metrics", "Verify metrics are preserved in database");
    match client.list_metrics().await {
        Ok(r) => {
            let metric_exists = r.data.iter().any(|m| m.name == "restart_test_metric");
            if metric_exists {
                reporter.step_success(step, Some(&format!("Found {} metric(s)", r.data.len())));
            } else {
                reporter.step_failed(step, "Test metric not found after restart");
                panic!("Metric not preserved after daemon restart");
            }
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            panic!("Failed to list metrics: {}", e);
        }
    }

    // Wait for NEW events after reconnect
    let step = reporter.step_start(
        "Capture new events",
        "Wait for new events after reconnection (20s max)",
    );

    let mut new_event_count = 0;
    let start = std::time::Instant::now();

    while start.elapsed() < Duration::from_secs(20) {
        match client.query_events("restart_test_metric", 100).await {
            Ok(r) => {
                new_event_count = r.data.len();
                if new_event_count > initial_event_count {
                    reporter.info(&format!(
                        "New events captured! Total: {} (was: {})",
                        new_event_count, initial_event_count
                    ));
                    break;
                }
            }
            Err(e) => {
                reporter.warn(&format!("Query failed: {}", e));
            }
        }
        sleep(Duration::from_secs(2)).await;
    }

    if new_event_count > initial_event_count {
        reporter.step_success(
            step,
            Some(&format!(
                "Events increased from {} to {} after reconnect",
                initial_event_count, new_event_count
            )),
        );
    } else {
        reporter.step_failed(
            step,
            &format!(
                "No new events after reconnect (initial: {}, after: {})",
                initial_event_count, new_event_count
            ),
        );
        executor.print_daemon_logs(100);
        executor.print_debugpy_logs(50);
        panic!("Failed to capture events after daemon restart");
    }

    // ========================================================================
    // CLEANUP & SUMMARY
    // ========================================================================
    reporter.section("TEST COMPLETE");
    reporter.info("✅ Daemon restart auto-reconnect test PASSED");
    reporter.info(&format!("   Initial events: {}", initial_event_count));
    reporter.info(&format!("   Events after restart: {}", new_event_count));
    reporter.info(&format!(
        "   New events captured: {}",
        new_event_count - initial_event_count
    ));

    // Cleanup is handled by Drop on executor
}

/// Test that daemon preserves connection state across restarts
///
/// Verifies that connection metadata (host, port, language, auto_reconnect flag)
/// is persisted and used for auto-reconnect.
#[tokio::test]
async fn test_daemon_restart_connection_persistence() {
    // Check if debugpy is available
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let reporter = TestReporter::new("daemon_restart_persistence", "REST");
    reporter.section("CONNECTION PERSISTENCE TEST");

    let mut executor = TestExecutor::new();

    // Start debugpy
    let script_path = executor
        .workspace_root
        .join("fixtures/python/trade_bot_forever.py");
    if !script_path.exists() {
        reporter.warn("trade_bot_forever.py not found, skipping test");
        return;
    }

    if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
        reporter.error(&format!("Failed to start debugpy: {}", e));
        return;
    }

    // Start daemon
    if let Err(e) = executor.start_daemon().await {
        reporter.error(&format!("Failed to start daemon: {}", e));
        return;
    }

    let client = RestClient::new(executor.http_port);

    // Create connection
    let connection_result = client
        .create_connection("127.0.0.1", executor.debugpy_port, "python")
        .await;

    let original_connection = match connection_result {
        Ok(r) => {
            reporter.info(&format!("Created connection: {}", r.data.connection_id));
            r.data
        }
        Err(e) => {
            reporter.error(&format!("Failed to create connection: {}", e));
            return;
        }
    };

    // Record connection details before restart
    let original_id = original_connection.connection_id.clone();
    let original_host = original_connection.host.clone();
    let original_port = original_connection.port;
    let original_language = original_connection.language.clone();

    reporter.info(&format!(
        "Original connection: id={}, host={}, port={}, language={}",
        original_id, original_host, original_port, original_language
    ));

    // Stop daemon
    executor.stop_daemon();
    sleep(Duration::from_secs(2)).await;

    // Restart daemon
    if let Err(e) = executor.restart_daemon().await {
        reporter.error(&format!("Failed to restart daemon: {}", e));
        return;
    }

    // Give time for auto-reconnect
    sleep(Duration::from_secs(5)).await;

    // Check connection after restart
    let client = RestClient::new(executor.http_port);

    match client.list_connections().await {
        Ok(r) => {
            reporter.info(&format!("After restart: {} connection(s)", r.data.len()));

            // Find the original connection
            let restored = r.data.iter().find(|c| c.connection_id == original_id);

            if let Some(conn) = restored {
                // Verify all fields are preserved
                assert_eq!(conn.host, original_host, "Host should be preserved");
                assert_eq!(conn.port, original_port, "Port should be preserved");
                assert_eq!(
                    conn.language, original_language,
                    "Language should be preserved"
                );

                reporter.info("✅ Connection metadata preserved correctly");
                reporter.info(&format!(
                    "   Restored: id={}, host={}, port={}, language={}, status={}",
                    conn.connection_id, conn.host, conn.port, conn.language, conn.status
                ));
            } else {
                reporter.error("Original connection not found after restart");
                panic!("Connection not restored");
            }
        }
        Err(e) => {
            reporter.error(&format!("Failed to list connections: {}", e));
            panic!("Failed to list connections: {}", e);
        }
    }

    reporter.info("✅ Connection persistence test PASSED");
}

/// Test daemon restart when debugger is not available
///
/// Verifies that daemon gracefully handles the case where a debugger
/// that was previously connected is no longer available.
#[tokio::test]
async fn test_daemon_restart_debugger_unavailable() {
    // Check if debugpy is available
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let reporter = TestReporter::new("daemon_restart_unavailable", "REST");
    reporter.section("DEBUGGER UNAVAILABLE TEST");

    let mut executor = TestExecutor::new();

    // Start debugpy
    let script_path = executor
        .workspace_root
        .join("fixtures/python/trade_bot_forever.py");
    if !script_path.exists() {
        reporter.warn("trade_bot_forever.py not found, skipping test");
        return;
    }

    if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
        reporter.error(&format!("Failed to start debugpy: {}", e));
        return;
    }

    // Start daemon
    if let Err(e) = executor.start_daemon().await {
        reporter.error(&format!("Failed to start daemon: {}", e));
        return;
    }

    let client = RestClient::new(executor.http_port);

    // Create connection
    let connection_result = client
        .create_connection("127.0.0.1", executor.debugpy_port, "python")
        .await;

    let connection_id = match connection_result {
        Ok(r) => {
            reporter.info(&format!("Created connection: {}", r.data.connection_id));
            r.data.connection_id
        }
        Err(e) => {
            reporter.error(&format!("Failed to create connection: {}", e));
            return;
        }
    };

    // Stop BOTH daemon AND debugpy
    reporter.info("Stopping daemon AND debugpy...");
    executor.stop_daemon();

    // Kill debugpy too (using proper method that also unregisters PID)
    executor.kill_debugpy();
    sleep(Duration::from_secs(2)).await;

    // Restart daemon (debugpy is NOT running)
    reporter.info("Restarting daemon (debugpy is NOT running)...");
    if let Err(e) = executor.restart_daemon().await {
        reporter.error(&format!("Failed to restart daemon: {}", e));
        return;
    }

    // Give time for auto-reconnect attempts
    sleep(Duration::from_secs(5)).await;

    // Check connection status - should be Failed or Disconnected
    let client = RestClient::new(executor.http_port);

    match client.list_connections().await {
        Ok(r) => {
            reporter.info(&format!("After restart: {} connection(s)", r.data.len()));

            for conn in &r.data {
                reporter.info(&format!(
                    "  - {}: {} ({}:{})",
                    conn.connection_id, conn.status, conn.host, conn.port
                ));

                // Verify it's NOT in Connected state (debugger is down)
                // Valid states: Failed, Disconnected, Connecting (retry in progress), Reconnecting
                if conn.connection_id == connection_id {
                    let status_lower = conn.status.to_lowercase();
                    assert!(
                        status_lower.contains("failed")
                            || status_lower.contains("disconnected")
                            || status_lower.contains("connecting")
                            || status_lower.contains("reconnecting")
                            || status_lower.contains("error"),
                        "Connection should NOT be in Connected state when debugger is unavailable, got: {}",
                        conn.status
                    );
                    // Check that it's NOT successfully connected
                    assert!(
                        !status_lower.eq("connected"),
                        "Connection should NOT be 'connected' when debugger is unavailable"
                    );
                    reporter.info(&format!(
                        "✅ Connection correctly in {} state (debugger unavailable)",
                        conn.status
                    ));
                }
            }
        }
        Err(e) => {
            reporter.error(&format!("Failed to list connections: {}", e));
            panic!("Failed to list connections: {}", e);
        }
    }

    reporter.info("✅ Debugger unavailable test PASSED");
}
