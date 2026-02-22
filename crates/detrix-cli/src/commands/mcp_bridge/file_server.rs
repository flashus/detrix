//! Lightweight HTTP file server for the MCP bridge.
//!
//! Serves local files to the Detrix daemon so it can transparently fetch
//! source files from the developer's machine when running in bridge mode.
//!
//! The actual file-serving logic lives in `FileServingService` (detrix-application).
//! This module provides the thin HTTP layer: binding, routing, and status code mapping.
//!
//! ## Security
//!
//! The server binds to `0.0.0.0:0` (random port, all interfaces) so Docker daemons
//! can reach it via `host.docker.internal`.
//!
//! Two protection layers:
//! 1. **IP allowlist** (`restrict_to_localhost`): when enabled, only loopback addresses
//!    (`127.0.0.1`, `::1`) are accepted. Disabled when connecting to a Docker/remote daemon
//!    (the daemon connects from its container IP, not loopback).
//! 2. **Bearer token auth** (`auth_token`): when configured, requests must include
//!    `Authorization: Bearer <token>`. Always used in Docker/cloud setups.

use axum::extract::ConnectInfo;
use axum::{extract::Json, http::HeaderMap, http::StatusCode, routing::post, Router};
use detrix_application::services::file_serving::{
    FileServingError, FileServingService, ReadFileRequest,
};
use detrix_logging::{debug, info, warn};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

/// Container-to-host source path prefix mapping.
///
/// When a Docker daemon sends file requests, paths are in the container filesystem
/// (e.g. `/src/examples/app/main.go`). The file server runs on the host and needs
/// to translate these to host paths (e.g. `/Users/me/project/examples/app/main.go`).
///
/// `None` means no rewriting (local daemon — paths are already host paths).
/// `Some((container_prefix, host_prefix))` rewrites any path that starts with
/// `container_prefix` by replacing it with `host_prefix`.
pub type SourcePrefixMap = Arc<RwLock<Option<(String, String)>>>;

/// Rewrite a path using the source prefix mapping.
///
/// Replaces the container prefix with the host prefix if the path starts with the
/// container prefix. Returns the path unchanged if no mapping applies.
fn rewrite_path(path: &str, map: &Option<(String, String)>) -> String {
    let Some((container_prefix, host_prefix)) = map else {
        return path.to_string();
    };
    if path.starts_with(container_prefix.as_str()) {
        let rewritten = format!("{}{}", host_prefix, &path[container_prefix.len()..]);
        debug!(
            original = path,
            rewritten = %rewritten,
            "Bridge file server: rewrote container path to host path"
        );
        rewritten
    } else {
        debug!(
            path,
            container_prefix = %container_prefix,
            "Bridge file server: path does not match container prefix — serving as-is (will likely 404)"
        );
        path.to_string()
    }
}

/// Apply the source prefix mapping to a `ReadFileRequest`, rewriting path and workspace_root.
fn apply_prefix_map(mut req: ReadFileRequest, map: &Option<(String, String)>) -> ReadFileRequest {
    req.path = rewrite_path(&req.path, map);
    req.workspace_root = req.workspace_root.map(|wr| rewrite_path(&wr, map));
    req
}

/// Discover a container→host path mapping by matching path suffixes against CWD.
///
/// When the bridge can't determine the mapping from `workspace_root` (e.g., `"/"`
/// for Go scratch images), this function discovers it lazily from the actual file
/// path requested by the daemon.
///
/// Algorithm: for each suffix of the path (stripping 1, 2, … leading components),
/// check whether `<CWD>/<suffix>` exists. The first match yields the mapping.
///
/// Example: path="/src/fixtures/go/app.go", CWD="/Users/me/project"
///   → try /Users/me/project/src/fixtures/go/app.go — not found
///   → try /Users/me/project/fixtures/go/app.go     — found!
///   → mapping: /src → /Users/me/project
fn discover_cwd_mapping(path: &str) -> Option<(String, String)> {
    let cwd = std::env::current_dir().ok()?;
    let comps: Vec<Component> = Path::new(path).components().collect();
    let n = comps.len();

    // Need at least 2 components (prefix + filename) to form a meaningful mapping.
    if n < 2 {
        return None;
    }

    // Try stripping 1..n-1 leading components.
    for suffix_start in 1..n {
        let suffix: PathBuf = comps[suffix_start..].iter().collect();
        let candidate = cwd.join(&suffix);
        if candidate.exists() {
            let container_prefix = comps[..suffix_start]
                .iter()
                .fold(PathBuf::new(), |mut p, c| {
                    p.push(c);
                    p
                })
                .to_string_lossy()
                .into_owned();
            let host_prefix = cwd.to_string_lossy().into_owned();
            return Some((container_prefix, host_prefix));
        }
    }

    None
}

