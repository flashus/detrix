//! Connection Crash E2E Tests
//!
//! Tests that connection status is correctly updated when debuggers crash or are killed.
//! This verifies the fix for the issue where `detrix status` showed stale connections.
//!
//! Test flow:
//! 1. Start daemon
//! 2. Start Python debugger (debugpy) and Rust debugger (lldb-dap)
//! 3. Connect to both debuggers
//! 4. Verify status shows 2 active connections
//! 5. Kill Python debugger
//! 6. Verify status shows Python disconnected, Rust still connected
//! 7. Kill Rust debugger
//! 8. Verify status shows both disconnected

use detrix_testing::e2e::{
    availability::{require_tool, ToolDependency},
    client::ApiClient,
    executor::TestExecutor,
    reporter::TestReporter,
    rest::RestClient,
};
use std::time::Duration;
use tokio::time::sleep;

/// Test that connection status is updated when debuggers crash
///
/// This test verifies the fix for the issue where `detrix status` showed
/// stale connections after debugger processes were killed.
#[tokio::test]
async fn test_connection_status_updated_on_debugger_crash() {
    // Check if both debugpy and lldb are available
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }
    if !require_tool(ToolDependency::Rustc).await {
        return;
    }
    if !require_tool(ToolDependency::LldbDap).await {
        return;
    }

    let reporter = TestReporter::new("connection_crash_status", "REST");
    reporter.section("CONNECTION CRASH STATUS UPDATE TEST");
    reporter.info("Testing that connection status updates correctly when debuggers crash");

    let mut executor = TestExecutor::new();

    // ========================================================================
    // PHASE 1: Start debuggers (Python and Rust)
    // ========================================================================
    reporter.section("PHASE 1: START DEBUGGERS");

    // Start Python debugger
    let python_script = executor
        .workspace_root
        .join("fixtures/python/trade_bot_forever.py");
    if !python_script.exists() {
        reporter.warn("trade_bot_forever.py not found, skipping test");
        return;
    }

    let step = reporter.step_start("Start debugpy", "Launch Python debugger");
    reporter.info(&format!("Script: {}", python_script.display()));
    reporter.info(&format!("Port: {}", executor.debugpy_port));

    if let Err(e) = executor
        .start_debugpy(python_script.to_str().unwrap())
        .await
    {
        reporter.step_failed(step, &e);
        executor.print_debugpy_logs(50);
        panic!("Failed to start debugpy: {}", e);
    }
    reporter.step_success(step, Some("debugpy started"));

    // Start Rust debugger (lldb-dap via detrix lldb-serve)
    let rust_source = executor.workspace_root.join("fixtures/rust/src/main.rs");
    if !rust_source.exists() {
        reporter.warn("detrix_example_app.rs not found, skipping test");
        return;
    }

    let step = reporter.step_start("Start lldb-dap", "Launch Rust debugger");
    reporter.info(&format!("Source: {}", rust_source.display()));
    reporter.info(&format!("Port: {}", executor.lldb_port));

    if let Err(e) = executor.start_lldb(rust_source.to_str().unwrap()).await {
        reporter.step_failed(step, &e);
        executor.print_lldb_logs(50);
        reporter.warn("Rust/lldb tests may not work on this platform");
        // Don't panic - continue with Python-only test
        return;
    }
    reporter.step_success(step, Some("lldb-dap started"));

    // ========================================================================
    // PHASE 2: Start daemon
    // ========================================================================
    reporter.section("PHASE 2: START DAEMON");

    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    reporter.info(&format!("HTTP port: {}", executor.http_port));
    reporter.info(&format!("gRPC port: {}", executor.grpc_port));

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
    // PHASE 3: Connect to both debuggers
    // ========================================================================
    reporter.section("PHASE 3: CONNECT TO DEBUGGERS");

    // Connect to Python
    let step = reporter.step_start(
        "Connect Python",
        &format!("Connect to debugpy on port {}", executor.debugpy_port),
    );

    let py_conn = match client
        .create_connection("127.0.0.1", executor.debugpy_port, "python")
        .await
    {
        Ok(r) => {
            reporter.step_success(
                step,
                Some(&format!("Python connection ID: {}", r.data.connection_id)),
            );
            r.data
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            executor.print_daemon_logs(100);
            panic!("Failed to create Python connection: {}", e);
        }
    };

    // Connect to Rust
    let step = reporter.step_start(
        "Connect Rust",
        &format!("Connect to lldb-dap on port {}", executor.lldb_port),
    );

    // Use create_connection_with_program for direct lldb-dap TCP mode
    let program = executor
        .rust_binary_path
        .as_ref()
        .map(|p| p.to_str().unwrap());
    let rs_conn = match client
        .create_connection_with_program("127.0.0.1", executor.lldb_port, "rust", program)
        .await
    {
        Ok(r) => {
            reporter.step_success(
                step,
                Some(&format!("Rust connection ID: {}", r.data.connection_id)),
            );
            r.data
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            executor.print_daemon_logs(100);
            panic!("Failed to create Rust connection: {}", e);
        }
    };

    // ========================================================================
    // PHASE 4: Verify both connections are active
    // ========================================================================
    reporter.section("PHASE 4: VERIFY INITIAL STATUS");

    let step = reporter.step_start(
        "Check status",
        "Verify both connections are in Connected state",
    );

    let connections = match client.list_connections().await {
        Ok(r) => r.data,
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
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

    // Verify Python is connected
    let py_connected = connections.iter().any(|c| {
        c.connection_id == py_conn.connection_id && c.status.to_lowercase() == "connected"
    });

    // Verify Rust is connected
    let rs_connected = connections.iter().any(|c| {
        c.connection_id == rs_conn.connection_id && c.status.to_lowercase() == "connected"
    });

    if py_connected && rs_connected {
        reporter.step_success(step, Some("Both connections are active"));
    } else {
        reporter.step_failed(
            step,
            &format!(
                "Python connected: {}, Rust connected: {}",
                py_connected, rs_connected
            ),
        );
        panic!("Not all connections are active");
    }

    // ========================================================================
    // PHASE 5: Kill Python debugger and verify status update
    // ========================================================================
    reporter.section("PHASE 5: KILL PYTHON DEBUGGER");

    let step = reporter.step_start(
        "Kill debugpy",
        "Kill Python debugger process and verify status updates",
    );

    // Kill debugpy
    executor.kill_debugpy();

    // Wait for the daemon to detect the crash and update status
    reporter.info("Waiting for daemon to detect Python debugger crash...");
    sleep(Duration::from_secs(3)).await;

    reporter.step_success(step, Some("debugpy killed"));

    // Check connection status
    let step = reporter.step_start(
        "Verify Python disconnected",
        "Python should be disconnected, Rust still connected",
    );

    let connections = match client.list_connections().await {
        Ok(r) => r.data,
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            panic!("Failed to list connections: {}", e);
        }
    };

    reporter.info(&format!(
        "After killing Python debugger, {} connection(s):",
        connections.len()
    ));
    for conn in &connections {
        reporter.info(&format!(
            "  - {}: {} ({}:{})",
            conn.connection_id, conn.status, conn.host, conn.port
        ));
    }

    // Find Python connection
    let py_status = connections
        .iter()
        .find(|c| c.connection_id == py_conn.connection_id)
        .map(|c| c.status.to_lowercase());

    // Find Rust connection
    let rs_status = connections
        .iter()
        .find(|c| c.connection_id == rs_conn.connection_id)
        .map(|c| c.status.to_lowercase());

    // Verify Python is disconnected (not "connected")
    let py_disconnected = py_status.as_ref().map(|s| s != "connected").unwrap_or(true);

    // Verify Rust is still connected
    let rs_still_connected = rs_status
        .as_ref()
        .map(|s| s == "connected")
        .unwrap_or(false);

    if py_disconnected && rs_still_connected {
        reporter.step_success(
            step,
            Some(&format!(
                "Python: {}, Rust: connected",
                py_status.unwrap_or_else(|| "removed".to_string())
            )),
        );
    } else {
        reporter.step_failed(
            step,
            &format!(
                "Expected Python disconnected and Rust connected. Got Python: {:?}, Rust: {:?}",
                py_status, rs_status
            ),
        );
        executor.print_daemon_logs(100);
        panic!("Connection status not updated correctly after Python crash");
    }

    // ========================================================================
    // PHASE 6: Kill Rust debugger and verify status update
    // ========================================================================
    reporter.section("PHASE 6: KILL RUST DEBUGGER");

    let step = reporter.step_start(
        "Kill lldb-dap",
        "Kill Rust debugger process and verify status updates",
    );

    // Kill lldb-dap
    executor.kill_lldb();

    // Wait for the daemon to detect the crash and update status
    reporter.info("Waiting for daemon to detect Rust debugger crash...");
    sleep(Duration::from_secs(3)).await;

    reporter.step_success(step, Some("lldb-dap killed"));

    // Check final connection status
    let step = reporter.step_start(
        "Verify both disconnected",
        "Both connections should now be disconnected",
    );

    let connections = match client.list_connections().await {
        Ok(r) => r.data,
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            panic!("Failed to list connections: {}", e);
        }
    };

    reporter.info(&format!(
        "After killing both debuggers, {} connection(s):",
        connections.len()
    ));
    for conn in &connections {
        reporter.info(&format!(
            "  - {}: {} ({}:{})",
            conn.connection_id, conn.status, conn.host, conn.port
        ));
    }

    // Verify both connections are now disconnected
    let py_final_status = connections
        .iter()
        .find(|c| c.connection_id == py_conn.connection_id)
        .map(|c| c.status.to_lowercase());

    let rs_final_status = connections
        .iter()
        .find(|c| c.connection_id == rs_conn.connection_id)
        .map(|c| c.status.to_lowercase());

    let py_final_disconnected = py_final_status
        .as_ref()
        .map(|s| s != "connected")
        .unwrap_or(true);

    let rs_final_disconnected = rs_final_status
        .as_ref()
        .map(|s| s != "connected")
        .unwrap_or(true);

    if py_final_disconnected && rs_final_disconnected {
        reporter.step_success(
            step,
            Some(&format!(
                "Python: {}, Rust: {}",
                py_final_status.unwrap_or_else(|| "removed".to_string()),
                rs_final_status.unwrap_or_else(|| "removed".to_string())
            )),
        );
    } else {
        reporter.step_failed(
            step,
            &format!(
                "Expected both disconnected. Got Python: {:?}, Rust: {:?}",
                py_final_status, rs_final_status
            ),
        );
        executor.print_daemon_logs(100);
        panic!("Connection status not updated correctly after both crashes");
    }

    // ========================================================================
    // CLEANUP & SUMMARY
    // ========================================================================
    reporter.section("TEST COMPLETE");
    reporter.info("✅ Connection crash status update test PASSED");
    reporter.info("   - Both Python and Rust debuggers were connected");
    reporter.info("   - After killing Python debugger, status correctly updated to disconnected");
    reporter.info("   - Rust debugger remained connected during Python crash");
    reporter.info("   - After killing Rust debugger, status correctly updated to disconnected");
    reporter.info("   - `detrix status` now correctly reflects actual connection state");

    // Cleanup is handled by Drop on executor
}

