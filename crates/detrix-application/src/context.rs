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
    ConnectionReferenceRepositoryRef, ConnectionRepositoryRef, DapAdapterFactoryRef,
    DlqRepositoryRef, EventOutputRef, EventRepositoryRef, MetricRepositoryRef, RemoteAppControlRef,
};
use crate::safety::ValidatorRegistry;
use crate::services::{
    AdapterLifecycleManager, AgentConnectionManagerRef, AnchorServiceConfig, ConnectionService,
    DefaultAnchorService, EventCaptureService, FileInspectionService, FileSourceChain,
    McpUsageService, MetricService, RemoteAppService, StreamingService,
};
use detrix_config::{
    AdapterConnectionConfig, AnchorConfig, ApiConfig, DaemonConfig, LimitsConfig, SafetyConfig,
    StorageConfig,
};
use detrix_core::{MetricEvent, SourceLanguage, SystemEvent};
use detrix_ports::{PurityAnalyzerRef, VfsRef};
use std::collections::HashMap;
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

    /// Agent connection manager (optional — present when agent mode is configured)
    pub agent_connection_manager: Option<AgentConnectionManagerRef>,

    /// MCP usage tracking service for tool usage analytics
    pub mcp_usage: Arc<McpUsageService>,

    /// Remote app control service (optional — only when HTTP client is available)
    pub remote_app_service: Option<Arc<RemoteAppService>>,

    /// File inspection service for code analysis (shared VFS)
    pub file_inspection: FileInspectionService,

    /// Virtual File System reference (shared across services)
    pub vfs: VfsRef,

    /// File source chain for transparent file fetching (VFS cache → control_plane → bridge → disk)
    pub file_source_chain: Arc<FileSourceChain>,
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
    /// * `remote_control` - Optional remote app control implementation (HTTP client)
    /// * `auth_token` - Optional authentication token for remote app control (e.g. from DETRIX_TOKEN env var, read in CLI layer)
    /// * `vfs` - Virtual File System for source file access (cache + disk fallback)
    /// * `file_source_chain` - Pluggable file source chain for transparent remote file fetching
    /// * `purity_analyzers` - LSP purity analyzers per language (empty map = disabled)
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
        remote_control: Option<RemoteAppControlRef>,
        auth_token: Option<String>,
        vfs: VfsRef,
        file_source_chain: Arc<FileSourceChain>,
        reference_repo: ConnectionReferenceRepositoryRef,
        purity_analyzers: HashMap<SourceLanguage, PurityAnalyzerRef>,
        agent_manager: Option<AgentConnectionManagerRef>,
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
        let adapter_lifecycle_manager = Arc::new(AdapterLifecycleManager::with_agent_manager(
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
            Arc::clone(&vfs),
            agent_manager.clone(),
        ));

        // Create the ConnectionService with the lifecycle manager
        let connection_service = Arc::new(ConnectionService::new(
            Arc::clone(&connection_repo),
            metric_storage.clone(),
            reference_repo,
            Arc::clone(&adapter_lifecycle_manager),
            system_event_tx.clone(),
            Arc::clone(&vfs),
        ));

        // Create the MCP usage service for tool analytics
        let mcp_usage = Arc::new(McpUsageService::new());

        // Create the remote app service if a remote control impl is provided
        let remote_app_service = remote_control.map(|rc| {
            Arc::new(RemoteAppService::new(
                rc,
                Arc::clone(&connection_service) as detrix_ports::ConnectionLookupRef,
                auth_token,
            ))
        });

        // Build StreamingService using builder pattern
        let mut streaming_builder =
            StreamingService::builder(event_storage, metric_storage.clone())
                .event_channel(event_tx)
                .system_event_channel(system_event_tx.clone())
                .api_config(api_config.clone());

        if let Some(out) = output {
            streaming_builder = streaming_builder.output(out);
        }

        // Create anchor service with config and VFS (for cloud mode file access)
        let anchor_service = Arc::new(DefaultAnchorService::with_vfs(
            AnchorServiceConfig::from(anchor_config),
            Arc::clone(&vfs),
        ));

        // Create file inspection service with VFS
        let file_inspection = FileInspectionService::new(Arc::clone(&vfs));

        Self {
            metric_service: Arc::new(
                MetricService::builder(
                    metric_storage,
                    Arc::clone(&adapter_lifecycle_manager),
                    system_event_tx.clone(),
                )
                .validators(validators)
                .purity_analyzers(purity_analyzers)
                .adapter_config(adapter_config.clone())
                .limits_config(limits_config.clone())
                .anchor_service(anchor_service)
                .vfs(Arc::clone(&vfs))
                .build(),
            ),
            streaming_service: Arc::new(streaming_builder.build()),
            event_capture_service,
            connection_service,
            adapter_lifecycle_manager,
            agent_connection_manager: None, // Set via with_agent_connection_manager()
            mcp_usage,
            remote_app_service,
            file_inspection,
            vfs,
            file_source_chain,
        }
    }

    /// Set the agent connection manager on this context.
    /// Returns a new AppContext with the agent manager configured.
    pub fn with_agent_connection_manager(mut self, mgr: AgentConnectionManagerRef) -> Self {
        self.agent_connection_manager = Some(mgr);
        self
    }

    /// Create new application context with default config
    pub fn with_defaults(
        metric_storage: MetricRepositoryRef,
        event_storage: EventRepositoryRef,
        connection_repo: ConnectionRepositoryRef,
        adapter_factory: DapAdapterFactoryRef,
        vfs: VfsRef,
        file_source_chain: Arc<FileSourceChain>,
        reference_repo: ConnectionReferenceRepositoryRef,
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
            None, // No remote app control
            None, // No auth token
            vfs,
            file_source_chain,
            reference_repo,
            HashMap::new(), // No LSP purity analyzers
            None,           // No agent manager
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
            .field("vfs", &"<VFS>")
            .finish()
    }
}
