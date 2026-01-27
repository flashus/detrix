//! Shared application state for API services

use crate::mcp_client_tracker::{McpClientSummary, McpClientTracker, McpTrackerConfig};
use chrono::{DateTime, Utc};
use detrix_application::{
    AppContext, ConfigService, EventRepositoryRef, McpUsageRepositoryRef, SystemEventRepositoryRef,
};
use detrix_config::Config;
use detrix_core::{MetricEvent, SystemEvent};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;

/// Builder for ApiState
///
/// # Example
/// ```ignore
/// let state = ApiState::builder(context, event_repository)
///     .full_config(config)
///     .mcp_spawned(true)
///     .config_path(Some(path))
///     .system_event_repository(sys_repo)
///     .mcp_usage_repository(usage_repo)
///     .build();
/// ```
pub struct ApiStateBuilder {
    context: AppContext,
    event_repository: EventRepositoryRef,
    config: Option<Config>,
    config_path: Option<PathBuf>,
    mcp_spawned: bool,
    system_event_repository: Option<SystemEventRepositoryRef>,
    mcp_usage_repository: Option<McpUsageRepositoryRef>,
}

impl ApiStateBuilder {
    /// Create a new builder with required dependencies
    pub fn new(context: AppContext, event_repository: EventRepositoryRef) -> Self {
        Self {
            context,
            event_repository,
            config: None,
            config_path: None,
            mcp_spawned: false,
            system_event_repository: None,
            mcp_usage_repository: None,
        }
    }

    /// Set the full configuration
    ///
    /// Use this when you have a complete Config loaded from a file.
    pub fn full_config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    /// Set the config file path for persistence
    ///
    /// Used by ConfigService to save configuration changes.
    pub fn config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Set MCP spawned mode
    ///
    /// When true, the daemon will auto-shutdown when all MCP clients disconnect.
    pub fn mcp_spawned(mut self, spawned: bool) -> Self {
        self.mcp_spawned = spawned;
        self
    }

    /// Set the system event repository
    ///
    /// Enables system event querying in MCP tools.
    pub fn system_event_repository(mut self, repository: SystemEventRepositoryRef) -> Self {
        self.system_event_repository = Some(repository);
        self
    }

    /// Set the MCP usage repository
    ///
    /// Enables MCP usage tracking persistence.
    pub fn mcp_usage_repository(mut self, repository: McpUsageRepositoryRef) -> Self {
        self.mcp_usage_repository = Some(repository);
        self
    }

    /// Build the ApiState
    pub fn build(self) -> ApiState {
        let config = self.config.unwrap_or_default();

        // CRITICAL: Use the same broadcast channels as AppContext
        // This ensures events from adapters reach API subscribers
        let event_tx = self.context.streaming_service.event_sender();
        let system_event_tx = self.context.streaming_service.system_event_sender();

        // Create MCP client tracker with config
        let mcp_tracker_config = McpTrackerConfig::from(&config.mcp);
        let mcp_client_tracker = Arc::new(McpClientTracker::with_config(
            self.mcp_spawned,
            mcp_tracker_config,
        ));

        // Create ConfigService for runtime config management
        let config_service = Arc::new(ConfigService::new(config.clone(), self.config_path));

        ApiState {
            context: self.context,
            event_tx,
            system_event_tx,
            start_time: Instant::now(),
            start_time_utc: Utc::now(),
            event_repository: self.event_repository,
            system_event_repository: self.system_event_repository,
            mcp_client_tracker,
            config_service,
            mcp_usage_repository: self.mcp_usage_repository,
        }
    }
}

/// Shared state across all API endpoints (gRPC, REST, WebSocket)
///
/// Following Clean Architecture, this state wraps the shared AppContext
/// and adds API-specific fields (broadcast channels, etc.)
///
/// NOTE: This state does NOT contain a singleton adapter. All adapters are managed
/// through AdapterLifecycleManager in AppContext. Use ConnectionService to create
/// connections which will start adapters via AdapterLifecycleManager.
///
/// Config is accessed through `config_service` for hot-reload support.
/// Use `config_service.get_config().await` to get current configuration.
#[derive(Clone)]
pub struct ApiState {
    /// Shared application context (services)
    pub context: AppContext,

    /// Broadcast channel for real-time metric event streaming
    /// Subscribers can listen to all events or filter by metric_id
    pub event_tx: broadcast::Sender<MetricEvent>,

    /// Broadcast channel for system events (crashes, connections, metric CRUD)
    /// Used by MCP subscribe_events tool for real-time streaming
    pub system_event_tx: broadcast::Sender<SystemEvent>,

    /// Server start time for uptime calculation (monotonic)
    pub start_time: Instant,

    /// Server start time (wall clock, for display)
    pub start_time_utc: DateTime<Utc>,

