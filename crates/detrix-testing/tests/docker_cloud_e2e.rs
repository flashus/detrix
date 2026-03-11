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

use detrix_testing::e2e::client::ApiClient;
use detrix_testing::e2e::dap_scenarios::go_lines;
use detrix_testing::e2e::{find_detrix_binary, get_workspace_root, McpClient};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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

/// Poll `list_connections` until a connected connection with the given language appears.
/// Returns the connection_id.
/// NOTE: Used only by Phase 8b (kept as-is). Other phases use poll_for_connection_bridge.
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

/// Sleep an app (stops its debugger) and wait briefly for cleanup.
/// NOTE: Used only by Phase 7/8 (kept as-is). Other phases use BridgeProcess::sleep_app.
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
// BridgeProcess — manages a `detrix mcp` child process (stdin/stdout JSON-RPC)
// =============================================================================

/// Manages a `detrix mcp` bridge subprocess for E2E testing.
/// Communicates via JSON-RPC over stdin/stdout, matching the real agent experience.
struct BridgeProcess {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

impl BridgeProcess {
    /// Spawn a bridge process pointing at the given daemon.
    async fn spawn(daemon_url: &str, token: &str, file_server_host: Option<&str>) -> Self {
        Self::spawn_inner(daemon_url, token, file_server_host, None).await
    }

    /// Spawn a bridge process with a custom CWD (needed for git-pinned tests
    /// where the bridge's file server must serve from a temp repo directory).
    async fn spawn_in_dir(
        daemon_url: &str,
        token: &str,
        file_server_host: Option<&str>,
        cwd: &Path,
    ) -> Self {
        Self::spawn_inner(daemon_url, token, file_server_host, Some(cwd)).await
    }

    async fn spawn_inner(
        daemon_url: &str,
        token: &str,
        file_server_host: Option<&str>,
        cwd: Option<&Path>,
    ) -> Self {
        let ws_root = get_workspace_root();
        let detrix_bin = find_detrix_binary(&ws_root)
            .expect("detrix binary not found — run `cargo build` first");

        let mut args = vec![
            "mcp".to_string(),
            "--daemon-url".to_string(),
            daemon_url.to_string(),
        ];
        if let Some(host) = file_server_host {
            args.push("--file-server-host".to_string());
            args.push(host.to_string());
        }

        let mut cmd = tokio::process::Command::new(&detrix_bin);
        cmd.args(&args)
            .env("DETRIX_TOKEN", token)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().expect("spawn detrix mcp failed");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let reader = BufReader::new(stdout);

        let mut bridge = Self {
            child,
            stdin,
            reader,
            next_id: 1,
        };

        // Perform MCP initialization handshake
        bridge.initialize().await;
        bridge
    }

    async fn initialize(&mut self) {
        // Send initialize request
        let id = self.next_id;
        self.next_id += 1;
        let init_req = json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "e2e-bridge", "version": "1.0" }
            },
            "id": id
        });
        self.write_message(&init_req).await;
        let resp = self.read_response(10).await;
        assert!(
            resp.get("result").is_some(),
            "initialize should succeed: {}",
            resp
        );

        // Send initialized notification (required by MCP protocol)
        let initialized = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        self.write_message(&initialized).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    async fn write_message(&mut self, msg: &Value) {
        let line = format!("{}\n", msg);
        self.stdin
            .write_all(line.as_bytes())
            .await
            .expect("write to bridge stdin");
        self.stdin.flush().await.expect("flush bridge stdin");
    }

    /// Read a JSON-RPC response (skips notifications).
    async fn read_response(&mut self, timeout_secs: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let mut line = String::new();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!("Bridge response timeout ({}s)", timeout_secs);
            }
            let n = tokio::time::timeout(remaining, self.reader.read_line(&mut line))
                .await
                .unwrap_or_else(|_| panic!("Bridge response timeout ({}s)", timeout_secs))
                .expect("read bridge response");
            assert!(
                n > 0,
                "EOF from bridge (response timeout {}s)",
                timeout_secs
            );
            if let Ok(parsed) = serde_json::from_str::<Value>(line.trim()) {
                // Skip notifications (no "id" or "id":null)
                if parsed.get("id").is_some_and(|v| !v.is_null()) {
                    return parsed;
                }
            }
        }
    }

    /// Call an MCP tool and return the result object. Returns Err on JSON-RPC error or isError.
    async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;

        let request = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": args
            },
            "id": id
        });

        self.write_message(&request).await;
        let resp = self.read_response(60).await;

        // Check for JSON-RPC error
        if let Some(error) = resp.get("error") {
            return Err(format!("JSON-RPC error: {}", error));
        }

        let result = resp
            .get("result")
            .cloned()
            .ok_or_else(|| "no result in response".to_string())?;

        // Check for isError flag
        if result.get("isError") == Some(&Value::Bool(true)) {
            let text = Self::extract_text(&result);
            return Err(format!("Tool error: {}", text));
        }

        Ok(result)
    }

    /// Extract concatenated text from MCP result.content array.
    fn extract_text(result: &Value) -> String {
        result
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    // ── High-level tool wrappers ──

    /// Wake an app. Returns response text.
    async fn wake(&mut self, app_url: &str) -> Result<String, String> {
        let result = self.call_tool("wake", json!({"app_url": app_url})).await?;
        Ok(Self::extract_text(&result))
    }

    /// Sleep an app (stops its debugger) and wait briefly for cleanup.
    async fn sleep_app(&mut self, app_url: &str) {
        let _ = self.call_tool("sleep", json!({"app_url": app_url})).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    /// Add a metric with a single expression.
    async fn add_metric(
        &mut self,
        name: &str,
        location: &str,
        expressions: &[&str],
        connection_id: &str,
    ) -> Result<(), String> {
        self.call_tool(
            "add_metric",
            json!({
                "name": name,
                "location": location,
                "expressions": expressions,
                "connection_id": connection_id
            }),
        )
        .await?;
        Ok(())
    }

    /// Observe (auto-find line, auto-select connection). Returns parsed ObserveInfo.
    async fn observe(&mut self, args: Value) -> Result<ObserveInfo, String> {
        let result = self.call_tool("observe", args).await?;
        let text = Self::extract_text(&result);
        ObserveInfo::parse(&text)
    }

    /// Remove a metric by name.
    async fn remove_metric(&mut self, name: &str) -> Result<(), String> {
        self.call_tool("remove_metric", json!({"name": name}))
            .await?;
        Ok(())
    }

    /// List metric names (for cleanup).
    async fn list_metrics_names(&mut self) -> Vec<String> {
        if let Ok(result) = self
            .call_tool("list_metrics", json!({"format": "json"}))
            .await
        {
            let text = Self::extract_text(&result);
            if let Some(start) = text.find('[') {
                if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&text[start..]) {
                    return arr
                        .iter()
                        .filter_map(|m| {
                            m.get("name")
                                .and_then(|n| n.as_str())
                                .map(|s| s.to_string())
                        })
                        .collect();
                }
            }
        }
        vec![]
    }

    /// Kill the bridge process.
    async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

/// Parsed observe response (minimal fields needed by test assertions).
struct ObserveInfo {
    success: bool,
    metric_name: String,
    file: String,
    line: u32,
    warnings: Vec<String>,
}

impl ObserveInfo {
    /// Parse from the text content of an observe MCP response.
    /// The observe tool returns nested JSON: {success, metric: {id, name, location, ...}, context: {...}, warnings: [...]}
    fn parse(text: &str) -> Result<Self, String> {
        // Try single-line JSON with "metric" key
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('{') && trimmed.contains("\"metric\"") {
                if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
                    return Self::from_json(&json);
                }
            }
        }
        // Try multiline JSON
        if let Some(start) = text.find("\n{") {
            if let Ok(json) = serde_json::from_str::<Value>(&text[start + 1..]) {
                if json.get("metric").is_some() {
                    return Self::from_json(&json);
                }
            }
        }
        // Try whole text as JSON
        if text.trim().starts_with('{') {
            if let Ok(json) = serde_json::from_str::<Value>(text.trim()) {
                if json.get("metric").is_some() {
                    return Self::from_json(&json);
                }
            }
        }
        Err(format!(
            "Failed to parse observe response: {}",
            &text[..text.len().min(500)]
        ))
    }

    fn from_json(json: &Value) -> Result<Self, String> {
        let success = json
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let metric = json.get("metric").unwrap_or(&Value::Null);
        let metric_name = metric
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Parse location "@file#line"
        let location = metric
            .get("location")
            .and_then(|v| v.as_str())
            .unwrap_or("@unknown#0");
        let location = location.strip_prefix('@').unwrap_or(location);
        let (file, line) = if let Some((f, l)) = location.rsplit_once('#') {
            (f.to_string(), l.parse::<u32>().unwrap_or(0))
        } else {
            (location.to_string(), 0)
        };

        let warnings = json
            .get("warnings")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            success,
            metric_name,
            file,
            line,
            warnings,
        })
    }
}

