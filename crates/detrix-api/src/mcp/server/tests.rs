use super::*;
use crate::ApiState;
use detrix_application::{
    AppContext, ConnectionReferenceRepositoryRef, ConnectionRepositoryRef, DapAdapterFactoryRef,
    EventRepositoryRef, MetricRepositoryRef,
};
use detrix_config::ApiConfig;
use detrix_core::ConnectionId;
use detrix_storage::{SqliteConfig, SqliteStorage};
use detrix_testing::fixtures::{auth_py_path, fixtures_dir, test_py_path};
use detrix_testing::MockDapAdapterFactory;
use rmcp::handler::server::wrapper::Parameters;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

/// Test fixture with access to private methods
struct TestFixture {
    _temp_dir: TempDir,
    server: DetrixServer,
    state: Arc<ApiState>,
}

impl TestFixture {
    async fn new() -> Self {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let sqlite_config = SqliteConfig {
            path: db_path,
            pool_size: 1,
            busy_timeout_ms: 5000,
        };

        let storage = Arc::new(SqliteStorage::new(&sqlite_config).await.unwrap());
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
            None, // No separate DLQ storage in tests
            None,
            None, // No auth token in tests
            vfs,
            file_source_chain,
            Arc::clone(&storage) as ConnectionReferenceRepositoryRef,
            HashMap::new(),
        None, // agent_config
        );

        let state = Arc::new(ApiState::builder(context, storage).build());
        let server = DetrixServer::new(Arc::clone(&state));

        Self {
            _temp_dir: temp_dir,
            server,
            state,
        }
    }

    /// Create a mock connection for tests that require a debugger connection
    async fn with_mock_connection(&self) -> ConnectionId {
        self.with_mock_connection_with_workspace("/test-workspace")
            .await
    }

    /// Create a mock connection with a custom workspace_root
    ///
    /// Use `fixtures_dir().to_string_lossy()` as workspace to test relative path resolution
    /// against real fixture files.
    async fn with_mock_connection_with_workspace(&self, workspace_root: &str) -> ConnectionId {
        // Use the connection_service to create a connection (saves to DB and starts adapter)
        // MockDapAdapterFactory will create a MockDapAdapter
        let identity = detrix_core::ConnectionIdentity::new(
            "test",
            detrix_core::SourceLanguage::Python,
            workspace_root,
            "test-host",
        );
        self.state
            .context
            .connection_service
            .create_connection(
                "127.0.0.1".to_string(),
                5678,
                identity,
                None,  // No program path for Python
                None,  // No PID (not using AttachPid mode)
                false, // SafeMode disabled for tests
            )
            .await
            .expect("Failed to create mock connection")
    }
}

// ------------------------------------------------------------------------
// Tool Routing / Handler Tests
// ------------------------------------------------------------------------

#[tokio::test]
async fn test_add_metric_success() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await; // Need connection for add_metric

    let params = AddMetricParams {
        name: "test_metric".to_string(),
        location: format!("@{}#127", auth_py_path()),
        line: None,
        expressions: vec!["user.id".to_string()],
        connection_id: conn_id.to_string(),
        group: Some("auth".to_string()),
        enabled: true,
        mode: None,
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "add_metric should succeed: {:?}",
        result.err()
    );

    let result = result.unwrap();
    assert!(result.is_error.is_none() || !result.is_error.unwrap());
}

#[tokio::test]
async fn test_add_metric_multi_expressions() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "multi_expr_metric".to_string(),
        location: format!("@{}#10", test_py_path()),
        line: None,
        expressions: vec![
            "user.id".to_string(),
            "user.name".to_string(),
            "user.email".to_string(),
        ],
        connection_id: conn_id.to_string(),
        group: Some("multi_test".to_string()),
        enabled: true,
        mode: None,
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "add_metric with multiple expressions should succeed: {:?}",
        result.err()
    );

    // Verify the metric was stored with all 3 expressions
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();

    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].name, "multi_expr_metric");
    assert_eq!(metrics[0].expressions.len(), 3);
    assert_eq!(metrics[0].expressions[0], "user.id");
    assert_eq!(metrics[0].expressions[1], "user.name");
    assert_eq!(metrics[0].expressions[2], "user.email");
}

