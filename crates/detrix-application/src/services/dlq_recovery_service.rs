//! Dead-Letter Queue Recovery Service
//!
//! Background service that retries failed events from the dead-letter queue.
//! Events that exceed max retries are marked as permanently failed.

use crate::ports::{DlqEntryStatus, DlqRepositoryRef, EventRepositoryRef};
use detrix_config::DlqConfig;
use detrix_core::error::Result;
use detrix_core::MetricEvent;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, trace, warn};

/// Dead-letter queue recovery service
///
/// Periodically processes pending DLQ entries and attempts to retry them.
/// Uses configurable retry intervals, max retries, and batch sizes.
pub struct DlqRecoveryService {
    dlq_repo: DlqRepositoryRef,
    event_repo: EventRepositoryRef,
    config: DlqConfig,
}

impl DlqRecoveryService {
    /// Create a new DLQ recovery service
    pub fn new(
        dlq_repo: DlqRepositoryRef,
        event_repo: EventRepositoryRef,
        config: DlqConfig,
    ) -> Self {
        Self {
            dlq_repo,
            event_repo,
            config,
        }
    }

    /// Spawn the background recovery task
    ///
    /// Returns a JoinHandle that can be used to cancel the task.
    /// The task runs indefinitely until cancelled.
    pub fn spawn_recovery_task(self: Arc<Self>) -> JoinHandle<()> {
        let retry_interval = Duration::from_millis(self.config.retry_interval_ms);

        tokio::spawn(async move {
            info!(
                retry_interval_ms = self.config.retry_interval_ms,
                max_retries = self.config.max_retries,
                batch_size = self.config.batch_size,
                "DLQ recovery service started"
            );

            let mut interval = tokio::time::interval(retry_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                if let Err(e) = self.process_pending_entries().await {
                    error!(error = %e, "DLQ recovery error");
                }
            }
        })
    }

    /// Process pending DLQ entries
    ///
    /// Fetches pending entries, retries them, and updates their status.
    /// Uses batch operations to minimize database queries (N+1 fix).
    ///
    /// This is the core recovery logic that runs on each tick of the background task.
    /// Made public for integration testing.
    pub async fn process_pending_entries(&self) -> Result<()> {
        let pending = self.dlq_repo.find_pending(self.config.batch_size).await?;

        if pending.is_empty() {
            trace!("No pending DLQ entries to process");
            return Ok(());
        }

        debug!(count = pending.len(), "Processing pending DLQ entries");

        // Phase 1: Separate entries that exceeded max retries from those to retry
        let (exceeded_max_retries, to_retry): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|e| e.retry_count >= self.config.max_retries);

        // Phase 2: Batch mark entries that exceeded max retries as failed
        let failed_ids: Vec<i64> = exceeded_max_retries.iter().map(|e| e.id).collect();
        if !failed_ids.is_empty() {
            self.dlq_repo.mark_failed_batch(&failed_ids).await?;
            for entry in &exceeded_max_retries {
                warn!(
                    dlq_id = entry.id,
                    retry_count = entry.retry_count,
                    max_retries = self.config.max_retries,
                    "DLQ entry marked as permanently failed after max retries"
                );
            }
        }

        // Phase 3: Batch mark remaining entries as retrying
        let retry_ids: Vec<i64> = to_retry.iter().map(|e| e.id).collect();
        if !retry_ids.is_empty() {
            self.dlq_repo.mark_retrying_batch(&retry_ids).await?;
        }

        // Phase 4: Attempt to recover each entry (must be sequential for proper error handling)
        let mut recovered_ids = Vec::new();
        let mut increment_ids = Vec::new();

        for entry in &to_retry {
            match self.retry_entry(entry).await {
                Ok(()) => {
                    info!(dlq_id = entry.id, "DLQ entry recovered successfully");
                    recovered_ids.push(entry.id);
                }
                Err(e) => {
                    debug!(
                        dlq_id = entry.id,
                        error = %e,
                        retry_count = entry.retry_count + 1,
                        "DLQ retry failed"
                    );
                    increment_ids.push(entry.id);
                }
            }
        }

        // Phase 5: Batch delete recovered entries
        if !recovered_ids.is_empty() {
            self.dlq_repo.delete_batch(&recovered_ids).await?;
        }

        // Phase 6: Batch increment retry count for failed entries
        if !increment_ids.is_empty() {
            self.dlq_repo.increment_retry_batch(&increment_ids).await?;
        }

        let failed = failed_ids.len();
        let recovered = recovered_ids.len();
        let retried = increment_ids.len();

        if recovered > 0 || failed > 0 || retried > 0 {
            info!(
                recovered = recovered,
                failed = failed,
                retried = retried,
                "DLQ processing complete"
            );
        }

