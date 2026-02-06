//! MCP Server End-to-End Tests
//!
//! These tests verify the MCP server functionality from an external perspective,
//! simulating how an LLM client would interact with the server.
//!
//! Since rmcp's RequestContext is complex to mock, we:
//! 1. Verify tools are properly registered via ServerHandler::get_info
//! 2. Test the underlying MetricService directly (same logic as MCP tools)
//! 3. Use the storage layer to verify state changes
//!
//! For internal tool logic tests, see the `#[cfg(test)]` module in `src/mcp/server.rs`.

use detrix_api::mcp::DetrixServer;
use detrix_api::ApiState;
use detrix_application::{
    AppContext, ConnectionRepositoryRef, DapAdapterFactoryRef, EventRepositoryRef,
    MetricRepositoryRef,
};
use detrix_config::ApiConfig;
use detrix_core::{
    AnchorStatus, ConnectionId, Location, Metric, MetricEvent, MetricMode, SafetyLevel,
    SourceLanguage,
};
use detrix_storage::{SqliteConfig, SqliteStorage};
use detrix_testing::fixtures::{app_py_path, test_py_path};
use detrix_testing::MockDapAdapterFactory;
use rmcp::ServerHandler;
use std::sync::Arc;
use tempfile::TempDir;

// ============================================================================
// Test Fixture
// ============================================================================

/// E2E test fixture for MCP server tests.
struct McpE2eFixture {
    _temp_dir: TempDir,
    server: DetrixServer,
    state: Arc<ApiState>,
}

impl McpE2eFixture {
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
        let identity =
            detrix_core::ConnectionIdentity::new("test", "python", "/test-workspace", "test-host");
        self.state
            .context
            .connection_service
            .create_connection(
                "127.0.0.1".to_string(),
                5678,
                identity,
                None,  // No program path for Python
                None,  // No pid (not using AttachPid mode)
                false, // SafeMode disabled for tests
            )
            .await
            .expect("Failed to create mock connection")
    }

    /// Helper to create a test metric
    fn sample_metric(name: &str, group: Option<&str>, conn_id: &ConnectionId) -> Metric {
        Self::sample_metric_at_line(name, group, 10, conn_id)
    }

    /// Helper to create a test metric at a specific line
    /// Uses real fixture file path for file validation
    fn sample_metric_at_line(
        name: &str,
        group: Option<&str>,
        line: u32,
        conn_id: &ConnectionId,
    ) -> Metric {
        Metric {
            id: None,
            name: name.to_string(),
            connection_id: conn_id.clone(),
            group: group.map(|s| s.to_string()),
            location: Location {
                file: test_py_path(),
                line,
            },
            expression: "x.value".to_string(),
            language: SourceLanguage::Python,
            mode: MetricMode::Stream,
            enabled: true,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: AnchorStatus::Unanchored,
        }
    }
}

// ============================================================================
// Server Registration Tests
// ============================================================================

#[tokio::test]
async fn test_server_info_has_tools_capability() {
    let fixture = McpE2eFixture::new().await;

    let info = fixture.server.get_info();

    // Verify server provides instructions
    assert!(info.instructions.is_some());
    assert!(info.instructions.as_ref().unwrap().contains("Detrix"));

    // Verify tools are enabled
    assert!(
        info.capabilities.tools.is_some(),
        "Server should have tools capability"
    );
}

// ============================================================================
// E2E Workflow Tests via MetricService
// ============================================================================
// These tests verify the business logic that MCP tools use, testing the same
// code paths without needing to mock the MCP transport layer.

#[tokio::test]
async fn test_e2e_add_and_list_metric() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let metric = McpE2eFixture::sample_metric("test_metric", Some("test_group"), &conn_id);

    let metric_id = fixture
        .state
        .context
        .metric_service
        .add_metric(metric, false)
        .await
        .expect("Should add metric")
        .value;

    // List metrics and verify
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .expect("Should list metrics");

    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].name, "test_metric");
    assert_eq!(metrics[0].id, Some(metric_id));
}

