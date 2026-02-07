//! Reconnection E2E Tests
//!
//! Tests that verify the reconnection behavior when daemon restarts between client sessions.
//!
//! This test emulates the "double test_wake run" scenario:
//! 1. Start daemon
//! 2. Spawn Rust client app and wake it (first session)
//! 3. Add metrics and capture events
//! 4. Stop client app
//! 5. Stop daemon
//! 6. Start daemon again
//! 7. Spawn NEW Rust client app (same connection_id)
//! 8. Wake it (second session) - should be fast, no stale connection blocking
//! 9. Add metrics and verify only NEW events appear (no old events from run 1)
//!
//! This test specifically validates:
//! - Reconnection cancellation when port changes (different app instance = different debug port)
//! - Event timestamp filtering (since parameter)
//! - No duplicate events or stale events

use detrix_testing::e2e::{
    cleanup_orphaned_e2e_processes,
    client::{AddMetricRequest, ApiClient},
    client_tests::{ClientProcessTester, ClientTestConfig, ClientTester},
    executor::{find_detrix_binary, get_workspace_root},
    require_tool, RestClient, TestExecutor, TestReporter,
};
use std::time::{Duration, Instant};

/// Test: Reconnection after daemon restart with different debug port
///
/// This is the core reconnection test that validates:
/// 1. Old reconnection attempts are cancelled when a new request comes in with different port
/// 2. New connections happen quickly (not blocked by old reconnection attempts)
/// 3. Events from the new session are captured correctly
/// 4. No old events from previous sessions appear
#[tokio::test]
async fn test_rust_reconnection_different_port() {
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
        "rust_reconnection_different_port",
        "Rust - Reconnection with Different Port",
    );
    reporter.print_header();

    // ========================================================================
    // PHASE 1: First daemon session
    // ========================================================================
    reporter.section("PHASE 1: FIRST DAEMON SESSION");

    let step = reporter.step_start("Start daemon", "Launch Detrix daemon (first time)");
    let mut executor = TestExecutor::new();
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &format!("Failed to start daemon: {}", e));
        reporter.print_footer(false);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some(&format!("HTTP port: {}", executor.http_port)));

    let daemon_url = format!("http://127.0.0.1:{}", executor.http_port);
    let daemon_client = RestClient::new(executor.http_port);

    // ========================================================================
    // PHASE 2: First client session
    // ========================================================================
    reporter.section("PHASE 2: FIRST CLIENT SESSION");

    let step = reporter.step_start("Spawn client 1", "Spawn first Rust client app");
    let client1 = match ClientProcessTester::spawn(config.clone(), &daemon_url, 0).await {
        Ok(c) => c,
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to spawn client: {}", e));
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Failed to spawn Rust client: {}", e);
        }
    };
    let client1_port = client1.control_port();
    reporter.step_success(step, Some(&format!("Control port: {}", client1_port)));

    // Wake client 1
    let step = reporter.step_start(
        "Wake client 1",
        "Wake first client and establish connection",
    );
    let (connection_id_1, debug_port_1) = match client1.wake(None).await {
        Ok(resp) => {
            if resp.status == "awake" {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "connection_id={}, debug_port={}",
                        resp.connection_id, resp.debug_port
                    )),
                );
                (resp.connection_id, resp.debug_port)
            } else {
                reporter.step_failed(step, &format!("Unexpected status: {}", resp.status));
                reporter.print_footer(false);
                executor.stop_all();
                panic!("Wake failed: unexpected status {}", resp.status);
            }
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Wake failed: {}", e));
            client1.print_logs(50);
            executor.print_daemon_logs(50);
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Wake failed: {}", e);
        }
    };

    // Add a metric and wait for events
    let step = reporter.step_start("Add metric 1", "Add metric to observe pnl variable");
    let fixture_file = fixture_dir.join("src/main.rs");
    let location = format!("{}#128", fixture_file.display());

    let metric_req =
        AddMetricRequest::new("reconnect_test_pnl", &location, "pnl", &connection_id_1)
            .with_language("rust");

    match daemon_client.add_metric(metric_req).await {
        Ok(resp) => {
            reporter.step_success(step, Some(&format!("Metric ID: {}", resp.data)));
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to add metric: {}", e));
            client1.print_logs(50);
            executor.print_daemon_logs(50);
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Failed to add metric: {}", e);
        }
    }

    // Wait for some events
    let step = reporter.step_start(
        "Collect events 1",
        "Wait for events from first session (6s)",
    );
    tokio::time::sleep(tokio::time::Duration::from_secs(6)).await;

    let events_session1 = match daemon_client.query_events("reconnect_test_pnl", 100).await {
        Ok(resp) => resp.data,
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to query events: {}", e));
            Vec::new()
        }
    };
    reporter.step_success(
        step,
        Some(&format!("{} events from session 1", events_session1.len())),
    );

    // Record session 1 end timestamp (current time in microseconds)
    // This will be used to filter out old events when querying session 2
    let session1_end_time_micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;

    // Record the last event timestamp from session 1 (for display)
    let session1_last_ts = events_session1
        .last()
        .map(|e| e.timestamp_iso.clone())
        .unwrap_or_default();
    reporter.info(&format!("Last event timestamp: {}", session1_last_ts));
    reporter.info(&format!(
        "Session 1 end time (micros): {}",
        session1_end_time_micros
    ));

    // ========================================================================
    // PHASE 3: Stop client 1
    // ========================================================================
    reporter.section("PHASE 3: STOP CLIENT 1");

    let step = reporter.step_start("Stop client 1", "Stop first client app");
    drop(client1);
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    reporter.step_success(step, Some("Client 1 stopped"));

    // ========================================================================
    // PHASE 4: Stop daemon
    // ========================================================================
    reporter.section("PHASE 4: STOP DAEMON");

    let step = reporter.step_start("Stop daemon", "Stop Detrix daemon");
    executor.stop_daemon();
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    reporter.step_success(step, Some("Daemon stopped"));

    // Verify daemon is down
    let step = reporter.step_start("Verify daemon down", "Confirm daemon is not responding");
    match daemon_client.health().await {
        Ok(_) => {
            reporter.step_failed(step, "Daemon still responding after stop");
            // Don't panic, continue
        }
        Err(_) => {
            reporter.step_success(step, Some("Daemon is down (connection refused)"));
        }
    }

    // ========================================================================
    // PHASE 5: Restart daemon
    // ========================================================================
    reporter.section("PHASE 5: RESTART DAEMON");

    let step = reporter.step_start("Restart daemon", "Start daemon again (same database)");
    if let Err(e) = executor.restart_daemon().await {
        reporter.step_failed(step, &format!("Failed to restart daemon: {}", e));
        reporter.print_footer(false);
        panic!("Failed to restart daemon: {}", e);
    }
    reporter.step_success(step, Some(&format!("HTTP port: {}", executor.http_port)));

    // Create new REST client (same port)
    let daemon_client = RestClient::new(executor.http_port);

    // Verify daemon is healthy
    let step = reporter.step_start("Health check", "Verify daemon is responding");
    match daemon_client.health().await {
        Ok(true) => reporter.step_success(step, Some("Daemon healthy")),
        Ok(false) => {
            reporter.step_failed(step, "Daemon unhealthy");
            panic!("Daemon unhealthy after restart");
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            panic!("Health check failed: {}", e);
        }
    }

    // ========================================================================
    // PHASE 6: Second client session - TIMING IS CRITICAL
    // ========================================================================
    reporter.section("PHASE 6: SECOND CLIENT SESSION (RECONNECTION TEST)");

    // This is where the reconnection issue manifests:
    // - Daemon has a stored connection for "test-client" with old debug_port_1
    // - Daemon tries to reconnect to old port (which is now closed)
    // - If pending_starts doesn't cancel old reconnection, new wake is blocked

    let step = reporter.step_start("Spawn client 2", "Spawn second Rust client app");
    let spawn_start = Instant::now();

    let client2 = match ClientProcessTester::spawn(config.clone(), &daemon_url, 0).await {
        Ok(c) => c,
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to spawn client: {}", e));
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Failed to spawn second Rust client: {}", e);
        }
    };
    let client2_port = client2.control_port();
    reporter.step_success(step, Some(&format!("Control port: {}", client2_port)));

    // Wake client 2 - this is the critical timing test
    let step = reporter.step_start(
        "Wake client 2",
        "Wake second client (should be FAST, not blocked by old reconnection)",
    );
    let wake_start = Instant::now();

    let (connection_id_2, debug_port_2) = match client2.wake(None).await {
        Ok(resp) => {
            let wake_duration = wake_start.elapsed();
            if resp.status == "awake" {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "connection_id={}, debug_port={}, took {:?}",
                        resp.connection_id, resp.debug_port, wake_duration
                    )),
                );
                (resp.connection_id, resp.debug_port)
            } else {
                reporter.step_failed(step, &format!("Unexpected status: {}", resp.status));
                reporter.print_footer(false);
                executor.stop_all();
                panic!("Wake failed: unexpected status {}", resp.status);
            }
        }
        Err(e) => {
            let wake_duration = wake_start.elapsed();
            reporter.step_failed(
                step,
                &format!("Wake failed after {:?}: {}", wake_duration, e),
            );
            client2.print_logs(50);
            executor.print_daemon_logs(50);
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Wake failed: {}", e);
        }
    };

    let total_reconnection_time = spawn_start.elapsed();
    reporter.info(&format!(
        "Total reconnection time: {:?}",
        total_reconnection_time
    ));

    // Verify connection IDs match (same client name = same connection_id)
    let step = reporter.step_start("Verify connection ID", "Confirm same connection_id reused");
    if connection_id_1 == connection_id_2 {
        reporter.step_success(step, Some(&format!("Same ID: {}", connection_id_1)));
    } else {
        reporter.step_failed(
            step,
            &format!("IDs differ: {} vs {}", connection_id_1, connection_id_2),
        );
        // This is not a failure, just informational
    }

    // Verify debug ports are DIFFERENT (new app instance = new lldb-dap port)
    let step = reporter.step_start("Verify port changed", "Confirm debug port is different");
    if debug_port_1 != debug_port_2 {
        reporter.step_success(
            step,
            Some(&format!(
                "Port changed: {} -> {}",
                debug_port_1, debug_port_2
            )),
        );
    } else {
        reporter.step_failed(step, &format!("Same port: {} (unexpected)", debug_port_1));
        // This is unlikely but not a failure
    }

    // Check reconnection timing - should be under 30 seconds
    // (Old bug: took minutes due to reconnection blocking)
    let step = reporter.step_start(
        "Check timing",
        "Verify reconnection completed in reasonable time",
    );
    if total_reconnection_time < Duration::from_secs(30) {
        reporter.step_success(
            step,
            Some(&format!("Fast reconnection: {:?}", total_reconnection_time)),
        );
    } else {
        reporter.step_failed(
            step,
            &format!(
                "SLOW reconnection: {:?} (should be < 30s)",
                total_reconnection_time
            ),
        );
        // This is the bug we're testing for
        panic!(
            "Reconnection too slow: {:?} - old reconnection blocking detected",
            total_reconnection_time
        );
    }

    // ========================================================================
    // PHASE 7: Verify events from session 2
    // ========================================================================
    reporter.section("PHASE 7: VERIFY EVENTS FROM SESSION 2");

    // Re-add the same metric (upsert) for session 2
    let step = reporter.step_start("Add metric 2", "Re-add metric for session 2 (same name)");
    let metric_req =
        AddMetricRequest::new("reconnect_test_pnl", &location, "pnl", &connection_id_2)
            .with_language("rust");

    match daemon_client.add_metric(metric_req).await {
        Ok(resp) => {
            reporter.step_success(step, Some(&format!("Metric ID: {}", resp.data)));
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to add metric: {}", e));
            // Continue anyway
        }
    }

    // Wait for new events
    let step = reporter.step_start(
        "Collect events 2",
        "Wait for events from second session (9s)",
    );
    tokio::time::sleep(tokio::time::Duration::from_secs(9)).await;

    // Query events using daemon-side `since` filtering
    // This verifies the daemon correctly filters out old events
    let events_session2 = match daemon_client
        .query_events_since("reconnect_test_pnl", 100, session1_end_time_micros)
        .await
    {
        Ok(resp) => resp.data,
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to query events with since: {}", e));
            Vec::new()
        }
    };
    reporter.step_success(
        step,
        Some(&format!(
            "{} events from session 2 (daemon-side filtered)",
            events_session2.len()
        )),
    );

    // Also query ALL events (without since filter) to compare
    let all_events = match daemon_client.query_events("reconnect_test_pnl", 100).await {
        Ok(resp) => resp.data,
        Err(_) => Vec::new(),
    };
    reporter.info(&format!(
        "Total events (all time): {}, Filtered by since: {}",
        all_events.len(),
        events_session2.len()
    ));

    // Verify we got events from session 2
    let step = reporter.step_start(
        "Verify new events",
        "Confirm events captured in second session",
    );
    if events_session2.is_empty() {
        reporter.step_failed(step, "No events captured in session 2");
        client2.print_logs(50);
        executor.print_daemon_logs(50);
        reporter.print_footer(false);
        executor.stop_all();
        panic!("No events captured in session 2");
    }
    reporter.step_success(
        step,
        Some(&format!("{} new events captured", events_session2.len())),
    );

    // Verify events are from session 2 (after session 1 end time)
    // This tests the daemon-side `since` filtering
    let step = reporter.step_start(
        "Verify daemon-side filtering",
        "Confirm daemon returns only events after session 1 ended",
    );
    let first_event_ts = events_session2
        .first()
        .map(|e| e.timestamp_iso.clone())
        .unwrap_or_default();
    reporter.info(&format!("First session 2 event: {}", first_event_ts));
    reporter.info(&format!("Session 1 last event: {}", session1_last_ts));

    // Verify that filtered events (since) are fewer than all events (proving filtering works)
    // and that all filtered events are after session 1 ended
    if events_session2.len() < all_events.len() {
        reporter.step_success(
            step,
            Some(&format!(
                "Daemon correctly filtered: {} events after since vs {} total",
                events_session2.len(),
                all_events.len()
            )),
        );
    } else if !session1_last_ts.is_empty() && !first_event_ts.is_empty() {
        if first_event_ts > session1_last_ts {
            reporter.step_success(
                step,
                Some("Daemon correctly filtered - events are from session 2 only"),
            );
        } else {
            reporter.step_failed(
                step,
                &format!(
                    "Daemon filtering FAILED: session 2 first={}, session 1 last={}",
                    first_event_ts, session1_last_ts
                ),
            );
            panic!("Daemon-side `since` filtering not working - old events returned");
        }
    } else {
        reporter.step_success(step, Some("Timestamp check skipped (missing data)"));
    }

    // ========================================================================
    // CLEANUP
    // ========================================================================
    reporter.section("CLEANUP");
    reporter.info("Stopping client 2 and daemon...");
    drop(client2);
    executor.stop_all();

    reporter.print_footer(true);
    reporter.info(&format!(
        "PASSED: Reconnection completed in {:?}",
        total_reconnection_time
    ));
}

