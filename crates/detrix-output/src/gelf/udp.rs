//! GELF UDP Transport
//!
//! Implements GELF over UDP with optional gzip compression and chunking.
//! See: <https://go2docs.graylog.org/current/getting_in_log_data/gelf.html#GELFviaUDP>
//!
//! # Chunking
//!
//! GELF over UDP has a maximum packet size of 8192 bytes. For larger messages,
//! the GELF specification defines a chunking protocol:
//!
//! - Each chunk has a 12-byte header:
//!   - Magic bytes: 0x1e 0x0f (2 bytes)
//!   - Message ID: 8 random bytes (unique per message)
//!   - Sequence number: 1 byte (0-indexed)
//!   - Sequence count: 1 byte (total number of chunks)
//! - Maximum 128 chunks per message
//! - Chunk data: up to 8180 bytes (8192 - 12)
//!
//! Chunking can be enabled/disabled via configuration.

use super::GelfMessage;
use crate::error::{OptionToOutputResult, ToOutputResult};
use async_trait::async_trait;
use detrix_config::constants::{
    DEFAULT_API_HOST, DEFAULT_COMPRESSION_LEVEL, DEFAULT_GELF_PORT, GELF_CHUNK_DATA_SIZE,
    GELF_CHUNK_HEADER_SIZE, GELF_CHUNK_MAGIC, GELF_MAX_CHUNKS, MAX_GELF_UDP_MESSAGE_SIZE,
};
use detrix_core::MetricEvent;
use detrix_core::Result;
use detrix_ports::{EventOutput, OutputStats};
use flate2::write::GzEncoder;
use flate2::Compression;
use rand::Rng;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

/// Configuration for GELF UDP output
#[derive(Debug, Clone)]
pub struct GelfUdpConfig {
    /// Graylog server address (e.g., "graylog:12201")
    pub address: String,
    /// Hostname to include in GELF messages (default: "detrix")
    pub source_host: Option<String>,
    /// Whether to use gzip compression (recommended for UDP)
    pub compress: bool,
    /// Compression level (0-9, default: 6)
    pub compression_level: u32,
    /// Enable chunking for large messages (default: true)
    /// When disabled, messages larger than 8192 bytes will fail
    pub enable_chunking: bool,
}

impl Default for GelfUdpConfig {
    fn default() -> Self {
        Self {
            address: format!("{}:{}", DEFAULT_API_HOST, DEFAULT_GELF_PORT),
            source_host: None,
            compress: true, // Compression recommended for UDP
            compression_level: DEFAULT_COMPRESSION_LEVEL,
            enable_chunking: true, // Enable chunking by default
        }
    }
}

impl GelfUdpConfig {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            ..Default::default()
        }
    }

    pub fn with_source_host(mut self, host: impl Into<String>) -> Self {
        self.source_host = Some(host.into());
        self
    }

    pub fn with_compression(mut self, enabled: bool) -> Self {
        self.compress = enabled;
        self
    }

    pub fn with_compression_level(mut self, level: u32) -> Self {
        self.compression_level = level.min(9);
        self
    }

    pub fn with_chunking(mut self, enabled: bool) -> Self {
        self.enable_chunking = enabled;
        self
    }
}

/// GELF UDP output implementation
///
/// Sends events to Graylog over UDP with optional gzip compression.
pub struct GelfUdpOutput {
    config: GelfUdpConfig,
    socket: Mutex<Option<UdpSocket>>,
    target_addr: SocketAddr,
    connected: AtomicBool,
    events_sent: AtomicU64,
    events_failed: AtomicU64,
    bytes_sent: AtomicU64,
}

impl GelfUdpOutput {
    /// Create a new GELF UDP output
    pub fn new(config: GelfUdpConfig) -> Result<Self> {
        let target_addr: SocketAddr = config
            .address
            .parse()
            .output_context(&format!("Invalid address '{}'", config.address))?;

        Ok(Self {
            config,
            socket: Mutex::new(None),
            target_addr,
            connected: AtomicBool::new(false),
            events_sent: AtomicU64::new(0),
            events_failed: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
        })
    }

    /// Create with default config for given address
    pub fn with_address(address: impl Into<String>) -> Result<Self> {
        Self::new(GelfUdpConfig::new(address))
    }

    /// Bind the UDP socket
    pub async fn bind(&self) -> Result<()> {
        // Bind to any available port
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .output_context("Failed to bind UDP socket")?;

        let mut guard = self.socket.lock().await;
        *guard = Some(socket);
        self.connected.store(true, Ordering::SeqCst);

        tracing::info!("GELF UDP bound, target: {}", self.target_addr);
        Ok(())
    }

