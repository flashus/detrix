//! HTTP request handlers for the control plane.

use std::sync::{Arc, RwLock};

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, Response, StatusCode};
use tracing::{debug, error};

use crate::auth::is_authorized;

/// Shared, thread-safe auth token reference.
///
/// Used by both `HandlerContext` (for validating incoming requests) and
/// `ControlServer` (for updating the token when the daemon restarts).
pub type SharedAuthToken = Arc<RwLock<Option<String>>>;
use crate::generated::{
    DiscoverResponse, ErrorResponse, HealthResponse, HealthResponseService, HealthResponseStatus,
    InfoResponse, ReadFileRequest, SleepResponse, StatusResponse, WakeRequest, WakeResponse,
};

// ============================================================================
// Extension impls for generated types
// ============================================================================

impl Default for HealthResponse {
    fn default() -> Self {
        Self {
            status: HealthResponseStatus::Ok,
            service: HealthResponseService::DetrixClient,
        }
    }
}

impl ErrorResponse {
    /// Create a new error response.
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
        }
    }
}

/// Callback type for getting status.
pub type StatusCallback = Arc<dyn Fn() -> StatusResponse + Send + Sync>;

/// Callback type for wake operation.
pub type WakeCallback = Arc<dyn Fn(Option<String>) -> Result<WakeResponse, String> + Send + Sync>;

/// Callback type for sleep operation.
pub type SleepCallback = Arc<dyn Fn() -> Result<SleepResponse, String> + Send + Sync>;

/// Callback type for discover operation.
pub type DiscoverCallback = Arc<dyn Fn() -> DiscoverResponse + Send + Sync>;

/// Handler context with callbacks and auth token.
pub struct HandlerContext {
    pub auth_token: SharedAuthToken,
    pub workspace_root: String,
    pub status_callback: StatusCallback,
    pub wake_callback: WakeCallback,
    pub sleep_callback: SleepCallback,
    pub discover_callback: DiscoverCallback,
}

/// Handle an incoming HTTP request.
pub async fn handle_request(
    req: Request<hyper::body::Incoming>,
    remote_addr: String,
    ctx: Arc<HandlerContext>,
) -> Response<Full<Bytes>> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    debug!("Request: {} {} from {}", method, path, remote_addr);

    // Get auth header before consuming the request
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Check auth for protected endpoints
    let needs_auth = path != "/detrix/health" && path != "/detrix/discover";
    if needs_auth {
        let token_guard = ctx.auth_token.read().unwrap_or_else(|e| e.into_inner());
        if !is_authorized(&remote_addr, auth_header.as_deref(), token_guard.as_deref()) {
            return unauthorized();
        }
    }

    // Route the request
    match (method, path.as_str()) {
        (Method::GET, "/detrix/health") => handle_health(),
        (Method::GET, "/detrix/discover") => handle_discover(&ctx),
        (Method::GET, "/detrix/status") => handle_status(&ctx),
        (Method::GET, "/detrix/info") => handle_info(&ctx),
        (Method::POST, "/detrix/wake") => handle_wake(req, &ctx).await,
        (Method::POST, "/detrix/sleep") => handle_sleep(&ctx).await,
        (Method::POST, "/detrix/files/read") => handle_files_read(req, &ctx).await,
        _ => not_found(),
    }
}

/// Handle GET /detrix/health (no auth required).
fn handle_health() -> Response<Full<Bytes>> {
    json_response(StatusCode::OK, &HealthResponse::default())
}

/// Handle GET /detrix/discover (no auth required).
fn handle_discover(ctx: &HandlerContext) -> Response<Full<Bytes>> {
    let response = (ctx.discover_callback)();
    json_response(StatusCode::OK, &response)
}

/// Handle GET /detrix/status.
fn handle_status(ctx: &HandlerContext) -> Response<Full<Bytes>> {
    let status = (ctx.status_callback)();
    json_response(StatusCode::OK, &status)
}

/// Handle GET /detrix/info.
fn handle_info(ctx: &HandlerContext) -> Response<Full<Bytes>> {
    let status = (ctx.status_callback)();

    let info = InfoResponse {
        name: status.name,
        pid: std::process::id() as i64,
        rust_version: Some(rust_version()),
        python_version: None,
        python_executable: None,
        go_version: None,
    };

    json_response(StatusCode::OK, &info)
}

/// Handle POST /detrix/wake.
async fn handle_wake(
    req: Request<hyper::body::Incoming>,
    ctx: &HandlerContext,
) -> Response<Full<Bytes>> {
    // Parse optional daemon_url from request body
    let daemon_url = parse_wake_request(req).await;

    // Run the blocking callback in a completely separate OS thread (not tokio's thread pool)
    // because reqwest::blocking::Client checks Handle::try_current() and fails if it
    // thinks it's inside a tokio context (spawn_blocking still exposes the Handle).
    // This is acceptable for infrequent wake operations.
    let wake_callback = ctx.wake_callback.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        let result = (wake_callback)(daemon_url);
        let _ = tx.send(result);
    });

    let result = rx
        .await
        .unwrap_or_else(|_| Err("wake thread panicked".to_string()));

    match result {
        Ok(response) => json_response(StatusCode::OK, &response),
        Err(e) => {
            let status = if e.contains("not reachable") || e.contains("daemon") {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            json_response(status, &ErrorResponse::new(e))
        }
    }
}

