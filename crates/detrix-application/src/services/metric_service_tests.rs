//! Unit tests for MetricService
//!
//! These tests use shared mock repositories from detrix-testing crate.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::services::AdapterLifecycleManager;
    use crate::{
        ConnectionRepositoryRef, DapAdapter, DapAdapterFactory, DapAdapterFactoryRef,
        DapAdapterRef, MetricRepositoryRef, RemoveMetricResult, SetMetricResult,
    };
    use async_trait::async_trait;
    use detrix_core::{
        ConnectionId, Location, Metric, MetricEvent, MetricId, MetricMode, SafetyLevel,
        SourceLanguage, SystemEvent,
    };
    use detrix_testing::{MockConnectionRepository, MockEventRepository, MockMetricRepository};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    // ==================== Fixture Path Helpers ====================
    // Reference the fixtures from detrix-testing crate via relative path

    fn fixtures_dir() -> PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest_dir)
            .parent()
            .unwrap()
            .join("detrix-testing")
            .join("fixtures")
    }

    fn test_py_path() -> String {
        fixtures_dir().join("test.py").to_string_lossy().to_string()
    }

    // ==================== Mock DAP Adapter ====================

    /// Mock adapter for testing that doesn't require a real connection
    struct MockAdapter {
        /// Event sender - kept alive to prevent channel from closing
        _event_tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<MetricEvent>>>>,
    }

    impl MockAdapter {
        fn new() -> Self {
            Self {
                _event_tx: Arc::new(RwLock::new(None)),
            }
        }
    }

    #[async_trait]
    impl DapAdapter for MockAdapter {
        async fn start(&self) -> detrix_core::Result<()> {
            Ok(())
        }

        async fn stop(&self) -> detrix_core::Result<()> {
            Ok(())
        }

        async fn ensure_connected(&self) -> detrix_core::Result<()> {
            Ok(())
        }

        fn is_connected(&self) -> bool {
            true
        }

        async fn set_metric(&self, metric: &Metric) -> detrix_core::Result<SetMetricResult> {
            Ok(SetMetricResult {
                verified: true,
                line: metric.location.line,
                message: None,
            })
        }

        async fn remove_metric(&self, _metric: &Metric) -> detrix_core::Result<RemoveMetricResult> {
            Ok(RemoveMetricResult::success())
        }

        async fn subscribe_events(
            &self,
        ) -> detrix_core::Result<tokio::sync::mpsc::Receiver<MetricEvent>> {
            let (tx, rx) = tokio::sync::mpsc::channel(1000);
            // Store the sender to keep the channel alive
            *self._event_tx.write().await = Some(tx);
            Ok(rx)
        }
    }

    /// Configurable mock adapter that can simulate failures
    struct FailingMockAdapter {
        /// Metric names that should fail on set_metric
        fail_on_set: Arc<RwLock<HashSet<String>>>,
        /// Metric names that should fail on remove_metric
        fail_on_remove: Arc<RwLock<HashSet<String>>>,
        /// If true, all operations fail (simulates disconnected adapter)
        disconnected: Arc<RwLock<bool>>,
        /// Event sender - kept alive to prevent channel from closing
        /// (which would trigger cleanup in AdapterLifecycleManager)
        _event_tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<MetricEvent>>>>,
    }

    impl FailingMockAdapter {
        fn new() -> Self {
            Self {
                fail_on_set: Arc::new(RwLock::new(HashSet::new())),
                fail_on_remove: Arc::new(RwLock::new(HashSet::new())),
                disconnected: Arc::new(RwLock::new(false)),
                _event_tx: Arc::new(RwLock::new(None)),
            }
        }

        async fn fail_on_set_metric(&self, name: &str) {
            self.fail_on_set.write().await.insert(name.to_string());
        }

        async fn fail_on_remove_metric(&self, name: &str) {
            self.fail_on_remove.write().await.insert(name.to_string());
        }

        async fn set_disconnected(&self, disconnected: bool) {
            *self.disconnected.write().await = disconnected;
        }
    }

    #[async_trait]
    impl DapAdapter for FailingMockAdapter {
        async fn start(&self) -> detrix_core::Result<()> {
            if *self.disconnected.read().await {
                return Err(detrix_core::Error::Adapter(
                    "Adapter disconnected".to_string(),
                ));
            }
            Ok(())
        }

        async fn stop(&self) -> detrix_core::Result<()> {
            Ok(())
        }

        async fn ensure_connected(&self) -> detrix_core::Result<()> {
            if *self.disconnected.read().await {
                return Err(detrix_core::Error::Adapter(
                    "Adapter disconnected".to_string(),
                ));
            }
            Ok(())
        }

        fn is_connected(&self) -> bool {
            // Best effort - use try_read to avoid blocking
            self.disconnected.try_read().map(|d| !*d).unwrap_or(false)
        }

        async fn set_metric(&self, metric: &Metric) -> detrix_core::Result<SetMetricResult> {
            if *self.disconnected.read().await {
                return Err(detrix_core::Error::Adapter(
                    "Adapter disconnected - cannot set metric".to_string(),
                ));
            }
            if self.fail_on_set.read().await.contains(&metric.name) {
                return Err(detrix_core::Error::Adapter(format!(
                    "Failed to set metric '{}': simulated failure",
                    metric.name
                )));
            }
            Ok(SetMetricResult {
                verified: true,
                line: metric.location.line,
                message: None,
            })
        }

        async fn remove_metric(&self, metric: &Metric) -> detrix_core::Result<RemoveMetricResult> {
            if *self.disconnected.read().await {
                return Err(detrix_core::Error::Adapter(
                    "Adapter disconnected - cannot remove metric".to_string(),
                ));
            }
            if self.fail_on_remove.read().await.contains(&metric.name) {
                return Err(detrix_core::Error::Adapter(format!(
                    "Failed to remove metric '{}': simulated failure",
                    metric.name
                )));
            }
            Ok(RemoveMetricResult::success())
        }

        async fn subscribe_events(
            &self,
        ) -> detrix_core::Result<tokio::sync::mpsc::Receiver<MetricEvent>> {
            if *self.disconnected.read().await {
                return Err(detrix_core::Error::Adapter(
                    "Adapter disconnected".to_string(),
                ));
            }
            let (tx, rx) = tokio::sync::mpsc::channel(1000);
            // Store the sender to keep the channel alive
            // Without this, the receiver would immediately return None,
            // triggering cleanup in AdapterLifecycleManager
            *self._event_tx.write().await = Some(tx);
            Ok(rx)
        }
    }

    // ==================== Mock DAP Adapter Factory ====================

    /// Mock factory that creates MockAdapter instances
    struct MockAdapterFactory;

    #[async_trait]
    impl DapAdapterFactory for MockAdapterFactory {
        async fn create_python_adapter(
            &self,
            _host: &str,
            _port: u16,
        ) -> detrix_core::Result<DapAdapterRef> {
            Ok(Arc::new(MockAdapter::new()) as DapAdapterRef)
        }

        async fn create_go_adapter(
            &self,
            _host: &str,
            _port: u16,
        ) -> detrix_core::Result<DapAdapterRef> {
            Ok(Arc::new(MockAdapter::new()) as DapAdapterRef)
        }

        async fn create_rust_adapter(
            &self,
            _host: &str,
            _port: u16,
            _program: Option<&str>,
        ) -> detrix_core::Result<DapAdapterRef> {
            Ok(Arc::new(MockAdapter::new()) as DapAdapterRef)
        }
    }

    /// Mock factory that creates FailingMockAdapter instances with shared state
    struct FailingMockAdapterFactory {
        /// Shared failing adapter instance - all created adapters share this state
        shared_adapter: Arc<FailingMockAdapter>,
    }

    impl FailingMockAdapterFactory {
        fn new(shared_adapter: Arc<FailingMockAdapter>) -> Self {
            Self { shared_adapter }
        }
    }

    #[async_trait]
    impl DapAdapterFactory for FailingMockAdapterFactory {
        async fn create_python_adapter(
            &self,
            _host: &str,
            _port: u16,
        ) -> detrix_core::Result<DapAdapterRef> {
            // Return a clone of the shared adapter reference
            Ok(Arc::clone(&self.shared_adapter) as DapAdapterRef)
        }

        async fn create_go_adapter(
            &self,
            _host: &str,
            _port: u16,
        ) -> detrix_core::Result<DapAdapterRef> {
            // Return a clone of the shared adapter reference
            Ok(Arc::clone(&self.shared_adapter) as DapAdapterRef)
        }

        async fn create_rust_adapter(
            &self,
            _host: &str,
            _port: u16,
            _program: Option<&str>,
        ) -> detrix_core::Result<DapAdapterRef> {
            // Return a clone of the shared adapter reference
            Ok(Arc::clone(&self.shared_adapter) as DapAdapterRef)
        }
    }

    // ==================== Test Helper Functions ====================

    fn create_test_adapter_manager() -> Arc<AdapterLifecycleManager> {
        let event_capture = Arc::new(crate::EventCaptureService::new(Arc::new(
            MockEventRepository::new(),
        )
            as crate::EventRepositoryRef));
        let (event_tx, _) = tokio::sync::broadcast::channel(100);
        let (system_event_tx, _) = tokio::sync::broadcast::channel::<SystemEvent>(100);
        let factory = Arc::new(MockAdapterFactory) as DapAdapterFactoryRef;
        let metric_repo = Arc::new(MockMetricRepository::new()) as MetricRepositoryRef;
        let connection_repo = Arc::new(MockConnectionRepository::new()) as ConnectionRepositoryRef;

        Arc::new(AdapterLifecycleManager::new(
            event_capture,
            event_tx,
            system_event_tx,
            factory,
            metric_repo,
            connection_repo,
        ))
    }

    fn create_test_adapter_manager_with_failing_adapter(
        failing_adapter: Arc<FailingMockAdapter>,
    ) -> Arc<AdapterLifecycleManager> {
        let event_capture = Arc::new(crate::EventCaptureService::new(Arc::new(
            MockEventRepository::new(),
        )
            as crate::EventRepositoryRef));
        let (event_tx, _) = tokio::sync::broadcast::channel(100);
        let (system_event_tx, _) = tokio::sync::broadcast::channel::<SystemEvent>(100);
        let factory =
            Arc::new(FailingMockAdapterFactory::new(failing_adapter)) as DapAdapterFactoryRef;
        let metric_repo = Arc::new(MockMetricRepository::new()) as MetricRepositoryRef;
        let connection_repo = Arc::new(MockConnectionRepository::new()) as ConnectionRepositoryRef;

        Arc::new(AdapterLifecycleManager::new(
            event_capture,
            event_tx,
            system_event_tx,
            factory,
            metric_repo,
            connection_repo,
        ))
    }

    async fn create_test_service() -> MetricService {
        let repository = Arc::new(MockMetricRepository::new());
        let adapter_manager = create_test_adapter_manager();

        // Register a mock adapter for the default connection
        adapter_manager
            .start_adapter(
                ConnectionId::from("default"),
                "127.0.0.1",
                5678,
                SourceLanguage::Python,
                None,
            )
            .await
            .expect("Failed to register mock adapter");

        let (system_event_tx, _) = tokio::sync::broadcast::channel::<SystemEvent>(100);
        MetricService::builder(
            repository as MetricRepositoryRef,
            adapter_manager,
            system_event_tx,
        )
        .build()
    }

    async fn create_test_service_with_failing_adapter() -> (MetricService, Arc<FailingMockAdapter>)
    {
        let repository = Arc::new(MockMetricRepository::new());
        let failing_adapter = Arc::new(FailingMockAdapter::new());

        // Create adapter manager with the failing adapter factory
        let adapter_manager =
            create_test_adapter_manager_with_failing_adapter(Arc::clone(&failing_adapter));

        // Register the failing adapter for the default connection
        adapter_manager
            .start_adapter(
                ConnectionId::from("default"),
                "127.0.0.1",
                5678,
                SourceLanguage::Python,
                None,
            )
            .await
            .expect("Failed to register failing mock adapter");

        let (system_event_tx, _) = tokio::sync::broadcast::channel::<SystemEvent>(100);
        let service = MetricService::builder(
            repository as MetricRepositoryRef,
            adapter_manager,
            system_event_tx,
        )
        .build();

        (service, failing_adapter)
    }

    fn create_test_metric(name: &str) -> Metric {
        create_test_metric_at_line(name, 30) // Line 30 has x = Value() in fixture
    }

    /// Creates a test metric using real fixture file path
    fn create_test_metric_at_line(name: &str, line: u32) -> Metric {
        Metric {
            id: None,
            name: name.to_string(),
            connection_id: detrix_core::ConnectionId::from("default"),
            group: Some("test_group".to_string()),
            location: Location {
                file: test_py_path(),
                line,
            },
            expression: "x.value".to_string(),
            language: SourceLanguage::Python,
            enabled: true,
            mode: MetricMode::Stream,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            // Default values for introspection fields
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            // Anchor tracking defaults
            anchor: None,
            anchor_status: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_add_metric() {
        let service = create_test_service().await;
        let metric = create_test_metric("test_metric_1");

        let metric_id = service
            .add_metric(metric.clone(), false)
            .await
            .unwrap()
            .value;

        assert!(metric_id.0 > 0, "Metric ID should be positive");

        // Verify we can retrieve it
        let retrieved = service.get_metric(metric_id).await.unwrap();
        assert!(retrieved.is_some(), "Metric should be retrievable");
        assert_eq!(retrieved.unwrap().name, "test_metric_1");
    }

    #[tokio::test]
    async fn test_get_metric_by_name() {
        let service = create_test_service().await;
        let metric = create_test_metric("unique_metric");

        service
            .add_metric(metric.clone(), false)
            .await
            .unwrap()
            .value;

        let retrieved = service.get_metric_by_name("unique_metric").await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "unique_metric");
    }

    #[tokio::test]
    async fn test_list_metrics() {
        let service = create_test_service().await;

        // Add multiple metrics at different lines (DAP only supports one logpoint per line)
        service
            .add_metric(create_test_metric_at_line("metric1", 10), false)
            .await
            .unwrap();
        service
            .add_metric(create_test_metric_at_line("metric2", 20), false)
            .await
            .unwrap();
        service
            .add_metric(create_test_metric_at_line("metric3", 30), false)
            .await
            .unwrap();

        let metrics = service.list_metrics().await.unwrap();
        assert_eq!(metrics.len(), 3);

        let names: std::collections::HashSet<_> = metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains("metric1"));
        assert!(names.contains("metric2"));
        assert!(names.contains("metric3"));

        let metric1 = metrics.iter().find(|m| m.name == "metric1").unwrap();
        let metric2 = metrics.iter().find(|m| m.name == "metric2").unwrap();
        let metric3 = metrics.iter().find(|m| m.name == "metric3").unwrap();
        assert_eq!(metric1.location.line, 10);
        assert_eq!(metric2.location.line, 20);
        assert_eq!(metric3.location.line, 30);
    }

    #[tokio::test]
    async fn test_list_metrics_by_group() {
        let service = create_test_service().await;

        // Each metric at different line (DAP only supports one logpoint per line)
        let mut metric1 = create_test_metric_at_line("metric1", 10);
        metric1.group = Some("group_a".to_string());

        let mut metric2 = create_test_metric_at_line("metric2", 20);
        metric2.group = Some("group_b".to_string());

        let mut metric3 = create_test_metric_at_line("metric3", 30);
        metric3.group = Some("group_a".to_string());

        service.add_metric(metric1, false).await.unwrap().value;
        service.add_metric(metric2, false).await.unwrap().value;
        service.add_metric(metric3, false).await.unwrap().value;

        let group_a_metrics = service.list_metrics_by_group("group_a").await.unwrap();
        assert_eq!(group_a_metrics.len(), 2);

        let group_b_metrics = service.list_metrics_by_group("group_b").await.unwrap();
        assert_eq!(group_b_metrics.len(), 1);
    }

    #[tokio::test]
    async fn test_update_metric() {
        let service = create_test_service().await;
        let mut metric = create_test_metric("test_update");

        let metric_id = service
            .add_metric(metric.clone(), false)
            .await
            .unwrap()
            .value;

        // Update the metric (complex expression skips file inspection)
        metric.id = Some(metric_id);
        metric.expression = "updated_expression.value".to_string();
        metric.enabled = false;

        service.update_metric(&metric).await.unwrap();

        // Verify update
        let retrieved = service.get_metric(metric_id).await.unwrap().unwrap();
        assert_eq!(retrieved.expression, "updated_expression.value");
        assert!(!retrieved.enabled);
    }

    #[tokio::test]
    async fn test_remove_metric() {
        let service = create_test_service().await;
        let metric = create_test_metric("test_remove");

        let metric_id = service.add_metric(metric, false).await.unwrap().value;

        // Verify it exists
        assert!(service.get_metric(metric_id).await.unwrap().is_some());

        // Remove it
        service.remove_metric(metric_id).await.unwrap();

        // Verify it's gone
        assert!(service.get_metric(metric_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_toggle_metric() {
        let service = create_test_service().await;
        let metric = create_test_metric("test_toggle");

        let metric_id = service.add_metric(metric, false).await.unwrap().value;

        // Initially enabled
        let retrieved = service.get_metric(metric_id).await.unwrap().unwrap();
        assert!(retrieved.enabled);

        // Disable
        service.toggle_metric(metric_id, false).await.unwrap();
        let retrieved = service.get_metric(metric_id).await.unwrap().unwrap();
        assert!(!retrieved.enabled);

        // Enable
        service.toggle_metric(metric_id, true).await.unwrap();
        let retrieved = service.get_metric(metric_id).await.unwrap().unwrap();
        assert!(retrieved.enabled);
    }

    #[tokio::test]
    async fn test_enable_group() {
        let service = create_test_service().await;

        // Each metric at different line (DAP only supports one logpoint per line)
        let mut metric1 = create_test_metric_at_line("m1", 10);
        metric1.group = Some("test_group".to_string());
        metric1.enabled = false;

        let mut metric2 = create_test_metric_at_line("m2", 20);
        metric2.group = Some("test_group".to_string());
        metric2.enabled = false;

        service.add_metric(metric1, false).await.unwrap().value;
        service.add_metric(metric2, false).await.unwrap().value;

        let result = service.enable_group("test_group").await.unwrap();
        assert_eq!(result.succeeded, 2);
        assert!(result.is_complete());

        // Verify all enabled
        let metrics = service.list_metrics_by_group("test_group").await.unwrap();
        assert!(metrics.iter().all(|m| m.enabled));
    }

    #[tokio::test]
    async fn test_disable_group() {
        let service = create_test_service().await;

        // Each metric at different line (DAP only supports one logpoint per line)
        let mut metric1 = create_test_metric_at_line("m1", 10);
        metric1.group = Some("test_group".to_string());
        metric1.enabled = true;

        let mut metric2 = create_test_metric_at_line("m2", 20);
        metric2.group = Some("test_group".to_string());
        metric2.enabled = true;

        service.add_metric(metric1, false).await.unwrap().value;
        service.add_metric(metric2, false).await.unwrap().value;

        let result = service.disable_group("test_group").await.unwrap();
        assert_eq!(result.succeeded, 2);
        assert!(result.is_complete());

        // Verify all disabled
        let metrics = service.list_metrics_by_group("test_group").await.unwrap();
        assert!(metrics.iter().all(|m| !m.enabled));
    }

    #[tokio::test]
    async fn test_get_nonexistent_metric() {
        let service = create_test_service().await;

        let result = service.get_metric(MetricId(99999)).await.unwrap();
        assert!(result.is_none());
    }

    // ==================== Partial Success Tests ====================

    #[tokio::test]
    async fn test_enable_group_partial_success() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Create 3 metrics at different lines (DAP only supports one logpoint per line)
        let mut metric1 = create_test_metric_at_line("metric_ok_1", 10);
        metric1.group = Some("partial_group".to_string());
        metric1.enabled = false;

        let mut metric2 = create_test_metric_at_line("metric_fail", 20);
        metric2.group = Some("partial_group".to_string());
        metric2.enabled = false;

        let mut metric3 = create_test_metric_at_line("metric_ok_2", 30);
        metric3.group = Some("partial_group".to_string());
        metric3.enabled = false;

        // Add metrics (disabled, so adapter won't be called yet)
        service.add_metric(metric1, false).await.unwrap().value;
        service.add_metric(metric2, false).await.unwrap().value;
        service.add_metric(metric3, false).await.unwrap().value;

        // Configure adapter to fail on "metric_fail"
        adapter.fail_on_set_metric("metric_fail").await;

        // Enable group - should partially succeed
        let result = service.enable_group("partial_group").await.unwrap();

        // Verify partial success
        assert_eq!(result.succeeded, 2, "Should have 2 successful enables");
        assert_eq!(result.failed.len(), 1, "Should have 1 failure");
        assert!(!result.is_complete(), "Should not be complete");
        assert!(result.has_failures(), "Should have failures");

        // Verify the failed metric name is in the failures
        assert!(
            result.failed.iter().any(|(name, _)| name == "metric_fail"),
            "Failed metric should be 'metric_fail'"
        );

        // Verify storage state: failed metric should still be disabled (rolled back)
        let metrics = service
            .list_metrics_by_group("partial_group")
            .await
            .unwrap();
        let failed_metric = metrics.iter().find(|m| m.name == "metric_fail").unwrap();
        assert!(
            !failed_metric.enabled,
            "Failed metric should be rolled back to disabled"
        );

        // Verify successful metrics are enabled
        let ok_metrics: Vec<_> = metrics.iter().filter(|m| m.name != "metric_fail").collect();
        assert!(
            ok_metrics.iter().all(|m| m.enabled),
            "Successful metrics should be enabled"
        );
    }

    #[tokio::test]
    async fn test_disable_group_partial_success() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Create 3 metrics at different lines (DAP only supports one logpoint per line)
        let mut metric1 = create_test_metric_at_line("metric_ok_1", 10);
        metric1.group = Some("partial_group".to_string());
        metric1.enabled = true;

        let mut metric2 = create_test_metric_at_line("metric_fail", 20);
        metric2.group = Some("partial_group".to_string());
        metric2.enabled = true;

        let mut metric3 = create_test_metric_at_line("metric_ok_2", 30);
        metric3.group = Some("partial_group".to_string());
        metric3.enabled = true;

        // Add metrics (enabled, adapter will be called)
        service.add_metric(metric1, false).await.unwrap().value;
        service.add_metric(metric2, false).await.unwrap().value;
        service.add_metric(metric3, false).await.unwrap().value;

        // Configure adapter to fail on remove for "metric_fail"
        adapter.fail_on_remove_metric("metric_fail").await;

        // Disable group - should partially succeed
        let result = service.disable_group("partial_group").await.unwrap();

        // Verify partial success
        assert_eq!(result.succeeded, 2, "Should have 2 successful disables");
        assert_eq!(result.failed.len(), 1, "Should have 1 failure");
        assert!(!result.is_complete());

        // Verify the failed metric is still enabled (rolled back)
        let metrics = service
            .list_metrics_by_group("partial_group")
            .await
            .unwrap();
        let failed_metric = metrics.iter().find(|m| m.name == "metric_fail").unwrap();
        assert!(
            failed_metric.enabled,
            "Failed metric should be rolled back to enabled"
        );

        // Verify successful metrics are disabled
        let ok_metrics: Vec<_> = metrics.iter().filter(|m| m.name != "metric_fail").collect();
        assert!(
            ok_metrics.iter().all(|m| !m.enabled),
            "Successful metrics should be disabled"
        );
    }

    #[tokio::test]
    async fn test_enable_group_ensure_complete_fails_on_partial() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Each metric at different line (DAP only supports one logpoint per line)
        let mut metric1 = create_test_metric_at_line("ok_metric", 10);
        metric1.group = Some("test".to_string());
        metric1.enabled = false;

        let mut metric2 = create_test_metric_at_line("fail_metric", 20);
        metric2.group = Some("test".to_string());
        metric2.enabled = false;

        service.add_metric(metric1, false).await.unwrap().value;
        service.add_metric(metric2, false).await.unwrap().value;

        adapter.fail_on_set_metric("fail_metric").await;

        // Get result and try ensure_complete
        let result = service.enable_group("test").await.unwrap();
        let ensure_result = result.ensure_complete();

        // Should be an error
        assert!(
            ensure_result.is_err(),
            "ensure_complete should fail on partial success"
        );

        // Verify error type
        match ensure_result {
            Err(crate::Error::PartialGroupFailure {
                succeeded,
                failures,
            }) => {
                assert_eq!(succeeded, 1);
                assert_eq!(failures.len(), 1);
            }
            _ => panic!("Expected PartialGroupFailure error"),
        }
    }

    // ==================== Adapter Disconnected Tests ====================

    #[tokio::test]
    async fn test_add_metric_fails_when_adapter_disconnected() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Disconnect adapter
        adapter.set_disconnected(true).await;

        // Try to add an enabled metric - should fail
        let metric = create_test_metric("test_metric");
        let result = service.add_metric(metric, false).await;

        assert!(result.is_err(), "Should fail when adapter is disconnected");

        // Verify metric was NOT saved (rollback worked)
        let metrics = service.list_metrics().await.unwrap();
        assert!(metrics.is_empty(), "Metric should be rolled back");
    }

    #[tokio::test]
    async fn test_add_disabled_metric_succeeds_when_adapter_disconnected() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Disconnect adapter
        adapter.set_disconnected(true).await;

        // Add a DISABLED metric - should succeed (adapter not called)
        let mut metric = create_test_metric("disabled_metric");
        metric.enabled = false;

        let result = service.add_metric(metric, false).await;
        assert!(
            result.is_ok(),
            "Disabled metric should be added even with disconnected adapter"
        );

        // Verify metric was saved
        let metrics = service.list_metrics().await.unwrap();
        assert_eq!(metrics.len(), 1);
    }

    #[tokio::test]
    async fn test_toggle_metric_fails_when_adapter_disconnected() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Add a disabled metric first (adapter not called)
        let mut metric = create_test_metric("test_metric");
        metric.enabled = false;
        let metric_id = service.add_metric(metric, false).await.unwrap().value;

        // Disconnect adapter
        adapter.set_disconnected(true).await;

        // Try to enable - should fail
        let result = service.toggle_metric(metric_id, true).await;
        assert!(result.is_err(), "Should fail when adapter is disconnected");

        // Verify metric is still disabled (rolled back)
        let metric = service.get_metric(metric_id).await.unwrap().unwrap();
        assert!(!metric.enabled, "Metric should be rolled back to disabled");
    }

    #[tokio::test]
    async fn test_enable_group_all_fail_when_adapter_disconnected() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Add disabled metrics at different lines (DAP only supports one logpoint per line)
        let mut metric1 = create_test_metric_at_line("m1", 10);
        metric1.group = Some("test".to_string());
        metric1.enabled = false;

        let mut metric2 = create_test_metric_at_line("m2", 20);
        metric2.group = Some("test".to_string());
        metric2.enabled = false;

        service.add_metric(metric1, false).await.unwrap().value;
        service.add_metric(metric2, false).await.unwrap().value;

        // Disconnect adapter
        adapter.set_disconnected(true).await;

        // Enable group - all should fail
        let result = service.enable_group("test").await.unwrap();

        assert_eq!(result.succeeded, 0, "No metrics should succeed");
        assert_eq!(result.failed.len(), 2, "All metrics should fail");
        assert!(!result.is_complete());

        // Verify all metrics are still disabled
        let metrics = service.list_metrics_by_group("test").await.unwrap();
        assert!(
            metrics.iter().all(|m| !m.enabled),
            "All metrics should be rolled back"
        );
    }

    #[tokio::test]
    async fn test_remove_metric_fails_when_adapter_disconnected() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Add an enabled metric
        let metric = create_test_metric("test_metric");
        let metric_id = service.add_metric(metric, false).await.unwrap().value;

        // Disconnect adapter
        adapter.set_disconnected(true).await;

        // Try to remove - should fail (adapter needs to remove logpoint first)
        let result = service.remove_metric(metric_id).await;
        assert!(result.is_err(), "Should fail when adapter is disconnected");

        // Verify metric still exists
        let metric = service.get_metric(metric_id).await.unwrap();
        assert!(metric.is_some(), "Metric should still exist");
    }
}
