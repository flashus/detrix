//! Tests for McpUsageRepository implementation
//!
//! Tests for MCP usage event storage and statistics.

use super::SqliteStorage;
use detrix_application::services::McpUsageEvent;
use detrix_application::McpUsageRepository;
use detrix_core::McpErrorCode;

/// Create in-memory storage for testing
async fn create_test_storage() -> SqliteStorage {
    SqliteStorage::in_memory().await.unwrap()
}

/// Helper to get current timestamp in microseconds
fn now_micros() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Create a test MCP usage event
fn create_test_event(tool_name: &str, success: bool) -> McpUsageEvent {
    McpUsageEvent {
        timestamp_micros: now_micros(),
        tool_name: tool_name.to_string(),
        success,
        error_code: if success {
            None
        } else {
            Some(McpErrorCode::Other)
        },
        latency_ms: 100,
        session_id: Some("test-session".to_string()),
        prior_tools: vec![],
    }
}

// =============================================================================
// Basic CRUD Tests
// =============================================================================

#[tokio::test]
async fn test_save_event_and_count() {
    let storage = create_test_storage().await;

    // Initially empty
    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 0);

    // Save an event
    let event = create_test_event("add_metric", true);
    let id = McpUsageRepository::save_event(&storage, &event)
        .await
        .unwrap();
    assert!(id > 0);

    // Count should be 1
    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_save_multiple_events() {
    let storage = create_test_storage().await;

    // Save multiple events
    for i in 0..5 {
        let event = create_test_event(&format!("tool_{}", i), true);
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 5);
}

#[tokio::test]
async fn test_save_batch_empty() {
    let storage = create_test_storage().await;

    let ids = McpUsageRepository::save_batch(&storage, &[]).await.unwrap();
    assert!(ids.is_empty());

    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_save_batch_with_events() {
    let storage = create_test_storage().await;

    let events: Vec<McpUsageEvent> = (0..10)
        .map(|i| create_test_event(&format!("batch_tool_{}", i), i % 2 == 0))
        .collect();

    let ids = McpUsageRepository::save_batch(&storage, &events)
        .await
        .unwrap();
    assert_eq!(ids.len(), 10);

    // All IDs should be positive
    assert!(ids.iter().all(|&id| id > 0));

    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 10);
}

// =============================================================================
// Statistics Tests
// =============================================================================

#[tokio::test]
async fn test_get_stats_empty_database() {
    let storage = create_test_storage().await;

    let stats = McpUsageRepository::get_stats(&storage, None, None)
        .await
        .unwrap();

    assert_eq!(stats.total_calls, 0);
    assert_eq!(stats.total_success, 0);
    assert_eq!(stats.total_errors, 0);
    assert_eq!(stats.avg_latency_ms, 0.0);
}

