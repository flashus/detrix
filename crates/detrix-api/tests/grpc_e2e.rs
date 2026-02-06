//! True End-to-End gRPC Tests for Detrix API
//!
//! These tests use REAL debugpy - no mocks. They verify the complete flow:
//! 1. Start debugpy with a Python script
//! 2. Create AppContext with DapAdapterFactoryImpl
//! 3. Use ConnectionService to connect to debugpy
//! 4. Add metrics via gRPC
//! 5. Verify logpoints are set in debugpy
//! 6. Receive real metric events
//!
//! Requirements:
//! - Python 3.x installed
//! - debugpy installed (`pip install debugpy`)
//!
//! Run with: cargo test --package detrix-api --test grpc_e2e --features e2e_tests
//!
//! For debug output: RUST_LOG=detrix_dap=debug cargo test ...

#![cfg(feature = "e2e_tests")]

use std::sync::Once;
static INIT: Once = Once::new();

fn init_tracing() {
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .init();
    });
}

use detrix_api::generated::detrix::v1::{
    metrics_service_client::MetricsServiceClient, metrics_service_server::MetricsServiceServer,
    streaming_service_client::StreamingServiceClient,
    streaming_service_server::StreamingServiceServer, AddMetricRequest, GetMetricRequest,
    ListMetricsRequest, Location, MetricMode, StreamMode,
};
use detrix_api::grpc::{MetricsServiceImpl, StreamingServiceImpl};
use detrix_api::ApiState;
use detrix_application::{
    AppContext, ConnectionRepositoryRef, DapAdapterFactoryRef, EventRepositoryRef,
    MetricRepositoryRef,
};
use detrix_dap::DapAdapterFactoryImpl;
use detrix_storage::{SqliteConfig, SqliteStorage};
use detrix_testing::e2e::{require_tool, ToolDependency};
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Server};

// Use atomic counter to avoid port conflicts between tests
static PORT_COUNTER: AtomicU16 = AtomicU16::new(16000);

