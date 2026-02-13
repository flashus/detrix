//! Lightweight HTTP file server for the MCP bridge.
//!
//! Serves local files to the Detrix daemon so it can transparently fetch
//! source files from the developer's machine when running in bridge mode.
//!
//! When a `commit` field is present in the request, the server serves the file
//! from that git commit via `git show`, with drift detection against the
//! working tree.
//!
//! The server binds to `127.0.0.1:0` (random port, localhost only) and
//! exposes a single endpoint: `POST /detrix/files/read`.

use axum::{extract::Json, http::StatusCode, response::IntoResponse, routing::post, Router};
use detrix_logging::{debug, info, warn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::net::TcpListener;

/// Maximum file size to serve (10 MB).
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

#[derive(Deserialize)]
struct ReadFileRequest {
    path: String,
    commit: Option<String>,
    workspace_root: Option<String>,
}

#[derive(Serialize)]
struct ReadFileResponse {
    content: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    differs_from_local: Option<bool>,
}

/// Start the bridge file server on a random localhost port.
///
/// Returns the actual port the server is listening on.
/// The server runs in a background tokio task and will stop when the runtime shuts down.
pub async fn start_file_server() -> anyhow::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let app = Router::new().route("/detrix/files/read", post(handle_read_file));

    info!(port, "Bridge file server started");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            warn!(error = %e, "Bridge file server error");
        }
    });

    Ok(port)
}

async fn handle_read_file(Json(req): Json<ReadFileRequest>) -> impl IntoResponse {
    let file_path = req.path.trim();
    if file_path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Missing 'path'"}).to_string(),
        );
    }

    let path = Path::new(file_path);

    // Only serve absolute paths (daemon resolves relative paths against workspace_root
    // before asking the bridge)
    if !path.is_absolute() {
        debug!(path = file_path, "Rejected non-absolute path");
        return (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Path must be absolute"}).to_string(),
        );
    }

    // Try git-pinned serving if commit and workspace_root are provided
    if let (Some(commit), Some(workspace_root)) = (&req.commit, &req.workspace_root) {
        return handle_git_pinned(file_path, commit, workspace_root).await;
    }

    // Standard disk serving
    handle_disk_read(file_path).await
}

/// Serve file from `git show commit:relative_path` with drift detection.
async fn handle_git_pinned(
    file_path: &str,
    commit: &str,
    workspace_root: &str,
) -> (StatusCode, String) {
    // Compute relative path by stripping workspace_root prefix
    let relative_path = match Path::new(file_path).strip_prefix(workspace_root) {
        Ok(rel) => rel.to_string_lossy().to_string(),
        Err(_) => {
            debug!(
                file = file_path,
                workspace_root, "File path not under workspace root, falling back to disk"
            );
            return handle_disk_read(file_path).await;
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

            let response = ReadFileResponse {
                content: git_content,
                source: "git".to_string(),
                commit: Some(commit.to_string()),
                differs_from_local,
            };
            (StatusCode::OK, serde_json::to_string(&response).unwrap())
        }
        Err(err) => {
            debug!(
                file = file_path,
                commit,
                error = %err,
                "git show failed, falling back to disk"
            );
            handle_disk_read(file_path).await
        }
    }
}

/// Standard disk file reading.
async fn handle_disk_read(file_path: &str) -> (StatusCode, String) {
    let path = Path::new(file_path);

    // Check file exists
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return (StatusCode::NOT_FOUND, String::new()),
    };

    // Check file size
    if metadata.len() > MAX_FILE_SIZE {
        warn!(
            path = file_path,
            size = metadata.len(),
            "File exceeds maximum size"
        );
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            serde_json::json!({"error": "File exceeds maximum size"}).to_string(),
        );
    }

    // Read file content
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let response = ReadFileResponse {
                content,
                source: "disk".to_string(),
                commit: None,
                differs_from_local: None,
            };
            (StatusCode::OK, serde_json::to_string(&response).unwrap())
        }
        Err(e) => {
            debug!(path = file_path, error = %e, "Failed to read file");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"error": format!("Read error: {e}")}).to_string(),
            )
        }
    }
}

