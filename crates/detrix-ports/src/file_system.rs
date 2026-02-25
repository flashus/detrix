//! Virtual File System Port
//!
//! Abstracts file access so cloud/Docker deployments (where the server has no
//! source files on disk) can receive file content from agents and cache it.
//!
//! # Design
//!
//! - **Cache → disk fallback**: `read_to_string` checks the in-memory cache
//!   first, then falls back to `std::fs` (for local daemon mode).
//! - **Hash-based validation**: On reconnect the cache is marked *stale*;
//!   the client then sends file hashes, and only mismatched files are evicted
//!   (zero transfer for unchanged files).
//! - **Connection-scoped**: Each cached file is scoped to a connection so
//!   different projects don't collide.

use async_trait::async_trait;
use detrix_core::{Connection, Result};
use std::sync::Arc;

/// Thread-safe reference to a virtual file system
pub type VfsRef = Arc<dyn VirtualFileSystem>;

/// Metadata about how a file was sourced
#[derive(Debug, Clone, Default)]
pub struct SourceMetadata {
    /// Which source provided the content ("bridge", "control_plane", "disk")
    pub source_kind: String,
    /// Git commit SHA the content was served from (pinned mode)
    pub commit: Option<String>,
    /// Whether git content differs from working tree
    pub differs_from_local: Option<bool>,
}

/// Result from a successful file source fetch
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub content: String,
    pub metadata: SourceMetadata,
}

/// Virtual File System abstraction for source file access.
///
/// Implementations must be `Send + Sync` (shared across async tasks).
pub trait VirtualFileSystem: Send + Sync {
    /// Read file content.
    ///
    /// Resolution order: cache → disk → error.
    fn read_to_string(&self, path: &str) -> Result<String>;

    /// Check whether a file is accessible (cached or on disk).
    fn exists(&self, path: &str) -> Result<bool>;

    /// Store file content, scoped to a connection. Computes + stores SHA-256.
    fn store(&self, connection_id: &str, path: &str, content: String);

    /// Return cached `(path, hash)` pairs for a connection.
    ///
    /// The client compares these with its local files to drive the
    /// validation protocol.
    fn cached_hashes(&self, connection_id: &str) -> Vec<(String, String)>;

    /// Validate hashes received from a client.
    ///
    /// - Matching hash → entry is un-staled.
    /// - Mismatched hash → entry is evicted.
    ///
    /// Returns the list of **evicted** paths (client should re-provide these).
    fn validate_hashes(
        &self,
        connection_id: &str,
        client_hashes: &[(String, String)],
    ) -> Vec<String>;

    /// Mark every cached entry for a connection as *stale* (e.g. after
    /// reconnect or hot-reload TTL expiry). Stale entries are still served
    /// but flagged for hash validation.
    fn mark_stale(&self, connection_id: &str);

    /// Remove all cached entries for a connection (permanent close).
    fn clear_connection(&self, connection_id: &str);

    /// Store file content with source metadata.
    ///
    /// Default implementation ignores metadata and delegates to `store()`.
    fn store_with_metadata(
        &self,
        connection_id: &str,
        path: &str,
        content: String,
        _metadata: SourceMetadata,
    ) {
        self.store(connection_id, path, content);
    }

    /// Retrieve source metadata for a cached file.
    ///
    /// Default implementation returns `None` (no metadata tracking).
    fn get_metadata(&self, _path: &str) -> Option<SourceMetadata> {
        None
    }
}

// ============================================================================
// File Source Trait (for pluggable file fetching chain)
// ============================================================================

/// Thread-safe reference to a file source
pub type FileSourceRef = Arc<dyn FileSource>;

/// A single file source provider (e.g., control_plane, bridge, disk).
///
/// Part of the pluggable file source chain. The server tries each source
/// in priority order until one succeeds. This makes file fetching transparent
/// to the agent — the server automatically resolves files from the best
/// available source.
///
/// # Return Convention
///
/// - `Ok(Some(FetchResult))` — file found, content + metadata returned
/// - `Ok(None)` — this source cannot provide the file (graceful skip)
/// - `Err(...)` — source encountered an error (logged, then skips to next source)
#[async_trait]
pub trait FileSource: Send + Sync {
    /// Provider name (matches config values: "control_plane", "bridge", "disk")
    fn name(&self) -> &str;

    /// Try to fetch file content from this source.
    async fn fetch(&self, connection: &Connection, file_path: &str) -> Result<Option<FetchResult>>;
}