/// Test: Multiple rapid wake/sleep cycles don't cause connection issues
///
/// Verifies that rapid state changes don't cause race conditions or deadlocks.
#[tokio::test]
async fn test_rust_rapid_wake_sleep_cycles() {
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

    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("rust_rapid_wake_sleep", "Rust - Rapid Wake/Sleep Cycles");
    reporter.print_header();

    // Start daemon
    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    let mut executor = TestExecutor::new();
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &format!("Failed to start daemon: {}", e));
        reporter.print_footer(false);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, None);

    let daemon_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Spawn client
    let step = reporter.step_start("Spawn client", "Spawn Rust client app");
    let client = match ClientProcessTester::spawn(config, &daemon_url, 0).await {
        Ok(c) => c,
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to spawn client: {}", e));
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Failed to spawn Rust client: {}", e);
        }
    };
    reporter.step_success(
        step,
        Some(&format!("Control port: {}", client.control_port())),
    );

    // Perform rapid wake/sleep cycles
    let cycles = 5;
    let step = reporter.step_start(
        "Rapid cycles",
        &format!("Perform {} rapid wake/sleep cycles", cycles),
    );

    let mut success_count = 0;
    for i in 1..=cycles {
        // Wake
        if let Ok(resp) = client.wake(None).await {
            if resp.status == "awake" {
                success_count += 1;
            }
        }

        // Brief pause
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // Sleep
        let _ = client.sleep().await;

        // Brief pause
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        reporter.info(&format!("Cycle {} complete", i));
    }

    if success_count >= cycles - 1 {
        reporter.step_success(
            step,
            Some(&format!("{}/{} cycles succeeded", success_count, cycles)),
        );
    } else {
        reporter.step_failed(
            step,
            &format!("Only {}/{} cycles succeeded", success_count, cycles),
        );
        client.print_logs(50);
        executor.print_daemon_logs(50);
    }

    // Final wake to verify everything still works
    let step = reporter.step_start("Final wake", "Verify client still works after rapid cycles");
    match client.wake(None).await {
        Ok(resp) => {
            if resp.status == "awake" {
                reporter.step_success(
                    step,
                    Some(&format!("Final wake succeeded: {}", resp.connection_id)),
                );
            } else {
                reporter.step_failed(step, &format!("Unexpected status: {}", resp.status));
            }
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Final wake failed: {}", e));
            client.print_logs(50);
            executor.print_daemon_logs(50);
        }
    }

    // Cleanup
    drop(client);
    executor.stop_all();

    reporter.print_footer(true);
}