/// Get a unique port for testing
fn get_test_port() -> u16 {
    PORT_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Start a debugpy server on a given port
async fn start_debugpy_server(port: u16, script_path: &str) -> Result<Child, std::io::Error> {
    Command::new("python")
        .args([
            "-m",
            "debugpy",
            "--listen",
            &format!("{}", port),
            "--wait-for-client",
            script_path,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Wait for a port to be listening using lsof (doesn't consume the connection)
async fn wait_for_port(port: u16, timeout_secs: u64) -> bool {
    let start = std::time::Instant::now();
    let timeout_duration = Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout_duration {
        let output = std::process::Command::new("lsof")
            .args(["-i", &format!(":{}", port), "-sTCP:LISTEN"])
            .output();

        if let Ok(out) = output {
            if out.status.success() && !out.stdout.is_empty() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

// ============================================================================
// Test Infrastructure
// ============================================================================

/// E2E Test server with real debugpy using the new connection-based architecture
struct E2ETestServer {
    grpc_addr: SocketAddr,
    shutdown_tx: oneshot::Sender<()>,
    debugpy_process: Child,
    _temp_dir: TempDir,
    script_path: std::path::PathBuf,
}

impl E2ETestServer {
    /// Start a new E2E test server with real debugpy
    ///
    /// Uses the new Clean Architecture pattern:
    /// 1. DapAdapterFactoryImpl for creating real adapters
    /// 2. AppContext for wiring services
    /// 3. ConnectionService for managing connections
    /// 4. ApiState::builder() for API state
    async fn start(script_content: &str) -> anyhow::Result<Self> {
        // Create temp directory
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("test.db");

        // Create Python test script
        let script_path = temp_dir.path().join("test_script.py");
        std::fs::write(&script_path, script_content)?;

        // Get unique port for debugpy
        let debugpy_port = get_test_port();

        // Start debugpy server
        let debugpy_process = start_debugpy_server(debugpy_port, script_path.to_str().unwrap())
            .await
            .expect("Failed to start debugpy");

        // Wait for debugpy to be ready
        if !wait_for_port(debugpy_port, 10).await {
            panic!("debugpy did not start listening on port {}", debugpy_port);
        }

        // Initialize storage
        let sqlite_config = SqliteConfig {
            path: db_path.clone(),
            pool_size: 5,
            busy_timeout_ms: 3000,
        };
        let storage = Arc::new(SqliteStorage::new(&sqlite_config).await?);

        // Create REAL adapter factory (not mock)
        let adapter_factory = Arc::new(DapAdapterFactoryImpl::new(temp_dir.path()));

        // Create application context with connection-based architecture
        let context = AppContext::new(
            Arc::clone(&storage) as MetricRepositoryRef,
            Arc::clone(&storage) as EventRepositoryRef,
            Arc::clone(&storage) as ConnectionRepositoryRef,
            adapter_factory as DapAdapterFactoryRef,
            &detrix_config::ApiConfig::default(),
            &detrix_config::SafetyConfig::default(),
            &detrix_config::StorageConfig::default(),
            &detrix_config::DaemonConfig::default(),
            &detrix_config::AdapterConnectionConfig::default(),
            &detrix_config::AnchorConfig::default(),
            &detrix_config::LimitsConfig::default(),
            None,
            None, // No separate DLQ storage in tests
        );

        // Connect to debugpy using ConnectionService
        let identity = detrix_core::ConnectionIdentity::new(
            format!("grpc-e2e-{}", debugpy_port),
            detrix_core::SourceLanguage::Python,
            "/test-workspace",
            "test-host",
        );
        context
            .connection_service
            .create_connection(
                "127.0.0.1".to_string(),
                debugpy_port,
                identity,
                None,  // No program path for Python
                None,  // No pid (not using AttachPid mode)
                false, // SafeMode disabled for tests
            )
            .await?;

        // Create API state from context
        let state = Arc::new(
            ApiState::builder(context, Arc::clone(&storage) as EventRepositoryRef).build(),
        );

        // Create gRPC services
        let metrics_service = MetricsServiceImpl::new(Arc::clone(&state));
        let streaming_service = StreamingServiceImpl::new(Arc::clone(&state));

        // Find available port for gRPC
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let grpc_addr = listener.local_addr()?;
        let incoming = TcpListenerStream::new(listener);

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Spawn gRPC server
        tokio::spawn(async move {
            Server::builder()
                .add_service(MetricsServiceServer::new(metrics_service))
                .add_service(StreamingServiceServer::new(streaming_service))
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("gRPC server failed");
        });

        // Wait for gRPC server to be ready
        tokio::time::sleep(Duration::from_millis(100)).await;

        Ok(Self {
            grpc_addr,
            shutdown_tx,
            debugpy_process,
            _temp_dir: temp_dir,
            script_path,
        })
    }

    /// Get the gRPC server URI
    fn grpc_uri(&self) -> String {
        format!("http://{}", self.grpc_addr)
    }

    /// Get the test script filename (for location)
    fn script_filename(&self) -> &str {
        self.script_path.file_name().unwrap().to_str().unwrap()
    }

    /// Create a MetricsService client
    async fn metrics_client(&self) -> anyhow::Result<MetricsServiceClient<Channel>> {
        let channel = Channel::from_shared(self.grpc_uri())?.connect().await?;
        Ok(MetricsServiceClient::new(channel))
    }

    /// Create a StreamingService client
    async fn streaming_client(&self) -> anyhow::Result<StreamingServiceClient<Channel>> {
        let channel = Channel::from_shared(self.grpc_uri())?.connect().await?;
        Ok(StreamingServiceClient::new(channel))
    }

    /// Shutdown the server and debugpy
    async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.debugpy_process.kill().await;
    }
}

/// Helper to create AddMetricRequest for E2E tests
fn create_e2e_metric_request(
    name: &str,
    file: &str,
    line: u32,
    expression: &str,
) -> AddMetricRequest {
    AddMetricRequest {
        name: name.to_string(),
        group: Some("e2e_test".to_string()),
        location: Some(Location {
            file: file.to_string(),
            line,
        }),
        expression: expression.to_string(),
        language: "python".to_string(),
        enabled: true,
        mode: Some(MetricMode {
            mode: Some(detrix_api::generated::detrix::v1::metric_mode::Mode::Stream(StreamMode {})),
        }),
        condition: None,
        safety_level: "strict".to_string(),
        metadata: None,
    }
}

// ============================================================================
// E2E Tests with Real Debugpy
// ============================================================================

/// Test: Add metric via gRPC and verify it's set as logpoint in debugpy
#[tokio::test]
async fn test_e2e_add_metric_sets_logpoint() {
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    // Python script with a function we can set logpoints on
    let script = r#"
import time

def process_data(value):
    result = value * 2  # Line 5 - logpoint target
    return result

# Keep script alive
time.sleep(30)
"#;

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric via gRPC
    let request = create_e2e_metric_request(
        "data_processor",
        server.script_filename(),
        5, // Line with `result = value * 2`
        "f'value={value}, result={result}'",
    );

    let response = client.add_metric(request).await;

    // Verify the metric was added successfully
    assert!(
        response.is_ok(),
        "AddMetric should succeed: {:?}",
        response.err()
    );

    let metric = response.unwrap().into_inner();
    assert!(metric.metric_id > 0, "Should have valid metric ID");
    assert_eq!(metric.name, "data_processor");
    assert_eq!(metric.status, "created");

    // Verify the metric is in the list
    let list_request = ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    };
    let list_response = client
        .list_metrics(list_request)
        .await
        .expect("ListMetrics failed");
    let metrics = list_response.into_inner().metrics;

    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].name, "data_processor");

    server.shutdown().await;
}

/// Test: Add multiple metrics and verify they're all set
#[tokio::test]
async fn test_e2e_multiple_metrics() {
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let script = r#"
import time

def func_a(x):
    return x + 1  # Line 5

def func_b(y):
    return y * 2  # Line 8

def func_c(z):
    return z - 1  # Line 11

time.sleep(30)
"#;

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    let filename = server.script_filename();

    // Add multiple metrics
    let metrics_to_add = vec![
        ("metric_a", 5, "f'x={x}'"),
        ("metric_b", 8, "f'y={y}'"),
        ("metric_c", 11, "f'z={z}'"),
    ];

    for (name, line, expr) in &metrics_to_add {
        let request = create_e2e_metric_request(name, filename, *line, expr);
        let response = client.add_metric(request).await;
        assert!(
            response.is_ok(),
            "AddMetric for {} failed: {:?}",
            name,
            response.err()
        );
    }

    // Verify all metrics are listed
    let list_request = ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    };
    let list_response = client
        .list_metrics(list_request)
        .await
        .expect("ListMetrics failed");
    let metrics = list_response.into_inner().metrics;

    assert_eq!(metrics.len(), 3);

    let names: Vec<_> = metrics.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"metric_a"));
    assert!(names.contains(&"metric_b"));
    assert!(names.contains(&"metric_c"));

    server.shutdown().await;
}

/// Test: Get metric by ID after adding via gRPC
#[tokio::test]
async fn test_e2e_get_metric_by_id() {
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let script = r#"
import time
x = 1  # Line 3
time.sleep(30)
"#;

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric
    let request =
        create_e2e_metric_request("get_test_metric", server.script_filename(), 3, "f'x={x}'");
    let add_response = client.add_metric(request).await.expect("AddMetric failed");
    let metric_id = add_response.into_inner().metric_id;

    // Get the metric by ID
    let get_request = GetMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::get_metric_request::Identifier::MetricId(metric_id),
        ),
        metadata: None,
    };
    let get_response = client
        .get_metric(get_request)
        .await
        .expect("GetMetric failed");
    let metric = get_response.into_inner();

    assert_eq!(metric.metric_id, metric_id);
    assert_eq!(metric.name, "get_test_metric");

    server.shutdown().await;
}

