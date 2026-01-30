//! Shared application context (protocol-agnostic)
//!
//! Contains core services used by all presentation layers (API, CLI).
//! This follows the Dependency Inversion principle - both API and CLI
//! depend on the Application Layer abstraction, not on each other.
//!
//! ## Config Invariant
//!
//! **AppContext does NOT store a Config reference.** Config values are read at
//! construction time and used to configure services. Once created, AppContext's
//! internal service configuration is immutable.
//!
//! For runtime config access (hot-reload support), use `ConfigService` in the
//! API layer (ApiState.config_service). AppContext is designed to be lightweight
//! and not depend on config management infrastructure.
//!
//! Config changes that affect AppContext services require server restart:
//! - `api.rest.port`, `api.grpc.port` - server bind addresses
//! - `storage.path` - database location
//! - `api.auth` - authentication configuration
//! - `adapter.*` - adapter connection timeouts

use crate::ports::{
    ConnectionRepositoryRef, DapAdapterFactoryRef, DlqRepositoryRef, EventOutputRef,
    EventRepositoryRef, MetricRepositoryRef,
};
use crate::safety::ValidatorRegistry;
use crate::services::{
    AdapterLifecycleManager, AnchorServiceConfig, ConnectionService, DefaultAnchorService,
    EventCaptureService, McpUsageService, MetricService, StreamingService,
};
use detrix_config::{
    AdapterConnectionConfig, AnchorConfig, ApiConfig, DaemonConfig, LimitsConfig, SafetyConfig,
    StorageConfig,
};
use detrix_core::{MetricEvent, SystemEvent};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Shared application context containing core services
///
/// This context is used by both API (detrix-api) and CLI (detrix-cli)
/// to ensure consistent service composition without coupling the presentation layers.
///
/// All adapters are managed through the AdapterLifecycleManager - there is no singleton adapter.
#[derive(Clone)]
pub struct AppContext {
    /// Metric management service (protocol-agnostic)
    pub metric_service: Arc<MetricService>,

    /// Streaming service (protocol-agnostic)
    pub streaming_service: Arc<StreamingService>,

    /// Event capture service (protocol-agnostic)
    pub event_capture_service: Arc<EventCaptureService>,

    /// Connection management service (protocol-agnostic)
    pub connection_service: Arc<ConnectionService>,

    /// Adapter lifecycle manager (protocol-agnostic)
    pub adapter_lifecycle_manager: Arc<AdapterLifecycleManager>,

    /// MCP usage tracking service for tool usage analytics
    pub mcp_usage: Arc<McpUsageService>,
}

