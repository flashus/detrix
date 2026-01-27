//! GELF HTTP Transport
//!
//! Implements GELF over HTTP/HTTPS with batching support.
//! See: <https://go2docs.graylog.org/current/getting_in_log_data/gelf.html#GELFviaHTTP>

use super::GelfMessage;
use crate::error::ToOutputResult;
use async_trait::async_trait;
use detrix_config::constants::{
    DEFAULT_API_HOST, DEFAULT_GELF_HTTP_BATCH_SIZE, DEFAULT_GELF_HTTP_FLUSH_INTERVAL_MS,
    DEFAULT_GELF_HTTP_PATH, DEFAULT_GELF_HTTP_TIMEOUT_MS, DEFAULT_GELF_PORT,
};
use detrix_core::{MetricEvent, Result};
use detrix_ports::{EventOutput, OutputStats};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Configuration for GELF HTTP output
#[derive(Debug, Clone)]
pub struct GelfHttpConfig {
    /// Graylog GELF HTTP endpoint URL (e.g., "http://graylog:12201/gelf")
    pub url: String,
    /// Hostname to include in GELF messages (default: "detrix")
    pub source_host: Option<String>,
    /// Request timeout in milliseconds
    pub timeout_ms: u64,
    /// Maximum batch size (number of messages)
    pub batch_size: usize,
    /// Flush interval in milliseconds (0 = no auto-flush)
    pub flush_interval_ms: u64,
    /// Whether to use gzip compression
    pub compress: bool,
    /// Optional authentication token
    pub auth_token: Option<String>,
}

impl Default for GelfHttpConfig {
    fn default() -> Self {
        Self {
            url: format!(
                "http://{}:{}{}",
                DEFAULT_API_HOST, DEFAULT_GELF_PORT, DEFAULT_GELF_HTTP_PATH
            ),
            source_host: None,
            timeout_ms: DEFAULT_GELF_HTTP_TIMEOUT_MS,
            batch_size: DEFAULT_GELF_HTTP_BATCH_SIZE,
            flush_interval_ms: DEFAULT_GELF_HTTP_FLUSH_INTERVAL_MS,
            compress: false,
            auth_token: None,
        }
    }
}

impl GelfHttpConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Default::default()
        }
    }

    pub fn with_source_host(mut self, host: impl Into<String>) -> Self {
        self.source_host = Some(host.into());
        self
    }

    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size;
        self
    }

    pub fn with_compression(mut self, enabled: bool) -> Self {
        self.compress = enabled;
        self
    }

    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }
}

/// Internal buffer for batching
struct BatchBuffer {
    messages: Vec<GelfMessage>,
}

impl BatchBuffer {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    fn add(&mut self, msg: GelfMessage) {
        self.messages.push(msg);
    }

    fn take(&mut self) -> Vec<GelfMessage> {
        std::mem::take(&mut self.messages)
    }

    fn len(&self) -> usize {
        self.messages.len()
    }

    #[allow(dead_code)] // Used for future flush interval logic
    fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// GELF HTTP output implementation
///
/// Sends events to Graylog over HTTP with optional batching.
/// Flushes automatically when batch_size is reached or flush_interval_ms elapses.
pub struct GelfHttpOutput {
    config: GelfHttpConfig,
    client: reqwest::Client,
    buffer: Mutex<BatchBuffer>,
    last_flush: Mutex<Instant>,
    connected: AtomicBool,
    events_sent: AtomicU64,
    events_failed: AtomicU64,
    bytes_sent: AtomicU64,
}

impl GelfHttpOutput {
    /// Create a new GELF HTTP output
    pub fn new(config: GelfHttpConfig) -> Result<Self> {
        let mut builder =
            reqwest::Client::builder().timeout(Duration::from_millis(config.timeout_ms));

        // Note: gzip compression is handled by the compress config field
        // which affects whether we send Accept-Encoding header
        if !config.compress {
            builder = builder.no_gzip();
        }

        let client = builder
            .build()
            .output_context("Failed to create HTTP client")?;

        Ok(Self {
            config,
            client,
            buffer: Mutex::new(BatchBuffer::new()),
            last_flush: Mutex::new(Instant::now()),
            connected: AtomicBool::new(true), // HTTP is connectionless
            events_sent: AtomicU64::new(0),
            events_failed: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
        })
    }

