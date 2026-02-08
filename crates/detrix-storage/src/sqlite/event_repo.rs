//! EventRepository implementation for SQLite

use super::SqliteStorage;
use crate::json_helpers::{deserialize_optional, serialize_optional};
use async_trait::async_trait;
use detrix_application::EventRepository;
use detrix_config::constants::SQLITE_MAX_VARIABLES;
use detrix_core::entities::{ExpressionValue, MetricEvent, MetricId};
use detrix_core::error::Result;
use detrix_core::ConnectionId;
use sqlx::Row;
use tracing::trace;

#[async_trait]
impl EventRepository for SqliteStorage {
    async fn save(&self, event: &MetricEvent) -> Result<i64> {
        // Serialize introspection data to JSON using helpers
        let context = format!("metric_id={}", event.metric_id.0);
        let stack_trace_json = serialize_optional(&event.stack_trace, "stack_trace", &context);
        let memory_snapshot_json =
            serialize_optional(&event.memory_snapshot, "memory_snapshot", &context);

        // Serialize multi-expression values as JSON
        let values_json = serde_json::to_string(&event.values)?;

        let id = sqlx::query(
            r#"
            INSERT INTO metric_events (
                metric_id, metric_name, connection_id, timestamp, thread_name, thread_id,
                values_json,
                is_error, error_type, error_message, error_traceback,
                request_id, session_id, stack_trace_json, memory_snapshot_json
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(event.metric_id.0 as i64) // SQLite only supports i64
        .bind(&event.metric_name)
        .bind(&event.connection_id.0)
        .bind(event.timestamp)
        .bind(&event.thread_name)
        .bind(event.thread_id)
        .bind(&values_json)
        .bind(event.is_error)
        .bind(&event.error_type)
        .bind(&event.error_message)
        .bind(&None::<String>) // error_traceback
        .bind(&event.request_id)
        .bind(&None::<String>) // session_id
        .bind(&stack_trace_json)
        .bind(&memory_snapshot_json)
        .execute(self.pool())
        .await?
        .last_insert_rowid();

        Ok(id)
    }

    async fn save_batch(&self, events: &[MetricEvent]) -> Result<Vec<i64>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        // SQLite has a variable limit (SQLITE_MAX_VARIABLE_NUMBER, default 999)
        // With 15 columns per event, we can safely batch ~66 events (66 * 15 = 990 < 999)
        const COLUMNS_PER_EVENT: usize = 15;
        const CHUNK_SIZE: usize = SQLITE_MAX_VARIABLES / COLUMNS_PER_EVENT;

        let mut all_ids = Vec::with_capacity(events.len());

        // Use a transaction for atomicity
        let mut tx = self.pool().begin().await?;

        for chunk in events.chunks(CHUNK_SIZE) {
            // Build multi-value INSERT: INSERT INTO ... VALUES (?,?,...), (?,?,...), ...
            let placeholder = "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
            let joined = vec![placeholder; chunk.len()].join(", ");

            let query = format!(
                r#"
                INSERT INTO metric_events (
                    metric_id, metric_name, connection_id, timestamp, thread_name, thread_id,
                    values_json,
                    is_error, error_type, error_message, error_traceback,
                    request_id, session_id, stack_trace_json, memory_snapshot_json
                )
                VALUES {}
                "#,
                joined
            );

            // Pre-serialize all introspection and values data so references live long enough
            let serialized_data: Vec<(Option<String>, Option<String>, String)> = chunk
                .iter()
                .map(|event| -> Result<_> {
                    let context = format!("metric_id={}", event.metric_id.0);
                    let stack_trace_json =
                        serialize_optional(&event.stack_trace, "stack_trace", &context);
                    let memory_snapshot_json =
                        serialize_optional(&event.memory_snapshot, "memory_snapshot", &context);
                    let values_json = serde_json::to_string(&event.values)?;
                    Ok((stack_trace_json, memory_snapshot_json, values_json))
                })
                .collect::<Result<Vec<_>>>()?;

            let mut query_builder = sqlx::query(&query);

            // Bind all parameters for all events in this chunk
            for (event, (stack_trace_json, memory_snapshot_json, values_json)) in
                chunk.iter().zip(serialized_data.iter())
            {
                query_builder = query_builder
                    .bind(event.metric_id.0 as i64) // SQLite only supports i64
                    .bind(&event.metric_name)
                    .bind(&event.connection_id.0)
                    .bind(event.timestamp)
                    .bind(&event.thread_name)
                    .bind(event.thread_id)
                    .bind(values_json)
                    .bind(event.is_error)
                    .bind(&event.error_type)
                    .bind(&event.error_message)
                    .bind(&None::<String>) // error_traceback
                    .bind(&event.request_id)
                    .bind(&None::<String>) // session_id
                    .bind(stack_trace_json)
                    .bind(memory_snapshot_json);
            }

            let result = query_builder.execute(&mut *tx).await?;

            // For multi-row INSERT, last_insert_rowid() returns the rowid of the LAST inserted row
            // SQLite assigns consecutive rowids for multi-row INSERT (assuming no conflicts)
            // We need to calculate IDs for all rows: first_id = last_id - rows_affected + 1
            let last_id = result.last_insert_rowid();
            let rows_affected = result.rows_affected() as i64;
            let first_id = last_id - rows_affected + 1;

            for i in 0..rows_affected {
                all_ids.push(first_id + i);
            }
        }

        tx.commit().await?;

        trace!(
            events_count = events.len(),
            ids_count = all_ids.len(),
            "Batch saved events with multi-value INSERT"
        );

        Ok(all_ids)
    }

    async fn find_by_metric_id(&self, metric_id: MetricId, limit: i64) -> Result<Vec<MetricEvent>> {
        let rows = sqlx::query(
            "SELECT * FROM metric_events WHERE metric_id = ? ORDER BY timestamp DESC LIMIT ?",
        )
        .bind(metric_id.0 as i64) // SQLite only supports i64
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_event).collect()
    }

