//! Keycloak MCP Bridge Per-User Scoping E2E Tests — Scenario 3
//!
//! Full cloud-like E2E: Keycloak + Detrix daemon + Go test app + MCP bridge.
//! Validates that MCP bridge processes inherit JWT-based user scoping:
//! - Metrics created via REST with user-specific JWTs
//! - MCP bridge spawned with `DETRIX_TOKEN=<jwt>` sees only that user's metrics
//! - Admin MCP bridge sees all metrics
//!
//! Run via: `task test-cloud-keycloak`

mod keycloak_helpers;

use detrix_testing::e2e::{find_detrix_binary, get_workspace_root};
use keycloak_helpers::*;
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// ─── MCP Bridge wrapper ─────────────────────────────────────────────────────

/// Minimal MCP bridge process for Keycloak tests.
/// Replicates the BridgeProcess pattern from docker_cloud_e2e.rs.
struct McpBridge {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

impl McpBridge {
    async fn spawn(daemon_url: &str, token: &str) -> Self {
        let ws_root = get_workspace_root();
        let detrix_bin = find_detrix_binary(&ws_root)
            .expect("detrix binary not found — run `cargo build` first");

        let mut child = tokio::process::Command::new(&detrix_bin)
            .args(["mcp", "--daemon-url", daemon_url])
            .env("DETRIX_TOKEN", token)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn detrix mcp");

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);

        let mut bridge = Self {
            child,
            stdin,
            reader,
            next_id: 1,
        };
        bridge.initialize().await;
        bridge
    }

    async fn initialize(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        let init_req = json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "keycloak-e2e", "version": "1.0" }
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

        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        self.write_message(&notif).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    async fn write_message(&mut self, msg: &Value) {
        let line = format!("{}\n", msg);
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

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
            assert!(n > 0, "EOF from bridge");
            if let Ok(parsed) = serde_json::from_str::<Value>(line.trim()) {
                if parsed.get("id").is_some_and(|v| !v.is_null()) {
                    return parsed;
                }
            }
        }
    }

    async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
            "id": id
        });
        self.write_message(&request).await;
        let resp = self.read_response(30).await;
        if let Some(error) = resp.get("error") {
            return Err(format!("JSON-RPC error: {}", error));
        }
        let result = resp.get("result").cloned().ok_or("no result")?;
        if result.get("isError") == Some(&Value::Bool(true)) {
            let text = extract_text(&result);
            return Err(format!("Tool error: {}", text));
        }
        Ok(result)
    }

    /// List metrics via MCP tool and return parsed metric names.
    async fn list_metric_names(&mut self) -> Vec<String> {
        if let Ok(result) = self.call_tool("list_metrics", json!({"format": "json"})).await {
            let text = extract_text(&result);
            if let Some(start) = text.find('[') {
                if let Ok(arr) = serde_json::from_str::<Vec<Value>>(&text[start..]) {
                    return arr
                        .iter()
                        .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
                        .collect();
                }
            }
        }
        vec![]
    }

    async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

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

// ─── Tests ──────────────────────────────────────────────────────────────────

