//! MCP (Model Context Protocol) integration tests
//!
//! Tests for MCP server functionality including:
//! - Adding metrics with new fields (stack trace, memory snapshot)
//! - Time-based sampling mode
//! - Security validation
//! - TTL handling
//!
//! These tests use MockDapAdapterFactory and don't require a real debugger.

use detrix_api::ApiState;
use detrix_application::{
    AppContext, ConnectionRepositoryRef, DapAdapterFactoryRef, EventRepositoryRef,
    MetricRepositoryRef,
};
use detrix_config::ApiConfig;
use detrix_core::{
    AnchorStatus, ConnectionId, Location, Metric, MetricMode, SnapshotScope, SourceLanguage,
    StackTraceSlice,
};
use detrix_storage::{SqliteConfig, SqliteStorage};
use detrix_testing::fixtures::test_py_path;
use detrix_testing::MockDapAdapterFactory;
use std::sync::Arc;
use tempfile::TempDir;

/// Test fixture for MCP tests
struct McpTestFixture {
    _temp_dir: TempDir,
    state: Arc<ApiState>,
}

impl McpTestFixture {
    async fn new() -> Self {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let sqlite_config = SqliteConfig {
            path: db_path,
            pool_size: 1,
            busy_timeout_ms: 5000,
        };

        let storage = Arc::new(SqliteStorage::new(&sqlite_config).await.unwrap());

        // Use MockDapAdapterFactory instead of PythonAdapter
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

        let state = Arc::new(
            ApiState::builder(context, Arc::clone(&storage) as EventRepositoryRef).build(),
        );

        Self {
            _temp_dir: temp_dir,
            state,
        }
    }
}

// ============================================================================
// Test 1: Add Metric with Stack Trace Options
// ============================================================================

#[tokio::test]
async fn test_add_metric_with_stack_trace() {
    let fixture = McpTestFixture::new().await;

    // Create metric with stack trace enabled
    let metric = Metric {
        id: None,
        name: "test_stack_trace".to_string(),
        connection_id: ConnectionId::from("test"),
        group: None,
        location: Location {
            file: test_py_path(),
            line: 10,
        },
        expressions: vec!["user.id".to_string()],
        language: SourceLanguage::Python,
        enabled: true,
        mode: MetricMode::Stream,
        condition: None,
        safety_level: detrix_core::SafetyLevel::Strict,
        created_at: None,
        // Stack trace options
        capture_stack_trace: true,
        stack_trace_ttl: Some(10), // 10 seconds
        stack_trace_slice: Some(StackTraceSlice {
            full: false,
            head: Some(10),
            tail: Some(10),
        }),
        // Memory snapshot options
        capture_memory_snapshot: false,
        snapshot_scope: None,
        snapshot_ttl: None,
        anchor: None,
        anchor_status: AnchorStatus::Unanchored,
    };

    // Import metric via service (storage only, no adapter needed for unit tests)
    let metric_id = fixture
        .state
        .context
        .metric_service
        .import_metric(metric.clone())
        .await
        .expect("Failed to import metric");

    // Verify metric was saved with stack trace options
    let saved_metric = fixture
        .state
        .context
        .metric_service
        .get_metric_by_name("test_stack_trace")
        .await
        .expect("Failed to get metric")
        .expect("Metric not found");

    assert_eq!(saved_metric.id, Some(metric_id));

    assert!(saved_metric.capture_stack_trace);
    assert_eq!(saved_metric.stack_trace_ttl, Some(10));
    assert!(saved_metric.stack_trace_slice.is_some());

    let slice = saved_metric.stack_trace_slice.unwrap();
    assert!(!slice.full);
    assert_eq!(slice.head, Some(10));
    assert_eq!(slice.tail, Some(10));
}

// ============================================================================
// Test 2: Add Metric with Memory Snapshot Options
// ============================================================================

