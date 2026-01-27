//! AST parse result caching for tree-sitter analyzers
//!
//! Caches parsed AST results to avoid re-parsing the same expressions.
//! Uses a thread-safe DashMap with LRU-style eviction.

use super::TreeSitterResult;
use dashmap::DashMap;
use detrix_config::constants::DEFAULT_AST_CACHE_MAX_ENTRIES;
use detrix_core::SafetyLevel;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use tracing::trace;

/// Global cache instance
static CACHE: OnceLock<TreeSitterCache> = OnceLock::new();

/// Initialize the global cache with a specific max_entries value.
///
/// This should be called early in application startup with `SafetyConfig.ast_cache_max_entries`.
/// If not called before first use, the cache will be initialized with the default value.
///
/// Returns `true` if the cache was initialized, `false` if it was already initialized.
pub fn init_global_cache(max_entries: usize) -> bool {
    CACHE.set(TreeSitterCache::new(max_entries)).is_ok()
}

/// Initialize the global cache from SafetyConfig.
///
/// This should be called early in application startup.
/// If not called before first use, the cache will be initialized with the default value.
///
/// Returns `true` if the cache was initialized, `false` if it was already initialized.
pub fn init_global_cache_from_config(config: &detrix_config::SafetyConfig) -> bool {
    init_global_cache(config.ast_cache_max_entries)
}

/// Get the global cache instance
pub fn global_cache() -> &'static TreeSitterCache {
    CACHE.get_or_init(TreeSitterCache::default)
}

/// Thread-safe cache for tree-sitter AST analysis results
pub struct TreeSitterCache {
    /// The cache storage: key -> (result, access_count)
    cache: DashMap<String, CacheEntry>,
    /// Maximum number of entries before eviction
    max_entries: usize,
    /// Hit counter for statistics
    hits: AtomicU64,
    /// Miss counter for statistics
    misses: AtomicU64,
}

/// Cache entry with access tracking for LRU-style eviction
struct CacheEntry {
    result: TreeSitterResult,
    last_access: u64,
}

impl Default for TreeSitterCache {
    fn default() -> Self {
        Self::new(DEFAULT_AST_CACHE_MAX_ENTRIES)
    }
}

impl TreeSitterCache {
    /// Create a new cache with specified max entries
    pub fn new(max_entries: usize) -> Self {
        Self {
            cache: DashMap::new(),
            max_entries,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Generate cache key from expression, language, and safety level
    ///
    /// We include allowed_functions hash for strict mode since it affects the result.
    pub fn make_key(
        expression: &str,
        language: &str,
        safety_level: SafetyLevel,
        allowed_functions: &HashSet<String>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(expression.as_bytes());
        hasher.update(language.as_bytes());
        hasher.update([safety_level as u8]);

        // For strict mode, the allowed functions affect the result
        if safety_level == SafetyLevel::Strict {
            // Sort for deterministic hashing
            let mut funcs: Vec<&String> = allowed_functions.iter().collect();
            funcs.sort();
            for func in funcs {
                hasher.update(func.as_bytes());
            }
        }

        let hash = hasher.finalize();
        // Use first 16 bytes (128 bits) for shorter keys, format as hex
        hash[..16]
            .iter()
            .fold(String::with_capacity(32), |mut s, b| {
                use std::fmt::Write;
                let _ = write!(s, "{:02x}", b);
                s
            })
    }

    /// Get a cached result or compute and cache it
    pub fn get_or_insert<F>(&self, key: &str, compute: F) -> TreeSitterResult
    where
        F: FnOnce() -> TreeSitterResult,
    {
        // Try to get from cache
        if let Some(mut entry) = self.cache.get_mut(key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            entry.last_access = self.access_counter();
            trace!(key = %key, "AST cache hit");
            return entry.result.clone();
        }

        // Cache miss - compute the result
        self.misses.fetch_add(1, Ordering::Relaxed);
        trace!(key = %key, "AST cache miss");

        let result = compute();

        // Evict if needed before inserting
        if self.cache.len() >= self.max_entries {
            self.evict_oldest();
        }

        // Insert into cache
        self.cache.insert(
            key.to_string(),
            CacheEntry {
                result: result.clone(),
                last_access: self.access_counter(),
            },
        );

        result
    }

    /// Get current access counter (approximates time for LRU)
    fn access_counter(&self) -> u64 {
        self.hits.load(Ordering::Relaxed) + self.misses.load(Ordering::Relaxed)
    }

    /// Evict the oldest entries (approximately 10% of cache)
    fn evict_oldest(&self) {
        let evict_count = self.max_entries / 10;
        if evict_count == 0 {
            return;
        }

        // Find entries with lowest access counts
        let mut entries: Vec<(String, u64)> = self
            .cache
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().last_access))
            .collect();

        entries.sort_by_key(|(_, access)| *access);

        // Remove oldest entries
        for (key, _) in entries.into_iter().take(evict_count) {
            self.cache.remove(&key);
        }

        trace!(evicted = evict_count, "AST cache eviction");
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.cache.len(),
            max_entries: self.max_entries,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    /// Clear the cache
    pub fn clear(&self) {
        self.cache.clear();
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }
}

/// Cache statistics
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub entries: usize,
    pub max_entries: usize,
    pub hits: u64,
    pub misses: u64,
}

