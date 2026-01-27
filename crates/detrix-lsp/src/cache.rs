//! Common cache operations for purity analyzers
//!
//! This module provides shared caching functionality for both Python and Go
//! purity analyzers, eliminating code duplication.

pub use crate::error::Result;
use crate::error::ToLspResult;
use detrix_application::{PurityCache, PurityCacheEntry, PurityCacheKey};
use detrix_config::LspConfig;
use detrix_core::PurityAnalysis;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, warn};

/// Type alias for cache reference used by purity analyzers
pub type CacheRef = Arc<dyn PurityCache + Send + Sync>;

/// Compute SHA256 hash of file content for cache key
///
/// This is used to invalidate cache entries when the source file changes.
pub async fn compute_file_hash(file_path: &Path) -> Result<String> {
    let content = tokio::fs::read_to_string(file_path)
        .await
        .lsp_file_not_found(file_path)?;

    Ok(format!("{:x}", Sha256::digest(content.as_bytes())))
}

/// Try to get cached analysis result
///
/// Returns None if:
/// - Caching is disabled in config
/// - No cache is configured
/// - File hash computation fails
/// - Cache lookup fails or misses
pub async fn get_cached(
    cache: Option<&CacheRef>,
    config: &LspConfig,
    function_name: &str,
    file_path: &Path,
) -> Option<PurityAnalysis> {
    if !config.cache_enabled {
        return None;
    }

    let cache = cache?;

    // Compute file hash for cache key
    let content_hash = match compute_file_hash(file_path).await {
        Ok(hash) => hash,
        Err(e) => {
            warn!("Failed to compute file hash: {}", e);
            return None;
        }
    };

    let key = PurityCacheKey::new(function_name, file_path.to_string_lossy(), content_hash);

    match cache.get(&key).await {
        Ok(Some(entry)) => {
            debug!(
                function = %function_name,
                file = %file_path.display(),
                "Cache hit"
            );
            Some(entry.analysis)
        }
        Ok(None) => None,
        Err(e) => {
            warn!("Cache lookup failed: {}", e);
            None
        }
    }
}

/// Store analysis result in cache
///
/// Does nothing if:
/// - Caching is disabled in config
/// - No cache is configured
/// - File hash computation fails
/// - Cache storage fails (logs warning)
pub async fn cache_result(
    cache: Option<&CacheRef>,
    config: &LspConfig,
    function_name: &str,
    file_path: &Path,
    analysis: &PurityAnalysis,
) {
    if !config.cache_enabled {
        return;
    }

    let cache = match cache {
        Some(c) => c,
        None => return,
    };

    // Compute file hash for cache key
    let content_hash = match compute_file_hash(file_path).await {
        Ok(hash) => hash,
        Err(e) => {
            warn!("Failed to compute file hash for caching: {}", e);
            return;
        }
    };

    let key = PurityCacheKey::new(function_name, file_path.to_string_lossy(), content_hash);

    let entry = PurityCacheEntry::new(analysis.clone(), config.max_call_depth as u32);

    if let Err(e) = cache.set(&key, &entry).await {
        warn!("Failed to cache result: {}", e);
    } else {
        debug!(
            function = %function_name,
            file = %file_path.display(),
            "Cached analysis result"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn test_config() -> LspConfig {
        LspConfig {
            enabled: true,
            command: "test".to_string(),
            args: vec![],
            max_call_depth: 10,
            analysis_timeout_ms: 5000,
            cache_enabled: true,
            working_directory: None,
        }
    }

    #[tokio::test]
    async fn test_compute_file_hash() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "test content").unwrap();

        let hash = compute_file_hash(file.path()).await.unwrap();

        // SHA256 hash should be 64 hex characters
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn test_compute_file_hash_same_content() {
        let mut file1 = NamedTempFile::new().unwrap();
        let mut file2 = NamedTempFile::new().unwrap();
        writeln!(file1, "identical content").unwrap();
        writeln!(file2, "identical content").unwrap();

        let hash1 = compute_file_hash(file1.path()).await.unwrap();
        let hash2 = compute_file_hash(file2.path()).await.unwrap();

        assert_eq!(hash1, hash2);
    }

    #[tokio::test]
    async fn test_compute_file_hash_different_content() {
        let mut file1 = NamedTempFile::new().unwrap();
        let mut file2 = NamedTempFile::new().unwrap();
        writeln!(file1, "content A").unwrap();
        writeln!(file2, "content B").unwrap();

        let hash1 = compute_file_hash(file1.path()).await.unwrap();
        let hash2 = compute_file_hash(file2.path()).await.unwrap();

        assert_ne!(hash1, hash2);
    }

    #[tokio::test]
    async fn test_get_cached_disabled() {
        let mut config = test_config();
        config.cache_enabled = false;

        let result = get_cached(None, &config, "test_func", Path::new("/test.py")).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_cached_no_cache() {
        let config = test_config();

        let result = get_cached(None, &config, "test_func", Path::new("/test.py")).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_cache_result_disabled() {
        let mut config = test_config();
        config.cache_enabled = false;

        let analysis = PurityAnalysis::pure();

        // Should not panic, just return early
        cache_result(None, &config, "test_func", Path::new("/test.py"), &analysis).await;
    }
}