/// Poll `list_connections` via BridgeProcess until a connected connection with the given language.
/// Returns the connection_id.
async fn poll_for_connection_bridge(
    bridge: &mut BridgeProcess,
    language: &str,
    timeout: Duration,
) -> Option<String> {
    let start = Instant::now();
    loop {
        if let Ok(result) = bridge.call_tool("list_connections", json!({})).await {
            let text = BridgeProcess::extract_text(&result);
            // Parse TOON-format CSV lines (same format as daemon returns)
            for line in text.lines() {
                if line.contains(',') && !line.starts_with('[') && !line.starts_with("Found") {
                    let parts: Vec<&str> = line.trim().split(',').collect();
                    if parts.len() >= 5 {
                        let lang = parts[3].trim().trim_matches('"');
                        let status_raw = parts[4].trim().trim_matches('"');
                        let is_connected = status_raw == "3" || status_raw == "connected";
                        if lang == language && is_connected {
                            return Some(parts[0].trim().trim_matches('"').to_string());
                        }
                    }
                }
            }
        }
        if start.elapsed() > timeout {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Poll `query_metrics` via BridgeProcess until events appear. Returns event count (0 = timeout).
async fn poll_for_events_bridge(
    bridge: &mut BridgeProcess,
    metric_name: &str,
    timeout: Duration,
) -> usize {
    let start = Instant::now();
    loop {
        if let Ok(result) = bridge
            .call_tool(
                "query_metrics",
                json!({"name": metric_name, "limit": 10, "format": "json"}),
            )
            .await
        {
            let text = BridgeProcess::extract_text(&result);
            // Look for JSON array in text (format=json returns "Found N events...\n[{...}, ...]")
            if let Some(arr_start) = text.find('[') {
                if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&text[arr_start..]) {
                    if !arr.is_empty() {
                        return arr.len();
                    }
                }
            }
        }
        if start.elapsed() > timeout {
            return 0;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Look up a metric's numeric ID by name using the admin token.
/// Returns the ID as a String, or None if not found.
async fn admin_get_metric_id(
    client: &reqwest::Client,
    daemon_url: &str,
    admin_token: &str,
    metric_name: &str,
) -> Option<String> {
    let resp = client
        .get(format!("{}/api/v1/metrics", daemon_url))
        .header("Authorization", format!("Bearer {}", admin_token))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    json.get("metrics")?
        .as_array()?
        .iter()
        .find(|m| {
            m.get("name")
                .and_then(|n| n.as_str())
                .map(|n| n == metric_name)
                .unwrap_or(false)
        })
        .and_then(|m| m.get("metricId").and_then(|id| id.as_u64()))
        .map(|id| id.to_string())
}

// =============================================================================
// Orchestrator
// =============================================================================

#[tokio::test]
#[ignore]
async fn test_cloud_e2e() {
    // Docker Compose assumed running (Taskfile manages lifecycle).
    // McpClient kept only for Phase 7/8 (kept as-is); all other phases use BridgeProcess.
    let client = McpClient::with_auth(DAEMON_HTTP_PORT, DOCKER_AUTH_TOKEN);

    // Print daemon build datetime (docker exec) to verify the image is fresh.
    let compose_abs = compose_file_abs();
    if let Ok(output) = std::process::Command::new("docker")
        .args([
            "compose",
            "-f",
            &compose_abs,
            "-p",
            COMPOSE_PROJECT,
            "exec",
            "detrix",
            "/usr/local/bin/detrix",
            "--version",
        ])
        .output()
    {
        let ver = String::from_utf8_lossy(&output.stdout);
        let ver = ver.trim();
        if ver.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            println!("WARNING: could not get daemon version: {}", stderr.trim());
        } else {
            println!("Daemon binary version:\n  {}", ver.replace('\n', "\n  "));
        }
    }

    // Verify daemon is reachable via direct HTTP (fast sanity check)
    let healthy = client.health().await.expect("health check failed");
    assert!(healthy, "Daemon should be healthy before starting tests");
    println!("Daemon healthy at port {}", DAEMON_HTTP_PORT);

    let daemon_url = format!("http://127.0.0.1:{}", DAEMON_HTTP_PORT);

    // ── Phase 1: Basic observation (Python + Go + Rust) ──
    // Wake each app via real MCP bridge, poll for connection, add_metric,
    // poll for events, assert events > 0.
    println!("\n{}", "=".repeat(60));
    println!("Phase 1: Basic observation");
    println!("{}", "=".repeat(60));

    let mut bridge = BridgeProcess::spawn(&daemon_url, DOCKER_AUTH_TOKEN, None).await;

    // ── Phase 1a: Python ──
    println!("\n--- Phase 1a: Python ---");
    bridge
        .wake(PYTHON_APP_URL)
        .await
        .expect("wake python failed");

    let py_conn = poll_for_connection_bridge(&mut bridge, "python", Duration::from_secs(15))
        .await
        .expect("Python connection not found within 15s");
    println!("  Python connection: {}", py_conn);

    bridge
        .add_metric(
            "cloud-py-basic",
            &format!("{}#60", PYTHON_FILE),
            &["order_id"],
            &py_conn,
        )
        .await
        .expect("add python metric failed");

    let py_event_count =
        poll_for_events_bridge(&mut bridge, "cloud-py-basic", Duration::from_secs(15)).await;
    assert!(
        py_event_count > 0,
        "Expected Python events but got none (waited 15s)"
    );
    println!("  Python events captured: {}", py_event_count);

    // ── Phase 1b: Go ──
    println!("\n--- Phase 1b: Go ---");
    bridge.wake(GO_APP_URL).await.expect("wake go failed");

    let go_conn = poll_for_connection_bridge(&mut bridge, "go", Duration::from_secs(15))
        .await
        .expect("Go connection not found within 15s");
    println!("  Go connection: {}", go_conn);

    bridge
        .add_metric(
            "cloud-go-basic",
            // find_logpoint("symbol"): symbol is declared at +26 but NOT yet in scope there;
            // first safe line is +27 (quantity declaration), where symbol IS in scope.
            &format!("{}#{}", GO_FILE, go_lines::CODEMAP.find_logpoint("symbol")),
            &["symbol"],
            &go_conn,
        )
        .await
        .expect("add go metric failed");

    let go_event_count =
        poll_for_events_bridge(&mut bridge, "cloud-go-basic", Duration::from_secs(15)).await;
    assert!(
        go_event_count > 0,
        "Expected Go events but got none (waited 15s)"
    );
    println!("  Go events captured: {}", go_event_count);

    // ── Phase 1c: Rust (skip if lldb-dap unavailable in container) ──
    println!("\n--- Phase 1c: Rust ---");
    let rust_available = match bridge.wake(RUST_APP_URL).await {
        Ok(_) => {
            if let Some(rust_conn) =
                poll_for_connection_bridge(&mut bridge, "rust", Duration::from_secs(15)).await
            {
                println!("  Rust connection: {}", rust_conn);
                if let Err(e) = bridge
                    .add_metric(
                        "cloud-rust-basic",
                        &format!("{}#108", RUST_FILE),
                        &["symbol"],
                        &rust_conn,
                    )
                    .await
                {
                    println!("  Rust add_metric failed (skipping): {}", e);
                    false
                } else {
                    let rust_event_count = poll_for_events_bridge(
                        &mut bridge,
                        "cloud-rust-basic",
                        Duration::from_secs(15),
                    )
                    .await;
                    println!("  Rust events captured: {}", rust_event_count);
                    rust_event_count > 0
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
    assert!(
        rust_available,
        "Rust E2E failed — lldb-dap not working in container. \
        Dockerfile.rust runtime uses debian:bookworm-slim + lldb-17 from apt.llvm.org. \
        If this fails, the Docker image may be stale — run: task rebuild-daemon-container"
    );

    // ── Cleanup: Sleep all apps (debuggers stop) ──
    println!("\n--- Phase 1 cleanup: sleeping all apps ---");
    bridge.sleep_app(PYTHON_APP_URL).await;
    bridge.sleep_app(GO_APP_URL).await;
    bridge.sleep_app(RUST_APP_URL).await;
    println!("  All apps sleeping");

    // Kill bridge before daemon restart
    bridge.kill().await;

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
    // Bridge sends file-server-url, but Python's /app/ path won't map to host,
    // so daemon falls back to app's /detrix/files/read endpoint (control plane).
    // observe() auto-finds the line via VFS (control_plane source).
    println!("\n{}", "=".repeat(60));
    println!("Phase 2: Control plane file serving (Python)");
    println!("{}", "=".repeat(60));

    let mut bridge = BridgeProcess::spawn(&daemon_url, DOCKER_AUTH_TOKEN, None).await;

    // Wake Python and use explicit connection_id
    println!("\n--- Phase 2: waking Python ---");
    bridge
        .wake(PYTHON_APP_URL)
        .await
        .expect("wake python failed");

    let py_conn = poll_for_connection_bridge(&mut bridge, "python", Duration::from_secs(15))
        .await
        .expect("Python connection not found within 15s");
    println!("  Python connection: {}", py_conn);

    // observe with auto-find line (VFS fetches file from control plane)
    let observe_resp = bridge
        .observe(json!({
            "file": "trade_bot_forever.py",
            "expressions": ["order_id"],
            "name": "cloud-py-cp",
            "connection_id": &py_conn
        }))
        .await
        .expect("observe python via control plane failed");
    assert!(observe_resp.success, "observe should succeed");
    println!(
        "  observe ok: metric={} at {}#{}",
        observe_resp.metric_name, observe_resp.file, observe_resp.line
    );
    if !observe_resp.warnings.is_empty() {
        println!("  observe warnings: {:?}", observe_resp.warnings);
    }

    let cp_event_count =
        poll_for_events_bridge(&mut bridge, "cloud-py-cp", Duration::from_secs(15)).await;
    assert!(
        cp_event_count > 0,
        "Expected control-plane events but got none (waited 15s)"
    );
    println!("  Control plane events captured: {}", cp_event_count);

    // Cleanup
    println!("\n--- Phase 2 cleanup: sleeping Python ---");
    bridge.sleep_app(PYTHON_APP_URL).await;
    bridge.kill().await;

    // ── DAEMON RESTART (clean state for Phase 3) ──
    println!("\n--- Restarting daemon (clean state for Phase 3) ---");
    restart_daemon().await;
    println!("  Daemon restarted");

    println!("\n{}", "=".repeat(60));
    println!("Phase 2 complete — control plane file serving verified");
    println!("{}", "=".repeat(60));

    // ── Phase 3: Bridge file serving (Go) ──
    // Real MCP bridge starts its own file server and auto-maps container paths.
    // The daemon fetches files via the bridge's file server (X-Detrix-File-Server-Url).
    println!("\n{}", "=".repeat(60));
    println!("Phase 3: Bridge file serving (Go)");
    println!("{}", "=".repeat(60));

    let ws_root = get_workspace_root();
    let mut bridge = BridgeProcess::spawn_in_dir(
        &daemon_url,
        DOCKER_AUTH_TOKEN,
        Some("host.docker.internal"),
        &ws_root,
    )
    .await;

    // Wake Go and use explicit connection_id
    println!("\n--- Phase 3: waking Go ---");
    bridge.wake(GO_APP_URL).await.expect("wake go failed");

    let go_conn = poll_for_connection_bridge(&mut bridge, "go", Duration::from_secs(15))
        .await
        .expect("Go connection not found within 15s");
    println!("  Go connection: {}", go_conn);

    // observe via bridge file serving
    // Use absolute path because Go workspace_root is "/" and relative "detrix_example_app.go"
    // would resolve to "/detrix_example_app.go" instead of the DWARF path.
    let observe_resp = bridge
        .observe(json!({
            "file": GO_FILE,
            "expressions": ["symbol"],
            "name": "cloud-go-bridge",
            "connection_id": &go_conn
        }))
        .await
        .expect("observe go via bridge failed");
    assert!(observe_resp.success, "observe should succeed");
    println!(
        "  observe ok: metric={} at {}#{}",
        observe_resp.metric_name, observe_resp.file, observe_resp.line
    );

    let bridge_event_count =
        poll_for_events_bridge(&mut bridge, "cloud-go-bridge", Duration::from_secs(15)).await;
    assert!(
        bridge_event_count > 0,
        "Expected bridge events but got none (waited 15s)"
    );
    println!("  Bridge events captured: {}", bridge_event_count);

    // Cleanup
    println!("\n--- Phase 3 cleanup ---");
    bridge.sleep_app(GO_APP_URL).await;
    bridge.kill().await;
    println!("  Go sleeping, bridge stopped");

    // ── DAEMON RESTART (clean state for Phase 4) ──
    println!("\n--- Restarting daemon (clean state for Phase 4) ---");
    restart_daemon().await;
    println!("  Daemon restarted");

    println!("\n{}", "=".repeat(60));
    println!("Phase 3 complete — bridge file serving verified");
    println!("{}", "=".repeat(60));

    // ── Phase 4: Git-pinned + drift detection (Python) ──
    // Create a temp git repo with the Python fixture, modify working tree
    // (drift), force-recreate Python with GIT_COMMIT, spawn bridge with CWD
    // in the temp repo → bridge file server auto-maps /app/ → repo path,
    // daemon does git show → detects drift.
    println!("\n{}", "=".repeat(60));
    println!("Phase 4: Git-pinned + drift detection (Python)");
    println!("{}", "=".repeat(60));

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

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

    // 4. Spawn bridge with CWD = temp git repo so the bridge file server
    //    can serve files from the repo and the auto-mapping finds /app → repo path.
    let mut bridge = BridgeProcess::spawn_in_dir(
        &daemon_url,
        DOCKER_AUTH_TOKEN,
        Some("host.docker.internal"),
        git_dir.path(),
    )
    .await;
    println!("  Bridge spawned with CWD={:?}", git_dir.path());

    // 5. Wake Python → it registers with build_commit=sha
    println!("\n--- Phase 4: waking Python ---");
    bridge
        .wake(PYTHON_APP_URL)
        .await
        .expect("wake python failed");

    let py_conn = poll_for_connection_bridge(&mut bridge, "python", Duration::from_secs(15))
        .await
        .expect("Python connection not found within 15s");
    println!(
        "  Python connection: {} (should have build_commit)",
        py_conn
    );

    // 6. observe → bridge file server → git show → drift detected
    let observe_resp = bridge
        .observe(json!({
            "file": "trade_bot_forever.py",
            "expressions": ["order_id"],
            "name": "cloud-py-git",
            "connection_id": &py_conn
        }))
        .await
        .expect("observe python via git-pinned bridge failed");
    assert!(observe_resp.success, "observe should succeed");
    println!(
        "  observe ok: metric={} at {}#{}",
        observe_resp.metric_name, observe_resp.file, observe_resp.line
    );

    // 7. Poll for events
    let git_event_count =
        poll_for_events_bridge(&mut bridge, "cloud-py-git", Duration::from_secs(15)).await;
    assert!(
        git_event_count > 0,
        "Expected git-pinned events but got none (waited 15s)"
    );
    println!("  Git-pinned events captured: {}", git_event_count);

    // Cleanup
    println!("\n--- Phase 4 cleanup ---");
    bridge.sleep_app(PYTHON_APP_URL).await;
    bridge.kill().await;
    // git_dir (TempDir) will be cleaned up when dropped
    println!("  Python sleeping, bridge stopped");

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
    // wake responses via real MCP bridge, then observe with each client language.
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
    {
        let mut bridge = BridgeProcess::spawn(&daemon_url, DOCKER_AUTH_TOKEN, None).await;

        let wake_text = bridge
            .wake(PYTHON_APP_URL)
            .await
            .expect("wake python failed");
        assert!(
            wake_text.contains("daemon_url")
                || wake_text.contains("daemonUrl")
                || wake_text.contains(ADVERTISE_URL),
            "Wake text should contain daemon_url or the URL itself.\nText: {}",
            wake_text
        );
        println!("  daemon_url verified: {}", ADVERTISE_URL);

        let py_conn = poll_for_connection_bridge(&mut bridge, "python", Duration::from_secs(15))
            .await
            .expect("Python connection not found within 15s");
        println!("  Python connection: {}", py_conn);

        // observe with auto-find line (VFS fetches file from control plane)
        let observe_resp = bridge
            .observe(json!({
                "file": "trade_bot_forever.py",
                "expressions": ["order_id"],
                "name": "cloud-py-adv",
                "connection_id": &py_conn
            }))
            .await
            .expect("observe python via control plane failed");
        assert!(observe_resp.success, "observe should succeed");
        println!(
            "  observe ok: {} at {}#{}",
            observe_resp.metric_name, observe_resp.file, observe_resp.line
        );

        let adv_py_event_count =
            poll_for_events_bridge(&mut bridge, "cloud-py-adv", Duration::from_secs(15)).await;
        assert!(
            adv_py_event_count > 0,
            "Expected Python advertise-URL events but got none (waited 15s)"
        );
        println!("  Events captured: {}", adv_py_event_count);

        bridge.sleep_app(PYTHON_APP_URL).await;
        bridge.kill().await;
        println!("  Python sleeping");
    }

    // ── Phase 6b: Go (bridge file serving) ──
    println!("\n--- Phase 6b: Go (bridge file serving) ---");
    {
        let mut bridge = BridgeProcess::spawn_in_dir(
            &daemon_url,
            DOCKER_AUTH_TOKEN,
            Some("host.docker.internal"),
            &ws_root,
        )
        .await;

        let go_wake_text = bridge.wake(GO_APP_URL).await.expect("wake go failed");
        assert!(
            go_wake_text.contains("daemon_url")
                || go_wake_text.contains("daemonUrl")
                || go_wake_text.contains(ADVERTISE_URL),
            "Wake text should contain daemon_url or the URL itself.\nText: {}",
            go_wake_text
        );
        println!("  daemon_url verified: {}", ADVERTISE_URL);

        let go_conn_6b = poll_for_connection_bridge(&mut bridge, "go", Duration::from_secs(15))
            .await
            .expect("Go connection not found within 15s");
        println!("  Go connection: {}", go_conn_6b);

        let observe_resp = bridge
            .observe(json!({
                "file": GO_FILE,
                "expressions": ["symbol"],
                "name": "cloud-go-adv",
                "connection_id": &go_conn_6b
            }))
            .await
            .expect("observe go via bridge failed");
        assert!(observe_resp.success, "observe should succeed");
        println!(
            "  observe ok: {} at {}#{}",
            observe_resp.metric_name, observe_resp.file, observe_resp.line
        );

        let adv_go_event_count =
            poll_for_events_bridge(&mut bridge, "cloud-go-adv", Duration::from_secs(15)).await;
        assert!(
            adv_go_event_count > 0,
            "Expected Go advertise-URL events but got none (waited 15s)"
        );
        println!("  Events captured: {}", adv_go_event_count);

        bridge.sleep_app(GO_APP_URL).await;
        bridge.kill().await;
        println!("  Go sleeping, bridge stopped");
    }

    // ── Phase 6c: Rust (explicit line, no file serving needed, skip if unavailable) ──
    println!("\n--- Phase 6c: Rust (explicit line) ---");
    let rust_adv_available = {
        let mut bridge = BridgeProcess::spawn(&daemon_url, DOCKER_AUTH_TOKEN, None).await;

        match bridge.wake(RUST_APP_URL).await {
            Ok(rust_wake_text) => {
                // Check daemon_url even if we might skip later
                if rust_wake_text.contains(ADVERTISE_URL) {
                    println!("  daemon_url verified: {}", ADVERTISE_URL);
                }

                if let Some(rust_conn) =
                    poll_for_connection_bridge(&mut bridge, "rust", Duration::from_secs(15)).await
                {
                    println!("  Rust connection: {}", rust_conn);
                    if let Err(e) = bridge
                        .add_metric(
                            "cloud-rust-adv",
                            &format!("{}#108", RUST_FILE),
                            &["symbol"],
                            &rust_conn,
                        )
                        .await
                    {
                        println!("  Rust add_metric failed (skipping): {}", e);
                        bridge.sleep_app(RUST_APP_URL).await;
                        bridge.kill().await;
                        false
                    } else {
                        let rust_event_count = poll_for_events_bridge(
                            &mut bridge,
                            "cloud-rust-adv",
                            Duration::from_secs(15),
                        )
                        .await;
                        if rust_event_count == 0 {
                            println!("  Rust events: 0 (skipping)");
                            bridge.sleep_app(RUST_APP_URL).await;
                            bridge.kill().await;
                            false
                        } else {
                            println!("  Events captured: {}", rust_event_count);
                            bridge.sleep_app(RUST_APP_URL).await;
                            bridge.kill().await;
                            true
                        }
                    }
                } else {
                    println!("  Rust connection not found (skipping)");
                    bridge.sleep_app(RUST_APP_URL).await;
                    bridge.kill().await;
                    false
                }
            }
            Err(e) => {
                println!("  Rust wake failed (lldb-dap likely unavailable): {}", e);
                bridge.kill().await;
                false
            }
        }
    };
    assert!(
        rust_adv_available,
        "Rust advertise-URL E2E failed — lldb-dap not working in container. \
        Dockerfile.rust runtime uses debian:bookworm-slim + lldb-17 from apt.llvm.org. \
        If this fails, the Docker image may be stale — run: task rebuild-daemon-container"
    );

    println!("\n{}", "=".repeat(60));
    println!("Phase 6 complete — advertise_url verified: Python + Go + Rust");
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

    // ── Phase 8: Discovery Endpoint + advertise_url ──
    // Tests the /detrix/discover endpoint on clients and daemon health advertise_url.
    // Relies on the advertise_url-configured daemon from Phase 7 (still running).
    println!("\n{}", "=".repeat(60));
    println!("Phase 8: Discovery endpoint + advertise_url verification");
    println!("{}", "=".repeat(60));

    // ── Phase 8a: Daemon health returns advertise_url ──
    println!("\n--- Phase 8a: Daemon health returns advertise_url ---");
    let http_client = reqwest::Client::new();
    let health_resp = http_client
        .get(format!("http://127.0.0.1:{}/health", DAEMON_HTTP_PORT))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("health request failed");
    assert!(health_resp.status().is_success(), "Health check failed");
    let health_json: serde_json::Value =
        health_resp.json().await.expect("health JSON parse failed");
    let health_advertise_url = health_json
        .get("advertiseUrl")
        .or_else(|| health_json.get("advertise_url"))
        .and_then(|v| v.as_str());
    assert_eq!(
        health_advertise_url,
        Some(ADVERTISE_URL),
        "Daemon health should return advertise_url.\nHealth JSON: {}",
        health_json
    );
    println!("  Daemon health advertise_url: {}", ADVERTISE_URL);

    // ── Phase 8b: Python client /detrix/discover returns advertise_url ──
    // Python control plane is mapped to host port 18091
    println!("\n--- Phase 8b: Python /detrix/discover endpoint ---");

    // Wake Python first so the client has registered with daemon and fetched advertise_url
    client
        .wake(PYTHON_APP_URL, None)
        .await
        .expect("wake python failed");
    let _py_conn = poll_for_connection(&client, "python", Duration::from_secs(15))
        .await
        .expect("Python connection not found within 15s");
    println!("  Python woke and registered");

    // Small delay for lazy fetch to complete
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Call /detrix/discover directly on the Python control plane (no auth required)
    let discover_resp = http_client
        .get("http://127.0.0.1:18091/detrix/discover")
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("discover request failed");
    assert!(
        discover_resp.status().is_success(),
        "Discover endpoint should succeed (no auth), got: {}",
        discover_resp.status()
    );
    let discover_json: serde_json::Value = discover_resp
        .json()
        .await
        .expect("discover JSON parse failed");
    println!("  Discover response: {}", discover_json);

    let discover_daemon_url = discover_json
        .get("daemon_url")
        .and_then(|v| v.as_str())
        .expect("discover should return daemon_url");
    assert_eq!(
        discover_daemon_url, ADVERTISE_URL,
        "Discover should return advertise_url ({}), not internal Docker URL",
        ADVERTISE_URL
    );
    println!("  Discover daemon_url: {} (correct!)", discover_daemon_url);

    let discover_name = discover_json
        .get("name")
        .and_then(|v| v.as_str())
        .expect("discover should return name");
    assert!(
        !discover_name.is_empty(),
        "Discover should return a non-empty name"
    );
    println!("  Discover name: {}", discover_name);

    // ── Phase 8c: Discover without auth (verify no token needed) ──
    println!("\n--- Phase 8c: Discover requires no auth ---");
    let no_auth_client = reqwest::Client::new(); // No auth headers
    let no_auth_resp = no_auth_client
        .get("http://127.0.0.1:18091/detrix/discover")
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("discover without auth failed");
    assert!(
        no_auth_resp.status().is_success(),
        "Discover should work without auth, got: {}",
        no_auth_resp.status()
    );
    println!("  Discover works without authentication");

    // ── Phase 8d: MCP Bridge discovery-first wake E2E ──
    // Spawn detrix mcp bridge against a DIFFERENT daemon URL.
    // The bridge should discover the Python app's daemon via /detrix/discover,
    // resolve credentials, switch to the correct daemon, and wake successfully.
    println!("\n--- Phase 8d: Bridge discovery-first wake E2E ---");

    // Set up credentials for the Docker daemon
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let creds_path = temp_dir.path().join("credentials.toml");
    let creds_content = format!(
        "[targets.\"localhost:{}\"]\ntoken = \"{}\"\n",
        DAEMON_HTTP_PORT, DOCKER_AUTH_TOKEN
    );
    std::fs::write(&creds_path, &creds_content).expect("write credentials");
    println!("  Credentials written to {:?}", creds_path);

    // Sleep Python first (clean state)
    sleep_app(&client, PYTHON_APP_URL).await;

    // Restart daemon with clean DB and advertise_url
    restart_daemon_with_env(&[("TEST_ADVERTISE_URL", ADVERTISE_URL)]).await;
    println!("  Daemon restarted with advertise_url");

    // Spawn bridge pointing to a wrong daemon URL initially
    // The bridge will discover via /detrix/discover and switch
    let ws_root_8 = get_workspace_root();
    let detrix_bin_8 =
        find_detrix_binary(&ws_root_8).expect("detrix binary not found — run `cargo build` first");

    // Use the actual daemon URL but the bridge should still go through discovery
    let bridge_daemon_url_8 = format!("http://127.0.0.1:{}", DAEMON_HTTP_PORT);
    let mut mcp_child_8 = tokio::process::Command::new(&detrix_bin_8)
        .args(["mcp", "--daemon-url", &bridge_daemon_url_8])
        .env("DETRIX_TOKEN", DOCKER_AUTH_TOKEN)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn detrix mcp failed");

    let mut mcp_stdin_8 = mcp_child_8.stdin.take().expect("stdin");
    let mcp_stdout_8 = mcp_child_8.stdout.take().expect("stdout");
    let mut mcp_reader_8 = BufReader::new(mcp_stdout_8);

    // Initialize
    let init_msg_8 = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "e2e-phase8", "version": "1.0" }
        },
        "id": 1
    });
    mcp_stdin_8
        .write_all(format!("{}\n", init_msg_8).as_bytes())
        .await
        .expect("write initialize");
    mcp_stdin_8.flush().await.expect("flush");

    let init_resp_8 = read_jsonrpc_response(&mut mcp_reader_8, 10).await;
    assert!(
        init_resp_8.get("result").is_some(),
        "initialize should succeed: {}",
        init_resp_8
    );
    println!("  Bridge initialized");

    // Send initialized notification
    let initialized_msg_8 = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    mcp_stdin_8
        .write_all(format!("{}\n", initialized_msg_8).as_bytes())
        .await
        .expect("write initialized");
    mcp_stdin_8.flush().await.expect("flush");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Wake via discovery flow: bridge discovers Python app's daemon
    // and forwards wake through it
    let wake_msg_8 = serde_json::json!({
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
    mcp_stdin_8
        .write_all(format!("{}\n", wake_msg_8).as_bytes())
        .await
        .expect("write wake");
    mcp_stdin_8.flush().await.expect("flush");

    let wake_resp_8 = read_jsonrpc_response(&mut mcp_reader_8, 60).await;
    let wake_text_8: String = wake_resp_8
        .pointer("/result/content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    // Wake should succeed (not an error)
    let is_error_8 = wake_resp_8
        .pointer("/result/isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        !is_error_8,
        "Wake via discovery should succeed.\nResponse text: {}",
        wake_text_8
    );
    println!(
        "  Wake via discovery succeeded: {}",
        &wake_text_8[..wake_text_8.len().min(200)]
    );

    // Cleanup
    drop(mcp_stdin_8);
    let _ = mcp_child_8.kill().await;
    sleep_app(&client, PYTHON_APP_URL).await;
    println!("  Bridge stopped, Python sleeping");

    println!("\n{}", "=".repeat(60));
    println!("Phase 8 complete — discovery endpoint + advertise_url verified");
    println!("{}", "=".repeat(60));

    // ── Phase 9: Scope-aware find_variable (Go) ──
    // Tests that observe() with find_variable="symbol" (no explicit line) picks
    // the function body usage (inside main()) rather than the struct field
    // definition (Order.Symbol) added before main().
    println!("\n{}", "=".repeat(60));
    println!("Phase 9: Scope-aware find_variable (Go)");
    println!("{}", "=".repeat(60));

    // Restart daemon (clean state)
    println!("\n--- Restarting daemon (clean state for Phase 9) ---");
    restart_daemon().await;
    println!("  Daemon restarted");

    let ws_root_9 = get_workspace_root();
    let mut bridge = BridgeProcess::spawn_in_dir(
        &daemon_url,
        DOCKER_AUTH_TOKEN,
        Some("host.docker.internal"),
        &ws_root_9,
    )
    .await;

    // Wake Go app
    println!("\n--- Phase 9: waking Go ---");
    bridge.wake(GO_APP_URL).await.expect("wake go failed");

    let go_conn_9 = poll_for_connection_bridge(&mut bridge, "go", Duration::from_secs(15))
        .await
        .expect("Go connection not found within 15s");
    println!("  Go connection: {}", go_conn_9);

    // Clean up any existing metrics
    for name in bridge.list_metrics_names().await {
        let _ = bridge.remove_metric(&name).await;
    }

    // Observe with find_variable="symbol" — no explicit line.
    // The Go fixture has Order.Symbol struct field AND symbol := ... in main().
    // Scope-aware logic should pick the function body usage, not the struct field.
    let observe_resp = bridge
        .observe(json!({
            "file": GO_FILE,
            "expressions": ["symbol"],
            "find_variable": "symbol",
            "name": "cloud-go-scope-aware",
            "connection_id": &go_conn_9
        }))
        .await
        .expect("observe go scope-aware failed");
    assert!(observe_resp.success, "observe should succeed");
    println!(
        "  observe ok: metric={} at {}#{}",
        observe_resp.metric_name, observe_resp.file, observe_resp.line
    );

    // Assert the chosen line is inside a function BODY, not at the struct field
    // definition (lines 83-87) or a function signature like `func placeOrder(symbol ...)` (line 89).
    // The scope-aware logic in inspect_generic_variable() deprioritizes both
    // struct fields (out-of-scope) and function signatures (is_function_signature).
    // The first valid body match is line 92: `log(..., symbol, ...)` inside placeOrder.
    let place_order_signature_line = go_lines::ORDER_STRUCT_END + 2; // line 89
    assert!(
        observe_resp.line > go_lines::ORDER_STRUCT_END,
        "Scope-aware find_variable should pick line inside a function body (> {}), \
         but got line {} (likely struct field definition).",
        go_lines::ORDER_STRUCT_END,
        observe_resp.line
    );
    assert!(
        observe_resp.line != place_order_signature_line,
        "Scope-aware find_variable should NOT pick function signature line {}, \
         but got it. Function signatures should be deprioritized.",
        place_order_signature_line
    );
    println!(
        "  Scope-aware line {} > ORDER_STRUCT_END {} and != signature {} — correctly deprioritized",
        observe_resp.line,
        go_lines::ORDER_STRUCT_END,
        place_order_signature_line
    );

    // Poll for events to confirm metric actually fires
    let scope_event_count =
        poll_for_events_bridge(&mut bridge, "cloud-go-scope-aware", Duration::from_secs(15)).await;
    assert!(
        scope_event_count > 0,
        "Expected scope-aware events but got none (waited 15s)"
    );
    println!("  Scope-aware events captured: {}", scope_event_count);

    // Cleanup
    println!("\n--- Phase 9 cleanup ---");
    bridge.sleep_app(GO_APP_URL).await;
    bridge.kill().await;
    println!("  Go sleeping, bridge stopped");

    println!("\n{}", "=".repeat(60));
    println!("Phase 9 complete — scope-aware find_variable verified");
    println!("{}", "=".repeat(60));

    // ── Phase 10: Multi-tenant access control ──
    // Restart daemon with multi-user config (alice/bob/admin), then:
    //   1. Alice adds a Python metric (alice token + PYTHON_APP_URL bridge)
    //   2. Bob tries DELETE on Alice's metric → 403
    //   3. Bob adds metric at same Python location → both metrics stored
    //   4. Alice disables her metric → Bob's metric still fires events
    //   5. Admin sees all metrics (both users)
    println!("\n{}", "=".repeat(60));
    println!("Phase 10: Multi-tenant access control");
    println!("{}", "=".repeat(60));

    // Restart daemon with multi-user config via DETRIX_CONFIG env var.
    // detrix-multiuser.toml is already mounted read-only at
    // /data/detrix/detrix-multiuser.toml (docker-compose.yml).
    println!("\n--- Restarting daemon with multi-user config ---");
    restart_daemon_with_env(&[("DETRIX_CONFIG", "/data/detrix/detrix-multiuser.toml")]).await;
    println!("  Daemon restarted with multi-user config");

    const ALICE_TOKEN: &str = "dtx_alice_cloud";
    const BOB_TOKEN: &str = "dtx_bob_cloud";
    const ADMIN_TOKEN: &str = "dtx_admin_cloud";

    let daemon_url_10 = format!("http://127.0.0.1:{}", DAEMON_HTTP_PORT);

    // ── Phase 10a: Alice wakes Python and adds a metric ──
    println!("\n--- Phase 10a: Alice adds Python metric ---");
    let mut alice_bridge = BridgeProcess::spawn(&daemon_url_10, ALICE_TOKEN, None).await;
    alice_bridge
        .wake(PYTHON_APP_URL)
        .await
        .expect("Phase 10: alice wake failed");

    let py_conn_10 =
        poll_for_connection_bridge(&mut alice_bridge, "python", Duration::from_secs(15))
            .await
            .expect("Phase 10: Python connection not found within 15s");
    println!("  Alice Python connection: {}", py_conn_10);

    alice_bridge
        .add_metric(
            "cloud-alice-metric",
            &format!("{}#60", PYTHON_FILE),
            &["price"],
            &py_conn_10,
        )
        .await
        .expect("Phase 10: alice add_metric failed");
    println!("  Alice added metric cloud-alice-metric");

    // Verify Alice's metric fires events
    let alice_events = poll_for_events_bridge(
        &mut alice_bridge,
        "cloud-alice-metric",
        Duration::from_secs(15),
    )
    .await;
    assert!(
        alice_events > 0,
        "Phase 10: Alice's metric should fire events (got 0)"
    );
    println!("  Alice's metric fired {} events", alice_events);

    alice_bridge.kill().await;

    // ── Phase 10b: Bob tries to DELETE Alice's metric → 403 ──
    println!("\n--- Phase 10b: Bob cannot delete Alice's metric (403) ---");
    let http_client_10 = reqwest::Client::new();

    // Get Alice's metric ID via admin token first
    let metrics_resp = http_client_10
        .get(format!("{}/api/v1/metrics", daemon_url_10))
        .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("Phase 10: list metrics (admin) failed");
    assert!(
        metrics_resp.status().is_success(),
        "Phase 10: admin list metrics failed: {}",
        metrics_resp.status()
    );
    let metrics_json: serde_json::Value = metrics_resp
        .json()
        .await
        .expect("Phase 10: parse metrics JSON failed");
    let alice_metric_id = metrics_json
        .get("metrics")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|m| {
                m.get("name")
                    .and_then(|n| n.as_str())
                    .map(|n| n == "cloud-alice-metric")
                    .unwrap_or(false)
            })
        })
        .and_then(|m| m.get("metricId").and_then(|id| id.as_u64()))
        .expect("Phase 10: cloud-alice-metric not found in admin list")
        .to_string();
    println!("  Alice's metric ID: {}", alice_metric_id);

    // Bob tries to delete Alice's metric
    let bob_delete_resp = http_client_10
        .delete(format!(
            "{}/api/v1/metrics/{}",
            daemon_url_10, alice_metric_id
        ))
        .header("Authorization", format!("Bearer {}", BOB_TOKEN))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("Phase 10: bob delete request failed");
    assert_eq!(
        bob_delete_resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "Phase 10: Bob should get 403 when deleting Alice's metric, got: {}",
        bob_delete_resp.status()
    );
    println!("  Bob DELETE Alice's metric → 403 (correct)");

    // ── Phase 10c: Bob adds metric at a different Python location → both stored ──
    // Note: Bob uses line 65 (pnl calculation) to get his own independent logpoint.
    // Each user gets their own metric in storage; admin sees both (multi-tenant storage verified).
    println!("\n--- Phase 10c: Bob adds metric at different Python location ---");
    let mut bob_bridge = BridgeProcess::spawn(&daemon_url_10, BOB_TOKEN, None).await;
    bob_bridge
        .wake(PYTHON_APP_URL)
        .await
        .expect("Phase 10: bob wake failed");

    let py_conn_bob =
        poll_for_connection_bridge(&mut bob_bridge, "python", Duration::from_secs(15))
            .await
            .expect("Phase 10: Bob Python connection not found within 15s");
    println!("  Bob Python connection: {}", py_conn_bob);

    bob_bridge
        .add_metric(
            "cloud-bob-metric",
            &format!("{}#65", PYTHON_FILE),
            &["quantity"],
            &py_conn_bob,
        )
        .await
        .expect("Phase 10: bob add_metric failed");
    println!("  Bob added metric cloud-bob-metric at line 65");

    // Verify Bob's metric fires events from his own logpoint
    let bob_events =
        poll_for_events_bridge(&mut bob_bridge, "cloud-bob-metric", Duration::from_secs(15)).await;
    assert!(
        bob_events > 0,
        "Phase 10: Bob's metric should fire events (got 0)"
    );
    println!("  Bob's metric fired {} events", bob_events);

    // Admin should see both metrics
    let admin_list_resp = http_client_10
        .get(format!("{}/api/v1/metrics", daemon_url_10))
        .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("Phase 10: admin list metrics failed");
    let admin_metrics: serde_json::Value = admin_list_resp
        .json()
        .await
        .expect("Phase 10: parse admin metrics failed");
    let admin_metric_count = admin_metrics
        .get("metrics")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        admin_metric_count >= 2,
        "Phase 10: Admin should see ≥2 metrics (alice + bob), got: {}",
        admin_metric_count
    );
    println!(
        "  Admin sees {} metrics (both alice + bob)",
        admin_metric_count
    );

    bob_bridge.kill().await;

    // ── Phase 10d: Alice disables her metric → Bob's still fires ──
    println!("\n--- Phase 10d: Alice disables her metric, Bob's still fires ---");
    let alice_disable_resp = http_client_10
        .post(format!(
            "{}/api/v1/metrics/{}/disable",
            daemon_url_10, alice_metric_id
        ))
        .header("Authorization", format!("Bearer {}", ALICE_TOKEN))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("Phase 10: alice disable request failed");
    assert!(
        alice_disable_resp.status().is_success(),
        "Phase 10: Alice disable her metric failed: {}",
        alice_disable_resp.status()
    );
    println!("  Alice disabled her metric");

    // Bob's metric at same line should still fire — open a new bridge to check
    let mut bob_bridge2 = BridgeProcess::spawn(&daemon_url_10, BOB_TOKEN, None).await;
    let bob_events2 = poll_for_events_bridge(
        &mut bob_bridge2,
        "cloud-bob-metric",
        Duration::from_secs(15),
    )
    .await;
    assert!(
        bob_events2 > 0,
        "Phase 10: Bob's metric should still fire after Alice disabled hers (got 0)"
    );
    println!(
        "  Bob's metric still fires {} events after Alice disabled hers",
        bob_events2
    );
    bob_bridge2.kill().await;

    // ── Phase 10e: Same-line overlapping metrics ──
    // Tests 3 overlap types × 3 steps (create, disable-one, delete-one).
    // All metrics at line 60: order_id = place_order(symbol, quantity, price)
    // Available vars at line 60: price, quantity, order_id, symbol, iteration
    //
    // For each type:
    //   Step 1: Alice + Bob add metrics at same line → admin sees both
    //   Step 2: Disable Alice → Bob becomes DAP template → Bob fires events
    //   Step 3: Delete Alice (disabled) → Bob still fires events
    println!("\n{}", "=".repeat(60));
    println!("Phase 10e: Same-line overlapping metrics");
    println!("{}", "=".repeat(60));

    // Delete cloud-alice-metric (disabled in Phase 10d, still in DB at line 60 for user "alice").
    // Without this, find_by_location would return the existing metric when Alice adds at line 60,
    // and p10e-*-alice would never be created (the service merges into the existing one instead).
    let _ = http_client_10
        .delete(format!(
            "{}/api/v1/metrics/{}",
            daemon_url_10, alice_metric_id
        ))
        .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    println!("  Pre-cleanup: removed cloud-alice-metric to avoid line-60 collision");

    // ── Type 1: Non-overlapping (Alice: price, Bob: quantity) ──
    println!("\n--- 10e-Type1: Non-overlapping expressions (Alice:price, Bob:quantity) ---");
    {
        let mut alice_b = BridgeProcess::spawn(&daemon_url_10, ALICE_TOKEN, None).await;
        let mut bob_b = BridgeProcess::spawn(&daemon_url_10, BOB_TOKEN, None).await;

        // Step 1: Both add metrics at line 60
        alice_b
            .add_metric(
                "p10e-1-alice",
                &format!("{}#60", PYTHON_FILE),
                &["price"],
                &py_conn_10,
            )
            .await
            .expect("10e-Type1: alice add_metric failed");
        bob_b
            .add_metric(
                "p10e-1-bob",
                &format!("{}#60", PYTHON_FILE),
                &["quantity"],
                &py_conn_10,
            )
            .await
            .expect("10e-Type1: bob add_metric failed");

        let a_id =
            admin_get_metric_id(&http_client_10, &daemon_url_10, ADMIN_TOKEN, "p10e-1-alice")
                .await
                .expect("10e-Type1: p10e-1-alice not found in admin list");
        let b_id = admin_get_metric_id(&http_client_10, &daemon_url_10, ADMIN_TOKEN, "p10e-1-bob")
            .await
            .expect("10e-Type1: p10e-1-bob not found in admin list");
        println!("  Step1: both stored (alice={}, bob={})", a_id, b_id);

        // Step 2: Disable Alice → Bob becomes template → Bob fires events
        http_client_10
            .post(format!("{}/api/v1/metrics/{}/disable", daemon_url_10, a_id))
            .header("Authorization", format!("Bearer {}", ALICE_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .expect("10e-Type1: alice disable request failed")
            .error_for_status()
            .expect("10e-Type1: alice disable returned error status");
        let bob_ev1 =
            poll_for_events_bridge(&mut bob_b, "p10e-1-bob", Duration::from_secs(20)).await;
        assert!(
            bob_ev1 > 0,
            "10e-Type1 Step2: Bob should get events after Alice disabled (got 0)"
        );
        println!("  Step2: Bob got {} events after Alice disabled", bob_ev1);

        // Step 3: Delete Alice's (disabled) metric → Bob still fires events
        http_client_10
            .delete(format!("{}/api/v1/metrics/{}", daemon_url_10, a_id))
            .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .expect("10e-Type1: alice delete request failed")
            .error_for_status()
            .expect("10e-Type1: alice delete returned error status");
        let bob_ev2 =
            poll_for_events_bridge(&mut bob_b, "p10e-1-bob", Duration::from_secs(20)).await;
        assert!(
            bob_ev2 > 0,
            "10e-Type1 Step3: Bob should get events after Alice deleted (got 0)"
        );
        println!("  Step3: Bob got {} events after Alice deleted", bob_ev2);

        // Cleanup: delete Bob's metric
        let _ = http_client_10
            .delete(format!("{}/api/v1/metrics/{}", daemon_url_10, b_id))
            .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        alice_b.kill().await;
        bob_b.kill().await;
    }
    println!("  10e-Type1 PASSED");

    // ── Type 2: Identical expressions (Alice: price, Bob: price) ──
    println!("\n--- 10e-Type2: Identical expressions (Alice:price, Bob:price) ---");
    {
        let mut alice_b = BridgeProcess::spawn(&daemon_url_10, ALICE_TOKEN, None).await;
        let mut bob_b = BridgeProcess::spawn(&daemon_url_10, BOB_TOKEN, None).await;

        // Step 1: Both add metrics at line 60 with same expression
        alice_b
            .add_metric(
                "p10e-2-alice",
                &format!("{}#60", PYTHON_FILE),
                &["price"],
                &py_conn_10,
            )
            .await
            .expect("10e-Type2: alice add_metric failed");
        bob_b
            .add_metric(
                "p10e-2-bob",
                &format!("{}#60", PYTHON_FILE),
                &["price"],
                &py_conn_10,
            )
            .await
            .expect("10e-Type2: bob add_metric failed");

        let a_id =
            admin_get_metric_id(&http_client_10, &daemon_url_10, ADMIN_TOKEN, "p10e-2-alice")
                .await
                .expect("10e-Type2: p10e-2-alice not found in admin list");
        let b_id = admin_get_metric_id(&http_client_10, &daemon_url_10, ADMIN_TOKEN, "p10e-2-bob")
            .await
            .expect("10e-Type2: p10e-2-bob not found in admin list");
        println!("  Step1: both stored (alice={}, bob={})", a_id, b_id);

        // Step 2: Disable Alice → Bob becomes template → Bob fires events
        http_client_10
            .post(format!("{}/api/v1/metrics/{}/disable", daemon_url_10, a_id))
            .header("Authorization", format!("Bearer {}", ALICE_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .expect("10e-Type2: alice disable request failed")
            .error_for_status()
            .expect("10e-Type2: alice disable returned error status");
        let bob_ev1 =
            poll_for_events_bridge(&mut bob_b, "p10e-2-bob", Duration::from_secs(20)).await;
        assert!(
            bob_ev1 > 0,
            "10e-Type2 Step2: Bob should get events after Alice disabled (got 0)"
        );
        println!("  Step2: Bob got {} events after Alice disabled", bob_ev1);

        // Step 3: Delete Alice's (disabled) metric → Bob still fires events
        http_client_10
            .delete(format!("{}/api/v1/metrics/{}", daemon_url_10, a_id))
            .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .expect("10e-Type2: alice delete request failed")
            .error_for_status()
            .expect("10e-Type2: alice delete returned error status");
        let bob_ev2 =
            poll_for_events_bridge(&mut bob_b, "p10e-2-bob", Duration::from_secs(20)).await;
        assert!(
            bob_ev2 > 0,
            "10e-Type2 Step3: Bob should get events after Alice deleted (got 0)"
        );
        println!("  Step3: Bob got {} events after Alice deleted", bob_ev2);

        // Cleanup: delete Bob's metric
        let _ = http_client_10
            .delete(format!("{}/api/v1/metrics/{}", daemon_url_10, b_id))
            .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        alice_b.kill().await;
        bob_b.kill().await;
    }
    println!("  10e-Type2 PASSED");

    // ── Type 3: Partially overlapping (Alice: price, Bob: price + quantity) ──
    println!("\n--- 10e-Type3: Partial overlap (Alice:[price], Bob:[price,quantity]) ---");
    {
        let mut alice_b = BridgeProcess::spawn(&daemon_url_10, ALICE_TOKEN, None).await;
        let mut bob_b = BridgeProcess::spawn(&daemon_url_10, BOB_TOKEN, None).await;

        // Step 1: Alice: [price], Bob: [price, quantity] at same line
        alice_b
            .add_metric(
                "p10e-3-alice",
                &format!("{}#60", PYTHON_FILE),
                &["price"],
                &py_conn_10,
            )
            .await
            .expect("10e-Type3: alice add_metric failed");
        bob_b
            .add_metric(
                "p10e-3-bob",
                &format!("{}#60", PYTHON_FILE),
                &["price", "quantity"],
                &py_conn_10,
            )
            .await
            .expect("10e-Type3: bob add_metric failed");

        let a_id =
            admin_get_metric_id(&http_client_10, &daemon_url_10, ADMIN_TOKEN, "p10e-3-alice")
                .await
                .expect("10e-Type3: p10e-3-alice not found in admin list");
        let b_id = admin_get_metric_id(&http_client_10, &daemon_url_10, ADMIN_TOKEN, "p10e-3-bob")
            .await
            .expect("10e-Type3: p10e-3-bob not found in admin list");
        println!("  Step1: both stored (alice={}, bob={})", a_id, b_id);

        // Step 2: Disable Alice → Bob becomes template (with price+quantity) → Bob fires events
        http_client_10
            .post(format!("{}/api/v1/metrics/{}/disable", daemon_url_10, a_id))
            .header("Authorization", format!("Bearer {}", ALICE_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .expect("10e-Type3: alice disable request failed")
            .error_for_status()
            .expect("10e-Type3: alice disable returned error status");
        let bob_ev1 =
            poll_for_events_bridge(&mut bob_b, "p10e-3-bob", Duration::from_secs(20)).await;
        assert!(
            bob_ev1 > 0,
            "10e-Type3 Step2: Bob should get events after Alice disabled (got 0)"
        );
        println!("  Step2: Bob got {} events after Alice disabled", bob_ev1);

        // Step 3: Delete Alice's (disabled) metric → Bob still fires events
        http_client_10
            .delete(format!("{}/api/v1/metrics/{}", daemon_url_10, a_id))
            .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .expect("10e-Type3: alice delete request failed")
            .error_for_status()
            .expect("10e-Type3: alice delete returned error status");
        let bob_ev2 =
            poll_for_events_bridge(&mut bob_b, "p10e-3-bob", Duration::from_secs(20)).await;
        assert!(
            bob_ev2 > 0,
            "10e-Type3 Step3: Bob should get events after Alice deleted (got 0)"
        );
        println!("  Step3: Bob got {} events after Alice deleted", bob_ev2);

        // Cleanup: delete Bob's metric
        let _ = http_client_10
            .delete(format!("{}/api/v1/metrics/{}", daemon_url_10, b_id))
            .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        alice_b.kill().await;
        bob_b.kill().await;
    }
    println!("  10e-Type3 PASSED");

    println!("\n{}", "=".repeat(60));
    println!("Phase 10e complete — same-line overlapping metrics verified");
    println!("{}", "=".repeat(60));

    // ── Phase 10 cleanup ──
    println!("\n--- Phase 10 cleanup ---");
    // cloud-alice-metric was already deleted at the start of Phase 10e; this is a no-op.
    let _ = http_client_10
        .delete(format!(
            "{}/api/v1/metrics/{}",
            daemon_url_10, alice_metric_id
        ))
        .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    // Delete bob metric (cloud-bob-metric at line 65, from Phase 10c)
    if let Some(bob_metric_id) = admin_get_metric_id(
        &http_client_10,
        &daemon_url_10,
        ADMIN_TOKEN,
        "cloud-bob-metric",
    )
    .await
    {
        let _ = http_client_10
            .delete(format!(
                "{}/api/v1/metrics/{}",
                daemon_url_10, bob_metric_id
            ))
            .header("Authorization", format!("Bearer {}", ADMIN_TOKEN))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
    }
    // Sleep Python app
    let mut cleanup_bridge = BridgeProcess::spawn(&daemon_url_10, ADMIN_TOKEN, None).await;
    cleanup_bridge.sleep_app(PYTHON_APP_URL).await;
    cleanup_bridge.kill().await;
    println!("  Phase 10 cleaned up");

    println!("\n{}", "=".repeat(60));
    println!("Phase 10 complete — multi-tenant access control verified");
    println!("{}", "=".repeat(60));

    println!("\n{}", "=".repeat(60));
    println!("ALL PHASES COMPLETE — Docker Cloud E2E passed!");
    println!("{}", "=".repeat(60));
}
