//! Cached Virtual File System
//!
//! In-memory cache with disk fallback for source file access.
//! Used by cloud/Docker deployments where the server has no local source files.
//!
//! # Cache semantics
//!
//! - **Store**: agents provide file content via `store()`. SHA-256 is computed
//!   and cached alongside the content.
//! - **Read**: cache hit returns immediately; cache miss falls back to
//!   `std::fs::read_to_string` (works in local-daemon mode).
//! - **Stale**: after reconnect, entries are marked stale. The client then
//!   sends hashes via `validate_hashes` — matching hashes un-stale, mismatches
//!   evict the entry.
//! - **Hot-reload TTL**: for hot-reload languages, entries become stale after
//!   a configurable TTL. On next read the caller is expected to re-validate.

use detrix_core::{Error, Result};
use detrix_ports::{SourceMetadata, VirtualFileSystem};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tracing::debug;

/// Cache entry stored per (connection_id, path) pair.
#[derive(Debug, Clone)]
struct CacheEntry {
    content: String,
    hash: String,
    stale: bool,
    stored_at: Instant,
    metadata: Option<SourceMetadata>,
}

/// In-memory cached file system with disk fallback.
///
/// Thread-safe via `std::sync::RwLock` (sync, not async — matching the
/// `FileInspectionService` usage which is synchronous).
pub struct CachedFileSystem {
    /// (connection_id, path) → entry
    cache: RwLock<HashMap<(String, String), CacheEntry>>,
    /// TTL for hot-reload languages. After expiry, entries are marked stale
    /// (not evicted — they await hash validation).
    hot_reload_ttl: Duration,
}

impl CachedFileSystem {
    /// Create a new CachedFileSystem.
    ///
    /// `hot_reload_ttl` is the duration after which entries for hot-reload
    /// languages become stale.
    pub fn new(hot_reload_ttl: Duration) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            hot_reload_ttl,
        }
    }

    /// Check and mark entries whose hot-reload TTL has expired as stale.
    fn check_ttl_expiry(&self, path: &str) {
        let mut cache = self.cache.write().unwrap_or_else(|p| p.into_inner());
        for ((_, p), entry) in cache.iter_mut() {
            if p == path && !entry.stale && entry.stored_at.elapsed() > self.hot_reload_ttl {
                debug!(path = p.as_str(), "Hot-reload TTL expired, marking stale");
                entry.stale = true;
            }
        }
    }
}

impl VirtualFileSystem for CachedFileSystem {
    fn read_to_string(&self, path: &str) -> Result<String> {
        // Check TTL for hot-reload languages
        self.check_ttl_expiry(path);

        // Look in cache (any connection)
        {
            let cache = self.cache.read().unwrap_or_else(|p| p.into_inner());
            for ((_, p), entry) in cache.iter() {
                if p == path {
                    return Ok(entry.content.clone());
                }
            }
        }

        // Disk fallback
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::FileNotFound(format!("File not found: {}", path)))
            }
            Err(e) => Err(Error::Io(format!("Failed to read '{}': {}", path, e))),
        }
    }

    fn exists(&self, path: &str) -> Result<bool> {
        // Check cache
        {
            let cache = self.cache.read().unwrap_or_else(|p| p.into_inner());
            for ((_, p), _) in cache.iter() {
                if p == path {
                    return Ok(true);
                }
            }
        }

        // Disk fallback
        Ok(std::path::Path::new(path).exists())
    }

    fn store(&self, connection_id: &str, path: &str, content: String) {
        self.store_with_metadata(connection_id, path, content, SourceMetadata::default());
    }

    fn store_with_metadata(
        &self,
        connection_id: &str,
        path: &str,
        content: String,
        metadata: SourceMetadata,
    ) {
        let hash = sha256_hex(&content);
        debug!(
            connection_id,
            path,
            hash = hash.as_str(),
            source = metadata.source_kind.as_str(),
            "VFS: stored file"
        );
        let mut cache = self.cache.write().unwrap_or_else(|p| p.into_inner());
        cache.insert(
            (connection_id.to_string(), path.to_string()),
            CacheEntry {
                content,
                hash,
                stale: false,
                stored_at: Instant::now(),
                metadata: Some(metadata),
            },
        );
    }

    fn get_metadata(&self, path: &str) -> Option<SourceMetadata> {
        let cache = self.cache.read().unwrap_or_else(|p| p.into_inner());
        for ((_, p), entry) in cache.iter() {
            if p == path {
                return entry.metadata.clone();
            }
        }
        None
    }

    fn cached_hashes(&self, connection_id: &str) -> Vec<(String, String)> {
        let cache = self.cache.read().unwrap_or_else(|p| p.into_inner());
        cache
            .iter()
            .filter(|((cid, _), _)| cid == connection_id)
            .map(|((_, path), entry)| (path.clone(), entry.hash.clone()))
            .collect()
    }

    fn validate_hashes(
        &self,
        connection_id: &str,
        client_hashes: &[(String, String)],
    ) -> Vec<String> {
        let mut cache = self.cache.write().unwrap_or_else(|p| p.into_inner());
        let mut evicted = Vec::new();

        let client_map: HashMap<&str, &str> = client_hashes
            .iter()
            .map(|(p, h)| (p.as_str(), h.as_str()))
            .collect();

        // Collect keys for this connection
        let keys: Vec<(String, String)> = cache
            .keys()
            .filter(|(cid, _)| cid == connection_id)
            .cloned()
            .collect();

        for key in keys {
            let path = &key.1;
            if let Some(&client_hash) = client_map.get(path.as_str()) {
                let entry = cache.get_mut(&key).unwrap();
                if entry.hash == client_hash {
                    // Match → un-stale, restart TTL
                    entry.stale = false;
                    entry.stored_at = Instant::now();
                    debug!(path = path.as_str(), "VFS: hash matched, un-staled");
                } else {
                    // Mismatch → evict
                    debug!(path = path.as_str(), "VFS: hash mismatch, evicting");
                    evicted.push(path.clone());
                    cache.remove(&key);
                }
            }
        }

        evicted
    }

    fn mark_stale(&self, connection_id: &str) {
        let mut cache = self.cache.write().unwrap_or_else(|p| p.into_inner());
        let mut count = 0;
        for ((cid, _), entry) in cache.iter_mut() {
            if cid == connection_id {
                entry.stale = true;
                count += 1;
            }
        }
        debug!(connection_id, count, "VFS: marked entries stale");
    }

    fn clear_connection(&self, connection_id: &str) {
        let mut cache = self.cache.write().unwrap_or_else(|p| p.into_inner());
        let before = cache.len();
        cache.retain(|(cid, _), _| cid != connection_id);
        let removed = before - cache.len();
        debug!(connection_id, removed, "VFS: cleared connection cache");
    }
}

