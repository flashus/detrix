//! Dead-Letter Queue repository port for failed event persistence
//!
//! When events fail to persist to the main event store, they are saved to
//! the dead-letter queue for retry. This prevents permanent data loss during
//! transient database failures.

use async_trait::async_trait;
use detrix_core::error::Result;
use std::str::FromStr;

/// Status of a dead-letter queue entry
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlqEntryStatus {
    /// Waiting for retry
    Pending,
    /// Currently being retried
    Retrying,
    /// Permanently failed (max retries exceeded)
    Failed,
}

impl DlqEntryStatus {
    /// Convert to database string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            DlqEntryStatus::Pending => "pending",
            DlqEntryStatus::Retrying => "retrying",
            DlqEntryStatus::Failed => "failed",
        }
    }
}

/// Error type for parsing DlqEntryStatus from string
#[derive(Debug, Clone)]
pub struct ParseDlqEntryStatusError(String);

impl std::fmt::Display for ParseDlqEntryStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid DLQ entry status: '{}'", self.0)
    }
}

impl std::error::Error for ParseDlqEntryStatusError {}

impl FromStr for DlqEntryStatus {
    type Err = ParseDlqEntryStatusError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(DlqEntryStatus::Pending),
            "retrying" => Ok(DlqEntryStatus::Retrying),
            "failed" => Ok(DlqEntryStatus::Failed),
            _ => Err(ParseDlqEntryStatusError(s.to_string())),
        }
    }
}

/// A dead-letter queue entry representing a failed event batch
#[derive(Debug, Clone)]
pub struct DlqEntry {
    /// Unique ID of the entry
    pub id: i64,
    /// JSON serialized event data
    pub event_json: String,
    /// Error message that caused the failure
    pub error_message: String,
    /// Number of retry attempts
    pub retry_count: u32,
    /// Current status
    pub status: DlqEntryStatus,
    /// When the entry was created (microseconds since epoch)
    pub created_at: i64,
    /// When the last retry was attempted (microseconds since epoch)
    pub last_retry_at: Option<i64>,
}

/// Repository for dead-letter queue operations
#[async_trait]
pub trait DlqRepository: Send + Sync {
    /// Save a failed event to the dead-letter queue
    ///
    /// # Arguments
    /// * `event_json` - JSON serialized event data
    /// * `error_message` - Error that caused the flush failure
    async fn save(&self, event_json: String, error_message: String) -> Result<i64>;

    /// Save multiple failed events in a batch
    async fn save_batch(
        &self,
        events: Vec<(String, String)>, // (event_json, error_message)
    ) -> Result<Vec<i64>>;

    /// Find pending entries ready for retry
    ///
    /// # Arguments
    /// * `limit` - Maximum number of entries to return
    async fn find_pending(&self, limit: usize) -> Result<Vec<DlqEntry>>;

    /// Mark an entry as currently being retried
    async fn mark_retrying(&self, id: i64) -> Result<()>;

    /// Mark multiple entries as currently being retried (batch operation)
    async fn mark_retrying_batch(&self, ids: &[i64]) -> Result<()>;

    /// Increment retry count and update last_retry_at
    async fn increment_retry(&self, id: i64) -> Result<()>;

    /// Increment retry count for multiple entries (batch operation)
    async fn increment_retry_batch(&self, ids: &[i64]) -> Result<()>;

    /// Mark an entry as permanently failed
    async fn mark_failed(&self, id: i64) -> Result<()>;

    /// Mark multiple entries as permanently failed (batch operation)
    async fn mark_failed_batch(&self, ids: &[i64]) -> Result<()>;

    /// Delete a successfully recovered entry
    async fn delete(&self, id: i64) -> Result<()>;

    /// Delete multiple successfully recovered entries (batch operation)
    async fn delete_batch(&self, ids: &[i64]) -> Result<()>;

    /// Count entries by status
    async fn count_by_status(&self, status: DlqEntryStatus) -> Result<u64>;

    /// Count total entries in the DLQ
    async fn count_all(&self) -> Result<u64>;

    /// Delete old failed entries (cleanup)
    ///
    /// # Arguments
    /// * `older_than_micros` - Delete entries created before this timestamp
    async fn delete_old_failed(&self, older_than_micros: i64) -> Result<u64>;
}
