//! File Source Chain — orchestrates transparent file fetching
//!
//! Tries each configured file source in priority order. The first source
//! that returns content wins; the result is cached in the VFS for
//! subsequent sync reads by `FileInspectionService`.
//!
//! # Design
//!
//! - **VFS cache always checked first** (instant, no network).
//! - Sources are ordered by `vfs.source_priority` from config.
//! - Unknown source names in config are silently skipped.
//! - Errors from individual sources are logged and skipped.
//! - If all sources fail, returns `Ok(())` — downstream will get the
//!   "file not found" error from `FileInspectionService.inspect()`.

use detrix_config::SourceKind;
use detrix_core::Connection;
use detrix_logging::{debug, warn};
use detrix_ports::{FileSourceRef, VfsRef};

/// Orchestrator for pluggable file source chain.
///
/// Created at startup from config, shared across all handlers via `AppContext`.
#[derive(Clone)]
pub struct FileSourceChain {
    sources: Vec<FileSourceRef>,
    vfs: VfsRef,
}

impl FileSourceChain {
    /// Create a new chain with sources ordered by config priority.
    ///
    /// Sources not matching any name in `priority` are excluded.
    /// Duplicate names in priority list are handled (first match wins).
    pub fn new(
        vfs: VfsRef,
        available_sources: Vec<FileSourceRef>,
        priority: &[SourceKind],
    ) -> Self {
        let mut sources = Vec::new();
        for kind in priority {
            if let Some(source) = available_sources.iter().find(|s| s.name() == kind.as_str()) {
                sources.push(source.clone());
            } else {
                debug!(source = %kind, "File source not available, skipping");
            }
        }
        Self { sources, vfs }
    }

    /// Ensure a file is available in the VFS cache.
    ///
    /// 1. Check VFS cache — if cached, return immediately.
    /// 2. Try each source in priority order.
    /// 3. First success → store in VFS → return Ok.
    /// 4. All sources failed → return Ok (let downstream handle missing file).
    pub async fn ensure_available(
        &self,
        connection: &Connection,
        file_path: &str,
    ) -> Result<(), detrix_core::Error> {
        // Fast path: already cached
        if self.vfs.exists(file_path).unwrap_or(false) {
            return Ok(());
        }

        // Try each source in priority order
        for source in &self.sources {
            match source.fetch(connection, file_path).await {
                Ok(Some(result)) => {
                    debug!(
                        source = source.name(),
                        file = file_path,
                        bytes = result.content.len(),
                        "File fetched from remote source"
                    );
                    self.vfs.store_with_metadata(
                        &connection.id.0,
                        file_path,
                        result.content,
                        result.metadata,
                    );
                    return Ok(());
                }
                Ok(None) => {
                    debug!(
                        source = source.name(),
                        file = file_path,
                        "File not available from source"
                    );
                }
                Err(e) => {
                    warn!(
                        source = source.name(),
                        file = file_path,
                        error = %e,
                        "File source error, trying next"
                    );
                }
            }
        }

        // All sources exhausted — that's OK, downstream will handle it
        debug!(file = file_path, "File not available from any source");
        Ok(())
    }
}