    /// Create with default config for given URL
    pub fn with_url(url: impl Into<String>) -> Result<Self> {
        Self::new(GelfHttpConfig::new(url))
    }

    /// Send a batch of messages
    async fn send_messages(&self, messages: Vec<GelfMessage>) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }

        // For GELF HTTP, we send messages one at a time or as NDJSON
        // Graylog accepts individual JSON messages
        for msg in &messages {
            let body = msg.to_json().output_context("Serialization failed")?;

            let mut request = self.client.post(&self.config.url).body(body.clone());

            // Add content type
            request = request.header("Content-Type", "application/json");

            // Add auth token if configured
            if let Some(ref token) = self.config.auth_token {
                request = request.header("Authorization", format!("Bearer {}", token));
            }

            let response = request.send().await.output_context("HTTP request failed")?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(detrix_core::Error::Output(format!(
                    "Graylog returned {}: {}",
                    status, body
                )));
            }

            self.bytes_sent
                .fetch_add(body.len() as u64, Ordering::Relaxed);
        }

        self.events_sent
            .fetch_add(messages.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Flush the buffer if batch size reached or flush interval elapsed
    async fn maybe_flush(&self) -> Result<()> {
        let should_flush = {
            let buffer = self.buffer.lock().await;
            if buffer.is_empty() {
                return Ok(());
            }

            // Flush if batch size reached
            if buffer.len() >= self.config.batch_size {
                true
            } else if self.config.flush_interval_ms > 0 {
                // Flush if time interval elapsed
                let last_flush = self.last_flush.lock().await;
                last_flush.elapsed() >= Duration::from_millis(self.config.flush_interval_ms)
            } else {
                false
            }
        };

        if !should_flush {
            return Ok(());
        }

        // Take messages and update last flush time
        let messages = {
            let mut buffer = self.buffer.lock().await;
            let mut last_flush = self.last_flush.lock().await;
            *last_flush = Instant::now();
            buffer.take()
        };

        if messages.is_empty() {
            return Ok(());
        }

        self.send_messages(messages).await
    }
}

#[async_trait]
impl EventOutput for GelfHttpOutput {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        let source = self.config.source_host.as_deref();
        let gelf_msg = GelfMessage::from_event(event, source);

        {
            let mut buffer = self.buffer.lock().await;
            buffer.add(gelf_msg);
        }

        // Check if we should flush
        if let Err(e) = self.maybe_flush().await {
            self.events_failed.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }

        Ok(())
    }

    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        let source = self.config.source_host.as_deref();
        let messages: Vec<GelfMessage> = events
            .iter()
            .map(|e| GelfMessage::from_event(e, source))
            .collect();

        self.send_messages(messages).await
    }

    async fn flush(&self) -> Result<()> {
        let messages = {
            let mut buffer = self.buffer.lock().await;
            let mut last_flush = self.last_flush.lock().await;
            *last_flush = Instant::now();
            buffer.take()
        };

        if messages.is_empty() {
            return Ok(());
        }

        self.send_messages(messages).await
    }

    fn stats(&self) -> OutputStats {
        OutputStats {
            events_sent: self.events_sent.load(Ordering::Relaxed),
            events_failed: self.events_failed.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            reconnections: 0, // HTTP is connectionless
            connected: self.connected.load(Ordering::SeqCst),
        }
    }

    fn name(&self) -> &str {
        "gelf-http"
    }
}

/// Builder for GelfHttpOutput
pub struct GelfHttpBuilder {
    config: GelfHttpConfig,
}

