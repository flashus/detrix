//! Purity Cache Port
//!
//! Defines the trait for caching purity analysis results.
//! This enables fast lookups for already-analyzed functions,
//! avoiding repeated LSP calls for the same function/file combinations.
//!
//! Per Clean Architecture, this output port belongs in the Application layer.
//! Infrastructure adapters (detrix-storage) implement this trait.

use async_trait::async_trait;
use detrix_core::{PurityAnalysis, Result};
use std::path::Path;

/// Key for purity cache lookups
///
/// Combines function name, file path, and content hash to uniquely identify
/// a function's purity analysis. The content hash ensures cache invalidation
/// when source code changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PurityCacheKey {
    /// Function name (e.g., "user.get_data", "calculate_total")
    pub function_name: String,

    /// Absolute file path containing the function
    pub file_path: String,

    /// SHA256 hash of file contents (for cache invalidation)
    pub content_hash: String,
}

impl PurityCacheKey {
    /// Create a new cache key
    pub fn new(
        function_name: impl Into<String>,
        file_path: impl Into<String>,
        content_hash: impl Into<String>,
    ) -> Self {
        Self {
            function_name: function_name.into(),
            file_path: file_path.into(),
            content_hash: content_hash.into(),
        }
    }

    /// Create a cache key from file path (computes hash)
    ///
    /// # Errors
    /// Returns an error if the file cannot be read
    pub fn from_file(function_name: impl Into<String>, file_path: &Path) -> Result<Self> {
        use sha2::{Digest, Sha256};
        use std::fs;

        let content = fs::read_to_string(file_path)?;

        let hash = format!("{:x}", Sha256::digest(content.as_bytes()));

        Ok(Self {
            function_name: function_name.into(),
            file_path: file_path.to_string_lossy().into_owned(),
            content_hash: hash,
        })
    }
}

/// Cache entry containing purity analysis with metadata
#[derive(Debug, Clone)]
pub struct PurityCacheEntry {
    /// The cached purity analysis
    pub analysis: PurityAnalysis,

    /// Unix timestamp when this entry was created
    pub created_at: i64,

    /// Call depth used during analysis
    pub max_depth: u32,
}

impl PurityCacheEntry {
    /// Create a new cache entry
    pub fn new(analysis: PurityAnalysis, max_depth: u32) -> Self {
        Self {
            analysis,
            created_at: chrono::Utc::now().timestamp(),
            max_depth,
        }
    }
}

/// Trait for caching purity analysis results
///
/// This trait abstracts the storage mechanism for caching function purity
/// analysis results. Implementations can use in-memory caches, SQLite,
/// or other storage backends.
///
/// Cache entries are keyed by function name, file path, and content hash.
/// The content hash ensures cache invalidation when source files change.
#[async_trait]
pub trait PurityCache: Send + Sync {
    /// Get a cached purity analysis
    ///
    /// # Arguments
    /// * `key` - The cache key (function name + file path + content hash)
    ///
    /// # Returns
    /// * `Ok(Some(entry))` - Cached entry found
    /// * `Ok(None)` - No cache entry exists
    /// * `Err(error)` - Cache lookup failed
    async fn get(&self, key: &PurityCacheKey) -> Result<Option<PurityCacheEntry>>;

    /// Store a purity analysis in the cache
    ///
    /// # Arguments
    /// * `key` - The cache key
    /// * `entry` - The analysis result to cache
    ///
    /// # Returns
    /// * `Ok(())` - Entry stored successfully
    /// * `Err(error)` - Cache storage failed
    async fn set(&self, key: &PurityCacheKey, entry: &PurityCacheEntry) -> Result<()>;

    /// Remove a specific cache entry
    ///
    /// # Arguments
    /// * `key` - The cache key to remove
    ///
    /// # Returns
    /// * `Ok(true)` - Entry was removed
    /// * `Ok(false)` - Entry didn't exist
    /// * `Err(error)` - Removal failed
    async fn remove(&self, key: &PurityCacheKey) -> Result<bool>;

    /// Invalidate all entries for a specific file
    ///
    /// Used when a file is modified and all its function caches should be cleared.
    ///
    /// # Arguments
    /// * `file_path` - The file path to invalidate
    ///
    /// # Returns
    /// * `Ok(count)` - Number of entries removed
    /// * `Err(error)` - Invalidation failed
    async fn invalidate_file(&self, file_path: &str) -> Result<usize>;

    /// Clear all cached entries
    ///
    /// # Returns
    /// * `Ok(count)` - Number of entries removed
    /// * `Err(error)` - Clear operation failed
    async fn clear(&self) -> Result<usize>;

    /// Get cache statistics
    ///
    /// Returns information about cache size and hit rates for monitoring.
    async fn stats(&self) -> Result<PurityCacheStats>;
}

/// Statistics about the purity cache
#[derive(Debug, Clone, Default)]
pub struct PurityCacheStats {
    /// Total number of cached entries
    pub entry_count: usize,

    /// Total number of cache hits since startup
    pub hits: u64,

    /// Total number of cache misses since startup
    pub misses: u64,
}

impl PurityCacheStats {
    /// Calculate the cache hit rate
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}
