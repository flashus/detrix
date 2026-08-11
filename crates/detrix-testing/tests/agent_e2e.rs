//! Agent Mode E2E Tests
//!
//! Runs the full agent flow inside a single privileged Linux container:
//! `detrix serve` + `detrix agent start` + a Go fixture binary.
//!
//! Run via: `task test-agent`

#![cfg(target_os = "linux")]

use detrix_api::generated::detrix::v1::{
    agent_message::Msg, agent_service_client::AgentServiceClient, AgentMessage,
};
use detrix_config::constants::{AUTHORIZATION_METADATA_KEY, BEARER_PREFIX};
use detrix_testing::e2e::client::{AddMetricRequest, ApiClient, EventInfo};
use detrix_testing::e2e::dap_scenarios::go_lines;
use detrix_testing::e2e::executor::{get_grpc_port, get_http_port, wait_for_port};
use detrix_testing::e2e::mcp::parse_event_info_value;
use detrix_testing::e2e::rest::RestClient;
use detrix_testing::e2e::{
    cleanup_orphaned_e2e_processes, find_detrix_binary, get_workspace_root, register_e2e_process,
    unregister_e2e_process,
};
use futures::stream;
use serde_json::{json, Value};
use serial_test::serial;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

const AGENT_TEST_TOKEN: &str = "test-token-12345";
const FIXTURE_MATCH_GLOB: &str = "*detrix_example_app*";
const TRADING_SYMBOLS: &[&str] = &["BTCUSD", "ETHUSD", "SOLUSD"];

fn toml_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn agent_token_hash() -> String {
    format!("{:x}", Sha256::digest(AGENT_TEST_TOKEN.as_bytes()))
}

fn fixture_binary_path() -> PathBuf {
    std::env::var("AGENT_FIXTURE_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/usr/local/bin/detrix_example_app"))
}

fn fixture_source_path() -> String {
    std::env::var("AGENT_FIXTURE_SOURCE")
        .unwrap_or_else(|_| "/src/fixtures/go/string_capture/main.go".to_string())
}

fn rust_fixture_binary_path() -> PathBuf {
    std::env::var("RUST_FIXTURE_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/usr/local/bin/rust_detrix_example_app"))
}

fn rust_fixture_source_path() -> String {
    std::env::var("RUST_FIXTURE_SOURCE")
        .unwrap_or_else(|_| "/src/fixtures/rust/src/main.rs".to_string())
}

