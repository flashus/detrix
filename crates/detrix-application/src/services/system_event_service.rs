//! System event persistence and query use cases (protocol-agnostic)
//!
//! Handles capturing, persisting, and querying system events.
//! System events include:
//! - Metric CRUD events (added, removed, toggled)
//! - Connection events (created, closed, lost, restored)
//! - Debugger crash events
//!
//! These events are used for:
//! - MCP client catch-up queries (reconnecting agents)
//! - Audit trail for critical events
//! - Debugging and observability

use crate::ports::SystemEventRepositoryRef;
use crate::Result;
use chrono::Utc;
use detrix_core::system_event::{SystemEvent, SystemEventType};
use detrix_core::ConnectionId;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// System event service (protocol-agnostic)
///
/// Provides methods for:
/// - Persisting system events to storage
/// - Querying events for MCP catch-up
/// - Acknowledging events after delivery
/// - Cleanup of old events
#[derive(Clone)]
pub struct SystemEventService {
    storage: SystemEventRepositoryRef,
}

impl std::fmt::Debug for SystemEventService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemEventService")
            .field("storage", &"<SystemEventRepository>")
            .finish()
    }
}

impl SystemEventService {
    /// Create new system event service
    pub fn new(storage: SystemEventRepositoryRef) -> Self {
        Self { storage }
    }

    // =========================================================================
    // Write Operations
    // =========================================================================

    /// Capture and store a system event
    ///
    /// This is the single entry point for saving system events from services.
    pub async fn capture_event(&self, event: &SystemEvent) -> Result<i64> {
        let event_id = self.storage.save(event).await?;
        Ok(event_id)
    }

