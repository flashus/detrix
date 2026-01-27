//! Protocol-agnostic business logic layer (Use Cases)
//!
//! This layer contains all business logic and is independent of
//! transport protocols (gRPC, REST, WebSocket). Controllers only
//! handle DTO mapping and delegate to these services.

pub mod adapter_lifecycle_manager;
pub mod anchor_service;
pub mod config_service;
pub mod connection_service;
pub mod dlq_recovery_service;
pub mod environment_service;
pub mod event_capture_service;
pub mod file_inspection_service;
pub mod file_inspection_types;
pub mod file_watcher_service;
pub mod jwt_validator;
pub mod mcp_usage_service;
pub mod metric_service;
pub mod shutdown;
pub mod streaming_service;
pub mod system_event_service;

pub use adapter_lifecycle_manager::{
    AdapterLifecycleManager, ManagedAdapterInfo, ManagedAdapterStatus, StartAdapterResult,
};
pub use anchor_service::{
    AnchorServiceConfig, DefaultAnchorService, LspSymbolLookup, LspSymbolLookupRef, NullLspLookup,
};
pub use config_service::{ConfigService, ConfigUpdateResult, RESTART_REQUIRED_FIELDS};
pub use connection_service::ConnectionService;
pub use environment_service::{EnvironmentCheckResult, EnvironmentService};
pub use event_capture_service::EventCaptureService;
pub use file_inspection_service::FileInspectionService;
pub use file_inspection_types::{
    CodeContext, CodeLine, FileInspectionRequest, FileInspectionResult, FileOverview,
    LanguageCapabilities, LineInspectionResult, SourceLanguage, TextSearchMatch,
    VariableDefinition, VariableSearchResult,
};
pub use file_watcher_service::{FileWatcherOrchestrator, NotifyFileWatcher};
pub use metric_service::{FileChangeResult, MetricService, MetricServiceBuilder};
pub use streaming_service::{
    output_dropped_events_count, StreamingService, StreamingServiceBuilder,
};
pub use system_event_service::SystemEventService;

// JWT validation for external auth mode
pub use jwt_validator::{Audience, JwksValidator, JwtClaims, JwtError};

// Dead-letter queue recovery
pub use dlq_recovery_service::{DlqRecoveryService, DlqStats};

// Graceful shutdown utilities
pub use shutdown::{GracefulShutdownHandle, ShutdownReceiver};
// Re-export drain timeout constant from config
pub use detrix_config::constants::DEFAULT_DRAIN_TIMEOUT_MS;

// MCP usage tracking
pub use mcp_usage_service::{
    CallTimer, ErrorCount, McpErrorCode, McpUsageCounters, McpUsageEvent, McpUsageService,
    SessionContext, UsageSnapshot, WorkflowStats,
};

#[cfg(test)]
mod adapter_lifecycle_manager_tests;
#[cfg(test)]
mod config_service_tests;
#[cfg(test)]
mod metric_service_tests;
#[cfg(test)]
mod streaming_service_tests;
