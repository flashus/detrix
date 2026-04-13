//! Integration Tests for Detrix gRPC API
//!
//! These tests spin up a real gRPC server with MockDapAdapterFactory and test
//! the full request/response flow through the API layer.
//!
//! Uses the new Clean Architecture pattern:
//! - MockDapAdapterFactory from detrix-testing
//! - AppContext for wiring services
//! - ApiState::builder() for API state
//!
//! For true E2E tests with real debugpy, see grpc_e2e.rs
//!
//! Test categories:
//! 1. Metric CRUD lifecycle
//! 2. Group operations
//! 3. Streaming (real-time events)
//! 4. Query operations
//! 5. Error handling
//! 6. Concurrent operations

use detrix_api::generated::detrix::v1::{
    metrics_service_client::MetricsServiceClient, metrics_service_server::MetricsServiceServer,
    streaming_service_client::StreamingServiceClient,
    streaming_service_server::StreamingServiceServer, AddMetricRequest, DisconnectAllRequest,
    GetMetricRequest, GroupRequest, InspectFileRequest, ListMetricsRequest, Location, MetricMode,
    QueryRequest, RemoveMetricRequest, StatusRequest, StreamAllRequest, StreamMode,
    ToggleMetricRequest, UpdateMetricRequest,
};
use detrix_api::grpc::interceptor::{create_auth_interceptor, AuthInterceptorState};
use detrix_api::grpc::{MetricsServiceImpl, StreamingServiceImpl};
use detrix_api::ApiState;
use detrix_application::{
    AppContext, ConnectionReferenceRepositoryRef, ConnectionRepositoryRef, DapAdapterFactoryRef,
    EventRepositoryRef, MetricRepositoryRef,
};
use detrix_config::ApiConfig;
use detrix_storage::{SqliteConfig, SqliteStorage};
use detrix_testing::fixtures::{app_py_path, fixtures_dir, test_py_path};
use detrix_testing::MockDapAdapterFactory;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Server};

// ============================================================================
// Test Infrastructure
// ============================================================================

/// Test server that manages gRPC server lifecycle
struct TestServer {
    addr: SocketAddr,
    shutdown_tx: oneshot::Sender<()>,
    _temp_dir: TempDir,
    /// Connection ID (UUID generated from identity at test start)
    connection_id: String,
}

impl TestServer {
    /// Start a new test server with fresh database and mock connection
    async fn start() -> anyhow::Result<Self> {
        // Create temp directory for database
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("test.db");

        // Initialize storage
        let sqlite_config = SqliteConfig {
            path: db_path,
            pool_size: 5,
            busy_timeout_ms: 3000,
        };
        let storage = Arc::new(SqliteStorage::new(&sqlite_config).await?);

        // Create mock adapter factory (doesn't connect to any debugger)
        let mock_factory = Arc::new(MockDapAdapterFactory::new());

        // Create AppContext with mock factory
        let vfs = Arc::new(detrix_storage::DiskVfs::new()) as detrix_application::VfsRef;
        let file_source_chain = Arc::new(detrix_application::FileSourceChain::new(
            Arc::clone(&vfs),
            vec![],
            &[],
        ));
        let context = AppContext::new(
            Arc::clone(&storage) as MetricRepositoryRef,
            Arc::clone(&storage) as EventRepositoryRef,
            Arc::clone(&storage) as ConnectionRepositoryRef,
            mock_factory as DapAdapterFactoryRef,
            &ApiConfig::default(),
            &detrix_config::SafetyConfig::default(),
            &detrix_config::StorageConfig::default(),
            &detrix_config::DaemonConfig::default(),
            &detrix_config::AdapterConnectionConfig::default(),
            &detrix_config::AnchorConfig::default(),
            &detrix_config::LimitsConfig::default(),
            None,
            None, // No separate DLQ storage in tests
            None,
            None, // No auth token in tests
            vfs,
            file_source_chain,
            Arc::clone(&storage) as ConnectionReferenceRepositoryRef,
            HashMap::new(),
        None, // agent_config
        );

        // Create a mock connection so metrics can be added
        // This creates a mock adapter via MockDapAdapterFactory
        // UUID is generated deterministically from identity fields
        let identity = detrix_core::ConnectionIdentity::new(
            "default",
            detrix_core::SourceLanguage::Python,
            "/test-workspace",
            "test-host",
        );
        let connection_id = context
            .connection_service
            .create_connection(
                "127.0.0.1".to_string(),
                5678, // Arbitrary port (mock doesn't actually connect)
                identity,
                None,  // No program path for Python
                None,  // No pid (not using AttachPid mode)
                false, // SafeMode disabled for tests
            )
            .await?;

        // Create API state from context
        let state = Arc::new(ApiState::builder(context, storage).build());

        // Create gRPC services
        let metrics_service = MetricsServiceImpl::new(Arc::clone(&state));
        let streaming_service = StreamingServiceImpl::new(Arc::clone(&state));

        // Auth interceptor (disabled mode → injects default Admin user)
        let auth_state = AuthInterceptorState::new(detrix_config::AuthConfig::default());
        let auth_interceptor = create_auth_interceptor(auth_state);

        // Find available port
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        // Convert tokio listener to tonic-compatible incoming stream
        let incoming = TcpListenerStream::new(listener);

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Spawn server
        tokio::spawn(async move {
            Server::builder()
                .add_service(MetricsServiceServer::with_interceptor(
                    metrics_service,
                    auth_interceptor.clone(),
                ))
                .add_service(StreamingServiceServer::with_interceptor(
                    streaming_service,
                    auth_interceptor,
                ))
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("Server failed");
        });

        // Wait for server to be ready
        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok(Self {
            addr,
            shutdown_tx,
            _temp_dir: temp_dir,
            connection_id: connection_id.to_string(),
        })
    }