impl GelfHttpBuilder {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            config: GelfHttpConfig::new(url),
        }
    }

    pub fn source_host(mut self, host: impl Into<String>) -> Self {
        self.config.source_host = Some(host.into());
        self
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.config.timeout_ms = ms;
        self
    }

    pub fn batch_size(mut self, size: usize) -> Self {
        self.config.batch_size = size;
        self
    }

    pub fn flush_interval_ms(mut self, ms: u64) -> Self {
        self.config.flush_interval_ms = ms;
        self
    }

    pub fn compress(mut self, enabled: bool) -> Self {
        self.config.compress = enabled;
        self
    }

    pub fn auth_token(mut self, token: impl Into<String>) -> Self {
        self.config.auth_token = Some(token.into());
        self
    }

    pub fn build(self) -> Result<Arc<GelfHttpOutput>> {
        Ok(Arc::new(GelfHttpOutput::new(self.config)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_testing::fixtures::sample_test_event as sample_event;

    #[test]
    fn test_gelf_http_config_builder() {
        let config = GelfHttpConfig::new("http://graylog.example.com/gelf")
            .with_source_host("my-service")
            .with_batch_size(50)
            .with_compression(true)
            .with_auth_token("secret-token");

        assert_eq!(config.url, "http://graylog.example.com/gelf");
        assert_eq!(config.source_host, Some("my-service".to_string()));
        assert_eq!(config.batch_size, 50);
        assert!(config.compress);
        assert_eq!(config.auth_token, Some("secret-token".to_string()));
    }

    #[tokio::test]
    async fn test_gelf_http_output_new() {
        let output = GelfHttpOutput::with_url("http://localhost:12201/gelf").unwrap();
        assert_eq!(output.name(), "gelf-http");
        assert!(output.is_healthy());
    }

    #[tokio::test]
    async fn test_gelf_http_stats_initial() {
        let output = GelfHttpOutput::with_url("http://localhost:12201/gelf").unwrap();
        let stats = output.stats();

        assert_eq!(stats.events_sent, 0);
        assert_eq!(stats.events_failed, 0);
        assert_eq!(stats.bytes_sent, 0);
        assert_eq!(stats.reconnections, 0);
        assert!(stats.connected);
    }

    #[tokio::test]
    async fn test_gelf_http_batching() {
        let output = GelfHttpBuilder::new("http://localhost:12201/gelf")
            .batch_size(5)
            .build()
            .unwrap();

        // Add events but don't reach batch size
        for _ in 0..3 {
            let event = sample_event();
            // This won't actually send since we can't reach the server
            // but it tests the batching logic
            let _ = output.send(&event).await;
        }

        // Check buffer has items
        let buffer_len = {
            let buffer = output.buffer.lock().await;
            buffer.len()
        };
        assert_eq!(buffer_len, 3);
    }

    #[tokio::test]
    async fn test_gelf_http_builder() {
        let output = GelfHttpBuilder::new("http://graylog.local/gelf")
            .source_host("my-app")
            .timeout_ms(5000)
            .batch_size(25)
            .flush_interval_ms(500)
            .compress(true)
            .auth_token("my-token")
            .build()
            .unwrap();

        assert_eq!(output.config.url, "http://graylog.local/gelf");
        assert_eq!(output.config.source_host, Some("my-app".to_string()));
        assert_eq!(output.config.timeout_ms, 5000);
        assert_eq!(output.config.batch_size, 25);
        assert!(output.config.compress);
    }

    #[tokio::test]
    async fn test_batch_buffer() {
        let mut buffer = BatchBuffer::new();
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);

        let event = sample_event();
        let msg = GelfMessage::from_event(&event, None);
        buffer.add(msg);

        assert!(!buffer.is_empty());
        assert_eq!(buffer.len(), 1);

        let messages = buffer.take();
        assert_eq!(messages.len(), 1);
        assert!(buffer.is_empty());
    }
}