    /// Compress data using gzip
    fn compress_data(&self, data: &[u8]) -> Result<Vec<u8>> {
        let level = Compression::new(self.config.compression_level);
        let mut encoder = GzEncoder::new(Vec::new(), level);
        encoder
            .write_all(data)
            .output_context("Compression failed")?;
        encoder
            .finish()
            .output_context("Compression finalize failed")
    }

    /// Send raw bytes over UDP
    async fn send_bytes(&self, bytes: &[u8]) -> Result<()> {
        let guard = self.socket.lock().await;
        let socket = guard.as_ref().output_ok_or("UDP socket not bound")?;

        socket
            .send_to(bytes, self.target_addr)
            .await
            .output_context("UDP send failed")?;

        self.bytes_sent
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Create GELF chunks from data that exceeds the maximum packet size
    ///
    /// Each chunk has a 12-byte header:
    /// - Magic bytes: 0x1e 0x0f (2 bytes)
    /// - Message ID: 8 random bytes (unique per message)
    /// - Sequence number: 1 byte (0-indexed)
    /// - Sequence count: 1 byte (total chunks)
    fn create_chunks(&self, data: &[u8]) -> Result<Vec<Vec<u8>>> {
        let chunk_count = data.len().div_ceil(GELF_CHUNK_DATA_SIZE);

        if chunk_count > GELF_MAX_CHUNKS {
            return Err(detrix_core::Error::Output(format!(
                "Message too large: requires {} chunks (max {}). Consider using TCP transport.",
                chunk_count, GELF_MAX_CHUNKS
            )));
        }

        // Generate a random 8-byte message ID
        let message_id: [u8; 8] = rand::rng().random();

        let mut chunks = Vec::with_capacity(chunk_count);

        for (seq_num, chunk_data) in data.chunks(GELF_CHUNK_DATA_SIZE).enumerate() {
            let mut chunk = Vec::with_capacity(GELF_CHUNK_HEADER_SIZE + chunk_data.len());

            // Magic bytes
            chunk.extend_from_slice(&GELF_CHUNK_MAGIC);
            // Message ID
            chunk.extend_from_slice(&message_id);
            // Sequence number (0-indexed)
            chunk.push(seq_num as u8);
            // Sequence count
            chunk.push(chunk_count as u8);
            // Data
            chunk.extend_from_slice(chunk_data);

            chunks.push(chunk);
        }

        Ok(chunks)
    }

    /// Send a chunked message over UDP
    async fn send_chunked(&self, data: &[u8]) -> Result<()> {
        let chunks = self.create_chunks(data)?;
        let chunk_count = chunks.len();

        tracing::debug!(
            "Sending GELF message in {} chunks ({} bytes total)",
            chunk_count,
            data.len()
        );

        for chunk in chunks {
            self.send_bytes(&chunk).await?;
        }

        Ok(())
    }
}

#[async_trait]
impl EventOutput for GelfUdpOutput {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        let source = self.config.source_host.as_deref();
        let gelf_msg = GelfMessage::from_event(event, source);

        let json_bytes = gelf_msg.to_json().output_context("Serialization failed")?;

        let bytes = if self.config.compress {
            self.compress_data(&json_bytes)?
        } else {
            json_bytes
        };

        // Check if message needs chunking
        let result = if bytes.len() > MAX_GELF_UDP_MESSAGE_SIZE {
            if self.config.enable_chunking {
                // Use chunking for large messages
                self.send_chunked(&bytes).await
            } else {
                // Chunking disabled, fail with error
                return Err(detrix_core::Error::Output(format!(
                    "Message too large for UDP: {} bytes (max {}). Enable chunking or use TCP.",
                    bytes.len(),
                    MAX_GELF_UDP_MESSAGE_SIZE
                )));
            }
        } else {
            // Small enough to send directly
            self.send_bytes(&bytes).await
        };