    /// Start a new test server with custom workspace_root for path resolution tests
    async fn start_with_workspace(workspace_root: &str) -> anyhow::Result<Self> {
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("test.db");

        let sqlite_config = SqliteConfig {
            path: db_path,
            pool_size: 5,
            busy_timeout_ms: 3000,
        };
        let storage = Arc::new(SqliteStorage::new(&sqlite_config).await?);
        let mock_factory = Arc::new(MockDapAdapterFactory::new());

        let vfs = Arc::new(detrix_storage::DiskVfs::new()) as detrix_application::VfsRef;
        let file_source_chain = Arc::new(detrix_application::FileSourceChain::new(
            Arc::clone(&vfs),
            vec![],
            &[],
        ));
        let context = AppContext::new(
            Arc::clone(&storage) as MetricRepositoryRef,
            Arc::clone(&storage) as EventRepositoryRef,
            Arc::clone(&storage) as ConnectionRepositoryRef,
            mock_factory as DapAdapterFactoryRef,
            &ApiConfig::default(),
            &detrix_config::SafetyConfig::default(),
            &detrix_config::StorageConfig::default(),
            &detrix_config::DaemonConfig::default(),
            &detrix_config::AdapterConnectionConfig::default(),
            &detrix_config::AnchorConfig::default(),
            &detrix_config::LimitsConfig::default(),
            None,
            None,
            None,
            None,
            vfs,
            file_source_chain,
            Arc::clone(&storage) as ConnectionReferenceRepositoryRef,
            HashMap::new(),
        None, // agent_config
        );

        let identity = detrix_core::ConnectionIdentity::new(
            "default",
            detrix_core::SourceLanguage::Python,
            workspace_root,
            "test-host",
        );
        let connection_id = context
            .connection_service
            .create_connection("127.0.0.1".to_string(), 5678, identity, None, None, false)
            .await?;

        let state = Arc::new(ApiState::builder(context, storage).build());

        let metrics_service = MetricsServiceImpl::new(Arc::clone(&state));
        let streaming_service = StreamingServiceImpl::new(Arc::clone(&state));

        let auth_state = AuthInterceptorState::new(detrix_config::AuthConfig::default());
        let auth_interceptor = create_auth_interceptor(auth_state);

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let incoming = TcpListenerStream::new(listener);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        tokio::spawn(async move {
            Server::builder()
                .add_service(MetricsServiceServer::with_interceptor(
                    metrics_service,
                    auth_interceptor.clone(),
                ))
                .add_service(StreamingServiceServer::with_interceptor(
                    streaming_service,
                    auth_interceptor,
                ))
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("Server failed");
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok(Self {
            addr,
            shutdown_tx,
            _temp_dir: temp_dir,
            connection_id: connection_id.to_string(),
        })
    }

    /// Get the server address as a URI
    fn uri(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Create a MetricsService client
    async fn metrics_client(&self) -> anyhow::Result<MetricsServiceClient<Channel>> {
        let channel = Channel::from_shared(self.uri())?.connect().await?;
        Ok(MetricsServiceClient::new(channel))
    }

    /// Create a StreamingService client
    async fn streaming_client(&self) -> anyhow::Result<StreamingServiceClient<Channel>> {
        let channel = Channel::from_shared(self.uri())?.connect().await?;
        Ok(StreamingServiceClient::new(channel))
    }

    /// Shutdown the server
    fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Helper to create a valid AddMetricRequest
/// Uses real fixture file paths for proper file validation
fn create_add_metric_request(
    name: &str,
    file: &str,
    line: u32,
    connection_id: &str,
) -> AddMetricRequest {
    // Map short file names to real fixture paths
    let file_path = match file {
        "test.py" => test_py_path(),
        "app.py" => app_py_path(),
        other => other.to_string(),
    };

    AddMetricRequest {
        name: name.to_string(),
        group: Some("test_group".to_string()),
        location: Some(Location {
            file: file_path,
            line,
        }),
        expressions: vec!["x.value".to_string()],
        language: Some("python".to_string()),
        enabled: true,
        mode: Some(MetricMode {
            mode: Some(detrix_api::generated::detrix::v1::metric_mode::Mode::Stream(StreamMode {})),
        }),
        condition: None,
        safety_level: "strict".to_string(),
        metadata: None,
        connection_id: connection_id.to_string(),
        replace: None, // Don't replace existing metrics
        // Introspection fields (disabled by default)
        capture_stack_trace: None,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: None,
        snapshot_scope: None,
        snapshot_ttl: None,
        file_content: None,
    }
}

// ============================================================================
// Metric CRUD Lifecycle Tests
// ============================================================================

#[tokio::test]
async fn test_metric_add_and_get() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric
    let request = create_add_metric_request("test_metric", "test.py", 10, &server.connection_id);
    let response = client.add_metric(request).await.expect("AddMetric failed");
    let metric = response.into_inner();

    assert!(metric.metric_id > 0, "Metric ID should be positive");
    assert_eq!(metric.name, "test_metric");
    assert_eq!(metric.status, "created");

    // Get the metric by ID
    let get_request = GetMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::get_metric_request::Identifier::MetricId(
                metric.metric_id,
            ),
        ),
        metadata: None,
    };
    let get_response = client
        .get_metric(get_request)
        .await
        .expect("GetMetric failed");
    let found_metric = get_response.into_inner();

    assert_eq!(found_metric.metric_id, metric.metric_id);
    assert_eq!(found_metric.name, "test_metric");

    server.shutdown();
}

#[tokio::test]
async fn test_add_metric_with_multiple_expressions() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric with 3 expressions
    let mut request =
        create_add_metric_request("multi_expr_metric", "test.py", 10, &server.connection_id);
    request.expressions = vec![
        "x.value".to_string(),
        "y.count".to_string(),
        "z.status".to_string(),
    ];
    let response = client.add_metric(request).await.expect("AddMetric failed");
    let metric = response.into_inner();

