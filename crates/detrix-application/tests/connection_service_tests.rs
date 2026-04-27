//! Tests for ConnectionService
//!
//! Following TDD: Write tests first, then implement ConnectionService to make them pass.

use detrix_application::{
    AdapterLifecycleManager, ConnectionReferenceRepositoryRef, ConnectionRepository,
    ConnectionRepositoryRef, ConnectionService, DapAdapterFactoryRef, EventCaptureService,
    MetricRepositoryRef,
};
use detrix_core::{
    Connection, ConnectionId, ConnectionIdentity, ConnectionStatus, Error, Location, MetricEvent,
    SourceLanguage, SystemEvent,
};
use detrix_ports::{MetricRepository, VfsRef};
use detrix_testing::fixtures::{sample_connection_identity, sample_metric_with_connection};
use detrix_testing::{
    MockConnectionReferenceRepository, MockMetricRepository as FullMockMetricRepository, MockVfs,
};
use std::sync::Arc;
use tokio::sync::broadcast;

mod test_support;
use test_support::{
    MockConnectionRepository, MockEventRepository, MockMetricRepository, SimpleMockAdapterFactory,
};

// ============================================================================
// Mock Implementations for Testing
// ============================================================================

/// Mock ConnectionRepository for testing

/// Mock EventRepository for testing

/// Mock MetricRepository that returns empty (no pre-existing metrics)

// ============================================================================
// Helper to create test fixtures
// ============================================================================

fn create_test_fixtures() -> (
    Arc<MockConnectionRepository>,
    MetricRepositoryRef,
    ConnectionReferenceRepositoryRef,
    Arc<AdapterLifecycleManager>,
    broadcast::Sender<SystemEvent>,
    VfsRef,
) {
    let connection_repo = Arc::new(MockConnectionRepository::new());
    let event_repo = Arc::new(MockEventRepository::new());
    let event_capture_service = Arc::new(EventCaptureService::new(event_repo));
    let (broadcast_tx, _) = broadcast::channel::<MetricEvent>(100);
    let adapter_factory: DapAdapterFactoryRef = Arc::new(SimpleMockAdapterFactory);
    let metric_repo: MetricRepositoryRef = Arc::new(MockMetricRepository::new());
    let reference_repo: ConnectionReferenceRepositoryRef =
        Arc::new(MockConnectionReferenceRepository::new());
    let vfs: VfsRef = Arc::new(MockVfs::new());

    let (system_event_tx, _) = broadcast::channel::<SystemEvent>(100);
    let adapter_lifecycle_manager = Arc::new(AdapterLifecycleManager::new(
        event_capture_service,
        broadcast_tx,
        system_event_tx.clone(),
        adapter_factory,
        metric_repo.clone(),
        Arc::clone(&connection_repo) as ConnectionRepositoryRef,
        vfs.clone(),
    ));

    (
        connection_repo,
        metric_repo,
        reference_repo,
        adapter_lifecycle_manager,
        system_event_tx,
        vfs,
    )
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_create_connection_with_auto_generated_id() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Create identity (UUID will be deterministic)
    let identity = sample_connection_identity();

    // Act
    let result = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            identity,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await;

    // Assert
    assert!(result.is_ok());
    let connection_id = result.unwrap();

    // ID should be deterministic UUID from identity
    let expected_uuid = sample_connection_identity().to_uuid();
    assert_eq!(connection_id.0, expected_uuid);

    // Connection should be saved in repository
    let saved_conn = repo.get_connection(&connection_id).await;
    assert!(saved_conn.is_some());
    let conn = saved_conn.unwrap();
    assert_eq!(conn.host, "127.0.0.1");
    assert_eq!(conn.port, 5678);
    assert_eq!(conn.name, Some("test-app".to_string()));
    assert_eq!(conn.workspace_root, "/workspace");
    assert_eq!(conn.hostname, "test-host");
    // After successful creation, status should be Connected
    assert_eq!(conn.status, ConnectionStatus::Connected);
}

