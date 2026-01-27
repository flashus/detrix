//! Storage configuration for SQLite and event batching

use crate::constants::{
    DEFAULT_BUSY_TIMEOUT_MS, DEFAULT_DLQ_BATCH_SIZE, DEFAULT_DLQ_BUSY_TIMEOUT_MS,
    DEFAULT_DLQ_MAX_RETRIES, DEFAULT_DLQ_POOL_SIZE, DEFAULT_DLQ_RETRY_INTERVAL_MS,
    DEFAULT_EVENT_BATCH_SIZE, DEFAULT_EVENT_FLUSH_INTERVAL_MS, DEFAULT_MAX_EVENTS_PER_METRIC,
    DEFAULT_POOL_SIZE, DEFAULT_RETENTION_HOURS, DEFAULT_STORAGE_TYPE,
    DEFAULT_SYSTEM_EVENT_CLEANUP_INTERVAL_SECS, DEFAULT_SYSTEM_EVENT_MAX_EVENTS,
    DEFAULT_SYSTEM_EVENT_RETENTION_HOURS,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ============================================================================
// Storage Config
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Storage backend type: "sqlite" (default), "postgres" (planned)
    ///
    /// NOTE: Currently only SQLite is implemented. Postgres support is planned
    /// for a future release. This field is reserved for forward compatibility.
    #[serde(default = "default_storage_type")]
    pub storage_type: String,
    #[serde(default = "default_storage_path")]
    pub path: PathBuf,
    #[serde(default = "default_max_events")]
    pub max_events_per_metric: usize,
    #[serde(default = "default_retention_hours")]
    pub retention_hours: u32,
    /// SQLite connection pool size
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,
    /// SQLite busy timeout in milliseconds
    #[serde(default = "default_busy_timeout_ms")]
    pub busy_timeout_ms: u64,
    /// Event batching configuration
    #[serde(default)]
    pub event_batching: EventBatchingConfig,
    /// System event retention configuration
    #[serde(default)]
    pub system_event_retention: SystemEventRetentionConfig,
    /// DLQ storage configuration (separate database from main storage)
    #[serde(default)]
    pub dlq_storage: DlqStorageConfig,
}

fn default_storage_type() -> String {
    DEFAULT_STORAGE_TYPE.to_string()
}

fn default_storage_path() -> PathBuf {
    crate::paths::default_database_path()
}

fn default_max_events() -> usize {
    DEFAULT_MAX_EVENTS_PER_METRIC
}

fn default_retention_hours() -> u32 {
    DEFAULT_RETENTION_HOURS
}

fn default_pool_size() -> u32 {
    DEFAULT_POOL_SIZE
}

fn default_busy_timeout_ms() -> u64 {
    DEFAULT_BUSY_TIMEOUT_MS
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            storage_type: default_storage_type(),
            path: default_storage_path(),
            max_events_per_metric: default_max_events(),
            retention_hours: default_retention_hours(),
            pool_size: default_pool_size(),
            busy_timeout_ms: default_busy_timeout_ms(),
            event_batching: EventBatchingConfig::default(),
            system_event_retention: SystemEventRetentionConfig::default(),
            dlq_storage: DlqStorageConfig::default(),
        }
    }
}

// ============================================================================
// Event Batching Config
// ============================================================================

/// Event batching configuration for high-throughput scenarios
///
/// NOTE: Batching is always enabled for optimal performance.
/// Backpressure is handled via channel capacity, not buffer limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBatchingConfig {
    /// Maximum events to buffer before flushing (default: 100)
    #[serde(default = "default_event_batch_size")]
    pub batch_size: usize,
    /// Maximum time to buffer events in milliseconds (default: 100ms)
    #[serde(default = "default_event_flush_interval_ms")]
    pub flush_interval_ms: u64,
    /// Dead-letter queue configuration (PERF-01 audit finding)
    #[serde(default)]
    pub dlq: DlqConfig,
}

// ============================================================================
// Dead-Letter Queue Storage Config
// ============================================================================

/// Backend type for DLQ storage
///
/// DLQ uses a separate database from main storage to provide true isolation.
/// If the main database has issues, DLQ can still capture failed events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DlqBackend {
    /// Separate SQLite file (default: "./detrix-dlq.db")
    #[default]
    SqliteFile,
    /// In-memory SQLite - data lost on restart, useful for testing
    SqliteMemory,
    // Future: Postgres (behind feature flag)
}