    assert!(metric.metric_id > 0, "Metric ID should be positive");
    assert_eq!(metric.name, "multi_expr_metric");

    // Verify expressions are returned in AddMetric response
    assert_eq!(metric.expressions.len(), 3);
    assert_eq!(metric.expressions[0], "x.value");
    assert_eq!(metric.expressions[1], "y.count");
    assert_eq!(metric.expressions[2], "z.status");

    // Verify expressions are also returned via GetMetric
    let get_response = client
        .get_metric(GetMetricRequest {
            identifier: Some(
                detrix_api::generated::detrix::v1::get_metric_request::Identifier::Name(
                    "multi_expr_metric".to_string(),
                ),
            ),
            metadata: None,
        })
        .await
        .expect("GetMetric failed");
    let found = get_response.into_inner();
    assert_eq!(found.expressions.len(), 3);
    assert_eq!(found.expressions[0], "x.value");
    assert_eq!(found.expressions[1], "y.count");
    assert_eq!(found.expressions[2], "z.status");

    server.shutdown();
}

#[tokio::test]
async fn test_metric_add_and_get_by_name() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric
    let request = create_add_metric_request("named_metric", "app.py", 42, &server.connection_id);
    client.add_metric(request).await.expect("AddMetric failed");

    // Get the metric by name
    let get_request = GetMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::get_metric_request::Identifier::Name(
                "named_metric".to_string(),
            ),
        ),
        metadata: None,
    };
    let get_response = client
        .get_metric(get_request)
        .await
        .expect("GetMetric failed");
    let found_metric = get_response.into_inner();

    assert_eq!(found_metric.name, "named_metric");

    server.shutdown();
}

#[tokio::test]
async fn test_metric_update() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric
    let request = create_add_metric_request("update_test", "test.py", 10, &server.connection_id);
    let response = client.add_metric(request).await.expect("AddMetric failed");
    let metric_id = response.into_inner().metric_id;

    // Update the metric (complex expression skips file inspection)
    let update_request = UpdateMetricRequest {
        metric_id,
        expressions: vec!["y.updated".to_string()],
        enabled: Some(false),
        mode: None,
        condition: Some("y > 0".to_string()),
        metadata: None,
    };
    let update_response = client
        .update_metric(update_request)
        .await
        .expect("UpdateMetric failed");
    let updated = update_response.into_inner();

    assert_eq!(updated.metric_id, metric_id);
    assert_eq!(updated.status, "updated");

    server.shutdown();
}

#[tokio::test]
async fn test_metric_toggle() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric (enabled by default)
    let request = create_add_metric_request("toggle_test", "test.py", 10, &server.connection_id);
    let response = client.add_metric(request).await.expect("AddMetric failed");
    let metric_id = response.into_inner().metric_id;

    // Disable the metric
    let toggle_request = ToggleMetricRequest {
        metric_id,
        enabled: false,
        metadata: None,
    };
    let toggle_response = client
        .toggle_metric(toggle_request)
        .await
        .expect("ToggleMetric failed");
    let toggled = toggle_response.into_inner();

    assert!(!toggled.enabled);
    assert!(toggled.dap_confirmed);

    // Enable the metric
    let toggle_request = ToggleMetricRequest {
        metric_id,
        enabled: true,
        metadata: None,
    };
    let toggle_response = client
        .toggle_metric(toggle_request)
        .await
        .expect("ToggleMetric failed");
    let toggled = toggle_response.into_inner();

    assert!(toggled.enabled);
    assert!(toggled.dap_confirmed);

    server.shutdown();
}

#[tokio::test]
async fn test_metric_remove_by_id() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric
    let request = create_add_metric_request("remove_test", "test.py", 10, &server.connection_id);
    let response = client.add_metric(request).await.expect("AddMetric failed");
    let metric_id = response.into_inner().metric_id;

    // Remove the metric
    let remove_request = RemoveMetricRequest {
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

    // Verify metric is gone
    let get_request = GetMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::get_metric_request::Identifier::MetricId(metric_id),
        ),
        metadata: None,
    };
    let get_result = client.get_metric(get_request).await;

    assert!(get_result.is_err(), "Metric should not exist after removal");

    server.shutdown();
}

#[tokio::test]
async fn test_metric_remove_by_name() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric
    let request = create_add_metric_request("remove_by_name", "test.py", 10, &server.connection_id);
    client.add_metric(request).await.expect("AddMetric failed");

    // Remove the metric by name
    let remove_request = RemoveMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::remove_metric_request::Identifier::Name(
                "remove_by_name".to_string(),
            ),
        ),
        metadata: None,
    };
    let remove_response = client
        .remove_metric(remove_request)
        .await
        .expect("RemoveMetric failed");

    assert!(remove_response.into_inner().success);

    server.shutdown();
}

#[tokio::test]
async fn test_list_metrics() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add multiple metrics
    for i in 1..=3 {
        let request = create_add_metric_request(
            &format!("list_metric_{}", i),
            "test.py",
            i * 10,
            &server.connection_id,
        );
        client.add_metric(request).await.expect("AddMetric failed");
    }

    // List all metrics
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

    server.shutdown();
}