#[tokio::test]
async fn test_create_connection_with_identity() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Create identity with custom name
    let identity = ConnectionIdentity::new(
        "my-custom-connection",
        SourceLanguage::Python,
        "/my/workspace",
        "my-host",
    );
    let expected_uuid = identity.to_uuid();

    // Act
    let result = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            identity,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await;

    // Assert
    assert!(result.is_ok());
    let connection_id = result.unwrap();

    // ID should be deterministic UUID from identity
    assert_eq!(connection_id.0, expected_uuid);

    // Connection should be saved with identity fields
    let saved_conn = repo.get_connection(&connection_id).await;
    assert!(saved_conn.is_some());
    let conn = saved_conn.unwrap();
    assert_eq!(conn.name, Some("my-custom-connection".to_string()));
    assert_eq!(conn.workspace_root, "/my/workspace");
    assert_eq!(conn.hostname, "my-host");
}

#[tokio::test]
async fn test_create_connection_validates_port_range() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    let identity = sample_connection_identity();

    // Act - Port below 1024 should fail
    let result = service
        .create_connection(
            "127.0.0.1".to_string(),
            80,
            identity,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await;

    // Assert
    assert!(result.is_err());
    if let Err(err) = result {
        assert!(matches!(err, Error::InvalidConfig(_)));
        assert!(err.to_string().contains("below 1024"));
    }

    // No connection should be saved
    assert_eq!(repo.connection_count().await, 0);
}

#[tokio::test]
async fn test_create_connection_validates_host() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    let identity = sample_connection_identity();

    // Act - Empty host should fail
    let result = service
        .create_connection(String::new(), 5678, identity, None, None, false)
        .await;

    // Assert
    assert!(result.is_err());
    if let Err(err) = result {
        assert!(matches!(err, Error::InvalidConfig(_)));
        assert!(err.to_string().contains("Host cannot be empty"));
    }
}

#[tokio::test]
async fn test_create_connection_starts_adapter() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo,
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager.clone(),
        system_event_tx,
        vfs,
    );

    let identity = sample_connection_identity();

    // Act
    let result = service
        .create_connection(
            "localhost".to_string(),
            5679,
            identity,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await;

    // Assert
    assert!(result.is_ok());
    let connection_id = result.unwrap();

    // Adapter should be registered and running via lifecycle manager
    assert!(service.has_running_adapter(&connection_id).await);

    // We can also get the adapter
    let adapter = service.get_adapter(&connection_id).await;
    assert!(adapter.is_some());
}

#[tokio::test]
async fn test_disconnect_stops_adapter_and_updates_status() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    let identity = sample_connection_identity();

    // First create a connection
    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            identity,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Verify adapter is running
    assert!(service.has_running_adapter(&connection_id).await);

    // Act - Disconnect
    let result = service.disconnect(&connection_id).await;

    // Assert
    assert!(result.is_ok());

    // Status should be updated to Disconnected
    let conn = repo.get_connection(&connection_id).await.unwrap();
    assert_eq!(conn.status, ConnectionStatus::Disconnected);

    // Adapter should no longer be running
    assert!(!service.has_running_adapter(&connection_id).await);
}

