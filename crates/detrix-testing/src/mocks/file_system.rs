//! Mock Virtual File System for testing
//!
//! HashMap-based in-memory implementation with no disk fallback.

use detrix_core::Result;
use detrix_ports::VirtualFileSystem;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::RwLock;

/// A mock VFS backed by an in-memory HashMap.
///
/// Useful in unit tests where no real file system access is needed.
/// Files can be pre-populated via [`MockVfs::add_file`].
#[allow(dead_code)] // Test infrastructure not yet integrated - used in internal tests
#[derive(Debug, Default)]
pub struct MockVfs {
    /// (connection_id, path) → CacheEntry
    cache: RwLock<HashMap<(String, String), MockCacheEntry>>,
    /// Standalone files not scoped to any connection (simulates disk)
    disk: RwLock<HashMap<String, String>>,
}

#[allow(dead_code)] // Internal struct used by MockVfs
#[derive(Debug, Clone)]
struct MockCacheEntry {
    content: String,
    hash: String,
    stale: bool,
}

impl MockVfs {
    #[allow(dead_code)] // Test helper - used in internal tests
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-populate a file on the mock "disk" (not scoped to any connection).
    #[allow(dead_code)] // Test helper - used in internal tests
    pub fn add_disk_file(&self, path: impl Into<String>, content: impl Into<String>) {
        self.disk
            .write()
            .expect("MockVfs disk lock poisoned")
            .insert(path.into(), content.into());
    }

    /// Pre-populate a file in the cache for a specific connection.
    #[allow(dead_code)] // Test helper - used in internal tests
    pub fn add_file(
        &self,
        connection_id: impl Into<String>,
        path: impl Into<String>,
        content: impl Into<String>,
    ) {
        let content = content.into();
        let hash = sha256_hex(&content);
        self.cache
            .write()
            .expect("MockVfs cache lock poisoned")
            .insert(
                (connection_id.into(), path.into()),
                MockCacheEntry {
                    content,
                    hash,
                    stale: false,
                },
            );
    }
}

impl VirtualFileSystem for MockVfs {
    fn read_to_string(&self, path: &str) -> Result<String> {
        // Check cache first (any connection)
        let cache = self.cache.read().expect("MockVfs cache lock poisoned");
        for ((_, p), entry) in cache.iter() {
            if p == path {
                return Ok(entry.content.clone());
            }
        }
        drop(cache);

        // Check mock disk
        let disk = self.disk.read().expect("MockVfs disk lock poisoned");
        if let Some(content) = disk.get(path) {
            return Ok(content.clone());
        }

        Err(detrix_core::Error::FileNotFound(format!(
            "File not found: {}",
            path
        )))
    }

    fn exists(&self, path: &str) -> Result<bool> {
        // Check cache
        let cache = self.cache.read().expect("MockVfs cache lock poisoned");
        for (_, p) in cache.keys() {
            if p == path {
                return Ok(true);
            }
        }
        drop(cache);

        // Check mock disk
        let disk = self.disk.read().expect("MockVfs disk lock poisoned");
        Ok(disk.contains_key(path))
    }

    fn store(&self, connection_id: &str, path: &str, content: String) {
        let hash = sha256_hex(&content);
        self.cache
            .write()
            .expect("MockVfs cache lock poisoned")
            .insert(
                (connection_id.to_string(), path.to_string()),
                MockCacheEntry {
                    content,
                    hash,
                    stale: false,
                },
            );
    }

    fn cached_hashes(&self, connection_id: &str) -> Vec<(String, String)> {
        let cache = self.cache.read().expect("MockVfs cache lock poisoned");
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
        let mut cache = self.cache.write().expect("MockVfs cache lock poisoned");
        let mut evicted = Vec::new();

        let client_map: HashMap<&str, &str> = client_hashes
            .iter()
            .map(|(p, h)| (p.as_str(), h.as_str()))
            .collect();

        // Collect keys to process
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
                    // Match → un-stale
                    entry.stale = false;
                } else {
                    // Mismatch → evict
                    evicted.push(path.clone());
                    cache.remove(&key);
                }
            }
            // Files not in client_hashes are left as-is (client may not know about them)
        }

        evicted
    }

    fn mark_stale(&self, connection_id: &str) {
        let mut cache = self.cache.write().expect("MockVfs cache lock poisoned");
        for ((cid, _), entry) in cache.iter_mut() {
            if cid == connection_id {
                entry.stale = true;
            }
        }
    }

    fn clear_connection(&self, connection_id: &str) {
        let mut cache = self.cache.write().expect("MockVfs cache lock poisoned");
        cache.retain(|(cid, _), _| cid != connection_id);
    }
}