/// Test connection status with Python debugger only
///
/// A simpler test that only uses Python, for environments where Rust/lldb
/// may not be available.
#[tokio::test]
async fn test_connection_status_updated_on_python_crash() {
    // Check if debugpy is available
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let reporter = TestReporter::new("connection_crash_python", "REST");
    reporter.section("PYTHON CONNECTION CRASH TEST");

    let mut executor = TestExecutor::new();

    // Start Python debugger
    let python_script = executor
        .workspace_root
        .join("fixtures/python/trade_bot_forever.py");
    if !python_script.exists() {
        reporter.warn("trade_bot_forever.py not found, skipping test");
        return;
    }

    let step = reporter.step_start("Start debugpy", "Launch Python debugger");
    if let Err(e) = executor
        .start_debugpy(python_script.to_str().unwrap())
        .await
    {
        reporter.step_failed(step, &e);
        return;
    }
    reporter.step_success(step, Some("debugpy started"));

    // Start daemon
    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e);
        return;
    }
    reporter.step_success(step, Some("Daemon started"));

    let client = RestClient::new(executor.http_port);

    // Connect to Python
    let step = reporter.step_start("Connect Python", "Connect to debugpy");
    let py_conn = match client
        .create_connection("127.0.0.1", executor.debugpy_port, "python")
        .await
    {
        Ok(r) => {
            reporter.step_success(
                step,
                Some(&format!("Connection ID: {}", r.data.connection_id)),
            );
            r.data
        }
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            return;
        }
    };

    // Verify connection is active
    let step = reporter.step_start("Verify connected", "Check initial status");
    let connections = match client.list_connections().await {
        Ok(r) => r.data,
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            return;
        }
    };

    let is_connected = connections.iter().any(|c| {
        c.connection_id == py_conn.connection_id && c.status.to_lowercase() == "connected"
    });

    if is_connected {
        reporter.step_success(step, Some("Connection is active"));
    } else {
        reporter.step_failed(step, "Connection not in Connected state");
        return;
    }

    // Kill debugpy
    let step = reporter.step_start("Kill debugpy", "Terminate Python debugger");
    executor.kill_debugpy();
    sleep(Duration::from_secs(3)).await;
    reporter.step_success(step, Some("debugpy killed"));

    // Verify status updated
    let step = reporter.step_start("Verify disconnected", "Check status after crash");
    let connections = match client.list_connections().await {
        Ok(r) => r.data,
        Err(e) => {
            reporter.step_failed(step, &e.to_string());
            return;
        }
    };

    let status = connections
        .iter()
        .find(|c| c.connection_id == py_conn.connection_id)
        .map(|c| c.status.to_lowercase());

    let is_disconnected = status.as_ref().map(|s| s != "connected").unwrap_or(true);

    if is_disconnected {
        reporter.step_success(
            step,
            Some(&format!(
                "Status: {}",
                status.unwrap_or_else(|| "removed".to_string())
            )),
        );
    } else {
        reporter.step_failed(step, "Connection still shows as connected after crash");
        executor.print_daemon_logs(100);
        panic!("Status not updated after debugger crash");
    }

    reporter.section("TEST COMPLETE");
    reporter.info("✅ Python connection crash test PASSED");
}