#[tokio::test]
async fn test_disconnect_nonexistent_adapter_succeeds() {
    // AdapterLifecycleManager.stop_adapter succeeds even for nonexistent adapters
    // But updating connection status will fail if connection doesn't exist
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo,
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Act - Try to disconnect nonexistent connection
    let nonexistent_id = ConnectionId::new("does-not-exist");
    let result = service.disconnect(&nonexistent_id).await;

    // Assert - Should fail because connection doesn't exist in repo
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_connections_returns_all() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Create multiple connections with different identities
    let identity1 = ConnectionIdentity::new("app1", SourceLanguage::Python, "/workspace1", "host1");
    service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            identity1,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    let identity2 = ConnectionIdentity::new("app2", SourceLanguage::Python, "/workspace2", "host2");
    service
        .create_connection(
            "127.0.0.1".to_string(),
            5679,
            identity2,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    let identity3 = ConnectionIdentity::new("app3", SourceLanguage::Python, "/workspace3", "host3");
    service
        .create_connection(
            "localhost".to_string(),
            5680,
            identity3,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Act
    let result = service.list_connections().await;

    // Assert
    assert!(result.is_ok());
    let connections = result.unwrap();
    assert_eq!(connections.len(), 3);

    // Verify all connections are present
    let hosts: Vec<_> = connections.iter().map(|c| &c.host).collect();
    assert!(hosts.contains(&&"127.0.0.1".to_string()));
    assert!(hosts.contains(&&"localhost".to_string()));
}

#[tokio::test]
async fn test_list_connections_empty_when_no_connections() {
    // Arrange
    let (_repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        _repo,
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Act
    let result = service.list_connections().await;

    // Assert
    assert!(result.is_ok());
    let connections = result.unwrap();
    assert_eq!(connections.len(), 0);
}

#[tokio::test]
async fn test_get_connection_by_id() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo,
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    let identity = ConnectionIdentity::new(
        "test-conn",
        SourceLanguage::Python,
        "/workspace",
        "test-host",
    );
    let expected_uuid = identity.to_uuid();

    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            identity,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Act
    let result = service.get_connection(&connection_id).await;

    // Assert
    assert!(result.is_ok());
    let conn = result.unwrap();
    assert!(conn.is_some());
    let conn = conn.unwrap();
    assert_eq!(conn.id.0, expected_uuid);
    assert_eq!(conn.name, Some("test-conn".to_string()));
    assert_eq!(conn.host, "127.0.0.1");
    assert_eq!(conn.port, 5678);
}

#[tokio::test]
async fn test_get_connection_returns_none_for_nonexistent() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo,
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Act
    let nonexistent_id = ConnectionId::new("does-not-exist");
    let result = service.get_connection(&nonexistent_id).await;

    // Assert
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[tokio::test]
async fn test_get_adapter_returns_adapter_for_active_connection() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo,
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    let identity = sample_connection_identity();

    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            identity,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Act
    let adapter = service.get_adapter(&connection_id).await;

    // Assert
    assert!(adapter.is_some());
    let adapter = adapter.unwrap();
    assert!(adapter.is_connected());
}

#[tokio::test]
async fn test_get_adapter_returns_none_after_disconnect() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo,
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    let identity = sample_connection_identity();

    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            identity,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Disconnect
    service.disconnect(&connection_id).await.unwrap();

    // Act
    let adapter = service.get_adapter(&connection_id).await;

    // Assert
    assert!(adapter.is_none());
}

/// Test that create_connection returns existing connection when already Connected.
/// This tests the early-exit path when connection is already fully established.
#[tokio::test]
async fn test_create_connection_returns_existing_when_connected() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager.clone(),
        system_event_tx,
        vfs,
    );

    // Create identity (same identity for both calls)
    let identity = ConnectionIdentity::new(
        "test-existing",
        SourceLanguage::Python,
        "/workspace",
        "test-host",
    );

    // First, create a connection normally
    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            9997,
            identity.clone(),
            None,
            None,
            false,
        )
        .await
        .unwrap();

    // Verify it's connected
    let conn = repo.get_connection(&connection_id).await.unwrap();
    assert_eq!(conn.status, ConnectionStatus::Connected);
    assert!(service.has_running_adapter(&connection_id).await);

    // Act - Call create_connection again with the same identity (idempotency)
    let result = service
        .create_connection("127.0.0.1".to_string(), 9997, identity, None, None, false)
        .await;

    // Assert - Should return the existing connection without error
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), connection_id);
}

// ============================================================================
// TTL and Cleanup Tests
// ============================================================================

#[tokio::test]
async fn test_cleanup_stale_connections_removes_old() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Create a connection
    let identity = sample_connection_identity();
    let connection_id = service
        .create_connection("127.0.0.1".to_string(), 5678, identity, None, None, false)
        .await
        .unwrap();

    // Manually set last_active to 10 days ago
    let ten_days_ago = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::days(10))
        .unwrap()
        .timestamp_micros();

    {
        let mut conn = repo.get_connection(&connection_id).await.unwrap();
        conn.last_active = ten_days_ago;
        repo.update(&conn).await.unwrap();
    }

    // Act - Cleanup with TTL of 7 days
    let result = service.cleanup_stale_connections(7).await;

    // Assert
    assert!(result.is_ok());
    let removed = result.unwrap();
    assert_eq!(removed, 1); // Should remove 1 connection

    // Connection should be deleted
    assert!(repo.get_connection(&connection_id).await.is_none());
}

#[tokio::test]
async fn test_cleanup_respects_ttl_indefinite() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Create a connection
    let identity = sample_connection_identity();
    let connection_id = service
        .create_connection("127.0.0.1".to_string(), 5678, identity, None, None, false)
        .await
        .unwrap();

    // Set last_active to 100 days ago
    let long_ago = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::days(100))
        .unwrap()
        .timestamp_micros();

    {
        let mut conn = repo.get_connection(&connection_id).await.unwrap();
        conn.last_active = long_ago;
        repo.update(&conn).await.unwrap();
    }

    // Act - Cleanup with TTL = -1 (indefinite)
    let result = service.cleanup_stale_connections(-1).await;

    // Assert
    assert!(result.is_ok());
    let removed = result.unwrap();
    assert_eq!(removed, 0); // Should NOT remove any connections

    // Connection should still exist
    assert!(repo.get_connection(&connection_id).await.is_some());
}

