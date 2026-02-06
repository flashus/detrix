//! AdapterLifecycleManager - Central manager for DAP adapter lifecycle and event routing
//!
//! This service provides:
//! - Centralized adapter lifecycle management (start, stop, registry)
//! - Automatic event subscription and routing to EventCaptureService
//! - Event broadcasting to real-time subscribers (WebSocket, gRPC streams)
//! - Event batching for high-throughput storage (configurable)
//!
//! Following Clean Architecture:
//! - Lives in Application layer
//! - Depends on port traits (DapAdapter, DapAdapterFactory)
//! - Is protocol-agnostic (no knowledge of gRPC, REST, MCP)

use crate::ports::{
    ConnectionRepositoryRef, DapAdapterFactoryRef, DapAdapterRef, EventOutputRef,
    MetricRepositoryRef,
};
use crate::services::EventCaptureService;
use dashmap::DashMap;
use detrix_config::{AdapterConnectionConfig, DaemonConfig, EventBatchingConfig};
use detrix_core::{
    ConnectionId, ConnectionStatus, Metric, MetricEvent, Result, SourceLanguage, SystemEvent,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, instrument, trace, warn};

/// Status of a managed adapter
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedAdapterStatus {
    /// Adapter is starting up
    Starting,
    /// Adapter is running and listening for events
    Running,
    /// Adapter is stopping
    Stopping,
    /// Adapter has stopped (error or graceful)
    Stopped,
    /// Adapter encountered an error
    Error,
}

/// Information about a managed adapter (public view)
#[derive(Debug, Clone)]
pub struct ManagedAdapterInfo {
    /// Connection identifier
    pub connection_id: ConnectionId,
    /// Current status
    pub status: ManagedAdapterStatus,
    /// When the adapter was started
    pub started_at: Instant,
    /// Whether the adapter is connected
    pub is_connected: bool,
    /// SafeMode: Only allow logpoints (non-blocking), disable breakpoint-based operations
    pub safe_mode: bool,
}

/// Result of starting an adapter, including any degradation info
#[derive(Debug, Clone, Default)]
pub struct StartAdapterResult {
    /// Whether metric sync failed (metrics may not be active)
    pub sync_failed: bool,
    /// Whether program resume failed (program may still be paused)
    pub resume_failed: bool,
    /// Number of metrics synced successfully
    pub metrics_synced: usize,
    /// Number of metrics that failed to sync
    pub metrics_failed: usize,
}

/// Internal representation of a managed adapter
struct ManagedAdapter {
    /// The adapter instance
    adapter: DapAdapterRef,
    /// Handle to the event listener task (for cancellation)
    event_listener_handle: JoinHandle<()>,
    /// Shutdown signal sender for graceful shutdown
    shutdown_tx: watch::Sender<bool>,
    /// When the adapter was started
    started_at: Instant,
    /// Current status
    status: ManagedAdapterStatus,
    /// SafeMode: Only allow logpoints (non-blocking), disable breakpoint-based operations
    safe_mode: bool,
}

/// Central manager for adapter lifecycle and event routing
///
/// This service handles:
/// - Creating and starting adapters
/// - Subscribing to adapter events and routing them
/// - Stopping adapters and cleaning up resources
/// - Providing adapter lookup and status information
/// - Batching events for efficient database writes
/// - Emitting system events for crashes and connection changes
pub struct AdapterLifecycleManager {
    /// Map of connection_id -> managed adapter
    /// Uses DashMap for better concurrent performance (no write lock contention)
    adapters: Arc<DashMap<ConnectionId, ManagedAdapter>>,

    /// Service for storing events to database
    event_capture_service: Arc<EventCaptureService>,

    /// Broadcast channel for real-time metric event subscribers
    event_broadcast_tx: broadcast::Sender<MetricEvent>,

    /// Broadcast channel for system events (crashes, connections)
    system_event_tx: broadcast::Sender<SystemEvent>,

    /// Factory for creating adapters
    adapter_factory: DapAdapterFactoryRef,

    /// Repository for loading metrics to sync on connect
    metric_repository: MetricRepositoryRef,

    /// Event batching configuration
    batching_config: EventBatchingConfig,

    /// Adapter configuration for batch operations
    adapter_config: AdapterConnectionConfig,

    /// Event buffer for batching (when batching is enabled)
    /// Note: pub(crate) for property-based testing access
    pub(crate) event_buffer: Arc<Mutex<VecDeque<MetricEvent>>>,

    /// Channel to signal flush requests
    flush_tx: mpsc::Sender<()>,

    /// Handle to the flush task (for cleanup)
    flush_task_handle: Arc<Mutex<Option<JoinHandle<()>>>>,

    /// Drain timeout for graceful shutdown (ms)
    drain_timeout_ms: u64,

    /// Channel for requesting adapter cleanup when crashes are detected
    /// Event listeners send connection IDs here when they detect a crash
    cleanup_tx: mpsc::Sender<ConnectionId>,

    /// Handle to the cleanup task
    cleanup_task_handle: Arc<Mutex<Option<JoinHandle<()>>>>,

    /// Connection repository for updating safe_mode and status
    connection_repository: ConnectionRepositoryRef,

    /// Optional event output for external destinations (Graylog/GELF, etc.)
    event_output: Option<EventOutputRef>,