#[tokio::test]
async fn test_get_stats_all_time() {
    let storage = create_test_storage().await;

    // Save mix of success and error events
    // i % 3 != 0: success at 1,2,4,5,7,8 (6 total), error at 0,3,6,9 (4 total)
    for i in 0..10 {
        let event = create_test_event("test_tool", i % 3 != 0);
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    let stats = McpUsageRepository::get_stats(&storage, None, None)
        .await
        .unwrap();

    assert_eq!(stats.total_calls, 10);
    assert_eq!(stats.total_success, 6);
    assert_eq!(stats.total_errors, 4);
    assert!(stats.avg_latency_ms > 0.0);
}

#[tokio::test]
async fn test_get_stats_with_time_filter() {
    let storage = create_test_storage().await;

    let base_time = 1_000_000_000_000_i64; // Arbitrary base timestamp

    // Create events at different times
    for i in 0..5 {
        let mut event = create_test_event("old_tool", true);
        event.timestamp_micros = base_time + (i * 1_000_000); // Old events
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    for i in 0..3 {
        let mut event = create_test_event("new_tool", false);
        event.timestamp_micros = base_time + 100_000_000 + (i * 1_000_000); // New events
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    // Filter for old events only
    let stats =
        McpUsageRepository::get_stats(&storage, Some(base_time), Some(base_time + 50_000_000))
            .await
            .unwrap();

    assert_eq!(stats.total_calls, 5);
    assert_eq!(stats.total_success, 5);
    assert_eq!(stats.total_errors, 0);

    // Filter for new events only
    let stats = McpUsageRepository::get_stats(&storage, Some(base_time + 50_000_000), None)
        .await
        .unwrap();

    assert_eq!(stats.total_calls, 3);
    assert_eq!(stats.total_success, 0);
    assert_eq!(stats.total_errors, 3);
}

// =============================================================================
// Error Breakdown Tests
// =============================================================================

#[tokio::test]
async fn test_get_error_breakdown_empty() {
    let storage = create_test_storage().await;

    let breakdown = McpUsageRepository::get_error_breakdown(&storage, None)
        .await
        .unwrap();

    assert!(breakdown.is_empty());
}

#[tokio::test]
async fn test_get_error_breakdown_with_errors() {
    let storage = create_test_storage().await;

    // Create events with different error codes
    for _ in 0..3 {
        let mut event = create_test_event("tool1", false);
        event.error_code = Some(McpErrorCode::MetricBeforeConnection);
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    for _ in 0..2 {
        let mut event = create_test_event("tool2", false);
        event.error_code = Some(McpErrorCode::Other);
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    // Also add a success event (should not appear in breakdown)
    let success_event = create_test_event("tool3", true);
    McpUsageRepository::save_event(&storage, &success_event)
        .await
        .unwrap();

    let breakdown = McpUsageRepository::get_error_breakdown(&storage, None)
        .await
        .unwrap();

    assert_eq!(breakdown.len(), 2);

    // Should be sorted by count descending
    assert_eq!(breakdown[0].count, 3);
    assert_eq!(breakdown[1].count, 2);
}

// =============================================================================
// Tool Counts Tests
// =============================================================================

#[tokio::test]
async fn test_get_tool_counts_empty() {
    let storage = create_test_storage().await;

    let counts = McpUsageRepository::get_tool_counts(&storage, None)
        .await
        .unwrap();

    assert!(counts.is_empty());
}

#[tokio::test]
async fn test_get_tool_counts_with_events() {
    let storage = create_test_storage().await;

    // Create events for different tools
    for _ in 0..5 {
        let event = create_test_event("add_metric", true);
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    for _ in 0..3 {
        let event = create_test_event("list_metrics", true);
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    for i in 0..2 {
        let event = create_test_event("remove_metric", i == 0); // 1 success, 1 error
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    let counts = McpUsageRepository::get_tool_counts(&storage, None)
        .await
        .unwrap();

    assert_eq!(counts.len(), 3);

    // Should be sorted by count descending
    assert_eq!(counts[0].tool_name, "add_metric");
    assert_eq!(counts[0].count, 5);
    assert_eq!(counts[0].success_count, 5);
    assert_eq!(counts[0].error_count, 0);

    assert_eq!(counts[1].tool_name, "list_metrics");
    assert_eq!(counts[1].count, 3);

    assert_eq!(counts[2].tool_name, "remove_metric");
    assert_eq!(counts[2].count, 2);
    assert_eq!(counts[2].success_count, 1);
    assert_eq!(counts[2].error_count, 1);
}

// =============================================================================
// Cleanup Tests
// =============================================================================

#[tokio::test]
async fn test_delete_older_than() {
    let storage = create_test_storage().await;

    let base_time = 1_000_000_000_000_i64;

    // Create old events
    for i in 0..5 {
        let mut event = create_test_event("old_tool", true);
        event.timestamp_micros = base_time + (i * 1_000); // Old
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    // Create new events
    for i in 0..3 {
        let mut event = create_test_event("new_tool", true);
        event.timestamp_micros = base_time + 100_000_000 + (i * 1_000); // New
        McpUsageRepository::save_event(&storage, &event)
            .await
            .unwrap();
    }

    // Verify total count
    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 8);

    // Delete old events
    let deleted = McpUsageRepository::delete_older_than(&storage, base_time + 50_000_000)
        .await
        .unwrap();
    assert_eq!(deleted, 5);

    // Verify remaining count
    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
async fn test_delete_older_than_none() {
    let storage = create_test_storage().await;

    // Create events in the future
    let mut event = create_test_event("future_tool", true);
    event.timestamp_micros = i64::MAX - 1000;
    McpUsageRepository::save_event(&storage, &event)
        .await
        .unwrap();

    // Try to delete events older than a very early time
    let deleted = McpUsageRepository::delete_older_than(&storage, 1000)
        .await
        .unwrap();
    assert_eq!(deleted, 0);

    // Event should still exist
    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 1);
}

// =============================================================================
// Edge Cases
// =============================================================================

#[tokio::test]
async fn test_event_with_prior_tools() {
    let storage = create_test_storage().await;

    let mut event = create_test_event("add_metric", true);
    event.prior_tools = vec![
        "list_connections".to_string(),
        "create_connection".to_string(),
        "inspect_file".to_string(),
    ];

    let id = McpUsageRepository::save_event(&storage, &event)
        .await
        .unwrap();
    assert!(id > 0);

    // Verify event was saved (prior_tools is stored as JSON)
    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_event_without_session_id() {
    let storage = create_test_storage().await;

    let mut event = create_test_event("anonymous_tool", true);
    event.session_id = None;

    let id = McpUsageRepository::save_event(&storage, &event)
        .await
        .unwrap();
    assert!(id > 0);

    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_save_batch_large() {
    let storage = create_test_storage().await;

    // Create a batch larger than typical chunk size
    let events: Vec<McpUsageEvent> = (0..150)
        .map(|i| create_test_event(&format!("tool_{}", i % 10), true))
        .collect();

    let ids = McpUsageRepository::save_batch(&storage, &events)
        .await
        .unwrap();
    assert_eq!(ids.len(), 150);

    let count = McpUsageRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 150);
}
