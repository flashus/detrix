//! File serving service for the MCP bridge.
//!
//! Serves source files to the daemon via the bridge file-serving protocol.
//! Supports disk reads, git-pinned serving (`git show commit:path`), and
//! drift detection (SHA256 compare of git vs working tree content).
//!
//! ## Design Decision
//!
//! This service performs direct I/O (filesystem reads, git CLI invocation)
//! in the application layer. Strictly, these should be behind port traits
//! (FileProvider) with infrastructure implementations. We chose the pragmatic
//! approach: the I/O is simple, deterministic, and well-tested. If file
//! serving grows in complexity, extract to a port trait + infrastructure impl
//! (~135 lines additional). See plan doc for full analysis.

use detrix_logging::{debug, warn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Maximum file size to serve (10 MB).
pub const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Request to read a file from the bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFileRequest {
    /// Absolute file path to read.
    pub path: String,
    /// Git commit SHA — if present, serve from `git show commit:path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Workspace root — used to compute the relative path for `git show`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
}

/// Response from reading a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFileResponse {
    /// File content.
    pub content: String,
    /// Source type: "disk" or "git".
    pub source: String,
    /// Git commit SHA (present when source is "git").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Whether the git content differs from the local working tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub differs_from_local: Option<bool>,
}

/// Error type for file serving operations.
#[derive(Debug)]
pub enum FileServingError {
    /// Path is empty.
    EmptyPath,
    /// Path is not absolute.
    RelativePath,
    /// File not found.
    NotFound,
    /// File exceeds MAX_FILE_SIZE.
    TooLarge { size: u64 },
    /// I/O error reading the file.
    ReadError(String),
}

impl std::fmt::Display for FileServingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPath => write!(f, "Missing 'path'"),
            Self::RelativePath => write!(f, "Path must be absolute"),
            Self::NotFound => write!(f, "File not found"),
            Self::TooLarge { size } => write!(f, "File exceeds maximum size ({size} bytes)"),
            Self::ReadError(e) => write!(f, "Read error: {e}"),
        }
    }
}

impl std::error::Error for FileServingError {}

/// Stateless file serving service.
///
/// Handles disk reads, git-pinned serving, and drift detection.
/// Does NOT handle TCP binding or HTTP routing — that remains in callers.
pub struct FileServingService;

impl FileServingService {
    pub fn new() -> Self {
        Self
    }

    /// Read a file based on the request parameters.
    ///
    /// If `commit` and `workspace_root` are provided, attempts git-pinned serving
    /// with drift detection. Falls back to disk read on git failure.
    pub async fn read_file(
        &self,
        req: &ReadFileRequest,
    ) -> Result<ReadFileResponse, FileServingError> {
        let file_path = req.path.trim();
        if file_path.is_empty() {
            return Err(FileServingError::EmptyPath);
        }

        let path = Path::new(file_path);

        // Only serve absolute paths
        if !path.is_absolute() {
            debug!(path = file_path, "Rejected non-absolute path");
            return Err(FileServingError::RelativePath);
        }

        // Try git-pinned serving if commit and workspace_root are provided
        if let (Some(commit), Some(workspace_root)) = (&req.commit, &req.workspace_root) {
            return self
                .read_git_pinned(file_path, commit, workspace_root)
                .await;
        }

        // Standard disk serving
        self.read_disk(file_path)
    }