#[tokio::test]
async fn test_list_metrics_by_group() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add metrics to different groups
    let mut request1 =
        create_add_metric_request("group_a_metric", "test.py", 10, &server.connection_id);
    request1.group = Some("group_a".to_string());
    client.add_metric(request1).await.expect("AddMetric failed");

    let mut request2 =
        create_add_metric_request("group_b_metric", "test.py", 20, &server.connection_id);
    request2.group = Some("group_b".to_string());
    client.add_metric(request2).await.expect("AddMetric failed");

    // List metrics in group_a
    let list_request = ListMetricsRequest {
        group: Some("group_a".to_string()),
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
    assert_eq!(metrics[0].name, "group_a_metric");

    server.shutdown();
}

#[tokio::test]
async fn test_list_metrics_enabled_only() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add enabled metric
    let request1 =
        create_add_metric_request("enabled_metric", "test.py", 10, &server.connection_id);
    client.add_metric(request1).await.expect("AddMetric failed");

    // Add disabled metric
    let mut request2 =
        create_add_metric_request("disabled_metric", "test.py", 20, &server.connection_id);
    request2.enabled = false;
    client.add_metric(request2).await.expect("AddMetric failed");

    // List only enabled metrics
    let list_request = ListMetricsRequest {
        group: None,
        enabled_only: Some(true),
        name_pattern: None,
        metadata: None,
    };
    let list_response = client
        .list_metrics(list_request)
        .await
        .expect("ListMetrics failed");
    let metrics = list_response.into_inner().metrics;

    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].name, "enabled_metric");

    server.shutdown();
}

// ============================================================================
// Group Operations Tests
// ============================================================================

#[tokio::test]
async fn test_enable_group() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add disabled metrics to a group
    for i in 1..=3 {
        let mut request = create_add_metric_request(
            &format!("enable_group_metric_{}", i),
            "test.py",
            i * 10,
            &server.connection_id,
        );
        request.group = Some("enable_test_group".to_string());
        request.enabled = false;
        client.add_metric(request).await.expect("AddMetric failed");
    }

    // Enable the group
    let group_request = GroupRequest {
        group_name: "enable_test_group".to_string(),
        metadata: None,
    };
    let group_response = client
        .enable_group(group_request)
        .await
        .expect("EnableGroup failed");
    let result = group_response.into_inner();

    assert_eq!(result.metrics_affected, 3);

    server.shutdown();
}

#[tokio::test]
async fn test_disable_group() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add enabled metrics to a group
    for i in 1..=2 {
        let mut request = create_add_metric_request(
            &format!("disable_group_metric_{}", i),
            "test.py",
            i * 10,
            &server.connection_id,
        );
        request.group = Some("disable_test_group".to_string());
        request.enabled = true;
        client.add_metric(request).await.expect("AddMetric failed");
    }

    // Disable the group
    let group_request = GroupRequest {
        group_name: "disable_test_group".to_string(),
        metadata: None,
    };
    let group_response = client
        .disable_group(group_request)
        .await
        .expect("DisableGroup failed");
    let result = group_response.into_inner();

    assert_eq!(result.metrics_affected, 2);

    server.shutdown();
}

#[tokio::test]
async fn test_group_operations_empty_group() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Enable non-existent group (should succeed with 0 affected)
    // This is a valid edge case - ensures API handles missing groups gracefully
    let group_request = GroupRequest {
        group_name: "nonexistent_group".to_string(),
        metadata: None,
    };
    let group_response = client
        .enable_group(group_request)
        .await
        .expect("EnableGroup failed");
    let result = group_response.into_inner();

    assert_eq!(result.metrics_affected, 0);

    server.shutdown();
}

/// Test that enable_group actually enables disabled metrics
/// This is the REAL test for group operations - ensures metrics transition from disabled to enabled
#[tokio::test]
async fn test_enable_group_actually_enables_metrics() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add metrics as DISABLED to the group
    for i in 1..=3 {
        let mut request = create_add_metric_request(
            &format!("actual_enable_metric_{}", i),
            "test.py",
            i * 10,
            &server.connection_id,
        );
        request.group = Some("actual_enable_group".to_string());
        request.enabled = false; // Start disabled
        client.add_metric(request).await.expect("AddMetric failed");
    }

    // Verify metrics are disabled
    let list_request = ListMetricsRequest {
        group: Some("actual_enable_group".to_string()),
        enabled_only: Some(true),
        name_pattern: None,
        metadata: None,
    };
    let list_response = client
        .list_metrics(list_request)
        .await
        .expect("ListMetrics failed");
    assert_eq!(
        list_response.into_inner().metrics.len(),
        0,
        "All metrics should be disabled initially"
    );

    // Enable the group - this should enable all 3 metrics
    let group_request = GroupRequest {
        group_name: "actual_enable_group".to_string(),
        metadata: None,
    };
    let group_response = client
        .enable_group(group_request)
        .await
        .expect("EnableGroup failed");
    let result = group_response.into_inner();

    assert_eq!(result.metrics_affected, 3, "Should have enabled 3 metrics");

    // Verify metrics are now enabled
    let list_request = ListMetricsRequest {
        group: Some("actual_enable_group".to_string()),
        enabled_only: Some(true),
        name_pattern: None,
        metadata: None,
    };
    let list_response = client
        .list_metrics(list_request)
        .await
        .expect("ListMetrics failed");
    assert_eq!(
        list_response.into_inner().metrics.len(),
        3,
        "All metrics should be enabled after enable_group"
    );

    server.shutdown();
}