#[tokio::test]
async fn test_touch_connection_updates_last_active() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Create a connection
    let identity = sample_connection_identity();
    let connection_id = service
        .create_connection("127.0.0.1".to_string(), 5678, identity, None, None, false)
        .await
        .unwrap();

    // Get initial last_active timestamp
    let initial_last_active = {
        let conn = repo.get_connection(&connection_id).await.unwrap();
        conn.last_active
    };

    // Wait a bit to ensure timestamp changes
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Act - Touch the connection
    let result = service.touch_connection(&connection_id).await;

    // Assert
    assert!(result.is_ok());

    // last_active should be updated
    let updated_last_active = {
        let conn = repo.get_connection(&connection_id).await.unwrap();
        conn.last_active
    };

    assert!(updated_last_active > initial_last_active);
}

#[tokio::test]
async fn test_disconnect_all_connections() {
    // Arrange
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Create multiple connections
    let identity1 = ConnectionIdentity::new("app1", SourceLanguage::Python, "/ws1", "host1");
    let conn1 = service
        .create_connection("127.0.0.1".to_string(), 5678, identity1, None, None, false)
        .await
        .unwrap();

    let identity2 = ConnectionIdentity::new("app2", SourceLanguage::Python, "/ws2", "host2");
    let conn2 = service
        .create_connection("127.0.0.1".to_string(), 5679, identity2, None, None, false)
        .await
        .unwrap();

    // Verify both are connected
    assert!(service.has_running_adapter(&conn1).await);
    assert!(service.has_running_adapter(&conn2).await);

    // Act - Disconnect all via admin (force disconnect)
    let result = service.admin_disconnect_all().await;

    // Assert
    assert!(result.is_ok());
    let count = result.unwrap();
    assert_eq!(count, 2);

    // Both adapters should be stopped
    assert!(!service.has_running_adapter(&conn1).await);
    assert!(!service.has_running_adapter(&conn2).await);

    // Both connections should have Disconnected status
    let c1 = repo.get_connection(&conn1).await.unwrap();
    let c2 = repo.get_connection(&conn2).await.unwrap();
    assert_eq!(c1.status, ConnectionStatus::Disconnected);
    assert_eq!(c2.status, ConnectionStatus::Disconnected);
}

// ============================================================================
// Container Restart / Metric Migration Tests
// ============================================================================

/// Create test fixtures that expose the full (stateful) metric repository so tests
/// can pre-populate metrics and inspect migration results.
fn create_migration_fixtures() -> (
    Arc<MockConnectionRepository>,
    Arc<FullMockMetricRepository>,
    ConnectionReferenceRepositoryRef,
    Arc<AdapterLifecycleManager>,
    broadcast::Sender<SystemEvent>,
    VfsRef,
) {
    let connection_repo = Arc::new(MockConnectionRepository::new());
    let metric_repo = Arc::new(FullMockMetricRepository::new());
    let event_repo = Arc::new(MockEventRepository::new());
    let event_capture_service = Arc::new(EventCaptureService::new(event_repo));
    let (broadcast_tx, _) = broadcast::channel::<MetricEvent>(100);
    let adapter_factory: DapAdapterFactoryRef = Arc::new(SimpleMockAdapterFactory);
    let reference_repo: ConnectionReferenceRepositoryRef =
        Arc::new(MockConnectionReferenceRepository::new());
    let vfs: VfsRef = Arc::new(MockVfs::new());

    let (system_event_tx, _) = broadcast::channel::<SystemEvent>(100);
    let adapter_lifecycle_manager = Arc::new(AdapterLifecycleManager::new(
        event_capture_service,
        broadcast_tx,
        system_event_tx.clone(),
        adapter_factory,
        metric_repo.clone() as MetricRepositoryRef,
        Arc::clone(&connection_repo) as ConnectionRepositoryRef,
        vfs.clone(),
    ));

    (
        connection_repo,
        metric_repo,
        reference_repo,
        adapter_lifecycle_manager,
        system_event_tx,
        vfs,
    )
}

