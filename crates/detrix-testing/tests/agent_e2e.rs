//! Agent Mode E2E Tests
//!
//! Tests agent mode functionality including eBPF observation, reconnection,
//! authentication, and scanner updates.
//!
//! Run via: `task test-agent` (manages Docker Compose lifecycle)
//! Or manually:
//! ```sh
//! docker compose -f fixtures/docker/docker-compose.agent-e2e.yml -p detrix-agent-e2e-test up -d --build --wait
//! cargo test -p detrix-testing --test agent_e2e -- --ignored --nocapture --test-threads=1
//! docker compose -f fixtures/docker/docker-compose.agent-e2e.yml -p detrix-agent-e2e-test down -v
//! ```
//!
//! NOTE: These tests require:
//! - Linux OS (for eBPF)
//! - Docker with privileged mode (--cap-add=ALL --privileged)
//! - Detrix agent binaries built

use detrix_api::generated::detrix::v1::{
    agent_message::Msg, agent_service_client::AgentServiceClient, AgentMessage,
};
use detrix_config::constants::{AUTHORIZATION_METADATA_KEY, BEARER_PREFIX};
use detrix_testing::e2e::client::{AddMetricRequest, ApiClient, EventInfo};
use detrix_testing::e2e::rest::RestClient;
use futures::stream;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

// =============================================================================
// Constants
// =============================================================================

const AGENT_COMPOSE_FILE: &str = "fixtures/docker/docker-compose.agent-e2e.yml";
const AGENT_COMPOSE_PROJECT: &str = "detrix-agent-e2e-test";
const AGENT_SERVER_HTTP_PORT: u16 = 8096;
const AGENT_SERVER_GRPC_PORT: u16 = 50066;

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
        .join(AGENT_COMPOSE_FILE);
    ws.to_string_lossy().into_owned()
}

/// Run docker compose command with agent-e2e configuration.
fn compose_agent(args: &[&str]) -> std::process::Output {
    let compose_file = compose_file_abs();
    std::process::Command::new("docker")
        .args(["compose", "-f", &compose_file, "-p", AGENT_COMPOSE_PROJECT])
        .args(args)
        .output()
        .expect("docker compose command failed")
}