/// MCP bridge per-user scoping: each bridge sees only its user's metrics.
///
/// Uses real Go app connection for metric creation, then validates
/// MCP bridge isolation with per-user JWT tokens.
#[tokio::test]
#[ignore]
async fn test_keycloak_mcp_bridge_per_user_scoping() {
    let kc = KeycloakTokenClient::new();

    // Get admin token for wake + connection setup
    let admin_token = kc.get_token(ADMIN_USER, ADMIN_PASS).await;
    let daemon_url = format!("http://127.0.0.1:{}", DETRIX_HTTP_PORT);
    let admin_rest = DetrixClient::new(&admin_token.access_token);

    // Wake Go app and get connection via admin
    let (status, _) = admin_rest.wake(GO_APP_URL).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "wake go app failed: {}",
        status
    );

    let go_conn = admin_rest
        .poll_for_connection("go", Duration::from_secs(30))
        .await
        .expect("Go connection not found within 30s");
    println!("  Go connection: {}", go_conn);

    // Fresh tokens after wake/poll (30s TTL may have expired)
    let alice_token = kc.get_token(ALICE_USER, ALICE_PASS).await;
    let bob_token = kc.get_token(BOB_USER, BOB_PASS).await;
    let admin_token = kc.get_token(ADMIN_USER, ADMIN_PASS).await;
    let admin_rest = DetrixClient::new(&admin_token.access_token);
    let alice_rest = DetrixClient::new(&alice_token.access_token);
    let bob_rest = DetrixClient::new(&bob_token.access_token);

    // Clean up pre-existing metrics
    let (_, existing) = admin_rest.list_metrics().await;
    for m in &existing {
        let _ = admin_rest.delete_metric_by_name(&m.name).await;
    }

    // Step 1: Create metrics via REST with user-specific tokens
    let (status, _) = alice_rest
        .create_metric(&CreateMetricBody {
            name: "mcp-alice-metric".to_string(),
            connection_id: go_conn.clone(),
            location: LocationBody {
                file: GO_FILE.to_string(),
                line: GO_LOGPOINT_LINE,
            },
            expressions: vec!["symbol".to_string()],
            enabled: false,
        })
        .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "alice create metric failed: {}",
        status
    );

    let (status, _) = bob_rest
        .create_metric(&CreateMetricBody {
            name: "mcp-bob-metric".to_string(),
            connection_id: go_conn.clone(),
            location: LocationBody {
                file: GO_FILE.to_string(),
                line: GO_LOGPOINT_LINE + 1,
            },
            expressions: vec!["quantity".to_string()],
            enabled: false,
        })
        .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "bob create metric failed: {}",
        status
    );
    println!("  Setup: created metrics for alice and bob via REST");

    // Step 2: Spawn MCP bridges with per-user JWT tokens
    let mut alice_bridge = McpBridge::spawn(&daemon_url, &alice_token.access_token).await;
    println!("  Alice MCP bridge spawned");

    let mut bob_bridge = McpBridge::spawn(&daemon_url, &bob_token.access_token).await;
    println!("  Bob MCP bridge spawned");

    let mut admin_bridge = McpBridge::spawn(&daemon_url, &admin_token.access_token).await;
    println!("  Admin MCP bridge spawned");

    // Step 3: Alice's bridge sees only her metrics
    let alice_names = alice_bridge.list_metric_names().await;
    assert!(
        alice_names.contains(&"mcp-alice-metric".to_string()),
        "alice bridge should see mcp-alice-metric, got: {:?}",
        alice_names
    );
    assert!(
        !alice_names.contains(&"mcp-bob-metric".to_string()),
        "alice bridge should NOT see bob's metric, got: {:?}",
        alice_names
    );
    println!("  Alice bridge: sees only her metric ✓");

    // Step 4: Bob's bridge sees only his metrics
    let bob_names = bob_bridge.list_metric_names().await;
    assert!(
        bob_names.contains(&"mcp-bob-metric".to_string()),
        "bob bridge should see mcp-bob-metric, got: {:?}",
        bob_names
    );
    assert!(
        !bob_names.contains(&"mcp-alice-metric".to_string()),
        "bob bridge should NOT see alice's metric, got: {:?}",
        bob_names
    );
    println!("  Bob bridge: sees only his metric ✓");

    // Step 5: Admin bridge sees all metrics
    let admin_names = admin_bridge.list_metric_names().await;
    assert!(
        admin_names.contains(&"mcp-alice-metric".to_string())
            && admin_names.contains(&"mcp-bob-metric".to_string()),
        "admin bridge should see all metrics, got: {:?}",
        admin_names
    );
    println!("  Admin bridge: sees all metrics ✓");

    // Cleanup
    alice_bridge.kill().await;
    bob_bridge.kill().await;
    admin_bridge.kill().await;
    let _ = alice_rest.delete_metric_by_name("mcp-alice-metric").await;
    let _ = bob_rest.delete_metric_by_name("mcp-bob-metric").await;

    println!("✓ MCP bridge per-user scoping test passed");
}