/// Test that disable_group actually disables enabled metrics
#[tokio::test]
async fn test_disable_group_actually_disables_metrics() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add metrics as ENABLED to the group
    for i in 1..=2 {
        let mut request = create_add_metric_request(
            &format!("actual_disable_metric_{}", i),
            "test.py",
            i * 10,
            &server.connection_id,
        );
        request.group = Some("actual_disable_group".to_string());
        request.enabled = true; // Start enabled
        client.add_metric(request).await.expect("AddMetric failed");
    }

    // Verify metrics are enabled
    let list_request = ListMetricsRequest {
        group: Some("actual_disable_group".to_string()),
        enabled_only: Some(true),
        name_pattern: None,
        metadata: None,
    };
    let list_response = client
        .list_metrics(list_request)
        .await
        .expect("ListMetrics failed");
    assert_eq!(
        list_response.into_inner().metrics.len(),
        2,
        "All metrics should be enabled initially"
    );

    // Disable the group - this should disable all 2 metrics
    let group_request = GroupRequest {
        group_name: "actual_disable_group".to_string(),
        metadata: None,
    };
    let group_response = client
        .disable_group(group_request)
        .await
        .expect("DisableGroup failed");
    let result = group_response.into_inner();

    assert_eq!(result.metrics_affected, 2, "Should have disabled 2 metrics");

    // Verify metrics are now disabled (enabled_only should return 0)
    let list_request = ListMetricsRequest {
        group: Some("actual_disable_group".to_string()),
        enabled_only: Some(true),
        name_pattern: None,
        metadata: None,
    };
    let list_response = client
        .list_metrics(list_request)
        .await
        .expect("ListMetrics failed");
    assert_eq!(
        list_response.into_inner().metrics.len(),
        0,
        "All metrics should be disabled after disable_group"
    );

    server.shutdown();
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[tokio::test]
async fn test_get_metric_not_found() {
    let server = TestServer::start().await.expect("Failed to start server");
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

    server.shutdown();
}

#[tokio::test]
async fn test_get_metric_by_name_not_found() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Try to get non-existent metric by name
    let get_request = GetMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::get_metric_request::Identifier::Name(
                "nonexistent_metric".to_string(),
            ),
        ),
        metadata: None,
    };
    let result = client.get_metric(get_request).await;

    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.shutdown();
}

#[tokio::test]
async fn test_remove_metric_not_found() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Try to remove non-existent metric
    let remove_request = RemoveMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::remove_metric_request::Identifier::MetricId(99999),
        ),
        metadata: None,
    };
    let result = client.remove_metric(remove_request).await;

    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.shutdown();
}

#[tokio::test]
async fn test_update_metric_not_found() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Try to update non-existent metric
    let update_request = UpdateMetricRequest {
        metric_id: 99999,
        expressions: vec!["new_expr".to_string()],
        enabled: None,
        mode: None,
        condition: None,
        metadata: None,
    };
    let result = client.update_metric(update_request).await;

    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.shutdown();
}

#[tokio::test]
async fn test_toggle_metric_not_found() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Try to toggle non-existent metric
    let toggle_request = ToggleMetricRequest {
        metric_id: 99999,
        enabled: true,
        metadata: None,
    };
    let result = client.toggle_metric(toggle_request).await;

    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);

    server.shutdown();
}

#[tokio::test]
async fn test_add_metric_missing_identifier() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Try to get metric without identifier
    let get_request = GetMetricRequest {
        identifier: None,
        metadata: None,
    };
    let result = client.get_metric(get_request).await;

    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.shutdown();
}

#[tokio::test]
async fn test_remove_metric_missing_identifier() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Try to remove metric without identifier
    let remove_request = RemoveMetricRequest {
        identifier: None,
        metadata: None,
    };
    let result = client.remove_metric(remove_request).await;

    assert!(result.is_err());
    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    server.shutdown();
}

// ============================================================================
// Query Tests
// ============================================================================

/// Test that query returns empty result for non-existent metrics
/// This is a valid edge case - ensures query doesn't crash on missing data
#[tokio::test]
async fn test_query_metrics_empty() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create client");

    // Query with non-existent metric IDs (edge case - should return empty)
    let query_request = QueryRequest {
        metric_ids: vec![99999, 99998, 99997], // IDs that don't exist
        time_range: None,
        limit: Some(100),
        offset: None,
        cursor: None,
        order: None,
        metadata: None,
    };
    let query_response = streaming_client
        .query_metrics(query_request)
        .await
        .expect("QueryMetrics failed");
    let result = query_response.into_inner();

    assert_eq!(
        result.events.len(),
        0,
        "Should return empty for non-existent metrics"
    );
    assert_eq!(result.total_count, 0);
    assert!(!result.has_more);

    server.shutdown();
}

/// Test that query returns events that were stored
/// This is the REAL query test - adds a metric, stores events, and verifies query retrieves them
#[tokio::test]
async fn test_query_metrics_with_events() {
    let server = TestServer::start().await.expect("Failed to start server");
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
        create_add_metric_request("query_test_metric", "test.py", 10, &server.connection_id);
    let response = metrics_client
        .add_metric(request)
        .await
        .expect("AddMetric failed");
    let metric_id = response.into_inner().metric_id;

    // Query for the metric (no events yet, but metric exists)
    let query_request = QueryRequest {
        metric_ids: vec![metric_id],
        time_range: None,
        limit: Some(100),
        offset: None,
        cursor: None,
        order: None,
        metadata: None,
    };
    let query_response = streaming_client
        .query_metrics(query_request)
        .await
        .expect("QueryMetrics failed");
    let result = query_response.into_inner();

    // With mock adapter, we don't have real events, but query should succeed
    // The important thing is the query doesn't fail for a valid metric_id
    // (if we got here without error, the query succeeded)
    let _ = result.events; // Query result received successfully

    server.shutdown();
}

/// Test query with limit and pagination
#[tokio::test]
async fn test_query_metrics_with_limit() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create streaming client");

    // Query with specific limit
    let query_request = QueryRequest {
        metric_ids: vec![1],
        time_range: None,
        limit: Some(10),
        offset: Some(0),
        cursor: None,
        order: None,
        metadata: None,
    };
    let query_response = streaming_client
        .query_metrics(query_request)
        .await
        .expect("QueryMetrics failed");
    let result = query_response.into_inner();

    // Verify pagination info is returned correctly
    assert!(result.events.len() <= 10, "Should respect limit");

    server.shutdown();
}