    /// Capture multiple events in batch
    ///
    /// Uses transaction batching for efficiency - all events are saved
    /// in a single database transaction.
    pub async fn capture_events(&self, events: &[SystemEvent]) -> Result<Vec<i64>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.storage.save_batch(events).await?)
    }

    // =========================================================================
    // Query Operations (for MCP catch-up)
    // =========================================================================

    /// Find events since a timestamp (for MCP client catch-up)
    ///
    /// Returns events in chronological order (oldest first).
    pub async fn find_since(&self, timestamp_micros: i64, limit: i64) -> Result<Vec<SystemEvent>> {
        Ok(self.storage.find_since(timestamp_micros, limit).await?)
    }

    /// Find events by type
    pub async fn find_by_type(
        &self,
        event_type: SystemEventType,
        limit: i64,
    ) -> Result<Vec<SystemEvent>> {
        Ok(self.storage.find_by_type(event_type, limit).await?)
    }

    /// Find events for a specific connection
    pub async fn find_by_connection(
        &self,
        connection_id: &ConnectionId,
        limit: i64,
    ) -> Result<Vec<SystemEvent>> {
        Ok(self
            .storage
            .find_by_connection(connection_id, limit)
            .await?)
    }

    /// Find unacknowledged events (for MCP clients reconnecting)
    ///
    /// Returns events that haven't been acknowledged yet, which is useful
    /// for clients that disconnected and need to catch up.
    pub async fn find_unacknowledged(&self, limit: i64) -> Result<Vec<SystemEvent>> {
        Ok(self.storage.find_unacknowledged(limit).await?)
    }

    /// Find recent events (most recent first)
    pub async fn find_recent(&self, limit: i64) -> Result<Vec<SystemEvent>> {
        Ok(self.storage.find_recent(limit).await?)
    }

    // =========================================================================
    // Acknowledgment Operations
    // =========================================================================

    /// Mark events as acknowledged
    ///
    /// Called after MCP client confirms receipt of events.
    /// Returns the number of events acknowledged.
    pub async fn acknowledge(&self, event_ids: &[i64]) -> Result<u64> {
        if event_ids.is_empty() {
            return Ok(0);
        }
        Ok(self.storage.acknowledge(event_ids).await?)
    }

    /// Count unacknowledged events
    pub async fn count_unacknowledged(&self) -> Result<i64> {
        Ok(self.storage.count_unacknowledged().await?)
    }

    // =========================================================================
    // Cleanup Operations
    // =========================================================================

    /// Delete events older than timestamp
    ///
    /// Used for retention cleanup. Returns the number of events deleted.
    pub async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64> {
        Ok(self.storage.delete_older_than(timestamp_micros).await?)
    }

    /// Count all events
    pub async fn count_all(&self) -> Result<i64> {
        Ok(self.storage.count_all().await?)
    }

    // =========================================================================
    // Retention Cleanup Task
    // =========================================================================

    /// Run a single retention cleanup pass (time-based)
    ///
    /// Deletes events older than `retention_hours` hours.
    /// Returns the number of events deleted.
    pub async fn run_time_retention_cleanup(&self, retention_hours: u32) -> Result<u64> {
        let cutoff = Utc::now().timestamp_micros() - (retention_hours as i64 * 3600 * 1_000_000);

        let deleted = self.delete_older_than(cutoff).await?;

        if deleted > 0 {
            tracing::info!(
                deleted = deleted,
                retention_hours = retention_hours,
                "System event time-based retention cleanup completed"
            );
        } else {
            tracing::debug!(
                retention_hours = retention_hours,
                "System event time-based retention cleanup: no events to delete"
            );
        }

        Ok(deleted)
    }

    /// Run a single count-based retention cleanup pass
    ///
    /// Keeps only the most recent `max_events` events.
    /// If max_events is 0, count-based retention is disabled.
    /// Returns the number of events deleted.
    pub async fn run_count_retention_cleanup(&self, max_events: usize) -> Result<u64> {
        if max_events == 0 {
            return Ok(0);
        }

        let deleted = self.storage.delete_keeping_recent(max_events).await?;

        if deleted > 0 {
            tracing::info!(
                deleted = deleted,
                max_events = max_events,
                "System event count-based retention cleanup completed"
            );
        } else {
            tracing::debug!(
                max_events = max_events,
                "System event count-based retention cleanup: no events to delete"
            );
        }

        Ok(deleted)
    }

    /// Start background retention cleanup task
    ///
    /// Periodically deletes events based on both time and count limits.
    /// The task runs every `cleanup_interval_secs` seconds.
    ///
    /// Returns a JoinHandle for the background task.
    ///
    /// # Arguments
    ///
    /// * `retention_hours` - Delete events older than this many hours
    /// * `cleanup_interval_secs` - How often to run cleanup (in seconds)
    /// * `max_events` - Maximum events to keep (0 = disabled)
    /// * `shutdown_rx` - Shutdown signal receiver
    pub fn start_retention_cleanup_task(
        &self,
        retention_hours: u32,
        cleanup_interval_secs: u64,
        max_events: usize,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let service = self.clone();
        let interval = Duration::from_secs(cleanup_interval_secs);

        tokio::spawn(async move {
            tracing::info!(
                retention_hours = retention_hours,
                cleanup_interval_secs = cleanup_interval_secs,
                max_events = max_events,
                "Starting system event retention cleanup task"
            );

            // Run initial cleanup (both time-based and count-based)
            if let Err(e) = service.run_time_retention_cleanup(retention_hours).await {
                tracing::error!(error = %e, "Initial time-based retention cleanup failed");
            }
            if let Err(e) = service.run_count_retention_cleanup(max_events).await {
                tracing::error!(error = %e, "Initial count-based retention cleanup failed");
            }

            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        // Run time-based cleanup first
                        if let Err(e) = service.run_time_retention_cleanup(retention_hours).await {
                            tracing::error!(error = %e, "Time-based retention cleanup failed");
                        }
                        // Then run count-based cleanup
                        if let Err(e) = service.run_count_retention_cleanup(max_events).await {
                            tracing::error!(error = %e, "Count-based retention cleanup failed");
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            tracing::info!("System event retention cleanup task shutting down");
                            break;
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_testing::fixtures::sample_system_event;
    use detrix_testing::MockSystemEventRepository;
    use std::sync::Arc;

    async fn create_test_service() -> (SystemEventService, Arc<MockSystemEventRepository>) {
        let repository = Arc::new(MockSystemEventRepository::new());
        let service = SystemEventService::new(Arc::clone(&repository) as SystemEventRepositoryRef);
        (service, repository)
    }

    #[tokio::test]
    async fn test_capture_event() {
        let (service, repository) = create_test_service().await;

        let event = sample_system_event(SystemEventType::MetricAdded);
        let event_id = service.capture_event(&event).await.unwrap();

        assert!(event_id > 0);
        assert_eq!(repository.count(), 1);
    }

    #[tokio::test]
    async fn test_capture_multiple_events() {
        let (service, repository) = create_test_service().await;

        let events = vec![
            sample_system_event(SystemEventType::MetricAdded),
            sample_system_event(SystemEventType::MetricRemoved),
            sample_system_event(SystemEventType::ConnectionCreated),
        ];

        let event_ids = service.capture_events(&events).await.unwrap();

        assert_eq!(event_ids.len(), 3);
        assert!(event_ids.iter().all(|&id| id > 0));
        assert_eq!(repository.count(), 3);
    }

    #[tokio::test]
    async fn test_find_unacknowledged() {
        let (service, _repository) = create_test_service().await;

        // Capture some events
        let event = sample_system_event(SystemEventType::MetricAdded);
        service.capture_event(&event).await.unwrap();

        // All should be unacknowledged initially
        let unacked = service.find_unacknowledged(100).await.unwrap();
        assert_eq!(unacked.len(), 1);
    }

    #[tokio::test]
    async fn test_acknowledge_events() {
        let (service, _repository) = create_test_service().await;

        // Capture some events
        let event = sample_system_event(SystemEventType::MetricAdded);
        let event_id = service.capture_event(&event).await.unwrap();

        // Acknowledge
        let acked = service.acknowledge(&[event_id]).await.unwrap();
        assert_eq!(acked, 1);

        // Should be no unacknowledged events now
        let unacked = service.find_unacknowledged(100).await.unwrap();
        assert!(unacked.is_empty());
    }

    #[tokio::test]
    async fn test_find_since() {
        let (service, _repository) = create_test_service().await;

        let event = sample_system_event(SystemEventType::MetricAdded);
        let timestamp_before = event.timestamp - 1000;
        service.capture_event(&event).await.unwrap();

        // Query for events since before the event
        let events = service.find_since(timestamp_before, 100).await.unwrap();
        assert_eq!(events.len(), 1);

        // Query for events since after the event
        let timestamp_after = event.timestamp + 1000;
        let events = service.find_since(timestamp_after, 100).await.unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn test_count_retention_cleanup_deletes_oldest() {
        let (service, repository) = create_test_service().await;

        // Create 5 events with different timestamps
        let base_timestamp = 1000000i64;
        for i in 0..5 {
            let mut event = sample_system_event(SystemEventType::MetricAdded);
            event.timestamp = base_timestamp + (i * 1000);
            service.capture_event(&event).await.unwrap();
        }

        assert_eq!(repository.count(), 5);

        // Keep only 3 most recent
        let deleted = service.run_count_retention_cleanup(3).await.unwrap();

        assert_eq!(deleted, 2);
        assert_eq!(repository.count(), 3);

        // Verify the remaining events are the most recent ones
        let remaining = service.find_recent(10).await.unwrap();
        assert_eq!(remaining.len(), 3);
        // Most recent should have the highest timestamps
        assert!(remaining[0].timestamp > remaining[1].timestamp);
        assert!(remaining[1].timestamp > remaining[2].timestamp);
    }

    #[tokio::test]
    async fn test_count_retention_cleanup_zero_disables() {
        let (service, repository) = create_test_service().await;

        // Create some events
        for _ in 0..5 {
            let event = sample_system_event(SystemEventType::MetricAdded);
            service.capture_event(&event).await.unwrap();
        }

        assert_eq!(repository.count(), 5);

        // max_events = 0 should disable count-based retention
        let deleted = service.run_count_retention_cleanup(0).await.unwrap();

        assert_eq!(deleted, 0);
        assert_eq!(repository.count(), 5);
    }

    #[tokio::test]
    async fn test_count_retention_cleanup_no_op_when_under_limit() {
        let (service, repository) = create_test_service().await;

        // Create 3 events
        for _ in 0..3 {
            let event = sample_system_event(SystemEventType::MetricAdded);
            service.capture_event(&event).await.unwrap();
        }

        assert_eq!(repository.count(), 3);

        // max_events = 10 (more than current count)
        let deleted = service.run_count_retention_cleanup(10).await.unwrap();

        assert_eq!(deleted, 0);
        assert_eq!(repository.count(), 3);
    }
}