#[tokio::test]
async fn test_add_metric_with_memory_snapshot() {
    let fixture = McpTestFixture::new().await;

    let metric = Metric {
        id: None,
        name: "test_snapshot".to_string(),
        connection_id: ConnectionId::from("test"),
        group: None,
        location: Location {
            file: test_py_path(),
            line: 20,
        },
        expressions: vec!["session.user_id".to_string()],
        language: SourceLanguage::Python,
        enabled: true,
        mode: MetricMode::First,
        condition: None,
        safety_level: detrix_core::SafetyLevel::Strict,
        created_at: None,
        // Stack trace options
        capture_stack_trace: false,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        // Memory snapshot options
        capture_memory_snapshot: true,
        snapshot_scope: Some(SnapshotScope::Local),
        snapshot_ttl: Some(10),
        anchor: None,
        anchor_status: AnchorStatus::Unanchored,
    };

    let metric_id = fixture
        .state
        .context
        .metric_service
        .import_metric(metric.clone())
        .await
        .expect("Failed to add metric");

    let saved_metric = fixture
        .state
        .context
        .metric_service
        .get_metric_by_name("test_snapshot")
        .await
        .expect("Failed to get metric")
        .expect("Metric not found");

    assert_eq!(saved_metric.id, Some(metric_id));

    assert!(saved_metric.capture_memory_snapshot);
    assert_eq!(saved_metric.snapshot_scope, Some(SnapshotScope::Local));
    assert_eq!(saved_metric.snapshot_ttl, Some(10));
}

// ============================================================================
// Test 3: Add Metric with Time-Based Sampling
// ============================================================================

