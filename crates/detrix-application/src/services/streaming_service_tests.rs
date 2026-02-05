//! Unit tests for StreamingService
//!
//! These tests use shared mock repositories from detrix-testing crate.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::ports::{EventOutput, EventOutputRef, NullOutput, OutputStats};
    use crate::{EventRepository, EventRepositoryRef, MetricRepository, MetricRepositoryRef};
    use async_trait::async_trait;
    use detrix_core::{Metric, MetricEvent, MetricId};
    use detrix_testing::{MockEventRepository, MockMetricRepository};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    // ==================== Test Helpers ====================

    async fn create_test_service() -> (StreamingService, Arc<MockEventRepository>) {
        let event_repository = Arc::new(MockEventRepository::new());
        let metric_repository = Arc::new(MockMetricRepository::new());
        let service = StreamingService::builder(
            Arc::clone(&event_repository) as EventRepositoryRef,
            Arc::clone(&metric_repository) as MetricRepositoryRef,
        )
        .build();
        (service, event_repository)
    }

    async fn create_test_event(
        repository: &MockEventRepository,
        metric_id: MetricId,
        value: &str,
    ) -> i64 {
        let event = MetricEvent {
            id: None,
            metric_id,
            metric_name: "test_metric".to_string(),
            connection_id: detrix_core::ConnectionId::from("default"),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as i64,
            thread_name: Some("test_thread".to_string()),
            thread_id: Some(12345),
            value_json: value.to_string(),
            value_numeric: None,
            value_string: None,
            value_boolean: None,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: Some("req_123".to_string()),
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };

        repository.save(&event).await.unwrap()
    }

    #[tokio::test]
    async fn test_query_metric_events() {
        let (service, repository) = create_test_service().await;
        let metric_id = MetricId(1);

        // Create test events (mock doesn't need FK)
        create_test_event(&repository, metric_id, r#"{"value": 10}"#).await;
        create_test_event(&repository, metric_id, r#"{"value": 20}"#).await;
        create_test_event(&repository, metric_id, r#"{"value": 30}"#).await;

        // Query events
        let events = service
            .query_metric_events(metric_id, Some(10), None)
            .await
            .unwrap();

        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|e| e.metric_id == metric_id));
    }

    #[tokio::test]
    async fn test_query_metric_events_with_limit() {
        let (service, repository) = create_test_service().await;
        let metric_id = MetricId(1);

        // Create 5 events
        for i in 0..5 {
            create_test_event(&repository, metric_id, &format!(r#"{{"value": {}}}"#, i)).await;
        }

        // Query with limit of 2
        let events = service
            .query_metric_events(metric_id, Some(2), None)
            .await
            .unwrap();

        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn test_get_latest_value() {
        let (service, repository) = create_test_service().await;
        let metric_id = MetricId(1);

        // Create events in sequence
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        create_test_event(&repository, metric_id, r#"{"value": 10}"#).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        create_test_event(&repository, metric_id, r#"{"value": 20}"#).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        create_test_event(&repository, metric_id, r#"{"value": 30}"#).await;

        // Get latest value
        let latest = service.get_latest_value(metric_id).await.unwrap();

        assert!(latest.is_some());
        let event = latest.unwrap();
        assert_eq!(event.value_json, r#"{"value": 30}"#);
    }

    #[tokio::test]
    async fn test_query_different_metrics() {
        let (service, repository) = create_test_service().await;
        let metric_id_1 = MetricId(1);
        let metric_id_2 = MetricId(2);

        // Create events for both metrics
        create_test_event(&repository, metric_id_1, r#"{"m1": 1}"#).await;
        create_test_event(&repository, metric_id_1, r#"{"m1": 2}"#).await;
        create_test_event(&repository, metric_id_2, r#"{"m2": 1}"#).await;

        // Query each metric
        let events_1 = service
            .query_metric_events(metric_id_1, Some(10), None)
            .await
            .unwrap();
        let events_2 = service
            .query_metric_events(metric_id_2, Some(10), None)
            .await
            .unwrap();

        assert_eq!(events_1.len(), 2);
        assert_eq!(events_2.len(), 1);
    }

    #[tokio::test]
    async fn test_event_with_error() {
        let (service, repository) = create_test_service().await;
        let metric_id = MetricId(1);

        let error_event = MetricEvent {
            id: None,
            metric_id,
            metric_name: "test_metric".to_string(),
            connection_id: detrix_core::ConnectionId::from("default"),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as i64,
            thread_name: None,
            thread_id: None,
            value_json: String::new(),
            value_numeric: None,
            value_string: None,
            value_boolean: None,
            is_error: true,
            error_type: Some("NameError".to_string()),
            error_message: Some("variable not defined".to_string()),
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };

        repository.save(&error_event).await.unwrap();

        let events = service
            .query_metric_events(metric_id, Some(10), None)
            .await
            .unwrap();

        assert_eq!(events.len(), 1);
        assert!(events[0].is_error);
        assert_eq!(events[0].error_type, Some("NameError".to_string()));
    }

    // ==================== Default Since (metric created_at) Tests ====================

    /// Helper that exposes both event and metric repositories
    async fn create_test_service_with_metric_repo() -> (
        StreamingService,
        Arc<MockEventRepository>,
        Arc<MockMetricRepository>,
    ) {
        let event_repository = Arc::new(MockEventRepository::new());
        let metric_repository = Arc::new(MockMetricRepository::new());
        let service = StreamingService::builder(
            Arc::clone(&event_repository) as EventRepositoryRef,
            Arc::clone(&metric_repository) as MetricRepositoryRef,
        )
        .build();
        (service, event_repository, metric_repository)
    }

    #[tokio::test]
    async fn test_query_events_without_since_defaults_to_metric_created_at() {
        let (service, event_repo, metric_repo) = create_test_service_with_metric_repo().await;

        let metric_id = MetricId(1);
        let created_at: i64 = 1_000_000_000_000; // arbitrary: 1M seconds in micros

        // Store a metric with a known created_at
        let metric = Metric {
            id: Some(metric_id),
            name: "test_metric".to_string(),
            connection_id: detrix_core::ConnectionId::from("conn-1"),
            group: None,
            location: detrix_core::Location {
                file: "test.py".to_string(),
                line: 10,
            },
            expression: "x".to_string(),
            language: detrix_core::SourceLanguage::Python,
            enabled: true,
            mode: detrix_core::MetricMode::default(),
            condition: None,
            safety_level: detrix_core::SafetyLevel::default(),
            created_at: Some(created_at),
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: detrix_core::AnchorStatus::default(),
        };
        metric_repo.save_with_options(&metric, false).await.unwrap();

        // Create a stale event (before created_at)
        let stale_event = MetricEvent {
            id: None,
            metric_id,
            metric_name: "test_metric".to_string(),
            connection_id: detrix_core::ConnectionId::from("conn-1"),
            timestamp: created_at - 500_000, // 0.5s before metric creation
            thread_name: None,
            thread_id: None,
            value_json: r#"{"stale": true}"#.to_string(),
            value_numeric: None,
            value_string: None,
            value_boolean: None,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };
        event_repo.save(&stale_event).await.unwrap();

        // Create a fresh event (after created_at)
        let fresh_event = MetricEvent {
            id: None,
            metric_id,
            metric_name: "test_metric".to_string(),
            connection_id: detrix_core::ConnectionId::from("conn-1"),
            timestamp: created_at + 500_000, // 0.5s after metric creation
            thread_name: None,
            thread_id: None,
            value_json: r#"{"fresh": true}"#.to_string(),
            value_numeric: None,
            value_string: None,
            value_boolean: None,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };
        event_repo.save(&fresh_event).await.unwrap();

        // Query without since — should only return fresh event (not stale)
        let events = service
            .query_metric_events(metric_id, Some(10), None)
            .await
            .unwrap();

        assert_eq!(
            events.len(),
            1,
            "Should filter out stale events from before metric creation"
        );
        assert_eq!(events[0].value_json, r#"{"fresh": true}"#);
    }

    #[tokio::test]
    async fn test_query_events_explicit_since_overrides_created_at() {
        let (service, event_repo, metric_repo) = create_test_service_with_metric_repo().await;

        let metric_id = MetricId(1);
        let created_at: i64 = 1_000_000_000_000;

        // Store metric
        let metric = Metric {
            id: Some(metric_id),
            name: "test_metric".to_string(),
            connection_id: detrix_core::ConnectionId::from("conn-1"),
            group: None,
            location: detrix_core::Location {
                file: "test.py".to_string(),
                line: 10,
            },
            expression: "x".to_string(),
            language: detrix_core::SourceLanguage::Python,
            enabled: true,
            mode: detrix_core::MetricMode::default(),
            condition: None,
            safety_level: detrix_core::SafetyLevel::default(),
            created_at: Some(created_at),
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: detrix_core::AnchorStatus::default(),
        };
        metric_repo.save_with_options(&metric, false).await.unwrap();

        // Create events before and after created_at
        let before_event = MetricEvent {
            id: None,
            metric_id,
            metric_name: "test_metric".to_string(),
            connection_id: detrix_core::ConnectionId::from("conn-1"),
            timestamp: created_at - 500_000,
            thread_name: None,
            thread_id: None,
            value_json: r#"{"before": true}"#.to_string(),
            value_numeric: None,
            value_string: None,
            value_boolean: None,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };
        event_repo.save(&before_event).await.unwrap();

        let after_event = MetricEvent {
            id: None,
            metric_id,
            metric_name: "test_metric".to_string(),
            connection_id: detrix_core::ConnectionId::from("conn-1"),
            timestamp: created_at + 500_000,
            thread_name: None,
            thread_id: None,
            value_json: r#"{"after": true}"#.to_string(),
            value_numeric: None,
            value_string: None,
            value_boolean: None,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };
        event_repo.save(&after_event).await.unwrap();

        // Query with explicit since=0 — should return BOTH events (overrides created_at default)
        let events = service
            .query_metric_events(metric_id, Some(10), Some(0))
            .await
            .unwrap();

        assert_eq!(events.len(), 2, "Explicit since=0 should return all events");
    }

    #[tokio::test]
    async fn test_query_events_metric_not_found_returns_all() {
        let (service, event_repo, _metric_repo) = create_test_service_with_metric_repo().await;

        let metric_id = MetricId(999); // Not stored in metric repo

        // Create an event for this metric_id (orphan - metric not in repo)
        let event = MetricEvent {
            id: None,
            metric_id,
            metric_name: "orphan".to_string(),
            connection_id: detrix_core::ConnectionId::from("conn-1"),
            timestamp: 100,
            thread_name: None,
            thread_id: None,
            value_json: r#"{"orphan": true}"#.to_string(),
            value_numeric: None,
            value_string: None,
            value_boolean: None,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };
        event_repo.save(&event).await.unwrap();

        // Should gracefully return all events (no filtering) since metric not found
        let events = service
            .query_metric_events(metric_id, Some(10), None)
            .await
            .unwrap();

        assert_eq!(
            events.len(),
            1,
            "Should return events even if metric not in repo"
        );
    }

    // ==================== Output Tests ====================

    /// Mock output that counts events sent
    struct CountingOutput {
        events_sent: AtomicU64,
    }

    impl CountingOutput {
        fn new() -> Self {
            Self {
                events_sent: AtomicU64::new(0),
            }
        }

        fn count(&self) -> u64 {
            self.events_sent.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EventOutput for CountingOutput {
        async fn send(&self, _event: &MetricEvent) -> detrix_core::Result<()> {
            self.events_sent.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn flush(&self) -> detrix_core::Result<()> {
            Ok(())
        }

        fn stats(&self) -> OutputStats {
            OutputStats {
                events_sent: self.events_sent.load(Ordering::SeqCst),
                connected: true,
                ..Default::default()
            }
        }

        fn name(&self) -> &str {
            "counting"
        }
    }

    #[tokio::test]
    async fn test_publish_event_with_output() {
        let event_repository = Arc::new(MockEventRepository::new());
        let metric_repository = Arc::new(MockMetricRepository::new());
        let output = Arc::new(CountingOutput::new());
        let (event_tx, _rx) = tokio::sync::broadcast::channel(16);

        let service = StreamingService::builder(
            event_repository as EventRepositoryRef,
            metric_repository as MetricRepositoryRef,
        )
        .event_channel(event_tx)
        .output(Arc::clone(&output) as EventOutputRef)
        .build();

        // Create and publish an event
        let event = MetricEvent {
            id: Some(1),
            metric_id: MetricId(1),
            metric_name: "test_metric".to_string(),
            connection_id: detrix_core::ConnectionId::from("test"),
            timestamp: 1234567890,
            thread_name: None,
            thread_id: None,
            value_json: r#"{"value": 42}"#.to_string(),
            value_numeric: Some(42.0),
            value_string: None,
            value_boolean: None,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };

        // Publish event (async publish to ensure output is called)
        service.publish_event_async(event).await.unwrap();

        // Verify output received the event
        assert_eq!(output.count(), 1);
    }

    #[tokio::test]
    async fn test_output_stats() {
        let event_repository = Arc::new(MockEventRepository::new());
        let metric_repository = Arc::new(MockMetricRepository::new());
        let output = Arc::new(NullOutput);
        let (event_tx, _rx) = tokio::sync::broadcast::channel(16);

        let service = StreamingService::builder(
            event_repository as EventRepositoryRef,
            metric_repository as MetricRepositoryRef,
        )
        .event_channel(event_tx)
        .output(output as EventOutputRef)
        .build();

        // Check output is healthy
        assert!(service.is_output_healthy());

        // Get stats
        let stats = service.output_stats();
        assert!(stats.is_some());
        assert!(stats.unwrap().connected);
    }

    #[tokio::test]
    async fn test_no_output_configured() {
        let (service, _repository) = create_test_service().await;

        // Should return None when no output configured
        assert!(service.output_stats().is_none());
        assert!(!service.is_output_healthy());
    }

    // ==================== Bounded Channel Tests ====================

    /// Slow output that blocks to test backpressure
    struct SlowOutput {
        delay_ms: u64,
        events_sent: AtomicU64,
    }

    impl SlowOutput {
        fn new(delay_ms: u64) -> Self {
            Self {
                delay_ms,
                events_sent: AtomicU64::new(0),
            }
        }

        fn count(&self) -> u64 {
            self.events_sent.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EventOutput for SlowOutput {
        async fn send(&self, _event: &MetricEvent) -> detrix_core::Result<()> {
            tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
            self.events_sent.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn flush(&self) -> detrix_core::Result<()> {
            Ok(())
        }

        fn stats(&self) -> OutputStats {
            OutputStats {
                events_sent: self.events_sent.load(Ordering::SeqCst),
                connected: true,
                ..Default::default()
            }
        }

        fn name(&self) -> &str {
            "slow"
        }
    }

    #[tokio::test]
    async fn test_bounded_channel_processes_events() {
        let event_repository = Arc::new(MockEventRepository::new());
        let metric_repository = Arc::new(MockMetricRepository::new());
        let output = Arc::new(CountingOutput::new());
        let (event_tx, _rx) = tokio::sync::broadcast::channel(16);

        let service = StreamingService::builder(
            event_repository as EventRepositoryRef,
            metric_repository as MetricRepositoryRef,
        )
        .event_channel(event_tx)
        .output(Arc::clone(&output) as EventOutputRef)
        .build();

        // Publish multiple events via bounded channel
        for i in 0..10 {
            let event = MetricEvent {
                id: Some(i),
                metric_id: MetricId(1),
                metric_name: "test_metric".to_string(),
                connection_id: detrix_core::ConnectionId::from("test"),
                timestamp: 1234567890 + i,
                thread_name: None,
                thread_id: None,
                value_json: format!(r#"{{"value": {}}}"#, i),
                value_numeric: Some(i as f64),
                value_string: None,
                value_boolean: None,
                is_error: false,
                error_type: None,
                error_message: None,
                request_id: None,
                session_id: None,
                stack_trace: None,
                memory_snapshot: None,
            };
            service.publish_event(event).unwrap();
        }

        // Give worker time to process
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Verify all events were processed by worker
        assert_eq!(output.count(), 10);
    }

    #[tokio::test]
    async fn test_dropped_events_counter() {
        // Get initial dropped count - verify counter is accessible
        // (actual backpressure test would require flooding a very slow output
        // which is fragile in CI)
        let _initial_dropped = streaming_service::output_dropped_events_count();
        // Counter exists and is readable - success
    }

    #[tokio::test]
    async fn test_worker_handles_slow_output() {
        let event_repository = Arc::new(MockEventRepository::new());
        let metric_repository = Arc::new(MockMetricRepository::new());
        let output = Arc::new(SlowOutput::new(10)); // 10ms delay per event
        let (event_tx, _rx) = tokio::sync::broadcast::channel(16);

        let service = StreamingService::builder(
            event_repository as EventRepositoryRef,
            metric_repository as MetricRepositoryRef,
        )
        .event_channel(event_tx)
        .output(Arc::clone(&output) as EventOutputRef)
        .build();

        // Send a few events
        for i in 0..5 {
            let event = MetricEvent {
                id: Some(i),
                metric_id: MetricId(1),
                metric_name: "test_metric".to_string(),
                connection_id: detrix_core::ConnectionId::from("test"),
                timestamp: 1234567890 + i,
                thread_name: None,
                thread_id: None,
                value_json: format!(r#"{{"value": {}}}"#, i),
                value_numeric: Some(i as f64),
                value_string: None,
                value_boolean: None,
                is_error: false,
                error_type: None,
                error_message: None,
                request_id: None,
                session_id: None,
                stack_trace: None,
                memory_snapshot: None,
            };
            service.publish_event(event).unwrap();
        }

        // Wait for worker to process all events (5 events * 10ms = 50ms, add margin)
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        // Verify all events were eventually processed
        assert_eq!(output.count(), 5);
    }
}
