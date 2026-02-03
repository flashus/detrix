//! HTTP request handlers for the control plane.

use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, Response, StatusCode};
use tracing::{debug, error};

use crate::auth::is_authorized;
use crate::generated::{
    ErrorResponse, HealthResponse, HealthResponseService, HealthResponseStatus, InfoResponse,
    SleepResponse, StatusResponse, WakeRequest, WakeResponse,
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

/// Handler context with callbacks and auth token.
pub struct HandlerContext {
    pub auth_token: Option<String>,
    pub status_callback: StatusCallback,
    pub wake_callback: WakeCallback,
    pub sleep_callback: SleepCallback,
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
    let needs_auth = path != "/detrix/health";
    if needs_auth
        && !is_authorized(
            &remote_addr,
            auth_header.as_deref(),
            ctx.auth_token.as_deref(),
        )
    {
        return unauthorized();
    }

    // Route the request
    match (method, path.as_str()) {
        (Method::GET, "/detrix/health") => handle_health(),
        (Method::GET, "/detrix/status") => handle_status(&ctx),
        (Method::GET, "/detrix/info") => handle_info(&ctx),
        (Method::POST, "/detrix/wake") => handle_wake(req, &ctx).await,
        (Method::POST, "/detrix/sleep") => handle_sleep(&ctx).await,
        _ => not_found(),
    }
}

/// Handle GET /detrix/health (no auth required).
fn handle_health() -> Response<Full<Bytes>> {
    json_response(StatusCode::OK, &HealthResponse::default())
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

    // Run the blocking callback in a completely separate thread (not tokio's thread pool)
    // because reqwest::blocking::Client checks Handle::try_current() and fails if it
    // thinks it's inside a tokio context (spawn_blocking still exposes the Handle)
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
    // Run the blocking callback in a completely separate thread (not tokio's thread pool)
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
