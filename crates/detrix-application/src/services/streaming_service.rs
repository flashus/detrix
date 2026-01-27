//! Event streaming use cases (protocol-agnostic)

use crate::ports::{EventOutputRef, EventRepositoryRef, MetricRepositoryRef, OutputStats};
use crate::services::shutdown::GracefulShutdownHandle;
use crate::Result;
use detrix_config::constants::DEFAULT_DRAIN_TIMEOUT_MS;
use detrix_config::{ApiConfig, DaemonConfig};
use detrix_core::{MetricEvent, MetricId, SystemEvent};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, Mutex};

/// Counter for dropped events due to backpressure
static OUTPUT_DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Get the count of dropped output events (for monitoring)
pub fn output_dropped_events_count() -> u64 {
    OUTPUT_DROPPED_EVENTS.load(Ordering::Relaxed)
}

/// Builder for StreamingService
///
/// # Example
/// ```ignore
/// let service = StreamingService::builder(event_storage, metric_storage)
///     .event_channel(tx)
///     .system_event_channel(sys_tx)
///     .api_config(config)
///     .output(gelf_output)
///     .build();
/// ```
pub struct StreamingServiceBuilder {
    event_storage: EventRepositoryRef,
    metric_storage: MetricRepositoryRef,
    event_tx: Option<broadcast::Sender<MetricEvent>>,
    system_event_tx: Option<broadcast::Sender<SystemEvent>>,
    api_config: ApiConfig,
    output: Option<EventOutputRef>,
    drain_timeout: Duration,
}

impl StreamingServiceBuilder {
    /// Create a new builder with required dependencies
    pub fn new(event_storage: EventRepositoryRef, metric_storage: MetricRepositoryRef) -> Self {
        Self {
            event_storage,
            metric_storage,
            event_tx: None,
            system_event_tx: None,
            api_config: ApiConfig::default(),
            output: None,
            drain_timeout: Duration::from_millis(DEFAULT_DRAIN_TIMEOUT_MS),
        }
    }

    /// Configure from DaemonConfig
    ///
    /// Sets drain_timeout from the daemon configuration.
    pub fn daemon_config(mut self, config: &DaemonConfig) -> Self {
        self.drain_timeout = Duration::from_millis(config.drain_timeout_ms);
        self
    }

    /// Set the drain timeout for graceful shutdown
    ///
    /// When shutting down, the output worker will attempt to drain queued
    /// events within this timeout before aborting.
    pub fn drain_timeout(mut self, timeout: Duration) -> Self {
        self.drain_timeout = timeout;
        self
    }

