//! Unit tests for MetricService
//!
//! These tests use shared mock repositories from detrix-testing crate.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::services::AdapterLifecycleManager;
    use crate::{
        ConnectionRepositoryRef, DapAdapter, DapAdapterFactory, DapAdapterFactoryRef,
        DapAdapterRef, MetricRepositoryRef, MetricScope, RemoveMetricResult, SetMetricResult,
    };
    use async_trait::async_trait;
    use detrix_core::{
        ConnectionId, Location, Metric, MetricEvent, MetricId, MetricMode, SafetyLevel,
        SourceLanguage, SystemEvent,
    };
    use detrix_ports::{ConnectionRepository, VfsRef};
    use detrix_testing::{
        MockConnectionRepository, MockEventRepository, MockMetricRepository, MockVfs,
    };
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
            _pid: Option<u32>,
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
            _pid: Option<u32>,
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
        let vfs: VfsRef = Arc::new(MockVfs::new());

        Arc::new(AdapterLifecycleManager::new(
            event_capture,
            event_tx,
            system_event_tx,
            factory,
            metric_repo,
            connection_repo,
            vfs,
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
        let vfs: VfsRef = Arc::new(MockVfs::new());

        Arc::new(AdapterLifecycleManager::new(
            event_capture,
            event_tx,
            system_event_tx,
            factory,
            metric_repo,
            connection_repo,
            vfs,
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
                None,  // program
                None,  // pid
                false, // safe_mode = false for normal tests
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
                None,  // program
                None,  // pid
                false, // safe_mode = false for normal tests
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
            expressions: vec!["x.value".to_string()],
            language: SourceLanguage::Python,
            enabled: true,
            mode: MetricMode::Stream,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            user_id: None,
            agent_id: None,
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
            .add_metric(metric.clone(), false, None)
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
    async fn test_add_metric_same_line_merges_expressions() {
        // When a second add_metric targets the same (connection, file, line), the new
        // expression should be merged into the existing metric rather than silently dropped.
        let service = create_test_service().await;

        // First add: creates the metric with expression "x.value"
        let mut metric1 = create_test_metric_at_line("metric_first", 42);
        metric1.expressions = vec!["x.value".to_string()];
        let outcome1 = service.add_metric(metric1, false, None).await.unwrap();
        let id1 = outcome1.value;
        assert!(outcome1.is_clean(), "First add should have no warnings");

        // Second add at the same line with a new expression — should merge, not replace
        let mut metric2 = create_test_metric_at_line("metric_second", 42);
        metric2.expressions = vec!["y.count".to_string()];
        let outcome2 = service.add_metric(metric2, false, None).await.unwrap();

        // Returns the FIRST metric's ID (same DAP logpoint)
        assert_eq!(outcome2.value, id1, "Should return existing metric ID");

        // Warns about the merge
        assert!(
            outcome2.has_warnings(),
            "Should warn about expressions being merged"
        );
        let warning = &outcome2.warnings[0];
        assert!(
            matches!(warning, crate::OperationWarning::ExpressionsMerged { .. }),
            "Warning should be ExpressionsMerged, got: {}",
            warning
        );

        // The stored metric should now have both expressions
        let stored = service.get_metric(id1).await.unwrap().unwrap();
        assert_eq!(
            stored.expressions,
            vec!["x.value".to_string(), "y.count".to_string()],
            "Both expressions should be present after merge"
        );
    }

    #[tokio::test]
    async fn test_add_metric_same_line_identical_expression_is_idempotent() {
        // Re-adding the exact same expression (daemon restart scenario) should be fully
        // idempotent: no merge warning, no change to stored expressions.
        let service = create_test_service().await;

        let mut metric = create_test_metric_at_line("metric_restart", 55);
        metric.expressions = vec!["x.value".to_string()];
        let id = service
            .add_metric(metric.clone(), false, None)
            .await
            .unwrap()
            .value;

        // Same location + same expression → idempotent
        let outcome2 = service.add_metric(metric, false, None).await.unwrap();
        assert_eq!(outcome2.value, id);
        assert!(
            outcome2.is_clean(),
            "Identical re-add should produce no warnings"
        );

        let stored = service.get_metric(id).await.unwrap().unwrap();
        assert_eq!(stored.expressions, vec!["x.value".to_string()]);
    }

    #[tokio::test]
    async fn test_get_metric_by_name() {
        let service = create_test_service().await;
        let metric = create_test_metric("unique_metric");

        service
            .add_metric(metric.clone(), false, None)
            .await
            .unwrap();

        let retrieved = service.get_metric_by_name("unique_metric").await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "unique_metric");
    }

    #[tokio::test]
    async fn test_list_metrics() {
        let service = create_test_service().await;

        // Add multiple metrics at different lines (DAP only supports one logpoint per line)
        service
            .add_metric(create_test_metric_at_line("metric1", 10), false, None)
            .await
            .unwrap();
        service
            .add_metric(create_test_metric_at_line("metric2", 20), false, None)
            .await
            .unwrap();
        service
            .add_metric(create_test_metric_at_line("metric3", 30), false, None)
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

        service.add_metric(metric1, false, None).await.unwrap();
        service.add_metric(metric2, false, None).await.unwrap();
        service.add_metric(metric3, false, None).await.unwrap();

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
            .add_metric(metric.clone(), false, None)
            .await
            .unwrap()
            .value;

        // Update the metric (complex expression skips file inspection)
        metric.id = Some(metric_id);
        metric.expressions = vec!["updated_expression.value".to_string()];
        metric.enabled = false;

        service
            .update_metric(&metric, &MetricScope::Admin)
            .await
            .unwrap();

        // Verify update
        let retrieved = service.get_metric(metric_id).await.unwrap().unwrap();
        assert_eq!(retrieved.expression(), "updated_expression.value");
        assert!(!retrieved.enabled);
    }

    #[tokio::test]
    async fn test_remove_metric() {
        let service = create_test_service().await;
        let metric = create_test_metric("test_remove");

        let metric_id = service.add_metric(metric, false, None).await.unwrap().value;

        // Verify it exists
        assert!(service.get_metric(metric_id).await.unwrap().is_some());

        // Remove it
        service
            .remove_metric(metric_id, &MetricScope::Admin)
            .await
            .unwrap();

        // Verify it's gone
        assert!(service.get_metric(metric_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_toggle_metric() {
        let service = create_test_service().await;
        let metric = create_test_metric("test_toggle");

        let metric_id = service.add_metric(metric, false, None).await.unwrap().value;

        // Initially enabled
        let retrieved = service.get_metric(metric_id).await.unwrap().unwrap();
        assert!(retrieved.enabled);

        // Disable
        service
            .toggle_metric(metric_id, false, &MetricScope::Admin)
            .await
            .unwrap();
        let retrieved = service.get_metric(metric_id).await.unwrap().unwrap();
        assert!(!retrieved.enabled);

        // Enable
        service
            .toggle_metric(metric_id, true, &MetricScope::Admin)
            .await
            .unwrap();
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

        service.add_metric(metric1, false, None).await.unwrap();
        service.add_metric(metric2, false, None).await.unwrap();

        let result = service
            .enable_group("test_group", &MetricScope::Admin)
            .await
            .unwrap();
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

        service.add_metric(metric1, false, None).await.unwrap();
        service.add_metric(metric2, false, None).await.unwrap();

        let result = service
            .disable_group("test_group", &MetricScope::Admin)
            .await
            .unwrap();
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
        service.add_metric(metric1, false, None).await.unwrap();
        service.add_metric(metric2, false, None).await.unwrap();
        service.add_metric(metric3, false, None).await.unwrap();

        // Configure adapter to fail on "metric_fail"
        adapter.fail_on_set_metric("metric_fail").await;

        // Enable group - should partially succeed
        let result = service
            .enable_group("partial_group", &MetricScope::Admin)
            .await
            .unwrap();

        // With sync_logpoint, storage is updated first and DAP sync is best-effort.
        // All 3 metrics get enabled in storage; sync_logpoint merges expressions per-location
        // so per-metric adapter failures don't apply.
        assert_eq!(
            result.succeeded, 3,
            "All 3 metrics should be enabled in storage"
        );
        assert!(
            result.failed.is_empty(),
            "No per-metric failures with sync_logpoint"
        );
        assert!(result.is_complete(), "Should be complete");

        // Verify all metrics are enabled in storage
        let metrics = service
            .list_metrics_by_group("partial_group")
            .await
            .unwrap();
        assert!(
            metrics.iter().all(|m| m.enabled),
            "All metrics should be enabled in storage"
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
        service.add_metric(metric1, false, None).await.unwrap();
        service.add_metric(metric2, false, None).await.unwrap();
        service.add_metric(metric3, false, None).await.unwrap();

        // Configure adapter to fail on remove for "metric_fail"
        adapter.fail_on_remove_metric("metric_fail").await;

        // Disable group - should partially succeed
        let result = service
            .disable_group("partial_group", &MetricScope::Admin)
            .await
            .unwrap();

        // With sync_logpoint, storage is updated first and DAP sync is best-effort.
        assert_eq!(
            result.succeeded, 3,
            "All 3 metrics should be disabled in storage"
        );
        assert!(
            result.failed.is_empty(),
            "No per-metric failures with sync_logpoint"
        );
        assert!(result.is_complete());

        // Verify all metrics are disabled in storage
        let metrics = service
            .list_metrics_by_group("partial_group")
            .await
            .unwrap();
        assert!(
            metrics.iter().all(|m| !m.enabled),
            "All metrics should be disabled in storage"
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

        service.add_metric(metric1, false, None).await.unwrap();
        service.add_metric(metric2, false, None).await.unwrap();

        adapter.fail_on_set_metric("fail_metric").await;

        // With sync_logpoint, storage updates succeed and DAP sync is best-effort.
        // Per-metric adapter failures don't prevent storage updates.
        let result = service
            .enable_group("test", &MetricScope::Admin)
            .await
            .unwrap();
        assert_eq!(result.succeeded, 2);

        // Should succeed — no per-metric failures with sync_logpoint
        let ensure_result = result.ensure_complete();
        assert!(
            ensure_result.is_ok(),
            "ensure_complete should succeed when all storage updates pass"
        );
    }

    // ==================== Adapter Disconnected Tests ====================

    #[tokio::test]
    async fn test_add_metric_fails_when_adapter_disconnected() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Disconnect adapter
        adapter.set_disconnected(true).await;

        // Try to add an enabled metric - should fail
        let metric = create_test_metric("test_metric");
        let result = service.add_metric(metric, false, None).await;

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

        let result = service.add_metric(metric, false, None).await;
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
        let metric_id = service.add_metric(metric, false, None).await.unwrap().value;

        // Disconnect adapter
        adapter.set_disconnected(true).await;

        // Try to enable - should fail
        let result = service
            .toggle_metric(metric_id, true, &MetricScope::Admin)
            .await;
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

        service.add_metric(metric1, false, None).await.unwrap();
        service.add_metric(metric2, false, None).await.unwrap();

        // Disconnect adapter
        adapter.set_disconnected(true).await;

        // With sync_logpoint, storage updates succeed even when adapter is disconnected.
        // sync_logpoint silently returns Ok when no adapter — logpoints will be synced
        // when the adapter reconnects.
        let result = service
            .enable_group("test", &MetricScope::Admin)
            .await
            .unwrap();

        assert_eq!(result.succeeded, 2, "Storage updates should succeed");
        assert!(
            result.failed.is_empty(),
            "No failures — sync is best-effort"
        );
        assert!(result.is_complete());

        // Verify all metrics are enabled in storage (will sync when adapter reconnects)
        let metrics = service.list_metrics_by_group("test").await.unwrap();
        assert!(
            metrics.iter().all(|m| m.enabled),
            "All metrics should be enabled in storage"
        );
    }

    #[tokio::test]
    async fn test_remove_metric_succeeds_when_adapter_disconnected() {
        let (service, adapter) = create_test_service_with_failing_adapter().await;

        // Add an enabled metric
        let metric = create_test_metric("test_metric");
        let metric_id = service.add_metric(metric, false, None).await.unwrap().value;

        // Disconnect adapter
        adapter.set_disconnected(true).await;

        // Remove should succeed — storage deletion happens, DAP cleanup is deferred
        // (sync_logpoint gracefully handles missing adapter)
        let result = service.remove_metric(metric_id, &MetricScope::Admin).await;
        assert!(
            result.is_ok(),
            "Remove should succeed even when adapter is disconnected"
        );

        // Verify metric is gone from storage
        let metric = service.get_metric(metric_id).await.unwrap();
        assert!(metric.is_none(), "Metric should be removed from storage");
    }

    // ==================== SafeMode Enforcement Tests ====================

    /// Creates a test service with a SafeMode-enabled connection
    async fn create_test_service_with_safe_mode_connection() -> MetricService {
        let repository = Arc::new(MockMetricRepository::new());
        let connection_repository = Arc::new(MockConnectionRepository::new());
        let adapter_manager = create_test_adapter_manager();

        let connection_id = ConnectionId::from("safe_mode_conn");

        // Create and save a connection with safe_mode enabled
        let mut connection = detrix_core::Connection::new(
            connection_id.clone(),
            "127.0.0.1".to_string(),
            5678,
            SourceLanguage::Python,
        )
        .unwrap();
        connection.safe_mode = true;
        connection_repository.save(&connection).await.unwrap();

        // Register a mock adapter for this connection with safe_mode enabled
        adapter_manager
            .start_adapter(
                connection_id,
                "127.0.0.1",
                5678,
                SourceLanguage::Python,
                None, // program
                None, // pid
                true, // safe_mode = true for SafeMode tests
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

    fn create_safe_mode_test_metric(name: &str) -> Metric {
        Metric {
            id: None,
            name: name.to_string(),
            connection_id: ConnectionId::from("safe_mode_conn"),
            group: Some("test_group".to_string()),
            location: Location {
                file: test_py_path(),
                line: 30,
            },
            expressions: vec!["x.value".to_string()],
            language: SourceLanguage::Python,
            enabled: true,
            mode: MetricMode::Stream,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            user_id: None,
            agent_id: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_safe_mode_blocks_capture_stack_trace() {
        let service = create_test_service_with_safe_mode_connection().await;

        let mut metric = create_safe_mode_test_metric("stack_trace_metric");
        metric.capture_stack_trace = true;

        let result = service.add_metric(metric, false, None).await;

        assert!(
            result.is_err(),
            "Should block capture_stack_trace in SafeMode"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SafeMode"),
            "Error should mention SafeMode: {}",
            err
        );
        assert!(
            err.contains("capture_stack_trace"),
            "Error should mention capture_stack_trace: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_safe_mode_blocks_capture_memory_snapshot() {
        let service = create_test_service_with_safe_mode_connection().await;

        let mut metric = create_safe_mode_test_metric("memory_snapshot_metric");
        metric.location.line = 31; // Different line to avoid conflict
        metric.capture_memory_snapshot = true;

        let result = service.add_metric(metric, false, None).await;

        assert!(
            result.is_err(),
            "Should block capture_memory_snapshot in SafeMode"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SafeMode"),
            "Error should mention SafeMode: {}",
            err
        );
        assert!(
            err.contains("capture_memory_snapshot"),
            "Error should mention capture_memory_snapshot: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_safe_mode_blocks_both_introspection_features() {
        let service = create_test_service_with_safe_mode_connection().await;

        let mut metric = create_safe_mode_test_metric("both_introspection_metric");
        metric.location.line = 32; // Different line to avoid conflict
        metric.capture_stack_trace = true;
        metric.capture_memory_snapshot = true;

        let result = service.add_metric(metric, false, None).await;

        assert!(
            result.is_err(),
            "Should block both introspection features in SafeMode"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SafeMode"),
            "Error should mention SafeMode: {}",
            err
        );
        // Error should mention both operations
        assert!(
            err.contains("capture_stack_trace") && err.contains("capture_memory_snapshot"),
            "Error should mention both operations: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_safe_mode_allows_basic_logpoints() {
        let service = create_test_service_with_safe_mode_connection().await;

        // Basic metric without introspection features
        let metric = create_safe_mode_test_metric("basic_logpoint");

        let result = service.add_metric(metric, false, None).await;

        assert!(
            result.is_ok(),
            "Should allow basic logpoints in SafeMode: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_safe_mode_blocks_update_with_introspection() {
        let service = create_test_service_with_safe_mode_connection().await;

        // First add a basic metric (should succeed)
        let metric = create_safe_mode_test_metric("update_test_metric");
        let outcome = service
            .add_metric(metric.clone(), false, None)
            .await
            .unwrap();
        let metric_id = outcome.value;

        // Now try to update it to enable capture_stack_trace
        let mut updated_metric = metric.clone();
        updated_metric.id = Some(metric_id);
        updated_metric.capture_stack_trace = true;

        let result = service
            .update_metric(&updated_metric, &MetricScope::Admin)
            .await;

        assert!(
            result.is_err(),
            "Should block update with capture_stack_trace in SafeMode"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SafeMode"),
            "Error should mention SafeMode: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_non_safe_mode_allows_introspection() {
        // Use the default test service (no SafeMode)
        let service = create_test_service().await;

        let mut metric = create_test_metric("introspection_metric");
        metric.capture_stack_trace = true;
        metric.capture_memory_snapshot = true;

        let result = service.add_metric(metric, false, None).await;

        assert!(
            result.is_ok(),
            "Should allow introspection features when NOT in SafeMode: {:?}",
            result
        );
    }

    // ==================== Phase 2.6: list_metrics_filtered user_id scope test ====================

    #[tokio::test]
    async fn test_list_metrics_filtered_by_user_id() {
        use crate::MetricFilter;

        let service = create_test_service().await;

        // Add alice's metric
        let mut alice_metric = create_test_metric("alice_metric");
        alice_metric.user_id = Some("alice".to_string());
        service.add_metric(alice_metric, false, None).await.unwrap();

        // Add bob's metric
        let mut bob_metric = create_test_metric_at_line("bob_metric", 200);
        bob_metric.user_id = Some("bob".to_string());
        service.add_metric(bob_metric, false, None).await.unwrap();

        // Filter by alice's user_id → only alice's metric returned
        let alice_filter = MetricFilter {
            user_id: Some("alice".to_string()),
            ..Default::default()
        };
        let (alice_metrics, alice_total) = service
            .list_metrics_filtered(&alice_filter, 100, 0)
            .await
            .unwrap();
        assert_eq!(alice_total, 1, "Alice should have exactly 1 metric");
        assert_eq!(alice_metrics[0].name, "alice_metric");

        // Filter by bob's user_id → only bob's metric returned
        let bob_filter = MetricFilter {
            user_id: Some("bob".to_string()),
            ..Default::default()
        };
        let (bob_metrics, bob_total) = service
            .list_metrics_filtered(&bob_filter, 100, 0)
            .await
            .unwrap();
        assert_eq!(bob_total, 1, "Bob should have exactly 1 metric");
        assert_eq!(bob_metrics[0].name, "bob_metric");

        // Admin (no user_id filter) → both metrics returned
        let admin_filter = MetricFilter::default();
        let (all_metrics, all_total) = service
            .list_metrics_filtered(&admin_filter, 100, 0)
            .await
            .unwrap();
        assert_eq!(all_total, 2, "Admin should see all 2 metrics");
        let names: Vec<_> = all_metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"alice_metric"));
        assert!(names.contains(&"bob_metric"));
    }

    // ========================================================================
    // Multi-tenant scope enforcement tests
    // ========================================================================

    #[tokio::test]
    async fn test_user_scope_cannot_remove_other_user_metric() {
        let service = create_test_service().await;

        // Alice creates a metric
        let mut metric = create_test_metric_at_line("alice_protected", 100);
        metric.user_id = Some("alice".to_string());
        let outcome = service.add_metric(metric, false, None).await.unwrap();
        let metric_id = outcome.value;

        // Bob tries to remove Alice's metric
        let bob_scope = MetricScope::User("bob".to_string());
        let result = service.remove_metric(metric_id, &bob_scope).await;
        assert!(
            result.is_err(),
            "Bob should not be able to remove Alice's metric"
        );
    }

    #[tokio::test]
    async fn test_user_scope_cannot_update_other_user_metric() {
        let service = create_test_service().await;

        // Alice creates a metric
        let mut metric = create_test_metric_at_line("alice_update", 110);
        metric.user_id = Some("alice".to_string());
        let outcome = service.add_metric(metric, false, None).await.unwrap();
        let metric_id = outcome.value;

        // Fetch and try to update as Bob
        let mut fetched = service.get_metric(metric_id).await.unwrap().unwrap();
        fetched.expressions = vec!["hacked".to_string()];

        let bob_scope = MetricScope::User("bob".to_string());
        let result = service.update_metric(&fetched, &bob_scope).await;
        assert!(
            result.is_err(),
            "Bob should not be able to update Alice's metric"
        );
    }

    #[tokio::test]
    async fn test_agent_scope_cannot_mutate_other_agent_metric() {
        let service = create_test_service().await;

        // Agent1 creates a metric
        let mut metric = create_test_metric_at_line("agent1_metric", 120);
        metric.user_id = Some("alice".to_string());
        metric.agent_id = Some("agent1".to_string());
        let outcome = service.add_metric(metric, false, None).await.unwrap();
        let metric_id = outcome.value;

        // Agent2 (same user) tries to toggle
        let agent2_scope = MetricScope::Agent {
            user_id: "alice".to_string(),
            agent_id: "agent2".to_string(),
        };
        let result = service.toggle_metric(metric_id, false, &agent2_scope).await;
        assert!(
            result.is_err(),
            "Agent2 should not be able to toggle Agent1's metric"
        );
    }

    #[tokio::test]
    async fn test_user_scope_can_mutate_own_metric() {
        let service = create_test_service().await;

        // Alice creates a metric
        let mut metric = create_test_metric_at_line("alice_own", 130);
        metric.user_id = Some("alice".to_string());
        let outcome = service.add_metric(metric, false, None).await.unwrap();
        let metric_id = outcome.value;

        // Alice can toggle her own metric
        let alice_scope = MetricScope::User("alice".to_string());
        let result = service.toggle_metric(metric_id, false, &alice_scope).await;
        assert!(
            result.is_ok(),
            "Alice should be able to toggle her own metric"
        );
    }
}