/// Helper: create and immediately disconnect a connection, leaving it Disconnected with metrics.
async fn create_disconnected_connection_with_metrics(
    service: &ConnectionService,
    metric_repo: &Arc<FullMockMetricRepository>,
    identity: ConnectionIdentity,
    metrics: &[(&str, &str, u32)], // (metric_name, file, line)
) -> ConnectionId {
    let conn_id = service
        .create_connection("127.0.0.1".to_string(), 5678, identity, None, None, false)
        .await
        .expect("create_connection should succeed");

    // Manually save metrics associated with this connection
    for (name, file, line) in metrics {
        let mut metric = sample_metric_with_connection(name, &conn_id.0);
        metric.location = Location {
            file: file.to_string(),
            line: *line,
        };
        metric_repo
            .save_with_options(&metric, false)
            .await
            .expect("save metric should succeed");
    }

    // Disconnect to put it in Disconnected state (simulating container stop)
    service
        .disconnect(&conn_id)
        .await
        .expect("disconnect should succeed");

    conn_id
}

/// Container restart: metrics migrate from old connection to new one.
///
/// Scenario: container restarts with new hostname. The old connection becomes
/// Disconnected. When the new container registers, existing metrics should
/// automatically move to the new connection_id.
#[tokio::test]
async fn test_container_restart_migrates_metrics_to_new_connection() {
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_migration_fixtures();
    let service = ConnectionService::new(
        repo.clone() as ConnectionRepositoryRef,
        metric_repo.clone() as MetricRepositoryRef,
        reference_repo,
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Arrange: old container connected and then stopped
    let old_identity =
        ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-A");
    let old_id = create_disconnected_connection_with_metrics(
        &service,
        &metric_repo,
        old_identity,
        &[
            ("metric1", "/workspace/app.py", 10),
            ("metric2", "/workspace/app.py", 20),
        ],
    )
    .await;

    assert_eq!(metric_repo.count(), 2);

    // Act: new container registers with same project but new hostname
    let new_identity =
        ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-B");
    let new_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5679,
            new_identity,
            None,
            None,
            false,
        )
        .await
        .expect("new connection should succeed");

    // Assert: connection IDs differ (different hostnames)
    assert_ne!(old_id, new_id);

    // Old connection should be gone
    assert!(
        repo.get_connection(&old_id).await.is_none(),
        "stale connection should be deleted"
    );

    // New connection should be Connected
    let new_conn = repo.get_connection(&new_id).await.unwrap();
    assert_eq!(new_conn.status, ConnectionStatus::Connected);

    // Both metrics should now belong to the new connection
    let metrics_on_new = metric_repo.find_by_connection_id(&new_id).await.unwrap();
    assert_eq!(
        metrics_on_new.len(),
        2,
        "both metrics should be migrated to the new connection"
    );
    assert!(metrics_on_new.iter().all(|m| m.connection_id == new_id));

    // No orphaned metrics should remain under the old ID
    let metrics_on_old = metric_repo.find_by_connection_id(&old_id).await.unwrap();
    assert!(
        metrics_on_old.is_empty(),
        "no metrics should remain on the deleted connection"
    );

    // Total metric count unchanged (migration, not duplication)
    assert_eq!(metric_repo.count(), 2);
}

/// A Connected (still-alive) connection for the same project must NOT be touched.
///
/// Two instances of the same app could be running simultaneously. The stale-cleanup
/// filter only targets Disconnected/Failed connections.
#[tokio::test]
async fn test_container_restart_preserves_connected_sibling() {
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_migration_fixtures();
    let service = ConnectionService::new(
        repo.clone() as ConnectionRepositoryRef,
        metric_repo.clone() as MetricRepositoryRef,
        reference_repo,
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Arrange: first container is still CONNECTED (e.g. blue-green deploy)
    let live_identity =
        ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-live");
    let live_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            live_identity,
            None,
            None,
            false,
        )
        .await
        .expect("create live connection");

    // Save a metric on the live connection
    let mut live_metric = sample_metric_with_connection("live-metric", &live_id.0);
    live_metric.location = Location {
        file: "/workspace/app.py".to_string(),
        line: 42,
    };
    metric_repo
        .save_with_options(&live_metric, false)
        .await
        .unwrap();

    // Act: a new container registers (different hostname)
    let new_identity =
        ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-new");
    let new_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5679,
            new_identity,
            None,
            None,
            false,
        )
        .await
        .expect("new connection should succeed");

    // Assert: live connection should still exist and be Connected
    let live_conn = repo.get_connection(&live_id).await;
    assert!(live_conn.is_some(), "live connection must not be deleted");
    assert_eq!(live_conn.unwrap().status, ConnectionStatus::Connected);

    // Live metric should still belong to the live connection (not migrated away)
    let metrics_on_live = metric_repo.find_by_connection_id(&live_id).await.unwrap();
    assert_eq!(metrics_on_live.len(), 1);

    // New connection should be present and connected
    assert!(repo.get_connection(&new_id).await.is_some());
    assert_eq!(metric_repo.count(), 1); // only the live metric, no new metrics
}

