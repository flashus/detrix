//! GELF TCP Transport
//!
//! Implements GELF over TCP with null-byte message delimiters.
//! See: <https://go2docs.graylog.org/current/getting_in_log_data/gelf.html#GELFviaTCP>
//!
//! # Circuit Breaker
//!
//! For circuit breaker functionality, wrap the output with `CircuitBreakerOutput`:
//!
//! ```rust,ignore
//! use detrix_output::{GelfTcpBuilder, CircuitBreakerOutput, CircuitBreakerConfig};
//!
//! let tcp = GelfTcpBuilder::new("graylog", 12201).build();
//! let protected = CircuitBreakerOutput::new(tcp, CircuitBreakerConfig::new(5, 30_000));
//! ```

use super::GelfMessage;
use crate::error::ToOutputResult;
use async_trait::async_trait;
use detrix_config::constants::{
    DEFAULT_API_HOST, DEFAULT_GELF_PORT, DEFAULT_GELF_TCP_CONNECT_TIMEOUT_MS,
    DEFAULT_GELF_TCP_WRITE_TIMEOUT_MS,
};
use detrix_core::MetricEvent;
use detrix_core::Result;
use detrix_ports::{EventOutput, OutputStats};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// Configuration for GELF TCP output
#[derive(Debug, Clone)]
pub struct GelfTcpConfig {
    /// Graylog server host
    pub host: String,
    /// Graylog server port (default: 12201)
    pub port: u16,
    /// Hostname to include in GELF messages (default: "detrix")
    pub source_host: Option<String>,
    /// Connection timeout in milliseconds
    pub connect_timeout_ms: u64,
    /// Write timeout in milliseconds
    pub write_timeout_ms: u64,
    /// Whether to reconnect on failure
    pub auto_reconnect: bool,
}

impl Default for GelfTcpConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_API_HOST.to_string(),
            port: DEFAULT_GELF_PORT,
            source_host: None,
            connect_timeout_ms: DEFAULT_GELF_TCP_CONNECT_TIMEOUT_MS,
            write_timeout_ms: DEFAULT_GELF_TCP_WRITE_TIMEOUT_MS,
            auto_reconnect: true,
        }
    }
}

impl GelfTcpConfig {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            ..Default::default()
        }
    }

    pub fn with_source_host(mut self, host: impl Into<String>) -> Self {
        self.source_host = Some(host.into());
        self
    }
}

/// GELF TCP output implementation
///
/// Sends events to Graylog over TCP with null-byte delimiters.
/// Thread-safe and handles reconnection automatically.
///
/// # Circuit Breaker
///
/// For circuit breaker protection, wrap this output with `CircuitBreakerOutput`:
/// ```rust,ignore
/// let tcp = GelfTcpBuilder::new("graylog", 12201).build();
/// let protected = CircuitBreakerOutput::new(tcp, CircuitBreakerConfig::new(5, 30_000));
/// ```
pub struct GelfTcpOutput {
    config: GelfTcpConfig,
    stream: Mutex<Option<TcpStream>>,
    connected: AtomicBool,
    events_sent: AtomicU64,
    events_failed: AtomicU64,
    bytes_sent: AtomicU64,
    reconnections: AtomicU64,
}

impl GelfTcpOutput {
    /// Create a new GELF TCP output (does not connect immediately)
    pub fn new(config: GelfTcpConfig) -> Self {
        Self {
            config,
            stream: Mutex::new(None),
            connected: AtomicBool::new(false),
            events_sent: AtomicU64::new(0),
            events_failed: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            reconnections: AtomicU64::new(0),
        }
    }

    /// Create with default config for given host/port
    pub fn with_address(host: impl Into<String>, port: u16) -> Self {
        Self::new(GelfTcpConfig::new(host, port))
    }

    /// Get source host configuration
    pub fn source_host(&self) -> Option<&str> {
        self.config.source_host.as_deref()
    }

    /// Send a pre-built GELF message (used by routing layer)
    pub async fn send_message(&self, msg: &GelfMessage) -> Result<()> {
        let bytes = msg
            .to_json_null_terminated()
            .output_context("Serialization failed")?;

        self.send_with_retry(&bytes).await
    }