#[tokio::test]
async fn test_add_metric_with_time_based_sampling() {
    let fixture = McpTestFixture::new().await;

    let metric = Metric {
        id: None,
        name: "test_time_sampling".to_string(),
        connection_id: ConnectionId::from("test"),
        group: None,
        location: Location {
            file: test_py_path(),
            line: 30,
        },
        expressions: vec!["request.count".to_string()],
        language: SourceLanguage::Python,
        enabled: true,
        mode: MetricMode::SampleInterval { seconds: 30 }, // Every 30 seconds
        condition: None,
        safety_level: detrix_core::SafetyLevel::Strict,
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
        .import_metric(metric.clone())
        .await
        .expect("Failed to add metric");

    let saved_metric = fixture
        .state
        .context
        .metric_service
        .get_metric_by_name("test_time_sampling")
        .await
        .expect("Failed to get metric")
        .expect("Metric not found");

    assert_eq!(saved_metric.id, Some(metric_id));

    match saved_metric.mode {
        MetricMode::SampleInterval { seconds } => {
            assert_eq!(seconds, 30);
        }
        _ => panic!("Expected SampleInterval mode"),
    }
}

// ============================================================================
// Test 4: Add Metric with Combined Features
// ============================================================================

#[tokio::test]
async fn test_add_metric_with_all_features() {
    let fixture = McpTestFixture::new().await;

    let metric = Metric {
        id: None,
        name: "test_combined".to_string(),
        connection_id: ConnectionId::from("test"),
        group: Some("test_group".to_string()),
        location: Location {
            file: "payment.py".to_string(),
            line: 89,
        },
        expressions: vec!["payment.amount".to_string()],
        language: SourceLanguage::Python,
        enabled: true,
        mode: MetricMode::SampleInterval { seconds: 30 },
        condition: None,
        safety_level: detrix_core::SafetyLevel::Strict,
        created_at: None,
        // Stack trace + memory snapshot + TTL
        capture_stack_trace: true,
        stack_trace_ttl: Some(10),
        stack_trace_slice: Some(StackTraceSlice {
            full: false,
            head: Some(10),
            tail: Some(10),
        }),
        capture_memory_snapshot: true,
        snapshot_scope: Some(SnapshotScope::Both),
        snapshot_ttl: Some(15),
        anchor: None,
        anchor_status: AnchorStatus::Unanchored,
    };

    let metric_id = fixture
        .state
        .context
        .metric_service
        .import_metric(metric.clone())
        .await
        .expect("Failed to add metric");

    let saved_metric = fixture
        .state
        .context
        .metric_service
        .get_metric_by_name("test_combined")
        .await
        .expect("Failed to get metric")
        .expect("Metric not found");

    assert_eq!(saved_metric.id, Some(metric_id));

    // Verify all fields
    assert!(saved_metric.capture_stack_trace);
    assert_eq!(saved_metric.stack_trace_ttl, Some(10));
    assert!(saved_metric.capture_memory_snapshot);
    assert_eq!(saved_metric.snapshot_scope, Some(SnapshotScope::Both));
    assert_eq!(saved_metric.snapshot_ttl, Some(15));

    match saved_metric.mode {
        MetricMode::SampleInterval { seconds } => {
            assert_eq!(seconds, 30);
        }
        _ => panic!("Expected SampleInterval mode"),
    }
}

// ============================================================================
// Test 5: Stack Trace Slice Variations
// ============================================================================

#[tokio::test]
async fn test_stack_trace_slice_full() {
    let fixture = McpTestFixture::new().await;

    let metric = Metric {
        id: None,
        name: "test_full_trace".to_string(),
        connection_id: ConnectionId::from("test"),
        group: None,
        location: Location {
            file: test_py_path(),
            line: 40,
        },
        expressions: vec!["result".to_string()],
        language: SourceLanguage::Python,
        enabled: true,
        mode: MetricMode::First,
        condition: None,
        safety_level: detrix_core::SafetyLevel::Strict,
        created_at: None,
        capture_stack_trace: true,
        stack_trace_ttl: None, // Continuous
        stack_trace_slice: Some(StackTraceSlice {
            full: true, // Capture full stack
            head: None,
            tail: None,
        }),
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
        .import_metric(metric)
        .await
        .expect("Failed to import metric");

    let saved_metric = fixture
        .state
        .context
        .metric_service
        .get_metric_by_name("test_full_trace")
        .await
        .expect("Failed to get metric")
        .expect("Metric not found");

    assert_eq!(saved_metric.id, Some(metric_id));

    let slice = saved_metric.stack_trace_slice.unwrap();
    assert!(slice.full);
    assert_eq!(saved_metric.stack_trace_ttl, None); // Continuous
}

// ============================================================================
// Test 6: Default Values
// ============================================================================

#[tokio::test]
async fn test_metric_defaults() {
    let fixture = McpTestFixture::new().await;

    // Create metric with minimal fields (no stack trace or snapshot)
    let metric = Metric::new(
        "test_defaults".to_string(),
        ConnectionId::from("test"),
        Location {
            file: test_py_path(),
            line: 50,
        },
        vec!["value".to_string()],
        SourceLanguage::Python,
    )
    .expect("Failed to create metric");

    let metric_id = fixture
        .state
        .context
        .metric_service
        .import_metric(metric)
        .await
        .expect("Failed to import metric");

    let saved_metric = fixture
        .state
        .context
        .metric_service
        .get_metric_by_name("test_defaults")
        .await
        .expect("Failed to get metric")
        .expect("Metric not found");

    assert_eq!(saved_metric.id, Some(metric_id));

    // Verify defaults
    assert!(!saved_metric.capture_stack_trace);
    assert_eq!(saved_metric.stack_trace_ttl, None);
    assert_eq!(saved_metric.stack_trace_slice, None);
    assert!(!saved_metric.capture_memory_snapshot);
    assert_eq!(saved_metric.snapshot_scope, None);
    assert_eq!(saved_metric.snapshot_ttl, None);
}

// ============================================================================
// Test 7: Snapshot Scope Variations
// ============================================================================

#[tokio::test]
async fn test_snapshot_scope_global() {
    let fixture = McpTestFixture::new().await;

    let metric = Metric {
        id: None,
        name: "test_global_scope".to_string(),
        connection_id: ConnectionId::from("test"),
        group: None,
        location: Location {
            file: test_py_path(),
            line: 60,
        },
        expressions: vec!["__detrix_snapshot__(scope='global')".to_string()],
        language: SourceLanguage::Python,
        enabled: true,
        mode: MetricMode::First,
        condition: None,
        safety_level: detrix_core::SafetyLevel::Trusted, // Allow special functions
        created_at: None,
        capture_stack_trace: false,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: true,
        snapshot_scope: Some(SnapshotScope::Global),
        snapshot_ttl: None,
        anchor: None,
        anchor_status: AnchorStatus::Unanchored,
    };

    let metric_id = fixture
        .state
        .context
        .metric_service
        .import_metric(metric)
        .await
        .expect("Failed to import metric");

    let saved_metric = fixture
        .state
        .context
        .metric_service
        .get_metric_by_name("test_global_scope")
        .await
        .expect("Failed to get metric")
        .expect("Metric not found");

    assert_eq!(saved_metric.id, Some(metric_id));

    assert_eq!(saved_metric.snapshot_scope, Some(SnapshotScope::Global));
}

// ============================================================================
// Test 8: List Metrics with New Fields
// ============================================================================

#[tokio::test]
async fn test_list_metrics_includes_new_fields() {
    let fixture = McpTestFixture::new().await;

    // Add multiple metrics with different configurations
    let metrics = vec![
        Metric {
            id: None,
            name: "metric_with_stack".to_string(),
            connection_id: ConnectionId::from("test"),
            group: None,
            location: Location {
                file: test_py_path(),
                line: 10,
            },
            expressions: vec!["x".to_string()],
            language: SourceLanguage::Python,
            enabled: true,
            mode: MetricMode::Stream,
            condition: None,
            safety_level: detrix_core::SafetyLevel::Strict,
            created_at: None,
            capture_stack_trace: true,
            stack_trace_ttl: Some(10),
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: AnchorStatus::Unanchored,
        },
        Metric {
            id: None,
            name: "metric_with_snapshot".to_string(),
            connection_id: ConnectionId::from("test"),
            group: None,
            location: Location {
                file: test_py_path(),
                line: 20,
            },
            expressions: vec!["y".to_string()],
            language: SourceLanguage::Python,
            enabled: true,
            mode: MetricMode::Stream,
            condition: None,
            safety_level: detrix_core::SafetyLevel::Strict,
            created_at: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: true,
            snapshot_scope: Some(SnapshotScope::Local),
            snapshot_ttl: Some(5),
            anchor: None,
            anchor_status: AnchorStatus::Unanchored,
        },
    ];

    for metric in metrics {
        fixture
            .state
            .context
            .metric_service
            .import_metric(metric)
            .await
            .expect("Failed to add metric");
    }

    // List all metrics
    let all_metrics = fixture
        .state
        .context
        .metric_service
        .list_metrics()
        .await
        .expect("Failed to list metrics");

    assert_eq!(all_metrics.len(), 2);

    // Verify first metric
    let stack_metric = all_metrics
        .iter()
        .find(|m| m.name == "metric_with_stack")
        .expect("Stack metric not found");
    assert!(stack_metric.capture_stack_trace);

    // Verify second metric
    let snapshot_metric = all_metrics
        .iter()
        .find(|m| m.name == "metric_with_snapshot")
        .expect("Snapshot metric not found");
    assert!(snapshot_metric.capture_memory_snapshot);
}

// ============================================================================
// Test 9: Serialization/Deserialization
// ============================================================================

#[tokio::test]
async fn test_metric_serialization_with_new_fields() {
    let metric = Metric {
        id: None,
        name: "test_serde".to_string(),
        connection_id: ConnectionId::from("test"),
        group: None,
        location: Location {
            file: test_py_path(),
            line: 100,
        },
        expressions: vec!["data".to_string()],
        language: SourceLanguage::Python,
        enabled: true,
        mode: MetricMode::SampleInterval { seconds: 60 },
        condition: None,
        safety_level: detrix_core::SafetyLevel::Strict,
        created_at: None,
        capture_stack_trace: true,
        stack_trace_ttl: Some(20),
        stack_trace_slice: Some(StackTraceSlice {
            full: false,
            head: Some(5),
            tail: Some(5),
        }),
        capture_memory_snapshot: true,
        snapshot_scope: Some(SnapshotScope::Both),
        snapshot_ttl: Some(30),
        anchor: None,
        anchor_status: AnchorStatus::Unanchored,
    };

    // Serialize to JSON
    let json = serde_json::to_string(&metric).expect("Failed to serialize");

    // Deserialize back
    let deserialized: Metric = serde_json::from_str(&json).expect("Failed to deserialize");

    // Verify all fields match
    assert_eq!(deserialized.name, metric.name);
    assert_eq!(deserialized.capture_stack_trace, metric.capture_stack_trace);
    assert_eq!(deserialized.stack_trace_ttl, metric.stack_trace_ttl);
    assert_eq!(
        deserialized.capture_memory_snapshot,
        metric.capture_memory_snapshot
    );
    assert_eq!(deserialized.snapshot_scope, metric.snapshot_scope);
    assert_eq!(deserialized.snapshot_ttl, metric.snapshot_ttl);

    match deserialized.mode {
        MetricMode::SampleInterval { seconds } => assert_eq!(seconds, 60),
        _ => panic!("Expected SampleInterval"),
    }
}