/// When the same project has had multiple crashed containers (e.g., flapping restarts),
/// all of their Disconnected connections should be cleaned up and their metrics merged.
#[tokio::test]
async fn test_container_restart_migrates_from_multiple_stale_connections() {
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_migration_fixtures();
    let service = ConnectionService::new(
        repo.clone() as ConnectionRepositoryRef,
        metric_repo.clone() as MetricRepositoryRef,
        reference_repo,
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Arrange: two previous container instances each disconnected with their own metric
    let old_id_1 = create_disconnected_connection_with_metrics(
        &service,
        &metric_repo,
        ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-v1"),
        &[("metric-v1", "/workspace/app.py", 10)],
    )
    .await;

    let old_id_2 = create_disconnected_connection_with_metrics(
        &service,
        &metric_repo,
        ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-v2"),
        &[("metric-v2", "/workspace/app.py", 20)],
    )
    .await;

    assert_eq!(metric_repo.count(), 2);

    // Act: third restart
    let new_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5680,
            ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-v3"),
            None,
            None,
            false,
        )
        .await
        .expect("new connection should succeed");

    // Assert: both old connections deleted
    assert!(repo.get_connection(&old_id_1).await.is_none());
    assert!(repo.get_connection(&old_id_2).await.is_none());

    // All metrics migrated to new connection
    let metrics_on_new = metric_repo.find_by_connection_id(&new_id).await.unwrap();
    assert_eq!(metrics_on_new.len(), 2, "both metrics should migrate");

    // Total unchanged
    assert_eq!(metric_repo.count(), 2);
}

