//! Remote app control service
//!
//! Orchestrates wake/sleep operations on remote applications.
//! The actual HTTP communication is delegated to the `RemoteAppControl` port.

use detrix_config::constants::{
    DEFAULT_REMOTE_APP_POLL_INTERVAL_MS, DEFAULT_REMOTE_APP_POLL_MAX_RETRIES,
};
use detrix_core::{ConnectionId, Result};
use detrix_logging::{info, warn};
use detrix_ports::{
    ConnectionLookupRef, RemoteAppControlRef, RemoteSleepResponse, RemoteWakeResponse,
};

/// Maximum number of polling attempts when verifying a connection appeared.
const CONNECTION_POLL_MAX_RETRIES: u32 = DEFAULT_REMOTE_APP_POLL_MAX_RETRIES;
/// Interval between polling attempts (milliseconds).
const CONNECTION_POLL_INTERVAL_MS: u64 = DEFAULT_REMOTE_APP_POLL_INTERVAL_MS;

/// Service for waking and sleeping remote applications.
///
/// Delegates HTTP communication to the `RemoteAppControl` port trait,
/// and adds application-level behavior like connection verification.
pub struct RemoteAppService {
    remote_control: RemoteAppControlRef,
    connection_lookup: ConnectionLookupRef,
    auth_token: Option<String>,
}

impl RemoteAppService {
    pub fn new(
        remote_control: RemoteAppControlRef,
        connection_lookup: ConnectionLookupRef,
        auth_token: Option<String>,
    ) -> Self {
        Self {
            remote_control,
            connection_lookup,
            auth_token,
        }
    }

    /// Wake a remote app and optionally verify the connection registered.
    ///
    /// Calls the remote app's `/detrix/wake` endpoint via the port trait.
    /// If the app returns a `connection_id`, polls the connection service
    /// up to 10 times (500ms apart) to verify it appeared.
    pub async fn wake_app(
        &self,
        app_url: &str,
        daemon_url: Option<&str>,
    ) -> Result<RemoteWakeResponse> {
        let response = self
            .remote_control
            .wake_app(app_url, daemon_url, self.auth_token.as_deref())
            .await?;

        // If we got a connection_id, wait briefly and verify it appeared
        if let Some(ref conn_id) = response.connection_id {
            info!(
                "wake: App reported connection_id={}, verifying registration...",
                conn_id
            );

            let conn_id_obj = ConnectionId::new(conn_id);

            for attempt in 0..CONNECTION_POLL_MAX_RETRIES {
                tokio::time::sleep(std::time::Duration::from_millis(
                    CONNECTION_POLL_INTERVAL_MS,
                ))
                .await;
                if let Ok(Some(_)) = self.connection_lookup.get_connection(&conn_id_obj).await {
                    info!(
                        "wake: Connection {} verified on attempt {}",
                        conn_id,
                        attempt + 1
                    );
                    return Ok(response);
                }
            }

            let total_wait_ms = CONNECTION_POLL_MAX_RETRIES as u64 * CONNECTION_POLL_INTERVAL_MS;
            warn!(
                "wake: Connection {} not found after {}ms, but app reported success",
                conn_id, total_wait_ms
            );
        }

        Ok(response)
    }