/// Handle POST /detrix/sleep.
async fn handle_sleep(ctx: &HandlerContext) -> Response<Full<Bytes>> {
    // Run the blocking callback in a completely separate OS thread (not tokio's thread pool)
    // for the same reason as handle_wake: reqwest::blocking can't run inside tokio.
    // This is acceptable for infrequent sleep operations.
    let sleep_callback = ctx.sleep_callback.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        let result = (sleep_callback)();
        let _ = tx.send(result);
    });

    let result = rx
        .await
        .unwrap_or_else(|_| Err("sleep thread panicked".to_string()));

    match result {
        Ok(response) => json_response(StatusCode::OK, &response),
        Err(e) => json_response(StatusCode::INTERNAL_SERVER_ERROR, &ErrorResponse::new(e)),
    }
}

/// Handle POST /detrix/files/read — serve source files to the daemon for anchor capture.
///
/// Request body: `{"path": "/absolute/or/relative/path"}`
/// Response: plain text file content (200), 404 if not found, 403 if outside workspace.
async fn handle_files_read(
    req: Request<hyper::body::Incoming>,
    ctx: &HandlerContext,
) -> Response<Full<Bytes>> {
    const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

    // Parse request body
    let body = match req.collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return plain_response(StatusCode::BAD_REQUEST, "Failed to read request body"),
    };

    let file_req: ReadFileRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => return plain_response(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };
    if file_req.path.is_empty() {
        return plain_response(StatusCode::BAD_REQUEST, "Missing or empty 'path' field");
    }
    let path_str = file_req.path;

    // Resolve path (relative → absolute using workspace_root from request or server config)
    let workspace_root = file_req
        .workspace_root
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ctx.workspace_root.clone());
    let workspace = if workspace_root.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
    } else {
        std::path::PathBuf::from(&workspace_root)
    };
    let workspace = match workspace.canonicalize() {
        Ok(p) => p,
        Err(_) => workspace,
    };

    // Security: validate that the request's workspace_root is within (or equal to)
    // the server's own configured workspace root. Prevents setting workspace_root
    // to "/" to bypass path containment checks.
    let trust_boundary = if ctx.workspace_root.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"))
    } else {
        std::path::PathBuf::from(&ctx.workspace_root)
    };
    let trust_boundary = match trust_boundary.canonicalize() {
        Ok(p) => p,
        Err(_) => trust_boundary,
    };
    if !workspace.starts_with(&trust_boundary) {
        return plain_response(
            StatusCode::FORBIDDEN,
            "Workspace root escapes trust boundary",
        );
    }

    let requested = std::path::Path::new(&path_str);
    let resolved = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    };
    let resolved = match resolved.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // File doesn't exist or can't be resolved
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())));
        }
    };

    // Security: path must be within workspace
    if !resolved.starts_with(&workspace) {
        return plain_response(StatusCode::FORBIDDEN, "Path not within workspace");
    }

    // Check file metadata
    let metadata = match std::fs::metadata(&resolved) {
        Ok(m) if m.is_file() => m,
        _ => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())));
        }
    };

    if metadata.len() > MAX_FILE_SIZE {
        return plain_response(StatusCode::PAYLOAD_TOO_LARGE, "File exceeds maximum size");
    }

    // Read and return file content
    match std::fs::read(&resolved) {
        Ok(content) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(Full::new(Bytes::from(content)))
            .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()))),
        Err(_) => plain_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read file"),
    }
}

/// Return a plain text HTTP response.
fn plain_response(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::from_static(body.as_bytes()))))
}

/// Parse wake request body.
async fn parse_wake_request(req: Request<hyper::body::Incoming>) -> Option<String> {
    let body = req.collect().await.ok()?.to_bytes();
    if body.is_empty() {
        return None;
    }

    let wake_req: WakeRequest = serde_json::from_slice(&body).ok()?;
    wake_req.daemon_url
}

/// Return a 401 Unauthorized response.
fn unauthorized() -> Response<Full<Bytes>> {
    json_response(
        StatusCode::UNAUTHORIZED,
        &ErrorResponse::new("unauthorized"),
    )
}

/// Return a 404 Not Found response.
fn not_found() -> Response<Full<Bytes>> {
    json_response(StatusCode::NOT_FOUND, &ErrorResponse::new("not found"))
}

/// Create a JSON response.
fn json_response<T: serde::Serialize>(status: StatusCode, body: &T) -> Response<Full<Bytes>> {
    let json = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());

    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(json)))
        .unwrap_or_else(|e| {
            error!("Failed to build HTTP response: {}", e);
            let body = r#"{"error":"internal server error"}"#;
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(body)))
                .unwrap_or_else(|_| Response::new(Full::new(Bytes::from(body))))
        })
}

/// Get the Rust version.
fn rust_version() -> String {
    // Try to get version from rustc if available
    if let Ok(output) = std::process::Command::new("rustc")
        .arg("--version")
        .output()
    {
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
    }

    // Fallback to compile-time version
    format!("rustc {} (compile-time)", env!("CARGO_PKG_RUST_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_rust_version() {
        let version = rust_version();
        assert!(!version.is_empty());
    }

    #[test]
    fn test_json_response() {
        let response = json_response(StatusCode::OK, &json!({"test": true}));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );
    }
}
