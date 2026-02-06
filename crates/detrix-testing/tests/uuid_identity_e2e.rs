//! UUID-Based Connection Identity Tests
//!
//! Tests that verify the UUID-based connection identity system provides:
//! 1. Stable identity across process restarts (same identity → same UUID)
//! 2. Workspace isolation (different directory → different UUID)
//! 3. Language isolation (same name, different language → different UUIDs)
//! 4. Hostname/machine isolation
//! 5. Deterministic UUID generation (same inputs → same output, always)
//!
//! The UUID generation is a pure domain function:
//!   UUID = SHA256(name|language|workspace_root|hostname)[0..16]
//!
//! This file contains both:
//! - Pure identity tests (fast, no daemon)
//! - Full E2E tests for Python, Go, and Rust (daemon + real debuggers)

use detrix_core::ConnectionIdentity;

// ============================================================================
// Helper: create connection with identity via REST API
// ============================================================================

/// Create a connection via REST API with full identity fields.
/// Returns the connection_id (UUID) from the response.
async fn create_connection_with_identity(
    http_port: u16,
    host: &str,
    port: u16,
    language: &str,
    name: &str,
    workspace_root: &str,
    hostname: &str,
    program: Option<&str>,
) -> Result<String, String> {
    let request = detrix_api::CreateConnectionRequest {
        host: host.to_string(),
        port: port.into(),
        language: language.to_string(),
        name: name.to_string(),
        workspace_root: workspace_root.to_string(),
        hostname: hostname.to_string(),
        metadata: None,
        program: program.map(|s| s.to_string()),
        safe_mode: false,
        pid: None,
    };

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/api/v1/connections", http_port))
        .json(&request)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;

    if !status.is_success() {
        return Err(format!("HTTP {} - {}", status, text));
    }

    let create_response: detrix_api::CreateConnectionResponse =
        serde_json::from_str(&text).map_err(|e| format!("Parse error: {}", e))?;

    Ok(create_response.connection_id)
}

// ============================================================================
// Pure UUID Identity Tests (no daemon needed - fast and reliable)
// ============================================================================

/// Same identity → same UUID (stable across restarts)
#[test]
fn test_same_identity_same_uuid() {
    let id1 = ConnectionIdentity::new("test-app", "python", "/workspace1", "test-host");
    let id2 = ConnectionIdentity::new("test-app", "python", "/workspace1", "test-host");
    assert_eq!(
        id1.to_uuid(),
        id2.to_uuid(),
        "Same identity must produce same UUID"
    );
}

/// Different workspace → different UUID
#[test]
fn test_different_workspace_different_uuid() {
    let uuid1 = ConnectionIdentity::new("test-app", "python", "/workspace1", "test-host").to_uuid();
    let uuid2 = ConnectionIdentity::new("test-app", "python", "/workspace2", "test-host").to_uuid();
    assert_ne!(
        uuid1, uuid2,
        "Different workspaces must produce different UUIDs"
    );
}

/// Different language → different UUID (cross-contamination prevention)
#[test]
fn test_different_language_different_uuid() {
    let py = ConnectionIdentity::new("trade-bot", "python", "/workspace", "host").to_uuid();
    let go = ConnectionIdentity::new("trade-bot", "go", "/workspace", "host").to_uuid();
    let rs = ConnectionIdentity::new("trade-bot", "rust", "/workspace", "host").to_uuid();
    assert_ne!(py, go, "Python and Go must have different UUIDs");
    assert_ne!(py, rs, "Python and Rust must have different UUIDs");
    assert_ne!(go, rs, "Go and Rust must have different UUIDs");
}

/// Different hostname → different UUID
#[test]
fn test_different_hostname_different_uuid() {
    let uuid1 = ConnectionIdentity::new("app", "python", "/workspace", "host1").to_uuid();
    let uuid2 = ConnectionIdentity::new("app", "python", "/workspace", "host2").to_uuid();
    assert_ne!(
        uuid1, uuid2,
        "Different hostnames must produce different UUIDs"
    );
}