    /// Set the event broadcast channel
    ///
    /// If not set, a new channel with default capacity will be created.
    pub fn event_channel(mut self, tx: broadcast::Sender<MetricEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Set the system event broadcast channel
    ///
    /// If not set, a new channel with default capacity will be created.
    pub fn system_event_channel(mut self, tx: broadcast::Sender<SystemEvent>) -> Self {
        self.system_event_tx = Some(tx);
        self
    }

    /// Set the API configuration
    ///
    /// If not set, defaults are used.
    pub fn api_config(mut self, config: ApiConfig) -> Self {
        self.api_config = config;
        self
    }

    /// Set the event output (Graylog/GELF, etc.)
    ///
    /// If set, spawns a background worker for bounded event processing.
    pub fn output(mut self, output: EventOutputRef) -> Self {
        self.output = Some(output);
        self
    }

    /// Build the StreamingService
    pub fn build(self) -> StreamingService {
        // Use configured event buffer capacity (from api_config)
        let event_buffer_capacity = self.api_config.event_buffer_capacity;

        // Create event channel if not provided
        let event_tx = self
            .event_tx
            .unwrap_or_else(|| broadcast::channel(event_buffer_capacity).0);

        // Create system event channel if not provided (use config capacity)
        let system_event_channel_capacity = self.api_config.system_event_channel_capacity;
        let system_event_tx = self
            .system_event_tx
            .unwrap_or_else(|| broadcast::channel(system_event_channel_capacity).0);

        // Spawn output worker if output is configured
        let (output_tx, output_worker_handle) = if let Some(out) = self.output.as_ref() {
            let (tx, rx) = mpsc::channel(event_buffer_capacity);
            let handle =
                StreamingService::spawn_output_worker(Arc::clone(out), rx, self.drain_timeout);
            (Some(tx), Some(handle))
        } else {
            (None, None)
        };

        StreamingService {
            event_storage: self.event_storage,
            metric_storage: self.metric_storage,
            event_tx: Arc::new(event_tx),
            system_event_tx: Arc::new(system_event_tx),
            api_config: self.api_config,
            drain_timeout: self.drain_timeout,
            output: self.output,
            output_tx,
            output_worker_handle: Arc::new(Mutex::new(output_worker_handle)),
        }
    }
}

/// Streaming service (protocol-agnostic)
#[derive(Clone)]
pub struct StreamingService {
    event_storage: EventRepositoryRef,
    metric_storage: MetricRepositoryRef,
    /// Broadcast channel for real-time metric events
    event_tx: Arc<broadcast::Sender<MetricEvent>>,
    /// Broadcast channel for system events (crashes, connections, metric CRUD)
    system_event_tx: Arc<broadcast::Sender<SystemEvent>>,
    /// API configuration (for query limits)
    api_config: ApiConfig,
    /// Drain timeout for graceful shutdown
    drain_timeout: Duration,
    /// Optional event output (Graylog/GELF, etc.)
    output: Option<EventOutputRef>,
    /// Bounded channel sender for output worker
    output_tx: Option<mpsc::Sender<MetricEvent>>,
    /// Handle for graceful shutdown of output worker
    output_worker_handle: Arc<Mutex<Option<GracefulShutdownHandle>>>,
}

impl std::fmt::Debug for StreamingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamingService")
            .field("event_storage", &"<EventRepository>")
            .field("metric_storage", &"<MetricRepository>")
            .field("output", &self.output.as_ref().map(|o| o.name()))
            .field("output_worker", &self.output_tx.is_some())
            .finish()
    }
}

impl StreamingService {
    /// Create a new builder for StreamingService
    ///
    /// # Example
    /// ```ignore
    /// let service = StreamingService::builder(event_storage, metric_storage)
    ///     .event_channel(tx)
    ///     .api_config(config)
    ///     .build();
    /// ```
    pub fn builder(
        event_storage: EventRepositoryRef,
        metric_storage: MetricRepositoryRef,
    ) -> StreamingServiceBuilder {
        StreamingServiceBuilder::new(event_storage, metric_storage)
    }