/// A metric that already exists on the new connection at the same location must not be
/// duplicated. The new connection's version wins (ON CONFLICT IGNORE semantics).
#[tokio::test]
async fn test_container_restart_skips_conflicting_metric_locations() {
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_migration_fixtures();
    let service = ConnectionService::new(
        repo.clone() as ConnectionRepositoryRef,
        metric_repo.clone() as MetricRepositoryRef,
        reference_repo,
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // Arrange: old container (Disconnected) has a metric at line 42
    let old_id = create_disconnected_connection_with_metrics(
        &service,
        &metric_repo,
        ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-old"),
        &[("old-metric", "/workspace/app.py", 42)],
    )
    .await;

    // Also pre-seed a metric on what will be the new connection_id at the same location.
    // This simulates a scenario where the new connection already had a metric added before
    // the migration runs (e.g., added by a concurrent tool call).
    let new_identity =
        ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-new");
    let new_id = ConnectionId::new(&new_identity.to_uuid());
    let mut conflict_metric = sample_metric_with_connection("new-metric", &new_id.0);
    conflict_metric.location = Location {
        file: "/workspace/app.py".to_string(),
        line: 42,
    };
    metric_repo
        .save_with_options(&conflict_metric, false)
        .await
        .unwrap();

    assert_eq!(metric_repo.count(), 2);

    // Act: new container registers
    service
        .create_connection(
            "127.0.0.1".to_string(),
            5679,
            new_identity,
            None,
            None,
            false,
        )
        .await
        .expect("new connection should succeed");

    // Assert: old connection cleaned up
    assert!(repo.get_connection(&old_id).await.is_none());

    // The conflicting old metric could not be migrated, so it stayed on old_id.
    // delete_connection then cleaned it up. Only the pre-seeded new metric survives.
    assert_eq!(
        metric_repo.count(),
        1,
        "conflicting old metric removed during cleanup"
    );

    let metrics_on_new = metric_repo.find_by_connection_id(&new_id).await.unwrap();
    assert_eq!(metrics_on_new.len(), 1);

    // The surviving metric at line 42 should be the one pre-seeded on the new connection
    assert_eq!(
        metrics_on_new[0].name, "new-metric",
        "new connection's version wins on conflict"
    );
}

/// When there are no stale connections, create_connection should work normally
/// without attempting any migration.
#[tokio::test]
async fn test_container_restart_no_stale_connections_is_noop() {
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_migration_fixtures();
    let service = ConnectionService::new(
        repo.clone() as ConnectionRepositoryRef,
        metric_repo.clone() as MetricRepositoryRef,
        reference_repo,
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    // No pre-existing connections
    let identity =
        ConnectionIdentity::new("my-app", SourceLanguage::Python, "/workspace", "host-A");
    let conn_id = service
        .create_connection("127.0.0.1".to_string(), 5678, identity, None, None, false)
        .await
        .expect("create connection should succeed");

    // Assert: connection is present and connected, no metrics, no errors
    assert_eq!(repo.connection_count().await, 1);
    assert_eq!(metric_repo.count(), 0);
    let conn = repo.get_connection(&conn_id).await.unwrap();
    assert_eq!(conn.status, ConnectionStatus::Connected);
}

// ============================================================================
// Unit tests for MockMetricRepository::migrate_connection_id
// ============================================================================

#[tokio::test]
async fn test_mock_metric_repo_migrate_connection_id_basic() {
    use detrix_ports::MetricRepository;

    let repo = FullMockMetricRepository::new();
    let old_id = ConnectionId::new("old-conn");
    let new_id = ConnectionId::new("new-conn");

    // Save two metrics on the old connection
    let mut m1 = sample_metric_with_connection("m1", "old-conn");
    m1.location = Location {
        file: "app.py".to_string(),
        line: 10,
    };
    let mut m2 = sample_metric_with_connection("m2", "old-conn");
    m2.location = Location {
        file: "app.py".to_string(),
        line: 20,
    };
    repo.save_with_options(&m1, false).await.unwrap();
    repo.save_with_options(&m2, false).await.unwrap();

    // Migrate
    let migrated = repo.migrate_connection_id(&old_id, &new_id).await.unwrap();
    assert_eq!(migrated, 2);

    // All metrics now on new connection
    let on_new = repo.find_by_connection_id(&new_id).await.unwrap();
    assert_eq!(on_new.len(), 2);
    assert!(on_new.iter().all(|m| m.connection_id == new_id));

    // Nothing on old connection
    let on_old = repo.find_by_connection_id(&old_id).await.unwrap();
    assert!(on_old.is_empty());
}

#[tokio::test]
async fn test_mock_metric_repo_migrate_connection_id_conflict_skipped() {
    use detrix_ports::MetricRepository;

    let repo = FullMockMetricRepository::new();
    let old_id = ConnectionId::new("old-conn");
    let new_id = ConnectionId::new("new-conn");

    // Old: metric at line 10
    let mut old_metric = sample_metric_with_connection("old-metric", "old-conn");
    old_metric.location = Location {
        file: "app.py".to_string(),
        line: 10,
    };
    repo.save_with_options(&old_metric, false).await.unwrap();

    // New: already has a metric at line 10 (conflict)
    let mut new_metric = sample_metric_with_connection("new-metric", "new-conn");
    new_metric.location = Location {
        file: "app.py".to_string(),
        line: 10,
    };
    repo.save_with_options(&new_metric, false).await.unwrap();

    // Migrate: the old metric at line 10 should be skipped
    let migrated = repo.migrate_connection_id(&old_id, &new_id).await.unwrap();
    assert_eq!(migrated, 0, "conflicting metric should be skipped");

    // New connection still has exactly its original metric
    let on_new = repo.find_by_connection_id(&new_id).await.unwrap();
    assert_eq!(on_new.len(), 1);
    assert_eq!(on_new[0].name, "new-metric");

    // Old metric remains on old connection (not migrated, not deleted)
    let on_old = repo.find_by_connection_id(&old_id).await.unwrap();
    assert_eq!(on_old.len(), 1);
}

#[tokio::test]
async fn test_mock_metric_repo_migrate_empty_source_is_noop() {
    use detrix_ports::MetricRepository;

    let repo = FullMockMetricRepository::new();
    let old_id = ConnectionId::new("empty-conn");
    let new_id = ConnectionId::new("new-conn");

    let migrated = repo.migrate_connection_id(&old_id, &new_id).await.unwrap();
    assert_eq!(migrated, 0);
    assert_eq!(repo.count(), 0);
}

/// Regression test for the startup-restore race condition.
///
/// `restore_connections_on_startup` is spawned as a background task. Under heavy parallel
/// load it may run AFTER the current session has already created and connected connections.
/// The fix: the caller (serve.rs) captures a snapshot of connections BEFORE the HTTP
/// server starts, and passes that snapshot to the restore task.  Connections created
/// AFTER the server starts are therefore never in the snapshot and cannot be touched.
///
/// This test verifies BOTH halves of the fix:
/// 1. An empty snapshot → restore does nothing even if active connections exist.
/// 2. A snapshot with a Connected connection → restore still skips it (has_adapter guard).
#[tokio::test]
async fn test_restore_connections_skips_already_connected() {
    // Arrange: set up service and create a connected connection
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    let identity = ConnectionIdentity::new(
        "race-test-app",
        SourceLanguage::Python,
        "/workspace",
        "host1",
    );

    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            identity,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Verify connection is Connected and adapter is running
    let conn = repo.get_connection(&connection_id).await.unwrap();
    assert_eq!(conn.status, ConnectionStatus::Connected);
    assert!(service.has_running_adapter(&connection_id).await);

    // Case 1: empty snapshot (simulate: server started → client created connection → restore runs
    // with the pre-server snapshot that didn't include this connection yet).
    // Even if the restore task runs after the connection is created, the empty snapshot means
    // it will not process any connections → the active connection is untouched.
    let (reconnected, deleted) = service.restore_connections_on_startup(vec![]).await;

    assert_eq!(
        reconnected, 0,
        "empty snapshot: restore should not reconnect anything"
    );
    assert_eq!(
        deleted, 0,
        "empty snapshot: restore should not delete anything"
    );

    let conn_after = repo.get_connection(&connection_id).await.unwrap();
    assert_eq!(
        conn_after.status,
        ConnectionStatus::Connected,
        "empty snapshot: restore must not change status of an active connection"
    );
    assert!(
        service.has_running_adapter(&connection_id).await,
        "empty snapshot: restore must not stop the running adapter"
    );

    // Case 2: snapshot includes the connected connection (e.g. snapshot was taken late).
    // The has_adapter guard ensures even a snapshotted Connected connection is skipped.
    let snapshot = service.list_connections().await.unwrap();
    let (reconnected2, deleted2) = service.restore_connections_on_startup(snapshot).await;

    assert_eq!(
        reconnected2, 0,
        "late snapshot: Connected connection must be skipped"
    );
    assert_eq!(
        deleted2, 0,
        "late snapshot: Connected connection must not be deleted"
    );

    let conn_final = repo.get_connection(&connection_id).await.unwrap();
    assert_eq!(
        conn_final.status,
        ConnectionStatus::Connected,
        "late snapshot: restore must not change status of an active connection"
    );
    assert!(
        service.has_running_adapter(&connection_id).await,
        "late snapshot: restore must not stop the running adapter"
    );
}

#[tokio::test]
async fn test_restore_connections_skips_agent_managed_snapshot_entries() {
    let (repo, metric_repo, reference_repo, lifecycle_manager, system_event_tx, vfs) =
        create_test_fixtures();
    let service = ConnectionService::new(
        repo.clone(),
        metric_repo,
        reference_repo.clone(),
        lifecycle_manager,
        system_event_tx,
        vfs,
    );

    let identity = ConnectionIdentity::new("agent/demo/exe", SourceLanguage::Go, "/", "host1");
    let mut conn =
        Connection::new_with_identity(identity, "/proc/1/exe".to_string(), 1024).unwrap();
    conn.safe_mode = true;
    repo.save(&conn).await.unwrap();

    let snapshot = repo.list_all().await.unwrap();
    let (reconnected, deleted) = service.restore_connections_on_startup(snapshot).await;

    assert_eq!(
        reconnected, 0,
        "agent-managed rows must be ignored by startup restore"
    );
    assert_eq!(
        deleted, 0,
        "agent-managed rows must not be deleted by startup restore"
    );

    let conn_after = repo.get_connection(&conn.id).await.unwrap();
    assert_eq!(
        conn_after.status,
        ConnectionStatus::Disconnected,
        "agent-managed row should remain untouched until the agent re-registers"
    );
    assert!(
        !service.has_running_adapter(&conn.id).await,
        "daemon must not start a local adapter for agent-managed rows during startup restore"
    );
}