#[tokio::test]
async fn test_add_metric_location_without_at_works() {
    // @ prefix is now optional - both formats should work
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "no_at_metric".to_string(),
        location: format!("{}#127", auth_py_path()), // No @ prefix - should work now
        line: None,
        expressions: vec!["user.id".to_string()],
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: None,
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "Location without @ should work: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_add_metric_invalid_location_no_line() {
    let fixture = TestFixture::new().await;

    let params = AddMetricParams {
        name: "bad_metric".to_string(),
        location: "@auth.py".to_string(), // Missing :line and no line parameter
        line: None,
        expressions: vec!["user.id".to_string()],
        connection_id: "test-conn".to_string(), // Placeholder, validation fails before connection check
        group: None,
        enabled: true,
        mode: None,
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_err(), "Should fail with missing line number");

    let err = result.unwrap_err();
    assert!(
        err.message.contains("Could not determine line"),
        "Expected 'Could not determine line' error, got: {}",
        err.message
    );
}

#[tokio::test]
async fn test_add_metric_with_separate_line_param() {
    // Test using separate line parameter instead of embedding in location
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "separate_line_metric".to_string(),
        location: auth_py_path(), // Just file, no :line
        line: Some(127),          // Line provided separately
        expressions: vec!["user.id".to_string()],
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: None,
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "Separate line parameter should work: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_add_metric_invalid_line_number() {
    let fixture = TestFixture::new().await;

    let params = AddMetricParams {
        name: "bad_metric".to_string(),
        location: "@auth.py#abc".to_string(), // Invalid line number (non-numeric)
        line: None,
        expressions: vec!["user.id".to_string()],
        connection_id: "test-conn".to_string(), // Placeholder, validation fails before connection check
        group: None,
        enabled: true,
        mode: None,
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_err(), "Should fail with invalid line number");

    let err = result.unwrap_err();
    // With flexible parsing, "abc" after : is not a number, so it falls through to "no line"
    assert!(
        err.message.contains("Could not determine line"),
        "Expected 'Could not determine line' error, got: {}",
        err.message
    );
}

// ------------------------------------------------------------------------
// Schema / Parameter Contract Tests
// ------------------------------------------------------------------------

#[tokio::test]
async fn test_mode_string_stream() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "stream_metric".to_string(),
        location: format!("@{}#30", test_py_path()),
        line: None,
        expressions: vec!["x.value".to_string()], // Use complex expression to skip scope validation
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: Some("stream".to_string()),
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_ok(), "Failed: {:?}", result);
}