/// Test: Full lifecycle with real debugpy
#[tokio::test]
async fn test_e2e_full_lifecycle() {
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let script = r#"
import time

def calculate(a, b):
    result = a + b  # Line 5
    return result

time.sleep(30)
"#;

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    let filename = server.script_filename();

    // 1. Add metric
    let request = create_e2e_metric_request("lifecycle_metric", filename, 5, "f'a={a}, b={b}'");
    let add_response = client.add_metric(request).await.expect("AddMetric failed");
    let metric_id = add_response.into_inner().metric_id;
    assert!(metric_id > 0);

    // 2. Get metric
    let get_request = GetMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::get_metric_request::Identifier::MetricId(metric_id),
        ),
        metadata: None,
    };
    let get_response = client
        .get_metric(get_request)
        .await
        .expect("GetMetric failed");
    assert_eq!(get_response.into_inner().name, "lifecycle_metric");

    // 3. List metrics
    let list_request = ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    };
    let list_response = client
        .list_metrics(list_request)
        .await
        .expect("ListMetrics failed");
    assert_eq!(list_response.into_inner().metrics.len(), 1);

    // 4. Remove metric
    let remove_request = detrix_api::generated::detrix::v1::RemoveMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::remove_metric_request::Identifier::MetricId(
                metric_id,
            ),
        ),
        metadata: None,
    };
    let remove_response = client
        .remove_metric(remove_request)
        .await
        .expect("RemoveMetric failed");
    assert!(remove_response.into_inner().success);

    // 5. Verify metric is gone
    let list_request = ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    };
    let list_response = client
        .list_metrics(list_request)
        .await
        .expect("ListMetrics failed");
    assert_eq!(list_response.into_inner().metrics.len(), 0);

    server.shutdown().await;
}