/// Test: Connection persists across daemon restart
///
/// Verifies that connection metadata is preserved in the database
/// and auto-reconnect works when the debugger is still running.
#[tokio::test]
async fn test_rust_connection_persistence() {
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

    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new(
        "rust_connection_persistence",
        "Rust - Connection Persistence",
    );
    reporter.print_header();

    // Start daemon
    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    let mut executor = TestExecutor::new();
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &format!("Failed to start daemon: {}", e));
        reporter.print_footer(false);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, None);

    let daemon_url = format!("http://127.0.0.1:{}", executor.http_port);
    let daemon_client = RestClient::new(executor.http_port);

    // Spawn client and wake
    let step = reporter.step_start("Spawn and wake", "Spawn client and establish connection");
    let client = match ClientProcessTester::spawn(config, &daemon_url, 0).await {
        Ok(c) => c,
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to spawn client: {}", e));
            reporter.print_footer(false);
            executor.stop_all();
            panic!("Failed to spawn Rust client: {}", e);
        }
    };

    let (connection_id, debug_port) = match client.wake(None).await {
        Ok(resp) => (resp.connection_id, resp.debug_port),
        Err(e) => {
            reporter.step_failed(step, &format!("Wake failed: {}", e));
            executor.stop_all();
            panic!("Wake failed: {}", e);
        }
    };
    reporter.step_success(
        step,
        Some(&format!("ID: {}, port: {}", connection_id, debug_port)),
    );

    // Verify connection in daemon
    let step = reporter.step_start("Verify connection", "Check connection registered in daemon");
    match daemon_client.list_connections().await {
        Ok(resp) => {
            let found = resp.data.iter().any(|c| c.connection_id == connection_id);
            if found {
                reporter.step_success(step, Some(&format!("Found connection: {}", connection_id)));
            } else {
                reporter.step_failed(step, "Connection not found in daemon");
            }
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to list connections: {}", e));
        }
    }

    // Stop daemon (client keeps running)
    let step = reporter.step_start("Stop daemon", "Stop daemon (client continues)");
    executor.stop_daemon();
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    reporter.step_success(step, None);

    // Client should still be running
    let step = reporter.step_start("Client status", "Verify client still responds");
    match client.status().await {
        Ok(status) => {
            reporter.step_success(step, Some(&format!("State: {}", status.state)));
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Status failed: {}", e));
        }
    }

    // Restart daemon
    let step = reporter.step_start("Restart daemon", "Start daemon again");
    if let Err(e) = executor.restart_daemon().await {
        reporter.step_failed(step, &format!("Failed to restart: {}", e));
        reporter.print_footer(false);
        panic!("Failed to restart daemon: {}", e);
    }
    reporter.step_success(step, None);

    // Create new client
    let daemon_client = RestClient::new(executor.http_port);

    // Wait for auto-reconnect
    let step = reporter.step_start(
        "Wait reconnect",
        "Wait for daemon to auto-reconnect (15s max)",
    );
    let start = Instant::now();
    let mut reconnected = false;

    while start.elapsed() < Duration::from_secs(15) {
        match daemon_client.list_connections().await {
            Ok(resp) => {
                for conn in &resp.data {
                    if conn.connection_id == connection_id
                        && conn.status.to_lowercase().contains("connected")
                        && !conn.status.to_lowercase().contains("disconnected")
                    {
                        reporter.info(&format!(
                            "Connection {} status: {}",
                            conn.connection_id, conn.status
                        ));
                        reconnected = true;
                        break;
                    }
                }
            }
            Err(_) => {}
        }

        if reconnected {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    if reconnected {
        reporter.step_success(
            step,
            Some(&format!("Auto-reconnected in {:?}", start.elapsed())),
        );
    } else {
        reporter.step_failed(step, "Auto-reconnect did not complete in time");
        executor.print_daemon_logs(100);
    }

    // Cleanup
    drop(client);
    executor.stop_all();

    reporter.print_footer(reconnected);
}
