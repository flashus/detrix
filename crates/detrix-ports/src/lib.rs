//! Port trait definitions for Detrix
//!
//! This crate contains all port (interface) traits following Clean Architecture.
//! Infrastructure crates implement these traits, application layer uses them.
//!
//! # Architecture
//!
//! Per Uncle Bob's Clean Architecture, output ports belong in the Use Case layer
//! (application layer), not the Domain layer. This crate extracts port definitions
//! to break circular dependencies between application and testing crates.
//!
//! # Port Types
//!
//! - **Repository ports**: Data persistence abstractions
//! - **Adapter ports**: External system integrations (DAP, etc.)
//! - **Cache ports**: Caching abstractions (purity cache, etc.)
//! - **Output ports**: Event streaming to external systems (Graylog, etc.)
//! - **Anchor ports**: Metric location tracking across code changes

mod adapter;
mod anchor;
mod cache;
mod dlq;
mod file_watcher;
mod mcp_usage;
mod output;
mod purity;
mod repository;

pub use adapter::{
    BatchRemoveResult, BatchSetResult, DapAdapter, DapAdapterFactory, DapAdapterFactoryRef,
    DapAdapterRef, FailedMetric, RemoveMetricResult, SetMetricResult, ToggleMetricResult,
};
pub use anchor::{AnchorService, AnchorServiceRef, SourceContext, SymbolInfo};
pub use cache::{PurityCache, PurityCacheEntry, PurityCacheKey, PurityCacheStats};
pub use detrix_config::constants::{DEFAULT_BATCH_CONCURRENCY, DEFAULT_BATCH_THRESHOLD};
pub use dlq::{DlqEntry, DlqEntryStatus, DlqRepository, ParseDlqEntryStatusError};
pub use file_watcher::{
    FileEvent, FileWatcher, FileWatcherConfig, FileWatcherRef, NullFileWatcher,
};
pub use mcp_usage::{ErrorCountRow, McpUsageRepository, ToolCountRow, UsageStats};
pub use output::{EventOutput, NullOutput, OutputStats};
pub use purity::PurityAnalyzer;
pub use repository::{
    ConnectionRepository, EventRepository, GroupSummary, MetricFilter, MetricRepository,
    SystemEventRepository,
};

// Type aliases for convenience
use std::sync::Arc;

/// Thread-safe reference to a metric repository
pub type MetricRepositoryRef = Arc<dyn MetricRepository + Send + Sync>;

/// Thread-safe reference to an event repository
pub type EventRepositoryRef = Arc<dyn EventRepository + Send + Sync>;

/// Thread-safe reference to a connection repository
pub type ConnectionRepositoryRef = Arc<dyn ConnectionRepository + Send + Sync>;

/// Thread-safe reference to a purity analyzer
pub type PurityAnalyzerRef = Arc<dyn PurityAnalyzer + Send + Sync>;

/// Thread-safe reference to a purity cache
pub type PurityCacheRef = Arc<dyn PurityCache + Send + Sync>;

/// Thread-safe reference to an event output
pub type EventOutputRef = Arc<dyn EventOutput + Send + Sync>;

/// Thread-safe reference to a system event repository
pub type SystemEventRepositoryRef = Arc<dyn SystemEventRepository + Send + Sync>;

/// Thread-safe reference to an MCP usage repository
pub type McpUsageRepositoryRef = Arc<dyn McpUsageRepository + Send + Sync>;

/// Thread-safe reference to a dead-letter queue repository
pub type DlqRepositoryRef = Arc<dyn DlqRepository + Send + Sync>;
