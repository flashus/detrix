//! Socket utilities for lldb-serve

#[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
use detrix_config::DEFAULT_TCP_KEEPALIVE_RETRIES;
use detrix_config::{
    DEFAULT_DAP_SOCKET_READ_TIMEOUT_SECS, DEFAULT_TCP_KEEPALIVE_INTERVAL_SECS,
    DEFAULT_TCP_KEEPALIVE_TIME_SECS,
};
use detrix_logging::{debug, warn};
use socket2::{SockRef, TcpKeepalive};
use std::net::TcpStream;
use std::time::Duration;

/// Normalize listen address for cross-platform compatibility
///
/// On Windows, binding to "localhost" may resolve to IPv6 ([::1]) while DAP clients
/// typically connect via IPv4 (127.0.0.1). This function normalizes "localhost" to
/// "127.0.0.1" to ensure consistent behavior across platforms.
pub fn normalize_listen_addr(addr: &str) -> String {
    if let Some(port) = addr.strip_prefix("localhost:") {
        format!("127.0.0.1:{}", port)
    } else if addr == "localhost" {
        "127.0.0.1".to_string()
    } else {
        addr.to_string()
    }
}

/// Configure TCP keepalive and read timeout on an accepted socket
///
/// This is critical for Windows compatibility:
/// - TCP keepalive prevents Windows from closing idle connections
/// - Read timeout prevents blocking forever when client stops sending data
pub fn configure_client_socket(stream: &TcpStream) {
    // Set read timeout to prevent blocking forever
    // This is especially important on Windows where sockets without timeout
    // can block indefinitely even after the client has disconnected
    if let Err(e) = stream.set_read_timeout(Some(Duration::from_secs(
        DEFAULT_DAP_SOCKET_READ_TIMEOUT_SECS,
    ))) {
        warn!("Failed to set socket read timeout: {}", e);
    }

    // Disable Nagle's algorithm for lower latency
    if let Err(e) = stream.set_nodelay(true) {
        warn!("Failed to set TCP_NODELAY: {}", e);
    }

    // Configure TCP keepalive to detect dead connections
    let socket = SockRef::from(stream);
    let keepalive = TcpKeepalive::new()
        .with_time(Duration::from_secs(DEFAULT_TCP_KEEPALIVE_TIME_SECS))
        .with_interval(Duration::from_secs(DEFAULT_TCP_KEEPALIVE_INTERVAL_SECS));

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    let keepalive = keepalive.with_retries(DEFAULT_TCP_KEEPALIVE_RETRIES);

    if let Err(e) = socket.set_tcp_keepalive(&keepalive) {
        warn!("Failed to set TCP keep-alive: {}", e);
    }

    debug!(
        read_timeout_secs = DEFAULT_DAP_SOCKET_READ_TIMEOUT_SECS,
        keepalive_time_secs = DEFAULT_TCP_KEEPALIVE_TIME_SECS,
        "Socket configured with read timeout and TCP keepalive"
    );
}