    /// Event repository for direct event queries (trait object, not concrete type)
    /// Following Clean Architecture - API layer depends on abstractions, not implementations
    pub event_repository: EventRepositoryRef,

    /// System event repository for querying system events (crashes, connections, metric CRUD)
    /// Set via `with_system_event_repository()` builder method.
    /// When None, query tools return "not configured" message.
    pub system_event_repository: Option<SystemEventRepositoryRef>,

    /// MCP client tracker for daemon lifecycle management
    /// Tracks active MCP bridge clients and signals shutdown when all disconnect
    pub mcp_client_tracker: Arc<McpClientTracker>,

    /// Configuration service for runtime config management
    /// Handles config updates, validation, and persistence
    pub config_service: Arc<ConfigService>,

    /// MCP usage repository for persisting tool usage analytics.
    /// Set via `with_mcp_usage_repository()` builder method.
    /// When None, usage analytics are not persisted.
    pub mcp_usage_repository: Option<McpUsageRepositoryRef>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState")
            .field("context", &self.context)
            .field("event_tx", &"<broadcast::Sender<MetricEvent>>")
            .field("system_event_tx", &"<broadcast::Sender<SystemEvent>>")
            .field("start_time", &self.start_time)
            .field("event_repository", &"<EventRepositoryRef>")
            .field(
                "system_event_repository",
                &self
                    .system_event_repository
                    .as_ref()
                    .map(|_| "<SystemEventRepositoryRef>"),
            )
            .field("config_service", &self.config_service)
            .field(
                "mcp_usage_repository",
                &self
                    .mcp_usage_repository
                    .as_ref()
                    .map(|_| "<McpUsageRepositoryRef>"),
            )
            .finish()
    }
}

impl ApiState {
    /// Create a new builder for ApiState
    ///
    /// # Example
    /// ```ignore
    /// let state = ApiState::builder(context, event_repository)
    ///     .full_config(config)
    ///     .mcp_spawned(true)
    ///     .build();
    /// ```
    pub fn builder(context: AppContext, event_repository: EventRepositoryRef) -> ApiStateBuilder {
        ApiStateBuilder::new(context, event_repository)
    }

    /// Get the shutdown receiver for MCP-spawned daemon auto-shutdown
    pub fn mcp_shutdown_receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.mcp_client_tracker.shutdown_receiver()
    }

    /// Start the MCP client cleanup task (should be called in serve command)
    pub fn start_mcp_cleanup_task(&self) -> tokio::task::JoinHandle<()> {
        self.mcp_client_tracker.start_cleanup_task()
    }

    /// Subscribe to all events
    pub fn subscribe_events(&self) -> broadcast::Receiver<MetricEvent> {
        self.event_tx.subscribe()
    }

    /// Publish an event to all subscribers
    pub fn publish_event(
        &self,
        event: MetricEvent,
    ) -> Result<usize, broadcast::error::SendError<MetricEvent>> {
        self.event_tx.send(event)
    }

    /// Subscribe to system events (crashes, connections, metric CRUD)
    pub fn subscribe_system_events(&self) -> broadcast::Receiver<SystemEvent> {
        self.system_event_tx.subscribe()
    }

    /// Publish a system event to all subscribers
    pub fn publish_system_event(
        &self,
        event: SystemEvent,
    ) -> Result<usize, broadcast::error::SendError<SystemEvent>> {
        self.system_event_tx.send(event)
    }

    /// Get uptime in seconds
    pub fn uptime_seconds(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    /// Get started timestamp as ISO 8601 string
    pub fn started_at(&self) -> String {
        self.start_time_utc.to_rfc3339()
    }

    /// Get connected MCP clients
    pub async fn get_mcp_clients(&self) -> Vec<McpClientSummary> {
        self.mcp_client_tracker.get_clients().await
    }

    /// Get current process info (PID, parent PID)
    pub fn get_daemon_info(&self) -> DaemonInfo {
        DaemonInfo::current()
    }
}

/// Information about the daemon process
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInfo {
    /// Process ID
    pub pid: u32,
    /// Parent process ID
    pub ppid: u32,
}

impl DaemonInfo {
    /// Get info for the current process
    pub fn current() -> Self {
        Self {
            pid: std::process::id(),
            ppid: get_parent_pid(),
        }
    }
}

/// Get parent process ID
#[cfg(unix)]
fn get_parent_pid() -> u32 {
    // SAFETY: getppid() is always safe to call
    unsafe { libc::getppid() as u32 }
}

