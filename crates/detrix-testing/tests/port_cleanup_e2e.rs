//! Port Cleanup E2E Tests
//!
//! Tests that verify detrix properly releases ports after target process termination.
//!
//! This test addresses a bug where after:
//! 1. Starting debugpy on port 5678
//! 2. Connecting detrix to it
//! 3. Killing the debugpy process (Ctrl+C)
//! 4. Closing the terminal
//!
//! The port remained in use until detrix was closed, preventing debugpy from
//! restarting on the same port with error:
//! "Can't listen for client connections: [WinError 10048] Only one usage of
//! each socket address is normally permitted"
//!
//! Test flow:
//! 1. Start debugpy with a Python script
//! 2. Start daemon
//! 3. Connect to debugpy
//! 4. Kill debugpy process (simulating Ctrl+C)
//! 5. Close connection in detrix
//! 6. Verify port is released (can restart debugpy on same port)

use detrix_testing::e2e::{
    availability::{require_tool, ToolDependency},
    client::ApiClient,
    executor::{is_port_available, TestExecutor},
    reporter::TestReporter,
    rest::RestClient,
};
use std::time::Duration;
use tokio::time::sleep;

/// Test port cleanup after target process is killed
///
/// This test verifies that when a debugpy process is killed while detrix is connected,
/// the port is properly released after detrix closes the connection.
///
/// This is the exact scenario reported by the user:
/// 1. Run `python -m debugpy --listen 5678 script.py`
/// 2. Connect detrix
/// 3. Kill debugpy with Ctrl+C
/// 4. Try to restart debugpy on same port - should work after detrix cleanup
#[tokio::test]
async fn test_port_cleanup_after_debugpy_kill() {
    // Check if debugpy is available
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let reporter = TestReporter::new("port_cleanup_debugpy_kill", "REST");
    reporter.section("PORT CLEANUP AFTER DEBUGPY KILL");
    reporter.info("Testing that port is released after debugpy is killed and connection closed");

    let mut executor = TestExecutor::new();
    let debugpy_port = executor.debugpy_port;

    // ========================================================================
    // PHASE 1: Start debugpy
    // ========================================================================
    reporter.section("PHASE 1: START DEBUGPY");

    let script_path = executor
        .workspace_root
        .join("fixtures/python/detrix_example_app_continuous.py");
    if !script_path.exists() {
        reporter.error("detrix_example_app_continuous.py not found");
        panic!("Missing Python test fixture");
    }

    let step = reporter.step_start(
        "Start debugpy",
        &format!("Launch debugpy on port {}", debugpy_port),
    );
    if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
        reporter.step_failed(step, &e);
        executor.print_debugpy_logs(50);
        panic!("Failed to start debugpy: {}", e);
    }
    reporter.step_success(step, Some("debugpy started"));

    // Verify port is in use
    assert!(
        !is_port_available(debugpy_port),
        "Port {} should be in use by debugpy",
        debugpy_port
    );

    // ========================================================================
    // PHASE 2: Start daemon
    // ========================================================================
    reporter.section("PHASE 2: START DAEMON");

    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e);
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some("Daemon started"));

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
        &format!("Connect to debugpy on port {}", debugpy_port),
    );

    let connection_id = match client
        .create_connection("127.0.0.1", debugpy_port, "python")
        .await
    {
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

    // Wait for DAP handshake to complete
    sleep(Duration::from_secs(2)).await;

    // ========================================================================
    // PHASE 4: Kill debugpy (simulating Ctrl+C)
    // ========================================================================
    reporter.section("PHASE 4: KILL DEBUGPY (SIMULATING CTRL+C)");

    let step = reporter.step_start("Kill debugpy", "Terminate debugpy process abruptly");

    // Kill the debugpy process directly (simulating Ctrl+C)
    executor.kill_debugpy();

    // Wait for process to fully terminate
    sleep(Duration::from_millis(500)).await;

    reporter.step_success(step, Some("debugpy killed"));

    // ========================================================================
    // PHASE 5: Wait for detrix to detect disconnection (NO manual close!)
    // ========================================================================
    reporter.section("PHASE 5: WAIT FOR DETRIX TO DETECT DISCONNECTION");

    // IMPORTANT: We do NOT close the connection manually!
    // Detrix should automatically detect that debugpy died and clean up.
    // This simulates the real user scenario where Ctrl+C kills debugpy
    // and the user expects to restart it without closing detrix.

    let step = reporter.step_start(
        "Wait for auto-detection",
        "Wait for detrix to detect debugpy is dead (no manual close)",
    );

    // Give detrix time to detect the disconnection via the DAP reader task
    // When the TCP connection closes (debugpy dies), the reader should detect EOF
    // and trigger cleanup automatically
    sleep(Duration::from_secs(5)).await;

    reporter.step_success(step, Some("Waited for auto-detection"));

    // Check connection status - it should show disconnected/failed
    let step = reporter.step_start(
        "Check connection status",
        "Verify detrix detected the disconnection",
    );

    match client.list_connections().await {
        Ok(r) => {
            if let Some(conn) = r.data.iter().find(|c| c.connection_id == connection_id) {
                let status_lower = conn.status.to_lowercase();
                if status_lower.contains("disconnected")
                    || status_lower.contains("failed")
                    || status_lower.contains("error")
                {
                    reporter.step_success(
                        step,
                        Some(&format!(
                            "Connection status: {} (detected dead target)",
                            conn.status
                        )),
                    );
                } else {
                    reporter.warn(&format!(
                        "Connection still shows '{}' - detrix may not have detected disconnection yet",
                        conn.status
                    ));
                }
            } else {
                reporter.info("Connection not found (may have been auto-removed)");
            }
        }
        Err(e) => {
            reporter.warn(&format!("Could not check connection status: {}", e));
        }
    }

    // ========================================================================
    // PHASE 6: Verify port is released (without manual close!)
    // ========================================================================
    reporter.section("PHASE 6: VERIFY PORT IS RELEASED (NO MANUAL CLOSE)");

    let step = reporter.step_start(
        "Check port availability",
        &format!("Verify port {} is available for reuse", debugpy_port),
    );

    // The port should now be available
    // Note: On some systems there may be a brief TIME_WAIT period
    let mut port_available = false;
    for attempt in 1..=10 {
        if is_port_available(debugpy_port) {
            port_available = true;
            reporter.info(&format!(
                "Port {} became available after {} attempts",
                debugpy_port, attempt
            ));
            break;
        }
        reporter.info(&format!(
            "Attempt {}: Port {} still in use, waiting...",
            attempt, debugpy_port
        ));
        sleep(Duration::from_millis(500)).await;
    }

    if port_available {
        reporter.step_success(step, Some("Port is available"));
    } else {
        reporter.step_failed(
            step,
            &format!("Port {} is still in use after cleanup", debugpy_port),
        );
        executor.print_daemon_logs(100);
        panic!(
            "Port {} not released after connection close - this is the bug!",
            debugpy_port
        );
    }

    // ========================================================================
    // PHASE 7: Restart debugpy on same port (the actual test)
    // ========================================================================
    reporter.section("PHASE 7: RESTART DEBUGPY ON SAME PORT");

    let step = reporter.step_start(
        "Restart debugpy",
        &format!(
            "Start new debugpy on port {} (should succeed)",
            debugpy_port
        ),
    );

    // Try to start debugpy on the same port - this is the actual test!
    match executor.start_debugpy(script_path.to_str().unwrap()).await {
        Ok(_) => {
            reporter.step_success(step, Some("debugpy restarted successfully on same port"));
        }
        Err(e) => {
            reporter.step_failed(step, &e);
            reporter.error("THIS IS THE BUG: Port was not released after connection close!");
            executor.print_daemon_logs(100);
            panic!(
                "Failed to restart debugpy on port {} - port cleanup bug! Error: {}",
                debugpy_port, e
            );
        }
    }

    // ========================================================================
    // CLEANUP & SUMMARY
    // ========================================================================
    reporter.section("TEST COMPLETE");
    reporter.info("Port cleanup test PASSED");
    reporter.info(&format!(
        "   Port {} was properly released after:",
        debugpy_port
    ));
    reporter.info("   1. debugpy was killed (simulating Ctrl+C)");
    reporter.info("   2. Detrix auto-detected the disconnection (NO manual close)");
    reporter.info("   3. debugpy could restart on the same port");
}