#[tokio::test]
async fn test_mode_string_sample() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "sample_metric".to_string(),
        location: format!("@{}#30", test_py_path()),
        line: None,
        expressions: vec!["x.value".to_string()], // Use complex expression to skip scope validation
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: Some("sample".to_string()),
        sample_rate: Some(50), // 50%
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_mode_string_first() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "first_metric".to_string(),
        location: format!("@{}#30", test_py_path()),
        line: None,
        expressions: vec!["x.value".to_string()], // Use complex expression to skip scope validation
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: Some("first".to_string()),
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_mode_string_throttle() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "throttle_metric".to_string(),
        location: format!("@{}#30", test_py_path()),
        line: None,
        expressions: vec!["x.value".to_string()], // Use complex expression to skip scope validation
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: Some("throttle".to_string()),
        sample_rate: None,
        sample_interval_seconds: None,
        max_per_second: Some(10),
        capture_stack_trace: None,
        stack_trace_ttl: None,
        stack_trace_full: None,
        stack_trace_head: None,
        stack_trace_tail: None,
        capture_memory_snapshot: None,
        snapshot_scope: None,
        snapshot_ttl: None,
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_snapshot_scope_local() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "local_snapshot".to_string(),
        location: format!("@{}#30", test_py_path()),
        line: None,
        expressions: vec!["x.value".to_string()], // Use complex expression to skip scope validation
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: None,
        sample_rate: None,
        sample_interval_seconds: None,
        max_per_second: None,
        capture_stack_trace: None,
        stack_trace_ttl: None,
        stack_trace_full: None,
        stack_trace_head: None,
        stack_trace_tail: None,
        capture_memory_snapshot: Some(true),
        snapshot_scope: Some("local".to_string()),
        snapshot_ttl: None,
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_snapshot_scope_global() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "global_snapshot".to_string(),
        location: format!("@{}#30", test_py_path()),
        line: None,
        expressions: vec!["x.value".to_string()], // Use complex expression to skip scope validation
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: None,
        sample_rate: None,
        sample_interval_seconds: None,
        max_per_second: None,
        capture_stack_trace: None,
        stack_trace_ttl: None,
        stack_trace_full: None,
        stack_trace_head: None,
        stack_trace_tail: None,
        capture_memory_snapshot: Some(true),
        snapshot_scope: Some("global".to_string()),
        snapshot_ttl: None,
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_snapshot_scope_both() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "both_snapshot".to_string(),
        location: format!("@{}#30", test_py_path()),
        line: None,
        expressions: vec!["x.value".to_string()], // Use complex expression to skip scope validation
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: None,
        sample_rate: None,
        sample_interval_seconds: None,
        max_per_second: None,
        capture_stack_trace: None,
        stack_trace_ttl: None,
        stack_trace_full: None,
        stack_trace_head: None,
        stack_trace_tail: None,
        capture_memory_snapshot: Some(true),
        snapshot_scope: Some("both".to_string()),
        snapshot_ttl: None,
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_stack_trace_options() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "stack_trace_metric".to_string(),
        location: format!("@{}#30", test_py_path()),
        line: None,
        expressions: vec!["x.value".to_string()], // Use complex expression to skip scope validation
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: None,
        sample_rate: None,
        sample_interval_seconds: None,
        max_per_second: None,
        capture_stack_trace: Some(true),
        stack_trace_ttl: Some(300),
        stack_trace_full: None,
        stack_trace_head: Some(5),
        stack_trace_tail: Some(3),
        capture_memory_snapshot: None,
        snapshot_scope: None,
        snapshot_ttl: None,
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_validate_expression_safe() {
    let fixture = TestFixture::new().await;

    let params = ValidateExpressionParams {
        expression: "user.id".to_string(),
        language: "python".to_string(),
    };

    let result = fixture.server.validate_expression(Parameters(params)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_get_status() {
    let fixture = TestFixture::new().await;

    let status_result = fixture.server.get_status().await;
    assert!(status_result.is_ok());
}

#[tokio::test]
async fn test_toggle_metric_not_found() {
    let fixture = TestFixture::new().await;

    let params = ToggleMetricParams {
        name: "nonexistent_metric".to_string(),
        enabled: false,
    };

    let result = fixture.server.toggle_metric(Parameters(params)).await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(err.message.contains("not found"));
}

#[tokio::test]
async fn test_remove_metric_not_found() {
    let fixture = TestFixture::new().await;

    let params = RemoveMetricParams {
        name: "nonexistent".to_string(),
    };

    let result = fixture.server.remove_metric(Parameters(params)).await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(err.message.contains("not found"));
}

#[tokio::test]
async fn test_list_metrics_empty() {
    let fixture = TestFixture::new().await;

    let params = ListMetricsParams {
        group: None,
        enabled_only: None,
        format: None,
    };

    let result = fixture.server.list_metrics(Parameters(params)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_query_metrics_requires_name_or_group() {
    let fixture = TestFixture::new().await;

    let params = QueryMetricsParams {
        name: None,
        group: None,
        limit: None,
        format: None,
    };

    let result = fixture.server.query_metrics(Parameters(params)).await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(err
        .message
        .contains("Either 'name' or 'group' must be specified"));
}

// ------------------------------------------------------------------------
// Observe Tool Tests
// ------------------------------------------------------------------------

#[tokio::test]
async fn test_observe_with_explicit_line() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = ObserveParams {
        file: auth_py_path(),
        expressions: vec!["user.id".to_string()],
        line: Some(127), // Explicit line
        connection_id: Some(conn_id.to_string()),
        name: Some("observe_test".to_string()),
        find_variable: None,
        group: None,
        capture_stack_trace: None,
        capture_memory_snapshot: None,
        ttl_seconds: None,
    };

    let result = fixture.server.observe(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "observe should succeed with explicit line: {:?}",
        result.err()
    );

    let result = result.unwrap();
    assert!(result.is_error.is_none() || !result.is_error.unwrap());
}

#[tokio::test]
async fn test_observe_auto_connection_selection() {
    let fixture = TestFixture::new().await;
    // Create only one connection - should be auto-selected
    let _conn_id = fixture.with_mock_connection().await;

    let params = ObserveParams {
        file: auth_py_path(),
        expressions: vec!["user.id".to_string()],
        line: Some(127),
        connection_id: None, // Not specified - should auto-select
        name: Some("auto_conn_test".to_string()),
        find_variable: None,
        group: None,
        capture_stack_trace: None,
        capture_memory_snapshot: None,
        ttl_seconds: None,
    };

    let result = fixture.server.observe(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "observe should auto-select single connection: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_observe_no_connection_error() {
    let fixture = TestFixture::new().await;
    // No connection created

    let params = ObserveParams {
        file: auth_py_path(),
        expressions: vec!["user.id".to_string()],
        line: Some(127),
        connection_id: None,
        name: Some("no_conn_test".to_string()),
        find_variable: None,
        group: None,
        capture_stack_trace: None,
        capture_memory_snapshot: None,
        ttl_seconds: None,
    };

    let result = fixture.server.observe(Parameters(params)).await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(
        err.message.contains("No connections available"),
        "Expected 'No connections available', got: {}",
        err.message
    );
}

#[tokio::test]
async fn test_observe_with_introspection() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = ObserveParams {
        file: auth_py_path(),
        expressions: vec!["user.id".to_string()],
        line: Some(127),
        connection_id: Some(conn_id.to_string()),
        name: Some("introspect_test".to_string()),
        find_variable: None,
        group: None,
        capture_stack_trace: Some(true),
        capture_memory_snapshot: Some(true),
        ttl_seconds: Some(300),
    };

    let result = fixture.server.observe(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "observe should succeed with introspection: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_observe_generates_metric_name() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = ObserveParams {
        file: auth_py_path(),
        expressions: vec!["user.id".to_string()],
        line: Some(127),
        connection_id: Some(conn_id.to_string()),
        name: None, // No name - should be auto-generated
        find_variable: None,
        group: None,
        capture_stack_trace: None,
        capture_memory_snapshot: None,
        ttl_seconds: None,
    };

    let result = fixture.server.observe(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "observe should generate metric name: {:?}",
        result.err()
    );

    // Verify metric was created with auto-generated name
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    // Name format: {filename}_{line} (e.g., "auth_py_127")
    assert_eq!(
        metrics[0].name, "auth_py_127",
        "Expected name 'auth_py_127' (filename_line format), got: {}",
        metrics[0].name
    );
}

// ------------------------------------------------------------------------
// Enable From Diff Tool Tests
// ------------------------------------------------------------------------

#[tokio::test]
async fn test_enable_from_diff_empty_diff() {
    let fixture = TestFixture::new().await;

    let params = EnableFromDiffParams {
        diff: "".to_string(),
        connection_id: None,
        group: None,
        ttl_seconds: None,
    };

    let result = fixture.server.enable_from_diff(Parameters(params)).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("No diff content provided"));
}

#[tokio::test]
async fn test_enable_from_diff_no_debug_statements() {
    let fixture = TestFixture::new().await;
    let _conn_id = fixture.with_mock_connection().await;

    let diff = r#"
diff --git a/file.py b/file.py
@@ -10,6 +10,7 @@
 x = 1
+    y = 2
 return
"#;

    let params = EnableFromDiffParams {
        diff: diff.to_string(),
        connection_id: None,
        group: None,
        ttl_seconds: None,
    };

    let result = fixture.server.enable_from_diff(Parameters(params)).await;
    assert!(result.is_ok());
    let result = result.unwrap();
    // Should succeed - when no debug statements are found, it returns success
    // but the result is_error should be None or false
    assert!(result.is_error.is_none() || !result.is_error.unwrap());
}

#[tokio::test]
async fn test_enable_from_diff_python_success() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    // Use the actual test file path
    let file_path = auth_py_path();
    let diff = format!(
        r#"
diff --git a/{file} b/{file}
@@ -125,6 +125,7 @@
 user = get_user()
+    print(f"user_id={{user.id}}")
 return user
"#,
        file = file_path
    );

    let params = EnableFromDiffParams {
        diff,
        connection_id: Some(conn_id.to_string()),
        group: Some("test_group".to_string()),
        ttl_seconds: None,
    };

    let result = fixture.server.enable_from_diff(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "enable_from_diff should succeed: {:?}",
        result.err()
    );

    // Verify metric was created
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].expression(), "user.id");
    assert_eq!(metrics[0].group, Some("test_group".to_string()));
}

