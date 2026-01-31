//! Integration tests for detrix-storage
//!
//! These tests verify end-to-end workflows across the storage layer,
//! testing the interaction between repositories, error handling, and data persistence.

use detrix_application::{EventRepository, MetricRepository};
use detrix_core::entities::{Location, Metric, MetricEvent, MetricId, MetricMode, SafetyLevel};
use detrix_core::error::Error;
use detrix_core::{ConnectionId, SourceLanguage};
use detrix_storage::SqliteStorage;

/// Helper to create test storage
async fn create_storage() -> SqliteStorage {
    SqliteStorage::in_memory().await.unwrap()
}

/// Helper to create test metric
fn create_metric(name: &str, location: &str, expression: &str) -> Metric {
    Metric::new(
        name.to_string(),
        ConnectionId::from("default"),
        Location::parse(location).unwrap(),
        expression.to_string(),
        SourceLanguage::Python,
    )
    .unwrap()
}

// ============================================================
// METRIC LIFECYCLE TESTS
// ============================================================

#[tokio::test]
async fn test_metric_full_lifecycle() {
    let storage = create_storage().await;

    // Create metric
    let mut metric = create_metric("user_login", "@auth.py#42", "user.id");
    metric.group = Some("authentication".to_string());
    metric.safety_level = SafetyLevel::Strict;

    // Save metric
    let id = MetricRepository::save(&storage, &metric).await.unwrap();
    assert!(id.0 > 0);

    // Verify metric exists
    assert!(MetricRepository::exists_by_name(&storage, "user_login")
        .await
        .unwrap());

    // Find by ID
    let found = MetricRepository::find_by_id(&storage, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.name, "user_login");
    assert_eq!(found.group, Some("authentication".to_string()));
    assert_eq!(found.safety_level, SafetyLevel::Strict);

    // Find by name
    let found_by_name = MetricRepository::find_by_name(&storage, "user_login")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found_by_name.id, Some(id));

    // Update metric
    let mut updated = found;
    updated.expression = "user.email".to_string();
    updated.enabled = false;
    MetricRepository::update(&storage, &updated).await.unwrap();

    // Verify update
    let after_update = MetricRepository::find_by_id(&storage, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_update.expression, "user.email");
    assert!(!after_update.enabled);

    // Delete metric
    MetricRepository::delete(&storage, id).await.unwrap();

    // Verify deletion
    let after_delete = MetricRepository::find_by_id(&storage, id).await.unwrap();
    assert!(after_delete.is_none());
}

#[tokio::test]
async fn test_metric_group_management() {
    let storage = create_storage().await;

    // Create metrics in different groups
    let mut auth_metric1 = create_metric("login", "@auth.py#10", "user.id");
    auth_metric1.group = Some("auth".to_string());

    let mut auth_metric2 = create_metric("logout", "@auth.py#20", "user.id");
    auth_metric2.group = Some("auth".to_string());

    let mut order_metric = create_metric("order_placed", "@orders.py#5", "order.total");
    order_metric.group = Some("orders".to_string());

    MetricRepository::save(&storage, &auth_metric1)
        .await
        .unwrap();
    MetricRepository::save(&storage, &auth_metric2)
        .await
        .unwrap();
    MetricRepository::save(&storage, &order_metric)
        .await
        .unwrap();

    // Find by group
    let auth_metrics = MetricRepository::find_by_group(&storage, "auth")
        .await
        .unwrap();
    assert_eq!(auth_metrics.len(), 2);

    let order_metrics = MetricRepository::find_by_group(&storage, "orders")
        .await
        .unwrap();
    assert_eq!(order_metrics.len(), 1);
    assert_eq!(order_metrics[0].name, "order_placed");

    // Find all
    let all_metrics = MetricRepository::find_all(&storage).await.unwrap();
    assert_eq!(all_metrics.len(), 3);
}

// ============================================================
// EVENT LIFECYCLE TESTS
// ============================================================

