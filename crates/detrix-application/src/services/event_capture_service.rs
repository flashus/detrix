//! Event capture use cases (protocol-agnostic)
//!
//! Handles capturing and storing metric events from adapters.
//! This service is responsible for the write path of events.
//!
//! ## Dead-Letter Queue
//!
//! When event storage fails, events can be saved to a dead-letter queue
//! for later retry. This prevents permanent data loss during transient
//! database failures.

use crate::ports::{DlqRepositoryRef, EventRepositoryRef};
use crate::Result;
use detrix_core::MetricEvent;
use tracing::error;

/// Event capture service (protocol-agnostic)
///
/// Responsible for capturing and storing metric events.
/// Separate from StreamingService which handles the read path.
///
/// Optionally supports dead-letter queue for failed events.
#[derive(Clone)]
pub struct EventCaptureService {
    storage: EventRepositoryRef,
    /// Optional dead-letter queue repository for failed events
    dlq: Option<DlqRepositoryRef>,
}

impl std::fmt::Debug for EventCaptureService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventCaptureService")
            .field("storage", &"<EventRepository>")
            .field("dlq", &self.dlq.as_ref().map(|_| "<DlqRepository>"))
            .finish()
    }
}

impl EventCaptureService {
    /// Create new event capture service
    pub fn new(storage: EventRepositoryRef) -> Self {
        Self { storage, dlq: None }
    }

    /// Create event capture service with dead-letter queue support
    pub fn with_dlq(storage: EventRepositoryRef, dlq: DlqRepositoryRef) -> Self {
        Self {
            storage,
            dlq: Some(dlq),
        }
    }

    /// Check if dead-letter queue is enabled
    pub fn has_dlq(&self) -> bool {
        self.dlq.is_some()
    }

    /// Save failed events to the dead-letter queue
    ///
    /// This is called when capture_events fails to persist events to the main store.
    /// Events are serialized to JSON and saved with the error message for later retry.
    ///
    /// Returns the number of events successfully saved to DLQ, or 0 if DLQ is disabled.
    pub async fn save_to_dlq(&self, events: &[MetricEvent], error_message: &str) -> usize {
        let Some(dlq) = &self.dlq else {
            // DLQ not enabled - log events as JSON for manual recovery (last resort)
            for event in events {
                error!(
                    event_json = %serde_json::to_string(event).unwrap_or_else(|_| "<serialization failed>".to_string()),
                    "Event lost - DLQ not available, manual recovery required"
                );
            }
            return 0;
        };

        // Serialize events and save to DLQ
        let dlq_entries: Vec<(String, String)> = events
            .iter()
            .filter_map(|event| {
                serde_json::to_string(event)
                    .ok()
                    .map(|json| (json, error_message.to_string()))
            })
            .collect();

        if dlq_entries.is_empty() {
            return 0;
        }

        match dlq.save_batch(dlq_entries.clone()).await {
            Ok(ids) => {
                tracing::info!(
                    count = ids.len(),
                    "Saved {} failed events to dead-letter queue",
                    ids.len()
                );
                ids.len()
            }
            Err(dlq_err) => {
                error!(
                    error = %dlq_err,
                    count = events.len(),
                    "CRITICAL: Failed to save events to dead-letter queue"
                );
                // Last resort: log events as JSON for manual recovery
                for (event_json, _) in dlq_entries {
                    error!(
                        event_json = %event_json,
                        "Event lost - manual recovery required"
                    );
                }
                0
            }
        }
    }

    /// Capture and store a metric event
    ///
    /// This is the single entry point for saving events from adapters.
    /// It can be extended in the future with:
    /// - Event validation
    /// - Rate limiting
    /// - Event enrichment (adding metadata)
    /// - Publishing to broadcast channels
    pub async fn capture_event(&self, event: &MetricEvent) -> Result<i64> {
        // Store event in repository
        let event_id = self.storage.save(event).await?;
        Ok(event_id)
    }