async fn poll_agent_connection(
    client: &RestClient,
    expected_host: &str,
    timeout: Duration,
) -> Option<String> {
    let start = Instant::now();
    loop {
        match client.list_connections().await {
            Ok(response) => {
                for conn in response.data {
                    if conn.host == expected_host
                        && (conn.status == "connected" || conn.status == "3")
                    {
                        return Some(conn.connection_id);
                    }
                }
            }
            Err(e) => eprintln!("Error listing connections: {e}"),
        }

        if start.elapsed() > timeout {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn poll_connection_status(
    client: &RestClient,
    connection_id: &str,
    expected_status: &str,
    timeout: Duration,
) -> Option<String> {
    let start = Instant::now();
    loop {
        match client.list_connections().await {
            Ok(response) => {
                for conn in response.data {
                    if conn.connection_id == connection_id
                        && (conn.status == expected_status
                            || (expected_status == "connected" && conn.status == "3"))
                    {
                        return Some(conn.host);
                    }
                }
            }
            Err(e) => eprintln!("Error listing connections: {e}"),
        }

        if start.elapsed() > timeout {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn poll_events(client: &RestClient, metric_name: &str, timeout: Duration) -> Vec<EventInfo> {
    poll_events_api(client, metric_name, timeout).await
}

async fn poll_events_api<C: ApiClient + Sync>(
    client: &C,
    metric_name: &str,
    timeout: Duration,
) -> Vec<EventInfo> {
    let start = Instant::now();
    loop {
        match client.query_events(metric_name, 10).await {
            Ok(response) if !response.data.is_empty() => return response.data,
            Ok(_) => {}
            Err(e) => eprintln!("Error querying events: {e}"),
        }

        if start.elapsed() > timeout {
            return Vec::new();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn event_value<'a>(event: &'a EventInfo, expression: &str) -> Option<&'a serde_json::Value> {
    event
        .values
        .iter()
        .find(|value| value["expression"] == expression)
        .map(|value| &value["valueJson"])
}

fn print_received_events(metric_name: &str, events: &[EventInfo]) {
    println!(
        "Received {} event(s) from server for metric {metric_name}:",
        events.len()
    );

    for (idx, event) in events.iter().enumerate() {
        let values = serde_json::to_string(&event.values)
            .unwrap_or_else(|_| "<failed to serialize event values>".to_string());
        println!(
            "  [{}] metricName={} timestamp={} ageSeconds={} value={} values={}",
            idx + 1,
            event.metric_name,
            event.timestamp_iso,
            event.age_seconds,
            event.value,
            values
        );
    }
}

async fn mcp_call(http_port: u16, tool_name: &str, arguments: Value) -> Value {
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{http_port}/mcp"))
        .json(&json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            },
            "id": 1
        }))
        .send()
        .await
        .expect("Failed to call MCP endpoint");

    let json: Value = response.json().await.expect("Invalid MCP JSON response");
    if let Some(error) = json.get("error") {
        panic!("MCP error: {error}");
    }
    json
}

async fn mcp_add_metric(http_port: u16, request: &AddMetricRequest) {
    let mut arguments = json!({
        "name": request.name,
        "location": request.location,
        "expressions": request.expressions,
        "connectionId": request.connection_id,
    });

    if let Some(language) = &request.language {
        arguments["language"] = json!(language);
    }
    if let Some(enabled) = request.enabled {
        arguments["enabled"] = json!(enabled);
    }

    let _ = mcp_call(http_port, "add_metric", arguments).await;
}

async fn poll_mcp_events(http_port: u16, metric_name: &str, timeout: Duration) -> Vec<EventInfo> {
    let start = Instant::now();
    loop {
        let json = mcp_call(
            http_port,
            "query_metrics",
            json!({
                "name": metric_name,
                "limit": 10,
                "format": "json",
            }),
        )
        .await;

        let events: Vec<_> = json
            .get("result")
            .and_then(|result| result.get("content"))
            .and_then(|content| content.as_array())
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
            .find_map(|text| serde_json::from_str::<Vec<Value>>(text).ok())
            .unwrap_or_default()
            .iter()
            .filter_map(parse_event_info_value)
            .collect();

        if !events.is_empty() {
            return events;
        }

        if start.elapsed() > timeout {
            return Vec::new();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

struct AgentE2eHarness {
    temp_dir: TempDir,
    workspace_root: PathBuf,
    detrix_binary: PathBuf,
    fixture_binary: PathBuf,
    fixture_source: String,
    http_port: u16,
    grpc_port: u16,
    server_process: Option<Child>,
    agent_process: Option<Child>,
    fixture_process: Option<Child>,
    rust_fixture_process: Option<Child>,
    server_log_path: PathBuf,
    agent_log_path: PathBuf,
    fixture_log_path: PathBuf,
    server_config_path: PathBuf,
    agent_config_path: PathBuf,
    agent_token_path: PathBuf,
}

impl AgentE2eHarness {
    fn new() -> Self {
        Self::new_with_fixture(fixture_binary_path(), fixture_source_path())
    }

    fn new_with_fixture(fixture_binary: PathBuf, fixture_source: String) -> Self {
        cleanup_orphaned_e2e_processes();

        let temp_dir = tempfile::TempDir::new().expect("Failed to create temp dir");
        let workspace_root = get_workspace_root();
        let detrix_binary = find_detrix_binary(&workspace_root)
            .or_else(|| std::env::var("DETRIX_BIN").ok().map(PathBuf::from))
            .expect("detrix binary not found");

        let harness = Self {
            server_log_path: temp_dir.path().join("server.log"),
            agent_log_path: temp_dir.path().join("agent.log"),
            fixture_log_path: temp_dir.path().join("fixture.log"),
            server_config_path: temp_dir.path().join("server.toml"),
            agent_config_path: temp_dir.path().join("agent.toml"),
            agent_token_path: temp_dir.path().join("agent-token"),
            temp_dir,
            workspace_root,
            detrix_binary,
            fixture_binary,
            fixture_source,
            http_port: get_http_port(),
            grpc_port: get_grpc_port(),
            server_process: None,
            agent_process: None,
            fixture_process: None,
            rust_fixture_process: None,
        };

        harness
    }

    fn fixture_host_path(&self) -> String {
        let pid = self
            .fixture_process
            .as_ref()
            .expect("fixture process not running")
            .id();
        format!("/proc/{pid}/exe")
    }

    fn print_logs(&self, last_n_lines: usize) {
        for (label, path) in [
            ("SERVER", &self.server_log_path),
            ("AGENT", &self.agent_log_path),
            ("FIXTURE", &self.fixture_log_path),
        ] {
            eprintln!("\n=== {label} LOG (last {last_n_lines} lines) ===");
            match fs::read_to_string(path) {
                Ok(content) => {
                    let lines: Vec<_> = content.lines().collect();
                    let start = lines.len().saturating_sub(last_n_lines);
                    for line in &lines[start..] {
                        eprintln!("{line}");
                    }
                }
                Err(_) => eprintln!("(could not read {})", path.display()),
            }
        }
    }

    fn write_server_config(&self, auth_enabled: bool) {
        let storage_path = toml_path(&self.temp_dir.path().join("detrix.db"));
        let log_dir = toml_path(&self.temp_dir.path().join("server-logs"));
        let workspace_root = toml_path(&self.workspace_root);
        let agent_section = if auth_enabled {
            format!(
                r#"
[agent]
agent_tokens = ["{}"]
"#,
                agent_token_hash()
            )
        } else {
            String::new()
        };
        let config = format!(
            r#"
[metadata]
version = "1.0"

[project]
base_path = "{workspace_root}"

[storage]
storage_type = "sqlite"
path = "{storage_path}"

[storage.dlq_storage]
backend = "sqlite_memory"

[daemon.logging]
log_dir = "{log_dir}"
file_logging_enabled = false

[api]
port_fallback = false

[api.rest]
enabled = true
host = "127.0.0.1"
port = {http_port}

[api.auth]
mode = "disabled"

[api.grpc]
enabled = true
host = "127.0.0.1"
port = {grpc_port}

[safety]
enable_ast_analysis = false

[ebpf]
max_capture_depth = 10
capture_goid = true
"#,
            http_port = self.http_port,
            grpc_port = self.grpc_port,
        );
        fs::write(&self.server_config_path, format!("{config}{agent_section}"))
            .expect("Failed to write server config");
    }

    fn write_agent_config(&self, token: Option<&str>) {
        let agent_id_file = toml_path(&self.temp_dir.path().join("agent-id"));
        let metrics_port = get_http_port();
        let token_line = if let Some(token) = token {
            fs::write(&self.agent_token_path, token).expect("Failed to write agent token");
            format!("token_file = \"{}\"\n", toml_path(&self.agent_token_path))
        } else {
            String::new()
        };
        let config = format!(
            r#"
[metadata]
version = "1.0"

[agent]
server_grpc_url = "http://127.0.0.1:{grpc_port}"
{token_line}
agent_id_file = "{agent_id_file}"
metrics_port = {metrics_port}
verify_tls = false

[agent.scanner]
scan_interval_secs = 1
include_patterns = ["{fixture_glob}"]
exclude_patterns = []
require_dwarf = true
allowed_read_prefixes = ["/src"]
"#,
            grpc_port = self.grpc_port,
            token_line = token_line,
            fixture_glob = FIXTURE_MATCH_GLOB,
        );
        fs::write(&self.agent_config_path, config).expect("Failed to write agent config");
    }

    async fn start_server_with_auth(&mut self, auth_enabled: bool) {
        self.write_server_config(auth_enabled);

        let stdout = fs::File::create(&self.server_log_path).expect("Failed to create server log");
        let stderr = stdout.try_clone().expect("Failed to clone server log");

        let process = Command::new(&self.detrix_binary)
            .args([
                "serve",
                "--config",
                self.server_config_path.to_str().unwrap(),
            ])
            .current_dir(&self.workspace_root)
            .env("DETRIX_HOME", self.temp_dir.path())
            .env("DETRIX_AGENT_RUST_EBPF", std::env::var("DETRIX_AGENT_RUST_EBPF").unwrap_or_default())
            .env(
                "RUST_LOG",
                "detrix=debug,detrix_application=debug,detrix_api=debug,detrix_agent=debug,detrix_ebpf=debug,info",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("Failed to spawn detrix serve");

        register_e2e_process("agent_server", process.id());
        self.server_process = Some(process);

        assert!(
            wait_for_port(self.http_port, 30).await,
            "Server HTTP not listening on {}",
            self.http_port
        );
        assert!(
            wait_for_port(self.grpc_port, 30).await,
            "Server gRPC not listening on {}",
            self.grpc_port
        );
    }

    async fn start_fixture(&mut self) {
        let stdout =
            fs::File::create(&self.fixture_log_path).expect("Failed to create fixture log");
        let stderr = stdout.try_clone().expect("Failed to clone fixture log");

        let process = Command::new(&self.fixture_binary)
            .env("DETRIX_EBPF_WORKERS", "1")
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("Failed to start Go fixture");

        register_e2e_process("agent_fixture", process.id());
        self.fixture_process = Some(process);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    fn rust_fixture_host_path(&self) -> String {
        let pid = self
            .rust_fixture_process
            .as_ref()
            .expect("Rust fixture process not running")
            .id();
        format!("/proc/{pid}/exe")
    }

    async fn start_rust_fixture(&mut self) {
        let stdout = fs::File::create(self.temp_dir.path().join("rust-fixture.log"))
            .expect("Failed to create Rust fixture log");
        let stderr = stdout
            .try_clone()
            .expect("Failed to clone Rust fixture log");
        let process = Command::new(rust_fixture_binary_path())
            .env("DETRIX_EBPF_WORKERS", "1")
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("Failed to start Rust fixture");
        register_e2e_process("agent_rust_fixture", process.id());
        self.rust_fixture_process = Some(process);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    async fn start_agent(&mut self, token: Option<&str>) {
        self.write_agent_config(token);

        let stdout = fs::File::create(&self.agent_log_path).expect("Failed to create agent log");
        let stderr = stdout.try_clone().expect("Failed to clone agent log");

        let process = Command::new(&self.detrix_binary)
            .args([
                "agent",
                "start",
                "--config",
                self.agent_config_path.to_str().unwrap(),
            ])
            .current_dir(&self.workspace_root)
            .env("DETRIX_HOME", self.temp_dir.path())
            .env(
                "RUST_LOG",
                "detrix_agent=debug,detrix_application=debug,detrix_api=debug,detrix_ebpf=debug,info",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("Failed to spawn detrix agent");

        register_e2e_process("agent_runner", process.id());
        self.agent_process = Some(process);
    }

    async fn start_stack(&mut self) {
        self.start_server_with_auth(true).await;
        self.start_fixture().await;
        if std::env::var("AGENT_FIXTURES").as_deref() == Ok("both") {
            self.start_rust_fixture().await;
        }
        self.start_agent(Some(AGENT_TEST_TOKEN)).await;
    }

    fn stop_named_process(name: &str, process: &mut Option<Child>) {
        if let Some(mut child) = process.take() {
            let pid = child.id();
            let _ = child.kill();
            let _ = child.wait();
            unregister_e2e_process(name, pid);
        }
    }

    async fn replace_agent(&mut self) {
        Self::stop_named_process("agent_runner", &mut self.agent_process);
        fs::remove_file(self.temp_dir.path().join("agent-id"))
            .expect("Failed to remove persisted agent ID");
        self.start_agent(Some(AGENT_TEST_TOKEN)).await;
    }

    async fn restart_fixture(&mut self) {
        Self::stop_named_process("agent_fixture", &mut self.fixture_process);
        Self::stop_named_process("agent_rust_fixture", &mut self.rust_fixture_process);
        self.start_fixture().await;
    }
}

impl Drop for AgentE2eHarness {
    fn drop(&mut self) {
        Self::stop_named_process("agent_runner", &mut self.agent_process);
        Self::stop_named_process("agent_fixture", &mut self.fixture_process);
        Self::stop_named_process("agent_server", &mut self.server_process);
    }
}

#[tokio::test]
#[ignore = "requires Linux + Docker privileged runner; use `task test-agent`"]
#[serial(agent_e2e)]
async fn test_agent_go_ebpf_basic() {
    let mut harness = AgentE2eHarness::new();
    harness.start_stack().await;

    let client = RestClient::new(harness.http_port);
    let fixture_host = harness.fixture_host_path();

    let connection_id =
        match poll_agent_connection(&client, &fixture_host, Duration::from_secs(30)).await {
            Some(id) => id,
            None => {
                harness.print_logs(120);
                panic!("Expected Go connection for fixture host {fixture_host}");
            }
        };

    let location = format!(
        "@{}#{}",
        harness.fixture_source,
        go_lines::CODEMAP.find_logpoint("quantity")
    );
    let metric_name = "agent-basic-symbol-qty";
    let metric_id = client
        .add_metric(AddMetricRequest {
            name: metric_name.to_string(),
            location,
            expressions: vec!["symbol".to_string(), "quantity".to_string()],
            connection_id: connection_id.clone(),
            language: Some("go".to_string()),
            group: None,
            mode: None,
            enabled: Some(true),
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            capture_stack_trace: None,
            stack_trace_ttl: None,
            stack_trace_full: None,
            stack_trace_head: None,
            stack_trace_tail: None,
            capture_memory_snapshot: None,
            snapshot_scope: None,
            snapshot_ttl: None,
        })
        .await
        .expect("Failed to create metric")
        .data;
    println!("Created metric: {metric_name} (id: {metric_id})");

    let events = poll_events(&client, metric_name, Duration::from_secs(20)).await;
    if events.is_empty() {
        harness.print_logs(120);
    } else {
        print_received_events(metric_name, &events);
    }
    assert!(
        !events.is_empty(),
        "Expected events from metric {metric_name}"
    );

    for event in &events {
        let symbol = event_value(event, "symbol")
            .and_then(|value| value.as_str())
            .map(|value| value.trim_matches('"').to_string())
            .expect("Expected captured symbol value");
        assert!(
            TRADING_SYMBOLS.iter().any(|candidate| *candidate == symbol),
            "Unexpected symbol value: {symbol}"
        );

        let quantity = event_value(event, "quantity")
            .and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str()?.parse::<i64>().ok())
            })
            .expect("Expected captured quantity value");
        assert!(quantity > 0, "Expected positive quantity, got {quantity}");
    }
}

#[tokio::test]
#[ignore = "requires Linux + Docker privileged runner; use `task test-agent-rust`"]
#[serial(agent_e2e)]
async fn test_agent_rust_ebpf_basic() {
    let mut harness =
        AgentE2eHarness::new_with_fixture(rust_fixture_binary_path(), rust_fixture_source_path());
    harness.start_stack().await;

    let client = RestClient::new(harness.http_port);
    let fixture_host = harness.fixture_host_path();
    let connection_id = poll_agent_connection(&client, &fixture_host, Duration::from_secs(30))
        .await
        .unwrap_or_else(|| {
            harness.print_logs(160);
            panic!("Expected Rust connection for fixture host {fixture_host}");
        });

    let metric_name = "agent-rust-ebpf-quantity-price";
    // Observe after all requested locals have been initialized and are live.
    // At line 109 `price` has not been declared yet, so a two-variable request
    // is correctly rejected by DWARF preflight as an incomplete location.
    let location = format!("@{}#114", harness.fixture_source);
    let metric_id = client
        .add_metric(AddMetricRequest {
            name: metric_name.to_string(),
            location,
            expressions: vec!["quantity".to_string(), "price".to_string()],
            connection_id,
            language: Some("rust".to_string()),
            group: None,
            mode: None,
            enabled: Some(true),
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            capture_stack_trace: None,
            stack_trace_ttl: None,
            stack_trace_full: None,
            stack_trace_head: None,
            stack_trace_tail: None,
            capture_memory_snapshot: None,
            snapshot_scope: None,
            snapshot_ttl: None,
        })
        .await
        .expect("Failed to create Rust eBPF metric")
        .data;
    println!("Created Rust eBPF metric: {metric_name} (id: {metric_id})");

    let events = poll_events(&client, metric_name, Duration::from_secs(25)).await;
    if events.is_empty() {
        harness.print_logs(160);
    } else {
        print_received_events(metric_name, &events);
    }
    assert!(
        !events.is_empty(),
        "Expected Rust eBPF events from {metric_name}"
    );

    for event in &events {
        let quantity = event_value(event, "quantity")
            .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
            .expect("Expected captured Rust quantity");
        if !(1..=50).contains(&quantity) {
            harness.print_logs(160);
            panic!("Expected Rust quantity in fixture range 1..=50, got {quantity}");
        }
        let price = event_value(event, "price")
            .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
            .expect("Expected captured Rust price");
        if !(100.0..=1000.0).contains(&price) {
            harness.print_logs(160);
            panic!("Expected Rust price in fixture range 100..=1000, got {price}");
        }
    }
}

#[tokio::test]
#[ignore = "requires Linux + Docker privileged runner; use `task test-agent-both`"]
#[serial(agent_e2e)]
async fn test_agent_go_and_rust_ebpf_same_agent() {
    assert_eq!(std::env::var("AGENT_FIXTURES").as_deref(), Ok("both"));
    let mut harness = AgentE2eHarness::new();
    harness.start_stack().await;
    let client = RestClient::new(harness.http_port);

    let go_host = harness.fixture_host_path();
    let rust_host = harness.rust_fixture_host_path();
    let go_connection = poll_agent_connection(&client, &go_host, Duration::from_secs(30))
        .await
        .expect("Go connection was not registered");
    let rust_connection = poll_agent_connection(&client, &rust_host, Duration::from_secs(30))
        .await
        .expect("Rust connection was not registered");

    let go_metric = client
        .add_metric(AddMetricRequest {
            name: "agent-both-go-quantity".to_string(),
            location: format!(
                "@{}#{}",
                harness.fixture_source,
                go_lines::CODEMAP.find_logpoint("quantity")
            ),
            expressions: vec!["quantity".to_string()],
            connection_id: go_connection,
            language: Some("go".to_string()),
            group: None,
            mode: None,
            enabled: Some(true),
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            capture_stack_trace: None,
            stack_trace_ttl: None,
            stack_trace_full: None,
            stack_trace_head: None,
            stack_trace_tail: None,
            capture_memory_snapshot: None,
            snapshot_scope: None,
            snapshot_ttl: None,
        })
        .await
        .expect("Failed to create Go metric")
        .data;
    let rust_metric = client
        .add_metric(AddMetricRequest {
            name: "agent-both-rust-quantity".to_string(),
            // Both Rust scalars are live at the call site (line 114).
            location: format!("@{}#114", rust_fixture_source_path()),
            expressions: vec!["quantity".to_string()],
            connection_id: rust_connection,
            language: Some("rust".to_string()),
            group: None,
            mode: None,
            enabled: Some(true),
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            capture_stack_trace: None,
            stack_trace_ttl: None,
            stack_trace_full: None,
            stack_trace_head: None,
            stack_trace_tail: None,
            capture_memory_snapshot: None,
            snapshot_scope: None,
            snapshot_ttl: None,
        })
        .await
        .expect("Failed to create Rust metric")
        .data;
    println!("Created heterogeneous metrics: Go={go_metric}, Rust={rust_metric}");

    let go_events = poll_events(&client, "agent-both-go-quantity", Duration::from_secs(25)).await;
    let rust_events =
        poll_events(&client, "agent-both-rust-quantity", Duration::from_secs(25)).await;
    assert!(
        !go_events.is_empty(),
        "Expected Go events from shared agent"
    );
    assert!(
        !rust_events.is_empty(),
        "Expected Rust events from shared agent"
    );
}

#[tokio::test]
#[ignore = "requires Linux + Docker privileged runner; use `task test-agent`"]
#[serial(agent_e2e)]
async fn test_agent_reconnect() {
    let mut harness = AgentE2eHarness::new();
    harness.start_stack().await;

    let client = RestClient::new(harness.http_port);
    let fixture_host = harness.fixture_host_path();

    let connection_id = poll_agent_connection(&client, &fixture_host, Duration::from_secs(30))
        .await
        .expect("Expected Go connection from agent");

    let metric_name = "agent-reconnect-symbol-qty";
    client
        .add_metric(AddMetricRequest {
            name: metric_name.to_string(),
            location: format!(
                "@{}#{}",
                harness.fixture_source,
                go_lines::CODEMAP.find_logpoint("quantity")
            ),
            expressions: vec!["symbol".to_string(), "quantity".to_string()],
            connection_id: connection_id.clone(),
            language: Some("go".to_string()),
            group: None,
            mode: None,
            enabled: Some(true),
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            capture_stack_trace: None,
            stack_trace_ttl: None,
            stack_trace_full: None,
            stack_trace_head: None,
            stack_trace_tail: None,
            capture_memory_snapshot: None,
            snapshot_scope: None,
            snapshot_ttl: None,
        })
        .await
        .expect("Failed to create metric");

    harness.replace_agent().await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    let new_connection_id =
        match poll_agent_connection(&client, &fixture_host, Duration::from_secs(30)).await {
            Some(id) => id,
            None => {
                harness.print_logs(120);
                panic!("Expected Go connection after agent restart");
            }
        };

    assert_eq!(
        connection_id, new_connection_id,
        "Connection ID should remain the same after agent restart"
    );

    let events = poll_events(&client, metric_name, Duration::from_secs(20)).await;
    if events.is_empty() {
        harness.print_logs(120);
    } else {
        print_received_events(metric_name, &events);
    }
    assert!(
        !events.is_empty(),
        "Expected events from restored metric {metric_name}"
    );
}

#[tokio::test]
#[ignore = "requires Linux + Docker privileged runner; use `task test-agent`"]
#[serial(agent_e2e)]
async fn test_agent_mcp_roundtrip() {
    let mut harness = AgentE2eHarness::new();
    harness.start_stack().await;

    let rest_client = RestClient::new(harness.http_port);
    let fixture_host = harness.fixture_host_path();

    let connection_id =
        match poll_agent_connection(&rest_client, &fixture_host, Duration::from_secs(30)).await {
            Some(id) => id,
            None => {
                harness.print_logs(120);
                panic!("Expected Go connection for fixture host {fixture_host}");
            }
        };

    let metric_name = "agent-mcp-symbol-qty";
    mcp_add_metric(
        harness.http_port,
        &AddMetricRequest {
            name: metric_name.to_string(),
            location: format!(
                "@{}#{}",
                harness.fixture_source,
                go_lines::CODEMAP.find_logpoint("quantity")
            ),
            expressions: vec!["symbol".to_string(), "quantity".to_string()],
            connection_id,
            language: Some("go".to_string()),
            group: None,
            mode: None,
            enabled: Some(true),
            sample_rate: None,
            sample_interval_seconds: None,
            max_per_second: None,
            capture_stack_trace: None,
            stack_trace_ttl: None,
            stack_trace_full: None,
            stack_trace_head: None,
            stack_trace_tail: None,
            capture_memory_snapshot: None,
            snapshot_scope: None,
            snapshot_ttl: None,
        },
    )
    .await;
    println!("Created MCP metric: {metric_name}");

    let events = poll_mcp_events(harness.http_port, metric_name, Duration::from_secs(20)).await;
    if events.is_empty() {
        harness.print_logs(120);
    } else {
        print_received_events(metric_name, &events);
    }
    assert!(
        !events.is_empty(),
        "Expected MCP events from metric {metric_name}"
    );

    for event in &events {
        let symbol = event_value(event, "symbol")
            .and_then(|value| value.as_str())
            .map(|value| value.trim_matches('"').to_string())
            .expect("Expected captured symbol value via MCP");
        assert!(
            TRADING_SYMBOLS.iter().any(|candidate| *candidate == symbol),
            "Unexpected symbol value via MCP: {symbol}"
        );

        let quantity = event_value(event, "quantity")
            .and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str()?.parse::<i64>().ok())
            })
            .expect("Expected captured quantity value via MCP");
        assert!(
            quantity > 0,
            "Expected positive quantity via MCP, got {quantity}"
        );
    }
}

#[tokio::test]
#[ignore = "requires Linux + Docker privileged runner; use `task test-agent`"]
#[serial(agent_e2e)]
async fn test_agent_wrong_token() {
    let mut harness = AgentE2eHarness::new();
    harness.start_server_with_auth(true).await;

    let endpoint = format!("http://127.0.0.1:{}", harness.grpc_port);
    let channel = Channel::from_shared(endpoint)
        .expect("Invalid gRPC endpoint")
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .expect("Failed to connect to gRPC server");

    let mut client = AgentServiceClient::new(channel);
    let register_msg = AgentMessage {
        msg: Some(Msg::Register(
            detrix_api::generated::detrix::v1::RegisterAgent {
                agent_id: "test-agent-wrong-token".to_string(),
                hostname: "localhost".to_string(),
                agent_version: "1.0.0".to_string(),
                capabilities: Some(detrix_api::generated::detrix::v1::AgentCapabilities {
                    ebpf: true,
                    dap_python: false,
                    dap_go: false,
                    dap_rust: false,
                }),
                binaries: vec![],
            },
        )),
    };

    let stream = stream::iter(vec![register_msg]);
    let mut request = Request::new(stream);
    let token = format!("{BEARER_PREFIX}wrong-token-secret")
        .parse()
        .unwrap();
    request
        .metadata_mut()
        .insert(AUTHORIZATION_METADATA_KEY, token);

    let result: Result<
        Response<tonic::codec::Streaming<detrix_api::generated::detrix::v1::ServerMessage>>,
        Status,
    > = client.connect_agent(request).await;

    match result {
        Err(status) => {
            if status.code() != tonic::Code::Unauthenticated {
                harness.print_logs(120);
            }
            assert_eq!(status.code(), tonic::Code::Unauthenticated);
            println!(
                "Correctly rejected with Unauthenticated: {}",
                status.message()
            );
        }
        Ok(_) => panic!("Expected Unauthenticated error, but connection succeeded"),
    }
}

#[tokio::test]
#[ignore = "requires Linux + Docker privileged runner; use `task test-agent`"]
#[serial(agent_e2e)]
async fn test_agent_scanner_update() {
    let mut harness = AgentE2eHarness::new();
    harness.start_stack().await;

    let client = RestClient::new(harness.http_port);
    let initial_host = harness.fixture_host_path();
    let initial_conn = poll_agent_connection(&client, &initial_host, Duration::from_secs(30))
        .await
        .expect("Expected initial Go connection");

    harness.restart_fixture().await;
    let observed_host =
        match poll_connection_status(&client, &initial_conn, "connected", Duration::from_secs(30))
            .await
        {
            Some(host) => host,
            None => {
                harness.print_logs(120);
                panic!(
                "Expected connection {initial_conn} to return to connected after fixture restart"
            );
            }
        };

    assert!(
        observed_host.starts_with("/proc/"),
        "Expected agent-managed procfs host after restart, got {observed_host}"
    );

    harness.restart_fixture().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    harness.restart_fixture().await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let connections = client
        .list_connections()
        .await
        .expect("Failed to list connections")
        .data;
    let go_connections: Vec<_> = connections
        .into_iter()
        .filter(|c| c.language == "go" && (c.status == "connected" || c.status == "3"))
        .collect();
    assert_eq!(
        go_connections.len(),
        1,
        "Expected exactly 1 connected agent-managed connection after rapid restarts, found {}",
        go_connections.len()
    );
}