#[tokio::test]
async fn test_event_full_lifecycle() {
    let storage = create_storage().await;

    // Create metric first
    let metric = create_metric("test_metric", "@test.py#1", "value");
    let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

    // Create and save event
    let mut event = MetricEvent::new(
        metric_id,
        "test_metric".to_string(),
        ConnectionId::from("default"),
        r#"{"result": 42}"#.to_string(),
    );
    event.value_numeric = Some(42.0);
    event.thread_name = Some("main".to_string());
    event.thread_id = Some(12345);

    let event_id = EventRepository::save(&storage, &event).await.unwrap();
    assert!(event_id > 0);

    // Find events for metric
    let events = EventRepository::find_by_metric_id(&storage, metric_id, 10)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].value_numeric, Some(42.0));
    assert_eq!(events[0].thread_name, Some("main".to_string()));

    // Count events
    let count = EventRepository::count_by_metric_id(&storage, metric_id)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_event_batch_operations() {
    let storage = create_storage().await;

    let metric = create_metric("batch_test", "@test.py#1", "value");
    let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

    // Create batch of events
    let events: Vec<MetricEvent> = (0..10)
        .map(|i| {
            let mut event = MetricEvent::new(
                metric_id,
                "batch_test".to_string(),
                ConnectionId::from("default"),
                format!(r#"{{"value": {}}}"#, i),
            );
            event.value_numeric = Some(i as f64);
            event
        })
        .collect();

    // Save batch
    let ids = EventRepository::save_batch(&storage, &events)
        .await
        .unwrap();
    assert_eq!(ids.len(), 10);

    // Verify count
    let count = EventRepository::count_by_metric_id(&storage, metric_id)
        .await
        .unwrap();
    assert_eq!(count, 10);

    // Test cleanup (keep only 5 most recent)
    let deleted = EventRepository::cleanup_metric_events(&storage, metric_id, 5)
        .await
        .unwrap();
    assert_eq!(deleted, 5);

    let remaining = EventRepository::count_by_metric_id(&storage, metric_id)
        .await
        .unwrap();
    assert_eq!(remaining, 5);
}

#[tokio::test]
async fn test_event_time_range_query() {
    let storage = create_storage().await;

    let metric = create_metric("time_test", "@test.py#1", "value");
    let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

    // Create events with specific timestamps
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;

    for i in 0..5 {
        let mut event = MetricEvent::new(
            metric_id,
            "time_test".to_string(),
            ConnectionId::from("default"),
            format!(r#"{{"i": {}}}"#, i),
        );
        event.timestamp = now + (i * 1_000_000); // 1 second apart
        EventRepository::save(&storage, &event).await.unwrap();
    }

    // Query middle range
    let start = now + 1_000_000;
    let end = now + 3_000_000;

    let events =
        EventRepository::find_by_metric_id_and_time_range(&storage, metric_id, start, end, 10)
            .await
            .unwrap();

    assert_eq!(events.len(), 3); // Events at t+1, t+2, t+3
}

// ============================================================
// ERROR HANDLING TESTS
// ============================================================

#[tokio::test]
async fn test_metric_duplicate_location_error() {
    let storage = create_storage().await;

    // Two metrics at the SAME location (different names is OK, but same location is not)
    let metric1 = create_metric("first_metric", "@test.py#100", "value");
    let metric2 = create_metric("second_metric", "@test.py#100", "other");

    // First save succeeds
    MetricRepository::save(&storage, &metric1).await.unwrap();

    // Second save fails with database error (same location)
    let result = MetricRepository::save(&storage, &metric2).await;
    assert!(result.is_err());

    // Verify it's a database error
    match result {
        Err(Error::Database(_)) => (),
        _ => panic!("Expected Database error for duplicate location"),
    }
}

#[tokio::test]
async fn test_metric_same_name_different_locations_ok() {
    let storage = create_storage().await;

    // Same name at different locations should now be allowed
    let metric1 = create_metric("shared_name", "@test.py#100", "value");
    let metric2 = create_metric("shared_name", "@test.py#200", "other");

    // Both saves should succeed
    let id1 = MetricRepository::save(&storage, &metric1).await.unwrap();
    let id2 = MetricRepository::save(&storage, &metric2).await.unwrap();

    // Should be different metrics
    assert_ne!(id1, id2);

    // Both should exist
    let all = MetricRepository::find_all(&storage).await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn test_metric_not_found_error() {
    let storage = create_storage().await;

    let non_existent_id = MetricId(99999);

    // Update non-existent metric
    let mut metric = create_metric("test", "@test.py#1", "value");
    metric.id = Some(non_existent_id);

    let result = MetricRepository::update(&storage, &metric).await;
    assert!(result.is_err());

    match result {
        Err(Error::MetricNotFound(_)) => (),
        _ => panic!("Expected MetricNotFound error"),
    }

    // Delete non-existent metric
    let result = MetricRepository::delete(&storage, non_existent_id).await;
    assert!(result.is_err());

    match result {
        Err(Error::MetricNotFound(_)) => (),
        _ => panic!("Expected MetricNotFound error"),
    }
}

#[tokio::test]
async fn test_invalid_metric_data() {
    // Test invalid metric name - starts with number
    let result = Metric::new(
        "123invalid".to_string(),
        ConnectionId::from("default"),
        Location::parse("@test.py#1").unwrap(),
        "value".to_string(),
        SourceLanguage::Python,
    );
    assert!(result.is_err());
    match result {
        Err(Error::InvalidMetricName(_)) => (),
        _ => panic!("Expected InvalidMetricName error for name starting with number"),
    }

    // Test invalid metric name - special characters
    let result = Metric::new(
        "invalid@name".to_string(),
        ConnectionId::from("default"),
        Location::parse("@test.py#1").unwrap(),
        "value".to_string(),
        SourceLanguage::Python,
    );
    assert!(result.is_err());
    match result {
        Err(Error::InvalidMetricName(_)) => (),
        _ => panic!("Expected InvalidMetricName error for special characters"),
    }

    // Test invalid metric name - consecutive hyphens
    let result = Metric::new(
        "invalid--name".to_string(),
        ConnectionId::from("default"),
        Location::parse("@test.py#1").unwrap(),
        "value".to_string(),
        SourceLanguage::Python,
    );
    assert!(result.is_err());
    match result {
        Err(Error::InvalidMetricName(_)) => (),
        _ => panic!("Expected InvalidMetricName error for consecutive hyphens"),
    }

    // Test invalid metric name - reserved word
    let result = Metric::new(
        "system".to_string(),
        ConnectionId::from("default"),
        Location::parse("@test.py#1").unwrap(),
        "value".to_string(),
        SourceLanguage::Python,
    );
    assert!(result.is_err());
    match result {
        Err(Error::InvalidMetricName(_)) => (),
        _ => panic!("Expected InvalidMetricName error for reserved word"),
    }

    // Test location without @ - now valid (@ is optional)
    let result = Location::parse("missing_at_sign.py#1");
    assert!(result.is_ok());
    let loc = result.unwrap();
    assert_eq!(loc.file, "missing_at_sign.py");
    assert_eq!(loc.line, 1);

    // Test invalid location - missing line number
    let result = Location::parse("@test.py");
    assert!(result.is_err());
    match result {
        Err(Error::InvalidLocation(_)) => (),
        _ => panic!("Expected InvalidLocation error for missing line"),
    }
}

// ============================================================
// CASCADE DELETE TESTS
// ============================================================

#[tokio::test]
async fn test_cascade_delete_events_on_metric_delete() {
    let storage = create_storage().await;

    // Create metric
    let metric = create_metric("cascade_test", "@test.py#1", "value");
    let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

    // Add events
    for i in 0..5 {
        let event = MetricEvent::new(
            metric_id,
            "cascade_test".to_string(),
            ConnectionId::from("default"),
            format!(r#"{{"i": {}}}"#, i),
        );
        EventRepository::save(&storage, &event).await.unwrap();
    }

    // Verify events exist
    let count_before = EventRepository::count_by_metric_id(&storage, metric_id)
        .await
        .unwrap();
    assert_eq!(count_before, 5);

    // Delete metric (should cascade delete events due to foreign key)
    MetricRepository::delete(&storage, metric_id).await.unwrap();

    // Verify events are gone (foreign key cascade)
    let count_after = EventRepository::count_by_metric_id(&storage, metric_id)
        .await
        .unwrap();
    assert_eq!(count_after, 0);
}

// ============================================================
// METRIC MODE SERIALIZATION TESTS
// ============================================================

#[tokio::test]
async fn test_metric_mode_persistence() {
    let storage = create_storage().await;

    // Test Stream mode
    let mut stream_metric = create_metric("stream", "@test.py#1", "value");
    stream_metric.mode = MetricMode::Stream;
    let id1 = MetricRepository::save(&storage, &stream_metric)
        .await
        .unwrap();
    let found1 = MetricRepository::find_by_id(&storage, id1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found1.mode, MetricMode::Stream);

    // Test Sample mode
    let mut sample_metric = create_metric("sample", "@test.py#2", "value");
    sample_metric.mode = MetricMode::Sample { rate: 10 };
    let id2 = MetricRepository::save(&storage, &sample_metric)
        .await
        .unwrap();
    let found2 = MetricRepository::find_by_id(&storage, id2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found2.mode, MetricMode::Sample { rate: 10 });

    // Test Throttle mode
    let mut throttle_metric = create_metric("throttle", "@test.py#3", "value");
    throttle_metric.mode = MetricMode::Throttle {
        max_per_second: 100,
    };
    let id3 = MetricRepository::save(&storage, &throttle_metric)
        .await
        .unwrap();
    let found3 = MetricRepository::find_by_id(&storage, id3)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        found3.mode,
        MetricMode::Throttle {
            max_per_second: 100
        }
    );

    // Test First mode
    let mut first_metric = create_metric("first", "@test.py#4", "value");
    first_metric.mode = MetricMode::First;
    let id4 = MetricRepository::save(&storage, &first_metric)
        .await
        .unwrap();
    let found4 = MetricRepository::find_by_id(&storage, id4)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found4.mode, MetricMode::First);
}

// ============================================================
// EVENT CLEANUP AND RETENTION TESTS
// ============================================================

#[tokio::test]
async fn test_event_retention_by_time() {
    let storage = create_storage().await;

    let metric = create_metric("retention_test", "@test.py#1", "value");
    let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;

    // Create old events
    for i in 0..3 {
        let mut event = MetricEvent::new(
            metric_id,
            "cleanup_test".to_string(),
            ConnectionId::from("default"),
            format!(r#"{{"old": {}}}"#, i),
        );
        event.timestamp = now - (86400 * 1_000_000); // 1 day ago
        EventRepository::save(&storage, &event).await.unwrap();
    }

    // Create new events
    for i in 0..3 {
        let mut event = MetricEvent::new(
            metric_id,
            "cleanup_test".to_string(),
            ConnectionId::from("default"),
            format!(r#"{{"new": {}}}"#, i),
        );
        event.timestamp = now;
        EventRepository::save(&storage, &event).await.unwrap();
    }

    // Delete events older than 1 hour ago
    let cutoff = now - (3600 * 1_000_000);
    let deleted = EventRepository::delete_older_than(&storage, cutoff)
        .await
        .unwrap();
    assert_eq!(deleted, 3);

    // Verify only new events remain
    let remaining = EventRepository::count_by_metric_id(&storage, metric_id)
        .await
        .unwrap();
    assert_eq!(remaining, 3);
}

// ============================================================
// CONCURRENT ACCESS TESTS
// ============================================================

#[tokio::test]
async fn test_concurrent_metric_creation() {
    let storage = create_storage().await;

    // Create 10 metrics concurrently
    // Each metric uses a unique location to satisfy location uniqueness constraint
    let mut handles = vec![];
    for i in 0..10 {
        let storage_clone = storage.clone();
        let handle = tokio::spawn(async move {
            let metric = create_metric(
                &format!("concurrent_{}", i),
                &format!("@test.py#{}", 100 + i),
                &format!("value_{}", i),
            );
            MetricRepository::save(&storage_clone, &metric).await
        });
        handles.push(handle);
    }

    // Wait for all to complete
    let results: Vec<_> = futures::future::join_all(handles).await;

    // All should succeed
    for result in results {
        assert!(result.unwrap().is_ok());
    }

    // Verify all 10 metrics exist
    let all = MetricRepository::find_all(&storage).await.unwrap();
    assert_eq!(all.len(), 10);
}

#[tokio::test]
async fn test_concurrent_event_insertion() {
    let storage = create_storage().await;

    let metric = create_metric("concurrent_events", "@test.py#1", "value");
    let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

    // Insert 50 events concurrently
    let mut handles = vec![];
    for i in 0..50 {
        let storage_clone = storage.clone();
        let handle = tokio::spawn(async move {
            let event = MetricEvent::new(
                metric_id,
                "concurrent_events".to_string(),
                ConnectionId::from("default"),
                format!(r#"{{"i": {}}}"#, i),
            );
            EventRepository::save(&storage_clone, &event).await
        });
        handles.push(handle);
    }

    // Wait for all to complete
    let results: Vec<_> = futures::future::join_all(handles).await;

    // All should succeed
    for result in results {
        assert!(result.unwrap().is_ok());
    }

    // Verify count
    let count = EventRepository::count_by_metric_id(&storage, metric_id)
        .await
        .unwrap();
    assert_eq!(count, 50);
}

// ============================================================
// DEAD-LETTER QUEUE E2E TESTS
// ============================================================

use detrix_application::ports::{DlqEntryStatus, DlqRepository};
use detrix_application::services::{DlqRecoveryService, EventCaptureService};
use detrix_config::DlqConfig;
use detrix_storage::DlqStorage;
use std::sync::Arc;

/// E2E test: Events fail to save → saved to DLQ → recovered successfully
///
/// This test simulates the complete DLQ recovery flow:
/// 1. Create metric and events
/// 2. Serialize events as if they failed to save (save to DLQ)
/// 3. Run DLQ recovery service
/// 4. Verify events are now in main storage
#[tokio::test]
async fn test_dlq_e2e_recovery_full_flow() {
    // Setup: Create separate storages for main DB and DLQ
    let main_storage = Arc::new(create_storage().await);
    let dlq_storage = Arc::new(DlqStorage::in_memory().await.unwrap());

    // Create a metric in main storage
    let metric = create_metric("dlq_test", "@test.py#100", "value");
    let metric_id = MetricRepository::save(&*main_storage, &metric)
        .await
        .unwrap();

    // Create events that would have "failed" to save
    let events: Vec<MetricEvent> = (0..5)
        .map(|i| {
            let mut event = MetricEvent::new(
                metric_id,
                "dlq_test".to_string(),
                ConnectionId::from("default"),
                format!(r#"{{"value": {}}}"#, i * 10),
            );
            event.value_numeric = Some((i * 10) as f64);
            event
        })
        .collect();

    // Simulate failure: Save events to DLQ (as if main storage failed)
    let error_message = "Database connection timeout";
    for event in &events {
        let event_json = serde_json::to_string(event).unwrap();
        dlq_storage
            .save(event_json, error_message.to_string())
            .await
            .unwrap();
    }

    // Verify events are in DLQ
    let pending_before = dlq_storage.find_pending(100).await.unwrap();
    assert_eq!(pending_before.len(), 5, "All 5 events should be in DLQ");

    // Verify NO events in main storage yet
    let events_before = EventRepository::count_by_metric_id(&*main_storage, metric_id)
        .await
        .unwrap();
    assert_eq!(
        events_before, 0,
        "Main storage should be empty before recovery"
    );

    // Create recovery service with separate DLQ storage
    let config = DlqConfig {
        enabled: true,
        retry_interval_ms: 100,
        max_retries: 3,
        batch_size: 50,
    };

    let recovery_service = DlqRecoveryService::new(
        dlq_storage.clone() as Arc<dyn DlqRepository + Send + Sync>,
        main_storage.clone() as Arc<dyn detrix_application::EventRepository + Send + Sync>,
        config,
    );

    // Run recovery (this is what happens on each tick of the background task)
    recovery_service.process_pending_entries().await.unwrap();

    // Verify: Events should now be in main storage
    let events_after = EventRepository::count_by_metric_id(&*main_storage, metric_id)
        .await
        .unwrap();
    assert_eq!(
        events_after, 5,
        "All 5 events should be recovered to main storage"
    );

    // Verify: DLQ should be empty (successfully recovered entries are deleted)
    let pending_after = dlq_storage.find_pending(100).await.unwrap();
    assert!(
        pending_after.is_empty(),
        "DLQ should be empty after successful recovery"
    );

    // Verify: Events have correct values
    let recovered_events = EventRepository::find_by_metric_id(&*main_storage, metric_id, 100)
        .await
        .unwrap();
    assert_eq!(recovered_events.len(), 5);

    // Check all values are present (order may vary)
    let values: Vec<i64> = recovered_events
        .iter()
        .filter_map(|e| e.value_numeric.map(|v| v as i64))
        .collect();
    assert!(values.contains(&0));
    assert!(values.contains(&10));
    assert!(values.contains(&20));
    assert!(values.contains(&30));
    assert!(values.contains(&40));
}

/// E2E test: DLQ retries up to max_retries then marks as failed
#[tokio::test]
async fn test_dlq_e2e_max_retries_exceeded() {
    let dlq_storage = Arc::new(DlqStorage::in_memory().await.unwrap());

    // Create a mock event repository that always fails
    let failing_storage = Arc::new(FailingEventRepository);

    // Save an event to DLQ with retry_count already at max
    let event = MetricEvent::new(
        MetricId(999),
        "test".to_string(),
        ConnectionId::from("default"),
        r#"{"value": 1}"#.to_string(),
    );
    let event_json = serde_json::to_string(&event).unwrap();

    // Manually insert with retry_count = 3 (at max)
    sqlx::query(
        r#"
        INSERT INTO dead_letter_events (event_json, error_message, retry_count, status, created_at)
        VALUES (?, ?, ?, 'pending', ?)
        "#,
    )
    .bind(&event_json)
    .bind("Simulated failure")
    .bind(3_i64) // max_retries
    .bind(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64,
    )
    .execute(dlq_storage.pool())
    .await
    .unwrap();

    let config = DlqConfig {
        enabled: true,
        retry_interval_ms: 100,
        max_retries: 3,
        batch_size: 50,
    };

    let recovery_service = DlqRecoveryService::new(
        dlq_storage.clone() as Arc<dyn DlqRepository + Send + Sync>,
        failing_storage as Arc<dyn detrix_application::EventRepository + Send + Sync>,
        config,
    );

    // Run recovery
    recovery_service.process_pending_entries().await.unwrap();

    // Entry should be marked as failed (not retried)
    let failed_count = dlq_storage
        .count_by_status(DlqEntryStatus::Failed)
        .await
        .unwrap();
    assert_eq!(
        failed_count, 1,
        "Entry should be marked as permanently failed"
    );

    let pending_count = dlq_storage
        .count_by_status(DlqEntryStatus::Pending)
        .await
        .unwrap();
    assert_eq!(pending_count, 0, "No entries should remain pending");
}

/// E2E test: DLQ stats tracking
#[tokio::test]
async fn test_dlq_e2e_stats() {
    let main_storage = Arc::new(create_storage().await);
    let dlq_storage = Arc::new(DlqStorage::in_memory().await.unwrap());

    let metric = create_metric("stats_test", "@stats.py#1", "x");
    let metric_id = MetricRepository::save(&*main_storage, &metric)
        .await
        .unwrap();

    // Add entries in different states
    // 2 pending (valid events that will recover)
    for i in 0..2 {
        let event = MetricEvent::new(
            metric_id,
            "stats_test".to_string(),
            ConnectionId::from("default"),
            format!(r#"{{"i": {}}}"#, i),
        );
        let event_json = serde_json::to_string(&event).unwrap();
        dlq_storage
            .save(event_json, "Error".to_string())
            .await
            .unwrap();
    }

    // 1 failed entry (manually set status)
    let failed_event = MetricEvent::new(
        metric_id,
        "stats_test".to_string(),
        ConnectionId::from("default"),
        r#"{"failed": true}"#.to_string(),
    );
    let failed_json = serde_json::to_string(&failed_event).unwrap();
    let failed_id = dlq_storage
        .save(failed_json, "Critical error".to_string())
        .await
        .unwrap();
    dlq_storage.mark_failed(failed_id).await.unwrap();

    let config = DlqConfig {
        enabled: true,
        retry_interval_ms: 100,
        max_retries: 3,
        batch_size: 50,
    };

    let recovery_service = DlqRecoveryService::new(
        dlq_storage.clone() as Arc<dyn DlqRepository + Send + Sync>,
        main_storage.clone() as Arc<dyn detrix_application::EventRepository + Send + Sync>,
        config,
    );

    // Get stats before recovery
    let stats_before = recovery_service.get_stats().await.unwrap();
    assert_eq!(stats_before.pending, 2);
    assert_eq!(stats_before.failed, 1);
    assert_eq!(stats_before.retrying, 0);
    assert_eq!(stats_before.total, 3);

    // Run recovery
    recovery_service.process_pending_entries().await.unwrap();

    // Get stats after recovery
    let stats_after = recovery_service.get_stats().await.unwrap();
    assert_eq!(stats_after.pending, 0, "Pending should be 0 after recovery");
    assert_eq!(stats_after.failed, 1, "Failed entry should remain");
    assert_eq!(stats_after.total, 1, "Only failed entry should remain");
}

/// E2E test: EventCaptureService.save_to_dlq integration
#[tokio::test]
async fn test_dlq_e2e_event_capture_service_integration() {
    let main_storage = Arc::new(create_storage().await);
    let dlq_storage = Arc::new(DlqStorage::in_memory().await.unwrap());

    let metric = create_metric("capture_dlq", "@capture.py#1", "val");
    let metric_id = MetricRepository::save(&*main_storage, &metric)
        .await
        .unwrap();

    // Create EventCaptureService with DLQ
    let capture_service = EventCaptureService::with_dlq(
        main_storage.clone() as Arc<dyn detrix_application::EventRepository + Send + Sync>,
        dlq_storage.clone() as Arc<dyn DlqRepository + Send + Sync>,
    );

    assert!(capture_service.has_dlq(), "DLQ should be enabled");

    // Create events
    let events: Vec<MetricEvent> = (0..3)
        .map(|i| {
            MetricEvent::new(
                metric_id,
                "capture_dlq".to_string(),
                ConnectionId::from("default"),
                format!(r#"{{"n": {}}}"#, i),
            )
        })
        .collect();

    // Simulate save failure by using save_to_dlq directly
    let saved_count = capture_service
        .save_to_dlq(&events, "Simulated storage failure")
        .await;
    assert_eq!(saved_count, 3, "All 3 events should be saved to DLQ");

    // Verify events are in DLQ
    let pending = dlq_storage.find_pending(100).await.unwrap();
    assert_eq!(pending.len(), 3);

    // Now run recovery
    let config = DlqConfig::default();
    let recovery_service = DlqRecoveryService::new(
        dlq_storage.clone() as Arc<dyn DlqRepository + Send + Sync>,
        main_storage.clone() as Arc<dyn detrix_application::EventRepository + Send + Sync>,
        config,
    );

    recovery_service.process_pending_entries().await.unwrap();

    // Verify recovery
    let event_count = EventRepository::count_by_metric_id(&*main_storage, metric_id)
        .await
        .unwrap();
    assert_eq!(event_count, 3, "All events should be recovered");
}

/// E2E test: Cleanup old failed DLQ entries
#[tokio::test]
async fn test_dlq_e2e_cleanup_old_failed() {
    let main_storage = Arc::new(create_storage().await);
    let dlq_storage = Arc::new(DlqStorage::in_memory().await.unwrap());

    // Create some failed entries
    for i in 0..5 {
        let id = dlq_storage
            .save(format!(r#"{{"old": {}}}"#, i), "Old error".to_string())
            .await
            .unwrap();
        dlq_storage.mark_failed(id).await.unwrap();
    }

    // Create some pending entries (should not be cleaned up)
    for i in 0..3 {
        dlq_storage
            .save(
                format!(r#"{{"pending": {}}}"#, i),
                "Recent error".to_string(),
            )
            .await
            .unwrap();
    }

    let config = DlqConfig::default();
    let recovery_service = DlqRecoveryService::new(
        dlq_storage.clone() as Arc<dyn DlqRepository + Send + Sync>,
        main_storage as Arc<dyn detrix_application::EventRepository + Send + Sync>,
        config,
    );

    // Cleanup entries older than 0 hours (all failed entries)
    let deleted = recovery_service.cleanup_old_failed(0).await.unwrap();
    assert_eq!(deleted, 5, "All 5 failed entries should be cleaned up");

    // Pending entries should remain
    let pending = dlq_storage.find_pending(100).await.unwrap();
    assert_eq!(pending.len(), 3, "Pending entries should not be affected");
}

// ============================================================
// SYSTEM EVENT & AUDIT TRAIL TESTS
// ============================================================

use detrix_application::SystemEventRepository;
use detrix_core::system_event::{SystemEvent, SystemEventType};

#[tokio::test]
async fn test_system_event_save_and_find() {
    let storage = create_storage().await;

    // Create and save a basic system event
    let event = SystemEvent::metric_added(1, "test_metric", "conn-1");
    let id = SystemEventRepository::save(&storage, &event).await.unwrap();
    assert!(id > 0);

    // Find recent events
    let events = SystemEventRepository::find_recent(&storage, 10)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, SystemEventType::MetricAdded);
    assert_eq!(events[0].metric_name, Some("test_metric".to_string()));
}

#[tokio::test]
async fn test_audit_event_config_updated_persistence() {
    let storage = create_storage().await;

    // Create config updated audit event
    let event = SystemEvent::config_updated("user-123", r#"{"timeout": 30}"#, r#"{"timeout": 60}"#);

    let id = SystemEventRepository::save(&storage, &event).await.unwrap();
    assert!(id > 0);

    // Find by type
    let events = SystemEventRepository::find_by_type(&storage, SystemEventType::ConfigUpdated, 10)
        .await
        .unwrap();

    assert_eq!(events.len(), 1);
    let saved = &events[0];

    // Verify audit fields
    assert_eq!(saved.event_type, SystemEventType::ConfigUpdated);
    assert_eq!(saved.actor, Some("user-123".to_string()));
    assert_eq!(saved.resource_type, Some("config".to_string()));
    assert_eq!(saved.resource_id, Some("global".to_string()));
    assert_eq!(saved.old_value, Some(r#"{"timeout": 30}"#.to_string()));
    assert_eq!(saved.new_value, Some(r#"{"timeout": 60}"#.to_string()));
    assert!(saved.is_audit());
}

#[tokio::test]
async fn test_audit_event_api_call_persistence() {
    let storage = create_storage().await;

    // Create API call audit event
    let event = SystemEvent::api_call(
        "client-abc",
        "POST",
        "/api/metrics",
        "metric",
        Some("metric-42".to_string()),
    );

    let id = SystemEventRepository::save(&storage, &event).await.unwrap();
    assert!(id > 0);

    // Find recent
    let events = SystemEventRepository::find_recent(&storage, 10)
        .await
        .unwrap();

    assert_eq!(events.len(), 1);
    let saved = &events[0];

    assert_eq!(saved.event_type, SystemEventType::ApiCallExecuted);
    assert_eq!(saved.actor, Some("client-abc".to_string()));
    assert_eq!(saved.resource_type, Some("metric".to_string()));
    assert_eq!(saved.resource_id, Some("metric-42".to_string()));
}

#[tokio::test]
async fn test_audit_event_authentication_failed_persistence() {
    let storage = create_storage().await;

    // Create authentication failed audit event
    let event = SystemEvent::authentication_failed(
        "unknown",
        "invalid JWT token",
        Some("192.168.1.100".to_string()),
    );

    let id = SystemEventRepository::save(&storage, &event).await.unwrap();
    assert!(id > 0);

    // Find by type
    let events =
        SystemEventRepository::find_by_type(&storage, SystemEventType::AuthenticationFailed, 10)
            .await
            .unwrap();

    assert_eq!(events.len(), 1);
    let saved = &events[0];

    assert_eq!(saved.event_type, SystemEventType::AuthenticationFailed);
    assert_eq!(saved.actor, Some("unknown".to_string()));
    assert_eq!(saved.resource_type, Some("authentication".to_string()));

    // Verify details JSON contains IP
    let details: serde_json::Value =
        serde_json::from_str(saved.details_json.as_ref().unwrap()).unwrap();
    assert_eq!(details["ip_address"], "192.168.1.100");
    assert_eq!(details["reason"], "invalid JWT token");
}

#[tokio::test]
async fn test_audit_event_expression_validated_persistence() {
    let storage = create_storage().await;

    // Create expression validated audit event (failed validation)
    let event = SystemEvent::expression_validated(
        "system",
        "os.system('ls')",
        "python",
        false,
        Some("prohibited function: os.system".to_string()),
    );

    let id = SystemEventRepository::save(&storage, &event).await.unwrap();
    assert!(id > 0);

    // Find by type
    let events =
        SystemEventRepository::find_by_type(&storage, SystemEventType::ExpressionValidated, 10)
            .await
            .unwrap();

    assert_eq!(events.len(), 1);
    let saved = &events[0];

    assert_eq!(saved.event_type, SystemEventType::ExpressionValidated);
    assert!(saved.is_audit());

    // Verify details JSON
    let details: serde_json::Value =
        serde_json::from_str(saved.details_json.as_ref().unwrap()).unwrap();
    assert_eq!(details["expression"], "os.system('ls')");
    assert_eq!(details["language"], "python");
    assert_eq!(details["is_safe"], false);
    assert!(details["rejection_reason"]
        .as_str()
        .unwrap()
        .contains("prohibited"));
}

#[tokio::test]
async fn test_audit_event_batch_save() {
    let storage = create_storage().await;

    // Create batch of audit events
    let events: Vec<SystemEvent> = vec![
        SystemEvent::config_updated("user-1", "old1", "new1"),
        SystemEvent::api_call("client-1", "GET", "/api/metrics", "metrics", None),
        SystemEvent::authentication_failed("unknown", "bad token", None),
    ];

    let ids = SystemEventRepository::save_batch(&storage, &events)
        .await
        .unwrap();
    assert_eq!(ids.len(), 3);

    // Verify all saved
    let all_events = SystemEventRepository::find_recent(&storage, 10)
        .await
        .unwrap();
    assert_eq!(all_events.len(), 3);

    // Check each type is present
    let types: Vec<SystemEventType> = all_events.iter().map(|e| e.event_type.clone()).collect();
    assert!(types.contains(&SystemEventType::ConfigUpdated));
    assert!(types.contains(&SystemEventType::ApiCallExecuted));
    assert!(types.contains(&SystemEventType::AuthenticationFailed));
}

#[tokio::test]
async fn test_audit_event_find_since() {
    let storage = create_storage().await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;

    // Create events with specific timestamps
    for i in 0..5 {
        let mut event = SystemEvent::config_updated(&format!("user-{}", i), "old", "new");
        event.timestamp = now + (i * 1_000_000); // 1 second apart
        SystemEventRepository::save(&storage, &event).await.unwrap();
    }

    // Find events since t+2 seconds
    let events = SystemEventRepository::find_since(&storage, now + 2_000_000, 10)
        .await
        .unwrap();

    // Should find events at t+3 and t+4
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn test_audit_event_acknowledge() {
    let storage = create_storage().await;

    // Create unacknowledged events
    let event1 = SystemEvent::authentication_failed("user-1", "bad token", None);
    let event2 = SystemEvent::authentication_failed("user-2", "expired token", None);

    let id1 = SystemEventRepository::save(&storage, &event1)
        .await
        .unwrap();
    let id2 = SystemEventRepository::save(&storage, &event2)
        .await
        .unwrap();

    // Verify unacknowledged count
    let unack_count = SystemEventRepository::count_unacknowledged(&storage)
        .await
        .unwrap();
    assert_eq!(unack_count, 2);

    // Acknowledge first event
    let ack_count = SystemEventRepository::acknowledge(&storage, &[id1])
        .await
        .unwrap();
    assert_eq!(ack_count, 1);

    // Verify only one unacknowledged
    let unack_count_after = SystemEventRepository::count_unacknowledged(&storage)
        .await
        .unwrap();
    assert_eq!(unack_count_after, 1);

    // Find unacknowledged - should only be id2
    let unack_events = SystemEventRepository::find_unacknowledged(&storage, 10)
        .await
        .unwrap();
    assert_eq!(unack_events.len(), 1);
    assert_eq!(unack_events[0].id, Some(id2));
}

#[tokio::test]
async fn test_audit_event_delete_older_than() {
    let storage = create_storage().await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;

    // Create old events (1 day ago)
    for i in 0..3 {
        let mut event = SystemEvent::config_updated(&format!("old-user-{}", i), "old", "new");
        event.timestamp = now - (86400 * 1_000_000); // 1 day ago
        SystemEventRepository::save(&storage, &event).await.unwrap();
    }

    // Create recent events
    for i in 0..2 {
        let mut event =
            SystemEvent::api_call(&format!("new-client-{}", i), "GET", "/api", "test", None);
        event.timestamp = now;
        SystemEventRepository::save(&storage, &event).await.unwrap();
    }

    // Delete events older than 1 hour
    let cutoff = now - (3600 * 1_000_000);
    let deleted = SystemEventRepository::delete_older_than(&storage, cutoff)
        .await
        .unwrap();
    assert_eq!(deleted, 3);

    // Verify only recent events remain
    let remaining = SystemEventRepository::count_all(&storage).await.unwrap();
    assert_eq!(remaining, 2);
}

#[tokio::test]
async fn test_mixed_system_and_audit_events() {
    let storage = create_storage().await;

    // Create mixed events
    let events: Vec<SystemEvent> = vec![
        // Regular system events
        SystemEvent::metric_added(1, "metric1", "conn-1"),
        SystemEvent::connection_created("conn-1", "localhost", 5678, "python"),
        // Audit events
        SystemEvent::config_updated("admin", "old", "new"),
        SystemEvent::api_call(
            "client",
            "POST",
            "/api/metrics",
            "metric",
            Some("1".to_string()),
        ),
    ];

    SystemEventRepository::save_batch(&storage, &events)
        .await
        .unwrap();

    // Find all
    let all = SystemEventRepository::find_recent(&storage, 10)
        .await
        .unwrap();
    assert_eq!(all.len(), 4);

    // Filter audit events
    let audit_events: Vec<&SystemEvent> = all.iter().filter(|e| e.is_audit()).collect();
    assert_eq!(audit_events.len(), 2);

    // Verify audit events have actor field
    for audit in &audit_events {
        assert!(audit.actor.is_some());
    }

    // Verify non-audit events don't have actor field
    let non_audit: Vec<&SystemEvent> = all.iter().filter(|e| !e.is_audit()).collect();
    for event in &non_audit {
        assert!(event.actor.is_none());
    }
}

#[tokio::test]
async fn test_audit_event_query_by_connection() {
    let storage = create_storage().await;

    // Create events for different connections
    let event1 = SystemEvent::metric_added(1, "m1", "conn-alpha");
    SystemEventRepository::save(&storage, &event1)
        .await
        .unwrap();

    let event2 = SystemEvent::metric_added(2, "m2", "conn-beta");
    SystemEventRepository::save(&storage, &event2)
        .await
        .unwrap();

    let event3 = SystemEvent::metric_removed(3, "m3", "conn-alpha");
    SystemEventRepository::save(&storage, &event3)
        .await
        .unwrap();

    // Query by connection
    let alpha_events =
        SystemEventRepository::find_by_connection(&storage, &ConnectionId::from("conn-alpha"), 10)
            .await
            .unwrap();

    assert_eq!(alpha_events.len(), 2);
    for event in &alpha_events {
        assert_eq!(event.connection_id, Some(ConnectionId::from("conn-alpha")));
    }
}

/// Mock event repository that always fails (for testing retry behavior)
struct FailingEventRepository;

#[async_trait::async_trait]
impl detrix_application::EventRepository for FailingEventRepository {
    async fn save(&self, _event: &MetricEvent) -> detrix_core::error::Result<i64> {
        Err(Error::Database("Simulated failure".to_string()))
    }

    async fn save_batch(&self, _events: &[MetricEvent]) -> detrix_core::error::Result<Vec<i64>> {
        Err(Error::Database("Simulated batch failure".to_string()))
    }

    async fn find_by_metric_id(
        &self,
        _metric_id: MetricId,
        _limit: i64,
    ) -> detrix_core::error::Result<Vec<MetricEvent>> {
        Ok(Vec::new())
    }

    async fn find_by_metric_ids(
        &self,
        _metric_ids: &[MetricId],
        _limit: i64,
    ) -> detrix_core::error::Result<Vec<MetricEvent>> {
        Ok(Vec::new())
    }

    async fn find_by_metric_id_and_time_range(
        &self,
        _metric_id: MetricId,
        _start: i64,
        _end: i64,
        _limit: i64,
    ) -> detrix_core::error::Result<Vec<MetricEvent>> {
        Ok(Vec::new())
    }

    async fn count_by_metric_id(&self, _metric_id: MetricId) -> detrix_core::error::Result<i64> {
        Ok(0)
    }

    async fn count_all(&self) -> detrix_core::error::Result<i64> {
        Ok(0)
    }

    async fn find_recent(&self, _limit: i64) -> detrix_core::error::Result<Vec<MetricEvent>> {
        Ok(Vec::new())
    }

    async fn cleanup_metric_events(
        &self,
        _metric_id: MetricId,
        _keep_count: usize,
    ) -> detrix_core::error::Result<u64> {
        Ok(0)
    }

    async fn delete_older_than(&self, _timestamp: i64) -> detrix_core::error::Result<u64> {
        Ok(0)
    }
}