/// UUID determinism and format
#[test]
fn test_uuid_determinism() {
    let identity = ConnectionIdentity::new("test-app", "python", "/workspace", "test-host");
    let uuid1 = identity.to_uuid();
    let uuid2 = identity.to_uuid();
    let uuid3 = identity.to_uuid();
    assert_eq!(uuid1, uuid2);
    assert_eq!(uuid2, uuid3);
    assert_eq!(uuid1.len(), 32, "UUID must be 32 hex characters");
    assert!(uuid1.chars().all(|c| c.is_ascii_hexdigit()));
}

/// Connection.id is the UUID (primary key = UUID)
#[test]
fn test_connection_id_is_uuid() {
    let identity = ConnectionIdentity::new("trade-bot", "python", "/workspace/project", "host1");
    let expected_uuid = identity.to_uuid();
    let connection =
        detrix_core::Connection::new_with_identity(identity, "127.0.0.1".to_string(), 5678)
            .expect("Should succeed");
    assert_eq!(connection.id.0, expected_uuid);
    assert_eq!(connection.name, Some("trade-bot".to_string()));
    assert_eq!(connection.workspace_root, "/workspace/project");
    assert_eq!(connection.hostname, "host1");
}

/// Validation rejects empty identity fields
#[test]
fn test_identity_validation() {
    assert!(ConnectionIdentity::new("", "python", "/ws", "host")
        .validate()
        .is_err());
    assert!(ConnectionIdentity::new("app", "", "/ws", "host")
        .validate()
        .is_err());
    assert!(ConnectionIdentity::new("app", "python", "", "host")
        .validate()
        .is_err());
    assert!(ConnectionIdentity::new("app", "python", "/ws", "")
        .validate()
        .is_err());
}

// ============================================================================
// E2E Tests: Python (daemon + debugpy)
// ============================================================================

/// E2E: Python connection returns correct UUID
#[tokio::test]
async fn test_e2e_python_uuid() {
    use detrix_testing::e2e::{
        availability::{require_tool, ToolDependency},
        executor::TestExecutor,
        reporter::TestReporter,
    };

    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let reporter = TestReporter::new("uuid_python_e2e", "REST");
    reporter.section("E2E: PYTHON UUID IDENTITY");

    let mut executor = TestExecutor::new();

    // Start daemon
    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some("Daemon started"));

    // Start debugpy
    let step = reporter.step_start("Start debugpy", "Launch debugpy process");
    let script_path = executor
        .detrix_example_app_path()
        .expect("Python fixture not found");
    if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
        reporter.step_failed(step, &e);
        panic!("Failed to start debugpy: {}", e);
    }
    reporter.step_success(step, Some(&format!("Port: {}", executor.debugpy_port)));

    // Compute expected UUID
    let identity = ConnectionIdentity::new(
        "py-uuid-test",
        "python",
        "/e2e/python-workspace",
        "e2e-host",
    );
    let expected_uuid = identity.to_uuid();

    // Create connection
    let step = reporter.step_start("Create connection", "POST with identity");
    let uuid = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        executor.debugpy_port,
        "python",
        "py-uuid-test",
        "/e2e/python-workspace",
        "e2e-host",
        None,
    )
    .await;

    match uuid {
        Ok(ref uuid) => {
            reporter.step_success(step, Some(&format!("UUID: {}", uuid)));
            assert_eq!(uuid, &expected_uuid, "API UUID must match computed UUID");
        }
        Err(ref e) => {
            reporter.step_failed(step, e);
            executor.print_daemon_logs(50);
            panic!("Failed: {}", e);
        }
    }

    // Verify idempotency
    let step = reporter.step_start("Idempotency", "Re-register same identity");
    let uuid2 = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        executor.debugpy_port,
        "python",
        "py-uuid-test",
        "/e2e/python-workspace",
        "e2e-host",
        None,
    )
    .await
    .expect("Re-register should succeed");

    assert_eq!(uuid2, expected_uuid, "Idempotent: same UUID on re-register");
    reporter.step_success(step, Some("Same UUID on re-register"));

    reporter.section("PYTHON E2E PASSED");
}

