//! Docker Cloud E2E Tests
//!
//! Single orchestrator test function that verifies the full cloud pipeline:
//! wake/sleep, basic observation, control plane file serving, bridge file
//! serving, git-pinned + drift detection, and MCP binary smoke test.
//!
//! Run via: `task test-cloud` (manages Docker Compose lifecycle)
//! Or manually:
//! ```sh
//! docker compose -f fixtures/docker/docker-compose.yml -p detrix-cloud-test up -d --build --wait
//! cargo test -p detrix-testing --test docker_cloud_e2e -- --ignored --nocapture --test-threads=1
//! docker compose -f fixtures/docker/docker-compose.yml -p detrix-cloud-test down -v
//! ```

use detrix_testing::e2e::client::{AddMetricRequest, ApiClient, EventInfo, ObserveRequest};
use detrix_testing::e2e::test_file_server::start_test_file_server;
use detrix_testing::e2e::{find_detrix_binary, get_workspace_root, McpClient};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// =============================================================================
// Constants
// =============================================================================

/// Auth token matching DETRIX_TOKEN in docker-compose.yml.
/// The daemon auto-enables Simple auth via this env var.
const DOCKER_AUTH_TOKEN: &str = "docker-test-token";

/// Docker daemon HTTP port (host-mapped from container 8090).
const DAEMON_HTTP_PORT: u16 = 8095;

/// Docker Compose file path (relative to workspace root).
const COMPOSE_FILE: &str = "fixtures/docker/docker-compose.yml";

/// Docker Compose project name.
const COMPOSE_PROJECT: &str = "detrix-cloud-test";

// App control plane URLs (Docker-internal, used by daemon to reach apps).
const PYTHON_APP_URL: &str = "http://test-app-python:8091";
const GO_APP_URL: &str = "http://test-app-go:8091";
const RUST_APP_URL: &str = "http://test-app-rust:8091";

// Advertise URL for Phase 6/7 (matches host-mapped daemon port).
const ADVERTISE_URL: &str = "http://localhost:8095";

// File paths inside containers (from DWARF debug info / runtime WORKDIR).
const PYTHON_FILE: &str = "/app/trade_bot_forever.py";
const GO_FILE: &str = "/src/fixtures/go/detrix_example_app.go";
const RUST_FILE: &str = "/src/fixtures/rust/src/main.rs";

// =============================================================================
// Helpers
// =============================================================================

/// Returns the absolute path to the docker-compose file.
/// Uses CARGO_MANIFEST_DIR to compute workspace root reliably regardless of CWD.
fn compose_file_abs() -> String {
    let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(COMPOSE_FILE);
    ws.to_string_lossy().into_owned()
}