        Ok(())
    }

    /// Retry a single DLQ entry
    async fn retry_entry(&self, entry: &crate::ports::DlqEntry) -> Result<()> {
        // Deserialize the event from JSON
        let event: MetricEvent = serde_json::from_str(&entry.event_json)?;

        // Attempt to save to event repository
        self.event_repo.save(&event).await?;

        Ok(())
    }

    /// Get DLQ statistics
    pub async fn get_stats(&self) -> Result<DlqStats> {
        let pending = self
            .dlq_repo
            .count_by_status(DlqEntryStatus::Pending)
            .await?;
        let retrying = self
            .dlq_repo
            .count_by_status(DlqEntryStatus::Retrying)
            .await?;
        let failed = self
            .dlq_repo
            .count_by_status(DlqEntryStatus::Failed)
            .await?;
        let total = self.dlq_repo.count_all().await?;

        Ok(DlqStats {
            pending,
            retrying,
            failed,
            total,
        })
    }

    /// Cleanup old failed entries
    ///
    /// Removes entries older than the specified duration that are marked as failed.
    /// This prevents the DLQ from growing indefinitely.
    pub async fn cleanup_old_failed(&self, older_than_hours: u32) -> Result<u64> {
        let cutoff_micros = {
            use std::time::{SystemTime, UNIX_EPOCH};
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_micros() as i64)
                .unwrap_or(0);
            now - (older_than_hours as i64 * 3600 * 1_000_000)
        };

        let deleted = self.dlq_repo.delete_old_failed(cutoff_micros).await?;

        if deleted > 0 {
            info!(
                deleted = deleted,
                older_than_hours = older_than_hours,
                "Cleaned up old failed DLQ entries"
            );
        }

        Ok(deleted)
    }
}

/// DLQ statistics
#[derive(Debug, Clone, Default)]
pub struct DlqStats {
    /// Number of entries pending retry
    pub pending: u64,
    /// Number of entries currently being retried
    pub retrying: u64,
    /// Number of permanently failed entries
    pub failed: u64,
    /// Total entries in DLQ
    pub total: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{DlqEntry, DlqEntryStatus, DlqRepository}; // For trait methods + types
    use detrix_testing::fixtures::sample_event;
    use detrix_testing::{MockDlqRepository, MockEventRepository};
    use std::sync::Arc;

    fn sample_event_json() -> String {
        let event = sample_event(1, 42.0);
        serde_json::to_string(&event).unwrap()
    }

    #[tokio::test]
    async fn test_dlq_recovery_successful() {
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let config = DlqConfig::default();

        // Add a pending entry with valid event JSON
        dlq_repo
            .add_entry(sample_event_json(), "Test error".to_string())
            .await;

        let service = Arc::new(DlqRecoveryService::new(
            dlq_repo.clone(),
            event_repo.clone(),
            config,
        ));

        // Process should recover the entry
        service.process_pending_entries().await.unwrap();

        // Entry should be deleted (recovered)
        let count = dlq_repo.count_all().await.unwrap();
        assert_eq!(count, 0);

        // Event should be saved
        assert_eq!(event_repo.count(), 1);
    }

    #[tokio::test]
    async fn test_dlq_max_retries_exceeded() {
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let config = DlqConfig {
            max_retries: 2,
            ..DlqConfig::default()
        };

        // Add an entry that's already at max retries
        dlq_repo
            .push_entry(DlqEntry {
                id: 1,
                event_json: "invalid json".to_string(), // Will fail to parse
                error_message: "Test".to_string(),
                retry_count: 2, // Already at max
                status: DlqEntryStatus::Pending,
                created_at: 0,
                last_retry_at: None,
            })
            .await;

        let service = Arc::new(DlqRecoveryService::new(
            dlq_repo.clone(),
            event_repo.clone(),
            config,
        ));

        // Process should mark as failed
        service.process_pending_entries().await.unwrap();

        // Entry should be marked as failed
        let failed_count = dlq_repo
            .count_by_status(DlqEntryStatus::Failed)
            .await
            .unwrap();
        assert_eq!(failed_count, 1);
    }

