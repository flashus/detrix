//! Dedicated DLQ storage - separate database from main storage
//!
//! This module provides an isolated SQLite database for the Dead-Letter Queue.
//! Keeping DLQ in a separate database ensures that if the main database fails,
//! failed events can still be captured and recovered later.

use async_trait::async_trait;
use detrix_application::ports::{DlqEntry, DlqEntryStatus, DlqRepository};
use detrix_config::constants::SQLITE_MAX_VARIABLES;
use detrix_config::{DlqBackend, DlqStorageConfig};
use detrix_core::error::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;
use tracing::{debug, info, trace, warn};

/// Dedicated storage for Dead-Letter Queue
///
/// Unlike main SqliteStorage, this operates on a separate database file
/// (or in-memory) to provide true isolation from main storage failures.
#[derive(Debug, Clone)]
pub struct DlqStorage {
    pool: SqlitePool,
}

impl DlqStorage {
    /// Create DLQ storage from a file path
    pub async fn new(path: &Path, pool_size: u32, busy_timeout_ms: u64) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(&path.to_string_lossy())?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_millis(busy_timeout_ms));

        let pool = SqlitePoolOptions::new()
            .max_connections(pool_size)
            .connect_with(options)
            .await?;

        run_dlq_migrations(&pool).await?;

        info!(path = %path.display(), "DLQ storage initialized");

        Ok(DlqStorage { pool })
    }

    /// Create in-memory DLQ storage (for testing or ephemeral use)
    pub async fn in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str(":memory:")?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        run_dlq_migrations(&pool).await?;

        debug!("In-memory DLQ storage created");

        Ok(DlqStorage { pool })
    }

    /// Factory: create DLQ storage based on configuration
    ///
    /// # Arguments
    /// * `config` - DLQ storage configuration
    /// * `config_dir` - Base directory for resolving relative paths
    pub async fn from_config(config: &DlqStorageConfig, config_dir: &Path) -> Result<Self> {
        match config.backend {
            DlqBackend::SqliteFile => {
                let path = resolve_path(config_dir, &config.path);
                Self::new(&path, config.pool_size, config.busy_timeout_ms).await
            }
            DlqBackend::SqliteMemory => Self::in_memory().await,
        }
    }

    /// Get reference to connection pool
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Resolve path relative to config directory
fn resolve_path(config_dir: &Path, path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.join(path)
    }
}

/// Run DLQ-specific migrations
async fn run_dlq_migrations(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("./dlq_migrations").run(pool).await?;

    debug!("DLQ database migrations completed");
    Ok(())
}

/// Helper to get current timestamp in microseconds
///
/// If the system clock is before UNIX_EPOCH (which should never happen in practice),
/// logs a warning and returns 0.
fn now_micros() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "System clock error: time is before UNIX_EPOCH, using timestamp 0"
            );
            0
        })
}

// =============================================================================
// DlqRepository Implementation
// =============================================================================

#[async_trait]
impl DlqRepository for DlqStorage {
    async fn save(&self, event_json: String, error_message: String) -> Result<i64> {
        let created_at = now_micros();

        let id = sqlx::query(
            r#"
            INSERT INTO dead_letter_events (event_json, error_message, created_at)
            VALUES (?, ?, ?)
            "#,
        )
        .bind(&event_json)
        .bind(&error_message)
        .bind(created_at)
        .execute(&self.pool)
        .await?
        .last_insert_rowid();

        trace!(id = id, "Saved event to dead-letter queue");
        Ok(id)
    }

    async fn save_batch(&self, events: Vec<(String, String)>) -> Result<Vec<i64>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        let created_at = now_micros();
        // SQLite variable limit (SQLITE_MAX_VARIABLE_NUMBER): 3 columns per event
        const COLUMNS_PER_EVENT: usize = 3;
        const CHUNK_SIZE: usize = SQLITE_MAX_VARIABLES / COLUMNS_PER_EVENT;

        let mut all_ids = Vec::with_capacity(events.len());
        let mut tx = self.pool.begin().await?;

        for chunk in events.chunks(CHUNK_SIZE) {
            let placeholders: Vec<String> = chunk.iter().map(|_| "(?, ?, ?)".to_string()).collect();

            let query = format!(
                r#"
                INSERT INTO dead_letter_events (event_json, error_message, created_at)
                VALUES {}
                "#,
                placeholders.join(", ")
            );

            let mut query_builder = sqlx::query(&query);

            for (event_json, error_message) in chunk {
                query_builder = query_builder
                    .bind(event_json)
                    .bind(error_message)
                    .bind(created_at);
            }

            let result = query_builder.execute(&mut *tx).await?;
            let first_id = result.last_insert_rowid() - (chunk.len() as i64 - 1);

            for i in 0..chunk.len() {
                all_ids.push(first_id + i as i64);
            }
        }