/// Ensure compose services are running; if not, start them.
fn ensure_services_running() {
    let output = compose_agent(&["ps", "-q"]);
    if String::from_utf8_lossy(&output.stdout).trim().is_empty() {
        println!("Starting agent e2e services...");
        let up_output = compose_agent(&["up", "-d", "--build"]);
        if !up_output.status.success() {
            panic!(
                "Failed to start services: {}",
                String::from_utf8_lossy(&up_output.stderr)
            );
        }
        // Wait for services to be healthy
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// Poll for agent connection by language until timeout.
/// Returns connection_id if found, None if timeout.
async fn poll_agent_connection(
    client: &RestClient,
    language: &str,
    timeout: Duration,
) -> Option<String> {
    let start = Instant::now();
    loop {
        match client.list_connections().await {
            Ok(response) => {
                for conn in response.data {
                    if conn.language == language
                        && (conn.status == "connected" || conn.status == "3")
                    {
                        return Some(conn.connection_id);
                    }
                }
            }
            Err(e) => {
                eprintln!("Error listing connections: {e}");
            }
        }

        if start.elapsed() > timeout {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Poll for events on a metric until timeout.
/// Returns vector of events if any found, empty vector if timeout.
async fn poll_events(client: &RestClient, metric_name: &str, timeout: Duration) -> Vec<EventInfo> {
    let start = Instant::now();
    loop {
        match client.query_events(metric_name, 10).await {
            Ok(response) => {
                if !response.data.is_empty() {
                    return response.data;
                }
            }
            Err(e) => {
                eprintln!("Error querying events: {e}");
            }
        }

        if start.elapsed() > timeout {
            return Vec::new();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// =============================================================================
// Tests
// =============================================================================

/// Test basic agent + eBPF observation flow.
///
/// 1. Ensure services are running
/// 2. Create REST client
/// 3. Poll for Go connection (30s timeout)
/// 4. Create metric at main.go:N with expression "n"
/// 5. Poll for events (20s timeout)
/// 6. Assert event values contain valid integer "n"
#[tokio::test]
#[ignore]
async fn test_agent_go_ebpf_basic() {
    ensure_services_running();

    let client = RestClient::new(AGENT_SERVER_HTTP_PORT);

    // Poll for agent connection
    let connection_id = poll_agent_connection(&client, "go", Duration::from_secs(30))
        .await
        .expect("Expected Go connection from agent");

    println!("Found connection: {}", connection_id);

    // Create metric
    let metric_name = "agent-basic-n";
    let request = AddMetricRequest {
        name: metric_name.to_string(),
        location: "main.go:24".to_string(), // Line where `n` is defined in go-bare fixture
        expressions: vec!["n".to_string()],
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
    };

    let metric_id = client
        .add_metric(request)
        .await
        .expect("Failed to create metric")
        .data;
    println!("Created metric: {} (id: {})", metric_name, metric_id);

    // Poll for events
    let events = poll_events(&client, metric_name, Duration::from_secs(20)).await;
    assert!(
        !events.is_empty(),
        "Expected events from metric {}",
        metric_name
    );

    // Validate event values
    for event in &events {
        let values = &event.values;
        if !values.is_empty() {
            let n_value = &values[0];
            if let Some(n) = n_value.as_i64() {
                assert!(n >= 0, "Expected non-negative integer for 'n', got {}", n);
                println!("Event: n = {}", n);
            } else if let Some(n) = n_value.as_u64() {
                println!("Event: n = {}", n);
            } else if let Some(n) = n_value.as_str() {
                // Try to parse as integer
                n.parse::<i64>()
                    .unwrap_or_else(|_| panic!("Expected integer string for 'n', got {}", n));
                println!("Event: n = {}", n);
            } else {
                panic!("Expected integer value for 'n', got {:?}", n_value);
            }
        }
    }
}

/// Test agent reconnection after restart.
///
/// 1. Ensure services are running
/// 2. Get connection_id via REST
/// 3. Add metric M
/// 4. Restart agent container
/// 5. Poll for same connection_id (should be deterministic UUID)
/// 6. Poll for events on M (metrics auto-restored)
#[tokio::test]
#[ignore]
async fn test_agent_reconnect() {
    ensure_services_running();

    let client = RestClient::new(AGENT_SERVER_HTTP_PORT);

    // Poll for agent connection
    let connection_id = poll_agent_connection(&client, "go", Duration::from_secs(30))
        .await
        .expect("Expected Go connection from agent");

    println!("Initial connection: {}", connection_id);

    // Create metric
    let metric_name = "agent-reconnect-n";
    let request = AddMetricRequest {
        name: metric_name.to_string(),
        location: "main.go:24".to_string(),
        expressions: vec!["n".to_string()],
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
    };

    client
        .add_metric(request)
        .await
        .expect("Failed to create metric");
    println!("Created metric: {}", metric_name);

    // Restart agent container
    println!("Restarting agent container...");
    let restart_output = compose_agent(&["restart", "detrix-agent-process"]);
    assert!(
        restart_output.status.success(),
        "Failed to restart agent: {}",
        String::from_utf8_lossy(&restart_output.stderr)
    );

    // Wait for agent to reconnect
    std::thread::sleep(Duration::from_secs(3));

    // Poll for same connection_id (deterministic UUID)
    let new_connection_id = poll_agent_connection(&client, "go", Duration::from_secs(30))
        .await
        .expect("Expected Go connection after restart");

    println!("After restart: {}", new_connection_id);
    assert_eq!(
        connection_id, new_connection_id,
        "Connection ID should remain the same after restart"
    );

    // Poll for events on existing metric (auto-restored)
    let events = poll_events(&client, metric_name, Duration::from_secs(20)).await;
    assert!(
        !events.is_empty(),
        "Expected events from restored metric {}",
        metric_name
    );
    println!("Received {} events from restored metric", events.len());
}

/// Test agent authentication with wrong token.
///
/// 1. Open tonic channel to agent gRPC server
/// 2. Call ConnectAgent with wrong Bearer token
/// 3. Expect Unauthenticated status
#[tokio::test]
#[ignore]
async fn test_agent_wrong_token() {
    ensure_services_running();

    // Create gRPC channel
    let endpoint = format!("http://127.0.0.1:{}", AGENT_SERVER_GRPC_PORT);
    let channel = Channel::from_shared(endpoint)
        .expect("Invalid gRPC endpoint")
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .expect("Failed to connect to gRPC server");

    // Create agent client
    let mut client = AgentServiceClient::new(channel);

    // Create a stream with a single RegisterAgent message
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

    // Create streaming request
    let stream = stream::iter(vec![register_msg]);
    let mut request = Request::new(stream);

    // Set wrong token in metadata
    let token = format!("{}wrong-token-secret", BEARER_PREFIX)
        .parse()
        .unwrap();
    request
        .metadata_mut()
        .insert(AUTHORIZATION_METADATA_KEY, token);

    // Try to connect - should fail with Unauthenticated
    let result: Result<
        Response<tonic::codec::Streaming<detrix_api::generated::detrix::v1::ServerMessage>>,
        Status,
    > = client.connect_agent(request).await;

    match result {
        Err(status) => {
            assert_eq!(
                status.code(),
                tonic::Code::Unauthenticated,
                "Expected Unauthenticated status, got {:?}",
                status.code()
            );
            println!(
                "Correctly rejected with Unauthenticated: {}",
                status.message()
            );
        }
        Ok(_) => {
            panic!("Expected Unauthenticated error, but connection succeeded");
        }
    }
}

/// Test agent scanner update detection.
///
/// 1. Ensure services are running
/// 2. Kill go-bare process (simulate binary exit)
/// 3. Restart go-fixture container
/// 4. Poll for new/updated connection (scanner detects change)
/// 5. Verify scanner cooldown: restart twice within 5s → only 1 re-registration
#[tokio::test]
#[ignore]
async fn test_agent_scanner_update() {
    ensure_services_running();

    let client = RestClient::new(AGENT_SERVER_HTTP_PORT);

    // Get initial connection
    let initial_conn = poll_agent_connection(&client, "go", Duration::from_secs(30))
        .await
        .expect("Expected initial Go connection");
    println!("Initial connection: {}", initial_conn);

    // Kill go-bare process inside container
    println!("Killing go-bare process...");
    let _kill_output = std::process::Command::new("docker")
        .args(["exec", "detrix-go-fixture", "pkill", "-9", "go-bare"])
        .output()
        .expect("Failed to kill go-bare process");

    // Wait for scanner to detect change
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Restart go-fixture container
    println!("Restarting go-fixture container...");
    compose_agent(&["restart", "detrix-go-fixture"]);

    // Wait for processes to restart
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Poll for connection (scanner should detect the change)
    let new_conn = poll_agent_connection(&client, "go", Duration::from_secs(30))
        .await
        .expect("Expected Go connection after fixture restart");
    println!("After restart: {}", new_conn);

    // Connection ID should be the same (deterministic from binary path)
    assert_eq!(
        initial_conn, new_conn,
        "Connection ID should remain the same for same binary"
    );

    // Test scanner cooldown: restart fixture twice quickly
    println!("Testing scanner cooldown (2 rapid restarts)...");
    compose_agent(&["restart", "detrix-go-fixture"]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    compose_agent(&["restart", "detrix-go-fixture"]);

    // Wait and verify only one registration occurred
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check that we still have exactly one Go connection
    let connections = client
        .list_connections()
        .await
        .expect("Failed to list connections")
        .data;
    let go_connections: Vec<_> = connections
        .into_iter()
        .filter(|c| c.language == "go")
        .collect();
    assert_eq!(
        go_connections.len(),
        1,
        "Expected exactly 1 Go connection after rapid restarts, found {}",
        go_connections.len()
    );
    println!("Scanner cooldown verified: 1 connection after 2 rapid restarts");
}