    async fn find_by_metric_id_since(
        &self,
        metric_id: MetricId,
        since_micros: i64,
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM metric_events
            WHERE metric_id = ? AND timestamp >= ?
            ORDER BY timestamp DESC
            LIMIT ?
            "#,
        )
        .bind(metric_id.0 as i64) // SQLite only supports i64
        .bind(since_micros)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_event).collect()
    }

    async fn find_by_metric_ids(
        &self,
        metric_ids: &[MetricId],
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        if metric_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Build IN clause with placeholders
        let placeholders: Vec<&str> = metric_ids.iter().map(|_| "?").collect();
        let query = format!(
            "SELECT * FROM metric_events WHERE metric_id IN ({}) ORDER BY timestamp DESC LIMIT ?",
            placeholders.join(", ")
        );

        let mut query_builder = sqlx::query(&query);
        for id in metric_ids {
            query_builder = query_builder.bind(id.0 as i64); // SQLite only supports i64
        }
        query_builder = query_builder.bind(limit);

        let rows = query_builder.fetch_all(self.pool()).await?;

        rows.iter().map(row_to_event).collect()
    }

    async fn find_by_metric_id_and_time_range(
        &self,
        metric_id: MetricId,
        start_micros: i64,
        end_micros: i64,
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM metric_events
            WHERE metric_id = ? AND timestamp >= ? AND timestamp <= ?
            ORDER BY timestamp DESC
            LIMIT ?
            "#,
        )
        .bind(metric_id.0 as i64) // SQLite only supports i64
        .bind(start_micros)
        .bind(end_micros)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_event).collect()
    }

    async fn count_by_metric_id(&self, metric_id: MetricId) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM metric_events WHERE metric_id = ?")
                .bind(metric_id.0 as i64) // SQLite only supports i64
                .fetch_one(self.pool())
                .await?;

        Ok(count)
    }

    async fn count_all(&self) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metric_events")
            .fetch_one(self.pool())
            .await?;

        Ok(count)
    }

    async fn find_recent(&self, limit: i64) -> Result<Vec<MetricEvent>> {
        let rows = sqlx::query("SELECT * FROM metric_events ORDER BY timestamp DESC LIMIT ?")
            .bind(limit)
            .fetch_all(self.pool())
            .await?;

        rows.iter().map(row_to_event).collect()
    }

    async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM metric_events WHERE timestamp < ?")
            .bind(timestamp_micros)
            .execute(self.pool())
            .await?;

        Ok(result.rows_affected())
    }

    async fn cleanup_metric_events(&self, metric_id: MetricId, keep_count: usize) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM metric_events
            WHERE metric_id = ? AND id NOT IN (
                SELECT id FROM metric_events
                WHERE metric_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
            )
            "#,
        )
        .bind(metric_id.0 as i64) // SQLite only supports i64
        .bind(metric_id.0 as i64) // SQLite only supports i64
        .bind(keep_count as i64)
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected())
    }
}