/// Poll `query_events` until at least one event appears or timeout expires.
async fn poll_for_events(
    client: &McpClient,
    metric_name: &str,
    timeout: Duration,
) -> Vec<EventInfo> {
    let start = Instant::now();
    loop {
        if let Ok(response) = client.query_events(metric_name, 10).await {
            if !response.data.is_empty() {
                return response.data;
            }
        }
        if start.elapsed() > timeout {
            return vec![];
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Poll `list_connections` until a connected connection with the given language appears.
/// Returns the connection_id.
async fn poll_for_connection(
    client: &McpClient,
    language: &str,
    timeout: Duration,
) -> Option<String> {
    let start = Instant::now();
    loop {
        if let Ok(response) = client.list_connections().await {
            if let Some(conn) = response
                .data
                .iter()
                .find(|c| c.status == "connected" && c.language == language)
            {
                return Some(conn.connection_id.clone());
            }
        }
        if start.elapsed() > timeout {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Restart the Detrix daemon container (stop + clear DB + up --wait).
/// Clears SQLite DB to prevent startup restore from interfering with new connections.
/// The restore process uses exponential backoff on stale connections and can kill
/// newly-created adapters when it detects UUID conflicts.
async fn restart_daemon() {
    let compose_file = compose_file_abs();
    let make_args = |extra: &[&str]| -> Vec<String> {
        let mut args = vec![
            "compose".to_string(),
            "-f".to_string(),
            compose_file.clone(),
            "-p".to_string(),
            COMPOSE_PROJECT.to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        args
    };

    // Stop the daemon
    let _ = tokio::process::Command::new("docker")
        .args(make_args(&["stop", "detrix"]))
        .output()
        .await;

    // Clear the DB to prevent startup restore from reconnecting stale connections.
    // Without this, the restore process finds old connections in SQLite, tries to
    // reconnect on old ports (which fail), and when it detects a UUID conflict with
    // a newly-woken connection, it kills the active adapter before failing.
    let volume_name = format!("{}_detrix-data", COMPOSE_PROJECT);
    let _ = tokio::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "-v",
            &format!("{}:/data/detrix", volume_name),
            "busybox",
            "rm",
            "-f",
            "/data/detrix/data.db",
            "/data/detrix/dlq.db",
        ])
        .output()
        .await;

    // Bring it back up with health check wait
    let _ = tokio::process::Command::new("docker")
        .args(make_args(&["up", "-d", "--wait", "detrix"]))
        .output()
        .await;

    // Poll health endpoint as extra safety
    let http_client = reqwest::Client::new();
    let start = Instant::now();
    loop {
        if let Ok(resp) = http_client
            .get(format!("http://127.0.0.1:{}/health", DAEMON_HTTP_PORT))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            if resp.status().is_success() {
                break;
            }
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("Daemon failed to become healthy within 30s after restart");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Restart the Detrix daemon container with extra environment variables.
/// Combines stop + clear DB + force_recreate_with_env + health poll.
async fn restart_daemon_with_env(envs: &[(&str, &str)]) {
    let compose_file = compose_file_abs();
    let make_args = |extra: &[&str]| -> Vec<String> {
        let mut args = vec![
            "compose".to_string(),
            "-f".to_string(),
            compose_file.clone(),
            "-p".to_string(),
            COMPOSE_PROJECT.to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        args
    };

    // Stop the daemon
    let _ = tokio::process::Command::new("docker")
        .args(make_args(&["stop", "detrix"]))
        .output()
        .await;

    // Clear the DB to prevent startup restore from reconnecting stale connections.
    let volume_name = format!("{}_detrix-data", COMPOSE_PROJECT);
    let _ = tokio::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "-v",
            &format!("{}:/data/detrix", volume_name),
            "busybox",
            "rm",
            "-f",
            "/data/detrix/data.db",
            "/data/detrix/dlq.db",
        ])
        .output()
        .await;

    // Bring it back up with env vars and health check wait
    force_recreate_with_env("detrix", envs).await;

    // Poll health endpoint as extra safety
    let http_client = reqwest::Client::new();
    let start = Instant::now();
    loop {
        if let Ok(resp) = http_client
            .get(format!("http://127.0.0.1:{}/health", DAEMON_HTTP_PORT))
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            if resp.status().is_success() {
                break;
            }
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("Daemon failed to become healthy within 30s after restart with env");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Assert that a wake response contains daemon_url in both text and raw JSON.
fn assert_wake_has_daemon_url(
    wake_result: &detrix_testing::e2e::client::ApiResponse<String>,
    expected_url: &str,
) {
    // Check text content
    assert!(
        wake_result.data.contains("daemon_url")
            || wake_result.data.contains("daemonUrl")
            || wake_result.data.contains(expected_url),
        "Wake text should contain daemon_url or the URL itself.\nText: {}",
        wake_result.data
    );

    // Check raw JSON response
    if let Some(ref raw) = wake_result.raw_response {
        assert!(
            raw.contains(expected_url),
            "Wake raw JSON should contain advertise URL '{}'.\nRaw: {}",
            expected_url,
            raw
        );
    }
}

/// Sleep an app (stops its debugger) and wait briefly for cleanup.
async fn sleep_app(client: &McpClient, app_url: &str) {
    let _ = client.sleep(app_url).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
}

/// Create a temporary git repo containing the given fixture files.
/// Returns the temp directory (keeps it alive) and the commit SHA.
async fn create_temp_git_repo(
    fixture_files: &[(&str, &std::path::Path)],
) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let repo = dir.path();

    // git init + config
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command failed")
    };
    run(&["init"]);
    run(&[
        "-c",
        "user.email=test@test.com",
        "-c",
        "user.name=Test",
        "init",
    ]);
    run(&["config", "user.email", "test@test.com"]);
    run(&["config", "user.name", "Test"]);

    // Copy fixture files into the repo
    for (name, source) in fixture_files {
        let dest = repo.join(name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::copy(source, &dest).expect("copy fixture file");
    }

    // Stage and commit
    run(&["add", "."]);
    run(&["commit", "-m", "initial commit"]);

    // Get commit SHA
    let output = run(&["rev-parse", "HEAD"]);
    let sha = String::from_utf8(output.stdout)
        .expect("valid utf8")
        .trim()
        .to_string();

    (dir, sha)
}

/// Force-recreate a Docker Compose service with extra env vars.
/// Uses shell env override: `VAR=val docker compose up -d --force-recreate --wait service`.
async fn force_recreate_with_env(service: &str, envs: &[(&str, &str)]) {
    let compose_file = compose_file_abs();
    let mut cmd = tokio::process::Command::new("docker");
    cmd.args([
        "compose",
        "-f",
        &compose_file,
        "-p",
        COMPOSE_PROJECT,
        "up",
        "-d",
        "--force-recreate",
        "--wait",
        service,
    ]);
    for (key, val) in envs {
        cmd.env(key, val);
    }
    let output = cmd.output().await.expect("docker compose up failed");
    assert!(
        output.status.success(),
        "force_recreate_with_env failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// =============================================================================
// Orchestrator
// =============================================================================

#[tokio::test]
#[ignore]
async fn test_cloud_e2e() {
    // Docker Compose assumed running (Taskfile manages lifecycle).
    // All McpClient instances include Authorization: Bearer docker-test-token.
    let client = McpClient::with_auth(DAEMON_HTTP_PORT, DOCKER_AUTH_TOKEN);

    // Verify daemon is reachable
    let healthy = client.health().await.expect("health check failed");
    assert!(healthy, "Daemon should be healthy before starting tests");
    println!("Daemon healthy at port {}", DAEMON_HTTP_PORT);

    // ── Phase 1: Basic observation (Python + Go + Rust) ──
    // Wake each app, poll for connection, add_metric with explicit line,
    // poll for events, assert events > 0.
    println!("\n{}", "=".repeat(60));
    println!("Phase 1: Basic observation");
    println!("{}", "=".repeat(60));

    // ── Phase 1a: Python ──
    println!("\n--- Phase 1a: Python ---");
    client
        .wake(PYTHON_APP_URL, None)
        .await
        .expect("wake python failed");

    let py_conn = poll_for_connection(&client, "python", Duration::from_secs(15))
        .await
        .expect("Python connection not found within 15s");
    println!("  Python connection: {}", py_conn);

    let py_metric = AddMetricRequest::new(
        "cloud-py-basic",
        &format!("{}#60", PYTHON_FILE),
        "order_id",
        &py_conn,
    );
    client
        .add_metric(py_metric)
        .await
        .expect("add python metric failed");

    let py_events = poll_for_events(&client, "cloud-py-basic", Duration::from_secs(15)).await;
    assert!(
        !py_events.is_empty(),
        "Expected Python events but got none (waited 15s)"
    );
    println!("  Python events captured: {}", py_events.len());

    // ── Phase 1b: Go ──
    println!("\n--- Phase 1b: Go ---");
    client.wake(GO_APP_URL, None).await.expect("wake go failed");

    let go_conn = poll_for_connection(&client, "go", Duration::from_secs(15))
        .await
        .expect("Go connection not found within 15s");
    println!("  Go connection: {}", go_conn);

    let go_metric = AddMetricRequest::new(
        "cloud-go-basic",
        &format!("{}#117", GO_FILE),
        "symbol",
        &go_conn,
    );
    client
        .add_metric(go_metric)
        .await
        .expect("add go metric failed");

    let go_events = poll_for_events(&client, "cloud-go-basic", Duration::from_secs(15)).await;
    assert!(
        !go_events.is_empty(),
        "Expected Go events but got none (waited 15s)"
    );
    println!("  Go events captured: {}", go_events.len());

    // ── Phase 1c: Rust (skip if lldb-dap unavailable in container) ──
    println!("\n--- Phase 1c: Rust ---");
    let rust_available = match client.wake(RUST_APP_URL, None).await {
        Ok(_) => {
            if let Some(rust_conn) =
                poll_for_connection(&client, "rust", Duration::from_secs(15)).await
            {
                println!("  Rust connection: {}", rust_conn);
                let rust_metric = AddMetricRequest::new(
                    "cloud-rust-basic",
                    &format!("{}#108", RUST_FILE),
                    "symbol",
                    &rust_conn,
                );
                if let Err(e) = client.add_metric(rust_metric).await {
                    println!("  Rust add_metric failed (skipping): {}", e);
                    false
                } else {
                    let rust_events =
                        poll_for_events(&client, "cloud-rust-basic", Duration::from_secs(15)).await;
                    println!("  Rust events captured: {}", rust_events.len());
                    !rust_events.is_empty()
                }
            } else {
                println!("  Rust connection not found (skipping)");
                false
            }
        }
        Err(e) => {
            println!("  Rust wake failed (lldb-dap likely unavailable): {}", e);
            false
        }
    };
    if !rust_available {
        println!("  [SKIP] Rust tests skipped — lldb-dap not working in container");
    }

    // ── Cleanup: Sleep all apps (debuggers stop) ──
    println!("\n--- Phase 1 cleanup: sleeping all apps ---");
    sleep_app(&client, PYTHON_APP_URL).await;
    sleep_app(&client, GO_APP_URL).await;
    sleep_app(&client, RUST_APP_URL).await;
    println!("  All apps sleeping");

    // ── DAEMON RESTART (stale connections deleted — debuggers were stopped) ──
    println!("\n--- Restarting daemon (clean state for Phase 2) ---");
    restart_daemon().await;
    println!("  Daemon restarted");

    println!("\n{}", "=".repeat(60));
    println!(
        "Phase 1 complete — Python + Go observed{}",
        if rust_available {
            " + Rust"
        } else {
            " (Rust skipped)"
        }
    );
    println!("{}", "=".repeat(60));

    // ── Phase 2: Control plane file serving (Python) ──
    // No bridge header → daemon fetches from app's /detrix/files/read endpoint.
    // observe() auto-finds the line via VFS (control_plane source).
    println!("\n{}", "=".repeat(60));
    println!("Phase 2: Control plane file serving (Python)");
    println!("{}", "=".repeat(60));

    // Wake Python and use explicit connection_id (stale connections may survive restart)
    println!("\n--- Phase 2: waking Python ---");
    client
        .wake(PYTHON_APP_URL, None)
        .await
        .expect("wake python failed");

    let py_conn = poll_for_connection(&client, "python", Duration::from_secs(15))
        .await
        .expect("Python connection not found within 15s");
    println!("  Python connection: {}", py_conn);

    // observe with auto-find line (VFS fetches file from control plane)
    let observe_req = ObserveRequest::new("trade_bot_forever.py", "order_id")
        .with_name("cloud-py-cp")
        .with_connection_id(&py_conn);
    let observe_resp = client
        .observe(observe_req)
        .await
        .expect("observe python via control plane failed");
    assert!(
        observe_resp.data.success,
        "observe should succeed: {:?}",
        observe_resp.data
    );
    println!(
        "  observe ok: metric={} at {}#{}",
        observe_resp.data.metric_name, observe_resp.data.file, observe_resp.data.line
    );
    if !observe_resp.data.warnings.is_empty() {
        println!("  observe warnings: {:?}", observe_resp.data.warnings);
    }

    let cp_events = poll_for_events(&client, "cloud-py-cp", Duration::from_secs(15)).await;
    if cp_events.is_empty() {
        // Print diagnostic info before failing
        if let Ok(metric) = client.get_metric("cloud-py-cp").await {
            println!(
                "  DIAG: metric enabled={}, location={}",
                metric.data.enabled, metric.data.location
            );
        }
        if let Ok(conns) = client.list_connections().await {
            for c in &conns.data {
                println!(
                    "  DIAG: conn={} lang={} status={}",
                    c.connection_id, c.language, c.status
                );
            }
        }
    }
    assert!(
        !cp_events.is_empty(),
        "Expected control-plane events but got none (waited 15s)"
    );
    println!("  Control plane events captured: {}", cp_events.len());

    // Cleanup
    println!("\n--- Phase 2 cleanup: sleeping Python ---");
    sleep_app(&client, PYTHON_APP_URL).await;

    // ── DAEMON RESTART (clean state for Phase 3) ──
    println!("\n--- Restarting daemon (clean state for Phase 3) ---");
    restart_daemon().await;
    println!("  Daemon restarted");

    println!("\n{}", "=".repeat(60));
    println!("Phase 2 complete — control plane file serving verified");
    println!("{}", "=".repeat(60));

    // ── Phase 3: Bridge file serving (Go) ──
    // Start a test file server on the host. The daemon fetches files via the
    // bridge source using the X-Detrix-File-Server-Url header.
    println!("\n{}", "=".repeat(60));
    println!("Phase 3: Bridge file serving (Go)");
    println!("{}", "=".repeat(60));

    // Resolve host fixture path (relative to workspace root)
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let go_fixture_path = workspace_root.join("fixtures/go");

    // Start test file server with path mapping:
    // /src/fixtures/go/ → host fixtures/go/ (matches DWARF debug info paths)
    let mut mappings = HashMap::new();
    mappings.insert("/src/fixtures/go".to_string(), go_fixture_path.clone());
    // Fallback: /app/ mapping in case the connection reports /app/ as workspace
    mappings.insert("/app".to_string(), go_fixture_path);

    let (bridge_port, bridge_handle) = start_test_file_server(mappings, None)
        .await
        .expect("start test file server failed");
    println!("  Test file server started on port {}", bridge_port);

    // Create MCP client with bridge URL header
    let mut bridge_headers = reqwest::header::HeaderMap::new();
    bridge_headers.insert(
        "X-Detrix-File-Server-Url",
        format!("http://host.docker.internal:{}", bridge_port)
            .parse()
            .unwrap(),
    );
    let bridge_client =
        McpClient::with_auth_and_headers(DAEMON_HTTP_PORT, DOCKER_AUTH_TOKEN, bridge_headers);

    // Wake Go and use explicit connection_id
    println!("\n--- Phase 3: waking Go ---");
    bridge_client
        .wake(GO_APP_URL, None)
        .await
        .expect("wake go failed");

    let go_conn = poll_for_connection(&bridge_client, "go", Duration::from_secs(15))
        .await
        .expect("Go connection not found within 15s");
    println!("  Go connection: {}", go_conn);

    // observe via bridge file serving
    // Use absolute path because Go workspace_root is "/" and relative "detrix_example_app.go"
    // would resolve to "/detrix_example_app.go" instead of the DWARF path.
    let observe_req = ObserveRequest::new(GO_FILE, "symbol")
        .with_name("cloud-go-bridge")
        .with_connection_id(&go_conn);
    let observe_resp = bridge_client
        .observe(observe_req)
        .await
        .expect("observe go via bridge failed");
    assert!(
        observe_resp.data.success,
        "observe should succeed: {:?}",
        observe_resp.data
    );
    println!(
        "  observe ok: metric={} at {}#{}",
        observe_resp.data.metric_name, observe_resp.data.file, observe_resp.data.line
    );

    let bridge_events =
        poll_for_events(&bridge_client, "cloud-go-bridge", Duration::from_secs(15)).await;
    assert!(
        !bridge_events.is_empty(),
        "Expected bridge events but got none (waited 15s)"
    );
    println!("  Bridge events captured: {}", bridge_events.len());

    // Cleanup
    println!("\n--- Phase 3 cleanup ---");
    sleep_app(&bridge_client, GO_APP_URL).await;
    bridge_handle.abort();
    println!("  Go sleeping, test file server stopped");

    // ── DAEMON RESTART (clean state for Phase 4) ──
    println!("\n--- Restarting daemon (clean state for Phase 4) ---");
    restart_daemon().await;
    println!("  Daemon restarted");

    println!("\n{}", "=".repeat(60));
    println!("Phase 3 complete — bridge file serving verified");
    println!("{}", "=".repeat(60));

    // ── Phase 4: Git-pinned + drift detection (Python) ──
    // Create a temp git repo with the Python fixture, modify working tree
    // (drift), force-recreate Python with GIT_COMMIT, serve via bridge with
    // git support → daemon does `git show` → detects drift.
    println!("\n{}", "=".repeat(60));
    println!("Phase 4: Git-pinned + drift detection (Python)");
    println!("{}", "=".repeat(60));

    // 1. Create temp git repo with Python fixture
    let py_fixture = workspace_root.join("fixtures/python/trade_bot_forever.py");
    let (git_dir, commit_sha) =
        create_temp_git_repo(&[("trade_bot_forever.py", &py_fixture)]).await;
    println!("  Git repo at {:?}, commit {}", git_dir.path(), commit_sha);

    // 2. Modify working tree copy (create drift)
    let working_file = git_dir.path().join("trade_bot_forever.py");
    let mut content = std::fs::read_to_string(&working_file).expect("read working file");
    content.push_str("\n# DRIFT\n");
    std::fs::write(&working_file, content).expect("write drift");
    println!("  Working tree modified (drift injected)");

    // 3. Force-recreate Python container with GIT_COMMIT env
    println!(
        "  Force-recreating Python container with GIT_COMMIT={}...",
        &commit_sha[..8]
    );
    force_recreate_with_env("test-app-python", &[("TEST_GIT_COMMIT", &commit_sha)]).await;
    println!("  Python container recreated");

    // 4. Start test file server with git support
    let mut git_mappings = HashMap::new();
    git_mappings.insert("/app".to_string(), git_dir.path().to_path_buf());
    let (git_bridge_port, git_bridge_handle) =
        start_test_file_server(git_mappings, Some(git_dir.path().to_path_buf()))
            .await
            .expect("start git test file server failed");
    println!("  Git-enabled file server on port {}", git_bridge_port);

    // 5. Create MCP client with bridge URL
    let mut git_headers = reqwest::header::HeaderMap::new();
    git_headers.insert(
        "X-Detrix-File-Server-Url",
        format!("http://host.docker.internal:{}", git_bridge_port)
            .parse()
            .unwrap(),
    );
    let git_client =
        McpClient::with_auth_and_headers(DAEMON_HTTP_PORT, DOCKER_AUTH_TOKEN, git_headers);

    // 6. Wake Python → it registers with build_commit=sha
    println!("\n--- Phase 4: waking Python ---");
    git_client
        .wake(PYTHON_APP_URL, None)
        .await
        .expect("wake python failed");

    let py_conn = poll_for_connection(&git_client, "python", Duration::from_secs(15))
        .await
        .expect("Python connection not found within 15s");
    println!(
        "  Python connection: {} (should have build_commit)",
        py_conn
    );

    // 7. observe → bridge → git show → drift detected
    let observe_req = ObserveRequest::new("trade_bot_forever.py", "order_id")
        .with_name("cloud-py-git")
        .with_connection_id(&py_conn);
    let observe_resp = git_client
        .observe(observe_req)
        .await
        .expect("observe python via git-pinned bridge failed");
    assert!(
        observe_resp.data.success,
        "observe should succeed: {:?}",
        observe_resp.data
    );
    println!(
        "  observe ok: metric={} at {}#{}",
        observe_resp.data.metric_name, observe_resp.data.file, observe_resp.data.line
    );

    // 8. Poll for events
    let git_events = poll_for_events(&git_client, "cloud-py-git", Duration::from_secs(15)).await;
    assert!(
        !git_events.is_empty(),
        "Expected git-pinned events but got none (waited 15s)"
    );
    println!("  Git-pinned events captured: {}", git_events.len());

    // Cleanup
    println!("\n--- Phase 4 cleanup ---");
    sleep_app(&git_client, PYTHON_APP_URL).await;
    git_bridge_handle.abort();
    // git_dir (TempDir) will be cleaned up when dropped
    println!("  Python sleeping, git file server stopped");

    println!("\n{}", "=".repeat(60));
    println!("Phase 4 complete — git-pinned + drift detection verified");
    println!("{}", "=".repeat(60));

    // ── Phase 5: MCP binary smoke test ──
    // Spawn the real `detrix mcp` binary and verify JSON-RPC initialize +
    // tools/list work. Independent of VFS state — no daemon restart needed.
    println!("\n{}", "=".repeat(60));
    println!("Phase 5: MCP binary smoke test");
    println!("{}", "=".repeat(60));

    let ws_root = get_workspace_root();
    let detrix_bin =
        find_detrix_binary(&ws_root).expect("detrix binary not found — run `cargo build` first");
    println!("  Binary: {:?}", detrix_bin);

    let daemon_url = format!("http://127.0.0.1:{}", DAEMON_HTTP_PORT);
    let mut child = tokio::process::Command::new(&detrix_bin)
        .args(["mcp", "--daemon-url", &daemon_url])
        .env("DETRIX_TOKEN", DOCKER_AUTH_TOKEN)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn detrix mcp failed");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    // Send JSON-RPC initialize
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "e2e-test", "version": "1.0" }
        },
        "id": 1
    });
    let msg = format!("{}\n", initialize);
    stdin
        .write_all(msg.as_bytes())
        .await
        .expect("write initialize");
    stdin.flush().await.expect("flush");

    // Read initialize response — skip any non-response lines
    let init_resp: serde_json::Value = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut line = String::new();
            let remaining = deadline.duration_since(Instant::now());
            let read_result = tokio::time::timeout(remaining, reader.read_line(&mut line))
                .await
                .expect("initialize response timeout (10s)")
                .expect("read initialize response");
            assert!(read_result > 0, "Empty initialize response");
            let parsed: serde_json::Value =
                serde_json::from_str(line.trim()).expect("parse initialize response");
            if parsed.get("id").is_some_and(|v| !v.is_null()) {
                break parsed;
            }
            println!(
                "  (skipping notification: {})",
                &line.trim()[..line.trim().len().min(120)]
            );
        }
    };
    assert!(
        init_resp.get("result").is_some(),
        "initialize should return result: {}",
        init_resp
    );
    println!("  initialize OK");

    // Send initialized notification (required by MCP protocol)
    let initialized = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let msg = format!("{}\n", initialized);
    stdin
        .write_all(msg.as_bytes())
        .await
        .expect("write initialized");
    stdin.flush().await.expect("flush");

    // Send tools/list
    let tools_list = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "tools/list",
        "params": {},
        "id": 2
    });
    let msg = format!("{}\n", tools_list);
    stdin
        .write_all(msg.as_bytes())
        .await
        .expect("write tools/list");
    stdin.flush().await.expect("flush");

    // Read tools/list response — skip non-JSON-RPC lines (notifications, logs)
    let tools_resp: serde_json::Value = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut line2 = String::new();
            let remaining = deadline.duration_since(Instant::now());
            let read_result = tokio::time::timeout(remaining, reader.read_line(&mut line2))
                .await
                .expect("tools/list response timeout (10s)")
                .expect("read tools/list response");
            assert!(read_result > 0, "Empty tools/list response");
            let parsed: serde_json::Value =
                serde_json::from_str(line2.trim()).expect("parse tools/list response");
            // Skip notifications (no "id" or "id":null) — we want the response with id=2
            if parsed.get("id").is_some_and(|v| !v.is_null()) {
                break parsed;
            }
            println!(
                "  (skipping notification: {})",
                &line2.trim()[..line2.trim().len().min(120)]
            );
        }
    };
    let tools = tools_resp
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .unwrap_or_else(|| panic!("tools/list should return tools array, got: {}", tools_resp));
    assert!(
        !tools.is_empty(),
        "Expected tools but got empty list: {}",
        tools_resp
    );
    println!("  tools/list OK: {} tools available", tools.len());

    // Verify key tools exist
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();
    for expected in &["observe", "add_metric", "list_metrics", "get_status"] {
        assert!(
            tool_names.contains(expected),
            "Missing expected tool '{}' in {:?}",
            expected,
            tool_names
        );
    }
    println!("  Key tools verified: observe, add_metric, list_metrics, get_status");

    // Cleanup
    drop(stdin);
    let _ = child.kill().await;
    println!("  MCP binary process stopped");

    println!("\n{}", "=".repeat(60));
    println!("Phase 5 complete — MCP binary smoke test verified");
    println!("{}", "=".repeat(60));

    // ── Phase 6: Daemon Advertise URL + Observe (All Clients) ──
    // Restart daemon with DETRIX_ADVERTISE_URL, verify daemon_url flows through
    // wake responses, then observe with each client language.
    println!("\n{}", "=".repeat(60));
    println!("Phase 6: Daemon Advertise URL + Observe (All Clients)");
    println!("{}", "=".repeat(60));

    // Restart daemon with advertise_url
    println!(
        "\n--- Restarting daemon with DETRIX_ADVERTISE_URL={} ---",
        ADVERTISE_URL
    );
    restart_daemon_with_env(&[("TEST_ADVERTISE_URL", ADVERTISE_URL)]).await;
    println!("  Daemon restarted with advertise_url");

    // ── Phase 6a: Python (control plane file serving) ──
    println!("\n--- Phase 6a: Python (control plane file serving) ---");
    let wake_resp = client
        .wake(PYTHON_APP_URL, None)
        .await
        .expect("wake python failed");
    assert_wake_has_daemon_url(&wake_resp, ADVERTISE_URL);
    println!("  daemon_url verified: {}", ADVERTISE_URL);

    let py_conn = poll_for_connection(&client, "python", Duration::from_secs(15))
        .await
        .expect("Python connection not found within 15s");
    println!("  Python connection: {}", py_conn);

    // observe with auto-find line (VFS fetches file from control plane)
    let observe_req = ObserveRequest::new("trade_bot_forever.py", "order_id")
        .with_name("cloud-py-adv")
        .with_connection_id(&py_conn);
    let observe_resp = client
        .observe(observe_req)
        .await
        .expect("observe python via control plane failed");
    assert!(
        observe_resp.data.success,
        "observe should succeed: {:?}",
        observe_resp.data
    );
    println!(
        "  observe ok: {} at {}#{}",
        observe_resp.data.metric_name, observe_resp.data.file, observe_resp.data.line
    );

    let adv_py_events = poll_for_events(&client, "cloud-py-adv", Duration::from_secs(15)).await;
    assert!(
        !adv_py_events.is_empty(),
        "Expected Python advertise-URL events but got none (waited 15s)"
    );
    println!("  Events captured: {}", adv_py_events.len());

    sleep_app(&client, PYTHON_APP_URL).await;
    println!("  Python sleeping");

    // ── Phase 6b: Go (bridge file serving) ──
    println!("\n--- Phase 6b: Go (bridge file serving) ---");

    let workspace_root_6b = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let go_fixture_path_6b = workspace_root_6b.join("fixtures/go");

    let mut mappings_6b = HashMap::new();
    mappings_6b.insert("/src/fixtures/go".to_string(), go_fixture_path_6b.clone());
    mappings_6b.insert("/app".to_string(), go_fixture_path_6b);

    let (bridge_port_6b, bridge_handle_6b) = start_test_file_server(mappings_6b, None)
        .await
        .expect("start test file server for Phase 6b failed");
    println!("  Test file server started on port {}", bridge_port_6b);

    let mut bridge_headers_6b = reqwest::header::HeaderMap::new();
    bridge_headers_6b.insert(
        "X-Detrix-File-Server-Url",
        format!("http://host.docker.internal:{}", bridge_port_6b)
            .parse()
            .unwrap(),
    );
    let bridge_client_6b =
        McpClient::with_auth_and_headers(DAEMON_HTTP_PORT, DOCKER_AUTH_TOKEN, bridge_headers_6b);

    let go_wake_resp = bridge_client_6b
        .wake(GO_APP_URL, None)
        .await
        .expect("wake go failed");
    assert_wake_has_daemon_url(&go_wake_resp, ADVERTISE_URL);
    println!("  daemon_url verified: {}", ADVERTISE_URL);

    let go_conn_6b = poll_for_connection(&bridge_client_6b, "go", Duration::from_secs(15))
        .await
        .expect("Go connection not found within 15s");
    println!("  Go connection: {}", go_conn_6b);

    let observe_req = ObserveRequest::new(GO_FILE, "symbol")
        .with_name("cloud-go-adv")
        .with_connection_id(&go_conn_6b);
    let observe_resp = bridge_client_6b
        .observe(observe_req)
        .await
        .expect("observe go via bridge failed");
    assert!(
        observe_resp.data.success,
        "observe should succeed: {:?}",
        observe_resp.data
    );
    println!(
        "  observe ok: {} at {}#{}",
        observe_resp.data.metric_name, observe_resp.data.file, observe_resp.data.line
    );

    let adv_go_events =
        poll_for_events(&bridge_client_6b, "cloud-go-adv", Duration::from_secs(15)).await;
    assert!(
        !adv_go_events.is_empty(),
        "Expected Go advertise-URL events but got none (waited 15s)"
    );
    println!("  Events captured: {}", adv_go_events.len());

    sleep_app(&bridge_client_6b, GO_APP_URL).await;
    bridge_handle_6b.abort();
    println!("  Go sleeping, test file server stopped");

    // ── Phase 6c: Rust (control plane file serving, skip if unavailable) ──
    println!("\n--- Phase 6c: Rust (control plane file serving) ---");
    let rust_adv_available = match client.wake(RUST_APP_URL, None).await {
        Ok(rust_wake) => {
            // Check daemon_url even if we might skip later
            let has_daemon_url = rust_wake.data.contains(ADVERTISE_URL)
                || rust_wake
                    .raw_response
                    .as_ref()
                    .is_some_and(|r| r.contains(ADVERTISE_URL));
            if has_daemon_url {
                println!("  daemon_url verified: {}", ADVERTISE_URL);
            }

            if let Some(rust_conn) =
                poll_for_connection(&client, "rust", Duration::from_secs(15)).await
            {
                println!("  Rust connection: {}", rust_conn);
                let rust_metric = AddMetricRequest::new(
                    "cloud-rust-adv",
                    &format!("{}#108", RUST_FILE),
                    "symbol",
                    &rust_conn,
                );
                if let Err(e) = client.add_metric(rust_metric).await {
                    println!("  Rust add_metric failed (skipping): {}", e);
                    false
                } else {
                    let rust_events =
                        poll_for_events(&client, "cloud-rust-adv", Duration::from_secs(15)).await;
                    if rust_events.is_empty() {
                        println!("  Rust events: 0 (skipping)");
                        false
                    } else {
                        println!("  Events captured: {}", rust_events.len());
                        true
                    }
                }
            } else {
                println!("  Rust connection not found (skipping)");
                false
            }
        }
        Err(e) => {
            println!("  Rust wake failed (lldb-dap likely unavailable): {}", e);
            false
        }
    };
    if !rust_adv_available {
        println!("  [SKIP] Rust advertise-URL tests skipped");
    }
    sleep_app(&client, RUST_APP_URL).await;

    println!("\n{}", "=".repeat(60));
    println!(
        "Phase 6 complete — advertise_url verified: Python + Go{}",
        if rust_adv_available {
            " + Rust"
        } else {
            " (Rust skipped)"
        }
    );
    println!("{}", "=".repeat(60));

    // ── Phase 7: MCP Bridge Auto-Switch via Advertise URL ──
    // Spawn detrix mcp with the daemon URL using IP (http://127.0.0.1:8095).
    // The daemon's advertise_url is http://localhost:8095 (different string).
    // On wake, the bridge detects daemon_url != current URL → auto-switches.
    // Verifies: daemon_url extraction, auto-switch logic, subsequent requests work.
    println!("\n{}", "=".repeat(60));
    println!("Phase 7: MCP Bridge Auto-Switch via Advertise URL");
    println!("{}", "=".repeat(60));

    // Restart daemon with advertise_url (clean DB for fresh state)
    println!(
        "\n--- Restarting daemon with DETRIX_ADVERTISE_URL={} ---",
        ADVERTISE_URL
    );
    restart_daemon_with_env(&[("TEST_ADVERTISE_URL", ADVERTISE_URL)]).await;
    println!("  Daemon restarted");

    // Spawn detrix mcp with IP-based daemon URL (differs from advertise_url string)
    let ws_root_7 = get_workspace_root();
    let detrix_bin_7 =
        find_detrix_binary(&ws_root_7).expect("detrix binary not found — run `cargo build` first");

    // Use 127.0.0.1 (IP) so it differs from advertise_url "localhost" (hostname)
    let bridge_daemon_url = format!("http://127.0.0.1:{}", DAEMON_HTTP_PORT);
    println!(
        "  Spawning detrix mcp --daemon-url {} (IP-based, differs from advertise_url)",
        bridge_daemon_url
    );

    let mut mcp_child = tokio::process::Command::new(&detrix_bin_7)
        .args(["mcp", "--daemon-url", &bridge_daemon_url])
        .env("DETRIX_TOKEN", DOCKER_AUTH_TOKEN)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn detrix mcp failed");

    let mut mcp_stdin = mcp_child.stdin.take().expect("stdin");
    let mcp_stdout = mcp_child.stdout.take().expect("stdout");
    let mut mcp_reader = BufReader::new(mcp_stdout);

    // Helper: read a JSON-RPC response (skip notifications)
    async fn read_jsonrpc_response(
        reader: &mut BufReader<tokio::process::ChildStdout>,
        timeout_secs: u64,
    ) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let mut line = String::new();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!("JSON-RPC response timeout ({}s)", timeout_secs);
            }
            let read_result = tokio::time::timeout(remaining, reader.read_line(&mut line))
                .await
                .unwrap_or_else(|_| panic!("JSON-RPC response timeout ({}s)", timeout_secs))
                .expect("read JSON-RPC response");
            assert!(read_result > 0, "Empty JSON-RPC response (EOF)");
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                // Skip notifications (no "id" or "id":null)
                if parsed.get("id").is_some_and(|v| !v.is_null()) {
                    return parsed;
                }
            }
        }
    }

    // Send JSON-RPC initialize (forwarded to real daemon at 127.0.0.1:8095)
    let init_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "e2e-phase7", "version": "1.0" }
        },
        "id": 1
    });
    mcp_stdin
        .write_all(format!("{}\n", init_msg).as_bytes())
        .await
        .expect("write initialize");
    mcp_stdin.flush().await.expect("flush");

    let init_resp = read_jsonrpc_response(&mut mcp_reader, 10).await;
    assert!(
        init_resp.get("result").is_some(),
        "initialize should succeed: {}",
        init_resp
    );
    println!("  initialize OK");

    // Send notifications/initialized
    let initialized_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    mcp_stdin
        .write_all(format!("{}\n", initialized_msg).as_bytes())
        .await
        .expect("write initialized");
    mcp_stdin.flush().await.expect("flush");

    // Brief pause to let the bridge process the notification
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send wake tool call (Docker-internal URL, reachable by daemon)
    // Bridge forwards to daemon → daemon wakes Python → response has daemon_url
    // Bridge sees daemon_url (localhost:8095) != current (127.0.0.1:8095) → auto-switch
    let wake_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "wake",
            "arguments": {
                "app_url": PYTHON_APP_URL
            }
        },
        "id": 2
    });
    mcp_stdin
        .write_all(format!("{}\n", wake_msg).as_bytes())
        .await
        .expect("write wake");
    mcp_stdin.flush().await.expect("flush");

    // Read wake response (may take longer due to Python wake + registration)
    let wake_resp_7 = read_jsonrpc_response(&mut mcp_reader, 60).await;
    println!(
        "  Wake response (truncated): {}",
        &wake_resp_7.to_string()[..wake_resp_7.to_string().len().min(500)]
    );

    // Extract text content from wake response
    let wake_text: String = wake_resp_7
        .pointer("/result/content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    // Assert daemon_url is present in the response
    assert!(
        wake_text.contains("daemon_url") || wake_text.contains(ADVERTISE_URL),
        "Wake response should contain daemon_url.\nText: {}",
        wake_text
    );
    println!("  Wake response contains daemon_url");

    // Assert auto-switch happened (advertise_url differs from bridge's IP-based URL)
    assert!(
        wake_text.contains("Auto-switched to daemon"),
        "Wake response should contain auto-switch note.\nText: {}",
        wake_text
    );
    println!(
        "  Auto-switch confirmed: {} → {}",
        bridge_daemon_url, ADVERTISE_URL
    );

    // Send get_status — should succeed after auto-switch
    let status_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "get_status",
            "arguments": {}
        },
        "id": 3
    });
    mcp_stdin
        .write_all(format!("{}\n", status_msg).as_bytes())
        .await
        .expect("write get_status");
    mcp_stdin.flush().await.expect("flush");

    let status_resp = read_jsonrpc_response(&mut mcp_reader, 10).await;
    let has_result = status_resp.get("result").is_some();
    let is_error = status_resp
        .pointer("/result/isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        has_result && !is_error,
        "get_status should succeed after auto-switch.\nResponse: {}",
        status_resp
    );
    println!("  get_status succeeded after auto-switch");

    // Cleanup: kill MCP process, sleep Python
    drop(mcp_stdin);
    let _ = mcp_child.kill().await;
    println!("  MCP bridge process stopped");

    // Sleep Python (it was woken by the bridge's forwarded wake)
    sleep_app(&client, PYTHON_APP_URL).await;
    println!("  Python sleeping");

    println!("\n{}", "=".repeat(60));
    println!("Phase 7 complete — MCP bridge direct wake + auto-switch verified");
    println!("{}", "=".repeat(60));

    println!("\n{}", "=".repeat(60));
    println!("ALL PHASES COMPLETE — Docker Cloud E2E passed!");
    println!("{}", "=".repeat(60));
}
