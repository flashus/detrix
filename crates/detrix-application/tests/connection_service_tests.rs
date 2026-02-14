//! Tests for ConnectionService
//!
//! Following TDD: Write tests first, then implement ConnectionService to make them pass.

use detrix_application::{
    AdapterLifecycleManager, ConnectionReferenceRepositoryRef, ConnectionRepository,
    ConnectionRepositoryRef, ConnectionService, DapAdapterFactoryRef, EventCaptureService,
    MetricRepositoryRef,
};
use detrix_core::{
    ConnectionId, ConnectionIdentity, ConnectionStatus, Error, MetricEvent, SourceLanguage,
    SystemEvent,
};
use detrix_ports::VfsRef;
use detrix_testing::fixtures::sample_connection_identity;
use detrix_testing::{MockConnectionReferenceRepository, MockVfs};
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