/// DLQ storage configuration - separate from main storage
///
/// This configures WHERE the DLQ stores data. For behavioral config
/// (retry intervals, max retries), see `DlqConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlqStorageConfig {
    /// Backend type for DLQ storage
    #[serde(default)]
    pub backend: DlqBackend,
    /// Path for SQLite file backend (relative to config dir)
    #[serde(default = "default_dlq_storage_path")]
    pub path: PathBuf,
    /// SQLite connection pool size (smaller than main storage)
    #[serde(default = "default_dlq_pool_size")]
    pub pool_size: u32,
    /// SQLite busy timeout in milliseconds
    #[serde(default = "default_dlq_busy_timeout_ms")]
    pub busy_timeout_ms: u64,
}

fn default_dlq_storage_path() -> PathBuf {
    crate::paths::detrix_home().join("dlq.db")
}

fn default_dlq_pool_size() -> u32 {
    DEFAULT_DLQ_POOL_SIZE
}

fn default_dlq_busy_timeout_ms() -> u64 {
    DEFAULT_DLQ_BUSY_TIMEOUT_MS
}

impl Default for DlqStorageConfig {
    fn default() -> Self {
        Self {
            backend: DlqBackend::default(),
            path: default_dlq_storage_path(),
            pool_size: default_dlq_pool_size(),
            busy_timeout_ms: default_dlq_busy_timeout_ms(),
        }
    }
}

// ============================================================================
// Dead-Letter Queue Behavior Config
// ============================================================================

/// Dead-letter queue behavioral configuration for handling failed event flushes
///
/// This configures HOW the DLQ behaves (retry logic). For storage config
/// (database location), see `DlqStorageConfig` in `StorageConfig.dlq_storage`.
///
/// When event batches fail to persist to the database, they are saved to a
/// dead-letter queue for later retry. This prevents permanent data loss
/// during transient database failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlqConfig {
    /// Enable dead-letter queue (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Interval between retry attempts in milliseconds (default: 30000 = 30 seconds)
    #[serde(default = "default_dlq_retry_interval_ms")]
    pub retry_interval_ms: u64,
    /// Maximum retry attempts before marking as permanently failed (default: 3)
    #[serde(default = "default_dlq_max_retries")]
    pub max_retries: u32,
    /// Number of events to process per retry batch (default: 50)
    #[serde(default = "default_dlq_batch_size")]
    pub batch_size: usize,
}

fn default_dlq_retry_interval_ms() -> u64 {
    DEFAULT_DLQ_RETRY_INTERVAL_MS
}

fn default_dlq_max_retries() -> u32 {
    DEFAULT_DLQ_MAX_RETRIES
}

fn default_dlq_batch_size() -> usize {
    DEFAULT_DLQ_BATCH_SIZE
}

impl Default for DlqConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            retry_interval_ms: default_dlq_retry_interval_ms(),
            max_retries: default_dlq_max_retries(),
            batch_size: default_dlq_batch_size(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_event_batch_size() -> usize {
    DEFAULT_EVENT_BATCH_SIZE
}

fn default_event_flush_interval_ms() -> u64 {
    DEFAULT_EVENT_FLUSH_INTERVAL_MS
}

impl Default for EventBatchingConfig {
    fn default() -> Self {
        Self {
            batch_size: default_event_batch_size(),
            flush_interval_ms: default_event_flush_interval_ms(),
            dlq: DlqConfig::default(),
        }
    }
}

// ============================================================================
// System Event Retention Config
// ============================================================================

/// System event retention configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEventRetentionConfig {
    /// Retention period in hours (default: 168 = 7 days)
    #[serde(default = "default_system_event_retention_hours")]
    pub retention_hours: u32,
    /// Cleanup interval in seconds (default: 3600 = 1 hour)
    #[serde(default = "default_system_event_cleanup_interval_secs")]
    pub cleanup_interval_secs: u64,
    /// Maximum events to keep (0 = unlimited, default: 100000)
    #[serde(default = "default_system_event_max_events")]
    pub max_events: usize,
}

fn default_system_event_retention_hours() -> u32 {
    DEFAULT_SYSTEM_EVENT_RETENTION_HOURS
}

fn default_system_event_cleanup_interval_secs() -> u64 {
    DEFAULT_SYSTEM_EVENT_CLEANUP_INTERVAL_SECS
}

fn default_system_event_max_events() -> usize {
    DEFAULT_SYSTEM_EVENT_MAX_EVENTS
}

impl Default for SystemEventRetentionConfig {
    fn default() -> Self {
        Self {
            retention_hours: default_system_event_retention_hours(),
            cleanup_interval_secs: default_system_event_cleanup_interval_secs(),
            max_events: default_system_event_max_events(),
        }
    }
}