// ============================================================================
// E2E Tests: Go (daemon + delve)
// ============================================================================

/// E2E: Go connection returns correct UUID
#[tokio::test]
async fn test_e2e_go_uuid() {
    use detrix_testing::e2e::{
        availability::{require_tool, ToolDependency},
        executor::TestExecutor,
        reporter::TestReporter,
    };

    if !require_tool(ToolDependency::Delve).await {
        return;
    }

    let reporter = TestReporter::new("uuid_go_e2e", "REST");
    reporter.section("E2E: GO UUID IDENTITY");

    let mut executor = TestExecutor::new();

    // Start daemon
    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some("Daemon started"));

    // Start delve (builds Go fixture automatically)
    let step = reporter.step_start("Start delve", "Build Go fixture and launch delve");
    let go_source = executor
        .go_detrix_example_app_path()
        .expect("Go fixture not found");
    if let Err(e) = executor.start_delve(go_source.to_str().unwrap()).await {
        reporter.step_failed(step, &e);
        executor.print_delve_logs(50);
        panic!("Failed to start delve: {}", e);
    }
    reporter.step_success(step, Some(&format!("Port: {}", executor.delve_port)));

    // Compute expected UUID
    let identity = ConnectionIdentity::new("go-uuid-test", "go", "/e2e/go-workspace", "e2e-host");
    let expected_uuid = identity.to_uuid();

    // Create connection
    let step = reporter.step_start("Create connection", "POST with Go identity");
    let uuid = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        executor.delve_port,
        "go",
        "go-uuid-test",
        "/e2e/go-workspace",
        "e2e-host",
        None,
    )
    .await;

    match uuid {
        Ok(ref uuid) => {
            reporter.step_success(step, Some(&format!("UUID: {}", uuid)));
            assert_eq!(uuid, &expected_uuid, "API UUID must match computed UUID");
        }
        Err(ref e) => {
            reporter.step_failed(step, e);
            executor.print_daemon_logs(50);
            executor.print_delve_logs(30);
            panic!("Failed: {}", e);
        }
    }

    // Verify idempotency
    let step = reporter.step_start("Idempotency", "Re-register same identity");
    let uuid2 = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        executor.delve_port,
        "go",
        "go-uuid-test",
        "/e2e/go-workspace",
        "e2e-host",
        None,
    )
    .await
    .expect("Re-register should succeed");

    assert_eq!(uuid2, expected_uuid, "Idempotent: same UUID on re-register");
    reporter.step_success(step, Some("Same UUID on re-register"));

    reporter.section("GO E2E PASSED");
}

// ============================================================================
// E2E Tests: Rust (daemon + lldb-serve)
// ============================================================================