        match result {
            Ok(()) => {
                self.events_sent.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(e) => {
                self.events_failed.fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    async fn flush(&self) -> Result<()> {
        // UDP is connectionless, no flush needed
        Ok(())
    }

    fn stats(&self) -> OutputStats {
        OutputStats {
            events_sent: self.events_sent.load(Ordering::Relaxed),
            events_failed: self.events_failed.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            reconnections: 0, // UDP is connectionless
            connected: self.connected.load(Ordering::SeqCst),
        }
    }

    fn name(&self) -> &str {
        "gelf-udp"
    }
}

/// Builder for GelfUdpOutput
pub struct GelfUdpBuilder {
    config: GelfUdpConfig,
}

impl GelfUdpBuilder {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            config: GelfUdpConfig::new(address),
        }
    }

    pub fn source_host(mut self, host: impl Into<String>) -> Self {
        self.config.source_host = Some(host.into());
        self
    }

    pub fn compress(mut self, enabled: bool) -> Self {
        self.config.compress = enabled;
        self
    }

    pub fn compression_level(mut self, level: u32) -> Self {
        self.config.compression_level = level.min(9);
        self
    }

    /// Enable or disable chunking for large messages (default: enabled)
    pub fn chunking(mut self, enabled: bool) -> Self {
        self.config.enable_chunking = enabled;
        self
    }

    pub fn build(self) -> Result<GelfUdpOutput> {
        GelfUdpOutput::new(self.config)
    }

    /// Build and bind the socket immediately
    pub async fn bind(self) -> Result<GelfUdpOutput> {
        let output = self.build()?;
        output.bind().await?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_testing::fixtures::sample_test_event as sample_event;

    #[test]
    fn test_gelf_udp_config_builder() {
        let config = GelfUdpConfig::new("graylog.example.com:12201")
            .with_source_host("my-service")
            .with_compression(false)
            .with_compression_level(9);

        assert_eq!(config.address, "graylog.example.com:12201");
        assert_eq!(config.source_host, Some("my-service".to_string()));
        assert!(!config.compress);
        assert_eq!(config.compression_level, 9);
    }

    #[test]
    fn test_gelf_udp_output_new() {
        let output = GelfUdpOutput::with_address("127.0.0.1:12201").unwrap();
        assert_eq!(output.name(), "gelf-udp");
        assert!(!output.connected.load(Ordering::SeqCst)); // Not bound yet
    }

    #[test]
    fn test_gelf_udp_stats_initial() {
        let output = GelfUdpOutput::with_address("127.0.0.1:12201").unwrap();
        let stats = output.stats();

        assert_eq!(stats.events_sent, 0);
        assert_eq!(stats.events_failed, 0);
        assert_eq!(stats.bytes_sent, 0);
        assert_eq!(stats.reconnections, 0);
        assert!(!stats.connected);
    }

    #[test]
    fn test_gelf_udp_invalid_address() {
        let result = GelfUdpOutput::with_address("invalid-address");
        assert!(result.is_err());
    }

    #[test]
    fn test_gelf_udp_compression() {
        let output = GelfUdpBuilder::new("127.0.0.1:12201")
            .compress(true)
            .compression_level(6)
            .build()
            .unwrap();

        let data = b"Hello, this is test data that should be compressed!";
        let compressed = output.compress_data(data).unwrap();

        // Compressed data should have gzip magic header
        assert_eq!(compressed[0], 0x1f);
        assert_eq!(compressed[1], 0x8b);
    }

    #[tokio::test]
    async fn test_gelf_udp_bind() {
        let output = GelfUdpBuilder::new("127.0.0.1:12201").build().unwrap();

        output.bind().await.unwrap();
        assert!(output.connected.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_gelf_udp_send_without_bind() {
        let output = GelfUdpOutput::with_address("127.0.0.1:12201").unwrap();
        let event = sample_event();

        // Should fail because socket not bound
        let result = output.send(&event).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_gelf_udp_send_to_mock_server() {
        use tokio::net::UdpSocket;

        // Start a mock UDP server
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        // Create and bind client
        let output = GelfUdpBuilder::new(server_addr.to_string())
            .compress(true)
            .bind()
            .await
            .unwrap();

        // Send an event
        let event = sample_event();
        output.send(&event).await.unwrap();

        // Receive on server
        let mut buf = vec![0u8; 8192];
        let (len, _) = server.recv_from(&mut buf).await.unwrap();
        buf.truncate(len);

        // Should be gzip compressed (magic header)
        assert_eq!(buf[0], 0x1f);
        assert_eq!(buf[1], 0x8b);

        // Decompress and verify JSON
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&buf[..]);
        let mut json_str = String::new();
        decoder.read_to_string(&mut json_str).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["version"], "1.1");
        assert_eq!(parsed["_metric_name"], "test_metric");

        // Check stats
        let stats = output.stats();
        assert_eq!(stats.events_sent, 1);
        assert!(stats.bytes_sent > 0);
    }

    #[tokio::test]
    async fn test_gelf_udp_uncompressed() {
        use tokio::net::UdpSocket;

        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let output = GelfUdpBuilder::new(server_addr.to_string())
            .compress(false)
            .bind()
            .await
            .unwrap();

        let event = sample_event();
        output.send(&event).await.unwrap();

        let mut buf = vec![0u8; 8192];
        let (len, _) = server.recv_from(&mut buf).await.unwrap();
        buf.truncate(len);

        // Should be plain JSON (starts with '{')
        assert_eq!(buf[0], b'{');

        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["version"], "1.1");
    }

    #[tokio::test]
    async fn test_gelf_udp_builder() {
        let output = GelfUdpBuilder::new("192.168.1.100:12201")
            .source_host("my-app")
            .compress(true)
            .compression_level(9)
            .build()
            .unwrap();

        assert_eq!(output.config.address, "192.168.1.100:12201");
        assert_eq!(output.config.source_host, Some("my-app".to_string()));
        assert!(output.config.compress);
        assert_eq!(output.config.compression_level, 9);
    }

    #[test]
    fn test_gelf_udp_chunking_config() {
        // Default should have chunking enabled
        let config = GelfUdpConfig::default();
        assert!(config.enable_chunking);

        // Can disable chunking
        let config = GelfUdpConfig::new("127.0.0.1:12201").with_chunking(false);
        assert!(!config.enable_chunking);
    }

    #[test]
    fn test_gelf_udp_create_chunks_small_message() {
        let output = GelfUdpOutput::with_address("127.0.0.1:12201").unwrap();

        // Small message should create single chunk
        let data = vec![0u8; 1000];
        let chunks = output.create_chunks(&data).unwrap();

        assert_eq!(chunks.len(), 1);
        // Verify header
        assert_eq!(chunks[0][0], GELF_CHUNK_MAGIC[0]);
        assert_eq!(chunks[0][1], GELF_CHUNK_MAGIC[1]);
        // Sequence number 0, count 1
        assert_eq!(chunks[0][10], 0);
        assert_eq!(chunks[0][11], 1);
    }

    #[test]
    fn test_gelf_udp_create_chunks_large_message() {
        let output = GelfUdpOutput::with_address("127.0.0.1:12201").unwrap();

        // Message larger than one chunk (GELF_CHUNK_DATA_SIZE = 8180)
        let data = vec![0u8; 20000];
        let chunks = output.create_chunks(&data).unwrap();

        // Should need 3 chunks (20000 / 8180 = 2.44, so 3)
        assert_eq!(chunks.len(), 3);

        // Verify all chunks have same message ID
        let msg_id = &chunks[0][2..10];
        for chunk in &chunks {
            assert_eq!(&chunk[2..10], msg_id);
        }

        // Verify sequence numbers
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk[0], GELF_CHUNK_MAGIC[0]);
            assert_eq!(chunk[1], GELF_CHUNK_MAGIC[1]);
            assert_eq!(chunk[10], i as u8); // Sequence number
            assert_eq!(chunk[11], 3); // Sequence count
        }
    }

