//! Tests for ConnectionService
//!
//! Following TDD: Write tests first, then implement ConnectionService to make them pass.

use detrix_application::{
    AdapterLifecycleManager, ConnectionRepositoryRef, ConnectionService, DapAdapterFactoryRef,
    EventCaptureService, MetricRepositoryRef,
};
use detrix_core::{ConnectionId, ConnectionStatus, Error, MetricEvent, SystemEvent};
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
    Arc<AdapterLifecycleManager>,
    broadcast::Sender<SystemEvent>,
) {
    let connection_repo = Arc::new(MockConnectionRepository::new());
    let event_repo = Arc::new(MockEventRepository::new());
    let event_capture_service = Arc::new(EventCaptureService::new(event_repo));
    let (broadcast_tx, _) = broadcast::channel::<MetricEvent>(100);
    let adapter_factory: DapAdapterFactoryRef = Arc::new(SimpleMockAdapterFactory);
    let metric_repo: MetricRepositoryRef = Arc::new(MockMetricRepository::new());

    let (system_event_tx, _) = broadcast::channel::<SystemEvent>(100);
    let adapter_lifecycle_manager = Arc::new(AdapterLifecycleManager::new(
        event_capture_service,
        broadcast_tx,
        system_event_tx.clone(),
        adapter_factory,
        metric_repo,
        Arc::clone(&connection_repo) as ConnectionRepositoryRef,
    ));

    (connection_repo, adapter_lifecycle_manager, system_event_tx)
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_create_connection_with_auto_generated_id() {
    // Arrange
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo.clone(), lifecycle_manager, system_event_tx);

    // Act
    let result = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            "python".to_string(),
            None,
            None,
        )
        .await;

    // Assert
    assert!(result.is_ok());
    let connection_id = result.unwrap();

    // ID should be auto-generated from host:port
    assert_eq!(connection_id.0, "127_0_0_1_5678");

    // Connection should be saved in repository
    let saved_conn = repo.get_connection(&connection_id).await;
    assert!(saved_conn.is_some());
    let conn = saved_conn.unwrap();
    assert_eq!(conn.host, "127.0.0.1");
    assert_eq!(conn.port, 5678);
    // After successful creation, status should be Connected
    assert_eq!(conn.status, ConnectionStatus::Connected);
}

#[tokio::test]
async fn test_create_connection_with_custom_id() {
    // Arrange
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo.clone(), lifecycle_manager, system_event_tx);

    // Act
    let result = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            "python".to_string(),
            Some("my-custom-connection".to_string()),
            None,
        )
        .await;

    // Assert
    assert!(result.is_ok());
    let connection_id = result.unwrap();

    // ID should be the custom one
    assert_eq!(connection_id.0, "my-custom-connection");

    // Connection should be saved
    let saved_conn = repo.get_connection(&connection_id).await;
    assert!(saved_conn.is_some());
}

#[tokio::test]
async fn test_create_connection_validates_port_range() {
    // Arrange
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo.clone(), lifecycle_manager, system_event_tx);

    // Act - Port below 1024 should fail
    let result = service
        .create_connection(
            "127.0.0.1".to_string(),
            80,
            "python".to_string(),
            None,
            None,
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
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo.clone(), lifecycle_manager, system_event_tx);

    // Act - Empty host should fail
    let result = service
        .create_connection(String::new(), 5678, "python".to_string(), None, None)
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
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo, lifecycle_manager.clone(), system_event_tx);

    // Act
    let result = service
        .create_connection(
            "localhost".to_string(),
            5679,
            "python".to_string(),
            None,
            None,
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
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo.clone(), lifecycle_manager, system_event_tx);

    // First create a connection
    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            "python".to_string(),
            None,
            None,
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
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo, lifecycle_manager, system_event_tx);

    // Act - Try to disconnect nonexistent connection
    let nonexistent_id = ConnectionId::new("does-not-exist");
    let result = service.disconnect(&nonexistent_id).await;

    // Assert - Should fail because connection doesn't exist in repo
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_connections_returns_all() {
    // Arrange
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo.clone(), lifecycle_manager, system_event_tx);

    // Create multiple connections
    service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            "python".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    service
        .create_connection(
            "127.0.0.1".to_string(),
            5679,
            "python".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    service
        .create_connection(
            "localhost".to_string(),
            5680,
            "python".to_string(),
            None,
            None,
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
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo, lifecycle_manager, system_event_tx);

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
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo, lifecycle_manager, system_event_tx);

    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            "python".to_string(),
            Some("test-conn".to_string()),
            None,
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
    assert_eq!(conn.id.0, "test-conn");
    assert_eq!(conn.host, "127.0.0.1");
    assert_eq!(conn.port, 5678);
}

#[tokio::test]
async fn test_get_connection_returns_none_for_nonexistent() {
    // Arrange
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo, lifecycle_manager, system_event_tx);

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
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo, lifecycle_manager, system_event_tx);

    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            "python".to_string(),
            None,
            None,
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
    let (repo, lifecycle_manager, system_event_tx) = create_test_fixtures();
    let service = ConnectionService::new(repo, lifecycle_manager, system_event_tx);

    let connection_id = service
        .create_connection(
            "127.0.0.1".to_string(),
            5678,
            "python".to_string(),
            None,
            None,
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