/// Convert database row to MetricEvent entity
fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> Result<MetricEvent> {
    let id: i64 = row.try_get("id")?;
    let metric_id: i64 = row.try_get("metric_id")?;
    let timestamp: i64 = row.try_get("timestamp")?;
    let thread_name: Option<String> = row.try_get("thread_name")?;
    let thread_id: Option<i64> = row.try_get("thread_id")?;
    let metric_name: String = row.try_get("metric_name").unwrap_or_default();
    let connection_id: String = row.try_get("connection_id")?;
    let is_error: bool = row.try_get("is_error")?;
    let error_type: Option<String> = row.try_get("error_type")?;
    let error_message: Option<String> = row.try_get("error_message")?;
    let request_id: Option<String> = row.try_get("request_id")?;

    // Read multi-expression values JSON (canonical column)
    let values_json_str: String = row.try_get("values_json")?;
    let values: Vec<ExpressionValue> = serde_json::from_str(&values_json_str).map_err(|e| {
        detrix_core::error::Error::Database(format!(
            "Failed to parse values_json for event {}: {}",
            id, e
        ))
    })?;

    // Parse introspection data from JSON columns
    let stack_trace_json: Option<String> = row.try_get("stack_trace_json").unwrap_or(None);
    let memory_snapshot_json: Option<String> = row.try_get("memory_snapshot_json").unwrap_or(None);

    let context = format!("metric_id={}", metric_id);
    let stack_trace = deserialize_optional(stack_trace_json.as_deref(), "stack_trace", &context);
    let memory_snapshot =
        deserialize_optional(memory_snapshot_json.as_deref(), "memory_snapshot", &context);

    Ok(MetricEvent {
        id: Some(id),
        metric_id: MetricId(metric_id as u64), // SQLite returns i64, domain uses u64
        metric_name,
        connection_id: ConnectionId::from(connection_id),
        timestamp,
        thread_name,
        thread_id,
        values,
        is_error,
        error_type,
        error_message,
        request_id,
        session_id: None, // Session tracking via gRPC metadata
        stack_trace,
        memory_snapshot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::metric_repo_tests::*;
    use detrix_application::MetricRepository;

    #[tokio::test]
    async fn test_event_save_and_find() {
        let storage = create_test_storage().await;
        let metric = create_test_metric("event_test").await;
        let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

        let event = MetricEvent::new(
            metric_id,
            "event_test".to_string(),
            ConnectionId::from("default"),
            r#"{"user_id":123}"#.to_string(),
        );

        let event_id = EventRepository::save(&storage, &event).await.unwrap();
        assert!(event_id > 0);

        let events = EventRepository::find_by_metric_id(&storage, metric_id, 10)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].value_json(), r#"{"user_id":123}"#);
    }

    #[tokio::test]
    async fn test_event_count() {
        let storage = create_test_storage().await;
        let metric = create_test_metric("count_test").await;
        let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

        EventRepository::save(
            &storage,
            &MetricEvent::new(
                metric_id,
                "count_test".to_string(),
                ConnectionId::from("default"),
                "{}".to_string(),
            ),
        )
        .await
        .unwrap();
        EventRepository::save(
            &storage,
            &MetricEvent::new(
                metric_id,
                "count_test".to_string(),
                ConnectionId::from("default"),
                "{}".to_string(),
            ),
        )
        .await
        .unwrap();
        EventRepository::save(
            &storage,
            &MetricEvent::new(
                metric_id,
                "count_test".to_string(),
                ConnectionId::from("default"),
                "{}".to_string(),
            ),
        )
        .await
        .unwrap();

        let count = EventRepository::count_by_metric_id(&storage, metric_id)
            .await
            .unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn test_event_cleanup() {
        let storage = create_test_storage().await;
        let metric = create_test_metric("cleanup_test").await;
        let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

        // Add 10 events
        for _ in 0..10 {
            EventRepository::save(
                &storage,
                &MetricEvent::new(
                    metric_id,
                    "cleanup_test".to_string(),
                    ConnectionId::from("default"),
                    "{}".to_string(),
                ),
            )
            .await
            .unwrap();
        }

        // Keep only 5
        let deleted = EventRepository::cleanup_metric_events(&storage, metric_id, 5)
            .await
            .unwrap();
        assert_eq!(deleted, 5);

        let count = EventRepository::count_by_metric_id(&storage, metric_id)
            .await
            .unwrap();
        assert_eq!(count, 5);
    }

    /// Test that connection_id is properly saved and retrieved for events
    /// This is critical for Issue 3.1 - multi-connection support
    #[tokio::test]
    async fn test_event_connection_id_persisted() {
        let storage = create_test_storage().await;
        let metric = create_test_metric("conn_test").await;
        let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

        // Save an event with a specific connection_id (NOT 'default')
        let custom_conn_id = ConnectionId::from("my-custom-connection");
        let event = MetricEvent::new(
            metric_id,
            "conn_test".to_string(),
            custom_conn_id.clone(),
            r#"{"test": true}"#.to_string(),
        );

        EventRepository::save(&storage, &event).await.unwrap();

        // Retrieve and verify connection_id was persisted
        let events = EventRepository::find_by_metric_id(&storage, metric_id, 10)
            .await
            .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].connection_id.0, "my-custom-connection",
            "connection_id should be persisted, not defaulting to 'default'"
        );
    }

    /// Test that batch save also persists connection_id correctly
    #[tokio::test]
    async fn test_event_batch_save_connection_id() {
        let storage = create_test_storage().await;
        let metric = create_test_metric("batch_conn_test").await;
        let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

        // Create events with different connection_ids
        let events = vec![
            MetricEvent::new(
                metric_id,
                "batch_conn_test".to_string(),
                ConnectionId::from("conn-a"),
                r#"{"idx": 1}"#.to_string(),
            ),
            MetricEvent::new(
                metric_id,
                "batch_conn_test".to_string(),
                ConnectionId::from("conn-b"),
                r#"{"idx": 2}"#.to_string(),
            ),
        ];

        EventRepository::save_batch(&storage, &events)
            .await
            .unwrap();

        // Retrieve and verify
        let saved = EventRepository::find_by_metric_id(&storage, metric_id, 10)
            .await
            .unwrap();

        assert_eq!(saved.len(), 2);
        // Events are ordered by timestamp DESC, so second event first
        let conn_ids: Vec<&str> = saved.iter().map(|e| e.connection_id.0.as_str()).collect();
        assert!(
            conn_ids.contains(&"conn-a") && conn_ids.contains(&"conn-b"),
            "Both connection_ids should be persisted"
        );
    }

    /// Test batch save with large number of events (exceeds chunk size)
    /// This tests the multi-value INSERT chunking logic
    #[tokio::test]
    async fn test_event_batch_save_large_batch() {
        let storage = create_test_storage().await;
        let metric = create_test_metric("large_batch_test").await;
        let metric_id = MetricRepository::save(&storage, &metric).await.unwrap();

        // Create 100 events (exceeds the chunk size of 62)
        let events: Vec<MetricEvent> = (0..100)
            .map(|i| {
                MetricEvent::new(
                    metric_id,
                    "large_batch_test".to_string(),
                    ConnectionId::from(format!("conn-{}", i)),
                    format!(r#"{{"idx": {}}}"#, i),
                )
            })
            .collect();

        let ids = EventRepository::save_batch(&storage, &events)
            .await
            .unwrap();

        // Verify we got 100 IDs back
        assert_eq!(ids.len(), 100, "Should return 100 IDs for 100 events");

        // Verify all events were saved
        let saved = EventRepository::find_by_metric_id(&storage, metric_id, 200)
            .await
            .unwrap();

        assert_eq!(saved.len(), 100, "All 100 events should be persisted");

        // Verify IDs are unique
        let unique_ids: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique_ids.len(), 100, "All IDs should be unique");
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// Create a test event with specified timestamp
    fn make_test_event(metric_id: MetricId, timestamp: i64) -> MetricEvent {
        MetricEvent {
            id: None,
            metric_id,
            metric_name: "test_metric".to_string(),
            connection_id: ConnectionId::from("test-conn"),
            timestamp,
            thread_name: Some("test".to_string()),
            thread_id: Some(1),
            values: vec![ExpressionValue::with_numeric("", r#"{"test": true}"#, 1.0)],
            is_error: false,
            error_type: None,
            error_message: None,
            stack_trace: None,
            memory_snapshot: None,
            request_id: None,
            session_id: None,
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// Time range query returns only events within the specified range
        #[test]
        fn proptest_time_range_returns_events_in_range(
            timestamps in proptest::collection::vec(1i64..1_000_000i64, 5..20),
            start_offset in 0usize..5,
            range_size in 100i64..500_000i64
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let storage = SqliteStorage::in_memory().await.unwrap();
                let metric_id = MetricId(1);

                // Insert events with various timestamps
                for &ts in &timestamps {
                    let event = make_test_event(metric_id, ts);
                    let _ = EventRepository::save(&storage, &event).await;
                }

                // Define time range based on sorted timestamps
                let mut sorted_ts = timestamps.clone();
                sorted_ts.sort();
                let start_idx = start_offset.min(sorted_ts.len().saturating_sub(1));
                let start_micros = sorted_ts[start_idx];
                let end_micros = start_micros + range_size;

                // Query
                let results = storage
                    .find_by_metric_id_and_time_range(metric_id, start_micros, end_micros, 1000)
                    .await
                    .unwrap();

                // Verify all returned events are within range
                for event in &results {
                    prop_assert!(
                        event.timestamp >= start_micros && event.timestamp <= end_micros,
                        "Event timestamp {} should be in range [{}, {}]",
                        event.timestamp, start_micros, end_micros
                    );
                }
                Ok::<(), proptest::test_runner::TestCaseError>(())
            })?;
        }

        /// Query limit is always respected
        #[test]
        fn proptest_limit_is_respected(
            event_count in 10usize..50,
            limit in 1i64..20i64
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let storage = SqliteStorage::in_memory().await.unwrap();
                let metric_id = MetricId(1);

                // Insert many events
                for i in 0..event_count {
                    let event = make_test_event(metric_id, 1000 + i as i64);
                    let _ = EventRepository::save(&storage, &event).await;
                }

                // Query with limit
                let results = storage
                    .find_by_metric_id(metric_id, limit)
                    .await
                    .unwrap();

                // Verify limit is respected
                prop_assert!(
                    results.len() <= limit as usize,
                    "Results count {} should be <= limit {}",
                    results.len(), limit
                );
                Ok::<(), proptest::test_runner::TestCaseError>(())
            })?;
        }

        /// Events are returned in descending timestamp order (most recent first)
        #[test]
        fn proptest_events_ordered_desc_by_timestamp(
            timestamps in proptest::collection::vec(1i64..1_000_000i64, 5..30)
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let storage = SqliteStorage::in_memory().await.unwrap();
                let metric_id = MetricId(1);

                // Insert events with various timestamps
                for &ts in &timestamps {
                    let event = make_test_event(metric_id, ts);
                    let _ = EventRepository::save(&storage, &event).await;
                }

                // Query all
                let results = storage
                    .find_by_metric_id(metric_id, 1000)
                    .await
                    .unwrap();

                // Verify descending order
                for i in 1..results.len() {
                    prop_assert!(
                        results[i-1].timestamp >= results[i].timestamp,
                        "Events should be in descending timestamp order: {} >= {}",
                        results[i-1].timestamp, results[i].timestamp
                    );
                }
                Ok::<(), proptest::test_runner::TestCaseError>(())
            })?;
        }

        /// Empty time range returns no results
        #[test]
        fn proptest_empty_range_returns_nothing(
            timestamps in proptest::collection::vec(100i64..1_000_000i64, 5..20),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let storage = SqliteStorage::in_memory().await.unwrap();
                let metric_id = MetricId(1);

                // Insert events
                for &ts in &timestamps {
                    let event = make_test_event(metric_id, ts);
                    let _ = EventRepository::save(&storage, &event).await;
                }

                // Query with range that contains no events (before all timestamps)
                let results = storage
                    .find_by_metric_id_and_time_range(metric_id, 0, 50, 1000)
                    .await
                    .unwrap();

                prop_assert!(
                    results.is_empty(),
                    "Query with range before all events should return empty"
                );
                Ok::<(), proptest::test_runner::TestCaseError>(())
            })?;
        }

        /// Query for non-existent metric returns empty
        #[test]
        fn proptest_nonexistent_metric_returns_empty(
            event_count in 1usize..10,
            query_metric_id in 1000u64..9999u64
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let storage = SqliteStorage::in_memory().await.unwrap();

                // Insert events for metric_id 1
                for i in 0..event_count {
                    let event = make_test_event(MetricId(1), 1000 + i as i64);
                    let _ = EventRepository::save(&storage, &event).await;
                }

                // Query for different metric_id
                let results = storage
                    .find_by_metric_id(MetricId(query_metric_id), 1000)
                    .await
                    .unwrap();

                prop_assert!(
                    results.is_empty(),
                    "Query for non-existent metric {} should return empty",
                    query_metric_id
                );
                Ok::<(), proptest::test_runner::TestCaseError>(())
            })?;
        }

        /// Count matches actual query results
        #[test]
        fn proptest_count_matches_query_results(
            event_count in 1usize..50,
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let storage = SqliteStorage::in_memory().await.unwrap();
                let metric_id = MetricId(1);

                // Insert events
                for i in 0..event_count {
                    let event = make_test_event(metric_id, 1000 + i as i64);
                    let _ = EventRepository::save(&storage, &event).await;
                }

                // Query count
                let count = storage
                    .count_by_metric_id(metric_id)
                    .await
                    .unwrap();

                // Query all (with high limit)
                let results = storage
                    .find_by_metric_id(metric_id, 10000)
                    .await
                    .unwrap();

                prop_assert_eq!(
                    count as usize,
                    results.len(),
                    "Count {} should match results length {}",
                    count, results.len()
                );
                Ok::<(), proptest::test_runner::TestCaseError>(())
            })?;
        }
    }
}