/// E2E: Rust connection returns correct UUID
#[tokio::test]
async fn test_e2e_rust_uuid() {
    use detrix_testing::e2e::{
        availability::{require_tool, ToolDependency},
        executor::TestExecutor,
        reporter::TestReporter,
    };

    if !require_tool(ToolDependency::LldbDap).await {
        return;
    }

    let reporter = TestReporter::new("uuid_rust_e2e", "REST");
    reporter.section("E2E: RUST UUID IDENTITY");

    let mut executor = TestExecutor::new();

    // Start daemon
    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some("Daemon started"));

    // Start lldb-serve (builds Rust fixture automatically)
    let step = reporter.step_start("Start lldb-serve", "Build Rust fixture and launch lldb");
    let rust_source = executor
        .rust_detrix_example_app_path()
        .expect("Rust fixture not found");
    if let Err(e) = executor.start_lldb(rust_source.to_str().unwrap()).await {
        reporter.step_failed(step, &e);
        executor.print_lldb_logs(50);
        reporter.info("Rust/lldb tests may not work on this platform");
        return;
    }
    reporter.step_success(step, Some(&format!("Port: {}", executor.lldb_port)));

    // Get binary path for launch mode (Rust requires program field)
    let program = executor
        .rust_binary_path
        .as_ref()
        .map(|p| p.to_str().unwrap());

    // Compute expected UUID
    let identity =
        ConnectionIdentity::new("rust-uuid-test", "rust", "/e2e/rust-workspace", "e2e-host");
    let expected_uuid = identity.to_uuid();

    // Create connection (Rust needs program path for launch mode)
    let step = reporter.step_start("Create connection", "POST with Rust identity");
    let uuid = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        executor.lldb_port,
        "rust",
        "rust-uuid-test",
        "/e2e/rust-workspace",
        "e2e-host",
        program,
    )
    .await;

    match uuid {
        Ok(ref uuid) => {
            reporter.step_success(step, Some(&format!("UUID: {}", uuid)));
            assert_eq!(uuid, &expected_uuid, "API UUID must match computed UUID");
        }
        Err(ref e) => {
            reporter.step_failed(step, e);
            executor.print_daemon_logs(50);
            executor.print_lldb_logs(30);
            panic!("Failed: {}", e);
        }
    }

    // Verify idempotency
    let step = reporter.step_start("Idempotency", "Re-register same identity");
    let uuid2 = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        executor.lldb_port,
        "rust",
        "rust-uuid-test",
        "/e2e/rust-workspace",
        "e2e-host",
        program,
    )
    .await
    .expect("Re-register should succeed");

    assert_eq!(uuid2, expected_uuid, "Idempotent: same UUID on re-register");
    reporter.step_success(step, Some("Same UUID on re-register"));

    reporter.section("RUST E2E PASSED");
}

// ============================================================================
// E2E Tests: Workspace Isolation (2 debugpy on different ports)
// ============================================================================

/// E2E: Different workspace → different UUID through full API
#[tokio::test]
async fn test_e2e_workspace_isolation() {
    use detrix_testing::e2e::{
        availability::{require_tool, ToolDependency},
        executor::{
            get_debugpy_port, register_e2e_process, start_debugpy_setsid, wait_for_port,
            TestExecutor,
        },
        reporter::TestReporter,
    };

    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let reporter = TestReporter::new("uuid_workspace_isolation", "REST");
    reporter.section("E2E: WORKSPACE ISOLATION");

    let mut executor = TestExecutor::new();

    // Start daemon
    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some("Daemon started"));

    let script_path = executor
        .detrix_example_app_path()
        .expect("Python fixture not found");
    let script = script_path.to_str().unwrap();

    // Start first debugpy (uses executor's built-in port)
    let step = reporter.step_start("Start debugpy #1", "First Python debugger");
    if let Err(e) = executor.start_debugpy(script).await {
        reporter.step_failed(step, &e);
        panic!("Failed to start debugpy #1: {}", e);
    }
    let port1 = executor.debugpy_port;
    reporter.step_success(step, Some(&format!("Port: {}", port1)));

    // Start second debugpy (manually managed)
    let step = reporter.step_start("Start debugpy #2", "Second Python debugger");
    let port2 = get_debugpy_port();
    let mut debugpy2 = start_debugpy_setsid(port2, script)
        .map_err(|e| e.to_string())
        .expect("Failed to spawn debugpy #2");
    register_e2e_process("debugpy", debugpy2.id());

    if !wait_for_port(port2, 10).await {
        let _ = debugpy2.kill();
        reporter.step_failed(step, &format!("debugpy #2 not listening on {}", port2));
        panic!("debugpy #2 timeout");
    }
    reporter.step_success(step, Some(&format!("Port: {}", port2)));

    // Create connection in /workspace/dir1
    let step = reporter.step_start(
        "Connection (workspace1)",
        "Create connection in /workspace/dir1",
    );
    let uuid1 = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        port1,
        "python",
        "test-app",
        "/workspace/dir1",
        "e2e-host",
        None,
    )
    .await;

    let uuid1 = match uuid1 {
        Ok(uuid) => {
            reporter.step_success(step, Some(&format!("UUID: {}", uuid)));
            uuid
        }
        Err(e) => {
            reporter.step_failed(step, &e);
            executor.print_daemon_logs(50);
            let _ = debugpy2.kill();
            panic!("Failed: {}", e);
        }
    };

    // Create connection in /workspace/dir2 (different workspace)
    let step = reporter.step_start(
        "Connection (workspace2)",
        "Create connection in /workspace/dir2",
    );
    let uuid2 = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        port2,
        "python",
        "test-app",        // Same name
        "/workspace/dir2", // Different workspace
        "e2e-host",        // Same hostname
        None,
    )
    .await;

    let uuid2 = match uuid2 {
        Ok(uuid) => {
            reporter.step_success(step, Some(&format!("UUID: {}", uuid)));
            uuid
        }
        Err(e) => {
            reporter.step_failed(step, &e);
            executor.print_daemon_logs(50);
            let _ = debugpy2.kill();
            panic!("Failed: {}", e);
        }
    };

    // Verify UUIDs differ
    let step = reporter.step_start("Verify isolation", "UUID1 != UUID2");
    assert_ne!(
        uuid1, uuid2,
        "Different workspaces must produce different UUIDs"
    );
    reporter.step_success(step, Some(&format!("Isolated: {} vs {}", uuid1, uuid2)));

    // Verify UUIDs match computed values
    let expected1 =
        ConnectionIdentity::new("test-app", "python", "/workspace/dir1", "e2e-host").to_uuid();
    let expected2 =
        ConnectionIdentity::new("test-app", "python", "/workspace/dir2", "e2e-host").to_uuid();
    assert_eq!(uuid1, expected1, "UUID1 must match computed value");
    assert_eq!(uuid2, expected2, "UUID2 must match computed value");

    // Cleanup second debugpy
    let _ = debugpy2.kill();
    let _ = debugpy2.wait();

    reporter.section("WORKSPACE ISOLATION E2E PASSED");
}