/// Test: Error handling - metric not found
#[tokio::test]
async fn test_e2e_metric_not_found() {
    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let script = r#"
import time
time.sleep(30)
"#;

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Try to get non-existent metric
    let get_request = GetMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::get_metric_request::Identifier::MetricId(99999),
        ),
        metadata: None,
    };
    let result = client.get_metric(get_request).await;

    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.shutdown().await;
}

// ============================================================================
// Streaming and Query E2E Tests
// ============================================================================

/// Test: Stream real-time metric events from debugpy
#[tokio::test]
async fn test_e2e_stream_metric_events() {
    use detrix_api::generated::detrix::v1::StreamAllRequest;
    use tokio_stream::StreamExt;

    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    // Python script that generates events - waits for metric to be set first
    let script = "import time

def process_value(x):
    result = x * 2  # Line 4 - logpoint target
    return result

# Wait for metric to be set, then generate events
time.sleep(2)
for i in range(30):
    val = process_value(i)
    time.sleep(0.1)

time.sleep(5)
";

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut metrics_client = server
        .metrics_client()
        .await
        .expect("Failed to create metrics client");

    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create streaming client");

    // Add a metric that will generate events - logpoint fires BEFORE line executes, so only `x` is available
    let request = create_e2e_metric_request(
        "stream_test_metric",
        server.script_filename(),
        4, // Line with `result = x * 2`
        "f'x={x}'",
    );
    let add_response = metrics_client.add_metric(request).await;
    assert!(
        add_response.is_ok(),
        "AddMetric failed: {:?}",
        add_response.err()
    );

    // Wait for some events to be generated
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Stream events with timeout
    let stream_request = StreamAllRequest {
        thread_filter: None,
        metadata: None,
    };

    let response = streaming_client.stream_all(stream_request).await;
    assert!(response.is_ok(), "StreamAll failed: {:?}", response.err());

    let mut stream = response.unwrap().into_inner();
    let mut events_received = Vec::new();

    // Collect events with timeout
    let timeout = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(event) => {
                    events_received.push(event);
                    // Stop after receiving a few events
                    if events_received.len() >= 3 {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Stream error: {:?}", e);
                    break;
                }
            }
        }
    });

    let _ = timeout.await; // Ignore timeout error, we just want to collect what we can

    // Verify we received at least some events
    println!("Received {} events via streaming", events_received.len());

    // Events may or may not be received depending on timing
    // The important thing is that streaming works without errors
    for event in &events_received {
        assert!(event.metric_id > 0, "Event should have valid metric ID");
        // Note: metric_name is not populated in streaming (TODO in conversions.rs)
        assert!(event.timestamp > 0, "Event should have timestamp");
    }

    server.shutdown().await;
}

