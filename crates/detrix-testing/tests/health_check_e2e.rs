//! Health Check E2E Tests
//!
//! Tests that the daemon health check correctly identifies detrix daemons
//! and rejects other services that happen to have a /health endpoint.
//!
//! This prevents MCP from accidentally connecting to the wrong service
//! when a port is reused.

use axum::{routing::get, Json, Router};
use detrix_api::http::handlers::HealthResponse;
use detrix_testing::e2e::{
    cleanup_orphaned_e2e_processes,
    executor::{find_detrix_binary, get_http_port, get_workspace_root},
    register_e2e_process,
    reporter::TestReporter,
    unregister_e2e_process,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::process::Command;

/// Generic health response from other services (no service field)
#[derive(Debug, Serialize, Deserialize)]
struct GenericHealthResponse {
    status: String,
    uptime: u64,
}

/// Response format used by is_daemon_healthy to check service identity
#[derive(Debug, Deserialize)]
struct HealthCheckResponse {
    service: Option<String>,
}

/// Replicate the is_daemon_healthy logic from detrix-cli
async fn is_daemon_healthy(host: &str, port: u16) -> bool {
    let url = format!("http://{}:{}/health", host, port);

    match reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(response) => {
            if !response.status().is_success() {
                return false;
            }
            match response.json::<HealthCheckResponse>().await {
                Ok(health) => health.service.as_deref() == Some("detrix"),
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}

/// Wait for daemon to become healthy
async fn wait_for_daemon(host: &str, port: u16, max_wait: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < max_wait {
        if is_daemon_healthy(host, port).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Start a mock "other service" HTTP server
async fn start_mock_other_service(port: u16) -> tokio::task::JoinHandle<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    let app = Router::new().route(
        "/health",
        get(|| async {
            // Return health response WITHOUT the "service" field
            Json(GenericHealthResponse {
                status: "ok".to_string(),
                uptime: 123,
            })
        }),
    );

    let listener = TcpListener::bind(addr).await.unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    })
}

// ============================================================================
// Test: Real detrix daemon health check returns true
// ============================================================================

/// Test that is_daemon_healthy returns true for a real detrix daemon
/// and false for a mock service without the 'service' field.
#[cfg(unix)]
#[tokio::test]
async fn test_health_check_real_daemon_vs_mock_service() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("health_check_e2e", "E2E");
    reporter.section("HEALTH CHECK E2E TEST");
    reporter.info("Testing health check identifies real detrix vs other services");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let step = reporter.step_start("Find binary", "Locate detrix binary");
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => {
            reporter.step_success(step, Some(&format!("Found: {}", path.display())));
            path
        }
        None => {
            reporter.step_failed(step, "Binary not found");
            reporter.warn("Skipping test: detrix binary not built");
            reporter.warn("Run `cargo build --release -p detrix-cli` to build");
            return;
        }
    };

    // Create temp directory for test
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");

    // Get unique ports
    let daemon_port = get_http_port();
    let grpc_port = get_http_port();
    let mock_port = get_http_port();

    reporter.info(&format!(
        "Daemon HTTP: {}, gRPC: {}, Mock: {}",
        daemon_port, grpc_port, mock_port
    ));

    // Create minimal config
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"

[api]
port_fallback = false

[api.rest]
port = {}

[api.grpc]
port = {}
"#,
        db_path.display(),
        pid_path.display(),
        daemon_port,
        grpc_port
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // PHASE 1: Start mock "other service"
    // ========================================================================
    reporter.section("PHASE 1: START MOCK SERVICE");

    let step = reporter.step_start("Start mock", "Start mock service without 'service' field");
    let mock_handle = start_mock_other_service(mock_port).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    reporter.step_success(step, Some(&format!("Mock running on port {}", mock_port)));

    // Verify mock service health check FAILS (no 'service' field)
    let step = reporter.step_start("Check mock", "Verify health check rejects mock service");
    let mock_healthy = is_daemon_healthy("127.0.0.1", mock_port).await;
    if mock_healthy {
        reporter.step_failed(step, "Mock service should NOT pass health check");
        mock_handle.abort();
        panic!("is_daemon_healthy should return false for mock service");
    }
    reporter.step_success(step, Some("Mock correctly rejected (no 'service' field)"));

    // ========================================================================
    // PHASE 2: Start real detrix daemon
    // ========================================================================
    reporter.section("PHASE 2: START REAL DAEMON");

    let step = reporter.step_start("Start daemon", "Start real detrix daemon");
    let mut daemon = Command::new(&binary)
        .arg("serve")
        .arg("--config")
        .arg(&config_path)
        .arg("--pid-file")
        .arg(&pid_path)
        .arg("--daemon")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start daemon");

    let daemon_pid = daemon.id().unwrap();
    register_e2e_process("health_test_daemon", daemon_pid);
    reporter.step_success(step, Some(&format!("Daemon PID: {}", daemon_pid)));

    // Wait for daemon to be healthy
    let step = reporter.step_start("Wait healthy", "Wait for daemon to become healthy");
    let daemon_healthy = wait_for_daemon("127.0.0.1", daemon_port, Duration::from_secs(10)).await;
    if !daemon_healthy {
        reporter.step_failed(step, "Daemon did not become healthy");
        let _ = daemon.kill().await;
        unregister_e2e_process("health_test_daemon", daemon_pid);
        mock_handle.abort();
        panic!("Daemon did not become healthy within timeout");
    }
    reporter.step_success(step, Some("Daemon healthy"));

    // ========================================================================
    // PHASE 3: Verify health check works correctly
    // ========================================================================
    reporter.section("PHASE 3: VERIFY HEALTH CHECKS");

    // Real daemon should pass
    let step = reporter.step_start("Check daemon", "Verify real daemon passes health check");
    let real_healthy = is_daemon_healthy("127.0.0.1", daemon_port).await;
    if !real_healthy {
        reporter.step_failed(step, "Real daemon should pass health check");
        let _ = daemon.kill().await;
        unregister_e2e_process("health_test_daemon", daemon_pid);
        mock_handle.abort();
        panic!("is_daemon_healthy should return true for real daemon");
    }
    reporter.step_success(step, Some("Real daemon correctly accepted"));

    // Verify we can read the actual health response
    let step = reporter.step_start(
        "Verify response",
        "Check health response has 'service: detrix'",
    );
    let url = format!("http://127.0.0.1:{}/health", daemon_port);
    let response: HealthResponse = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .expect("Failed to fetch health")
        .json()
        .await
        .expect("Failed to parse health response");

    assert_eq!(
        response.service, "detrix",
        "Health response should have service='detrix'"
    );
    reporter.step_success(
        step,
        Some(&format!(
            "service='{}', status='{}'",
            response.service, response.status
        )),
    );

    // ========================================================================
    // PHASE 4: Cleanup
    // ========================================================================
    reporter.section("PHASE 4: CLEANUP");

    let step = reporter.step_start("Stop daemon", "Kill daemon process");
    let _ = daemon.kill().await;
    unregister_e2e_process("health_test_daemon", daemon_pid);
    reporter.step_success(step, None);

    let step = reporter.step_start("Stop mock", "Stop mock service");
    mock_handle.abort();
    reporter.step_success(step, None);

    reporter.section("TEST PASSED");
    reporter.info("Health check correctly distinguishes detrix from other services");
}