    /// Serve file from `git show commit:relative_path` with drift detection.
    async fn read_git_pinned(
        &self,
        file_path: &str,
        commit: &str,
        workspace_root: &str,
    ) -> Result<ReadFileResponse, FileServingError> {
        // Compute relative path by stripping workspace_root prefix
        let relative_path = match Path::new(file_path).strip_prefix(workspace_root) {
            Ok(rel) => rel.to_string_lossy().to_string(),
            Err(_) => {
                debug!(
                    file = file_path,
                    workspace_root, "File path not under workspace root, falling back to disk"
                );
                return self.read_disk(file_path);
            }
        };

        // Try git show
        match git_show(workspace_root, commit, &relative_path).await {
            Ok(git_content) => {
                // Drift detection: compare git content with disk
                let differs_from_local = match std::fs::read_to_string(file_path) {
                    Ok(disk_content) => Some(sha256_hex(&git_content) != sha256_hex(&disk_content)),
                    Err(_) => None, // No local file to compare
                };

                if differs_from_local == Some(true) {
                    debug!(
                        file = file_path,
                        commit, "Git content differs from local working tree"
                    );
                }

                Ok(ReadFileResponse {
                    content: git_content,
                    source: "git".to_string(),
                    commit: Some(commit.to_string()),
                    differs_from_local,
                })
            }
            Err(err) => {
                debug!(
                    file = file_path,
                    commit,
                    error = %err,
                    "git show failed, falling back to disk"
                );
                self.read_disk(file_path)
            }
        }
    }

    /// Standard disk file reading.
    fn read_disk(&self, file_path: &str) -> Result<ReadFileResponse, FileServingError> {
        let path = Path::new(file_path);

        // Check file exists
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return Err(FileServingError::NotFound),
        };

        // Check file size
        if metadata.len() > MAX_FILE_SIZE {
            warn!(
                path = file_path,
                size = metadata.len(),
                "File exceeds maximum size"
            );
            return Err(FileServingError::TooLarge {
                size: metadata.len(),
            });
        }

        // Read file content
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(ReadFileResponse {
                content,
                source: "disk".to_string(),
                commit: None,
                differs_from_local: None,
            }),
            Err(e) => {
                debug!(path = file_path, error = %e, "Failed to read file");
                Err(FileServingError::ReadError(e.to_string()))
            }
        }
    }
}

impl Default for FileServingService {
    fn default() -> Self {
        Self::new()
    }
}

/// Run `git show commit:relative_path` and return the file content.
pub async fn git_show(
    workspace_root: &str,
    commit: &str,
    relative_path: &str,
) -> Result<String, String> {
    let git_ref = format!("{}:{}", commit, relative_path);
    let output = tokio::process::Command::new("git")
        .args(["-C", workspace_root, "show", &git_ref])
        .output()
        .await
        .map_err(|e| format!("Failed to execute git: {}", e))?;

    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|e| format!("Git output is not valid UTF-8: {}", e))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git show failed: {}", stderr.trim()))
    }
}