#[tokio::test]
async fn test_e2e_toggle_metric() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let metric = McpE2eFixture::sample_metric("toggle_test", None, &conn_id);

    let metric_id = fixture
        .state
        .context
        .metric_service
        .add_metric(metric, false)
        .await
        .unwrap()
        .value;

    // Disable
    fixture
        .state
        .context
        .metric_service
        .toggle_metric(metric_id, false)
        .await
        .unwrap();

    let metric = fixture
        .state
        .context
        .metric_service
        .get_metric_by_name("toggle_test")
        .await
        .unwrap()
        .unwrap();
    assert!(!metric.enabled, "Metric should be disabled");

    // Re-enable
    fixture
        .state
        .context
        .metric_service
        .toggle_metric(metric_id, true)
        .await
        .unwrap();

    let metric = fixture
        .state
        .context
        .metric_service
        .get_metric_by_name("toggle_test")
        .await
        .unwrap()
        .unwrap();
    assert!(metric.enabled, "Metric should be re-enabled");
}

#[tokio::test]
async fn test_e2e_query_events() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    let metric = McpE2eFixture::sample_metric("query_test", None, &conn_id);

    let metric_id = fixture
        .state
        .context
        .metric_service
        .add_metric(metric, false)
        .await
        .unwrap()
        .value;

    // Add events
    for i in 0..5 {
        let event = MetricEvent::new(
            metric_id,
            "query_test".to_string(),
            conn_id.clone(),
            format!(r#"{{"value": {}}}"#, i),
        );
        fixture.state.event_repository.save(&event).await.unwrap();
    }

    // Query events
    let events = fixture
        .state
        .event_repository
        .find_by_metric_id(metric_id, 100)
        .await
        .unwrap();

    assert_eq!(events.len(), 5);
}

#[tokio::test]
async fn test_e2e_validate_expression() {
    let fixture = McpE2eFixture::new().await;

    // Safe expression
    let result = fixture.state.context.metric_service.validate_expression(
        "user.id",
        "python",
        SafetyLevel::Strict,
    );
    assert!(result.is_ok());
    assert!(result.unwrap().is_safe);
}

#[tokio::test]
async fn test_e2e_full_metric_lifecycle() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    // 1. Add metric
    let metric = Metric {
        id: None,
        name: "lifecycle_test".to_string(),
        connection_id: conn_id.clone(),
        group: Some("test".to_string()),
        location: Location {
            file: app_py_path(),
            line: 100,
        },
        expression: "request.user_id".to_string(),
        language: SourceLanguage::Python,
        mode: MetricMode::Stream,
        enabled: true,
        condition: None,
        safety_level: SafetyLevel::Strict,
        created_at: None,
        capture_stack_trace: false,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: false,
        snapshot_scope: None,
        snapshot_ttl: None,
        anchor: None,
        anchor_status: AnchorStatus::Unanchored,
    };

    let metric_id = fixture
        .state
        .context
        .metric_service
        .add_metric(metric, false)
        .await
        .unwrap()
        .value;

    // 2. List and verify
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics_by_group("test")
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].name, "lifecycle_test");

    // 3. Disable
    fixture
        .state
        .context
        .metric_service
        .toggle_metric(metric_id, false)
        .await
        .unwrap();

    // 4. Re-enable
    fixture
        .state
        .context
        .metric_service
        .toggle_metric(metric_id, true)
        .await
        .unwrap();

    // 5. Remove
    fixture
        .state
        .context
        .metric_service
        .remove_metric(metric_id)
        .await
        .unwrap();

    // 6. Verify gone
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 0);
}

#[tokio::test]
async fn test_e2e_multiple_metrics_in_group() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    // Add metrics in different groups at different lines (DAP only supports one logpoint per line)
    for (name, group, line) in [
        ("metric_a", "group_a", 10),
        ("metric_b", "group_b", 20),
        ("metric_c", "group_a", 30),
    ] {
        let metric = McpE2eFixture::sample_metric_at_line(name, Some(group), line, &conn_id);
        fixture
            .state
            .context
            .metric_service
            .add_metric(metric, false)
            .await
            .unwrap();
    }

    // Filter by group_a
    let group_a_metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics_by_group("group_a")
        .await
        .unwrap();

    assert_eq!(group_a_metrics.len(), 2);
    assert!(group_a_metrics.iter().any(|m| m.name == "metric_a"));
    assert!(group_a_metrics.iter().any(|m| m.name == "metric_c"));
}