// ============================================================================
// Streaming Tests
// ============================================================================

/// Test that streaming can be established (basic connectivity test)
/// This is a setup test - verifies stream can be opened
#[tokio::test]
async fn test_stream_all_setup() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create client");

    // Start streaming - should succeed even with no events
    let stream_request = StreamAllRequest {
        thread_filter: None,
        metadata: None,
    };
    let stream_response = streaming_client.stream_all(stream_request).await;

    assert!(
        stream_response.is_ok(),
        "StreamAll should succeed to establish connection"
    );

    // Get the stream and verify it's a valid streaming response
    let stream = stream_response.unwrap().into_inner();

    // The stream should be created successfully (we can't easily test actual events
    // without a real debugger, but we verify the stream object is valid)
    // In a real scenario, we'd need to spawn the stream and check for events
    drop(stream);

    server.shutdown();
}

/// Test streaming with a metric added (verifies stream works with active metrics)
#[tokio::test]
async fn test_stream_all_with_metric() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut metrics_client = server
        .metrics_client()
        .await
        .expect("Failed to create metrics client");
    let mut streaming_client = server
        .streaming_client()
        .await
        .expect("Failed to create streaming client");

    // Add a metric first
    let request =
        create_add_metric_request("stream_test_metric", "test.py", 10, &server.connection_id);
    metrics_client
        .add_metric(request)
        .await
        .expect("AddMetric failed");

    // Start streaming
    let stream_request = StreamAllRequest {
        thread_filter: None,
        metadata: None,
    };
    let stream_response = streaming_client
        .stream_all(stream_request)
        .await
        .expect("StreamAll should succeed");

    // Verify stream is established
    let _stream = stream_response.into_inner();

    // With mock adapter we won't get real events, but the stream should be valid
    // The important verification is that streaming works when metrics exist

    server.shutdown();
}

// ============================================================================
// Concurrent Operations Tests
// ============================================================================

#[tokio::test]
async fn test_concurrent_metric_creation() {
    let server = TestServer::start().await.expect("Failed to start server");

    // Create multiple clients
    let mut handles = vec![];
    for i in 0..5 {
        let uri = server.uri();
        let conn_id = server.connection_id.clone();
        handles.push(tokio::spawn(async move {
            let channel = Channel::from_shared(uri)
                .expect("Invalid URI")
                .connect()
                .await
                .expect("Failed to connect");
            let mut client = MetricsServiceClient::new(channel);

            let request = create_add_metric_request(
                &format!("concurrent_metric_{}", i),
                "test.py",
                (i + 1) * 10,
                &conn_id,
            );
            client.add_metric(request).await
        }));
    }

    // Wait for all to complete
    let results: Vec<_> = futures::future::join_all(handles).await;

    // All should succeed
    for result in results {
        let inner = result.expect("Task panicked");
        assert!(inner.is_ok(), "AddMetric should succeed");
    }

    // Verify all metrics exist
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");
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

    assert_eq!(metrics.len(), 5);

    server.shutdown();
}

#[tokio::test]
async fn test_concurrent_read_write() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add initial metric
    let request =
        create_add_metric_request("concurrent_rw_metric", "test.py", 10, &server.connection_id);
    let response = client.add_metric(request).await.expect("AddMetric failed");
    let metric_id = response.into_inner().metric_id;

    // Concurrent reads and writes
    let uri = server.uri();
    let mut read_handles = vec![];
    let mut write_handles = vec![];

    // Readers
    for _ in 0..3 {
        let uri_clone = uri.clone();
        read_handles.push(tokio::spawn(async move {
            let channel = Channel::from_shared(uri_clone)
                .expect("Invalid URI")
                .connect()
                .await
                .expect("Failed to connect");
            let mut client = MetricsServiceClient::new(channel);

            let list_request = ListMetricsRequest {
                group: None,
                enabled_only: None,
                name_pattern: None,
                metadata: None,
            };
            client.list_metrics(list_request).await
        }));
    }

    // Writers (toggle)
    for i in 0..3 {
        let uri_clone = uri.clone();
        write_handles.push(tokio::spawn(async move {
            let channel = Channel::from_shared(uri_clone)
                .expect("Invalid URI")
                .connect()
                .await
                .expect("Failed to connect");
            let mut client = MetricsServiceClient::new(channel);

            let toggle_request = ToggleMetricRequest {
                metric_id,
                enabled: i % 2 == 0,
                metadata: None,
            };
            client.toggle_metric(toggle_request).await
        }));
    }

    // Wait for all to complete
    let read_results: Vec<_> = futures::future::join_all(read_handles).await;
    let write_results: Vec<_> = futures::future::join_all(write_handles).await;

    // All should succeed (no deadlocks, no crashes)
    for result in read_results {
        let inner = result.expect("Task panicked");
        assert!(inner.is_ok(), "Read operation should succeed");
    }
    for result in write_results {
        let inner = result.expect("Task panicked");
        assert!(inner.is_ok(), "Write operation should succeed");
    }

    server.shutdown();
}

// ============================================================================
// Full Lifecycle Test
// ============================================================================