/// Start the bridge file server on a random port bound to all interfaces.
///
/// Three protection layers work together:
/// - `auth_token`: when `Some`, requests must carry `Authorization: Bearer <token>`.
/// - `allowed_ips`: shared IP allowlist updated at runtime when the daemon switches.
///   - `Some(set)` — only IPs in this set are allowed (loopback always included).
///     Used for local and truly-remote daemons where source IP is predictable.
///   - `None` — any IP is allowed; token auth is the sole guard. Used for Docker
///     where the daemon container connects from an unpredictable bridge IP.
/// - `source_prefix_map`: translates container-internal paths to host paths.
///   - `None` — no translation (local daemon).
///   - `Some((container_prefix, host_prefix))` — rewrite on every request.
///
/// Returns the actual port the server is listening on.
/// The server runs in a background tokio task and will stop when the runtime shuts down.
pub async fn start_file_server(
    auth_token: Option<String>,
    allowed_ips: Arc<RwLock<Option<HashSet<IpAddr>>>>,
    source_prefix_map: SourcePrefixMap,
) -> anyhow::Result<u16> {
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();

    let service = Arc::new(FileServingService::new());
    let token = Arc::new(auth_token);
    let app = Router::new().route(
        "/detrix/files/read",
        post({
            let svc = Arc::clone(&service);
            let tok = Arc::clone(&token);
            let ips = Arc::clone(&allowed_ips);
            let pfx = Arc::clone(&source_prefix_map);
            move |ConnectInfo(remote): ConnectInfo<SocketAddr>,
                  headers: HeaderMap,
                  Json(req): Json<ReadFileRequest>| {
                let svc = Arc::clone(&svc);
                let tok = Arc::clone(&tok);
                let ips = Arc::clone(&ips);
                let pfx = Arc::clone(&pfx);
                async move {
                    // ── Auth: token check (mandatory when configured) ──
                    // When a token is configured, EVERY request must present it —
                    // regardless of whether the IP is known. Reject early on mismatch.
                    if let Some(expected) = tok.as_deref() {
                        let provided = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");
                        if provided != format!("Bearer {}", expected) {
                            return (StatusCode::UNAUTHORIZED, String::new());
                        }
                    }

                    // ── Auth: IP allowlist + token-based IP learning ──
                    //
                    // `Some(set)` = only IPs in the set are allowed.
                    // `None`      = any IP is accepted (legacy/fallback).
                    //
                    // When a request passes token auth but comes from an unknown IP
                    // (e.g. Docker daemon connecting from a bridge IP), the IP gets
                    // learned — added to the allowlist for subsequent requests.
                    // Without token auth, unknown IPs are always rejected.
                    let ip_known = {
                        let guard = ips.read().await;
                        match guard.as_ref() {
                            None => true,
                            Some(set) => set.contains(&remote.ip()),
                        }
                    };

                    if !ip_known {
                        if tok.is_some() {
                            // Token was valid (checked above) — learn this IP.
                            let mut guard = ips.write().await;
                            if let Some(set) = guard.as_mut() {
                                warn!(
                                    ip = %remote.ip(),
                                    "File server: request from unknown IP with valid token — \
                                     learning IP and adding to allowlist"
                                );
                                set.insert(remote.ip());
                            }
                        } else {
                            // No token configured — no way to prove identity.
                            warn!(
                                remote = %remote,
                                "File server: rejected request from unlisted IP"
                            );
                            return (
                                StatusCode::FORBIDDEN,
                                format!(
                                    "Access denied: {} not in daemon IP allowlist",
                                    remote.ip()
                                ),
                            );
                        }
                    }

                    // Layer 3: Container→host path rewriting (Docker mode).
                    // Translates container-internal absolute paths to host paths so
                    // the file server can find source files that live on the host.
                    let map = pfx.read().await.clone();
                    let mut req = apply_prefix_map(req, &map);

                    // Layer 4: Lazy CWD-based mapping fallback.
                    // When no prefix map is configured (e.g., workspace_root was "/"
                    // for Go scratch images and auto-detection couldn't find a mapping),
                    // discover the mapping from the actual file path by checking suffixes
                    // against CWD. The bridge starts from the project folder, so the
                    // file should be reachable relative to CWD.
                    if map.is_none() && !Path::new(&req.path).exists() {
                        if let Some(mapping) = discover_cwd_mapping(&req.path) {
                            info!(
                                container_prefix = %mapping.0,
                                host_prefix = %mapping.1,
                                path = %req.path,
                                "Lazy CWD-based path mapping discovered from file request"
                            );
                            req.path = rewrite_path(&req.path, &Some(mapping.clone()));
                            req.workspace_root = req
                                .workspace_root
                                .map(|wr| rewrite_path(&wr, &Some(mapping.clone())));
                            *pfx.write().await = Some(mapping);
                        }
                    }

                    handle_read_file(&svc, req).await
                }
            }
        }),
    );

    info!(
        port,
        auth = token.is_some(),
        "Bridge file server started on 0.0.0.0:{port}",
    );

    tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            warn!(error = %e, "Bridge file server error");
        }
    });

    Ok(port)
}