    /// Connections currently being started (prevents concurrent start attempts)
    /// This prevents race conditions when restore_connections and create_connection
    /// try to start the same adapter simultaneously.
    /// Maps connection_id -> port being connected to (allows cancellation when port changes)
    pending_starts: Arc<DashMap<ConnectionId, u16>>,
}

impl AdapterLifecycleManager {
    /// Create a new AdapterLifecycleManager with default batching config
    ///
    /// # Arguments
    /// * `event_capture_service` - Service for storing events to database
    /// * `event_broadcast_tx` - Broadcast channel for real-time subscribers
    /// * `system_event_tx` - Broadcast channel for system events (crashes, connections)
    /// * `adapter_factory` - Factory for creating language-specific adapters
    /// * `metric_repository` - Repository for loading metrics to sync on connect
    /// * `connection_repository` - Repository for updating connection status on crash
    pub fn new(
        event_capture_service: Arc<EventCaptureService>,
        event_broadcast_tx: broadcast::Sender<MetricEvent>,
        system_event_tx: broadcast::Sender<SystemEvent>,
        adapter_factory: DapAdapterFactoryRef,
        metric_repository: MetricRepositoryRef,
        connection_repository: ConnectionRepositoryRef,
    ) -> Self {
        let daemon_config = DaemonConfig::default();
        Self::with_config(
            event_capture_service,
            event_broadcast_tx,
            system_event_tx,
            adapter_factory,
            metric_repository,
            connection_repository,
            EventBatchingConfig::default(),
            AdapterConnectionConfig::default(),
            daemon_config.drain_timeout_ms,
            None, // No GELF output by default
        )
    }

    /// Create a new AdapterLifecycleManager with full configuration
    ///
    /// # Arguments
    /// * `event_capture_service` - Service for storing events to database
    /// * `event_broadcast_tx` - Broadcast channel for real-time subscribers
    /// * `system_event_tx` - Broadcast channel for system events (crashes, connections)
    /// * `adapter_factory` - Factory for creating language-specific adapters
    /// * `metric_repository` - Repository for loading metrics to sync on connect
    /// * `connection_repository` - Repository for updating connection status on crash
    /// * `batching_config` - Event batching configuration
    /// * `adapter_config` - Adapter connection configuration (for batch thresholds)
    /// * `drain_timeout_ms` - Timeout for draining events during graceful shutdown
    /// * `event_output` - Optional event output for external destinations (Graylog/GELF)
    #[allow(clippy::too_many_arguments)]
    pub fn with_config(
        event_capture_service: Arc<EventCaptureService>,
        event_broadcast_tx: broadcast::Sender<MetricEvent>,
        system_event_tx: broadcast::Sender<SystemEvent>,
        adapter_factory: DapAdapterFactoryRef,
        metric_repository: MetricRepositoryRef,
        connection_repository: ConnectionRepositoryRef,
        batching_config: EventBatchingConfig,
        adapter_config: AdapterConnectionConfig,
        drain_timeout_ms: u64,
        event_output: Option<EventOutputRef>,
    ) -> Self {
        let (flush_tx, flush_rx) = mpsc::channel(1);
        let (cleanup_tx, cleanup_rx) = mpsc::channel(16); // Buffer for cleanup requests
        let event_buffer = Arc::new(Mutex::new(VecDeque::with_capacity(
            batching_config.batch_size,
        )));
        let flush_task_handle = Arc::new(Mutex::new(None));
        let cleanup_task_handle = Arc::new(Mutex::new(None));
        let adapters = Arc::new(DashMap::new());

        let manager = Self {
            adapters: Arc::clone(&adapters),
            event_capture_service: Arc::clone(&event_capture_service),
            event_broadcast_tx,
            system_event_tx,
            adapter_factory,
            metric_repository,
            batching_config: batching_config.clone(),
            adapter_config,
            event_buffer: Arc::clone(&event_buffer),
            flush_tx,
            flush_task_handle: Arc::clone(&flush_task_handle),
            drain_timeout_ms,
            cleanup_tx,
            cleanup_task_handle: Arc::clone(&cleanup_task_handle),
            event_output,
            pending_starts: Arc::new(DashMap::new()),
            connection_repository: Arc::clone(&connection_repository),
        };

        // Start the flush task if flush interval is configured
        // NOTE: We store the handle synchronously to avoid race conditions where
        // stop_all() is called before the async spawn completes storing the handle.
        // Batching is always enabled for performance.
        if batching_config.flush_interval_ms > 0 {
            let handle = Self::spawn_flush_task(
                Arc::clone(&event_buffer),
                Arc::clone(&event_capture_service),
                flush_rx,
                Duration::from_millis(batching_config.flush_interval_ms),
            );
            // Store handle synchronously using try_lock to avoid blocking
            // This is safe because we just created the mutex and no other code has access yet
            if let Ok(mut guard) = flush_task_handle.try_lock() {
                *guard = Some(handle);
            } else {
                // Fallback: spawn async storage (original behavior, should never happen here)
                let handle_clone = flush_task_handle.clone();
                tokio::spawn(async move {
                    let mut guard = handle_clone.lock().await;
                    *guard = Some(handle);
                });
            }
        }

        // Start the cleanup task to handle crashed adapter cleanup
        {
            let handle = Self::spawn_cleanup_task(
                Arc::clone(&adapters),
                connection_repository,
                cleanup_rx,
                drain_timeout_ms,
            );
            if let Ok(mut guard) = cleanup_task_handle.try_lock() {
                *guard = Some(handle);
            }
        }

        // Log whether GELF output is configured
        if manager.event_output.is_some() {
            debug!("AdapterLifecycleManager initialized with GELF output enabled");
        } else {
            debug!("AdapterLifecycleManager initialized without GELF output");
        }

        manager
    }