    /// Sleep a remote app by calling its control plane.
    ///
    /// Thin delegation to the port trait.
    pub async fn sleep_app(&self, app_url: &str) -> Result<RemoteSleepResponse> {
        self.remote_control
            .sleep_app(app_url, self.auth_token.as_deref())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use detrix_config::DaemonConfig;
    use detrix_core::{MetricEvent, SystemEvent};
    use detrix_ports::{
        DapAdapterFactoryRef, MetricRepositoryRef, RemoteAppControl, RemoteSleepResponse,
        RemoteWakeResponse, VfsRef,
    };
    use detrix_testing::{
        MockConnectionReferenceRepository, MockConnectionRepository, MockDapAdapterFactory,
        MockEventRepository, MockMetricRepository, MockRemoteAppControl, MockVfs,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::broadcast;

    use crate::services::{AdapterLifecycleManager, ConnectionService, EventCaptureService};
    use std::sync::Arc;

    fn create_test_service(mock: impl RemoteAppControl + 'static) -> RemoteAppService {
        let connection_repo = Arc::new(MockConnectionRepository::new());
        let metric_repo = Arc::new(MockMetricRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let adapter_factory = Arc::new(MockDapAdapterFactory) as DapAdapterFactoryRef;

        let event_capture = Arc::new(EventCaptureService::new(
            event_repo as detrix_ports::EventRepositoryRef,
        ));
        let (event_tx, _) = broadcast::channel::<MetricEvent>(16);
        let (system_event_tx, _) = broadcast::channel::<SystemEvent>(16);
        let vfs: VfsRef = Arc::new(MockVfs::new());

        let adapter_lifecycle = Arc::new(AdapterLifecycleManager::with_config(
            event_capture,
            event_tx,
            system_event_tx.clone(),
            adapter_factory,
            metric_repo.clone() as MetricRepositoryRef,
            connection_repo.clone() as detrix_ports::ConnectionRepositoryRef,
            Default::default(),
            Default::default(),
            DaemonConfig::default().drain_timeout_ms,
            None,
            vfs.clone(),
        ));

        let reference_repo: crate::ConnectionReferenceRepositoryRef =
            Arc::new(MockConnectionReferenceRepository::new());
        let connection_service = Arc::new(ConnectionService::new(
            connection_repo as detrix_ports::ConnectionRepositoryRef,
            metric_repo as MetricRepositoryRef,
            reference_repo,
            adapter_lifecycle,
            system_event_tx,
            vfs,
        ));

        RemoteAppService::new(
            Arc::new(mock),
            connection_service as ConnectionLookupRef,
            Some("test-token".to_string()),
        )
    }

    /// Mock ConnectionLookup that returns a connection after N calls
    struct MockConnectionLookup {
        find_after_attempt: Option<usize>,
        call_count: AtomicUsize,
    }

    impl MockConnectionLookup {
        /// Returns Some(connection) after `n` calls (0-indexed).
        fn finds_after(n: usize) -> Self {
            Self {
                find_after_attempt: Some(n),
                call_count: AtomicUsize::new(0),
            }
        }

        /// Always returns None (connection never found).
        fn never_finds() -> Self {
            Self {
                find_after_attempt: None,
                call_count: AtomicUsize::new(0),
            }
        }

        fn _calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl detrix_ports::ConnectionLookup for MockConnectionLookup {
        async fn get_connection(
            &self,
            id: &detrix_core::ConnectionId,
        ) -> detrix_core::Result<Option<detrix_core::Connection>> {
            let attempt = self.call_count.fetch_add(1, Ordering::SeqCst);
            if let Some(threshold) = self.find_after_attempt {
                if attempt >= threshold {
                    let conn = detrix_core::Connection::new(
                        id.clone(),
                        "127.0.0.1".to_string(),
                        5678,
                        detrix_core::SourceLanguage::Python,
                    )
                    .unwrap();
                    return Ok(Some(conn));
                }
            }
            Ok(None)
        }
    }

    fn create_simple_service(
        mock_control: impl RemoteAppControl + 'static,
        mock_lookup: impl detrix_ports::ConnectionLookup + 'static,
    ) -> RemoteAppService {
        RemoteAppService::new(
            Arc::new(mock_control),
            Arc::new(mock_lookup) as ConnectionLookupRef,
            Some("test-token".to_string()),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn test_wake_with_connection_id_found() {
        let mock = MockRemoteAppControl::new(
            RemoteWakeResponse {
                app_url: "http://app:8091".to_string(),
                status: "ok".to_string(),
                connection_id: Some("test-conn-123".to_string()),
                debug_port: Some(5679),
                daemon_url: None,
            },
            RemoteSleepResponse {
                app_url: "http://app:8091".to_string(),
                status: "sleeping".to_string(),
            },
        );

        // Connection appears on the 2nd poll attempt
        let lookup = MockConnectionLookup::finds_after(1);
        let service = create_simple_service(mock, lookup);

        let result = service.wake_app("http://app:8091", None).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.connection_id, Some("test-conn-123".to_string()));
        assert_eq!(response.debug_port, Some(5679));
    }

    #[tokio::test(start_paused = true)]
    async fn test_wake_with_connection_id_timeout() {
        let mock = MockRemoteAppControl::new(
            RemoteWakeResponse {
                app_url: "http://app:8091".to_string(),
                status: "ok".to_string(),
                connection_id: Some("missing-conn".to_string()),
                debug_port: None,
                daemon_url: None,
            },
            RemoteSleepResponse {
                app_url: "http://app:8091".to_string(),
                status: "sleeping".to_string(),
            },
        );

        // Connection is never found — polling should exhaust all 10 retries
        let lookup = MockConnectionLookup::never_finds();
        let service = create_simple_service(mock, lookup);

        let result = service.wake_app("http://app:8091", None).await;
        // Should still return Ok (app reported success, we just log a warning)
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.connection_id, Some("missing-conn".to_string()));
    }

    #[tokio::test]
    async fn test_wake_app_without_connection_id() {
        let mock = MockRemoteAppControl::new(
            RemoteWakeResponse {
                app_url: "http://app:8091".to_string(),
                status: "ok".to_string(),
                connection_id: None,
                debug_port: None,
                daemon_url: None,
            },
            RemoteSleepResponse {
                app_url: "http://app:8091".to_string(),
                status: "sleeping".to_string(),
            },
        );

        let service = create_test_service(mock);
        let result = service.wake_app("http://app:8091", None).await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.app_url, "http://app:8091");
        assert_eq!(response.status, "ok");
        assert!(response.connection_id.is_none());
    }

    #[tokio::test]
    async fn test_sleep_app_success() {
        let mock = MockRemoteAppControl::new(
            RemoteWakeResponse {
                app_url: "http://app:8091".to_string(),
                status: "ok".to_string(),
                connection_id: None,
                debug_port: None,
                daemon_url: None,
            },
            RemoteSleepResponse {
                app_url: "http://app:8091".to_string(),
                status: "sleeping".to_string(),
            },
        );

        let service = create_test_service(mock);
        let result = service.sleep_app("http://app:8091").await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status, "sleeping");
    }

    #[tokio::test]
    async fn test_wake_app_error_propagation() {
        let service = create_test_service(MockRemoteAppControl::failing("unreachable"));
        let result = service.wake_app("http://unreachable:8091", None).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, detrix_core::Error::RemoteApp(_)));
    }

    #[tokio::test]
    async fn test_sleep_app_error_propagation() {
        let service = create_test_service(MockRemoteAppControl::failing("unreachable"));
        let result = service.sleep_app("http://unreachable:8091").await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, detrix_core::Error::RemoteApp(_)));
    }
}