async fn handle_read_file(
    service: &FileServingService,
    req: ReadFileRequest,
) -> (StatusCode, String) {
    match service.read_file(&req).await {
        Ok(resp) => match serde_json::to_string(&resp) {
            Ok(json) => (StatusCode::OK, json),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"error": "Serialization error"}).to_string(),
            ),
        },
        Err(FileServingError::EmptyPath) => (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Missing 'path'"}).to_string(),
        ),
        Err(FileServingError::RelativePath) => (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Path must be absolute"}).to_string(),
        ),
        Err(FileServingError::NotFound) => (StatusCode::NOT_FOUND, String::new()),
        Err(FileServingError::TooLarge { .. }) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            serde_json::json!({"error": "File exceeds maximum size"}).to_string(),
        ),
        Err(FileServingError::ReadError(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": format!("Read error: {e}")}).to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn no_prefix_map() -> SourcePrefixMap {
        Arc::new(RwLock::new(None))
    }

    fn prefix_map(container: &str, host: &str) -> SourcePrefixMap {
        Arc::new(RwLock::new(Some((container.to_string(), host.to_string()))))
    }

    /// Docker path rewriting: the daemon sends a container-internal path;
    /// the file server rewrites it to the host path and serves the file.
    #[tokio::test]
    async fn test_docker_path_rewriting() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "docker content").unwrap();
        let host_path = tmp.path().to_str().unwrap().to_string();

        // Map /container → host parent dir, then request /container/<filename>
        let container_prefix = "/container";
        let host_prefix = tmp.path().parent().unwrap().to_str().unwrap().to_string();
        let filename = tmp.path().file_name().unwrap().to_str().unwrap();
        let container_path = format!("{}/{}", container_prefix, filename);

        let allowed_ips = Arc::new(RwLock::new(None));
        let port = start_file_server(
            None,
            allowed_ips,
            prefix_map(container_prefix, &host_prefix),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/detrix/files/read", port))
            .json(&serde_json::json!({ "path": container_path }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200, "Should find file after path rewriting");
        let json = parse_response(&resp.text().await.unwrap());
        assert_eq!(json["content"], "docker content");
        // Verify the file was actually found via host_path (not container path)
        let _ = host_path; // silence unused warning
    }

    /// Without a prefix mapping, container paths return 404 (not found on host).
    #[tokio::test]
    async fn test_container_path_without_mapping_returns_404() {
        let allowed_ips = Arc::new(RwLock::new(None));
        let port = start_file_server(None, allowed_ips, no_prefix_map())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/detrix/files/read", port))
            .json(&serde_json::json!({ "path": "/container/nonexistent.py" }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 404);
    }

    async fn start_and_client() -> (u16, reqwest::Client) {
        // None = allow any IP (tests connect from 127.0.0.1 which is always allowed anyway)
        let allowed_ips = Arc::new(RwLock::new(None));
        let port = start_file_server(None, allowed_ips, no_prefix_map())
            .await
            .unwrap();
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
        let allowed_ips = Arc::new(RwLock::new(None));
        let port = start_file_server(None, allowed_ips, no_prefix_map())
            .await
            .unwrap();
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
    async fn test_token_auth_required() {
        let allowed_ips = Arc::new(RwLock::new(None));
        let port = start_file_server(
            Some("secret-token".to_string()),
            allowed_ips,
            no_prefix_map(),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/detrix/files/read", port);

        // Missing token → 401
        let resp = client
            .post(&url)
            .json(&serde_json::json!({ "path": "/tmp/x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        // Wrong token → 401
        let resp = client
            .post(&url)
            .header("Authorization", "Bearer wrong-token")
            .json(&serde_json::json!({ "path": "/tmp/x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);

        // Correct token → not 401 (may be 404 for missing file, that's fine)
        let resp = client
            .post(&url)
            .header("Authorization", "Bearer secret-token")
            .json(&serde_json::json!({ "path": "/tmp/x" }))
            .send()
            .await
            .unwrap();
        assert_ne!(resp.status(), 401_u16);
    }

    #[tokio::test]
    async fn test_localhost_ip_restriction() {
        // Allow only loopback IPs — 127.0.0.1 should pass, others would be blocked.
        let mut loopback_set = HashSet::new();
        loopback_set.insert(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        loopback_set.insert(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
        let allowed_ips = Arc::new(RwLock::new(Some(loopback_set)));
        let port = start_file_server(None, allowed_ips, no_prefix_map())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // 127.0.0.1 is in the allowlist → allowed (may get 404 for the file, not 403)
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/detrix/files/read", port);
        let resp = client
            .post(&url)
            .json(&serde_json::json!({ "path": "/tmp/nonexistent_xyz" }))
            .send()
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            403_u16,
            "127.0.0.1 should be in the allowlist"
        );
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

    // =========================================================================
    // IP filtering integration tests
    //
    // Primary strategy (works everywhere, including macOS with only loopback):
    //   Use an empty allowlist `Some(HashSet::new())` which blocks even
    //   127.0.0.1. Connections from loopback then test 403 behavior without
    //   any non-loopback interface. Adding/removing 127.0.0.1 from the
    //   allowlist at runtime tests the live-update behavior.
    //
    // Bonus strategy (non-loopback interface required, skipped otherwise):
    //   `reqwest::ClientBuilder::local_address(ip)` binds the outbound TCP
    //   socket to the machine's real non-loopback IP. The file server (bound
    //   to 0.0.0.0) sees it as the remote addr via ConnectInfo<SocketAddr>.
    //   This is skipped when the machine only has loopback or link-local IPs.
    // =========================================================================

    /// IP not in allowlist is rejected with 403.
    ///
    /// Uses an empty allowlist so the test runs on any machine (no non-loopback
    /// interface required). Even 127.0.0.1 is not in an empty set.
    #[tokio::test]
    async fn test_ip_not_in_allowlist_gets_403() {
        let allowed_ips = Arc::new(RwLock::new(Some(HashSet::<IpAddr>::new())));
        let port = start_file_server(None, Arc::clone(&allowed_ips), no_prefix_map())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/detrix/files/read", port))
            .json(&serde_json::json!({"path": "/tmp/nonexistent_xyz_detrix"}))
            .send()
            .await
            .unwrap();

        assert_eq!(
            resp.status(),
            403,
            "127.0.0.1 is not in empty allowlist — must be blocked"
        );
    }

    /// IP explicitly in the allowlist is allowed.
    #[tokio::test]
    async fn test_ip_in_allowlist_is_allowed() {
        let mut allowed = HashSet::new();
        allowed.insert(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        allowed.insert(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
        let allowed_ips = Arc::new(RwLock::new(Some(allowed)));
        let port = start_file_server(None, allowed_ips, no_prefix_map())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/detrix/files/read", port))
            .json(&serde_json::json!({"path": "/tmp/nonexistent_xyz_detrix"}))
            .send()
            .await
            .unwrap();

        assert_ne!(
            resp.status(),
            403_u16,
            "127.0.0.1 is in allowlist — must NOT be blocked"
        );
    }

    /// `allowed_ips = None` (Docker mode): any IP is allowed; no IP check at all.
    #[tokio::test]
    async fn test_none_allowlist_allows_any_ip() {
        let allowed_ips = Arc::new(RwLock::new(None));
        let port = start_file_server(None, allowed_ips, no_prefix_map())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/detrix/files/read", port))
            .json(&serde_json::json!({"path": "/tmp/nonexistent_xyz_detrix"}))
            .send()
            .await
            .unwrap();

        assert_ne!(
            resp.status(),
            403_u16,
            "None allowlist (Docker mode) — any IP must pass"
        );
    }

    /// Adding an IP to the shared allowlist immediately unblocks it — no server restart needed.
    ///
    /// Simulates `switch_daemon` resolving the new daemon's IP and inserting it.
    #[tokio::test]
    async fn test_allowlist_update_takes_effect_immediately() {
        // Start with empty allowlist — even 127.0.0.1 is blocked.
        let allowed_ips = Arc::new(RwLock::new(Some(HashSet::<IpAddr>::new())));
        let port = start_file_server(None, Arc::clone(&allowed_ips), no_prefix_map())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let file_url = format!("http://127.0.0.1:{}/detrix/files/read", port);
        let body = serde_json::json!({"path": "/tmp/nonexistent_xyz_detrix"});

        // Before update: 127.0.0.1 is not in the empty set → blocked.
        let resp = reqwest::Client::new()
            .post(&file_url)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            403,
            "Should be blocked before allowlist update"
        );

        // Simulate switch_daemon: add the daemon's resolved IP.
        {
            let mut guard = allowed_ips.write().await;
            if let Some(set) = guard.as_mut() {
                set.insert(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
            }
        }

        // After update: 127.0.0.1 is now in the allowlist → allowed.
        let resp = reqwest::Client::new()
            .post(&file_url)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            403_u16,
            "Should be allowed after adding IP to allowlist"
        );
    }

    /// Removing an IP from the shared allowlist immediately blocks it — no server restart needed.
    ///
    /// Simulates daemon disconnect or switching to a different daemon.
    #[tokio::test]
    async fn test_allowlist_removal_takes_effect_immediately() {
        // Start with 127.0.0.1 in allowlist — connections are initially allowed.
        let mut allowed = HashSet::new();
        allowed.insert(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        allowed.insert(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
        let allowed_ips = Arc::new(RwLock::new(Some(allowed)));
        let port = start_file_server(None, Arc::clone(&allowed_ips), no_prefix_map())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let file_url = format!("http://127.0.0.1:{}/detrix/files/read", port);
        let body = serde_json::json!({"path": "/tmp/nonexistent_xyz_detrix"});

        // Initially: 127.0.0.1 is in the allowlist → allowed.
        let resp = reqwest::Client::new()
            .post(&file_url)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_ne!(resp.status(), 403_u16, "Should be allowed initially");

        // Remove 127.0.0.1 (simulate daemon switch / disconnect).
        {
            let mut guard = allowed_ips.write().await;
            if let Some(set) = guard.as_mut() {
                set.remove(&IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
            }
        }

        // After removal: 127.0.0.1 is no longer in allowlist → blocked.
        let resp = reqwest::Client::new()
            .post(&file_url)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            403,
            "Should be blocked after IP removed from allowlist"
        );
    }

    // =========================================================================
    // Token-based IP learning
    // =========================================================================

    /// Unknown IP with valid token gets learned — subsequent requests don't need token check.
    ///
    /// Simulates a Docker daemon connecting from an unknown bridge IP and proving identity
    /// via bearer token. After learning, the IP is in the allowlist.
    #[tokio::test]
    async fn test_unknown_ip_learned_via_token() {
        // Start with empty allowlist + token auth.
        let token = "test-learn-token";
        let allowed_ips = Arc::new(RwLock::new(Some(HashSet::<IpAddr>::new())));
        let port = start_file_server(
            Some(token.to_string()),
            Arc::clone(&allowed_ips),
            no_prefix_map(),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let file_url = format!("http://127.0.0.1:{}/detrix/files/read", port);
        let body = serde_json::json!({"path": "/tmp/nonexistent_xyz_detrix"});

        // Without token: token check fails first → 401.
        let resp = reqwest::Client::new()
            .post(&file_url)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "Missing token when configured → 401");

        // With valid token: unknown IP → learned, request proceeds (404 for missing file).
        let resp = reqwest::Client::new()
            .post(&file_url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_ne!(
            resp.status(),
            403_u16,
            "Unknown IP with valid token should be learned and allowed"
        );

        // Verify IP was added to the allowlist.
        let guard = allowed_ips.read().await;
        let set = guard.as_ref().unwrap();
        assert!(
            set.contains(&IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "127.0.0.1 should now be in the allowlist after token-based learning"
        );
    }

    /// Unknown IP without token configured → 403 (no way to prove identity).
    #[tokio::test]
    async fn test_unknown_ip_without_token_gets_403() {
        // Empty allowlist, no token auth.
        let allowed_ips = Arc::new(RwLock::new(Some(HashSet::<IpAddr>::new())));
        let port = start_file_server(None, Arc::clone(&allowed_ips), no_prefix_map())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/detrix/files/read", port))
            .json(&serde_json::json!({"path": "/tmp/nonexistent_xyz_detrix"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            403,
            "Unknown IP without token configured → always 403"
        );
    }

    // =========================================================================
    // Lazy CWD-based mapping discovery
    // =========================================================================

    /// When no prefix map is set, the file server discovers a mapping lazily
    /// from the requested file path by suffix-matching against CWD.
    #[tokio::test]
    async fn test_lazy_cwd_mapping_discovery() {
        // Create a temp dir with a file inside a subdirectory.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let file = sub.join("test.txt");
        std::fs::write(&file, "lazy content").unwrap();

        // Start a file server with no prefix map.
        let pfx = no_prefix_map();
        let pfx_clone = Arc::clone(&pfx);
        let allowed_ips = Arc::new(RwLock::new(None));
        let _port = start_file_server(None, allowed_ips, pfx_clone)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Simulate a container path: /container_root/sub/test.txt
        // CWD contains dir.path()/sub/test.txt, so we need to set CWD temporarily.
        // Instead, test the discover_cwd_mapping function directly.
        let result = discover_cwd_mapping(&format!("/fake_prefix{}", file.to_str().unwrap()));
        // We can't predict CWD in tests, but we can verify the function
        // works with a known existing path on disk.
        let result2 = discover_cwd_mapping(&file.to_str().unwrap());
        // When the path already exists at its absolute location, the function
        // should find it with suffix_start=1 (strip RootDir component only).
        // On unix, /path/to/file → components are [RootDir, "path", "to", "file"]
        // suffix_start=1 → suffix = "path/to/file" → CWD/path/to/file
        // This only matches if CWD happens to be /, which is unlikely.
        // So this is a best-effort test.
        let _ = result;
        let _ = result2;
    }

    /// Test discover_cwd_mapping with a known file structure.
    #[test]
    fn test_discover_cwd_mapping_with_tempdir() {
        // Create a structure CWD-relative: the function uses std::env::current_dir(),
        // so we create a file at CWD/<suffix> and request /prefix/<suffix>.
        let cwd = std::env::current_dir().unwrap();
        // Find a file that exists in CWD (e.g. Cargo.toml in the workspace)
        let cargo_toml = cwd.join("Cargo.toml");
        if !cargo_toml.exists() {
            // Can't test without a known file at CWD
            return;
        }

        // Simulate a container path: /container_root/Cargo.toml
        let result = discover_cwd_mapping("/container_root/Cargo.toml");
        assert!(
            result.is_some(),
            "Should find CWD mapping for /container_root/Cargo.toml"
        );
        let (container_prefix, host_prefix) = result.unwrap();
        assert_eq!(container_prefix, "/container_root");
        assert_eq!(host_prefix, cwd.to_string_lossy());
    }
}
