//! Shared MCP bridge subprocess client for E2E tests.
//!
//! Spawns `detrix mcp --daemon-url <url>` as a child process and communicates
//! via JSON-RPC over stdin/stdout.  Used by Keycloak and Docker Cloud E2E tests.

use crate::e2e::executor::{find_detrix_binary, get_workspace_root};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::Instant;

/// A running `detrix mcp` subprocess, communicating via JSON-RPC over stdio.
pub struct McpBridgeProcess {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

impl McpBridgeProcess {
    /// Spawn a bridge process pointing at the given daemon URL.
    pub async fn spawn(daemon_url: &str, token: &str) -> Self {
        Self::spawn_inner(daemon_url, token, None, None).await
    }

    /// Spawn with a working directory override.
    pub async fn spawn_in_dir(daemon_url: &str, token: &str, cwd: &Path) -> Self {
        Self::spawn_inner(daemon_url, token, None, Some(cwd)).await
    }

    /// Spawn with an optional file-server host (for Docker cloud tests).
    pub async fn spawn_with_file_server(
        daemon_url: &str,
        token: &str,
        file_server_host: &str,
    ) -> Self {
        Self::spawn_inner(daemon_url, token, Some(file_server_host), None).await
    }

    /// Spawn with both a working directory and a file-server host.
    pub async fn spawn_in_dir_with_file_server(
        daemon_url: &str,
        token: &str,
        file_server_host: &str,
        cwd: &Path,
    ) -> Self {
        Self::spawn_inner(daemon_url, token, Some(file_server_host), Some(cwd)).await
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

        let mut cmd = tokio::process::Command::new(&detrix_bin);
        cmd.args(["mcp", "--daemon-url", daemon_url])
            .env("DETRIX_TOKEN", token)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(host) = file_server_host {
            cmd.env("DETRIX_FILE_SERVER_HOST", host);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().expect("spawn detrix mcp");

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

    /// Perform MCP protocol handshake.
    async fn initialize(&mut self) {
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

        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        self.write_message(&notif).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    /// Send a JSON-RPC message to the bridge.
    pub async fn write_message(&mut self, msg: &Value) {
        let line = format!("{}\n", msg);
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    /// Read the next JSON-RPC response (with id), skipping notifications.
    pub async fn read_response(&mut self, timeout_secs: u64) -> Value {
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

    /// Call an MCP tool and return its result.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
            "id": id
        });
        self.write_message(&request).await;
        let resp = self.read_response(60).await;
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

    /// List metrics via MCP and return metric names.
    pub async fn list_metric_names(&mut self) -> Result<Vec<String>, String> {
        let result = self
            .call_tool("list_metrics", json!({"format": "json"}))
            .await?;
        let text = extract_text(&result);
        let start = text
            .find('[')
            .ok_or_else(|| format!("No JSON array found in list_metrics output: {text}"))?;
        let arr: Vec<Value> = serde_json::from_str(&text[start..])
            .map_err(|e| format!("Failed to parse list_metrics JSON: {e}"))?;
        Ok(arr
            .iter()
            .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect())
    }

    /// Kill the subprocess.
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

/// Extract concatenated text content from an MCP tool result.
pub fn extract_text(result: &Value) -> String {
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