    /// Spawn the background worker that processes output events
    /// Returns a GracefulShutdownHandle for lifecycle management
    fn spawn_output_worker(
        output: EventOutputRef,
        mut rx: mpsc::Receiver<MetricEvent>,
        drain_timeout: Duration,
    ) -> GracefulShutdownHandle {
        let output_name = output.name().to_string();
        let (mut handle, mut shutdown_rx) =
            GracefulShutdownHandle::new(drain_timeout, format!("output-worker-{}", output_name));

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;

                    // Check shutdown signal first
                    _ = shutdown_rx.wait_for_shutdown() => {
                        tracing::info!("Output worker for '{}' received shutdown signal, draining", output.name());
                        // Drain remaining events from the channel
                        while let Ok(event) = rx.try_recv() {
                            if let Err(e) = output.send(&event).await {
                                tracing::warn!("Failed to send event during drain to '{}': {}", output.name(), e);
                            }
                        }
                        tracing::debug!("Output worker for '{}' drain complete", output.name());
                        break;
                    }

                    // Process incoming events
                    event = rx.recv() => {
                        match event {
                            Some(event) => {
                                if let Err(e) = output.send(&event).await {
                                    tracing::warn!("Failed to send event to output '{}': {}", output.name(), e);
                                }
                            }
                            None => {
                                // Channel closed, exit
                                tracing::debug!("Output worker for '{}' channel closed", output.name());
                                break;
                            }
                        }
                    }
                }
            }
            tracing::debug!("Output worker for '{}' shutting down", output.name());
        });

        handle.set_task(task);
        handle
    }

    /// Set the event output (can be called after creation)
    ///
    /// This spawns a background worker for bounded event processing.
    /// Note: This is async because it may need to shutdown an existing worker.
    pub async fn with_output(self, output: EventOutputRef) -> Self {
        // Gracefully shutdown any existing worker
        if let Some(mut handle) = self.output_worker_handle.lock().await.take() {
            handle.shutdown().await;
        }

        // Use configured event buffer capacity
        let (tx, rx) = mpsc::channel(self.api_config.event_buffer_capacity);
        let handle = Self::spawn_output_worker(Arc::clone(&output), rx, self.drain_timeout);

        // Store the new handle
        *self.output_worker_handle.lock().await = Some(handle);

        Self {
            output: Some(output),
            output_tx: Some(tx),
            ..self
        }
    }

    /// Get output statistics (if configured)
    pub fn output_stats(&self) -> Option<OutputStats> {
        self.output.as_ref().map(|o| o.stats())
    }

    /// Check if output is healthy
    pub fn is_output_healthy(&self) -> bool {
        self.output.as_ref().is_some_and(|o| o.is_healthy())
    }

    /// Query events for a specific metric
    pub async fn query_metric_events(
        &self,
        metric_id: MetricId,
        limit: Option<i64>,
    ) -> Result<Vec<MetricEvent>> {
        let events = self
            .event_storage
            .find_by_metric_id(
                metric_id,
                limit.unwrap_or(self.api_config.default_query_limit),
            )
            .await?;
        Ok(events)
    }

    /// Query events for multiple metrics (single DB call)
    pub async fn query_metric_events_batch(
        &self,
        metric_ids: &[MetricId],
        limit: Option<i64>,
    ) -> Result<Vec<MetricEvent>> {
        let events = self
            .event_storage
            .find_by_metric_ids(
                metric_ids,
                limit.unwrap_or(self.api_config.default_query_limit),
            )
            .await?;
        Ok(events)
    }

    /// Query events for a group
    ///
    /// Fetches metrics belonging to the group, then queries events for those metrics.
    pub async fn query_group_events(
        &self,
        group: &str,
        limit: Option<i64>,
    ) -> Result<Vec<MetricEvent>> {
        // Get all metrics in this group
        let metrics = self.metric_storage.find_by_group(group).await?;

        if metrics.is_empty() {
            return Ok(vec![]);
        }

        // Extract metric IDs
        let metric_ids: Vec<MetricId> = metrics.into_iter().filter_map(|m| m.id).collect();

        if metric_ids.is_empty() {
            return Ok(vec![]);
        }

        // Query events for those metrics
        self.query_metric_events_batch(&metric_ids, limit).await
    }

    /// Query all events (most recent first)
    pub async fn query_all_events(&self, limit: Option<i64>) -> Result<Vec<MetricEvent>> {
        let events = self
            .event_storage
            .find_recent(limit.unwrap_or(self.api_config.default_query_limit))
            .await?;
        Ok(events)
    }

    /// Get the latest value for a metric
    pub async fn get_latest_value(&self, metric_id: MetricId) -> Result<Option<MetricEvent>> {
        let events = self.event_storage.find_by_metric_id(metric_id, 1).await?;
        Ok(events.into_iter().next())
    }

    /// Subscribe to real-time events (returns a channel)
    /// This will be used by WebSocket and gRPC streaming
    pub fn subscribe_to_events(&self) -> broadcast::Receiver<MetricEvent> {
        self.event_tx.subscribe()
    }

    /// Subscribe to events for a specific metric
    ///
    /// Note: Filtering happens on the receiver side. The broadcast channel
    /// sends all events, and the caller filters by metric_id.
    pub fn subscribe_to_metric(&self, _metric_id: MetricId) -> broadcast::Receiver<MetricEvent> {
        // Return the same channel - caller filters by metric_id
        self.event_tx.subscribe()
    }

    /// Subscribe to events for a group
    ///
    /// Note: Filtering happens on the receiver side. The broadcast channel
    /// sends all events, and the caller filters by group's metric_ids.
    pub fn subscribe_to_group(&self, _group: &str) -> broadcast::Receiver<MetricEvent> {
        // Return the same channel - caller filters by group's metric_ids
        self.event_tx.subscribe()
    }

    /// Publish an event to all subscribers and to the external output (if configured)
    ///
    /// This broadcasts to WebSocket/gRPC subscribers and queues the event for
    /// the output worker via a bounded channel. If the output queue is full,
    /// the event is dropped with a warning (backpressure handling).
    pub fn publish_event(&self, event: MetricEvent) -> Result<usize> {
        // Broadcast to subscribers
        let subscriber_count = self.event_tx.send(event.clone())?;

        // Queue event for output worker (bounded channel with backpressure)
        if let Some(tx) = &self.output_tx {
            if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(event) {
                OUTPUT_DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    "Output queue full, event dropped (total dropped: {})",
                    OUTPUT_DROPPED_EVENTS.load(Ordering::Relaxed)
                );
            }
            // Disconnected error means worker died - silently ignore
        }

        Ok(subscriber_count)
    }

    /// Publish an event asynchronously (waits for output to complete)
    ///
    /// Use this when you need to confirm the event was sent to all outputs.
    pub async fn publish_event_async(&self, event: MetricEvent) -> Result<usize> {
        // Broadcast to subscribers
        let subscriber_count = self.event_tx.send(event.clone())?;

        // Send to external output (Graylog/GELF)
        if let Some(output) = &self.output {
            if let Err(e) = output.send(&event).await {
                tracing::warn!("Failed to send event to output '{}': {}", output.name(), e);
                // Don't fail - output errors are non-fatal
            }
        }

        Ok(subscriber_count)
    }

    /// Flush the external output (if configured)
    pub async fn flush_output(&self) -> Result<()> {
        if let Some(output) = &self.output {
            output.flush().await?;
        }
        Ok(())
    }

    /// Get the broadcast sender (for external publishers like adapters)
    pub fn event_sender(&self) -> broadcast::Sender<MetricEvent> {
        (*self.event_tx).clone()
    }

    // === System Event Methods ===

    /// Subscribe to system events (crashes, connections, metric CRUD)
    pub fn subscribe_to_system_events(&self) -> broadcast::Receiver<SystemEvent> {
        self.system_event_tx.subscribe()
    }

    /// Publish a system event to all subscribers
    ///
    /// Returns the number of subscribers that received the event.
    pub fn publish_system_event(&self, event: SystemEvent) -> Result<usize> {
        Ok(self.system_event_tx.send(event)?)
    }

    /// Get the system event broadcast sender (for external publishers)
    pub fn system_event_sender(&self) -> broadcast::Sender<SystemEvent> {
        (*self.system_event_tx).clone()
    }

    /// Shutdown the streaming service gracefully.
    ///
    /// This method:
    /// 1. Signals the output worker to begin shutdown
    /// 2. Waits for the worker to drain queued events (with timeout)
    /// 3. Aborts the worker if drain times out
    ///
    /// Call this during graceful shutdown to clean up background tasks.
    pub async fn shutdown(&self) {
        if let Some(mut handle) = self.output_worker_handle.lock().await.take() {
            tracing::debug!("Initiating graceful shutdown of output worker");
            handle.shutdown().await;
        }
    }
}
