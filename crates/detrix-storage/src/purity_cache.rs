//! SQLite implementation of PurityCache trait
//!
//! Stores purity analysis results to avoid repeated LSP calls for unchanged functions.
//! Cache entries are keyed by function name, file path, and file content hash.

use async_trait::async_trait;
use detrix_application::{PurityCache, PurityCacheEntry, PurityCacheKey, PurityCacheStats};
use detrix_core::{ImpureCall, PurityAnalysis, PurityLevel, Result};
use sqlx::{Row, SqlitePool};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, trace, warn};

/// SQLite-backed purity cache
#[derive(Debug)]
pub struct SqlitePurityCache {
    pool: SqlitePool,
    /// Cache hit counter (in-memory for performance)
    hits: AtomicU64,
    /// Cache miss counter (in-memory for performance)
    misses: AtomicU64,
}

impl SqlitePurityCache {
    /// Create a new SQLite purity cache with the given connection pool
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Parse purity level from database string
    fn parse_purity_level(s: &str) -> PurityLevel {
        match s {
            "pure" => PurityLevel::Pure,
            "unknown" => PurityLevel::Unknown,
            "impure" => PurityLevel::Impure,
            _ => PurityLevel::Unknown,
        }
    }

    /// Convert purity level to database string
    fn purity_level_to_str(level: PurityLevel) -> &'static str {
        match level {
            PurityLevel::Pure => "pure",
            PurityLevel::Unknown => "unknown",
            PurityLevel::Impure => "impure",
        }
    }
}