#[cfg(not(unix))]
fn get_parent_pid() -> u32 {
    0 // Not supported on non-Unix platforms
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_application::{
        ConnectionRepositoryRef, DapAdapterFactoryRef, EventRepositoryRef, MetricRepositoryRef,
    };
    use detrix_config::ApiConfig;
    use detrix_storage::{SqliteConfig, SqliteStorage};
    use detrix_testing::MockDapAdapterFactory;
    use tempfile::TempDir;

    async fn create_test_context() -> (AppContext, EventRepositoryRef, TempDir) {
        // Use real SQLite for integration tests (tests that verify ApiState integration)
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let sqlite_config = SqliteConfig {
            path: db_path,
            pool_size: 1,
            busy_timeout_ms: 5000,
        };

        let storage = Arc::new(SqliteStorage::new(&sqlite_config).await.unwrap());
        let mock_factory = Arc::new(MockDapAdapterFactory::new());

        let context = AppContext::new(
            Arc::clone(&storage) as MetricRepositoryRef,
            Arc::clone(&storage) as EventRepositoryRef,
            Arc::clone(&storage) as ConnectionRepositoryRef,
            mock_factory as DapAdapterFactoryRef,
            &ApiConfig::default(),
            &detrix_config::SafetyConfig::default(),
            &detrix_config::StorageConfig::default(),
            &detrix_config::DaemonConfig::default(),
            &detrix_config::AdapterConnectionConfig::default(),
            &detrix_config::AnchorConfig::default(),
            &detrix_config::LimitsConfig::default(),
            None,
            None, // No separate DLQ storage in tests
        );

        (context, storage as EventRepositoryRef, temp_dir)
    }

    #[tokio::test]
    async fn test_from_context() {
        let (context, event_repo, _temp_dir) = create_test_context().await;

        let state = ApiState::builder(context, event_repo).build();
        assert_eq!(state.uptime_seconds(), 0);
    }

    #[tokio::test]
    async fn test_event_broadcast() {
        let (context, event_repo, _temp_dir) = create_test_context().await;

        let state = ApiState::builder(context, event_repo).build();

        // Subscribe to events
        let mut rx = state.subscribe_events();

        // Publish an event
        let event = MetricEvent::new(
            detrix_core::MetricId(1),
            "test_metric".to_string(),
            detrix_core::ConnectionId::from("test"),
            "{}".to_string(),
        );

        let count = state.publish_event(event.clone()).unwrap();
        assert_eq!(count, 1);

        // Receive the event
        let received = rx.try_recv().unwrap();
        assert_eq!(received.metric_id, event.metric_id);
    }

    /// Test that ApiState uses the same broadcast channel as AppContext
    /// This is the critical test for Issue 2.1 - events from adapters should
    /// be received by ApiState subscribers
    #[tokio::test]
    async fn test_api_state_shares_broadcast_channel_with_context() {
        let (context, event_repo, _temp_dir) = create_test_context().await;

        // Get a sender from the context's streaming service (this is what adapters use)
        let context_sender = context.streaming_service.event_sender();

        let state = ApiState::builder(context, event_repo).build();

        // Subscribe via ApiState
        let mut rx = state.subscribe_events();

        // Publish via the context's channel (simulating what adapters do)
        let event = MetricEvent::new(
            detrix_core::MetricId(42),
            "adapter_event".to_string(),
            detrix_core::ConnectionId::from("test-conn"),
            r#"{"value": 123}"#.to_string(),
        );

        // This should be received by ApiState subscriber
        context_sender.send(event.clone()).unwrap();

        // The test will FAIL if ApiState creates its own channel
        let received = rx
            .try_recv()
            .expect("ApiState should receive events from AppContext's broadcast channel");
        assert_eq!(received.metric_id.0, 42);
        assert_eq!(received.metric_name, "adapter_event");
    }

    /// Test that subscribing from AppContext receives events published via ApiState
    ///
    /// This confirms the shared broadcast channel architecture:
    /// - ApiState.event_tx is the SAME channel as StreamingService.event_sender()
    /// - Events published anywhere on this channel reach all subscribers
    /// - No republisher task is needed - the channel is shared by design
    #[tokio::test]
    async fn test_context_receives_api_state_events() {
        let (context, event_repo, _temp_dir) = create_test_context().await;

        // Subscribe via context before creating ApiState
        let mut context_rx = context.subscribe_events();

        let state = ApiState::builder(context, event_repo).build();

        // Publish via ApiState
        let event = MetricEvent::new(
            detrix_core::MetricId(99),
            "api_event".to_string(),
            detrix_core::ConnectionId::from("api"),
            r#"{"source": "api"}"#.to_string(),
        );

        state.publish_event(event.clone()).unwrap();

        // Context subscriber should receive it (shared channel)
        let received = context_rx
            .try_recv()
            .expect("AppContext subscriber should receive events published via ApiState");
        assert_eq!(received.metric_id.0, 99);
    }
}