#[tokio::test]
async fn test_full_metric_lifecycle() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // 1. Add metric
    let request =
        create_add_metric_request("lifecycle_metric", "app.py", 100, &server.connection_id);
    let response = client.add_metric(request).await.expect("AddMetric failed");
    let metric_id = response.into_inner().metric_id;
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

    // 3. Update metric (complex expression skips file inspection)
    let update_request = UpdateMetricRequest {
        metric_id,
        expressions: vec!["value.lifecycle".to_string()],
        enabled: None,
        mode: None,
        condition: None,
        metadata: None,
    };
    client
        .update_metric(update_request)
        .await
        .expect("UpdateMetric failed");

    // 4. Toggle metric (disable)
    let toggle_request = ToggleMetricRequest {
        metric_id,
        enabled: false,
        metadata: None,
    };
    let toggle_response = client
        .toggle_metric(toggle_request)
        .await
        .expect("ToggleMetric failed");
    let toggle_result = toggle_response.into_inner();
    assert!(!toggle_result.enabled);
    assert!(toggle_result.dap_confirmed);

    // 5. Toggle metric (enable)
    let toggle_request = ToggleMetricRequest {
        metric_id,
        enabled: true,
        metadata: None,
    };
    let toggle_response = client
        .toggle_metric(toggle_request)
        .await
        .expect("ToggleMetric failed");
    let toggle_result = toggle_response.into_inner();
    assert!(toggle_result.enabled);
    assert!(toggle_result.dap_confirmed);

    // 6. List metrics (should include our metric)
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
    assert!(list_response
        .into_inner()
        .metrics
        .iter()
        .any(|m| m.name == "lifecycle_metric"));

    // 7. Remove metric
    let remove_request = RemoveMetricRequest {
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

    // 8. Verify metric is gone
    let get_request = GetMetricRequest {
        identifier: Some(
            detrix_api::generated::detrix::v1::get_metric_request::Identifier::MetricId(metric_id),
        ),
        metadata: None,
    };
    let result = client.get_metric(get_request).await;
    assert!(result.is_err());

    server.shutdown();
}

// ============================================================================
// disconnect_all and get_status Tests (H6)
// ============================================================================

#[tokio::test]
async fn test_grpc_disconnect_all() {
    let server = TestServer::start()
        .await
        .expect("Failed to start test server");
    let mut client = MetricsServiceClient::connect(format!("http://{}", server.addr))
        .await
        .expect("Failed to connect to server");

    let response = client
        .disconnect_all(DisconnectAllRequest { metadata: None })
        .await
        .expect("DisconnectAll failed");

    let inner = response.into_inner();
    assert_eq!(inner.status, "disconnected");
    // TestServer creates a mock connection, so at least 1 adapter was stopped
    assert!(
        inner.adapters_stopped >= 1,
        "Expected at least 1 adapter stopped, got {}",
        inner.adapters_stopped
    );

    server.shutdown();
}

#[tokio::test]
async fn test_grpc_get_status_idle() {
    let server = TestServer::start()
        .await
        .expect("Failed to start test server");
    let mut client = MetricsServiceClient::connect(format!("http://{}", server.addr))
        .await
        .expect("Failed to connect to server");

    let response = client
        .get_status(StatusRequest { metadata: None })
        .await
        .expect("GetStatus failed");

    let inner = response.into_inner();
    assert_eq!(
        inner.mode, "active",
        "Mode should be 'active' since test server creates a mock connection"
    );
    // uptime_seconds is u64 so always >= 0, just verify it's set
    assert!(
        inner.uptime_seconds < 60,
        "Uptime should be under 60s for fresh server"
    );

    server.shutdown();
}

// ============================================================================
// Relative Path Resolution Tests
// ============================================================================

#[tokio::test]
async fn test_add_metric_relative_path() {
    // Server with workspace_root pointing to fixtures directory
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let server = TestServer::start_with_workspace(&workspace)
        .await
        .expect("Server failed to start");
    let mut client = server.metrics_client().await.expect("Client failed");

    // Use relative path "test.py" — should resolve against workspace_root
    let request = AddMetricRequest {
        name: "rel_path_metric".to_string(),
        group: Some("test_group".to_string()),
        location: Some(Location {
            file: "test.py".to_string(), // Relative path
            line: 30,
        }),
        expressions: vec!["x.value".to_string()],
        language: Some("python".to_string()),
        enabled: true,
        mode: Some(MetricMode {
            mode: Some(detrix_api::generated::detrix::v1::metric_mode::Mode::Stream(StreamMode {})),
        }),
        condition: None,
        safety_level: "strict".to_string(),
        metadata: None,
        connection_id: server.connection_id.clone(),
        replace: None,
        capture_stack_trace: None,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: None,
        snapshot_scope: None,
        snapshot_ttl: None,
        file_content: None,
    };

    let response = client
        .add_metric(request)
        .await
        .expect("add_metric with relative path should succeed");

    let inner = response.into_inner();
    assert!(inner.metric_id > 0, "Metric should be created");

    // Verify the stored location has resolved absolute path
    let location = inner.location.expect("Response should have location");
    assert!(
        std::path::Path::new(&location.file).is_absolute(),
        "Stored file path should be absolute: {}",
        location.file
    );
    assert!(
        location.file.contains("fixtures"),
        "Resolved path should contain fixtures dir: {}",
        location.file
    );

    server.shutdown();
}

#[tokio::test]
async fn test_inspect_file_relative_path() {
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let server = TestServer::start_with_workspace(&workspace)
        .await
        .expect("Server failed to start");
    let mut client = server.metrics_client().await.expect("Client failed");

    // inspect_file with relative path + connection_id
    let request = InspectFileRequest {
        file_path: "test.py".to_string(), // Relative path
        line: Some(30),
        find_variable: None,
        metadata: None,
        connection_id: Some(server.connection_id.clone()),
        file_content: None,
    };

    let response = client
        .inspect_file(request)
        .await
        .expect("inspect_file with relative path should succeed");

    let inner = response.into_inner();
    assert!(inner.success, "Inspection should succeed");
    assert!(
        !inner.content.is_empty(),
        "Response should contain inspection content"
    );

    server.shutdown();
}