impl std::fmt::Debug for FileSourceChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.sources.iter().map(|s| s.name()).collect();
        f.debug_struct("FileSourceChain")
            .field("sources", &names)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use detrix_core::{Connection, ConnectionIdentity, SourceLanguage};
    use detrix_ports::{FetchResult, FileSource, SourceMetadata, VirtualFileSystem};
    use detrix_testing::MockVfs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Inline mock file source for testing.
    struct MockFileSource {
        source_name: &'static str,
        response: Mutex<Option<String>>,
        call_count: AtomicUsize,
    }

    impl MockFileSource {
        fn returning(name: &'static str, content: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                source_name: name,
                response: Mutex::new(content.map(|s| s.to_string())),
                call_count: AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl FileSource for MockFileSource {
        fn name(&self) -> &str {
            self.source_name
        }

        async fn fetch(
            &self,
            _connection: &Connection,
            _file_path: &str,
        ) -> detrix_core::Result<Option<FetchResult>> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .response
                .lock()
                .unwrap()
                .clone()
                .map(|content| FetchResult {
                    content,
                    metadata: SourceMetadata {
                        source_kind: self.source_name.to_string(),
                        ..Default::default()
                    },
                }))
        }
    }

    fn test_connection() -> Connection {
        let identity = ConnectionIdentity::new(
            "test-app",
            SourceLanguage::Python,
            "/workspace",
            "test-host",
        );
        Connection::new_with_identity(identity, "127.0.0.1".into(), 5678).unwrap()
    }

    #[tokio::test]
    async fn test_cache_hit_skips_sources() {
        let vfs = Arc::new(MockVfs::new());
        vfs.store("conn1", "/app/main.py", "cached content".to_string());

        let source = MockFileSource::returning("disk", Some("disk content"));
        let chain = FileSourceChain::new(
            vfs,
            vec![source.clone() as FileSourceRef],
            &[SourceKind::Disk],
        );

        let conn = test_connection();
        chain.ensure_available(&conn, "/app/main.py").await.unwrap();

        assert_eq!(
            source.calls(),
            0,
            "Source should not be called when VFS has the file"
        );
    }

    #[tokio::test]
    async fn test_first_source_success() {
        let vfs = Arc::new(MockVfs::new());
        let source = MockFileSource::returning("disk", Some("fetched content"));

        let chain = FileSourceChain::new(
            vfs.clone(),
            vec![source.clone() as FileSourceRef],
            &[SourceKind::Disk],
        );

        let conn = test_connection();
        chain.ensure_available(&conn, "/app/main.py").await.unwrap();

        assert_eq!(source.calls(), 1);
        assert!(
            vfs.exists("/app/main.py").unwrap(),
            "File should be cached in VFS"
        );
        assert_eq!(
            vfs.read_to_string("/app/main.py").unwrap(),
            "fetched content"
        );
    }

    #[tokio::test]
    async fn test_fallthrough_to_second_source() {
        let vfs = Arc::new(MockVfs::new());
        let source1 = MockFileSource::returning("control_plane", None);
        let source2 = MockFileSource::returning("disk", Some("from disk"));

        let chain = FileSourceChain::new(
            vfs.clone(),
            vec![
                source1.clone() as FileSourceRef,
                source2.clone() as FileSourceRef,
            ],
            &[SourceKind::ControlPlane, SourceKind::Disk],
        );

        let conn = test_connection();
        chain.ensure_available(&conn, "/app/main.py").await.unwrap();

        assert_eq!(source1.calls(), 1, "First source should be tried");
        assert_eq!(
            source2.calls(),
            1,
            "Second source should be tried after first returns None"
        );
        assert_eq!(vfs.read_to_string("/app/main.py").unwrap(), "from disk");
    }

    #[tokio::test]
    async fn test_all_sources_fail_graceful() {
        let vfs = Arc::new(MockVfs::new());
        let source1 = MockFileSource::returning("control_plane", None);
        let source2 = MockFileSource::returning("disk", None);

        let chain = FileSourceChain::new(
            vfs.clone(),
            vec![
                source1.clone() as FileSourceRef,
                source2.clone() as FileSourceRef,
            ],
            &[SourceKind::ControlPlane, SourceKind::Disk],
        );

        let conn = test_connection();
        let result = chain.ensure_available(&conn, "/no/such/file.py").await;

        assert!(
            result.is_ok(),
            "Should return Ok(()) even when all sources fail"
        );
        assert!(
            !vfs.exists("/no/such/file.py").unwrap(),
            "File should NOT be in VFS"
        );
    }

    #[tokio::test]
    async fn test_priority_ordering() {
        let vfs = Arc::new(MockVfs::new());
        let disk = MockFileSource::returning("disk", Some("disk version"));
        let cp = MockFileSource::returning("control_plane", Some("cp version"));

        // Priority: disk first, then control_plane
        let chain = FileSourceChain::new(
            vfs.clone(),
            vec![disk.clone() as FileSourceRef, cp.clone() as FileSourceRef],
            &[SourceKind::Disk, SourceKind::ControlPlane],
        );

        let conn = test_connection();
        chain.ensure_available(&conn, "/app/main.py").await.unwrap();

        assert_eq!(disk.calls(), 1, "Disk source should be tried first");
        assert_eq!(
            cp.calls(),
            0,
            "CP source should NOT be tried (disk succeeded)"
        );
        assert_eq!(vfs.read_to_string("/app/main.py").unwrap(), "disk version");
    }

    #[tokio::test]
    async fn test_empty_priority() {
        let vfs = Arc::new(MockVfs::new());
        let disk = MockFileSource::returning("disk", Some("content"));

        // Empty priority — no sources active
        let chain = FileSourceChain::new(vfs.clone(), vec![disk.clone() as FileSourceRef], &[]);

        let conn = test_connection();
        chain.ensure_available(&conn, "/app/main.py").await.unwrap();

        assert_eq!(
            disk.calls(),
            0,
            "No sources should be called with empty priority"
        );
        assert!(!vfs.exists("/app/main.py").unwrap());
    }

    #[tokio::test]
    async fn test_cached_after_fetch() {
        let vfs = Arc::new(MockVfs::new());
        let source = MockFileSource::returning("disk", Some("content"));

        let chain = FileSourceChain::new(
            vfs.clone(),
            vec![source.clone() as FileSourceRef],
            &[SourceKind::Disk],
        );

        let conn = test_connection();

        // First call — fetches from source
        chain.ensure_available(&conn, "/app/main.py").await.unwrap();
        assert_eq!(source.calls(), 1);

        // Second call — VFS cache hit, no source call
        chain.ensure_available(&conn, "/app/main.py").await.unwrap();
        assert_eq!(
            source.calls(),
            1,
            "Source should NOT be called again (cache hit)"
        );
    }
}
