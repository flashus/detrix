//! HTTP control plane server.

use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::error::{Result, ResultExt};

use super::handlers::{
    handle_request, HandlerContext, SharedAuthToken, SleepCallback, StatusCallback, WakeCallback,
};

/// HTTP control plane server.
pub struct ControlServer {
    /// Actual port the server is listening on.
    actual_port: u16,

    /// Shutdown signal sender.
    shutdown_tx: Option<oneshot::Sender<()>>,

    /// Server thread handle.
    thread_handle: Option<thread::JoinHandle<()>>,

    /// Shared auth token (also held by HandlerContext).
    auth_token: SharedAuthToken,
}

impl ControlServer {
    /// Create and start a new control server.
    pub fn start(
        host: &str,
        port: u16,
        auth_token: Option<String>,
        status_callback: StatusCallback,
        wake_callback: WakeCallback,
        sleep_callback: SleepCallback,
    ) -> Result<Self> {
        let addr: SocketAddr = format!("{}:{}", host, port)
            .parse()
            .control_plane("invalid address")?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let auth_token = Arc::new(std::sync::RwLock::new(auth_token));
        let ctx = Arc::new(HandlerContext {
            auth_token: Arc::clone(&auth_token),
            status_callback,
            wake_callback,
            sleep_callback,
        });

        // Bind on main thread to get the actual port before spawning
        let std_listener = std::net::TcpListener::bind(addr).control_plane("failed to bind")?;

        let actual_addr = std_listener
            .local_addr()
            .control_plane("failed to get local addr")?;

        let actual_port = actual_addr.port();

        info!("Control plane starting on {}", actual_addr);

        std_listener
            .set_nonblocking(true)
            .control_plane("failed to set non-blocking")?;

        let thread_handle = thread::Builder::new()
            .name("detrix-control-plane".to_string())
            .spawn(move || run_server_thread(std_listener, shutdown_rx, ctx))
            .control_plane("failed to spawn thread")?;

        Ok(Self {
            actual_port,
            shutdown_tx: Some(shutdown_tx),
            thread_handle: Some(thread_handle),
            auth_token,
        })
    }

    /// Get the actual port the server is listening on.
    pub fn port(&self) -> u16 {
        self.actual_port
    }

    /// Update the auth token used for validating incoming requests.
    ///
    /// This should be called when the daemon restarts with a new token, so the
    /// control server accepts requests authenticated with the fresh token.
    pub fn update_token(&self, token: Option<String>) {
        let mut guard = self.auth_token.write().unwrap_or_else(|e| e.into_inner());
        *guard = token;
    }

    /// Stop the server gracefully.
    pub fn stop(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }

        debug!("Control plane stopped");
        Ok(())
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Run the server event loop in a dedicated thread.
fn run_server_thread(
    std_listener: std::net::TcpListener,
    shutdown_rx: oneshot::Receiver<()>,
    ctx: Arc<HandlerContext>,
) {
    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            error!("Failed to create tokio runtime: {}", e);
            return;
        }
    };

    rt.block_on(run_server_loop(std_listener, shutdown_rx, ctx));
}

/// Async server accept loop.
async fn run_server_loop(
    std_listener: std::net::TcpListener,
    shutdown_rx: oneshot::Receiver<()>,
    ctx: Arc<HandlerContext>,
) {
    let listener = match TcpListener::from_std(std_listener) {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to convert listener: {}", e);
            return;
        }
    };

    let mut shutdown_rx = shutdown_rx;

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                debug!("Control plane received shutdown signal");
                break;
            }
            result = listener.accept() => {
                handle_accept(result, &ctx);
            }
        }
    }

    debug!("Control plane server stopped");
}

/// Handle a single accepted connection.
fn handle_accept(
    result: std::io::Result<(tokio::net::TcpStream, SocketAddr)>,
    ctx: &Arc<HandlerContext>,
) {
    let (stream, remote_addr) = match result {
        Ok(conn) => conn,
        Err(e) => {
            error!("Accept error: {}", e);
            return;
        }
    };

    let ctx = Arc::clone(ctx);
    let remote_addr_str = remote_addr.to_string();

    tokio::spawn(async move {
        serve_connection(stream, remote_addr_str, ctx).await;
    });
}

/// Serve a single HTTP connection.
async fn serve_connection(
    stream: tokio::net::TcpStream,
    remote_addr: String,
    ctx: Arc<HandlerContext>,
) {
    let io = TokioIo::new(stream);

    let service = service_fn(move |req| {
        let ctx = Arc::clone(&ctx);
        let remote = remote_addr.clone();
        async move { Ok::<_, std::convert::Infallible>(handle_request(req, remote, ctx).await) }
    });

    if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
        debug!("Connection error: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::{
        ClientState, SleepResponse, SleepResponseStatus, StatusResponse, WakeResponse,
        WakeResponseStatus,
    };

    fn mock_status() -> StatusResponse {
        StatusResponse {
            state: ClientState::Sleeping,
            name: "test-client".to_string(),
            control_host: "127.0.0.1".to_string(),
            control_port: 0,
            debug_port: 0,
            debug_port_active: false,
            daemon_url: "http://127.0.0.1:8090".to_string(),
            connection_id: None,
        }
    }

    fn mock_wake(_: Option<String>) -> std::result::Result<WakeResponse, String> {
        Ok(WakeResponse {
            status: WakeResponseStatus::Awake,
            debug_port: 5678,
            connection_id: "conn-123".to_string(),
        })
    }

    fn mock_sleep() -> std::result::Result<SleepResponse, String> {
        Ok(SleepResponse {
            status: SleepResponseStatus::Sleeping,
        })
    }

    #[test]
    fn test_server_start_stop() {
        let server = ControlServer::start(
            "127.0.0.1",
            0,
            None,
            Arc::new(mock_status),
            Arc::new(mock_wake),
            Arc::new(mock_sleep),
        )
        .unwrap();

        assert!(server.port() > 0);
    }

    #[test]
    fn test_update_token() {
        let server = ControlServer::start(
            "127.0.0.1",
            0,
            Some("initial-token".to_string()),
            Arc::new(mock_status),
            Arc::new(mock_wake),
            Arc::new(mock_sleep),
        )
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(100));

        let client = reqwest::blocking::Client::new();
        let base = format!("http://127.0.0.1:{}", server.port());

        // Remote request with initial token should succeed
        let resp = client
            .get(format!("{}/detrix/status", base))
            .header("Authorization", "Bearer initial-token")
            .send()
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Update token
        server.update_token(Some("new-token".to_string()));

        // Old token should now fail from remote
        // Note: localhost always succeeds (auth bypass), so we can only test
        // that the new token works by checking the internal state
        let guard = server.auth_token.read().unwrap();
        assert_eq!(guard.as_deref(), Some("new-token"));
    }

    #[test]
    fn test_server_health_endpoint() {
        let server = ControlServer::start(
            "127.0.0.1",
            0,
            None,
            Arc::new(mock_status),
            Arc::new(mock_wake),
            Arc::new(mock_sleep),
        )
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(100));

        let client = reqwest::blocking::Client::new();
        let response = client
            .get(&format!("http://127.0.0.1:{}/detrix/health", server.port()))
            .send()
            .unwrap();

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().unwrap();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "detrix-client");
    }
}
