//! Multi-tenant E2E tests
//!
//! Verifies per-user metric isolation and access control in Simple auth mode.
//! Three users: alice (user), bob (user), admin (admin).
//!
//! All tests require debugpy to create live connections.  Skip gracefully when
//! debugpy is unavailable.
//!
//! Run with:
//!   cargo test --package detrix-testing --test multitenant_e2e -- --nocapture

use detrix_config::constants::{AUTHORIZATION_HEADER, BEARER_PREFIX};
use detrix_testing::e2e::availability::is_debugpy_available;
use detrix_testing::e2e::executor::{
    find_detrix_binary, get_debugpy_port, get_grpc_port, get_http_port, is_port_available,
    start_debugpy_setsid, wait_for_debugger_port, wait_for_port, TestDaemonSetup,
};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::sleep;

// ─── Auth tokens ─────────────────────────────────────────────────────────────

const ALICE_TOKEN: &str = "dtx_alice_test";
const BOB_TOKEN: &str = "dtx_bob_test";
const ADMIN_TOKEN: &str = "dtx_admin_test";

// ─── Daemon wrapper ───────────────────────────────────────────────────────────

struct MultitenantDaemon {
    temp_dir: TempDir,
    http_port: u16,
    grpc_port: u16,
    daemon_process: Option<Child>,
    daemon_log_path: PathBuf,
    workspace_root: PathBuf,
    debugpy_process: Option<Child>,
    debugpy_port: u16,
    /// Single shared connection ID for all metric operations.
    /// debugpy only supports one DAP client at a time, so all tests share one
    /// connection. User isolation is enforced via user_id on metrics (from auth
    /// token), not via separate connections.
    connection_id: String,
}

impl MultitenantDaemon {
    fn new() -> Self {
        let setup = TestDaemonSetup::new();
        let http_port = get_http_port();
        let grpc_port = get_grpc_port();
        let debugpy_port = get_debugpy_port();
        let daemon_log_path = setup.temp_dir.path().join("daemon.log");

        MultitenantDaemon {
            http_port,
            grpc_port,
            daemon_log_path,
            workspace_root: setup.workspace_root.clone(),
            debugpy_port,
            debugpy_process: None,
            daemon_process: None,
            temp_dir: setup.temp_dir,
            connection_id: String::new(), // set by start_debugpy after connection is created
        }
    }