// ============================================================================
// Phase 4 Integration Tests - Connection Robustness
// ============================================================================

/// Test 4.3: Concurrent MCP tool calls don't deadlock
/// Verifies that multiple simultaneous operations work correctly
#[tokio::test]
async fn test_concurrent_tool_calls_no_deadlock() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    // Spawn multiple concurrent add operations
    let mut add_handles = vec![];
    let fixture_path = test_py_path(); // Capture path once for all tasks
    for i in 0..10 {
        let state = Arc::clone(&fixture.state);
        let file_path = fixture_path.clone();
        let conn_id_clone = conn_id.clone();
        let handle = tokio::spawn(async move {
            let metric = Metric {
                id: None,
                name: format!("concurrent_metric_{}", i),
                connection_id: conn_id_clone,
                group: Some("concurrent_group".to_string()),
                location: Location {
                    file: file_path,
                    line: 40 + i as u32, // Lines 40-49 have x0-x9 variables
                },
                expression: format!("x{}.value", i),
                language: SourceLanguage::Python,
                mode: MetricMode::Stream,
                enabled: true,
                condition: None,
                safety_level: SafetyLevel::Strict,
                created_at: None,
                capture_stack_trace: false,
                stack_trace_ttl: None,
                stack_trace_slice: None,
                capture_memory_snapshot: false,
                snapshot_scope: None,
                snapshot_ttl: None,
                anchor: None,
                anchor_status: AnchorStatus::Unanchored,
            };
            state.context.metric_service.add_metric(metric, false).await
        });
        add_handles.push(handle);
    }

    // Spawn concurrent list operations separately
    let mut list_handles = vec![];
    for _ in 0..5 {
        let state = Arc::clone(&fixture.state);
        let handle = tokio::spawn(async move { state.context.metric_service.list_metrics().await });
        list_handles.push(handle);
    }

    // Wait for all adds with timeout to detect deadlocks
    let add_results = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        futures::future::join_all(add_handles),
    )
    .await;

    assert!(
        add_results.is_ok(),
        "Add operations timed out - possible deadlock"
    );

    // Wait for all lists
    let list_results = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        futures::future::join_all(list_handles),
    )
    .await;

    assert!(
        list_results.is_ok(),
        "List operations timed out - possible deadlock"
    );

    // All operations should complete successfully
    for result in add_results.unwrap() {
        assert!(result.is_ok(), "Add task panicked");
    }
    for result in list_results.unwrap() {
        assert!(result.is_ok(), "List task panicked");
    }

    // Verify all metrics were added
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();

    assert_eq!(metrics.len(), 10, "All 10 metrics should be added");
}

/// Test 4.5: Graceful degradation - list_metrics works when disconnected
/// Read-only operations should work even without debugger connection
#[tokio::test]
async fn test_graceful_degradation_list_metrics_when_disconnected() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    // Add a metric first (MockDapAdapter is "connected")
    let metric = McpE2eFixture::sample_metric("test_metric", None, &conn_id);
    fixture
        .state
        .context
        .metric_service
        .add_metric(metric, false)
        .await
        .unwrap();

    // MockDapAdapter.is_connected() returns true by default
    // But list_metrics should work regardless of connection state
    // since it only reads from the database

    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();

    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].name, "test_metric");
}

/// Test 4.5: Graceful degradation - query_metrics works when disconnected
#[tokio::test]
async fn test_graceful_degradation_query_events_when_disconnected() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    // Add a metric
    let metric = McpE2eFixture::sample_metric("query_test", None, &conn_id);
    let metric_id = fixture
        .state
        .context
        .metric_service
        .add_metric(metric, false)
        .await
        .unwrap()
        .value;

    // Add some events directly to storage using the correct struct
    let event = MetricEvent {
        id: None,
        metric_id,
        metric_name: "query_test".to_string(),
        connection_id: conn_id.clone(),
        timestamp: MetricEvent::now_micros(),
        thread_name: None,
        thread_id: None,
        value_json: r#"{"test": "value"}"#.to_string(),
        value_numeric: None,
        value_string: Some("test_value".to_string()),
        value_boolean: None,
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    };

    fixture.state.event_repository.save(&event).await.unwrap();

    // Query should work regardless of connection state
    let events = fixture
        .state
        .event_repository
        .find_by_metric_id(metric_id, 100)
        .await
        .unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value_string, Some("test_value".to_string()));
}