/// Run `git show commit:relative_path` and return the file content.
async fn git_show(
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

fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    async fn start_and_client() -> (u16, reqwest::Client) {
        let port = start_file_server().await.unwrap();
        // Small delay to ensure the server is ready
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (port, reqwest::Client::new())
    }

    fn url(port: u16) -> String {
        format!("http://127.0.0.1:{}/detrix/files/read", port)
    }

    fn parse_response(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("Response should be valid JSON")
    }

    #[tokio::test]
    async fn test_read_existing_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "hello from bridge").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let (port, client) = start_and_client().await;
        let resp = client
            .post(url(port))
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let json = parse_response(&resp.text().await.unwrap());
        assert_eq!(json["content"], "hello from bridge");
        assert_eq!(json["source"], "disk");
    }

    #[tokio::test]
    async fn test_read_nonexistent() {
        let (port, client) = start_and_client().await;
        let resp = client
            .post(url(port))
            .json(&serde_json::json!({ "path": "/nonexistent/abc123.py" }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn test_empty_path_rejected() {
        let (port, client) = start_and_client().await;
        let resp = client
            .post(url(port))
            .json(&serde_json::json!({ "path": "" }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_relative_path_rejected() {
        let (port, client) = start_and_client().await;
        let resp = client
            .post(url(port))
            .json(&serde_json::json!({ "path": "relative/file.py" }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_large_file_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("large.bin");
        {
            let mut f = std::fs::File::create(&file_path).unwrap();
            // Write just over MAX_FILE_SIZE (10MB + 1 byte)
            let chunk = vec![b'A'; 1024 * 1024]; // 1 MB chunk
            for _ in 0..10 {
                f.write_all(&chunk).unwrap();
            }
            f.write_all(&[b'B']).unwrap();
        }

        let (port, client) = start_and_client().await;
        let resp = client
            .post(url(port))
            .json(&serde_json::json!({ "path": file_path.to_str().unwrap() }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 413);
    }

    #[tokio::test]
    async fn test_binds_random_port() {
        let port = start_file_server().await.unwrap();
        assert!(port > 0, "Should bind to a non-zero port");
    }

    #[tokio::test]
    async fn test_json_response_format() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "test content").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let (port, client) = start_and_client().await;
        let resp = client
            .post(url(port))
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let json = parse_response(&resp.text().await.unwrap());
        assert!(json["content"].is_string());
        assert_eq!(json["source"], "disk");
        // commit and differs_from_local should be absent for disk reads
        assert!(json.get("commit").is_none() || json["commit"].is_null());
    }

    #[tokio::test]
    async fn test_git_show_with_commit() {
        // Create a temp git repo with a committed file
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

        // Get commit SHA
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        let commit = String::from_utf8(output.stdout).unwrap().trim().to_string();

        let (port, client) = start_and_client().await;
        let resp = client
            .post(url(port))
            .json(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "commit": commit,
                "workspace_root": repo_path.to_str().unwrap(),
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let json = parse_response(&resp.text().await.unwrap());
        assert_eq!(json["content"], "x = 42\n");
        assert_eq!(json["source"], "git");
        assert_eq!(json["commit"], commit);
        assert_eq!(json["differs_from_local"], false);
    }

    #[tokio::test]
    async fn test_git_show_drift_detection() {
        // Create a temp git repo, commit, then modify working tree
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

        let (port, client) = start_and_client().await;
        let resp = client
            .post(url(port))
            .json(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "commit": commit,
                "workspace_root": repo_path.to_str().unwrap(),
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let json = parse_response(&resp.text().await.unwrap());
        assert_eq!(json["content"], "x = 42\n", "Should serve git version");
        assert_eq!(json["source"], "git");
        assert_eq!(json["differs_from_local"], true, "Should detect drift");
    }

    #[tokio::test]
    async fn test_git_show_fallback_on_bad_commit() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "disk content").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let (port, client) = start_and_client().await;
        let resp = client
            .post(url(port))
            .json(&serde_json::json!({
                "path": path,
                "commit": "deadbeef",
                "workspace_root": "/tmp",
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let json = parse_response(&resp.text().await.unwrap());
        assert_eq!(json["content"], "disk content");
        assert_eq!(json["source"], "disk", "Should fall back to disk");
    }
}