    /// Capture multiple events in batch
    ///
    /// Uses transaction batching for efficiency - all events are saved
    /// in a single database transaction.
    /// Can be used for bulk imports or high-throughput scenarios.
    pub async fn capture_events(&self, events: &[MetricEvent]) -> Result<Vec<i64>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        // Use batch save for transaction-based efficiency
        Ok(self.storage.save_batch(events).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_testing::fixtures::sample_event;
    use detrix_testing::{MockDlqRepository, MockEventRepository};
    use std::sync::Arc;

    async fn create_test_service() -> (EventCaptureService, Arc<MockEventRepository>) {
        let repository = Arc::new(MockEventRepository::new());
        let service = EventCaptureService::new(Arc::clone(&repository) as EventRepositoryRef);
        (service, repository)
    }

    async fn create_test_service_with_dlq() -> (
        EventCaptureService,
        Arc<MockEventRepository>,
        Arc<MockDlqRepository>,
    ) {
        let event_repo = Arc::new(MockEventRepository::new());
        let dlq_repo = Arc::new(MockDlqRepository::new());
        let service = EventCaptureService::with_dlq(
            Arc::clone(&event_repo) as EventRepositoryRef,
            Arc::clone(&dlq_repo) as DlqRepositoryRef,
        );
        (service, event_repo, dlq_repo)
    }

    #[tokio::test]
    async fn test_capture_event() {
        let (service, repository) = create_test_service().await;

        let event = sample_event(1, 42.0);
        let event_id = service.capture_event(&event).await.unwrap();

        assert!(event_id > 0);
        assert_eq!(repository.count(), 1);
    }

    #[tokio::test]
    async fn test_capture_multiple_events() {
        let (service, repository) = create_test_service().await;

        let events = vec![
            sample_event(1, 42.0),
            sample_event(2, 42.0),
            sample_event(3, 42.0),
        ];

        let event_ids = service.capture_events(&events).await.unwrap();

        assert_eq!(event_ids.len(), 3);
        assert!(event_ids.iter().all(|&id| id > 0));
        assert_eq!(repository.count(), 3);
    }

    #[tokio::test]
    async fn test_capture_events_empty() {
        let (service, _repository) = create_test_service().await;

        let events: Vec<MetricEvent> = vec![];
        let event_ids = service.capture_events(&events).await.unwrap();

        assert!(event_ids.is_empty());
    }

    #[tokio::test]
    async fn test_new_without_dlq() {
        let (service, _) = create_test_service().await;

        assert!(!service.has_dlq());
    }

    #[tokio::test]
    async fn test_with_dlq() {
        let (service, _, _) = create_test_service_with_dlq().await;

        assert!(service.has_dlq());
    }

    #[tokio::test]
    async fn test_save_to_dlq_without_dlq_returns_zero() {
        let (service, _) = create_test_service().await;

        let events = vec![sample_event(1, 42.0), sample_event(2, 42.0)];

        // Without DLQ configured, save_to_dlq should return 0
        let saved = service.save_to_dlq(&events, "Test error").await;

        assert_eq!(saved, 0);
    }

    #[tokio::test]
    async fn test_save_to_dlq_with_dlq_saves_events() {
        let (service, _, dlq_repo) = create_test_service_with_dlq().await;

        let events = vec![sample_event(1, 42.0), sample_event(2, 42.0)];

        let saved = service.save_to_dlq(&events, "Storage failure").await;

        assert_eq!(saved, 2);
        assert_eq!(dlq_repo.count().await, 2);

        // Verify error message is preserved
        let entries = dlq_repo.get_entries().await;
        assert!(entries.iter().all(|(_, err)| err == "Storage failure"));
    }

    #[tokio::test]
    async fn test_save_to_dlq_with_empty_events() {
        let (service, _, dlq_repo) = create_test_service_with_dlq().await;

        let events: Vec<MetricEvent> = vec![];
        let saved = service.save_to_dlq(&events, "Error").await;

        assert_eq!(saved, 0);
        assert_eq!(dlq_repo.count().await, 0);
    }

    #[tokio::test]
    async fn test_save_to_dlq_failure_returns_zero() {
        let event_repo = Arc::new(MockEventRepository::new());
        let dlq_repo = Arc::new(MockDlqRepository::failing());
        let service = EventCaptureService::with_dlq(
            Arc::clone(&event_repo) as EventRepositoryRef,
            Arc::clone(&dlq_repo) as DlqRepositoryRef,
        );

        let events = vec![sample_event(1, 42.0)];
        let saved = service.save_to_dlq(&events, "Error").await;

        // When DLQ itself fails, should return 0
        assert_eq!(saved, 0);
    }

    #[tokio::test]
    async fn test_save_to_dlq_serializes_events_to_json() {
        let (service, _, dlq_repo) = create_test_service_with_dlq().await;

        let event = sample_event(42, 42.0);
        let events = vec![event.clone()];

        service.save_to_dlq(&events, "Error").await;

        let entries = dlq_repo.get_entries().await;
        assert_eq!(entries.len(), 1);

        // Verify JSON is valid and contains expected fields
        let (json, _) = &entries[0];
        let parsed: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(parsed["metric_id"], 42);
    }

    #[tokio::test]
    async fn test_debug_impl() {
        let (service, _) = create_test_service().await;
        let debug_str = format!("{:?}", service);
        assert!(debug_str.contains("EventCaptureService"));
        assert!(debug_str.contains("<EventRepository>"));
    }

    #[tokio::test]
    async fn test_debug_impl_with_dlq() {
        let (service, _, _) = create_test_service_with_dlq().await;
        let debug_str = format!("{:?}", service);
        assert!(debug_str.contains("EventCaptureService"));
        assert!(debug_str.contains("<DlqRepository>"));
    }
}