// ============================================================================
// Test: Verify health response format from real daemon
// ============================================================================

/// Verify that the real daemon returns the expected health response format
#[cfg(unix)]
#[tokio::test]
async fn test_daemon_health_response_format() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("health_format_e2e", "E2E");
    reporter.section("HEALTH RESPONSE FORMAT TEST");

    let workspace_root = get_workspace_root();
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping: detrix binary not found");
            return;
        }
    };

    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let port = get_http_port();
    let grpc_port = get_http_port();

    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"

[api]
port_fallback = false

[api.rest]
port = {}

[api.grpc]
port = {}
"#,
        db_path.display(),
        pid_path.display(),
        port,
        grpc_port
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // Start daemon
    let mut daemon = Command::new(&binary)
        .arg("serve")
        .arg("--config")
        .arg(&config_path)
        .arg("--pid-file")
        .arg(&pid_path)
        .arg("--daemon")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start daemon");

    let daemon_pid = daemon.id().unwrap();
    register_e2e_process("health_format_daemon", daemon_pid);

    // Wait for healthy
    if !wait_for_daemon("127.0.0.1", port, Duration::from_secs(10)).await {
        let _ = daemon.kill().await;
        unregister_e2e_process("health_format_daemon", daemon_pid);
        panic!("Daemon did not become healthy");
    }

    // Fetch and verify health response format
    let url = format!("http://127.0.0.1:{}/health", port);
    let response: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .expect("Failed to fetch")
        .json()
        .await
        .expect("Failed to parse");

    // Verify required fields exist
    assert!(
        response.get("service").is_some(),
        "Health response must have 'service' field"
    );
    assert_eq!(
        response["service"].as_str(),
        Some("detrix"),
        "service must be 'detrix'"
    );
    assert!(
        response.get("status").is_some(),
        "Health response must have 'status' field"
    );
    assert!(
        response.get("uptimeSeconds").is_some(),
        "Health response must have 'uptimeSeconds' field"
    );

    reporter.info(&format!("Health response: {}", response));
    reporter.section("TEST PASSED");

    // Cleanup
    let _ = daemon.kill().await;
    unregister_e2e_process("health_format_daemon", daemon_pid);
}