    async fn start(&mut self) -> Result<(), String> {
        let db_path = self.temp_dir.path().join("detrix.db");
        let config_content = format!(
            r#"
[storage]
path = "{db}"

[api]
port_fallback = false

[api.rest]
enabled = true
host = "127.0.0.1"
port = {http}

[api.grpc]
enabled = true
host = "127.0.0.1"
port = {grpc}

[api.auth]
mode = "simple"

[[api.auth.users]]
token = "{alice}"
user_id = "alice"
role = "user"

[[api.auth.users]]
token = "{bob}"
user_id = "bob"
role = "user"

[[api.auth.users]]
token = "{admin}"
user_id = "admin"
role = "admin"

[safety]
enable_ast_analysis = false
"#,
            db = db_path.display(),
            http = self.http_port,
            grpc = self.grpc_port,
            alice = ALICE_TOKEN,
            bob = BOB_TOKEN,
            admin = ADMIN_TOKEN,
        );

        let config_path = self.temp_dir.path().join("detrix.toml");
        std::fs::write(&config_path, config_content).map_err(|e| e.to_string())?;

        let binary = find_detrix_binary(&self.workspace_root)
            .ok_or("detrix binary not found — run `cargo build`")?;

        let log_file = std::fs::File::create(&self.daemon_log_path).map_err(|e| e.to_string())?;
        let log_stderr = log_file.try_clone().map_err(|e| e.to_string())?;

        let process = Command::new(&binary)
            .args(["serve", "--config", config_path.to_str().unwrap()])
            .current_dir(&self.workspace_root)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_stderr))
            .spawn()
            .map_err(|e| format!("spawn failed: {}", e))?;

        self.daemon_process = Some(process);

        if !wait_for_port(self.http_port, 30).await {
            self.print_logs(20);
            return Err(format!("daemon not ready on :{}", self.http_port));
        }
        Ok(())
    }

    /// Start debugpy on the pre-allocated debugpy_port and create a shared DAP connection.
    ///
    /// debugpy in `--wait-for-client` mode accepts exactly ONE DAP client.  All tests
    /// share this connection — user isolation comes from the user_id stamped on each
    /// metric by the auth middleware, not from separate connections.
    async fn start_debugpy(&mut self) -> Result<(), String> {
        let script = self
            .workspace_root
            .join("fixtures/python/detrix_example_app.py");

        if !is_port_available(self.debugpy_port) {
            return Err(format!("debugpy port {} already in use", self.debugpy_port));
        }
        let process = start_debugpy_setsid(self.debugpy_port, script.to_str().unwrap())
            .map_err(|e| format!("start_debugpy_setsid: {}", e))?;

        self.debugpy_process = Some(process);

        // Use wait_for_debugger_port (lsof-based, NOT TCP connect).
        // A TCP connect probe would consume debugpy's single connection slot in
        // --wait-for-client mode, causing the daemon's subsequent DAP init to fail.
        if !wait_for_debugger_port(self.debugpy_port, 60).await {
            return Err(format!("debugpy not listening on :{}", self.debugpy_port));
        }
        // Stabilization pause: allow debugpy to finish internal setup after port opens
        sleep(Duration::from_millis(500)).await;

        // Create the single shared connection.  ALICE_TOKEN is used but the connection
        // itself has no user-level ownership; user_id is stamped on metrics by auth.
        self.connection_id = self.create_connection(ALICE_TOKEN, "test-conn").await?;
        Ok(())
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    /// Build a reqwest Client with a Bearer token.
    fn client_for(token: &str) -> Client {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            AUTHORIZATION_HEADER,
            format!("{}{}", BEARER_PREFIX, token).parse().unwrap(),
        );
        Client::builder()
            // 60s: create_connection involves DAP initialization which can be slow
            // under concurrent load (8+ tests, multiple Python processes).
            .timeout(Duration::from_secs(60))
            .default_headers(headers)
            .build()
            .unwrap()
    }

    /// POST /api/v1/connections using the debugpy port.  Returns connection_id.
    async fn create_connection(&self, token: &str, name: &str) -> Result<String, String> {
        let client = Self::client_for(token);
        let resp = client
            .post(format!("{}/api/v1/connections", self.base_url()))
            .json(&json!({
                "name": name,
                "host": "127.0.0.1",
                "port": self.debugpy_port,
                "language": "python",
                "workspaceRoot": self.workspace_root.to_string_lossy(),
                "hostname": "localhost"
            }))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("create_connection → {}: {}", status, body));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        body["connectionId"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| format!("no connectionId in response: {}", body))
    }

    /// POST /api/v1/metrics (disabled).  Returns metric ID string.
    async fn create_disabled_metric(
        &self,
        token: &str,
        name: &str,
        connection_id: &str,
    ) -> Result<String, String> {
        self.create_metric_in_group(token, name, connection_id, None)
            .await
    }

    /// POST /api/v1/metrics with optional group.  Returns metric ID string.
    async fn create_metric_in_group(
        &self,
        token: &str,
        name: &str,
        connection_id: &str,
        group: Option<&str>,
    ) -> Result<String, String> {
        let client = Self::client_for(token);
        let mut body = json!({
            "name": name,
            "connectionId": connection_id,
            "location": {"file": "/tmp/test.py", "line": 1},
            "expressions": ["x"],
            "enabled": false
        });
        if let Some(g) = group {
            body["group"] = json!(g);
        }
        let resp = client
            .post(format!("{}/api/v1/metrics", self.base_url()))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("create_metric → {}: {}", status, text));
        }
        let json: Value = resp.json().await.map_err(|e| e.to_string())?;
        json["metricId"]
            .as_i64()
            .map(|id| id.to_string())
            .ok_or_else(|| format!("no metricId in response: {}", json))
    }

    /// GET /api/v1/metrics.  Returns list of metric names.
    async fn list_metric_names(&self, token: &str) -> Result<Vec<String>, String> {
        let client = Self::client_for(token);
        let resp = client
            .get(format!("{}/api/v1/metrics", self.base_url()))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("list_metrics → {} {}", status, body));
        }
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        let names = body["metrics"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|m| m["name"].as_str().map(|s| s.to_string()))
            .collect();
        Ok(names)
    }

    /// DELETE /api/v1/metrics/{id}.  Returns HTTP status.
    async fn delete_metric(&self, token: &str, metric_id: &str) -> StatusCode {
        Self::client_for(token)
            .delete(format!("{}/api/v1/metrics/{}", self.base_url(), metric_id))
            .send()
            .await
            .map(|r| r.status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// POST /api/v1/metrics/{id}/enable.  Returns HTTP status.
    async fn enable_metric(&self, token: &str, metric_id: &str) -> StatusCode {
        Self::client_for(token)
            .post(format!(
                "{}/api/v1/metrics/{}/enable",
                self.base_url(),
                metric_id
            ))
            .send()
            .await
            .map(|r| r.status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// POST /api/v1/groups/{name}/disable.  Returns HTTP status.
    async fn disable_group(&self, token: &str, group: &str) -> StatusCode {
        Self::client_for(token)
            .post(format!(
                "{}/api/v1/groups/{}/disable",
                self.base_url(),
                group
            ))
            .send()
            .await
            .map(|r| r.status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    fn print_logs(&self, n: usize) {
        if let Ok(content) = std::fs::read_to_string(&self.daemon_log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(n);
            println!("=== daemon log (last {}) ===", n);
            for line in &lines[start..] {
                println!("{}", line);
            }
            println!("===========================");
        }
    }
}

impl Drop for MultitenantDaemon {
    fn drop(&mut self) {
        if let Some(mut p) = self.daemon_process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
        if let Some(mut p) = self.debugpy_process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}

// ─── Test helpers ─────────────────────────────────────────────────────────────

/// Start daemon + debugpy.  Returns None (prints skip) if debugpy unavailable.
async fn setup() -> Option<MultitenantDaemon> {
    if !is_debugpy_available().await {
        println!("[SKIP] debugpy not available — skipping multitenant test");
        return None;
    }
    let mut d = MultitenantDaemon::new();
    if let Err(e) = d.start().await {
        d.print_logs(30);
        panic!("daemon start failed: {}", e);
    }
    if let Err(e) = d.start_debugpy().await {
        d.print_logs(20);
        panic!("debugpy start failed: {}", e);
    }
    Some(d)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// Alice lists metrics → only her own; Bob lists → only his.
#[tokio::test]
async fn test_list_isolation() {
    let Some(d) = setup().await else { return };

    // Both users share the same connection; user_id is stamped per metric by auth.
    let conn = d.connection_id.clone();
    d.create_disabled_metric(ALICE_TOKEN, "alice-metric-1", &conn)
        .await
        .expect("alice metric");
    d.create_disabled_metric(BOB_TOKEN, "bob-metric-1", &conn)
        .await
        .expect("bob metric");

    let alice_list = d.list_metric_names(ALICE_TOKEN).await.unwrap();
    let bob_list = d.list_metric_names(BOB_TOKEN).await.unwrap();

    assert!(
        alice_list.contains(&"alice-metric-1".to_string()),
        "alice should see her own metric; got: {:?}",
        alice_list
    );
    assert!(
        !alice_list.contains(&"bob-metric-1".to_string()),
        "alice should NOT see bob's metric; got: {:?}",
        alice_list
    );
    assert!(
        bob_list.contains(&"bob-metric-1".to_string()),
        "bob should see his own metric; got: {:?}",
        bob_list
    );
    assert!(
        !bob_list.contains(&"alice-metric-1".to_string()),
        "bob should NOT see alice's metric; got: {:?}",
        bob_list
    );
    println!("✓ list isolation verified");
}

/// Admin sees metrics from all users.
#[tokio::test]
async fn test_admin_sees_all() {
    let Some(d) = setup().await else { return };

    let conn = d.connection_id.clone();
    d.create_disabled_metric(ALICE_TOKEN, "alice-admin-test", &conn)
        .await
        .expect("alice metric");
    d.create_disabled_metric(BOB_TOKEN, "bob-admin-test", &conn)
        .await
        .expect("bob metric");

    let admin_list = d.list_metric_names(ADMIN_TOKEN).await.unwrap();

    assert!(
        admin_list.contains(&"alice-admin-test".to_string()),
        "admin should see alice's metric; got: {:?}",
        admin_list
    );
    assert!(
        admin_list.contains(&"bob-admin-test".to_string()),
        "admin should see bob's metric; got: {:?}",
        admin_list
    );
    println!("✓ admin sees all metrics");
}

/// Bob DELETE on Alice's metric → 403.
#[tokio::test]
async fn test_cross_user_delete_denied() {
    let Some(d) = setup().await else { return };

    let conn = d.connection_id.clone();
    let alice_id = d
        .create_disabled_metric(ALICE_TOKEN, "alice-del-test", &conn)
        .await
        .expect("alice metric");

    let status = d.delete_metric(BOB_TOKEN, &alice_id).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "bob deleting alice's metric should → 403"
    );

    // Metric still exists
    let alice_list = d.list_metric_names(ALICE_TOKEN).await.unwrap();
    assert!(
        alice_list.contains(&"alice-del-test".to_string()),
        "alice's metric should still exist after bob's failed delete"
    );
    println!("✓ cross-user delete denied");
}

/// Bob enable on Alice's metric → 403.
#[tokio::test]
async fn test_cross_user_toggle_denied() {
    let Some(d) = setup().await else { return };

    let conn = d.connection_id.clone();
    let alice_id = d
        .create_disabled_metric(ALICE_TOKEN, "alice-toggle-test", &conn)
        .await
        .expect("alice metric");

    let status = d.enable_metric(BOB_TOKEN, &alice_id).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "bob enabling alice's metric should → 403"
    );
    println!("✓ cross-user toggle denied");
}

/// Bob disable-group "shared-grp"; Alice's metrics in that group remain unaffected.
#[tokio::test]
async fn test_cross_user_group_no_effect() {
    let Some(d) = setup().await else { return };

    let conn = d.connection_id.clone();
    d.create_metric_in_group(ALICE_TOKEN, "alice-grp-metric", &conn, Some("shared-grp"))
        .await
        .expect("alice metric in group");

    // Bob disables group "shared-grp" — only affects Bob's own metrics in that group
    let status = d.disable_group(BOB_TOKEN, "shared-grp").await;
    assert!(
        status.is_success() || status == StatusCode::NOT_FOUND,
        "group disable unexpected status: {}",
        status
    );

    // Alice's metric still present
    let alice_list = d.list_metric_names(ALICE_TOKEN).await.unwrap();
    assert!(
        alice_list.contains(&"alice-grp-metric".to_string()),
        "alice's metric should still exist after bob's group disable"
    );
    println!("✓ cross-user group disable has no effect on other user's metrics");
}

/// Admin can delete any user's metric.
#[tokio::test]
async fn test_admin_can_delete_any() {
    let Some(d) = setup().await else { return };

    let conn = d.connection_id.clone();
    let alice_id = d
        .create_disabled_metric(ALICE_TOKEN, "alice-admin-del", &conn)
        .await
        .expect("alice metric");

    let status = d.delete_metric(ADMIN_TOKEN, &alice_id).await;
    assert!(
        status.is_success(),
        "admin should delete any metric, got: {}",
        status
    );

    let alice_list = d.list_metric_names(ALICE_TOKEN).await.unwrap();
    assert!(
        !alice_list.contains(&"alice-admin-del".to_string()),
        "metric should be gone after admin delete"
    );
    println!("✓ admin can delete any metric");
}

/// No auth → 401.
#[tokio::test]
async fn test_unauthenticated_denied() {
    let Some(d) = setup().await else { return };

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let status = client
        .get(format!("{}/api/v1/metrics", d.base_url()))
        .send()
        .await
        .map(|r| r.status())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    assert_eq!(status, StatusCode::UNAUTHORIZED, "no-auth should → 401");
    println!("✓ unauthenticated → 401");
}

/// Wrong token → 401.
#[tokio::test]
async fn test_wrong_token_denied() {
    let Some(d) = setup().await else { return };

    // list_metric_names returns Err when status is not 2xx
    let result = d.list_metric_names("wrong-token-xyz").await;
    assert!(
        result.is_err(),
        "wrong token should fail (got success with: {:?})",
        result
    );
    // The error message should contain 401
    let err = result.unwrap_err();
    assert!(
        err.contains("401"),
        "error should mention 401, got: {}",
        err
    );
    println!("✓ wrong token → 401");
}
