//! Disk file source — reads files from the local filesystem.
//!
//! This is the simplest source and the default fallback. Works in local
//! daemon mode where source files are on the same machine.

use async_trait::async_trait;
use detrix_application::{FetchResult, FileSource, SourceMetadata};
use detrix_config::SourceKind;
use detrix_core::{Connection, Result};
use tracing::debug;

pub struct DiskSource;

#[async_trait]
impl FileSource for DiskSource {
    fn name(&self) -> &str {
        SourceKind::Disk.as_str()
    }

    async fn fetch(
        &self,
        _connection: &Connection,
        file_path: &str,
    ) -> Result<Option<FetchResult>> {
        match std::fs::read_to_string(file_path) {
            Ok(content) => {
                debug!(
                    file = file_path,
                    bytes = content.len(),
                    "File read from disk"
                );
                Ok(Some(FetchResult {
                    content,
                    metadata: SourceMetadata {
                        source_kind: SourceKind::Disk.as_str().to_string(),
                        ..Default::default()
                    },
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => {
                debug!(file = file_path, error = %e, "Disk read error");
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_core::{ConnectionIdentity, SourceLanguage};
    use std::io::Write;

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
    async fn test_reads_existing_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "hello world").unwrap();

        let source = DiskSource;
        let conn = test_connection();
        let result = source
            .fetch(&conn, tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let fetch_result = result.expect("Should return Some");
        assert_eq!(fetch_result.content, "hello world");
        assert_eq!(fetch_result.metadata.source_kind, "disk");
    }

    #[tokio::test]
    async fn test_missing_file_returns_none() {
        let source = DiskSource;
        let conn = test_connection();
        let result = source
            .fetch(&conn, "/nonexistent/path/abc123.py")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_name() {
        assert_eq!(DiskSource.name(), "disk");
    }
}