        tx.commit().await?;
        debug!(count = all_ids.len(), "Batch saved to dead-letter queue");
        Ok(all_ids)
    }

    async fn find_pending(&self, limit: usize) -> Result<Vec<DlqEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT id, event_json, error_message, retry_count, status, created_at, last_retry_at
            FROM dead_letter_events
            WHERE status = 'pending'
            ORDER BY created_at ASC
            LIMIT ?
            "#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let entries = rows
            .iter()
            .map(|row| {
                let status_str: String = row.get("status");
                DlqEntry {
                    id: row.get("id"),
                    event_json: row.get("event_json"),
                    error_message: row.get("error_message"),
                    retry_count: row.get::<i64, _>("retry_count") as u32,
                    status: DlqEntryStatus::from_str(&status_str)
                        .unwrap_or(DlqEntryStatus::Pending),
                    created_at: row.get("created_at"),
                    last_retry_at: row.get("last_retry_at"),
                }
            })
            .collect();

        Ok(entries)
    }

    async fn mark_retrying(&self, id: i64) -> Result<()> {
        let now = now_micros();

        sqlx::query(
            r#"
            UPDATE dead_letter_events
            SET status = 'retrying', last_retry_at = ?
            WHERE id = ?
            "#,
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;

        trace!(id = id, "Marked DLQ entry as retrying");
        Ok(())
    }

    async fn mark_retrying_batch(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let now = now_micros();
        let placeholders: Vec<String> = ids.iter().map(|_| "?".to_string()).collect();
        let query = format!(
            r#"
            UPDATE dead_letter_events
            SET status = 'retrying', last_retry_at = ?
            WHERE id IN ({})
            "#,
            placeholders.join(", ")
        );

        let mut query_builder = sqlx::query(&query).bind(now);
        for id in ids {
            query_builder = query_builder.bind(id);
        }

        query_builder.execute(&self.pool).await?;

        trace!(count = ids.len(), "Batch marked DLQ entries as retrying");
        Ok(())
    }

    async fn increment_retry(&self, id: i64) -> Result<()> {
        let now = now_micros();

        sqlx::query(
            r#"
            UPDATE dead_letter_events
            SET retry_count = retry_count + 1, last_retry_at = ?, status = 'pending'
            WHERE id = ?
            "#,
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;

        trace!(id = id, "Incremented DLQ entry retry count");
        Ok(())
    }

    async fn increment_retry_batch(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let now = now_micros();
        let placeholders: Vec<String> = ids.iter().map(|_| "?".to_string()).collect();
        let query = format!(
            r#"
            UPDATE dead_letter_events
            SET retry_count = retry_count + 1, last_retry_at = ?, status = 'pending'
            WHERE id IN ({})
            "#,
            placeholders.join(", ")
        );

        let mut query_builder = sqlx::query(&query).bind(now);
        for id in ids {
            query_builder = query_builder.bind(id);
        }

        query_builder.execute(&self.pool).await?;

        trace!(
            count = ids.len(),
            "Batch incremented DLQ entry retry counts"
        );
        Ok(())
    }

    async fn mark_failed(&self, id: i64) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE dead_letter_events
            SET status = 'failed'
            WHERE id = ?
            "#,
        )
        .bind(id)
        .execute(&self.pool)
        .await?;

        warn!(id = id, "Marked DLQ entry as permanently failed");
        Ok(())
    }

    async fn mark_failed_batch(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let placeholders: Vec<String> = ids.iter().map(|_| "?".to_string()).collect();
        let query = format!(
            r#"
            UPDATE dead_letter_events
            SET status = 'failed'
            WHERE id IN ({})
            "#,
            placeholders.join(", ")
        );

        let mut query_builder = sqlx::query(&query);
        for id in ids {
            query_builder = query_builder.bind(id);
        }

        query_builder.execute(&self.pool).await?;

        warn!(
            count = ids.len(),
            "Batch marked DLQ entries as permanently failed"
        );
        Ok(())
    }

    async fn delete(&self, id: i64) -> Result<()> {
        sqlx::query(
            r#"
            DELETE FROM dead_letter_events
            WHERE id = ?
            "#,
        )
        .bind(id)
        .execute(&self.pool)
        .await?;

        trace!(id = id, "Deleted DLQ entry");
        Ok(())
    }

    async fn delete_batch(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let placeholders: Vec<String> = ids.iter().map(|_| "?".to_string()).collect();
        let query = format!(
            r#"
            DELETE FROM dead_letter_events
            WHERE id IN ({})
            "#,
            placeholders.join(", ")
        );

        let mut query_builder = sqlx::query(&query);
        for id in ids {
            query_builder = query_builder.bind(id);
        }

        query_builder.execute(&self.pool).await?;

        trace!(count = ids.len(), "Batch deleted DLQ entries");
        Ok(())
    }

    async fn count_by_status(&self, status: DlqEntryStatus) -> Result<u64> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as count
            FROM dead_letter_events
            WHERE status = ?
            "#,
        )
        .bind(status.as_str())
        .fetch_one(&self.pool)
        .await?;

        let count: i64 = row.get("count");
        Ok(count as u64)
    }

    async fn count_all(&self) -> Result<u64> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as count
            FROM dead_letter_events
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        let count: i64 = row.get("count");
        Ok(count as u64)
    }

    async fn delete_old_failed(&self, older_than_micros: i64) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM dead_letter_events
            WHERE status = 'failed' AND created_at < ?
            "#,
        )
        .bind(older_than_micros)
        .execute(&self.pool)
        .await?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            debug!(deleted = deleted, "Cleaned up old failed DLQ entries");
        }
        Ok(deleted)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_config::DlqBackend;

    // =========================================================================
    // Basic CRUD Tests
    // =========================================================================

    #[tokio::test]
    async fn test_dlq_save_and_find() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // Save an event
        let event_json = r#"{"metric_id": 1, "value": 42}"#.to_string();
        let error_message = "Database busy".to_string();
        let id = storage
            .save(event_json.clone(), error_message.clone())
            .await
            .unwrap();
        assert!(id > 0);

        // Find pending
        let pending = storage.find_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].event_json, event_json);
        assert_eq!(pending[0].error_message, error_message);
        assert_eq!(pending[0].retry_count, 0);
        assert_eq!(pending[0].status, DlqEntryStatus::Pending);
    }

    #[tokio::test]
    async fn test_dlq_retry_flow() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // Save an event
        let id = storage
            .save(r#"{"test": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();

        // Mark as retrying
        storage.mark_retrying(id).await.unwrap();
        let pending = storage.find_pending(10).await.unwrap();
        assert!(pending.is_empty()); // Not pending anymore

        // Increment retry (sets back to pending)
        storage.increment_retry(id).await.unwrap();
        let pending = storage.find_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].retry_count, 1);
    }

    #[tokio::test]
    async fn test_dlq_mark_failed() {
        let storage = DlqStorage::in_memory().await.unwrap();

        let id = storage
            .save(r#"{"test": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();

        storage.mark_failed(id).await.unwrap();

        // Should not appear in pending
        let pending = storage.find_pending(10).await.unwrap();
        assert!(pending.is_empty());

        // Count by status
        let failed_count = storage
            .count_by_status(DlqEntryStatus::Failed)
            .await
            .unwrap();
        assert_eq!(failed_count, 1);
    }

    #[tokio::test]
    async fn test_dlq_delete() {
        let storage = DlqStorage::in_memory().await.unwrap();

        let id = storage
            .save(r#"{"test": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();

        storage.delete(id).await.unwrap();

        let count = storage.count_all().await.unwrap();
        assert_eq!(count, 0);
    }

    // =========================================================================
    // Batch Operation Tests
    // =========================================================================

    #[tokio::test]
    async fn test_dlq_batch_save() {
        let storage = DlqStorage::in_memory().await.unwrap();

        let events: Vec<(String, String)> = (0..10)
            .map(|i| (format!(r#"{{"id": {}}}"#, i), format!("Error {}", i)))
            .collect();

        let ids = storage.save_batch(events).await.unwrap();
        assert_eq!(ids.len(), 10);

        let count = storage.count_all().await.unwrap();
        assert_eq!(count, 10);
    }

    #[tokio::test]
    async fn test_dlq_batch_save_empty() {
        let storage = DlqStorage::in_memory().await.unwrap();

        let ids = storage.save_batch(Vec::new()).await.unwrap();
        assert!(ids.is_empty());

        let count = storage.count_all().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_dlq_batch_mark_retrying() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // Save multiple events
        let events: Vec<(String, String)> = (0..5)
            .map(|i| (format!(r#"{{"id": {}}}"#, i), "Error".to_string()))
            .collect();
        let ids = storage.save_batch(events).await.unwrap();

        // Mark all as retrying
        storage.mark_retrying_batch(&ids).await.unwrap();

        // None should be pending
        let pending = storage.find_pending(10).await.unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn test_dlq_batch_increment_retry() {
        let storage = DlqStorage::in_memory().await.unwrap();

        let events: Vec<(String, String)> = (0..3)
            .map(|i| (format!(r#"{{"id": {}}}"#, i), "Error".to_string()))
            .collect();
        let ids = storage.save_batch(events).await.unwrap();

        // Mark as retrying first
        storage.mark_retrying_batch(&ids).await.unwrap();

        // Increment retry (should be pending again with count=1)
        storage.increment_retry_batch(&ids).await.unwrap();

        let pending = storage.find_pending(10).await.unwrap();
        assert_eq!(pending.len(), 3);
        for entry in pending {
            assert_eq!(entry.retry_count, 1);
            assert_eq!(entry.status, DlqEntryStatus::Pending);
        }
    }

    #[tokio::test]
    async fn test_dlq_batch_mark_failed() {
        let storage = DlqStorage::in_memory().await.unwrap();

        let events: Vec<(String, String)> = (0..4)
            .map(|i| (format!(r#"{{"id": {}}}"#, i), "Error".to_string()))
            .collect();
        let ids = storage.save_batch(events).await.unwrap();

        storage.mark_failed_batch(&ids).await.unwrap();

        let failed_count = storage
            .count_by_status(DlqEntryStatus::Failed)
            .await
            .unwrap();
        assert_eq!(failed_count, 4);

        let pending = storage.find_pending(10).await.unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn test_dlq_batch_delete() {
        let storage = DlqStorage::in_memory().await.unwrap();

        let events: Vec<(String, String)> = (0..5)
            .map(|i| (format!(r#"{{"id": {}}}"#, i), "Error".to_string()))
            .collect();
        let ids = storage.save_batch(events).await.unwrap();

        storage.delete_batch(&ids[0..3]).await.unwrap();

        let count = storage.count_all().await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_dlq_batch_operations_empty() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // All batch operations should handle empty arrays gracefully
        storage.mark_retrying_batch(&[]).await.unwrap();
        storage.increment_retry_batch(&[]).await.unwrap();
        storage.mark_failed_batch(&[]).await.unwrap();
        storage.delete_batch(&[]).await.unwrap();
    }

    // =========================================================================
    // Count and Cleanup Tests
    // =========================================================================

    #[tokio::test]
    async fn test_dlq_count_by_status() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // Create entries in different states
        let id1 = storage
            .save(r#"{"a": 1}"#.to_string(), "e1".to_string())
            .await
            .unwrap();
        let id2 = storage
            .save(r#"{"a": 2}"#.to_string(), "e2".to_string())
            .await
            .unwrap();
        let _id3 = storage
            .save(r#"{"a": 3}"#.to_string(), "e3".to_string())
            .await
            .unwrap();

        storage.mark_failed(id1).await.unwrap();
        storage.mark_retrying(id2).await.unwrap();
        // id3 remains pending

        assert_eq!(
            storage
                .count_by_status(DlqEntryStatus::Failed)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .count_by_status(DlqEntryStatus::Retrying)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .count_by_status(DlqEntryStatus::Pending)
                .await
                .unwrap(),
            1
        );
        assert_eq!(storage.count_all().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn test_dlq_delete_old_failed() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // Save and mark as failed
        let id = storage
            .save(r#"{"old": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();
        storage.mark_failed(id).await.unwrap();

        // Delete entries older than far future (should delete everything)
        let future_micros = now_micros() + 1_000_000_000;
        let deleted = storage.delete_old_failed(future_micros).await.unwrap();
        assert_eq!(deleted, 1);

        assert_eq!(storage.count_all().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_dlq_delete_old_failed_preserves_pending() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // Save two events: one pending, one failed
        let id1 = storage
            .save(r#"{"pending": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();
        let id2 = storage
            .save(r#"{"failed": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();
        storage.mark_failed(id2).await.unwrap();

        // Delete old failed entries
        let future_micros = now_micros() + 1_000_000_000;
        let deleted = storage.delete_old_failed(future_micros).await.unwrap();
        assert_eq!(deleted, 1);

        // Pending entry should remain
        let pending = storage.find_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id1);
    }

    // =========================================================================
    // Factory Method Tests
    // =========================================================================

    #[tokio::test]
    async fn test_dlq_from_config_in_memory() {
        let config = DlqStorageConfig {
            backend: DlqBackend::SqliteMemory,
            path: std::path::PathBuf::from("unused"),
            pool_size: 5,
            busy_timeout_ms: 3000,
        };

        let storage = DlqStorage::from_config(&config, std::path::Path::new("/tmp"))
            .await
            .unwrap();

        // Verify it works
        let count = storage.count_all().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_dlq_from_config_sqlite_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test-dlq.db");

        let config = DlqStorageConfig {
            backend: DlqBackend::SqliteFile,
            path: db_path.clone(),
            pool_size: 5,
            busy_timeout_ms: 3000,
        };

        let storage = DlqStorage::from_config(&config, temp_dir.path())
            .await
            .unwrap();

        // Verify it works
        let id = storage
            .save(r#"{"test": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();
        assert!(id > 0);

        // Verify file was created
        assert!(db_path.exists());
    }

    #[tokio::test]
    async fn test_dlq_from_config_relative_path() {
        let temp_dir = tempfile::tempdir().unwrap();

        let config = DlqStorageConfig {
            backend: DlqBackend::SqliteFile,
            path: std::path::PathBuf::from("./data/dlq.db"),
            pool_size: 5,
            busy_timeout_ms: 3000,
        };

        // Create the data directory
        std::fs::create_dir_all(temp_dir.path().join("data")).unwrap();

        let storage = DlqStorage::from_config(&config, temp_dir.path())
            .await
            .unwrap();

        // Verify it works
        let count = storage.count_all().await.unwrap();
        assert_eq!(count, 0);

        // Verify file was created at the right location
        assert!(temp_dir.path().join("data/dlq.db").exists());
    }

    // =========================================================================
    // Edge Cases
    // =========================================================================

    #[tokio::test]
    async fn test_dlq_find_pending_limit() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // Save 10 events
        for i in 0..10 {
            storage
                .save(format!(r#"{{"id": {}}}"#, i), "Error".to_string())
                .await
                .unwrap();
        }

        // Find with limit
        let pending = storage.find_pending(5).await.unwrap();
        assert_eq!(pending.len(), 5);
    }

    #[tokio::test]
    async fn test_dlq_find_pending_orders_by_created_at() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // Save events with small delays to ensure ordering
        let id1 = storage
            .save(r#"{"first": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();
        let id2 = storage
            .save(r#"{"second": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();
        let id3 = storage
            .save(r#"{"third": true}"#.to_string(), "Error".to_string())
            .await
            .unwrap();

        let pending = storage.find_pending(10).await.unwrap();
        assert_eq!(pending.len(), 3);
        // Should be ordered by created_at ASC
        assert_eq!(pending[0].id, id1);
        assert_eq!(pending[1].id, id2);
        assert_eq!(pending[2].id, id3);
    }

    #[tokio::test]
    async fn test_dlq_large_batch() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // Create a batch larger than SQLite variable limit (999/3 = 333)
        let events: Vec<(String, String)> = (0..500)
            .map(|i| (format!(r#"{{"id": {}}}"#, i), format!("Error {}", i)))
            .collect();

        let ids = storage.save_batch(events).await.unwrap();
        assert_eq!(ids.len(), 500);

        let count = storage.count_all().await.unwrap();
        assert_eq!(count, 500);
    }

    #[tokio::test]
    async fn test_dlq_full_lifecycle() {
        let storage = DlqStorage::in_memory().await.unwrap();

        // 1. Save event
        let id = storage
            .save(
                r#"{"metric_id": 1, "value": "test"}"#.to_string(),
                "Connection timeout".to_string(),
            )
            .await
            .unwrap();

        // 2. Find pending
        let pending = storage.find_pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].retry_count, 0);

        // 3. Mark as retrying
        storage.mark_retrying(id).await.unwrap();
        assert!(storage.find_pending(10).await.unwrap().is_empty());

        // 4. Retry failed - increment retry count
        storage.increment_retry(id).await.unwrap();
        let pending = storage.find_pending(10).await.unwrap();
        assert_eq!(pending[0].retry_count, 1);

        // 5. Another retry attempt
        storage.mark_retrying(id).await.unwrap();
        storage.increment_retry(id).await.unwrap();
        let pending = storage.find_pending(10).await.unwrap();
        assert_eq!(pending[0].retry_count, 2);

        // 6. Third retry - mark as permanently failed
        storage.mark_retrying(id).await.unwrap();
        storage.mark_failed(id).await.unwrap();

        // 7. Verify counts
        assert_eq!(
            storage
                .count_by_status(DlqEntryStatus::Failed)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .count_by_status(DlqEntryStatus::Pending)
                .await
                .unwrap(),
            0
        );
    }
}