#[allow(dead_code)] // Helper function used by MockVfs and tests
fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_read() {
        let vfs = MockVfs::new();
        vfs.store("conn1", "/app/main.py", "x = 42\n".to_string());
        let content = vfs.read_to_string("/app/main.py").unwrap();
        assert_eq!(content, "x = 42\n");
    }

    #[test]
    fn test_hash_computed_correctly() {
        let vfs = MockVfs::new();
        let content = "hello world";
        vfs.store("conn1", "/f.py", content.to_string());

        let hashes = vfs.cached_hashes("conn1");
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0].0, "/f.py");
        assert_eq!(hashes[0].1, sha256_hex(content));
    }

    #[test]
    fn test_disk_fallback() {
        let vfs = MockVfs::new();
        vfs.add_disk_file("/on/disk.py", "disk content");
        let content = vfs.read_to_string("/on/disk.py").unwrap();
        assert_eq!(content, "disk content");
    }

    #[test]
    fn test_cache_miss_no_disk() {
        let vfs = MockVfs::new();
        let result = vfs.read_to_string("/no/such/file.py");
        assert!(result.is_err());
    }

    #[test]
    fn test_mark_stale_and_validate() {
        let vfs = MockVfs::new();
        let content = "code";
        vfs.store("conn1", "/a.py", content.to_string());

        vfs.mark_stale("conn1");

        // Validate with matching hash → should un-stale (no eviction)
        let evicted = vfs.validate_hashes("conn1", &[("/a.py".into(), sha256_hex(content))]);
        assert!(evicted.is_empty());
    }

    #[test]
    fn test_validate_evicts_mismatched() {
        let vfs = MockVfs::new();
        vfs.store("conn1", "/a.py", "old code".to_string());

        // Validate with different hash → should evict
        let evicted = vfs.validate_hashes("conn1", &[("/a.py".into(), "badhash".into())]);
        assert_eq!(evicted, vec!["/a.py"]);

        // File should no longer be readable from cache
        let result = vfs.read_to_string("/a.py");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_keeps_matched() {
        let vfs = MockVfs::new();
        let content = "good code";
        vfs.store("conn1", "/a.py", content.to_string());
        vfs.mark_stale("conn1");

        let evicted = vfs.validate_hashes("conn1", &[("/a.py".into(), sha256_hex(content))]);
        assert!(evicted.is_empty());

        // File should still be readable
        let result = vfs.read_to_string("/a.py").unwrap();
        assert_eq!(result, "good code");
    }

    #[test]
    fn test_clear_connection() {
        let vfs = MockVfs::new();
        vfs.store("conn1", "/a.py", "code a".to_string());
        vfs.store("conn1", "/b.py", "code b".to_string());
        vfs.store("conn2", "/c.py", "code c".to_string());

        vfs.clear_connection("conn1");

        assert!(vfs.read_to_string("/a.py").is_err());
        assert!(vfs.read_to_string("/b.py").is_err());
        // conn2's file still accessible
        assert_eq!(vfs.read_to_string("/c.py").unwrap(), "code c");
    }

    #[test]
    fn test_connection_isolation() {
        let vfs = MockVfs::new();
        vfs.store("conn1", "/shared.py", "version A".to_string());
        vfs.store("conn2", "/shared.py", "version B".to_string());

        // Both are accessible (read_to_string scans all connections)
        // but cached_hashes are connection-scoped
        let h1 = vfs.cached_hashes("conn1");
        let h2 = vfs.cached_hashes("conn2");
        assert_eq!(h1.len(), 1);
        assert_eq!(h2.len(), 1);
        assert_ne!(h1[0].1, h2[0].1); // different content → different hashes

        // clear_connection only affects the targeted connection
        vfs.clear_connection("conn1");
        assert!(vfs.cached_hashes("conn1").is_empty());
        assert_eq!(vfs.cached_hashes("conn2").len(), 1);
    }

    #[test]
    fn test_exists() {
        let vfs = MockVfs::new();
        assert!(!vfs.exists("/a.py").unwrap());

        vfs.store("conn1", "/a.py", "code".to_string());
        assert!(vfs.exists("/a.py").unwrap());

        vfs.add_disk_file("/on/disk.py", "content");
        assert!(vfs.exists("/on/disk.py").unwrap());
    }
}