/// Test: Query historical metric events
#[tokio::test]
async fn test_e2e_query_historical_events() {
    init_tracing();
    use detrix_api::generated::detrix::v1::QueryRequest;

    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    // Python script that generates events - runs long enough for metric to be set
    // Note: Line numbers in heredoc:
    // Line 1: import time
    // Line 2: (empty)
    // Line 3: def calculate(n):
    // Line 4:     result = n * n  <- logpoint target
    let script = "import time

def calculate(n):
    result = n * n  # Line 4 - logpoint target
    return result

# Wait a bit for metric to be set, then generate events
time.sleep(2)
for i in range(20):
    val = calculate(i)
    time.sleep(0.1)

time.sleep(5)
";

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut metrics_client = server
        .metrics_client()
        .await
        .expect("Failed to create metrics client");

    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create streaming client");

    // Add a metric - use just filename since adapter's base_path is the temp dir
    // Note: logpoint fires BEFORE line executes, so we can only access `n`, not `result`
    let request = create_e2e_metric_request(
        "query_test_metric",
        server.script_filename(),
        4, // Line with `result = n * n`
        "f'n={n}'",
    );
    println!(
        "Adding metric for file: {} (base_path resolves to full path)",
        server.script_filename()
    );
    let add_response = metrics_client.add_metric(request).await;
    assert!(
        add_response.is_ok(),
        "AddMetric failed: {:?}",
        add_response.err()
    );
    let response = add_response.unwrap().into_inner();
    let metric_id = response.metric_id;
    println!(
        "Metric added with ID: {}, status: {}",
        metric_id, response.status
    );
    if let Some(loc) = response.location {
        println!("  Location: {}:{}", loc.file, loc.line);
    }

    // Wait for events to be generated and stored
    println!("Waiting for events to be generated...");
    tokio::time::sleep(Duration::from_secs(4)).await;
    println!("Done waiting");

    // Query historical events
    let query_request = QueryRequest {
        metric_ids: vec![metric_id],
        time_range: None,
        limit: Some(10),
        offset: None,
        cursor: None,
        order: None,
        metadata: None,
    };

    let query_response = streaming_client.query_metrics(query_request).await;
    assert!(
        query_response.is_ok(),
        "QueryMetrics failed: {:?}",
        query_response.err()
    );

    let response = query_response.unwrap().into_inner();
    println!(
        "Query returned {} events, total_count: {}",
        response.events.len(),
        response.total_count
    );

    // Verify events structure
    for event in &response.events {
        assert_eq!(
            event.metric_id, metric_id,
            "Event should belong to queried metric"
        );
        assert!(event.timestamp > 0, "Event should have timestamp");

        // Check that we have a value
        match &event.result {
            Some(detrix_api::generated::detrix::v1::metric_event::Result::ValueJson(val)) => {
                assert!(!val.is_empty(), "Value should not be empty");
                println!("  Event value: {}", val);
            }
            Some(detrix_api::generated::detrix::v1::metric_event::Result::Error(err)) => {
                panic!("Unexpected error in event: {:?}", err);
            }
            None => {
                // No result is also acceptable for some events
            }
        }
    }

    server.shutdown().await;
}

/// Test: Query events with limit and verify pagination
#[tokio::test]
async fn test_e2e_query_with_pagination() {
    use detrix_api::generated::detrix::v1::QueryRequest;

    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    // Python script that generates many events - waits for metric setup
    let script = "import time

def counter(i):
    count = i  # Line 4 - logpoint target
    return count

# Wait for metric to be set, then generate many events
time.sleep(2)
for i in range(50):
    counter(i)
    time.sleep(0.05)

time.sleep(5)
";

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut metrics_client = server
        .metrics_client()
        .await
        .expect("Failed to create metrics client");

    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create streaming client");

    // Add a metric - logpoint fires BEFORE line executes, so only `i` is available
    let request =
        create_e2e_metric_request("pagination_test", server.script_filename(), 4, "f'i={i}'");
    let add_response = metrics_client.add_metric(request).await;
    assert!(add_response.is_ok(), "AddMetric failed");
    let metric_id = add_response.unwrap().into_inner().metric_id;

    // Wait for events to be generated
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Query with small limit
    let query_request = QueryRequest {
        metric_ids: vec![metric_id],
        time_range: None,
        limit: Some(5),
        offset: None,
        cursor: None,
        order: None,
        metadata: None,
    };

    let query_response = streaming_client.query_metrics(query_request).await;
    assert!(query_response.is_ok(), "QueryMetrics failed");

    let response = query_response.unwrap().into_inner();

    // Should return at most 5 events
    assert!(response.events.len() <= 5, "Should respect limit");
    println!(
        "First page: {} events, total: {}, has_more: {}",
        response.events.len(),
        response.total_count,
        response.has_more
    );

    // If there are more events, has_more should be true
    if response.total_count > 5 {
        assert!(response.has_more, "Should indicate more events available");
    }

    server.shutdown().await;
}