    #[test]
    fn test_gelf_udp_create_chunks_too_large() {
        let output = GelfUdpOutput::with_address("127.0.0.1:12201").unwrap();

        // Message requiring more than 128 chunks
        let data = vec![0u8; GELF_CHUNK_DATA_SIZE * 129];
        let result = output.create_chunks(&data);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("requires 129 chunks"));
    }

    #[tokio::test]
    async fn test_gelf_udp_send_chunked_message() {
        use tokio::net::UdpSocket;

        // Start a mock UDP server
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        // Create output with NO compression so we can control size
        let output = GelfUdpBuilder::new(server_addr.to_string())
            .compress(false)
            .chunking(true)
            .bind()
            .await
            .unwrap();

        // Create a large event with a very long value
        let mut event = sample_event();
        // Make the value_json large enough to require chunking when uncompressed
        let large_value = "x".repeat(10000);
        if let Some(v) = event.values.first_mut() {
            v.value_json = large_value;
        }

        // Send should succeed with chunking
        output.send(&event).await.unwrap();

        // Receive chunks on server
        let mut chunks_received = Vec::new();
        let mut buf = vec![0u8; MAX_GELF_UDP_MESSAGE_SIZE];

        // Use timeout to avoid hanging
        let timeout = tokio::time::Duration::from_millis(100);
        loop {
            match tokio::time::timeout(timeout, server.recv_from(&mut buf)).await {
                Ok(Ok((len, _))) => {
                    chunks_received.push(buf[..len].to_vec());
                }
                _ => break,
            }
        }

        // Should have received multiple chunks
        assert!(
            chunks_received.len() > 1,
            "Expected multiple chunks, got {}",
            chunks_received.len()
        );

        // Verify all chunks have GELF magic
        for chunk in &chunks_received {
            assert_eq!(chunk[0], GELF_CHUNK_MAGIC[0]);
            assert_eq!(chunk[1], GELF_CHUNK_MAGIC[1]);
        }

        // Verify all chunks have same message ID
        let msg_id = &chunks_received[0][2..10];
        for chunk in &chunks_received {
            assert_eq!(&chunk[2..10], msg_id);
        }
    }

    #[tokio::test]
    async fn test_gelf_udp_chunking_disabled_fails_large_message() {
        let output = GelfUdpBuilder::new("127.0.0.1:12201")
            .compress(false)
            .chunking(false)
            .build()
            .unwrap();
        output.bind().await.unwrap();

        // Create a large event
        let mut event = sample_event();
        if let Some(v) = event.values.first_mut() {
            v.value_json = "x".repeat(10000);
        }

        // Should fail because chunking is disabled
        let result = output.send(&event).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("too large"));
    }
}