#[tokio::test]
async fn test_enable_from_diff_no_connection() {
    let fixture = TestFixture::new().await;
    // Don't create any connection

    let diff = r#"
diff --git a/file.py b/file.py
@@ -10,6 +10,7 @@
 x = 1
+    print(f"x={x}")
 return
"#;

    let params = EnableFromDiffParams {
        diff: diff.to_string(),
        connection_id: None,
        group: None,
        ttl_seconds: None,
    };

    let result = fixture.server.enable_from_diff(Parameters(params)).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.message.contains("No connections available"),
        "Expected 'no connections' error, got: {}",
        err.message
    );
}

#[tokio::test]
async fn test_enable_from_diff_multiple_expressions() {
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let file_path = auth_py_path();
    let diff = format!(
        r#"
diff --git a/{file} b/{file}
@@ -125,6 +125,8 @@
 user = get_user()
+    print(f"name={{user.name}}")
+    print(f"email={{user.email}}")
 return user
"#,
        file = file_path
    );

    let params = EnableFromDiffParams {
        diff,
        connection_id: Some(conn_id.to_string()),
        group: None,
        ttl_seconds: None,
    };

    let result = fixture.server.enable_from_diff(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "enable_from_diff should succeed: {:?}",
        result.err()
    );

    // Verify both metrics were created
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 2);

    let expressions: Vec<&str> = metrics.iter().map(|m| m.expression()).collect();
    assert!(
        expressions.contains(&"user.name"),
        "Expected 'user.name' expression"
    );
    assert!(
        expressions.contains(&"user.email"),
        "Expected 'user.email' expression"
    );
}