    /// Spawn the background flush task
    fn spawn_flush_task(
        event_buffer: Arc<Mutex<VecDeque<MetricEvent>>>,
        event_capture_service: Arc<EventCaptureService>,
        mut flush_rx: mpsc::Receiver<()>,
        flush_interval: Duration,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(flush_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        // Timer-based flush
                        Self::flush_buffer_static(&event_buffer, &event_capture_service).await;
                    }
                    msg = flush_rx.recv() => {
                        match msg {
                            Some(()) => {
                                // Explicit flush request (e.g., batch_size reached)
                                Self::flush_buffer_static(&event_buffer, &event_capture_service).await;
                            }
                            None => {
                                // Channel closed, do final flush and exit
                                debug!("Flush channel closed, performing final flush");
                                Self::flush_buffer_static(&event_buffer, &event_capture_service).await;
                                break;
                            }
                        }
                    }
                }
            }
            info!("Event flush task stopped");
        })
    }

    /// Spawn background task to clean up crashed adapters
    ///
    /// This task receives connection IDs from event listeners when they detect
    /// that the adapter has crashed (e.g., debugpy was killed). It then performs
    /// the actual cleanup to release resources like TCP sockets and updates
    /// the connection status in the database.
    fn spawn_cleanup_task(
        adapters: Arc<DashMap<ConnectionId, ManagedAdapter>>,
        connection_repository: ConnectionRepositoryRef,
        mut cleanup_rx: mpsc::Receiver<ConnectionId>,
        drain_timeout_ms: u64,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            info!("Adapter cleanup task started");

            while let Some(connection_id) = cleanup_rx.recv().await {
                info!(
                    "Cleanup request received for crashed adapter: {}",
                    connection_id.0
                );

                // Remove adapter from registry and clean up
                if let Some((_, mut managed)) = adapters.remove(&connection_id) {
                    managed.status = ManagedAdapterStatus::Stopped;

                    // The event listener has already exited (that's how we got here),
                    // but we still try to send shutdown signal in case it's still draining
                    let _ = managed.shutdown_tx.send(true);

                    // Wait briefly for the event listener to finish
                    let drain_timeout = Duration::from_millis(drain_timeout_ms);
                    match tokio::time::timeout(drain_timeout, &mut managed.event_listener_handle)
                        .await
                    {
                        Ok(_) => {
                            debug!(
                                "Event listener finished for crashed connection {}",
                                connection_id.0
                            );
                        }
                        Err(_) => {
                            // Timeout - force abort
                            warn!(
                                "Event listener timeout for crashed connection {}, aborting",
                                connection_id.0
                            );
                            managed.event_listener_handle.abort();
                        }
                    }

                    // Stop the underlying adapter (this releases the TCP socket!)
                    if let Err(e) = managed.adapter.stop().await {
                        warn!(
                            "Error stopping crashed adapter for connection {}: {}",
                            connection_id.0, e
                        );
                    }

                    // Update connection status to Disconnected in database
                    // This ensures `detrix status` shows the correct state
                    if let Err(e) = connection_repository
                        .update_status(&connection_id, ConnectionStatus::Disconnected)
                        .await
                    {
                        warn!(
                            "Failed to update connection status for {}: {}",
                            connection_id.0, e
                        );
                    } else {
                        debug!(
                            "Connection {} status updated to Disconnected",
                            connection_id.0
                        );
                    }

                    info!(
                        "Crashed adapter cleaned up for connection {}, resources released",
                        connection_id.0
                    );
                } else {
                    debug!(
                        "Adapter for connection {} already removed (cleanup race)",
                        connection_id.0
                    );
                }
            }

            info!("Adapter cleanup task stopped");
        })
    }

    /// Static helper to flush the buffer (for use in spawn_flush_task)
    ///
    /// ## Dead-Letter Queue
    ///
    /// When flush fails, events are saved to the dead-letter queue for later retry.
    /// If DLQ is not configured, events are logged as JSON for manual recovery.
    async fn flush_buffer_static(
        event_buffer: &Arc<Mutex<VecDeque<MetricEvent>>>,
        event_capture_service: &Arc<EventCaptureService>,
    ) {
        let events_to_save: Vec<MetricEvent> = {
            let mut buffer = event_buffer.lock().await;
            if buffer.is_empty() {
                return;
            }
            buffer.drain(..).collect()
        };

        let count = events_to_save.len();
        debug!("Flushing {} events to storage (batch flush)", count);

        if let Err(e) = event_capture_service.capture_events(&events_to_save).await {
            error!("Failed to flush {} events to storage: {}", count, e);

            // Save failed events to dead-letter queue for later retry
            let saved = event_capture_service
                .save_to_dlq(&events_to_save, &e.to_string())
                .await;

            if saved > 0 {
                info!(
                    saved = saved,
                    total = count,
                    "Saved failed events to dead-letter queue"
                );
            } else if !event_capture_service.has_dlq() {
                warn!(
                    count = count,
                    "Events lost - dead-letter queue not configured"
                );
            }
        } else {
            info!("Successfully flushed {} events to storage", count);
        }
    }

    /// Flush any remaining buffered events
    #[instrument(skip(self))]
    pub async fn flush_events(&self) {
        Self::flush_buffer_static(&self.event_buffer, &self.event_capture_service).await;
    }

    /// Start an adapter for the given connection
    ///
    /// This method:
    /// 1. Creates the adapter via factory (dispatched by language)
    /// 2. Starts the adapter (connects to debugger)
    /// 3. Subscribes to events
    /// 4. Spawns event listener task
    /// 5. Stores in registry
    ///
    /// # Arguments
    /// * `connection_id` - Unique identifier for this connection
    /// * `host` - Host address (e.g., "127.0.0.1")
    /// * `port` - Port number where the debugger is listening
    /// * `language` - Language/adapter type (Python, Go, Rust, etc.)
    ///
    /// # Errors
    /// Returns error if adapter creation, start, or event subscription fails
    ///
    /// # Returns
    /// `StartAdapterResult` with degradation info (sync_failed, resume_failed, metrics counts)
    #[instrument(skip(self), fields(connection_id = %connection_id.0, host = %host, port = port, language = %language, safe_mode = safe_mode))]
    pub async fn start_adapter(
        &self,
        connection_id: ConnectionId,
        host: &str,
        port: u16,
        language: SourceLanguage,
        program: Option<String>,
        pid: Option<u32>,
        safe_mode: bool,
    ) -> Result<StartAdapterResult> {
        info!(
            "Starting {} adapter for connection {} ({}:{}) pid={:?} safe_mode={}",
            language, connection_id.0, host, port, pid, safe_mode
        );

        // Atomically try to mark this connection as being started
        // If another caller is already starting it, we'll wait for them
        // This prevents race conditions when restore_connections and create_connection overlap
        //
        // We use DashMap's entry API for atomic check-and-insert to prevent TOCTOU race
        // The entry check is wrapped in a block to ensure lock is released before waiting
        let max_retries = self.adapter_config.adapter_start_max_retries;
        let mut retry_count = 0;

        loop {
            // Atomically try to insert into pending_starts
            // The block ensures the Entry (and its lock) is dropped before we wait
            let (already_pending, pending_port) = {
                use dashmap::mapref::entry::Entry;
                match self.pending_starts.entry(connection_id.clone()) {
                    Entry::Occupied(entry) => {
                        let pending_port = *entry.get();
                        (true, Some(pending_port))
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(port);
                        (false, None)
                    }
                }
            }; // Entry is dropped here, lock released

            if !already_pending {
                // We're the first to start this connection
                break;
            }

            // Check if the pending connection is to a DIFFERENT port
            // If so, cancel the old attempt and take over (new port = new debugger instance)
            if let Some(old_port) = pending_port {
                if old_port != port {
                    warn!(
                        "Connection {} is being reconnected to new port {} (was {}), cancelling old attempt",
                        connection_id.0, port, old_port
                    );
                    // Remove the old entry to signal cancellation to the old task
                    self.pending_starts.remove(&connection_id);
                    // Insert our new entry
                    self.pending_starts.insert(connection_id.clone(), port);
                    // Proceed with our connection attempt
                    break;
                }
            }

            // Same port - another caller is starting this connection, wait for them
            info!(
                "Adapter start already in progress for connection {}, waiting for it to complete",
                connection_id.0
            );

            // Wait for the pending start to complete (poll with timeout)
            let max_wait = std::time::Duration::from_secs(30);
            let poll_interval = std::time::Duration::from_millis(100);
            let start = std::time::Instant::now();

            while self.pending_starts.contains_key(&connection_id) {
                if start.elapsed() > max_wait {
                    warn!(
                        "Timeout waiting for pending start of connection {}",
                        connection_id.0
                    );
                    return Err(detrix_core::Error::Adapter(format!(
                        "Timeout waiting for connection {} to be started by another caller",
                        connection_id.0
                    )));
                }
                tokio::time::sleep(poll_interval).await;
            }

            // The other caller finished - check if adapter now exists
            if self.adapters.contains_key(&connection_id) {
                info!(
                    "Connection {} was started by another caller, returning success",
                    connection_id.0
                );
                return Ok(StartAdapterResult::default());
            }

            // Other caller failed - retry if we haven't exceeded limit
            retry_count += 1;
            if retry_count >= max_retries {
                warn!(
                    "Max retries ({}) exceeded for connection {}",
                    max_retries, connection_id.0
                );
                return Err(detrix_core::Error::Adapter(format!(
                    "Failed to start connection {} after {} retries",
                    connection_id.0, max_retries
                )));
            }

            info!(
                "Previous start attempt for {} failed, retrying ({}/{})",
                connection_id.0, retry_count, max_retries
            );
            // continue to retry
        }

        // RAII-style cleanup: ensure pending_starts entry is removed on all exit paths
        // Only removes if the port matches (to avoid cancelling a newer request)
        struct PendingGuard {
            pending_starts: Arc<DashMap<ConnectionId, u16>>,
            connection_id: ConnectionId,
            port: u16,
        }
        impl Drop for PendingGuard {
            fn drop(&mut self) {
                // Only remove if our port is still the one registered
                // If a newer request came in with a different port, don't remove their entry
                self.pending_starts
                    .remove_if(&self.connection_id, |_, v| *v == self.port);
            }
        }
        let _pending_guard = PendingGuard {
            pending_starts: Arc::clone(&self.pending_starts),
            connection_id: connection_id.clone(),
            port,
        };

        // Check if adapter already exists
        if self.adapters.contains_key(&connection_id) {
            warn!(
                "Adapter for connection {} already exists, stopping existing",
                connection_id.0
            );
            self.stop_adapter(&connection_id).await?;
        }

        // Create adapter via factory, dispatch by language
        let adapter = match language {
            SourceLanguage::Go => self.adapter_factory.create_go_adapter(host, port).await?,
            SourceLanguage::Python => {
                self.adapter_factory
                    .create_python_adapter(host, port)
                    .await?
            }
            SourceLanguage::Rust => {
                self.adapter_factory
                    .create_rust_adapter(host, port, program.as_deref(), pid)
                    .await?
            }
            _ => {
                return Err(detrix_core::Error::InvalidConfig(
                    SourceLanguage::language_error(language.as_str()).into(),
                ));
            }
        };

        // Start the adapter (connects to debugpy)
        if let Err(e) = adapter.start().await {
            error!(
                "Failed to start adapter for connection {}: {}",
                connection_id.0, e
            );
            return Err(e);
        }

        info!("Adapter started for connection {}", connection_id.0);

        // Subscribe to events
        let event_rx = match adapter.subscribe_events().await {
            Ok(rx) => rx,
            Err(e) => {
                error!(
                    "Failed to subscribe to events for connection {}: {}",
                    connection_id.0, e
                );
                // Clean up adapter
                if let Err(stop_err) = adapter.stop().await {
                    warn!(
                        "Failed to stop adapter during cleanup for connection {}: {}",
                        connection_id.0, stop_err
                    );
                }
                return Err(e);
            }
        };

        // Spawn event listener task
        let (handle, shutdown_tx) =
            self.spawn_event_listener(connection_id.clone(), event_rx, language);

        // Track degradation indicators
        let mut result = StartAdapterResult::default();

        // Sync existing metrics from repository BEFORE continuing program execution
        // This ensures logpoints are set before the program runs
        let metrics = match self
            .metric_repository
            .find_by_connection_id(&connection_id)
            .await
        {
            Ok(metrics) => metrics,
            Err(e) => {
                warn!(
                    error = %e,
                    connection_id = %connection_id.0,
                    "Failed to load metrics for connection, sync_failed=true"
                );
                result.sync_failed = true;
                Vec::new()
            }
        };

        let enabled_metrics: Vec<Metric> = metrics.into_iter().filter(|m| m.enabled).collect();

        if !enabled_metrics.is_empty() {
            info!(
                "Syncing {} enabled metrics as logpoints for connection {}",
                enabled_metrics.len(),
                connection_id.0
            );

            // Use batch operation for efficiency (avoid N+1 individual calls)
            let batch_result = adapter
                .set_metrics_batch(
                    &enabled_metrics,
                    self.adapter_config.batch_threshold,
                    self.adapter_config.batch_concurrency,
                )
                .await;

            // Track metrics sync results
            result.metrics_synced = batch_result.success_count();
            result.metrics_failed = batch_result.failed.len();

            // Log results
            for sync_result in &batch_result.succeeded {
                if sync_result.verified {
                    debug!("Set logpoint for metric at line {}", sync_result.line);
                } else {
                    warn!("Logpoint not verified: {:?}", sync_result.message);
                }
            }

            for failed in &batch_result.failed {
                warn!(
                    "Failed to set logpoint for metric '{}' (id={:?}): {}",
                    failed.name, failed.id, failed.error
                );
            }

            info!(
                "Synced {} metrics ({} succeeded, {} failed) for connection {}",
                enabled_metrics.len(),
                batch_result.success_count(),
                batch_result.failed.len(),
                connection_id.0
            );
        }

        // Resume program execution after setting logpoints
        // For debuggers like Delve that start paused, this is essential
        match adapter.continue_execution().await {
            Ok(resumed) => {
                if resumed {
                    info!(
                        "Resumed program execution for connection {}",
                        connection_id.0
                    );
                } else {
                    debug!("Program already running for connection {}", connection_id.0);
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    connection_id = %connection_id.0,
                    "Failed to resume program execution, resume_failed=true"
                );
                result.resume_failed = true;
            }
        }

        // Check if we were cancelled (a newer request took over with a different port)
        // If our port is no longer in pending_starts, stop the adapter and return
        let was_cancelled = self
            .pending_starts
            .get(&connection_id)
            .map(|entry| *entry != port)
            .unwrap_or(true); // If entry is gone, we were cancelled

        if was_cancelled {
            warn!(
                "Connection {} start was superseded by a newer request, stopping adapter",
                connection_id.0
            );
            // Stop the adapter we just created since a newer request will handle it
            if let Err(e) = adapter.stop().await {
                warn!("Failed to stop superseded adapter: {}", e);
            }
            // Abort the event listener
            handle.abort();
            return Err(detrix_core::Error::Adapter(format!(
                "Connection {} start was superseded by a newer request",
                connection_id.0
            )));
        }

        // Store managed adapter
        let managed = ManagedAdapter {
            adapter,
            event_listener_handle: handle,
            shutdown_tx,
            started_at: Instant::now(),
            status: ManagedAdapterStatus::Running,
            safe_mode,
        };

        self.adapters.insert(connection_id.clone(), managed);

        info!(
            "Adapter registered and listening for events: {}",
            connection_id.0
        );
        Ok(result)
    }

    /// Start an adapter with an existing adapter instance
    ///
    /// Use this when you already have an adapter instance (e.g., from DapAdapterFactory).
    /// The adapter should already be started.
    ///
    /// # Arguments
    /// * `connection_id` - Unique identifier for this connection
    /// * `adapter` - Pre-created adapter instance (should be started)
    /// * `language` - Language/adapter type (for system event reporting)
    /// * `safe_mode` - Whether this connection is in SafeMode (blocks breakpoint-based operations)
    #[instrument(skip(self, adapter), fields(connection_id = %connection_id.0, language = %language, safe_mode = safe_mode))]
    pub async fn register_adapter(
        &self,
        connection_id: ConnectionId,
        adapter: DapAdapterRef,
        language: SourceLanguage,
        safe_mode: bool,
    ) -> Result<()> {
        info!(
            "Registering adapter for connection {} safe_mode={}",
            connection_id.0, safe_mode
        );

        // Check if adapter already exists
        if self.adapters.contains_key(&connection_id) {
            warn!(
                "Adapter for connection {} already exists, stopping existing",
                connection_id.0
            );
            self.stop_adapter(&connection_id).await?;
        }

        // Subscribe to events
        let event_rx = adapter.subscribe_events().await?;

        // Spawn event listener task
        let (handle, shutdown_tx) =
            self.spawn_event_listener(connection_id.clone(), event_rx, language);

        // Store managed adapter
        let managed = ManagedAdapter {
            adapter,
            event_listener_handle: handle,
            shutdown_tx,
            started_at: Instant::now(),
            status: ManagedAdapterStatus::Running,
            safe_mode,
        };

        self.adapters.insert(connection_id.clone(), managed);

        info!(
            "Adapter registered and listening for events: {}",
            connection_id.0
        );
        Ok(())
    }

    /// Spawn event listener task for an adapter
    ///
    /// Returns a tuple of (JoinHandle, shutdown_tx) for graceful shutdown
    fn spawn_event_listener(
        &self,
        connection_id: ConnectionId,
        mut event_rx: tokio::sync::mpsc::Receiver<MetricEvent>,
        language: SourceLanguage,
    ) -> (JoinHandle<()>, watch::Sender<bool>) {
        let broadcast_tx = self.event_broadcast_tx.clone();
        let system_event_tx = self.system_event_tx.clone();
        let cleanup_tx = self.cleanup_tx.clone();
        let conn_id = connection_id.clone();
        let event_buffer = Arc::clone(&self.event_buffer);
        let batch_size = self.batching_config.batch_size;
        let max_buffer_size = self.batching_config.max_buffer_size;
        let flush_tx = self.flush_tx.clone();
        let lang_str = language.to_string();
        let event_output = self.event_output.clone();

        // Create shutdown channel for graceful shutdown
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let handle = tokio::spawn(async move {
            debug!("Event listener started for connection {}", conn_id.0);

            loop {
                tokio::select! {
                    // Check shutdown signal first (biased)
                    biased;

                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Shutdown signal received for connection {}, draining events", conn_id.0);
                            // Drain remaining events from the channel
                            while let Ok(event) = event_rx.try_recv() {
                                Self::process_event(
                                    &event,
                                    &event_buffer,
                                    batch_size,
                                    max_buffer_size,
                                    &flush_tx,
                                    &broadcast_tx,
                                    &conn_id,
                                    &event_output,
                                ).await;
                            }
                            info!("Event drain complete for connection {}", conn_id.0);
                            break;
                        }
                    }

                    event = event_rx.recv() => {
                        match event {
                            Some(event) => {
                                Self::process_event(
                                    &event,
                                    &event_buffer,
                                    batch_size,
                                    max_buffer_size,
                                    &flush_tx,
                                    &broadcast_tx,
                                    &conn_id,
                                    &event_output,
                                ).await;
                            }
                            None => {
                                // Event channel closed - adapter disconnected or crashed
                                warn!(
                                    "Event channel closed for connection {} - adapter disconnected or crashed",
                                    conn_id.0
                                );

                                let crash_event = SystemEvent::debugger_crash(
                                    conn_id.clone(),
                                    "Event channel closed unexpectedly",
                                    &lang_str,
                                );

                                // Emit system event
                                match system_event_tx.send(crash_event) {
                                    Ok(count) => {
                                        info!(
                                            "Debugger crash event broadcast to {} subscribers for connection {}",
                                            count, conn_id.0
                                        );
                                    }
                                    Err(e) => {
                                        warn!(
                                            "No system event subscribers for debugger crash on connection {}: {}",
                                            conn_id.0, e
                                        );
                                    }
                                }

                                // Request cleanup to release resources (TCP socket, etc.)
                                // This is critical for port reuse - without this, the port stays bound!
                                if let Err(e) = cleanup_tx.send(conn_id.clone()).await {
                                    error!(
                                        "Failed to send cleanup request for crashed connection {}: {}",
                                        conn_id.0, e
                                    );
                                } else {
                                    info!(
                                        "Cleanup request sent for crashed connection {}",
                                        conn_id.0
                                    );
                                }

                                break;
                            }
                        }
                    }
                }
            }

            info!("Event listener stopped for connection {}", conn_id.0);
        });

        (handle, shutdown_tx)
    }

    /// Process a single event (helper for spawn_event_listener)
    ///
    /// Events are always buffered for optimal performance. When the buffer
    /// exceeds `max_buffer_size`, oldest events are dropped to prevent
    /// unbounded memory growth under high load.
    /// Events are also sent to external output (Graylog/GELF) if configured.
    #[allow(clippy::too_many_arguments)]
    async fn process_event(
        event: &MetricEvent,
        event_buffer: &Arc<Mutex<VecDeque<MetricEvent>>>,
        batch_size: usize,
        max_buffer_size: usize,
        flush_tx: &mpsc::Sender<()>,
        broadcast_tx: &broadcast::Sender<MetricEvent>,
        conn_id: &ConnectionId,
        event_output: &Option<EventOutputRef>,
    ) {
        debug!(
            "Event received in lifecycle manager for connection {}: metric_id={:?}, metric_name={}",
            conn_id.0, event.metric_id, event.metric_name
        );

        // Buffered mode: add to buffer, flush when batch_size reached
        // Overflow handling: drop oldest events if buffer exceeds max_buffer_size
        let should_flush = {
            let mut buffer = event_buffer.lock().await;

            // Overflow handling: drop oldest events if buffer is at max capacity
            if buffer.len() >= max_buffer_size {
                let to_drop = buffer.len() - max_buffer_size + 1;
                for _ in 0..to_drop {
                    buffer.pop_front();
                }
                warn!(
                    "Event buffer overflow for connection {}: dropped {} oldest events (buffer at max {})",
                    conn_id.0, to_drop, max_buffer_size
                );
            }

            buffer.push_back(event.clone());
            let current_len = buffer.len();
            debug!(
                "Event buffered for connection {} (buffer size: {}/{})",
                conn_id.0, current_len, batch_size
            );
            current_len >= batch_size
        };

        if should_flush {
            debug!("Buffer full, triggering flush for connection {}", conn_id.0);
            // Signal the flush task to flush now
            let _ = flush_tx.send(()).await;
        }

        // Broadcast to real-time subscribers (WebSocket, gRPC streams)
        // Broadcasting happens immediately regardless of batching mode
        // Don't fail if no subscribers - that's normal
        match broadcast_tx.send(event.clone()) {
            Ok(count) => {
                trace!("Event broadcast to {} subscribers", count);
            }
            Err(_) => {
                // No active subscribers - this is normal
                trace!("No active event subscribers");
            }
        }

        // Send to external output (Graylog/GELF) if configured
        if let Some(output) = event_output {
            if let Err(e) = output.send(event).await {
                warn!(
                    "Failed to send event to output '{}' for connection {}: {}",
                    output.name(),
                    conn_id.0,
                    e
                );
            } else {
                trace!(
                    "Event sent to output '{}' for connection {}",
                    output.name(),
                    conn_id.0
                );
            }
        }
    }

    /// Stop an adapter and clean up resources with graceful shutdown
    ///
    /// This method:
    /// 1. Sends shutdown signal to event listener task
    /// 2. Waits for graceful drain with configurable timeout
    /// 3. Aborts if timeout expires
    /// 4. Stops the adapter
    /// 5. Removes from registry
    ///
    /// # Arguments
    /// * `connection_id` - ID of the connection to stop
    ///
    /// # Errors
    /// Returns error if adapter not found or stop fails
    #[instrument(skip(self), fields(connection_id = %connection_id.0))]
    pub async fn stop_adapter(&self, connection_id: &ConnectionId) -> Result<()> {
        info!("Stopping adapter for connection {}", connection_id.0);

        // DashMap::remove returns Option<(K, V)>
        let managed = self.adapters.remove(connection_id);

        if let Some((_, mut managed)) = managed {
            // Update status
            managed.status = ManagedAdapterStatus::Stopping;

            // Send shutdown signal to event listener for graceful drain
            if let Err(e) = managed.shutdown_tx.send(true) {
                warn!(
                    "Failed to send shutdown signal for connection {}: {}",
                    connection_id.0, e
                );
                // Continue with abort if signal fails
                managed.event_listener_handle.abort();
            } else {
                // Wait for graceful shutdown with configurable timeout
                let drain_timeout = Duration::from_millis(self.drain_timeout_ms);
                match tokio::time::timeout(drain_timeout, &mut managed.event_listener_handle).await
                {
                    Ok(Ok(())) => {
                        debug!(
                            "Event listener drained gracefully for connection {}",
                            connection_id.0
                        );
                    }
                    Ok(Err(e)) => {
                        // JoinError - task panicked or was cancelled
                        warn!(
                            "Event listener task error for connection {}: {}",
                            connection_id.0, e
                        );
                    }
                    Err(_) => {
                        // Timeout - abort the task
                        warn!(
                            "Drain timeout ({}ms) reached for connection {}, aborting",
                            self.drain_timeout_ms, connection_id.0
                        );
                        managed.event_listener_handle.abort();
                    }
                }
            }

            // Stop the adapter
            if let Err(e) = managed.adapter.stop().await {
                warn!(
                    "Error stopping adapter for connection {}: {}",
                    connection_id.0, e
                );
                // Continue with removal even if stop fails
            }

            info!("Adapter stopped for connection {}", connection_id.0);
            Ok(())
        } else {
            warn!("Adapter not found for connection {}", connection_id.0);
            // Not an error - adapter might have already been stopped
            Ok(())
        }
    }

    /// Get an adapter by connection ID
    ///
    /// Returns the adapter reference if found
    pub async fn get_adapter(&self, connection_id: &ConnectionId) -> Option<DapAdapterRef> {
        self.adapters
            .get(connection_id)
            .map(|r| Arc::clone(&r.adapter))
    }

    /// Check if an adapter is registered for a connection
    pub async fn has_adapter(&self, connection_id: &ConnectionId) -> bool {
        self.adapters.contains_key(connection_id)
    }

    /// Check if a connection is in SafeMode
    ///
    /// Returns `Some(true)` if the connection is in SafeMode,
    /// `Some(false)` if not in SafeMode, or `None` if no adapter is registered.
    pub fn is_safe_mode(&self, connection_id: &ConnectionId) -> Option<bool> {
        self.adapters.get(connection_id).map(|r| r.safe_mode)
    }

    /// Update SafeMode for an existing connection
    ///
    /// Updates both the in-memory adapter state and persists to the database.
    /// Database is updated FIRST to ensure consistency (if DB fails, in-memory stays unchanged).
    ///
    /// # Returns
    /// - `Ok(true)` if updated successfully
    /// - `Ok(false)` if adapter not registered (no-op)
    /// - `Err` on database error during persistence
    ///
    /// # Race Condition Handling
    /// If the adapter is removed during the async DB operation, the DB update still
    /// completes (which is the authoritative state), and we log a warning. This is
    /// acceptable because the adapter is gone anyway.
    #[instrument(skip(self), fields(connection_id = %connection_id.0, safe_mode = safe_mode))]
    pub async fn update_safe_mode(
        &self,
        connection_id: &ConnectionId,
        safe_mode: bool,
    ) -> Result<bool> {
        // Check adapter exists first (without modifying)
        // Note: Due to async operations, adapter could be removed between this check
        // and the final update. This is handled below with a warning.
        if !self.adapters.contains_key(connection_id) {
            warn!(
                "Cannot update safe_mode for connection {}: adapter not found",
                connection_id.0
            );
            return Ok(false);
        }

        // Persist to database FIRST (fail fast - if DB fails, in-memory stays unchanged)
        if let Some(mut connection) = self.connection_repository.find_by_id(connection_id).await? {
            connection.safe_mode = safe_mode;
            self.connection_repository.update(&connection).await?;
        } else {
            // Connection not in database - unusual state, log warning but continue
            // (update in-memory anyway since caller expects the change to take effect)
            warn!(
                "Connection {} has adapter but not in database (updating in-memory only)",
                connection_id.0
            );
        }

        // Update in-memory AFTER successful DB write
        // Handle race condition: adapter could have been removed during async DB operation
        if let Some(mut entry) = self.adapters.get_mut(connection_id) {
            entry.safe_mode = safe_mode;
            info!(
                "Updated safe_mode={} for connection {}",
                safe_mode, connection_id.0
            );
        } else {
            // Adapter was removed during the DB operation - this is a race condition
            // but acceptable: DB is updated (authoritative state), adapter is gone
            warn!(
                "Adapter for connection {} was removed during safe_mode update \
                 (DB updated, in-memory update skipped)",
                connection_id.0
            );
        }

        Ok(true)
    }

    /// List all managed adapters with their status
    pub fn list_adapters(&self) -> Vec<ManagedAdapterInfo> {
        self.adapters
            .iter()
            .map(|r| ManagedAdapterInfo {
                connection_id: r.key().clone(),
                status: r.value().status,
                started_at: r.value().started_at,
                is_connected: r.value().adapter.is_connected(),
                safe_mode: r.value().safe_mode,
            })
            .collect()
    }

    /// Get count of active adapters
    pub async fn adapter_count(&self) -> usize {
        self.adapters.len()
    }

    /// Check if any adapter is connected
    ///
    /// Returns true if at least one adapter is connected to its debugger
    pub async fn has_connected_adapters(&self) -> bool {
        self.adapters
            .iter()
            .any(|r| r.value().adapter.is_connected())
    }

    /// Stop all adapters (for graceful shutdown)
    #[instrument(skip(self))]
    pub async fn stop_all(&self) -> Result<()> {
        info!("Stopping all adapters...");

        // Collect connection IDs first to avoid holding the lock while stopping
        let connection_ids: Vec<ConnectionId> =
            self.adapters.iter().map(|r| r.key().clone()).collect();

        for conn_id in connection_ids {
            if let Err(e) = self.stop_adapter(&conn_id).await {
                error!("Error stopping adapter {}: {}", conn_id.0, e);
                // Continue stopping other adapters
            }
        }

        // Flush any remaining buffered events
        self.flush_events().await;

        // Stop the flush task
        if let Some(handle) = self.flush_task_handle.lock().await.take() {
            handle.abort();
        }

        // Stop the cleanup task
        if let Some(handle) = self.cleanup_task_handle.lock().await.take() {
            handle.abort();
        }

        info!("All adapters stopped");
        Ok(())
    }

    /// Subscribe to the event broadcast channel
    ///
    /// Use this to receive real-time events from all adapters
    pub fn subscribe_events(&self) -> broadcast::Receiver<MetricEvent> {
        self.event_broadcast_tx.subscribe()
    }
}

impl std::fmt::Debug for AdapterLifecycleManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterLifecycleManager")
            .field("adapter_count", &"<async>")
            .finish()
    }
}