impl AppContext {
    /// Create new application context
    ///
    /// # Arguments
    /// * `metric_storage` - Metric repository implementation (trait object)
    /// * `event_storage` - Event repository implementation (trait object)
    /// * `connection_repo` - Connection repository implementation (trait object)
    /// * `adapter_factory` - Factory for creating language-specific adapters
    /// * `api_config` - API configuration
    /// * `safety_config` - Safety validation configuration
    /// * `storage_config` - Storage configuration (includes event batching)
    /// * `daemon_config` - Daemon configuration (includes drain timeout)
    /// * `adapter_config` - Adapter connection configuration (timeouts, batching)
    /// * `anchor_config` - Anchor configuration for metric location tracking
    /// * `limits_config` - Limits configuration (max metrics, expression length)
    /// * `output` - Optional event output (e.g., Graylog/GELF)
    /// * `dlq_repo` - Optional dead-letter queue repository (separate from main storage)
    ///
    /// Note: In practice, `metric_storage` and `event_storage` often point to the
    /// same underlying storage (e.g., SqliteStorage), but they're separate parameters
    /// to allow flexibility and proper trait object typing.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        metric_storage: MetricRepositoryRef,
        event_storage: EventRepositoryRef,
        connection_repo: ConnectionRepositoryRef,
        adapter_factory: DapAdapterFactoryRef,
        api_config: &ApiConfig,
        safety_config: &SafetyConfig,
        storage_config: &StorageConfig,
        daemon_config: &DaemonConfig,
        adapter_config: &AdapterConnectionConfig,
        anchor_config: &AnchorConfig,
        limits_config: &LimitsConfig,
        output: Option<EventOutputRef>,
        dlq_repo: Option<DlqRepositoryRef>,
    ) -> Self {
        // Create broadcast channels for real-time events
        let (event_tx, _) = broadcast::channel::<MetricEvent>(api_config.event_buffer_capacity);
        let (system_event_tx, _) =
            broadcast::channel::<SystemEvent>(api_config.event_buffer_capacity);

        // Create validator registry from safety config
        let validators = ValidatorRegistry::new(safety_config);

        // Create the EventCaptureService (needed by AdapterLifecycleManager)
        // Use separate DLQ storage if provided for true isolation
        let event_capture_service = Arc::new(match dlq_repo {
            Some(dlq) => EventCaptureService::with_dlq(event_storage.clone(), dlq),
            None => EventCaptureService::new(event_storage.clone()),
        });

        // Create the AdapterLifecycleManager with event batching and adapter config
        let adapter_lifecycle_manager = Arc::new(AdapterLifecycleManager::with_config(
            Arc::clone(&event_capture_service),
            event_tx.clone(),
            system_event_tx.clone(),
            adapter_factory,
            metric_storage.clone(),
            Arc::clone(&connection_repo), // For updating status on crash
            storage_config.event_batching.clone(),
            adapter_config.clone(),
            daemon_config.drain_timeout_ms,
            output.clone(), // Pass GELF output to lifecycle manager
        ));

        // Create the ConnectionService with the lifecycle manager
        let connection_service = Arc::new(ConnectionService::new(
            Arc::clone(&connection_repo),
            Arc::clone(&adapter_lifecycle_manager),
            system_event_tx.clone(),
        ));

        // Create the MCP usage service for tool analytics
        let mcp_usage = Arc::new(McpUsageService::new());

        // Build StreamingService using builder pattern
        let mut streaming_builder =
            StreamingService::builder(event_storage, metric_storage.clone())
                .event_channel(event_tx)
                .system_event_channel(system_event_tx.clone())
                .api_config(api_config.clone());

        if let Some(out) = output {
            streaming_builder = streaming_builder.output(out);
        }

        // Create anchor service with config
        let anchor_service = Arc::new(DefaultAnchorService::with_config(
            AnchorServiceConfig::from(anchor_config),
        ));

        Self {
            metric_service: Arc::new(
                MetricService::builder(
                    metric_storage,
                    Arc::clone(&adapter_lifecycle_manager),
                    system_event_tx.clone(),
                )
                .validators(validators)
                .adapter_config(adapter_config.clone())
                .limits_config(limits_config.clone())
                .anchor_service(anchor_service)
                .build(),
            ),
            streaming_service: Arc::new(streaming_builder.build()),
            event_capture_service,
            connection_service,
            adapter_lifecycle_manager,
            mcp_usage,
        }
    }

    /// Create new application context with default config
    pub fn with_defaults(
        metric_storage: MetricRepositoryRef,
        event_storage: EventRepositoryRef,
        connection_repo: ConnectionRepositoryRef,
        adapter_factory: DapAdapterFactoryRef,
    ) -> Self {
        Self::new(
            metric_storage,
            event_storage,
            connection_repo,
            adapter_factory,
            &ApiConfig::default(),
            &SafetyConfig::default(),
            &StorageConfig::default(),
            &DaemonConfig::default(),
            &AdapterConnectionConfig::default(),
            &AnchorConfig::default(),
            &LimitsConfig::default(),
            None,
            None, // No separate DLQ storage
        )
    }

    /// Get a reference to the AdapterLifecycleManager
    pub fn adapter_lifecycle_manager(&self) -> &Arc<AdapterLifecycleManager> {
        &self.adapter_lifecycle_manager
    }

    /// Subscribe to the event broadcast channel
    ///
    /// Use this to receive real-time events from all adapters
    pub fn subscribe_events(&self) -> broadcast::Receiver<MetricEvent> {
        self.adapter_lifecycle_manager.subscribe_events()
    }

    /// Subscribe to the system event broadcast channel
    ///
    /// Use this to receive real-time system events (crashes, connections, metric CRUD)
    pub fn subscribe_system_events(&self) -> broadcast::Receiver<SystemEvent> {
        self.streaming_service.subscribe_to_system_events()
    }
}

impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContext")
            .field("metric_service", &self.metric_service)
            .field("streaming_service", &self.streaming_service)
            .field("event_capture_service", &self.event_capture_service)
            .field("mcp_usage", &"<McpUsageService>")
            .finish()
    }
}