    /// Connect to the Graylog server
    pub async fn connect(&self) -> Result<()> {
        let addr = format!("{}:{}", self.config.host, self.config.port);

        let connect_future = TcpStream::connect(&addr);
        let timeout = std::time::Duration::from_millis(self.config.connect_timeout_ms);

        let stream = tokio::time::timeout(timeout, connect_future)
            .await
            .map_err(|_| detrix_core::Error::Output(format!("Connection timeout to {}", addr)))?
            .output_context(&format!("Failed to connect to {}", addr))?;

        // Disable Nagle's algorithm for lower latency
        stream.set_nodelay(true).ok();

        let mut guard = self.stream.lock().await;
        *guard = Some(stream);
        self.connected.store(true, Ordering::SeqCst);

        tracing::info!("Connected to Graylog at {}", addr);
        Ok(())
    }

    /// Disconnect from the server
    pub async fn disconnect(&self) {
        let mut guard = self.stream.lock().await;
        if let Some(stream) = guard.take() {
            drop(stream);
        }
        self.connected.store(false, Ordering::SeqCst);
    }

    /// Attempt to reconnect if auto_reconnect is enabled
    async fn try_reconnect(&self) -> Result<()> {
        if !self.config.auto_reconnect {
            return Err(detrix_core::Error::Output(
                "Auto-reconnect disabled".to_string(),
            ));
        }

        self.disconnect().await;
        self.reconnections.fetch_add(1, Ordering::Relaxed);
        self.connect().await
    }