    #[tokio::test]
    async fn test_dlq_stats() {
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let config = DlqConfig::default();

        // Add entries with different statuses
        dlq_repo
            .add_entry(sample_event_json(), "Error 1".to_string())
            .await;
        dlq_repo
            .add_entry(sample_event_json(), "Error 2".to_string())
            .await;
        dlq_repo.mark_failed(1).await.unwrap();

        let service = DlqRecoveryService::new(dlq_repo, event_repo, config);
        let stats = service.get_stats().await.unwrap();

        assert_eq!(stats.total, 2);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.failed, 1);
    }

    #[tokio::test]
    async fn test_dlq_empty_queue() {
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let config = DlqConfig::default();

        let service = Arc::new(DlqRecoveryService::new(
            dlq_repo.clone(),
            event_repo.clone(),
            config,
        ));

        // Process should complete without errors on empty queue
        let result = service.process_pending_entries().await;
        assert!(result.is_ok());

        // Nothing should be saved
        assert_eq!(event_repo.count(), 0);
    }

    #[tokio::test]
    async fn test_dlq_cleanup_old_failed() {
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let config = DlqConfig::default();

        // Add entries and mark some as failed
        dlq_repo
            .add_entry(sample_event_json(), "Error 1".to_string())
            .await;
        dlq_repo
            .add_entry(sample_event_json(), "Error 2".to_string())
            .await;
        dlq_repo.mark_failed(1).await.unwrap();
        dlq_repo.mark_failed(2).await.unwrap();

        // Add a pending entry (shouldn't be deleted)
        dlq_repo
            .add_entry(sample_event_json(), "Error 3".to_string())
            .await;

        let service = DlqRecoveryService::new(dlq_repo.clone(), event_repo, config);

        // Cleanup should delete failed entries
        let deleted = service.cleanup_old_failed(0).await.unwrap();

        assert_eq!(deleted, 2);
        assert_eq!(dlq_repo.count_all().await.unwrap(), 1);
        assert_eq!(
            dlq_repo
                .count_by_status(DlqEntryStatus::Pending)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn test_dlq_batch_recovery() {
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let config = DlqConfig {
            batch_size: 10,
            ..DlqConfig::default()
        };

        // Add multiple valid entries
        for _ in 0..5 {
            dlq_repo
                .add_entry(sample_event_json(), "Test error".to_string())
                .await;
        }

        let service = Arc::new(DlqRecoveryService::new(
            dlq_repo.clone(),
            event_repo.clone(),
            config,
        ));

        // Process all entries
        service.process_pending_entries().await.unwrap();

        // All should be recovered
        assert_eq!(dlq_repo.count_all().await.unwrap(), 0);
        assert_eq!(event_repo.count(), 5);
    }

    #[tokio::test]
    async fn test_dlq_partial_batch_success() {
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let config = DlqConfig {
            batch_size: 10,
            ..DlqConfig::default()
        };

        // Add valid entries
        dlq_repo
            .add_entry(sample_event_json(), "Error 1".to_string())
            .await;
        dlq_repo
            .add_entry(sample_event_json(), "Error 2".to_string())
            .await;

        // Add invalid entry (will fail to parse)
        dlq_repo
            .push_entry(DlqEntry {
                id: 3,
                event_json: "invalid json".to_string(),
                error_message: "Test".to_string(),
                retry_count: 0,
                status: DlqEntryStatus::Pending,
                created_at: 0,
                last_retry_at: None,
            })
            .await;

        let service = Arc::new(DlqRecoveryService::new(
            dlq_repo.clone(),
            event_repo.clone(),
            config,
        ));

        // Process should partially succeed
        service.process_pending_entries().await.unwrap();

        // 2 should be recovered, 1 should remain with incremented retry
        assert_eq!(event_repo.count(), 2);

        let entries = dlq_repo.all_entries().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].retry_count, 1); // Retry count incremented
    }

    #[tokio::test]
    async fn test_dlq_retry_increment() {
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let config = DlqConfig {
            max_retries: 5,
            ..DlqConfig::default()
        };

        // Add invalid entry
        dlq_repo
            .push_entry(DlqEntry {
                id: 1,
                event_json: "invalid".to_string(),
                error_message: "Test".to_string(),
                retry_count: 0,
                status: DlqEntryStatus::Pending,
                created_at: 0,
                last_retry_at: None,
            })
            .await;

        let service = Arc::new(DlqRecoveryService::new(
            dlq_repo.clone(),
            event_repo.clone(),
            config,
        ));

        // Process multiple times - retry count should increment each time
        for expected_count in 1..=3 {
            service.process_pending_entries().await.unwrap();
            let entry = dlq_repo.get_entry(1).await.unwrap();
            assert_eq!(entry.retry_count, expected_count);
        }
    }

    #[tokio::test]
    async fn test_dlq_respects_batch_size() {
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let event_repo = Arc::new(MockEventRepository::new());
        let config = DlqConfig {
            batch_size: 2, // Only process 2 at a time
            ..DlqConfig::default()
        };

        // Add 5 valid entries
        for _ in 0..5 {
            dlq_repo
                .add_entry(sample_event_json(), "Test error".to_string())
                .await;
        }

        let service = Arc::new(DlqRecoveryService::new(
            dlq_repo.clone(),
            event_repo.clone(),
            config,
        ));

        // First process - should only handle 2
        service.process_pending_entries().await.unwrap();
        assert_eq!(dlq_repo.count_all().await.unwrap(), 3);
        assert_eq!(event_repo.count(), 2);

        // Second process - another 2
        service.process_pending_entries().await.unwrap();
        assert_eq!(dlq_repo.count_all().await.unwrap(), 1);
        assert_eq!(event_repo.count(), 4);

        // Third process - final 1
        service.process_pending_entries().await.unwrap();
        assert_eq!(dlq_repo.count_all().await.unwrap(), 0);
        assert_eq!(event_repo.count(), 5);
    }
}