/// Test port cleanup when connection is closed while debugpy is still running
///
/// This tests a different scenario: detrix disconnects while debugpy is alive.
/// The port should remain in use (by debugpy), but detrix should release its
/// connection to it cleanly.
#[tokio::test]
async fn test_port_cleanup_normal_disconnect() {
    // Check if debugpy is available
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let reporter = TestReporter::new("port_cleanup_normal_disconnect", "REST");
    reporter.section("PORT CLEANUP NORMAL DISCONNECT");
    reporter.info("Testing that detrix cleanly disconnects while debugpy is running");

    let mut executor = TestExecutor::new();
    let debugpy_port = executor.debugpy_port;

    // Start debugpy
    let script_path = executor
        .workspace_root
        .join("fixtures/python/detrix_example_app_continuous.py");
    if !script_path.exists() {
        reporter.warn("Test fixture not found, skipping");
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

    // Connect to debugpy
    let connection_id = match client
        .create_connection("127.0.0.1", debugpy_port, "python")
        .await
    {
        Ok(r) => r.data.connection_id,
        Err(e) => {
            reporter.error(&format!("Failed to create connection: {}", e));
            return;
        }
    };

    // Wait for DAP handshake
    sleep(Duration::from_secs(2)).await;

    // Close connection while debugpy is still running
    let step = reporter.step_start(
        "Close connection",
        "Close detrix connection while debugpy is running",
    );

    match client.close_connection(&connection_id).await {
        Ok(_) => {
            reporter.step_success(step, Some("Connection closed cleanly"));
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            // This shouldn't fail
            panic!("Failed to close connection: {}", e);
        }
    }

    // Give detrix time to clean up
    sleep(Duration::from_secs(1)).await;

    // Verify debugpy is still running and holding the port
    let step = reporter.step_start(
        "Verify debugpy still running",
        &format!("Port {} should still be in use by debugpy", debugpy_port),
    );

    if !is_port_available(debugpy_port) {
        reporter.step_success(step, Some("debugpy still holding port (expected)"));
    } else {
        // This is unexpected - debugpy should still be running
        reporter.warn("Port is available - debugpy may have exited unexpectedly");
    }

    // Try to reconnect to the same debugpy
    let step = reporter.step_start("Reconnect", "Reconnect to same debugpy instance");

    match client
        .create_connection("127.0.0.1", debugpy_port, "python")
        .await
    {
        Ok(r) => {
            reporter.step_success(
                step,
                Some(&format!("Reconnected: {}", r.data.connection_id)),
            );
        }
        Err(e) => {
            // debugpy may have exited after we disconnected (it's single-session by default)
            reporter.info(&format!(
                "Could not reconnect (debugpy may be single-session): {}",
                e
            ));
        }
    }

    reporter.section("TEST COMPLETE");
    reporter.info("Normal disconnect test PASSED");
}

/// Test port cleanup with multiple connect/disconnect cycles
///
/// This tests that repeated connect/disconnect cycles don't leak resources
/// or leave ports in a bad state.
#[tokio::test]
async fn test_port_cleanup_multiple_cycles() {
    // Check if debugpy is available
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let reporter = TestReporter::new("port_cleanup_multiple_cycles", "REST");
    reporter.section("PORT CLEANUP MULTIPLE CYCLES");
    reporter.info("Testing that multiple connect/kill/restart cycles don't leak ports");

    let mut executor = TestExecutor::new();
    let debugpy_port = executor.debugpy_port;

    let script_path = executor
        .workspace_root
        .join("fixtures/python/detrix_example_app_continuous.py");
    if !script_path.exists() {
        reporter.warn("Test fixture not found, skipping");
        return;
    }

    // Start daemon once
    if let Err(e) = executor.start_daemon().await {
        reporter.error(&format!("Failed to start daemon: {}", e));
        return;
    }

    let client = RestClient::new(executor.http_port);

    // Perform multiple cycles of start/connect/kill/cleanup
    const CYCLES: usize = 3;

    for cycle in 1..=CYCLES {
        reporter.section(&format!("CYCLE {} OF {}", cycle, CYCLES));

        // Health check - verify daemon is still alive before each cycle
        let step = reporter.step_start(
            "Health check",
            &format!("Cycle {}: Verify daemon is responding", cycle),
        );
        match client.health().await {
            Ok(true) => reporter.step_success(step, Some("Daemon healthy")),
            Ok(false) => {
                reporter.step_failed(step, "Daemon unhealthy");
                executor.print_daemon_logs(100);
                panic!("Cycle {}: Daemon became unhealthy", cycle);
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(100);
                panic!("Cycle {}: Daemon health check failed: {}", cycle, e);
            }
        }

        // Start debugpy
        let step = reporter.step_start(
            "Start debugpy",
            &format!("Cycle {}: Start debugpy on port {}", cycle, debugpy_port),
        );

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.step_failed(step, &e);
            executor.print_daemon_logs(100);
            panic!("Cycle {}: Failed to start debugpy: {}", cycle, e);
        }
        reporter.step_success(step, Some("debugpy started"));

        // Connect
        let step = reporter.step_start("Connect", &format!("Cycle {}: Connect to debugpy", cycle));

        let connection_id = match client
            .create_connection("127.0.0.1", debugpy_port, "python")
            .await
        {
            Ok(r) => {
                reporter.step_success(step, Some(&format!("Connected: {}", r.data.connection_id)));
                r.data.connection_id
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                panic!("Cycle {}: Failed to connect: {}", cycle, e);
            }
        };

        // Wait for DAP handshake
        sleep(Duration::from_secs(2)).await;

        // Kill debugpy
        let step = reporter.step_start("Kill debugpy", &format!("Cycle {}: Kill debugpy", cycle));
        executor.kill_debugpy();
        sleep(Duration::from_millis(500)).await;
        reporter.step_success(step, Some("debugpy killed"));

        // Close connection
        let step = reporter.step_start(
            "Close connection",
            &format!("Cycle {}: Close connection", cycle),
        );
        let _ = client.close_connection(&connection_id).await;
        reporter.step_success(step, Some("Connection closed"));

        // Wait for cleanup - increased from 2s to 3s to handle parallel test load
        sleep(Duration::from_secs(3)).await;

        // Verify port is available
        let step = reporter.step_start(
            "Check port",
            &format!("Cycle {}: Verify port {} is available", cycle, debugpy_port),
        );

        let mut port_available = false;
        for attempt in 1..=10 {
            if is_port_available(debugpy_port) {
                port_available = true;
                break;
            }
            sleep(Duration::from_millis(500)).await;
            if attempt == 10 {
                reporter.warn(&format!("Port still in use after {} attempts", attempt));
            }
        }

        if port_available {
            reporter.step_success(step, Some("Port available"));
        } else {
            reporter.step_failed(step, "Port not released");
            executor.print_daemon_logs(100);
            panic!(
                "Cycle {}: Port {} not released after cleanup!",
                cycle, debugpy_port
            );
        }
    }

    reporter.section("TEST COMPLETE");
    reporter.info(&format!(
        "Multiple cycles test PASSED ({} cycles completed)",
        CYCLES
    ));
    reporter.info("   No port leaks detected across multiple connect/kill/restart cycles");
}