impl CacheStats {
    /// Calculate hit ratio (0.0 to 1.0)
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_allowed() -> HashSet<String> {
        ["len", "str", "int"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn test_cache_key_generation() {
        let allowed = sample_allowed();

        let key1 = TreeSitterCache::make_key("x + 1", "python", SafetyLevel::Strict, &allowed);
        let key2 = TreeSitterCache::make_key("x + 1", "python", SafetyLevel::Strict, &allowed);
        let key3 = TreeSitterCache::make_key("x + 2", "python", SafetyLevel::Strict, &allowed);
        let key4 = TreeSitterCache::make_key("x + 1", "go", SafetyLevel::Strict, &allowed);
        let key5 = TreeSitterCache::make_key("x + 1", "python", SafetyLevel::Trusted, &allowed);

        // Same inputs produce same key
        assert_eq!(key1, key2);

        // Different expression produces different key
        assert_ne!(key1, key3);

        // Different language produces different key
        assert_ne!(key1, key4);

        // Different safety level produces different key
        assert_ne!(key1, key5);
    }

    #[test]
    fn test_cache_get_or_insert() {
        let cache = TreeSitterCache::new(100);
        let allowed = sample_allowed();

        let key = TreeSitterCache::make_key("test", "python", SafetyLevel::Strict, &allowed);

        // First call - cache miss
        let mut compute_count = 0;
        let result1 = cache.get_or_insert(&key, || {
            compute_count += 1;
            TreeSitterResult::safe()
        });

        assert_eq!(compute_count, 1);
        assert!(result1.is_safe);

        // Second call - cache hit
        let result2 = cache.get_or_insert(&key, || {
            compute_count += 1;
            TreeSitterResult::unsafe_with_violations(vec!["error".to_string()])
        });

        // Should not have called compute again
        assert_eq!(compute_count, 1);
        // Should return cached result (safe, not unsafe)
        assert!(result2.is_safe);
    }

    #[test]
    fn test_cache_stats() {
        let cache = TreeSitterCache::new(100);
        let allowed = sample_allowed();

        let key1 = TreeSitterCache::make_key("expr1", "python", SafetyLevel::Strict, &allowed);
        let key2 = TreeSitterCache::make_key("expr2", "python", SafetyLevel::Strict, &allowed);

        // First access - miss
        cache.get_or_insert(&key1, TreeSitterResult::safe);
        // Second access - hit
        cache.get_or_insert(&key1, TreeSitterResult::safe);
        // Third access - miss (different key)
        cache.get_or_insert(&key2, TreeSitterResult::safe);

        let stats = cache.stats();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 2);
        assert!((stats.hit_ratio() - 0.333).abs() < 0.01);
    }

    #[test]
    fn test_cache_eviction() {
        let cache = TreeSitterCache::new(10);
        let allowed = sample_allowed();

        // Fill cache beyond capacity
        for i in 0..15 {
            let key = TreeSitterCache::make_key(
                &format!("expr{}", i),
                "python",
                SafetyLevel::Strict,
                &allowed,
            );
            cache.get_or_insert(&key, TreeSitterResult::safe);
        }

        // Cache should have evicted some entries
        let stats = cache.stats();
        assert!(stats.entries <= 10);
    }

    #[test]
    fn test_cache_clear() {
        let cache = TreeSitterCache::new(100);
        let allowed = sample_allowed();

        let key = TreeSitterCache::make_key("test", "python", SafetyLevel::Strict, &allowed);
        cache.get_or_insert(&key, TreeSitterResult::safe);

        assert_eq!(cache.stats().entries, 1);

        cache.clear();

        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().hits, 0);
        assert_eq!(cache.stats().misses, 0);
    }

    #[test]
    fn test_allowed_functions_affects_strict_key() {
        let allowed1: HashSet<String> = ["len", "str"].iter().map(|s| s.to_string()).collect();
        let allowed2: HashSet<String> = ["len", "int"].iter().map(|s| s.to_string()).collect();

        let key1 = TreeSitterCache::make_key("test", "python", SafetyLevel::Strict, &allowed1);
        let key2 = TreeSitterCache::make_key("test", "python", SafetyLevel::Strict, &allowed2);

        // Different allowed functions should produce different keys in strict mode
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_allowed_functions_ignored_trusted_mode() {
        let allowed1: HashSet<String> = ["len", "str"].iter().map(|s| s.to_string()).collect();
        let allowed2: HashSet<String> = ["len", "int"].iter().map(|s| s.to_string()).collect();

        let key1 = TreeSitterCache::make_key("test", "python", SafetyLevel::Trusted, &allowed1);
        let key2 = TreeSitterCache::make_key("test", "python", SafetyLevel::Trusted, &allowed2);

        // In trusted mode, allowed functions don't affect the result
        assert_eq!(key1, key2);
    }
}