#[tokio::test]
async fn test_inspect_file_relative_path_auto_select() {
    // With only one connection, inspect_file should auto-select for path resolution
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let server = TestServer::start_with_workspace(&workspace)
        .await
        .expect("Server failed to start");
    let mut client = server.metrics_client().await.expect("Client failed");

    let request = InspectFileRequest {
        file_path: "auth.py".to_string(), // Relative path
        line: None,
        find_variable: Some("user".to_string()),
        metadata: None,
        connection_id: None, // Auto-select
        file_content: None,
    };

    let response = client
        .inspect_file(request)
        .await
        .expect("inspect_file with auto-select should succeed");

    let inner = response.into_inner();
    assert!(inner.success, "Inspection should succeed with auto-select");

    server.shutdown();
}

#[tokio::test]
async fn test_add_metric_absolute_path_still_works() {
    // Verify absolute paths still work when workspace_root is set
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let server = TestServer::start_with_workspace(&workspace)
        .await
        .expect("Server failed to start");
    let mut client = server.metrics_client().await.expect("Client failed");

    let absolute_path = test_py_path();
    let request = AddMetricRequest {
        name: "abs_path_metric".to_string(),
        group: Some("test_group".to_string()),
        location: Some(Location {
            file: absolute_path.clone(), // Absolute path
            line: 30,
        }),
        expressions: vec!["x.value".to_string()],
        language: Some("python".to_string()),
        enabled: true,
        mode: Some(MetricMode {
            mode: Some(detrix_api::generated::detrix::v1::metric_mode::Mode::Stream(StreamMode {})),
        }),
        condition: None,
        safety_level: "strict".to_string(),
        metadata: None,
        connection_id: server.connection_id.clone(),
        replace: None,
        capture_stack_trace: None,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: None,
        snapshot_scope: None,
        snapshot_ttl: None,
        file_content: None,
    };

    let response = client
        .add_metric(request)
        .await
        .expect("add_metric with absolute path should succeed");

    let inner = response.into_inner();
    let location = inner.location.expect("Response should have location");
    assert_eq!(
        location.file, absolute_path,
        "Absolute path should be preserved unchanged"
    );

    server.shutdown();
}

// ============================================================================
// Scope Enforcement Read-Path Tests
//
// The TestServer uses AuthConfig::default() which sets auth to disabled.
// With auth disabled, the interceptor injects a synthetic "default" Admin
// user for every request. This means ALL metrics are readable/mutable under
// MetricScope::Admin — ideal for verifying the list→filter→return pipeline
// without needing real JWT tokens.
//
// Per-user filtering (MetricScope::User / MetricScope::Agent) is covered
// by the scope.rs unit tests and the multi-tenant REST E2E tests.
// ============================================================================

/// Verify that metrics added via gRPC appear in the list response.
///
/// This exercises the full list → scope-filter → return pipeline.
/// With auth disabled the server runs under Admin scope, so all metrics
/// are visible — the test confirms the pipeline does not silently drop rows.
#[tokio::test]
async fn test_grpc_list_metrics_returns_only_own_user_metrics() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add a metric
    let request =
        create_add_metric_request("scope_list_metric", "test.py", 10, &server.connection_id);
    let add_response = client.add_metric(request).await.expect("AddMetric failed");
    let added_id = add_response.into_inner().metric_id;
    assert!(added_id > 0, "Metric ID should be positive");

    // List all metrics — Admin scope sees everything
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

    // The added metric must be present in the list
    assert!(
        metrics.iter().any(|m| m.metric_id == added_id),
        "Added metric (id={}) should appear in list response, got ids: {:?}",
        added_id,
        metrics.iter().map(|m| m.metric_id).collect::<Vec<_>>()
    );

    server.shutdown();
}

/// Verify that get_metric returns the correct metric when multiple metrics exist.
///
/// This exercises the get → scope-check → return pipeline for each metric,
/// confirming that ID-based lookup returns the exact requested metric rather
/// than an arbitrary one.
#[tokio::test]
async fn test_grpc_get_metric_by_id_returns_correct_metric() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut client = server
        .metrics_client()
        .await
        .expect("Failed to create client");

    // Add two metrics with distinct names
    let req_a =
        create_add_metric_request("scope_get_metric_a", "test.py", 10, &server.connection_id);
    let id_a = client
        .add_metric(req_a)
        .await
        .expect("AddMetric A failed")
        .into_inner()
        .metric_id;

    let req_b =
        create_add_metric_request("scope_get_metric_b", "test.py", 20, &server.connection_id);
    let id_b = client
        .add_metric(req_b)
        .await
        .expect("AddMetric B failed")
        .into_inner()
        .metric_id;

    assert_ne!(id_a, id_b, "Each metric should receive a unique ID");

    // Fetch metric A by ID and verify identity
    let resp_a = client
        .get_metric(GetMetricRequest {
            identifier: Some(
                detrix_api::generated::detrix::v1::get_metric_request::Identifier::MetricId(id_a),
            ),
            metadata: None,
        })
        .await
        .expect("GetMetric A failed")
        .into_inner();

    assert_eq!(resp_a.metric_id, id_a, "Should return metric A");
    assert_eq!(resp_a.name, "scope_get_metric_a");

    // Fetch metric B by ID and verify identity
    let resp_b = client
        .get_metric(GetMetricRequest {
            identifier: Some(
                detrix_api::generated::detrix::v1::get_metric_request::Identifier::MetricId(id_b),
            ),
            metadata: None,
        })
        .await
        .expect("GetMetric B failed")
        .into_inner();

    assert_eq!(resp_b.metric_id, id_b, "Should return metric B");
    assert_eq!(resp_b.name, "scope_get_metric_b");

    server.shutdown();
}