    /// Send raw bytes to the stream
    async fn send_bytes(&self, bytes: &[u8]) -> Result<()> {
        let mut guard = self.stream.lock().await;

        let stream = guard
            .as_mut()
            .ok_or_else(|| detrix_core::Error::Output("Not connected".to_string()))?;

        let write_future = stream.write_all(bytes);
        let timeout = std::time::Duration::from_millis(self.config.write_timeout_ms);

        tokio::time::timeout(timeout, write_future)
            .await
            .map_err(|_| detrix_core::Error::Output("Write timeout".to_string()))?
            .output_context("Write failed")?;

        self.bytes_sent
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Send bytes with automatic retry on failure
    ///
    /// This is the core send-with-retry pattern:
    /// 1. Attempt to send bytes
    /// 2. On failure, if auto_reconnect is enabled, reconnect and retry once
    /// 3. Update event counters based on final result
    async fn send_with_retry(&self, bytes: &[u8]) -> Result<()> {
        let result = self.send_bytes(bytes).await;

        if result.is_err() && self.config.auto_reconnect {
            // Try reconnect and resend once
            if self.try_reconnect().await.is_ok() && self.send_bytes(bytes).await.is_ok() {
                self.events_sent.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            self.events_failed.fetch_add(1, Ordering::Relaxed);
            return result;
        }

        if result.is_ok() {
            self.events_sent.fetch_add(1, Ordering::Relaxed);
        } else {
            self.events_failed.fetch_add(1, Ordering::Relaxed);
        }

        result
    }
}

#[async_trait]
impl EventOutput for GelfTcpOutput {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        let source = self.config.source_host.as_deref();
        let gelf_msg = GelfMessage::from_event(event, source);

        let bytes = gelf_msg
            .to_json_null_terminated()
            .output_context("Serialization failed")?;

        self.send_with_retry(&bytes).await
    }

    async fn flush(&self) -> Result<()> {
        let mut guard = self.stream.lock().await;
        if let Some(stream) = guard.as_mut() {
            stream.flush().await.output_context("Flush failed")?;
        }
        Ok(())
    }

    fn stats(&self) -> OutputStats {
        OutputStats {
            events_sent: self.events_sent.load(Ordering::Relaxed),
            events_failed: self.events_failed.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            reconnections: self.reconnections.load(Ordering::Relaxed),
            connected: self.connected.load(Ordering::SeqCst),
        }
    }

    fn name(&self) -> &str {
        "gelf-tcp"
    }
}

/// Builder for GelfTcpOutput with convenience methods
pub struct GelfTcpBuilder {
    config: GelfTcpConfig,
}

impl GelfTcpBuilder {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            config: GelfTcpConfig::new(host, port),
        }
    }

    pub fn source_host(mut self, host: impl Into<String>) -> Self {
        self.config.source_host = Some(host.into());
        self
    }

    pub fn connect_timeout_ms(mut self, ms: u64) -> Self {
        self.config.connect_timeout_ms = ms;
        self
    }

    pub fn write_timeout_ms(mut self, ms: u64) -> Self {
        self.config.write_timeout_ms = ms;
        self
    }

    pub fn auto_reconnect(mut self, enabled: bool) -> Self {
        self.config.auto_reconnect = enabled;
        self
    }

    pub fn build(self) -> GelfTcpOutput {
        GelfTcpOutput::new(self.config)
    }

    /// Build and connect immediately
    pub async fn connect(self) -> Result<Arc<GelfTcpOutput>> {
        let output = Arc::new(self.build());
        output.connect().await?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_testing::fixtures::sample_test_event as sample_event;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[test]
    fn test_gelf_tcp_config_builder() {
        let config =
            GelfTcpConfig::new("graylog.example.com", 12202).with_source_host("my-service");

        assert_eq!(config.host, "graylog.example.com");
        assert_eq!(config.port, 12202);
        assert_eq!(config.source_host, Some("my-service".to_string()));
    }

    #[test]
    fn test_gelf_tcp_output_new() {
        let output = GelfTcpOutput::with_address("localhost", 12201);
        assert!(!output.connected.load(Ordering::SeqCst));
        assert_eq!(output.name(), "gelf-tcp");
    }

    #[test]
    fn test_gelf_tcp_stats_initial() {
        let output = GelfTcpOutput::with_address("localhost", 12201);
        let stats = output.stats();

        assert_eq!(stats.events_sent, 0);
        assert_eq!(stats.events_failed, 0);
        assert_eq!(stats.bytes_sent, 0);
        assert_eq!(stats.reconnections, 0);
        assert!(!stats.connected);
    }

    #[tokio::test]
    async fn test_gelf_tcp_connect_to_mock_server() {
        // Start a mock TCP server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn server task
        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        // Connect client
        let output = GelfTcpOutput::with_address("127.0.0.1", addr.port());
        output.connect().await.unwrap();

        assert!(output.connected.load(Ordering::SeqCst));
        assert!(output.is_healthy());

        // Send an event
        let event = sample_event();
        output.send(&event).await.unwrap();
        output.flush().await.unwrap();

        // Check what server received
        let received = server_handle.await.unwrap();

        // Should be null-terminated JSON
        assert!(!received.is_empty());
        assert_eq!(received.last(), Some(&0u8)); // Null terminator

        // Parse JSON (without null byte)
        let json_bytes = &received[..received.len() - 1];
        let parsed: serde_json::Value = serde_json::from_slice(json_bytes).unwrap();

        assert_eq!(parsed["version"], "1.1");
        assert_eq!(parsed["_metric_name"], "test_metric");
        assert_eq!(parsed["_connection_id"], "test-conn");

        // Check stats
        let stats = output.stats();
        assert_eq!(stats.events_sent, 1);
        assert_eq!(stats.events_failed, 0);
        assert!(stats.bytes_sent > 0);
    }

    #[tokio::test]
    async fn test_gelf_tcp_send_batch() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut all_data = Vec::new();
            let mut buf = vec![0u8; 4096];

            // Read in a loop until connection closes or we have enough data
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break, // Connection closed
                    Ok(n) => {
                        all_data.extend_from_slice(&buf[..n]);
                        // If we've seen 3 null bytes, we're done
                        let null_count = all_data.iter().filter(|&&b| b == 0).count();
                        if null_count >= 3 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            // Count null bytes (message delimiters)
            all_data.iter().filter(|&&b| b == 0).count()
        });

        let output = GelfTcpOutput::with_address("127.0.0.1", addr.port());
        output.connect().await.unwrap();

        // Send batch of 3 events
        let events: Vec<MetricEvent> = (0..3)
            .map(|i| {
                let mut e = sample_event();
                e.metric_name = format!("metric_{}", i);
                e
            })
            .collect();

        output.send_batch(&events).await.unwrap();
        output.flush().await.unwrap();

        // Disconnect to signal EOF to server
        output.disconnect().await;

        let null_count = server_handle.await.unwrap();
        assert_eq!(null_count, 3); // 3 messages, 3 null terminators

        let stats = output.stats();
        assert_eq!(stats.events_sent, 3);
    }

    /// Find a port that is guaranteed to be unused (not listening)
    async fn find_unused_port() -> u16 {
        // Try up to 10 times to find a truly unused port
        for _ in 0..10 {
            // Get an ephemeral port from the OS
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener); // Release the port

            // Small delay to ensure OS has fully released the port
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;

            // Verify port is still not in use by trying to bind again
            if let Ok(check) = TcpListener::bind(format!("127.0.0.1:{}", port)).await {
                drop(check); // Release - port is confirmed unused
                return port;
            }
            // Port got taken, try again with a new one
        }
        panic!("Could not find an unused port after 10 attempts");
    }

    #[tokio::test]
    async fn test_gelf_tcp_connection_failure() {
        // Find a port that's guaranteed to be unused
        let unused_port = find_unused_port().await;

        // Try to connect to the port that's not listening
        let output = GelfTcpBuilder::new("127.0.0.1", unused_port)
            .connect_timeout_ms(100)
            .auto_reconnect(false)
            .build();

        let result = output.connect().await;
        assert!(
            result.is_err(),
            "Expected connection to fail for unused port {}",
            unused_port
        );
        assert!(!output.connected.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_gelf_tcp_send_without_connect() {
        // Use a port that's unlikely to be in use (avoid 12201 where Graylog might be running)
        // Also disable auto_reconnect to ensure the test is deterministic
        let config = GelfTcpConfig {
            auto_reconnect: false,
            ..GelfTcpConfig::new("127.0.0.1", 59999)
        };
        let output = GelfTcpOutput::new(config);
        let event = sample_event();

        // Should fail because not connected and auto_reconnect is disabled
        let result = output.send(&event).await;
        assert!(result.is_err());

        let stats = output.stats();
        assert_eq!(stats.events_failed, 1);
    }

    #[tokio::test]
    async fn test_gelf_tcp_builder() {
        let output = GelfTcpBuilder::new("graylog.local", 12201)
            .source_host("my-app")
            .connect_timeout_ms(3000)
            .write_timeout_ms(5000)
            .auto_reconnect(true)
            .build();

        assert_eq!(output.config.host, "graylog.local");
        assert_eq!(output.config.port, 12201);
        assert_eq!(output.config.source_host, Some("my-app".to_string()));
        assert_eq!(output.config.connect_timeout_ms, 3000);
        assert_eq!(output.config.write_timeout_ms, 5000);
        assert!(output.config.auto_reconnect);
    }

    #[tokio::test]
    async fn test_gelf_tcp_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept connection in background
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let output = GelfTcpOutput::with_address("127.0.0.1", addr.port());
        output.connect().await.unwrap();
        assert!(output.connected.load(Ordering::SeqCst));

        output.disconnect().await;
        assert!(!output.connected.load(Ordering::SeqCst));
    }

    // ============================================================================
    // Circuit Breaker Integration Tests (now using middleware wrapper)
    // ============================================================================

    #[tokio::test]
    async fn test_gelf_tcp_with_circuit_breaker_middleware() {
        use crate::{middleware::CircuitBreakerOutput, CircuitBreakerConfig, CircuitState};

        let tcp = GelfTcpBuilder::new("127.0.0.1", 59999) // Non-existent port
            .connect_timeout_ms(50)
            .auto_reconnect(false)
            .build();

        // Wrap with circuit breaker middleware
        let protected = CircuitBreakerOutput::new(tcp, CircuitBreakerConfig::new(2, 60_000));

        let event = sample_event();

        // First two failures should open the circuit
        let _ = protected.send(&event).await;
        let _ = protected.send(&event).await;

        // Circuit should now be open
        assert_eq!(protected.circuit_state(), CircuitState::Open);

        // Third request should be rejected by circuit breaker
        let result = protected.send(&event).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Circuit breaker is open"));

        // Check rejection was counted
        assert!(protected.events_rejected() > 0);
    }

    #[tokio::test]
    async fn test_gelf_tcp_circuit_breaker_middleware_success() {
        use crate::{middleware::CircuitBreakerOutput, CircuitBreakerConfig, CircuitState};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept connections and consume data
        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            while socket.read(&mut buf).await.unwrap_or(0) > 0 {}
        });

        let tcp = GelfTcpBuilder::new("127.0.0.1", addr.port()).build();
        let protected = CircuitBreakerOutput::new(tcp, CircuitBreakerConfig::new(3, 30_000));

        protected.inner().connect().await.unwrap();

        let event = sample_event();
        protected.send(&event).await.unwrap();
        protected.send(&event).await.unwrap();

        // Circuit should remain closed
        assert_eq!(protected.circuit_state(), CircuitState::Closed);
        assert_eq!(protected.circuit_stats().total_successes, 2);
        assert_eq!(protected.events_rejected(), 0);

        protected.inner().disconnect().await;
        let _ = server_handle.await;
    }
}
