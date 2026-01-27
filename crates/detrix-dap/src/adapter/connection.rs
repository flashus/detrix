//! Connection Management
//!
//! TCP connection, retry logic, and TCP keepalive configuration.

use super::config::AdapterConfig;
use super::initialization::initialize_dap;
use crate::{Capabilities, DapBroker, Error, Result};
#[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
use detrix_config::DEFAULT_TCP_KEEPALIVE_RETRIES;
use detrix_config::{
    AdapterConnectionConfig, DEFAULT_TCP_KEEPALIVE_INTERVAL_SECS, DEFAULT_TCP_KEEPALIVE_TIME_SECS,
    LOCALHOST_IPV4,
};
use socket2::{SockRef, TcpKeepalive};
use std::net::Ipv4Addr;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, info, trace, warn};

/// Launch adapter as subprocess (Launch mode)
pub async fn launch_subprocess(
    config: &AdapterConfig,
    connection_config: &AdapterConnectionConfig,
    process: &Arc<Mutex<Option<Child>>>,
    broker: &Arc<Mutex<Option<Arc<DapBroker>>>>,
    capabilities: &Arc<Mutex<Option<Capabilities>>>,
) -> Result<()> {
    info!(
        "Launching adapter: {} {}",
        config.command,
        config.args.join(" ")
    );

    let args = config.substitute_port();
    let mut cmd = Command::new(&config.command);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(cwd) = &config.cwd {
        cmd.current_dir(cwd);
    }

    for (key, value) in &config.env {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn()?;

    // Get stdin/stdout for DAP communication
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Communication("Failed to get stdin".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Communication("Failed to get stdout".to_string()))?;

    // Store process
    {
        let mut proc = process.lock().await;
        *proc = Some(child);
    }

    // Create DAP broker with connection config
    let dap_broker = Arc::new(DapBroker::new_with_config(
        stdout,
        stdin,
        connection_config.clone(),
    ));

    // Initialize DAP connection
    initialize_dap(&dap_broker, config, capabilities).await?;

    // Store broker
    {
        let mut broker_lock = broker.lock().await;
        *broker_lock = Some(dap_broker);
    }

    Ok(())
}

/// Connect directly to debugpy server via TCP (Attach mode)
///
/// When the user runs `python -m debugpy --listen <port> script.py`, the debugpy server
/// acts as BOTH the debug server AND the DAP adapter. We can connect directly to it
/// without spawning a separate adapter subprocess.
///
/// Flow:
/// 1. User runs: `python -m debugpy --listen 5678 script.py` (debugpy server with built-in adapter)
/// 2. We connect directly to port 5678 via TCP
/// 3. We send DAP initialize/attach/configurationDone requests
pub async fn connect_tcp(
    debugpy_host: &str,
    debugpy_port: u16,
    config: &AdapterConfig,
    connection_config: &AdapterConnectionConfig,
    broker: &Arc<Mutex<Option<Arc<DapBroker>>>>,
    capabilities: &Arc<Mutex<Option<Capabilities>>>,
) -> Result<()> {
    info!(
        "Connecting directly to DAP server at {}:{}...",
        debugpy_host, debugpy_port
    );

    // Connect to the debugpy server via TCP (with retry)
    // Normalize localhost to IPv4 to avoid IPv6 resolution issues on Windows
    let host: Ipv4Addr = if debugpy_host == "localhost" {
        // "localhost" may resolve to IPv6 (::1) on Windows, causing connection issues
        // Force IPv4 for consistent cross-platform behavior
        LOCALHOST_IPV4
    } else {
        match debugpy_host.parse() {
            Ok(addr) => addr,
            Err(e) => {
                warn!(
                    "Invalid host '{}': {}. Falling back to 127.0.0.1",
                    debugpy_host, e
                );
                LOCALHOST_IPV4
            }
        }
    };
    let stream = connect_with_retry(host, debugpy_port, connection_config).await?;

    // Configure TCP keep-alive to prevent Windows from closing idle connections
    configure_tcp_keepalive(&stream);

    info!("Connected to DAP server on port {}", debugpy_port);

    // Split into read/write halves
    let (reader, writer) = tokio::io::split(stream);

    // Create DAP broker with connection config
    let dap_broker = Arc::new(DapBroker::new_with_config(
        reader,
        writer,
        connection_config.clone(),
    ));

    // Initialize DAP connection (direct mode - no connect info needed)
    initialize_dap(&dap_broker, config, capabilities).await?;

    // Store broker
    {
        let mut broker_lock = broker.lock().await;
        *broker_lock = Some(dap_broker);
    }

    Ok(())
}

/// Connect to a TCP address with retry logic and exponential backoff
///
/// Uses exponential backoff with jitter to avoid thundering herd:
/// - Starts with configured retry_interval_ms
/// - Doubles interval on each retry (up to reconnect.max_delay_ms)
/// - Adds random jitter (0-100ms) to spread out retries
///
/// Fast-fail behavior:
/// - If we get "connection refused" N times in a row, fail immediately
///   (nothing is listening on that port)
async fn connect_with_retry(
    host: Ipv4Addr,
    port: u16,
    config: &AdapterConnectionConfig,
) -> Result<TcpStream> {
    use rand::Rng;

    let address = format!("{}:{}", host, port);
    let start = std::time::Instant::now();
    let timeout = Duration::from_millis(config.connection_timeout_ms);

    // Exponential backoff parameters from config
    let mut retry_interval_ms = config.retry_interval_ms;
    let max_retry_interval_ms = config.reconnect.max_delay_ms;
    let max_connection_refused = config.max_connection_refused_attempts;
    let mut attempt = 0u32;
    let mut connection_refused_count = 0u32;

    loop {
        match TcpStream::connect(&address).await {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                attempt += 1;

                // Check for "connection refused" - nothing listening on port
                let is_connection_refused = e.kind() == std::io::ErrorKind::ConnectionRefused;
                if is_connection_refused {
                    connection_refused_count += 1;
                    if connection_refused_count >= max_connection_refused {
                        return Err(Error::Communication(format!(
                            "No debugger listening on {} (connection refused {} times). \
                            Ensure the debugger process is started with --listen {}",
                            address, connection_refused_count, port
                        )));
                    }
                } else {
                    // Reset counter for non-refused errors (e.g., timeout)
                    connection_refused_count = 0;
                }

                if start.elapsed() > timeout {
                    return Err(Error::Communication(format!(
                        "Timeout connecting to debugger at {} after {} attempts: {}",
                        address, attempt, e
                    )));
                }

                // Exponential backoff with jitter to prevent thundering herd
                let jitter_ms = rand::rng().random_range(0..100);
                let wait_ms = retry_interval_ms.saturating_add(jitter_ms);

                trace!(
                    "Connection attempt {} failed, retrying in {}ms (backoff: {}ms + jitter: {}ms)",
                    attempt,
                    wait_ms,
                    retry_interval_ms,
                    jitter_ms
                );

                tokio::time::sleep(Duration::from_millis(wait_ms)).await;

                // Double the interval for next retry, capped at max from config
                retry_interval_ms = (retry_interval_ms * 2).min(max_retry_interval_ms);
            }
        }
    }
}