/// Compute SHA256 hex digest of the given string.
pub fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn test_read_existing_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "hello from service").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let svc = FileServingService::new();
        let req = ReadFileRequest {
            path,
            commit: None,
            workspace_root: None,
        };
        let resp = svc.read_file(&req).await.unwrap();
        assert_eq!(resp.content, "hello from service");
        assert_eq!(resp.source, "disk");
        assert!(resp.commit.is_none());
        assert!(resp.differs_from_local.is_none());
    }

    #[tokio::test]
    async fn test_read_nonexistent() {
        let svc = FileServingService::new();
        let req = ReadFileRequest {
            path: "/nonexistent/abc123.py".to_string(),
            commit: None,
            workspace_root: None,
        };
        let result = svc.read_file(&req).await;
        assert!(matches!(result, Err(FileServingError::NotFound)));
    }

    #[tokio::test]
    async fn test_empty_path_rejected() {
        let svc = FileServingService::new();
        let req = ReadFileRequest {
            path: String::new(),
            commit: None,
            workspace_root: None,
        };
        let result = svc.read_file(&req).await;
        assert!(matches!(result, Err(FileServingError::EmptyPath)));
    }

    #[tokio::test]
    async fn test_relative_path_rejected() {
        let svc = FileServingService::new();
        let req = ReadFileRequest {
            path: "relative/file.py".to_string(),
            commit: None,
            workspace_root: None,
        };
        let result = svc.read_file(&req).await;
        assert!(matches!(result, Err(FileServingError::RelativePath)));
    }

    #[tokio::test]
    async fn test_large_file_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("large.bin");
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            let chunk = vec![b'A'; 1024 * 1024]; // 1 MB chunk
            for _ in 0..10 {
                f.write_all(&chunk).unwrap();
            }
            f.write_all(&[b'B']).unwrap();
        }

        let svc = FileServingService::new();
        let req = ReadFileRequest {
            path: file_path.to_str().unwrap().to_string(),
            commit: None,
            workspace_root: None,
        };
        let result = svc.read_file(&req).await;
        assert!(matches!(result, Err(FileServingError::TooLarge { .. })));
    }

    #[tokio::test]
    async fn test_git_show_with_commit() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path();
        let file_path = repo_path.join("app.py");

        // Init repo and commit
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .output()
            .unwrap();

        std::fs::write(&file_path, "x = 42\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "app.py"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo_path)
            .output()
            .unwrap();

        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        let commit = String::from_utf8(output.stdout).unwrap().trim().to_string();

        let svc = FileServingService::new();
        let req = ReadFileRequest {
            path: file_path.to_str().unwrap().to_string(),
            commit: Some(commit.clone()),
            workspace_root: Some(repo_path.to_str().unwrap().to_string()),
        };
        let resp = svc.read_file(&req).await.unwrap();
        assert_eq!(resp.content, "x = 42\n");
        assert_eq!(resp.source, "git");
        assert_eq!(resp.commit.as_deref(), Some(commit.as_str()));
        assert_eq!(resp.differs_from_local, Some(false));
    }

    #[tokio::test]
    async fn test_git_show_drift_detection() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path();
        let file_path = repo_path.join("app.py");

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .output()
            .unwrap();

        std::fs::write(&file_path, "x = 42\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "app.py"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo_path)
            .output()
            .unwrap();

        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        let commit = String::from_utf8(output.stdout).unwrap().trim().to_string();

        // Modify working tree
        std::fs::write(&file_path, "x = 99\n").unwrap();

        let svc = FileServingService::new();
        let req = ReadFileRequest {
            path: file_path.to_str().unwrap().to_string(),
            commit: Some(commit),
            workspace_root: Some(repo_path.to_str().unwrap().to_string()),
        };
        let resp = svc.read_file(&req).await.unwrap();
        assert_eq!(resp.content, "x = 42\n", "Should serve git version");
        assert_eq!(resp.source, "git");
        assert_eq!(resp.differs_from_local, Some(true), "Should detect drift");
    }

    #[tokio::test]
    async fn test_git_show_fallback_on_bad_commit() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "disk content").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let svc = FileServingService::new();
        let req = ReadFileRequest {
            path,
            commit: Some("deadbeef".to_string()),
            workspace_root: Some("/tmp".to_string()),
        };
        let resp = svc.read_file(&req).await.unwrap();
        assert_eq!(resp.content, "disk content");
        assert_eq!(resp.source, "disk", "Should fall back to disk");
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex("hello");
        assert_eq!(hash.len(), 64); // SHA256 produces 32 bytes = 64 hex chars
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_read_file_request_serialization() {
        let req = ReadFileRequest {
            path: "/app/main.py".to_string(),
            commit: Some("abc123".to_string()),
            workspace_root: Some("/app".to_string()),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["path"], "/app/main.py");
        assert_eq!(json["commit"], "abc123");
        assert_eq!(json["workspace_root"], "/app");

        // None fields should be omitted
        let req2 = ReadFileRequest {
            path: "/app/main.py".to_string(),
            commit: None,
            workspace_root: None,
        };
        let json2 = serde_json::to_value(&req2).unwrap();
        assert_eq!(json2["path"], "/app/main.py");
        assert!(json2.get("commit").is_none());
        assert!(json2.get("workspace_root").is_none());
    }
}