// ============================================================================
// Observe Tool E2E Tests (via Service Layer)
// ============================================================================
//
// Note: The observe tool is tested via the MetricService layer, as the tool
// method itself is private (generated by the #[tool] macro). This matches
// the pattern used by other E2E tests in this file.

/// Test E2E: observe workflow - add metric with introspection options
/// Tests the same functionality as the observe MCP tool via service layer
#[tokio::test]
async fn test_e2e_observe_workflow_with_introspection() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    // Create a metric with introspection options (like observe tool would do)
    let metric = Metric {
        id: None,
        name: "introspection_test".to_string(),
        connection_id: conn_id,
        group: Some("observe_tests".to_string()),
        location: Location {
            file: test_py_path(),
            line: 30,
        },
        expression: "transaction.amount".to_string(),
        language: SourceLanguage::Python,
        mode: MetricMode::Stream,
        enabled: true,
        condition: None,
        safety_level: SafetyLevel::Strict,
        created_at: None,
        capture_stack_trace: true,
        stack_trace_ttl: Some(600), // TTL like observe would set
        stack_trace_slice: None,
        capture_memory_snapshot: true,
        snapshot_scope: None,
        snapshot_ttl: Some(600),
        anchor: None,
        anchor_status: AnchorStatus::Unanchored,
    };

    let result = fixture
        .state
        .context
        .metric_service
        .add_metric(metric, false)
        .await;

    assert!(
        result.is_ok(),
        "observe workflow should succeed: {:?}",
        result.err()
    );

    // Verify introspection settings were preserved
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert!(metrics[0].capture_stack_trace);
    assert!(metrics[0].capture_memory_snapshot);
    assert_eq!(metrics[0].stack_trace_ttl, Some(600));
    assert_eq!(metrics[0].snapshot_ttl, Some(600));
}

/// Test E2E: observe workflow respects connection_id
/// Verifies metrics are properly associated with connections
#[tokio::test]
async fn test_e2e_observe_workflow_connection_binding() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    // Create metric bound to specific connection
    let metric = Metric {
        id: None,
        name: "connection_bound_metric".to_string(),
        connection_id: conn_id.clone(),
        group: None,
        location: Location {
            file: test_py_path(),
            line: 31,
        },
        expression: "user.session".to_string(),
        language: SourceLanguage::Python,
        mode: MetricMode::Stream,
        enabled: true,
        condition: None,
        safety_level: SafetyLevel::Strict,
        created_at: None,
        capture_stack_trace: false,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: false,
        snapshot_scope: None,
        snapshot_ttl: None,
        anchor: None,
        anchor_status: AnchorStatus::Unanchored,
    };

    fixture
        .state
        .context
        .metric_service
        .add_metric(metric, false)
        .await
        .unwrap();

    // Verify connection_id is preserved
    let metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].connection_id, conn_id);
}

/// Test E2E: observe workflow with group assignment
/// Verifies metrics can be grouped for batch operations
#[tokio::test]
async fn test_e2e_observe_workflow_with_group() {
    let fixture = McpE2eFixture::new().await;
    let conn_id = fixture.with_mock_connection().await;

    // Create multiple metrics in same group
    for (name, line) in [
        ("auth_metric_1", 32),
        ("auth_metric_2", 33),
        ("auth_metric_3", 34),
    ] {
        let metric = Metric {
            id: None,
            name: name.to_string(),
            connection_id: conn_id.clone(),
            group: Some("auth_group".to_string()),
            location: Location {
                file: test_py_path(),
                line,
            },
            expression: "user.id".to_string(),
            language: SourceLanguage::Python,
            mode: MetricMode::Stream,
            enabled: true,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: AnchorStatus::Unanchored,
        };

        fixture
            .state
            .context
            .metric_service
            .add_metric(metric, false)
            .await
            .unwrap();
    }

    // Verify all metrics in group
    let group_metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics_by_group("auth_group")
        .await
        .unwrap();
    assert_eq!(group_metrics.len(), 3);

    // Disable group
    let result = fixture
        .state
        .context
        .metric_service
        .disable_group("auth_group")
        .await
        .unwrap();
    assert_eq!(result.succeeded, 3);
}