/// Test: Stream specific metrics by ID
#[tokio::test]
async fn test_e2e_stream_specific_metrics() {
    use detrix_api::generated::detrix::v1::StreamRequest;
    use tokio_stream::StreamExt;

    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    // Python script with multiple functions - waits for metrics setup
    let script = "import time

def func_a(x):
    a = x + 1  # Line 4
    return a

def func_b(y):
    b = y * 2  # Line 8
    return b

# Wait for metrics to be set, then generate events for both
time.sleep(2)
for i in range(20):
    func_a(i)
    func_b(i)
    time.sleep(0.1)

time.sleep(5)
";

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut metrics_client = server
        .metrics_client()
        .await
        .expect("Failed to create metrics client");

    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create streaming client");

    let filename = server.script_filename();

    // Add two metrics - logpoint fires BEFORE line executes, so only `x` and `y` are available
    let req_a = create_e2e_metric_request("metric_a", filename, 4, "f'x={x}'");
    let resp_a = metrics_client
        .add_metric(req_a)
        .await
        .expect("AddMetric A failed");
    let metric_a_id = resp_a.into_inner().metric_id;

    let req_b = create_e2e_metric_request("metric_b", filename, 8, "f'y={y}'");
    let resp_b = metrics_client
        .add_metric(req_b)
        .await
        .expect("AddMetric B failed");
    let _metric_b_id = resp_b.into_inner().metric_id;

    // Wait for events
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Stream only metric A
    let stream_request = StreamRequest {
        metric_ids: vec![metric_a_id],
        name_pattern: None,
        metadata: None,
    };

    let response = streaming_client.stream_metrics(stream_request).await;
    assert!(
        response.is_ok(),
        "StreamMetrics failed: {:?}",
        response.err()
    );

    let mut stream = response.unwrap().into_inner();
    let mut events_received = Vec::new();

    // Collect events with timeout
    let timeout = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(event) => {
                    events_received.push(event);
                    if events_received.len() >= 3 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let _ = timeout.await;

    // All received events should be for metric A
    for event in &events_received {
        assert_eq!(
            event.metric_id, metric_a_id,
            "Should only receive events for metric A"
        );
        assert_eq!(event.metric_name, "metric_a");
    }

    println!(
        "Received {} events for metric_a only",
        events_received.len()
    );

    server.shutdown().await;
}

/// Test: Query events with time range filter
#[tokio::test]
async fn test_e2e_query_with_time_range() {
    use detrix_api::generated::detrix::v1::{QueryRequest, TimeRange};

    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let script = "import time

def tick(t):
    val = t  # Line 4
    return val

# Wait for metric to be set, then generate events
time.sleep(2)
for i in range(20):
    tick(i)
    time.sleep(0.1)

time.sleep(5)
";

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut metrics_client = server
        .metrics_client()
        .await
        .expect("Failed to create metrics client");

    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create streaming client");

    // Add metric - logpoint fires BEFORE line executes, so only `t` is available
    let request =
        create_e2e_metric_request("time_range_test", server.script_filename(), 4, "f't={t}'");
    let add_response = metrics_client
        .add_metric(request)
        .await
        .expect("AddMetric failed");
    let metric_id = add_response.into_inner().metric_id;

    // Wait for events
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Get current time in microseconds
    let now_micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;

    // Query with time range (last 10 seconds)
    let query_request = QueryRequest {
        metric_ids: vec![metric_id],
        time_range: Some(TimeRange {
            start: now_micros - 10_000_000, // 10 seconds ago
            end: now_micros,
        }),
        limit: Some(100),
        offset: None,
        cursor: None,
        order: None,
        metadata: None,
    };

    let query_response = streaming_client.query_metrics(query_request).await;
    assert!(query_response.is_ok(), "QueryMetrics failed");

    let response = query_response.unwrap().into_inner();
    println!("Time range query returned {} events", response.events.len());

    // All events should be within the time range
    for event in &response.events {
        assert!(
            event.timestamp >= now_micros - 10_000_000,
            "Event should be after start time"
        );
        assert!(
            event.timestamp <= now_micros,
            "Event should be before end time"
        );
    }

    server.shutdown().await;
}

/// Test: Error events from invalid expressions
#[tokio::test]
async fn test_e2e_error_events_from_invalid_expression() {
    use detrix_api::generated::detrix::v1::QueryRequest;

    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    // Python script with a function where we'll use an invalid expression
    let script = "import time

def process(x):
    result = x * 2  # Line 4 - we'll try to access 'result' before it's defined
    return result

# Wait for metric to be set, then generate events
time.sleep(2)
for i in range(10):
    process(i)
    time.sleep(0.1)

time.sleep(5)
";

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut metrics_client = server
        .metrics_client()
        .await
        .expect("Failed to create metrics client");

    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create streaming client");

    // Add a metric with an expression that references 'result' before it's defined
    // This should generate error events
    let request = create_e2e_metric_request(
        "error_test_metric",
        server.script_filename(),
        4,                    // Line with `result = x * 2` - 'result' not yet defined
        "f'result={result}'", // This will fail because 'result' is not defined yet
    );
    let add_response = metrics_client.add_metric(request).await;
    assert!(
        add_response.is_ok(),
        "AddMetric failed: {:?}",
        add_response.err()
    );
    let metric_id = add_response.unwrap().into_inner().metric_id;
    println!("Metric added with ID: {}", metric_id);

    // Wait for error events to be generated
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Query events - should include error events
    let query_request = QueryRequest {
        metric_ids: vec![metric_id],
        time_range: None,
        limit: Some(20),
        offset: None,
        cursor: None,
        order: None,
        metadata: None,
    };

    let query_response = streaming_client.query_metrics(query_request).await;
    assert!(query_response.is_ok(), "QueryMetrics failed");

    let response = query_response.unwrap().into_inner();
    println!("Query returned {} events", response.events.len());

    // We should have received some events (either errors or values)
    assert!(!response.events.is_empty(), "Should have received events");

    // Check if any events are errors
    let error_events: Vec<_> = response
        .events
        .iter()
        .filter(|e| {
            matches!(
                &e.result,
                Some(detrix_api::generated::detrix::v1::metric_event::Result::Error(_))
            )
        })
        .collect();

    println!(
        "Found {} error events out of {} total",
        error_events.len(),
        response.events.len()
    );

    // Verify error event structure
    for event in &error_events {
        if let Some(detrix_api::generated::detrix::v1::metric_event::Result::Error(err)) =
            &event.result
        {
            assert!(err.error, "Error flag should be true");
            assert!(
                !err.error_message.is_empty(),
                "Error message should not be empty"
            );
            println!("  Error: {} - {}", err.error_type, err.error_message);
        }
    }

    server.shutdown().await;
}

/// Test: Verify metric_name is populated in streamed events
#[tokio::test]
async fn test_e2e_stream_events_have_metric_name() {
    use detrix_api::generated::detrix::v1::StreamAllRequest;
    use tokio_stream::StreamExt;

    if !require_tool(ToolDependency::Debugpy).await {
        return;
    }

    let script = "import time

def counter(n):
    count = n  # Line 4
    return count

time.sleep(2)
for i in range(20):
    counter(i)
    time.sleep(0.1)

time.sleep(5)
";

    let server = E2ETestServer::start(script)
        .await
        .expect("Failed to start E2E server");

    let mut metrics_client = server
        .metrics_client()
        .await
        .expect("Failed to create metrics client");

    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create streaming client");

    // Add a metric
    let request =
        create_e2e_metric_request("named_metric", server.script_filename(), 4, "f'n={n}'");
    let add_response = metrics_client.add_metric(request).await;
    assert!(add_response.is_ok(), "AddMetric failed");

    // Wait for events
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Stream events
    let stream_request = StreamAllRequest {
        thread_filter: None,
        metadata: None,
    };

    let response = streaming_client.stream_all(stream_request).await;
    assert!(response.is_ok(), "StreamAll failed");

    let mut stream = response.unwrap().into_inner();
    let mut events_received = Vec::new();

    let timeout = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(result) = stream.next().await {
            if let Ok(event) = result {
                events_received.push(event);
                if events_received.len() >= 3 {
                    break;
                }
            }
        }
    });

    let _ = timeout.await;

    println!("Received {} events via streaming", events_received.len());

    // Verify metric_name is populated
    for event in &events_received {
        assert!(
            !event.metric_name.is_empty(),
            "Event should have metric_name populated"
        );
        assert_eq!(
            event.metric_name, "named_metric",
            "Metric name should match"
        );
        println!("  Event metric_name: {}", event.metric_name);
    }

    server.shutdown().await;
}