/// A minimal disk-only VFS that delegates everything to `std::fs`.
///
/// Used as the default VFS when no caching is needed (e.g., local daemon mode
/// or in tests that use real temp files).
pub struct DiskVfs;

impl DiskVfs {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DiskVfs {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualFileSystem for DiskVfs {
    fn read_to_string(&self, path: &str) -> Result<String> {
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::FileNotFound(format!("File not found: {}", path)))
            }
            Err(e) => Err(Error::Io(format!("Failed to read '{}': {}", path, e))),
        }
    }

    fn exists(&self, path: &str) -> Result<bool> {
        Ok(std::path::Path::new(path).exists())
    }

    fn store(&self, _connection_id: &str, _path: &str, _content: String) {
        // No-op: disk VFS doesn't cache
    }

    fn cached_hashes(&self, _connection_id: &str) -> Vec<(String, String)> {
        vec![]
    }

    fn validate_hashes(
        &self,
        _connection_id: &str,
        _client_hashes: &[(String, String)],
    ) -> Vec<String> {
        vec![]
    }

    fn mark_stale(&self, _connection_id: &str) {
        // No-op
    }

    fn clear_connection(&self, _connection_id: &str) {
        // No-op
    }
}

fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vfs() -> CachedFileSystem {
        CachedFileSystem::new(Duration::from_secs(3600))
    }

    #[test]
    fn test_store_and_read() {
        let vfs = make_vfs();
        vfs.store("conn1", "/app/main.py", "x = 42\n".to_string());
        let content = vfs.read_to_string("/app/main.py").unwrap();
        assert_eq!(content, "x = 42\n");
    }

    #[test]
    fn test_hash_computed_correctly() {
        let vfs = make_vfs();
        let content = "hello world";
        vfs.store("conn1", "/f.py", content.to_string());

        let hashes = vfs.cached_hashes("conn1");
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0].0, "/f.py");
        assert_eq!(hashes[0].1, sha256_hex(content));
    }

    #[test]
    fn test_disk_fallback() {
        let vfs = make_vfs();
        // Create a real temp file for disk fallback
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.py");
        std::fs::write(&file, "disk content").unwrap();

        let content = vfs.read_to_string(file.to_str().unwrap()).unwrap();
        assert_eq!(content, "disk content");
    }

    #[test]
    fn test_cache_miss_no_disk() {
        let vfs = make_vfs();
        let result = vfs.read_to_string("/absolutely/no/such/file.py");
        assert!(result.is_err());
    }

    #[test]
    fn test_mark_stale_and_validate() {
        let vfs = make_vfs();
        let content = "code";
        vfs.store("conn1", "/a.py", content.to_string());
        vfs.mark_stale("conn1");

        // Validate with matching hash → un-stale
        let evicted = vfs.validate_hashes("conn1", &[("/a.py".into(), sha256_hex(content))]);
        assert!(evicted.is_empty());

        // Content still readable
        assert_eq!(vfs.read_to_string("/a.py").unwrap(), "code");
    }

    #[test]
    fn test_validate_evicts_mismatched() {
        let vfs = make_vfs();
        vfs.store("conn1", "/a.py", "old code".to_string());

        let evicted = vfs.validate_hashes("conn1", &[("/a.py".into(), "badhash".into())]);
        assert_eq!(evicted, vec!["/a.py"]);

        // File no longer in cache (and no disk fallback for this path)
        assert!(vfs.read_to_string("/a.py").is_err());
    }

    #[test]
    fn test_validate_keeps_matched() {
        let vfs = make_vfs();
        let content = "good code";
        vfs.store("conn1", "/a.py", content.to_string());
        vfs.mark_stale("conn1");

        let evicted = vfs.validate_hashes("conn1", &[("/a.py".into(), sha256_hex(content))]);
        assert!(evicted.is_empty());

        assert_eq!(vfs.read_to_string("/a.py").unwrap(), "good code");
    }

    #[test]
    fn test_hot_reload_ttl() {
        // Use very short TTL
        let vfs = CachedFileSystem::new(Duration::from_millis(1));
        vfs.store("conn1", "/a.py", "code".to_string());

        // Wait for TTL to expire
        std::thread::sleep(Duration::from_millis(10));

        // Entry should become stale on next read (but still returns content)
        let content = vfs.read_to_string("/a.py").unwrap();
        assert_eq!(content, "code");

        // Verify entry is stale by checking that validate can act on it
        let cache = vfs.cache.read().unwrap();
        let entry = cache
            .get(&("conn1".to_string(), "/a.py".to_string()))
            .unwrap();
        assert!(entry.stale);
    }

    #[test]
    fn test_hot_reload_hash_restart_ttl() {
        let vfs = CachedFileSystem::new(Duration::from_millis(1));
        let content = "code";
        vfs.store("conn1", "/a.py", content.to_string());

        // Wait for TTL to expire
        std::thread::sleep(Duration::from_millis(10));

        // Force TTL check
        vfs.check_ttl_expiry("/a.py");

        // Validate hash → should restart TTL (un-stale)
        let evicted = vfs.validate_hashes("conn1", &[("/a.py".into(), sha256_hex(content))]);
        assert!(evicted.is_empty());

        // Entry should be un-staled
        let cache = vfs.cache.read().unwrap();
        let entry = cache
            .get(&("conn1".to_string(), "/a.py".to_string()))
            .unwrap();
        assert!(!entry.stale);
    }

    #[test]
    fn test_clear_connection() {
        let vfs = make_vfs();
        vfs.store("conn1", "/a.py", "code a".to_string());
        vfs.store("conn1", "/b.py", "code b".to_string());
        vfs.store("conn2", "/c.py", "code c".to_string());

        vfs.clear_connection("conn1");

        // conn1 files gone
        assert!(vfs.cached_hashes("conn1").is_empty());
        // conn2 untouched
        assert_eq!(vfs.cached_hashes("conn2").len(), 1);
        assert_eq!(vfs.read_to_string("/c.py").unwrap(), "code c");
    }

    #[test]
    fn test_connection_isolation() {
        let vfs = make_vfs();
        vfs.store("conn1", "/shared.py", "version A".to_string());
        vfs.store("conn2", "/shared.py", "version B".to_string());

        let h1 = vfs.cached_hashes("conn1");
        let h2 = vfs.cached_hashes("conn2");
        assert_eq!(h1.len(), 1);
        assert_eq!(h2.len(), 1);
        assert_ne!(h1[0].1, h2[0].1);

        vfs.clear_connection("conn1");
        assert!(vfs.cached_hashes("conn1").is_empty());
        assert_eq!(vfs.cached_hashes("conn2").len(), 1);
    }

    #[test]
    fn test_thread_safety() {
        use std::sync::Arc;
        use std::thread;

        let vfs = Arc::new(make_vfs());
        let mut handles = Vec::new();

        // Concurrent writes
        for i in 0..10 {
            let vfs = Arc::clone(&vfs);
            handles.push(thread::spawn(move || {
                let path = format!("/file_{}.py", i);
                let content = format!("content {}", i);
                vfs.store("conn1", &path, content.clone());
                assert_eq!(vfs.read_to_string(&path).unwrap(), content);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // All 10 files should be cached
        assert_eq!(vfs.cached_hashes("conn1").len(), 10);
    }

    #[test]
    fn test_exists() {
        let vfs = make_vfs();
        assert!(!vfs.exists("/no/such/file.py").unwrap());

        vfs.store("conn1", "/cached.py", "code".to_string());
        assert!(vfs.exists("/cached.py").unwrap());
    }
}