#[async_trait]
impl PurityCache for SqlitePurityCache {
    async fn get(&self, key: &PurityCacheKey) -> Result<Option<PurityCacheEntry>> {
        let row = sqlx::query(
            r#"
            SELECT
                purity_level,
                impure_calls_json,
                unknown_calls_json,
                max_depth_reached,
                max_depth_used,
                created_at
            FROM purity_cache
            WHERE function_name = ? AND file_path = ? AND content_hash = ?
            "#,
        )
        .bind(&key.function_name)
        .bind(&key.file_path)
        .bind(&key.content_hash)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                self.hits.fetch_add(1, Ordering::Relaxed);

                let purity_level: String = row.get("purity_level");
                let impure_calls_json: String = row.get("impure_calls_json");
                let unknown_calls_json: String = row.get("unknown_calls_json");
                let max_depth_reached: i32 = row.get("max_depth_reached");
                let max_depth_used: i32 = row.get("max_depth_used");
                let created_at: i64 = row.get("created_at");

                // Parse impure calls from JSON
                let impure_calls: Vec<ImpureCall> = serde_json::from_str(&impure_calls_json)
                    .unwrap_or_else(|e| {
                        warn!(
                            function = %key.function_name,
                            file = %key.file_path,
                            error = %e,
                            json = %impure_calls_json,
                            "Failed to parse impure_calls from cache, using empty"
                        );
                        Vec::new()
                    });

                // Parse unknown calls from JSON
                let unknown_calls: Vec<String> = serde_json::from_str(&unknown_calls_json)
                    .unwrap_or_else(|e| {
                        warn!(
                            function = %key.function_name,
                            file = %key.file_path,
                            error = %e,
                            json = %unknown_calls_json,
                            "Failed to parse unknown_calls from cache, using empty"
                        );
                        Vec::new()
                    });

                let analysis = PurityAnalysis {
                    level: Self::parse_purity_level(&purity_level),
                    impure_calls,
                    unknown_calls,
                    max_depth_reached: max_depth_reached != 0,
                };

                trace!(
                    function = %key.function_name,
                    file = %key.file_path,
                    level = ?analysis.level,
                    "Cache hit"
                );

                Ok(Some(PurityCacheEntry {
                    analysis,
                    created_at,
                    max_depth: max_depth_used as u32,
                }))
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                trace!(
                    function = %key.function_name,
                    file = %key.file_path,
                    "Cache miss"
                );
                Ok(None)
            }
        }
    }

    async fn set(&self, key: &PurityCacheKey, entry: &PurityCacheEntry) -> Result<()> {
        let purity_level = Self::purity_level_to_str(entry.analysis.level);

        // Serialize impure calls to JSON
        let impure_calls_json =
            serde_json::to_string(&entry.analysis.impure_calls).unwrap_or_else(|e| {
                warn!(
                    function = %key.function_name,
                    file = %key.file_path,
                    error = %e,
                    "Failed to serialize impure_calls for cache, using empty"
                );
                "[]".to_string()
            });

        // Serialize unknown calls to JSON
        let unknown_calls_json = serde_json::to_string(&entry.analysis.unknown_calls)
            .unwrap_or_else(|e| {
                warn!(
                    function = %key.function_name,
                    file = %key.file_path,
                    error = %e,
                    "Failed to serialize unknown_calls for cache, using empty"
                );
                "[]".to_string()
            });

        let max_depth_reached = if entry.analysis.max_depth_reached {
            1
        } else {
            0
        };

        // Use INSERT OR REPLACE for upsert behavior
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO purity_cache (
                function_name,
                file_path,
                content_hash,
                purity_level,
                impure_calls_json,
                unknown_calls_json,
                max_depth_reached,
                max_depth_used,
                created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&key.function_name)
        .bind(&key.file_path)
        .bind(&key.content_hash)
        .bind(purity_level)
        .bind(&impure_calls_json)
        .bind(&unknown_calls_json)
        .bind(max_depth_reached)
        .bind(entry.max_depth as i32)
        .bind(entry.created_at)
        .execute(&self.pool)
        .await?;

        debug!(
            function = %key.function_name,
            file = %key.file_path,
            level = ?entry.analysis.level,
            "Cache entry stored"
        );

        Ok(())
    }

    async fn remove(&self, key: &PurityCacheKey) -> Result<bool> {
        let result = sqlx::query(
            r#"
            DELETE FROM purity_cache
            WHERE function_name = ? AND file_path = ? AND content_hash = ?
            "#,
        )
        .bind(&key.function_name)
        .bind(&key.file_path)
        .bind(&key.content_hash)
        .execute(&self.pool)
        .await?;

        let removed = result.rows_affected() > 0;
        if removed {
            debug!(
                function = %key.function_name,
                file = %key.file_path,
                "Cache entry removed"
            );
        }
        Ok(removed)
    }

    async fn invalidate_file(&self, file_path: &str) -> Result<usize> {
        let result = sqlx::query(
            r#"
            DELETE FROM purity_cache
            WHERE file_path = ?
            "#,
        )
        .bind(file_path)
        .execute(&self.pool)
        .await?;

        let count = result.rows_affected() as usize;
        if count > 0 {
            debug!(
                file = %file_path,
                count = count,
                "Cache entries invalidated for file"
            );
        }
        Ok(count)
    }

    async fn clear(&self) -> Result<usize> {
        let result = sqlx::query("DELETE FROM purity_cache")
            .execute(&self.pool)
            .await?;

        let count = result.rows_affected() as usize;
        debug!(count = count, "Cache cleared");

        // Reset hit/miss counters
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);

        Ok(count)
    }

    async fn stats(&self) -> Result<PurityCacheStats> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM purity_cache")
            .fetch_one(&self.pool)
            .await?;

        let entry_count: i64 = row.get("count");

        Ok(PurityCacheStats {
            entry_count: entry_count as usize,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStorage;

    async fn create_test_cache() -> SqlitePurityCache {
        let storage = SqliteStorage::in_memory().await.unwrap();
        SqlitePurityCache::new(storage.pool().clone())
    }

    #[tokio::test]
    async fn test_cache_set_and_get_pure() {
        let cache = create_test_cache().await;

        let key = PurityCacheKey::new("calculate_total", "/app/math.py", "abc123");
        let entry = PurityCacheEntry::new(PurityAnalysis::pure(), 10);

        cache.set(&key, &entry).await.unwrap();

        let retrieved = cache.get(&key).await.unwrap();
        assert!(retrieved.is_some());

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.analysis.level, PurityLevel::Pure);
        assert!(retrieved.analysis.impure_calls.is_empty());
        assert!(retrieved.analysis.unknown_calls.is_empty());
        assert!(!retrieved.analysis.max_depth_reached);
        assert_eq!(retrieved.max_depth, 10);
    }

    #[tokio::test]
    async fn test_cache_set_and_get_impure() {
        let cache = create_test_cache().await;

        let key = PurityCacheKey::new("process_order", "/app/orders.py", "def456");
        let analysis = PurityAnalysis::impure(vec![
            ImpureCall::new("print", "I/O operation"),
            ImpureCall::new("os.system", "shell execution"),
        ]);
        let entry = PurityCacheEntry::new(analysis, 5);

        cache.set(&key, &entry).await.unwrap();

        let retrieved = cache.get(&key).await.unwrap().unwrap();
        assert_eq!(retrieved.analysis.level, PurityLevel::Impure);
        assert_eq!(retrieved.analysis.impure_calls.len(), 2);
        assert_eq!(retrieved.analysis.impure_calls[0].function_name, "print");
        assert_eq!(
            retrieved.analysis.impure_calls[1].function_name,
            "os.system"
        );
    }

    #[tokio::test]
    async fn test_cache_set_and_get_unknown() {
        let cache = create_test_cache().await;

        let key = PurityCacheKey::new("analyze", "/app/data.py", "ghi789");
        let analysis =
            PurityAnalysis::unknown(vec!["external_lib.func".into(), "other.helper".into()]);
        let entry = PurityCacheEntry::new(analysis, 8);

        cache.set(&key, &entry).await.unwrap();

        let retrieved = cache.get(&key).await.unwrap().unwrap();
        assert_eq!(retrieved.analysis.level, PurityLevel::Unknown);
        assert_eq!(retrieved.analysis.unknown_calls.len(), 2);
        assert!(retrieved
            .analysis
            .unknown_calls
            .contains(&"external_lib.func".to_string()));
    }

    #[tokio::test]
    async fn test_cache_miss() {
        let cache = create_test_cache().await;

        let key = PurityCacheKey::new("nonexistent", "/app/test.py", "xyz");
        let result = cache.get(&key).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_cache_upsert() {
        let cache = create_test_cache().await;

        let key = PurityCacheKey::new("func", "/app/test.py", "hash1");

        // Insert pure
        let entry1 = PurityCacheEntry::new(PurityAnalysis::pure(), 10);
        cache.set(&key, &entry1).await.unwrap();

        // Update to impure
        let entry2 = PurityCacheEntry::new(
            PurityAnalysis::impure(vec![ImpureCall::new("print", "I/O")]),
            10,
        );
        cache.set(&key, &entry2).await.unwrap();

        // Should get impure
        let retrieved = cache.get(&key).await.unwrap().unwrap();
        assert_eq!(retrieved.analysis.level, PurityLevel::Impure);
    }

    #[tokio::test]
    async fn test_cache_remove() {
        let cache = create_test_cache().await;

        let key = PurityCacheKey::new("func", "/app/test.py", "hash1");
        let entry = PurityCacheEntry::new(PurityAnalysis::pure(), 10);

        cache.set(&key, &entry).await.unwrap();
        assert!(cache.get(&key).await.unwrap().is_some());

        let removed = cache.remove(&key).await.unwrap();
        assert!(removed);

        assert!(cache.get(&key).await.unwrap().is_none());

        // Second remove returns false
        let removed_again = cache.remove(&key).await.unwrap();
        assert!(!removed_again);
    }

    #[tokio::test]
    async fn test_cache_invalidate_file() {
        let cache = create_test_cache().await;

        let entry = PurityCacheEntry::new(PurityAnalysis::pure(), 10);

        // Add entries for two files
        let key1 = PurityCacheKey::new("func1", "/app/file1.py", "hash1");
        let key2 = PurityCacheKey::new("func2", "/app/file1.py", "hash1");
        let key3 = PurityCacheKey::new("func3", "/app/file2.py", "hash2");

        cache.set(&key1, &entry).await.unwrap();
        cache.set(&key2, &entry).await.unwrap();
        cache.set(&key3, &entry).await.unwrap();

        // Invalidate file1.py
        let removed = cache.invalidate_file("/app/file1.py").await.unwrap();
        assert_eq!(removed, 2);

        // file1.py entries should be gone
        assert!(cache.get(&key1).await.unwrap().is_none());
        assert!(cache.get(&key2).await.unwrap().is_none());

        // file2.py entry should still exist
        assert!(cache.get(&key3).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_cache_clear() {
        let cache = create_test_cache().await;

        let entry = PurityCacheEntry::new(PurityAnalysis::pure(), 10);

        for i in 0..5 {
            let key = PurityCacheKey::new(format!("func{}", i), "/app/test.py", "hash");
            cache.set(&key, &entry).await.unwrap();
        }

        let stats_before = cache.stats().await.unwrap();
        assert_eq!(stats_before.entry_count, 5);

        let removed = cache.clear().await.unwrap();
        assert_eq!(removed, 5);

        let stats_after = cache.stats().await.unwrap();
        assert_eq!(stats_after.entry_count, 0);
    }

    #[tokio::test]
    async fn test_cache_stats_hit_miss() {
        let cache = create_test_cache().await;

        let key = PurityCacheKey::new("func", "/app/test.py", "hash");
        let entry = PurityCacheEntry::new(PurityAnalysis::pure(), 10);

        // 2 misses
        cache.get(&key).await.unwrap();
        cache.get(&key).await.unwrap();

        // Set and 3 hits
        cache.set(&key, &entry).await.unwrap();
        cache.get(&key).await.unwrap();
        cache.get(&key).await.unwrap();
        cache.get(&key).await.unwrap();

        let stats = cache.stats().await.unwrap();
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.hits, 3);
        assert_eq!(stats.misses, 2);
        assert!((stats.hit_rate() - 0.6).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_max_depth_reached_flag() {
        let cache = create_test_cache().await;

        let key = PurityCacheKey::new("deep_func", "/app/test.py", "hash");
        let mut analysis = PurityAnalysis::unknown(vec!["deep.call".into()]);
        analysis.max_depth_reached = true;

        let entry = PurityCacheEntry::new(analysis, 3);
        cache.set(&key, &entry).await.unwrap();

        let retrieved = cache.get(&key).await.unwrap().unwrap();
        assert!(retrieved.analysis.max_depth_reached);
        assert_eq!(retrieved.max_depth, 3);
    }
}