// ============================================================================
// E2E Tests: Cross-Language Isolation (Python + Go + Rust)
// ============================================================================

/// E2E: Same name in different languages → different UUIDs through full API
///
/// This is the critical test that prevents the cross-contamination bug:
/// Go "trade-bot" must NOT load Python "trade-bot" metrics.
#[tokio::test]
async fn test_e2e_language_isolation() {
    use detrix_testing::e2e::{
        availability::{require_tool, ToolDependency},
        executor::TestExecutor,
        reporter::TestReporter,
    };

    // Require all three debuggers
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }
    if !require_tool(ToolDependency::Delve).await {
        return;
    }
    if !require_tool(ToolDependency::LldbDap).await {
        return;
    }

    let reporter = TestReporter::new("uuid_language_isolation", "REST");
    reporter.section("E2E: CROSS-LANGUAGE ISOLATION (Python + Go + Rust)");

    let mut executor = TestExecutor::new();

    // Start daemon
    let step = reporter.step_start("Start daemon", "Launch Detrix daemon");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some("Daemon started"));

    // Start debugpy (Python)
    let step = reporter.step_start("Start debugpy", "Python debugger");
    let py_script = executor
        .detrix_example_app_path()
        .expect("Python fixture not found");
    if let Err(e) = executor.start_debugpy(py_script.to_str().unwrap()).await {
        reporter.step_failed(step, &e);
        panic!("Failed to start debugpy: {}", e);
    }
    reporter.step_success(step, Some(&format!("Port: {}", executor.debugpy_port)));

    // Start delve (Go)
    let step = reporter.step_start("Start delve", "Go debugger");
    let go_source = executor
        .go_detrix_example_app_path()
        .expect("Go fixture not found");
    if let Err(e) = executor.start_delve(go_source.to_str().unwrap()).await {
        reporter.step_failed(step, &e);
        executor.print_delve_logs(50);
        panic!("Failed to start delve: {}", e);
    }
    reporter.step_success(step, Some(&format!("Port: {}", executor.delve_port)));

    // Start lldb-serve (Rust)
    let step = reporter.step_start("Start lldb-serve", "Rust debugger");
    let rust_source = executor
        .rust_detrix_example_app_path()
        .expect("Rust fixture not found");
    if let Err(e) = executor.start_lldb(rust_source.to_str().unwrap()).await {
        reporter.step_failed(step, &e);
        executor.print_lldb_logs(50);
        reporter.info("Rust/lldb may not work on this platform - skipping Rust");
        return;
    }
    reporter.step_success(step, Some(&format!("Port: {}", executor.lldb_port)));

    let rust_program = executor
        .rust_binary_path
        .as_ref()
        .map(|p| p.to_str().unwrap());

    // All debuggers running - same identity name for all three
    let workspace = "/e2e/shared-workspace";
    let hostname = "e2e-host";
    let app_name = "trade-bot"; // Same name for all three

    // Create Python connection
    let step = reporter.step_start("Python connection", "trade-bot (python)");
    let py_uuid = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        executor.debugpy_port,
        "python",
        app_name,
        workspace,
        hostname,
        None,
    )
    .await;
    let py_uuid = match py_uuid {
        Ok(uuid) => {
            reporter.step_success(step, Some(&format!("Python UUID: {}", uuid)));
            uuid
        }
        Err(e) => {
            reporter.step_failed(step, &e);
            executor.print_daemon_logs(50);
            panic!("Python connection failed: {}", e);
        }
    };

    // Create Go connection
    let step = reporter.step_start("Go connection", "trade-bot (go)");
    let go_uuid = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        executor.delve_port,
        "go",
        app_name,
        workspace,
        hostname,
        None,
    )
    .await;
    let go_uuid = match go_uuid {
        Ok(uuid) => {
            reporter.step_success(step, Some(&format!("Go UUID: {}", uuid)));
            uuid
        }
        Err(e) => {
            reporter.step_failed(step, &e);
            executor.print_daemon_logs(50);
            executor.print_delve_logs(30);
            panic!("Go connection failed: {}", e);
        }
    };

    // Create Rust connection
    let step = reporter.step_start("Rust connection", "trade-bot (rust)");
    let rust_uuid = create_connection_with_identity(
        executor.http_port,
        "127.0.0.1",
        executor.lldb_port,
        "rust",
        app_name,
        workspace,
        hostname,
        rust_program,
    )
    .await;
    let rust_uuid = match rust_uuid {
        Ok(uuid) => {
            reporter.step_success(step, Some(&format!("Rust UUID: {}", uuid)));
            uuid
        }
        Err(e) => {
            reporter.step_failed(step, &e);
            executor.print_daemon_logs(50);
            executor.print_lldb_logs(30);
            panic!("Rust connection failed: {}", e);
        }
    };

    // Verify all three UUIDs are different
    let step = reporter.step_start("Verify isolation", "All UUIDs must differ");
    assert_ne!(py_uuid, go_uuid, "Python != Go");
    assert_ne!(py_uuid, rust_uuid, "Python != Rust");
    assert_ne!(go_uuid, rust_uuid, "Go != Rust");
    reporter.step_success(
        step,
        Some(&format!(
            "All isolated: py={}, go={}, rs={}",
            &py_uuid[..8],
            &go_uuid[..8],
            &rust_uuid[..8]
        )),
    );

    // Verify UUIDs match computed values
    let step = reporter.step_start("Verify correctness", "UUIDs match computed values");
    let expected_py = ConnectionIdentity::new(app_name, "python", workspace, hostname).to_uuid();
    let expected_go = ConnectionIdentity::new(app_name, "go", workspace, hostname).to_uuid();
    let expected_rs = ConnectionIdentity::new(app_name, "rust", workspace, hostname).to_uuid();
    assert_eq!(py_uuid, expected_py, "Python UUID must match computed");
    assert_eq!(go_uuid, expected_go, "Go UUID must match computed");
    assert_eq!(rust_uuid, expected_rs, "Rust UUID must match computed");
    reporter.step_success(step, Some("All UUIDs match computed values"));

    reporter.section("CROSS-LANGUAGE ISOLATION E2E PASSED");
    reporter.info("Go 'trade-bot' will NOT load Python 'trade-bot' metrics");
}