/// Configure TCP keep-alive on a socket for cross-platform reliability
///
/// Windows aggressively closes idle TCP connections (10-30 seconds).
/// This configures TCP keep-alive probes to maintain the connection during
/// periods of inactivity when waiting for DAP messages.
fn configure_tcp_keepalive(stream: &TcpStream) {
    // SockRef can borrow directly from tokio TcpStream (implements AsFd/AsRawSocket)
    let socket = SockRef::from(stream);

    // Disable Nagle's algorithm for low-latency DAP message delivery
    if let Err(e) = socket.set_nodelay(true) {
        warn!("Failed to set TCP_NODELAY: {}", e);
    }

    // Configure TCP keep-alive to prevent Windows from closing idle connections
    let keepalive = TcpKeepalive::new()
        .with_time(Duration::from_secs(DEFAULT_TCP_KEEPALIVE_TIME_SECS))
        .with_interval(Duration::from_secs(DEFAULT_TCP_KEEPALIVE_INTERVAL_SECS));

    // Set retries on platforms that support it (Linux, Android, FreeBSD - NOT Windows/macOS)
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    let keepalive = keepalive.with_retries(DEFAULT_TCP_KEEPALIVE_RETRIES);

    if let Err(e) = socket.set_tcp_keepalive(&keepalive) {
        warn!("Failed to set TCP keep-alive: {}", e);
    } else {
        debug!("TCP keep-alive configured successfully");
    }
}