// ============================================================================
// disconnect_all and get_status Tests (H6)
// ============================================================================

#[tokio::test]
async fn test_disconnect_all_tool() {
    let fixture = TestFixture::new().await;

    let params = crate::mcp::params::DisconnectAllParams {};
    let result = fixture.server.disconnect_all(Parameters(params)).await;

    assert!(
        result.is_ok(),
        "disconnect_all should succeed: {:?}",
        result.err()
    );

    let tool_result = result.unwrap();
    // Should contain a message about adapters stopped
    let text = tool_result
        .content
        .iter()
        .filter_map(|c| match &c.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("disconnected") || text.contains("adapters_stopped"),
        "Response should mention disconnect status, got: {}",
        text
    );
}

#[tokio::test]
async fn test_get_status_idle_mode() {
    let fixture = TestFixture::new().await;

    let result = fixture.server.get_status().await;
    assert!(
        result.is_ok(),
        "get_status should succeed: {:?}",
        result.err()
    );

    let tool_result = result.unwrap();
    let text = tool_result
        .content
        .iter()
        .filter_map(|c| match &c.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Verify "idle" mode (not "sleeping") and key fields present
    assert!(
        text.contains("\"mode\"") || text.contains("mode"),
        "Response should contain mode field, got: {}",
        text
    );
    assert!(
        !text.contains("sleeping"),
        "Status should not contain 'sleeping' for daemon status, got: {}",
        text
    );
    // Verify comprehensive status fields are present
    assert!(
        text.contains("uptime") || text.contains("uptime_seconds"),
        "Response should contain uptime, got: {}",
        text
    );
    assert!(
        text.contains("enabled_metrics") || text.contains("total_metrics"),
        "Response should contain metrics info, got: {}",
        text
    );
}

// ============================================================================
// Relative Path Resolution Tests
// ============================================================================

#[tokio::test]
async fn test_add_metric_relative_path() {
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    // Use relative path "auth.py" — should resolve to {fixtures_dir}/auth.py
    let params = AddMetricParams {
        name: "rel_path_metric".to_string(),
        location: "auth.py#127".to_string(), // Relative path
        line: None,
        expressions: vec!["user.id".to_string()],
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: None,
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "add_metric with relative path should succeed: {:?}",
        result.err()
    );

    // Verify the stored metric has the resolved absolute path
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert!(
        metrics[0].location.file.contains("fixtures"),
        "Stored file path should be resolved to absolute: {}",
        metrics[0].location.file
    );
    assert!(
        std::path::Path::new(&metrics[0].location.file).is_absolute(),
        "Stored file path should be absolute: {}",
        metrics[0].location.file
    );
}

#[tokio::test]
async fn test_add_metric_relative_path_with_separate_line() {
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    // Use relative path without line in location, line provided separately
    let params = AddMetricParams {
        name: "rel_path_sep_line".to_string(),
        location: "test.py".to_string(), // Relative path, no line
        line: Some(30),                  // Line provided separately
        expressions: vec!["x.value".to_string()],
        connection_id: conn_id.to_string(),
        group: None,
        enabled: true,
        mode: None,
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
        replace: None,
    };

    let result = fixture.server.add_metric(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "add_metric with relative path + separate line should succeed: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_observe_relative_path() {
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    let params = ObserveParams {
        file: "auth.py".to_string(), // Relative path
        expressions: vec!["user.id".to_string()],
        line: Some(127),
        connection_id: Some(conn_id.to_string()),
        name: Some("observe_rel".to_string()),
        find_variable: None,
        group: None,
        capture_stack_trace: None,
        capture_memory_snapshot: None,
        ttl_seconds: None,
    };

    let result = fixture.server.observe(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "observe with relative path should succeed: {:?}",
        result.err()
    );

    // Verify the stored metric has resolved absolute path
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert!(
        std::path::Path::new(&metrics[0].location.file).is_absolute(),
        "Stored file path should be absolute: {}",
        metrics[0].location.file
    );
}

#[tokio::test]
async fn test_observe_relative_path_auto_connection() {
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    // Create single connection — should be auto-selected
    let _conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    let params = ObserveParams {
        file: "auth.py".to_string(), // Relative path
        expressions: vec!["user.id".to_string()],
        line: Some(127),
        connection_id: None, // Auto-select single connection
        name: Some("observe_rel_auto".to_string()),
        find_variable: None,
        group: None,
        capture_stack_trace: None,
        capture_memory_snapshot: None,
        ttl_seconds: None,
    };

    let result = fixture.server.observe(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "observe with relative path + auto-connection should succeed: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_observe_filename_only() {
    // Test with just a filename (no directory prefix), resolved via workspace_root
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    let params = ObserveParams {
        file: "test.py".to_string(), // Just filename
        expressions: vec!["x.value".to_string()],
        line: Some(30),
        connection_id: Some(conn_id.to_string()),
        name: Some("observe_filename".to_string()),
        find_variable: None,
        group: None,
        capture_stack_trace: None,
        capture_memory_snapshot: None,
        ttl_seconds: None,
    };

    let result = fixture.server.observe(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "observe with filename-only should succeed: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_inspect_file_relative_path_with_connection() {
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    let params = InspectFileParams {
        file_path: "test.py".to_string(), // Relative path
        line: Some(30),
        find_variable: None,
        connection_id: Some(conn_id.to_string()),
    };

    let result = fixture.server.inspect_file(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "inspect_file with relative path + connection_id should succeed: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_inspect_file_relative_path_auto_select() {
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    // Create single connection — should be auto-selected for path resolution
    let _conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    let params = InspectFileParams {
        file_path: "auth.py".to_string(), // Relative path
        line: None,
        find_variable: Some("user".to_string()),
        connection_id: None, // Auto-select single connection
    };

    let result = fixture.server.inspect_file(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "inspect_file with relative path + auto-select should succeed: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_inspect_file_find_variable_relative_path() {
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    let params = InspectFileParams {
        file_path: "auth.py".to_string(), // Relative
        line: None,
        find_variable: Some("user".to_string()),
        connection_id: Some(conn_id.to_string()),
    };

    let result = fixture.server.inspect_file(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "inspect_file find_variable with relative path should succeed: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_enable_from_diff_relative_path() {
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    // Diff uses relative path "auth.py" — should resolve against workspace_root
    let diff = r#"
diff --git a/auth.py b/auth.py
@@ -125,6 +125,7 @@
 user = get_user()
+    print(f"user_id={user.id}")
 return user
"#;

    let params = EnableFromDiffParams {
        diff: diff.to_string(),
        connection_id: Some(conn_id.to_string()),
        group: Some("diff_rel_test".to_string()),
        ttl_seconds: None,
    };

    let result = fixture.server.enable_from_diff(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "enable_from_diff with relative path should succeed: {:?}",
        result.err()
    );

    // Verify the stored metric has the resolved absolute path
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert!(
        std::path::Path::new(&metrics[0].location.file).is_absolute(),
        "Diff-created metric should have absolute file path: {}",
        metrics[0].location.file
    );
}

#[tokio::test]
async fn test_absolute_path_still_works_with_workspace() {
    // Ensure absolute paths pass through unchanged even when workspace_root is set
    let fixture = TestFixture::new().await;
    let workspace = fixtures_dir().to_string_lossy().to_string();
    let conn_id = fixture
        .with_mock_connection_with_workspace(&workspace)
        .await;

    let absolute_path = auth_py_path();
    let params = ObserveParams {
        file: absolute_path.clone(), // Absolute path
        expressions: vec!["user.id".to_string()],
        line: Some(127),
        connection_id: Some(conn_id.to_string()),
        name: Some("abs_path_test".to_string()),
        find_variable: None,
        group: None,
        capture_stack_trace: None,
        capture_memory_snapshot: None,
        ttl_seconds: None,
    };

    let result = fixture.server.observe(Parameters(params)).await;
    assert!(
        result.is_ok(),
        "observe with absolute path should still succeed: {:?}",
        result.err()
    );

    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(
        metrics[0].location.file, absolute_path,
        "Absolute path should be preserved unchanged"
    );
}
