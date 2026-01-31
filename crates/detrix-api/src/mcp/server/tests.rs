use super::*;
use crate::ApiState;
use detrix_application::{
    AppContext, ConnectionRepositoryRef, DapAdapterFactoryRef, EventRepositoryRef,
    MetricRepositoryRef,
};
use detrix_config::ApiConfig;
use detrix_core::ConnectionId;
use detrix_storage::{SqliteConfig, SqliteStorage};
use detrix_testing::fixtures::{auth_py_path, test_py_path};
use detrix_testing::MockDapAdapterFactory;
use rmcp::handler::server::wrapper::Parameters;
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
        // Use the connection_service to create a connection (saves to DB and starts adapter)
        // MockDapAdapterFactory will create a MockDapAdapter
        self.state
            .context
            .connection_service
            .create_connection(
                "127.0.0.1".to_string(),
                5678,
                "python".to_string(),
                Some("test".to_string()),
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
        expression: "user.id".to_string(),
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
async fn test_add_metric_location_without_at_works() {
    // @ prefix is now optional - both formats should work
    let fixture = TestFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let params = AddMetricParams {
        name: "no_at_metric".to_string(),
        location: format!("{}#127", auth_py_path()), // No @ prefix - should work now
        line: None,
        expression: "user.id".to_string(),
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
        expression: "user.id".to_string(),
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
        expression: "user.id".to_string(),
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
        expression: "user.id".to_string(),
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
        expression: "x.value".to_string(), // Use complex expression to skip scope validation
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
        expression: "x.value".to_string(), // Use complex expression to skip scope validation
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
        expression: "x.value".to_string(), // Use complex expression to skip scope validation
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
        expression: "x.value".to_string(), // Use complex expression to skip scope validation
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
        expression: "x.value".to_string(), // Use complex expression to skip scope validation
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
        expression: "x.value".to_string(), // Use complex expression to skip scope validation
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
        expression: "x.value".to_string(), // Use complex expression to skip scope validation
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
        expression: "x.value".to_string(), // Use complex expression to skip scope validation
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
async fn test_wake_and_sleep() {
    let fixture = TestFixture::new().await;

    let wake_result = fixture.server.wake().await;
    assert!(wake_result.is_ok());

    let sleep_result = fixture.server.sleep().await;
    assert!(sleep_result.is_ok());
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
        expression: "user.id".to_string(),
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
        expression: "user.id".to_string(),
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
        expression: "user.id".to_string(),
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
        expression: "user.id".to_string(),
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
        expression: "user.id".to_string(),
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
    assert_eq!(metrics[0].expression, "user.id");
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

    let expressions: Vec<&str> = metrics.iter().map(|m| m.expression.as_str()).collect();
    assert!(
        expressions.contains(&"user.name"),
        "Expected 'user.name' expression"
    );
    assert!(
        expressions.contains(&"user.email"),
        "Expected 'user.email' expression"
    );
}
