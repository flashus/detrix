//! GELF Stream Router
//!
//! Routes events to different Graylog streams based on:
//! - Connection ID (which debugged process)
//! - Metric name patterns (e.g., "error_*" → errors stream)
//! - Metric group (from metric configuration)
//!
//! Streams are implemented via the `_stream` custom field in GELF messages.

use super::GelfMessage;
use crate::error::ToOutputResult;
use async_trait::async_trait;
use detrix_core::MetricEvent;
use detrix_core::Result;
use detrix_ports::{EventOutput, OutputStats};
use regex::Regex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Rule for routing events to streams
#[derive(Debug, Clone)]
pub enum StreamRule {
    /// Route by connection ID (exact match)
    ConnectionId {
        connection_id: String,
        stream: String,
    },

    /// Route by metric name pattern (regex)
    MetricPattern { pattern: Regex, stream: String },

    /// Route events with errors to a specific stream
    ErrorsTo { stream: String },

    /// Default stream for unmatched events
    Default { stream: String },
}

impl StreamRule {
    /// Create a connection ID rule
    pub fn connection(connection_id: impl Into<String>, stream: impl Into<String>) -> Self {
        Self::ConnectionId {
            connection_id: connection_id.into(),
            stream: stream.into(),
        }
    }

    /// Create a metric pattern rule
    pub fn pattern(pattern: &str, stream: impl Into<String>) -> Result<Self> {
        let regex = Regex::new(pattern).output_context("Invalid regex pattern")?;
        Ok(Self::MetricPattern {
            pattern: regex,
            stream: stream.into(),
        })
    }

    /// Create an errors rule
    pub fn errors(stream: impl Into<String>) -> Self {
        Self::ErrorsTo {
            stream: stream.into(),
        }
    }

    /// Create a default rule
    pub fn default_stream(stream: impl Into<String>) -> Self {
        Self::Default {
            stream: stream.into(),
        }
    }

    /// Check if this rule matches the event and return the stream name
    fn matches(&self, event: &MetricEvent) -> Option<&str> {
        match self {
            Self::ConnectionId {
                connection_id,
                stream,
            } => {
                if event.connection_id.0 == *connection_id {
                    Some(stream)
                } else {
                    None
                }
            }
            Self::MetricPattern { pattern, stream } => {
                if pattern.is_match(&event.metric_name) {
                    Some(stream)
                } else {
                    None
                }
            }
            Self::ErrorsTo { stream } => {
                if event.is_error {
                    Some(stream)
                } else {
                    None
                }
            }
            Self::Default { stream } => Some(stream),
        }
    }
}

/// Configuration for stream routing
#[derive(Debug, Clone, Default)]
pub struct RouterConfig {
    /// Rules evaluated in order (first match wins)
    pub rules: Vec<StreamRule>,
    /// Whether to add connection_id as a custom field
    pub add_connection_field: bool,
}

impl RouterConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_rule(mut self, rule: StreamRule) -> Self {
        self.rules.push(rule);
        self
    }

    pub fn with_rules(mut self, rules: Vec<StreamRule>) -> Self {
        self.rules.extend(rules);
        self
    }

    pub fn add_connection_field(mut self, enabled: bool) -> Self {
        self.add_connection_field = enabled;
        self
    }
}

/// GELF output with stream routing for TCP transport
///
/// Wraps a GelfTcpOutput and adds stream routing based on rules.
/// This is the production implementation that actually applies
/// routing rules to GELF messages.
pub struct RoutedGelfTcpOutput {
    inner: Arc<super::GelfTcpOutput>,
    config: RouterConfig,
    extra_fields: std::collections::HashMap<String, String>,
    events_routed: AtomicU64,
}

impl RoutedGelfTcpOutput {
    /// Create a new routed TCP output
    pub fn new(
        inner: Arc<super::GelfTcpOutput>,
        config: RouterConfig,
        extra_fields: std::collections::HashMap<String, String>,
    ) -> Self {
        Self {
            inner,
            config,
            extra_fields,
            events_routed: AtomicU64::new(0),
        }
    }

    /// Find the stream for an event based on rules
    fn find_stream(&self, event: &MetricEvent) -> Option<&str> {
        for rule in &self.config.rules {
            if let Some(stream) = rule.matches(event) {
                return Some(stream);
            }
        }
        None
    }

    /// Create a routed GELF message from an event
    fn create_routed_message(&self, event: &MetricEvent) -> GelfMessage {
        let source = self.inner.source_host();
        let mut msg = GelfMessage::from_event(event, source);

        // Apply routing: add _stream field if a rule matched
        if let Some(stream) = self.find_stream(event) {
            msg.add_field("_stream", serde_json::json!(stream));
        }

        // Optionally add connection_id as a separate searchable field
        if self.config.add_connection_field {
            msg.add_field(
                "_detrix_connection",
                serde_json::json!(&event.connection_id.0),
            );
        }

        // Add extra fields from config
        for (key, value) in &self.extra_fields {
            msg.add_field(key, serde_json::json!(value));
        }

        msg
    }
}

#[async_trait]
impl EventOutput for RoutedGelfTcpOutput {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        let msg = self.create_routed_message(event);
        self.events_routed.fetch_add(1, Ordering::Relaxed);
        self.inner.send_message(&msg).await
    }

    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        for event in events {
            self.send(event).await?;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }

    fn stats(&self) -> OutputStats {
        self.inner.stats()
    }

    fn is_healthy(&self) -> bool {
        self.inner.is_healthy()
    }

    fn name(&self) -> &str {
        "gelf-tcp-routed"
    }
}

/// GELF output with stream routing (generic wrapper for testing)
///
/// This is a pass-through wrapper used for testing and when routing
/// is not needed. For production TCP routing, use `RoutedGelfTcpOutput`.
pub struct RoutedGelfOutput<O: EventOutput> {
    inner: Arc<O>,
    config: RouterConfig,
    events_routed: AtomicU64,
}

impl<O: EventOutput> RoutedGelfOutput<O> {
    /// Create a new routed output
    pub fn new(inner: Arc<O>, config: RouterConfig) -> Self {
        Self {
            inner,
            config,
            events_routed: AtomicU64::new(0),
        }
    }

    /// Find the stream for an event based on rules
    pub fn find_stream(&self, event: &MetricEvent) -> Option<&str> {
        for rule in &self.config.rules {
            if let Some(stream) = rule.matches(event) {
                return Some(stream);
            }
        }
        None
    }

    /// Create a GELF message with routing applied
    pub fn create_routed_message(
        &self,
        event: &MetricEvent,
        source_host: Option<&str>,
    ) -> GelfMessage {
        let mut msg = GelfMessage::from_event(event, source_host);

        // Apply routing
        if let Some(stream) = self.find_stream(event) {
            msg.add_field("_stream", serde_json::json!(stream));
        }

        if self.config.add_connection_field {
            msg.add_field(
                "_detrix_connection",
                serde_json::json!(&event.connection_id.0),
            );
        }

        msg
    }
}

#[async_trait]
impl<O: EventOutput + 'static> EventOutput for RoutedGelfOutput<O> {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        self.events_routed.fetch_add(1, Ordering::Relaxed);
        self.inner.send(event).await
    }

    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        self.inner.send_batch(events).await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }

    fn stats(&self) -> OutputStats {
        let mut stats = self.inner.stats();
        stats.events_sent = self.events_routed.load(Ordering::Relaxed);
        stats
    }

    fn is_healthy(&self) -> bool {
        self.inner.is_healthy()
    }

    fn name(&self) -> &str {
        "gelf-routed"
    }
}

/// Builder for creating routed GELF outputs
pub struct RoutedGelfBuilder<O: EventOutput> {
    inner: Arc<O>,
    rules: Vec<StreamRule>,
    add_connection_field: bool,
}

impl<O: EventOutput + 'static> RoutedGelfBuilder<O> {
    pub fn new(inner: Arc<O>) -> Self {
        Self {
            inner,
            rules: vec![],
            add_connection_field: true,
        }
    }

    /// Add a routing rule
    pub fn rule(mut self, rule: StreamRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Route specific connection to a stream
    pub fn route_connection(
        mut self,
        connection_id: impl Into<String>,
        stream: impl Into<String>,
    ) -> Self {
        self.rules
            .push(StreamRule::connection(connection_id, stream));
        self
    }

    /// Route metric pattern to a stream
    pub fn route_pattern(mut self, pattern: &str, stream: impl Into<String>) -> Result<Self> {
        self.rules.push(StreamRule::pattern(pattern, stream)?);
        Ok(self)
    }

    /// Route errors to a dedicated stream
    pub fn route_errors(mut self, stream: impl Into<String>) -> Self {
        self.rules.push(StreamRule::errors(stream));
        self
    }

    /// Set default stream for unmatched events
    pub fn default_stream(mut self, stream: impl Into<String>) -> Self {
        self.rules.push(StreamRule::default_stream(stream));
        self
    }

    /// Whether to add connection_id as a custom field
    pub fn add_connection_field(mut self, enabled: bool) -> Self {
        self.add_connection_field = enabled;
        self
    }

    /// Build the routed output
    pub fn build(self) -> RoutedGelfOutput<O> {
        let config = RouterConfig {
            rules: self.rules,
            add_connection_field: self.add_connection_field,
        };
        RoutedGelfOutput::new(self.inner, config)
    }
}

/// Extension trait to add routing to any GELF message
pub trait GelfMessageExt {
    /// Set the stream for this message
    fn with_stream(self, stream: &str) -> Self;

    /// Set stream based on routing rules
    fn apply_routing(self, rules: &[StreamRule], event: &MetricEvent) -> Self;
}

impl GelfMessageExt for GelfMessage {
    fn with_stream(mut self, stream: &str) -> Self {
        self.add_field("_stream", serde_json::json!(stream));
        self
    }

    fn apply_routing(mut self, rules: &[StreamRule], event: &MetricEvent) -> Self {
        for rule in rules {
            if let Some(stream) = rule.matches(event) {
                self.add_field("_stream", serde_json::json!(stream));
                break;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_core::{ConnectionId, MetricId};
    use detrix_ports::NullOutput;

    fn sample_event(name: &str, connection: &str, is_error: bool) -> MetricEvent {
        MetricEvent {
            id: Some(1),
            metric_id: MetricId(42),
            metric_name: name.to_string(),
            connection_id: ConnectionId::new(connection),
            timestamp: 1733590800_123456,
            thread_name: Some("MainThread".to_string()),
            thread_id: Some(12345),
            value_json: r#"{"value": 42}"#.to_string(),
            value_numeric: Some(42.0),
            value_string: None,
            value_boolean: None,
            is_error,
            error_type: if is_error {
                Some("TestError".to_string())
            } else {
                None
            },
            error_message: if is_error {
                Some("test error".to_string())
            } else {
                None
            },
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        }
    }

    #[test]
    fn test_stream_rule_connection_match() {
        let rule = StreamRule::connection("trading-bot", "trading-stream");
        let event = sample_event("test", "trading-bot", false);

        assert_eq!(rule.matches(&event), Some("trading-stream"));
    }

    #[test]
    fn test_stream_rule_connection_no_match() {
        let rule = StreamRule::connection("trading-bot", "trading-stream");
        let event = sample_event("test", "other-service", false);

        assert_eq!(rule.matches(&event), None);
    }

    #[test]
    fn test_stream_rule_pattern_match() {
        let rule = StreamRule::pattern(r"^error_.*", "error-stream").unwrap();
        let event = sample_event("error_context", "test", false);

        assert_eq!(rule.matches(&event), Some("error-stream"));
    }

    #[test]
    fn test_stream_rule_pattern_no_match() {
        let rule = StreamRule::pattern(r"^error_.*", "error-stream").unwrap();
        let event = sample_event("order_placed", "test", false);

        assert_eq!(rule.matches(&event), None);
    }

    #[test]
    fn test_stream_rule_errors_match() {
        let rule = StreamRule::errors("error-stream");
        let event = sample_event("test", "test", true);

        assert_eq!(rule.matches(&event), Some("error-stream"));
    }

    #[test]
    fn test_stream_rule_errors_no_match() {
        let rule = StreamRule::errors("error-stream");
        let event = sample_event("test", "test", false);

        assert_eq!(rule.matches(&event), None);
    }

    #[test]
    fn test_stream_rule_default() {
        let rule = StreamRule::default_stream("default-stream");
        let event = sample_event("test", "test", false);

        assert_eq!(rule.matches(&event), Some("default-stream"));
    }

    #[test]
    fn test_router_config_first_match_wins() {
        let config = RouterConfig::new()
            .with_rule(StreamRule::connection("special", "special-stream"))
            .with_rule(StreamRule::errors("error-stream"))
            .with_rule(StreamRule::default_stream("default-stream"));

        // Should match connection rule first
        let event1 = sample_event("test", "special", true);
        let stream1 = config
            .rules
            .iter()
            .find_map(|r| r.matches(&event1))
            .unwrap();
        assert_eq!(stream1, "special-stream");

        // Should match error rule
        let event2 = sample_event("test", "other", true);
        let stream2 = config
            .rules
            .iter()
            .find_map(|r| r.matches(&event2))
            .unwrap();
        assert_eq!(stream2, "error-stream");

        // Should match default
        let event3 = sample_event("test", "other", false);
        let stream3 = config
            .rules
            .iter()
            .find_map(|r| r.matches(&event3))
            .unwrap();
        assert_eq!(stream3, "default-stream");
    }

    #[test]
    fn test_gelf_message_with_stream() {
        let event = sample_event("test", "test", false);
        let msg = GelfMessage::from_event(&event, None).with_stream("my-stream");

        assert_eq!(
            msg.extra_fields.get("_stream"),
            Some(&serde_json::json!("my-stream"))
        );
    }

    #[test]
    fn test_gelf_message_apply_routing() {
        let rules = vec![
            StreamRule::pattern(r"^order_.*", "orders").unwrap(),
            StreamRule::default_stream("default"),
        ];

        let event1 = sample_event("order_placed", "test", false);
        let msg1 = GelfMessage::from_event(&event1, None).apply_routing(&rules, &event1);
        assert_eq!(
            msg1.extra_fields.get("_stream"),
            Some(&serde_json::json!("orders"))
        );

        let event2 = sample_event("user_login", "test", false);
        let msg2 = GelfMessage::from_event(&event2, None).apply_routing(&rules, &event2);
        assert_eq!(
            msg2.extra_fields.get("_stream"),
            Some(&serde_json::json!("default"))
        );
    }

    #[tokio::test]
    async fn test_routed_gelf_output() {
        let inner = Arc::new(NullOutput);
        let routed = RoutedGelfBuilder::new(inner)
            .route_connection("trading", "trading-stream")
            .route_errors("errors")
            .default_stream("default")
            .build();

        let event = sample_event("test", "trading", false);
        routed.send(&event).await.unwrap();

        let stats = routed.stats();
        assert_eq!(stats.events_sent, 1);
    }

    #[tokio::test]
    async fn test_routed_gelf_builder_pattern() {
        let inner = Arc::new(NullOutput);
        let routed = RoutedGelfBuilder::new(inner)
            .route_pattern(r"^payment_.*", "payments")
            .unwrap()
            .default_stream("default")
            .build();

        assert_eq!(routed.name(), "gelf-routed");
    }

    #[test]
    fn test_invalid_pattern() {
        let result = StreamRule::pattern(r"[invalid", "stream");
        assert!(result.is_err());
    }

    // =========================================================================
    // E2E Tests for RoutedGelfTcpOutput (actual stream routing over TCP)
    // =========================================================================

    #[tokio::test]
    async fn test_routed_tcp_output_adds_stream_field() {
        use super::super::GelfTcpOutput;
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        // Start mock TCP server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn server to receive one message
        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        // Create TCP output and connect
        let tcp_output = Arc::new(GelfTcpOutput::with_address("127.0.0.1", addr.port()));
        tcp_output.connect().await.unwrap();

        // Create routed output with trading-bot → trading-stream rule
        let config = RouterConfig::new()
            .with_rule(StreamRule::connection("trading-bot", "trading-stream"))
            .with_rule(StreamRule::default_stream("default-stream"))
            .add_connection_field(true);

        let routed = RoutedGelfTcpOutput::new(tcp_output, config, std::collections::HashMap::new());

        // Send event from trading-bot connection
        let event = sample_event("order_placed", "trading-bot", false);
        routed.send(&event).await.unwrap();
        routed.flush().await.unwrap();

        // Get data from server
        let received = server_handle.await.unwrap();

        // Parse JSON (without null byte)
        let json_bytes = &received[..received.len() - 1];
        let parsed: serde_json::Value = serde_json::from_slice(json_bytes).unwrap();

        // Verify _stream field is present and correct
        assert_eq!(parsed["_stream"], "trading-stream");
        assert_eq!(parsed["_detrix_connection"], "trading-bot");
        assert_eq!(parsed["_metric_name"], "order_placed");
    }

    #[tokio::test]
    async fn test_routed_tcp_output_default_stream() {
        use super::super::GelfTcpOutput;
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        let tcp_output = Arc::new(GelfTcpOutput::with_address("127.0.0.1", addr.port()));
        tcp_output.connect().await.unwrap();

        let config = RouterConfig::new()
            .with_rule(StreamRule::connection("special", "special-stream"))
            .with_rule(StreamRule::default_stream("fallback-stream"));

        let routed = RoutedGelfTcpOutput::new(tcp_output, config, std::collections::HashMap::new());

        // Send event from different connection (should hit default)
        let event = sample_event("user_login", "other-service", false);
        routed.send(&event).await.unwrap();
        routed.flush().await.unwrap();

        let received = server_handle.await.unwrap();
        let json_bytes = &received[..received.len() - 1];
        let parsed: serde_json::Value = serde_json::from_slice(json_bytes).unwrap();

        assert_eq!(parsed["_stream"], "fallback-stream");
    }

    #[tokio::test]
    async fn test_routed_tcp_output_error_routing() {
        use super::super::GelfTcpOutput;
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        let tcp_output = Arc::new(GelfTcpOutput::with_address("127.0.0.1", addr.port()));
        tcp_output.connect().await.unwrap();

        let config = RouterConfig::new()
            .with_rule(StreamRule::errors("errors-stream"))
            .with_rule(StreamRule::default_stream("default-stream"));

        let routed = RoutedGelfTcpOutput::new(tcp_output, config, std::collections::HashMap::new());

        // Send error event
        let event = sample_event("error_context", "any-service", true);
        routed.send(&event).await.unwrap();
        routed.flush().await.unwrap();

        let received = server_handle.await.unwrap();
        let json_bytes = &received[..received.len() - 1];
        let parsed: serde_json::Value = serde_json::from_slice(json_bytes).unwrap();

        assert_eq!(parsed["_stream"], "errors-stream");
        assert_eq!(parsed["_is_error"], true);
        assert_eq!(parsed["_error_type"], "TestError");
    }

    #[tokio::test]
    async fn test_routed_tcp_output_pattern_matching() {
        use super::super::GelfTcpOutput;
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        let tcp_output = Arc::new(GelfTcpOutput::with_address("127.0.0.1", addr.port()));
        tcp_output.connect().await.unwrap();

        let config = RouterConfig::new()
            .with_rule(StreamRule::pattern(r"^order_.*", "orders-stream").unwrap())
            .with_rule(StreamRule::pattern(r"^payment_.*", "payments-stream").unwrap())
            .with_rule(StreamRule::default_stream("default-stream"));

        let routed = RoutedGelfTcpOutput::new(tcp_output, config, std::collections::HashMap::new());

        // Send order metric
        let event = sample_event("order_placed", "any", false);
        routed.send(&event).await.unwrap();
        routed.flush().await.unwrap();

        let received = server_handle.await.unwrap();
        let json_bytes = &received[..received.len() - 1];
        let parsed: serde_json::Value = serde_json::from_slice(json_bytes).unwrap();

        assert_eq!(parsed["_stream"], "orders-stream");
    }

    #[tokio::test]
    async fn test_routed_tcp_output_extra_fields() {
        use super::super::GelfTcpOutput;
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        let tcp_output = Arc::new(GelfTcpOutput::with_address("127.0.0.1", addr.port()));
        tcp_output.connect().await.unwrap();

        let config = RouterConfig::new().with_rule(StreamRule::default_stream("default"));

        let mut extra_fields = std::collections::HashMap::new();
        extra_fields.insert("environment".to_string(), "production".to_string());
        extra_fields.insert("datacenter".to_string(), "us-east-1".to_string());

        let routed = RoutedGelfTcpOutput::new(tcp_output, config, extra_fields);

        let event = sample_event("test_metric", "test", false);
        routed.send(&event).await.unwrap();
        routed.flush().await.unwrap();

        let received = server_handle.await.unwrap();
        let json_bytes = &received[..received.len() - 1];
        let parsed: serde_json::Value = serde_json::from_slice(json_bytes).unwrap();

        // Extra fields should be prefixed with underscore
        assert_eq!(parsed["_environment"], "production");
        assert_eq!(parsed["_datacenter"], "us-east-1");
    }

    /// E2E Test: Full scenario with multiple events routed to different streams
    ///
    /// This test simulates a real-world scenario where:
    /// 1. Trading bot events go to "trading" stream
    /// 2. Error events go to "errors" stream
    /// 3. Everything else goes to "default" stream
    #[tokio::test]
    async fn test_e2e_full_routing_scenario() {
        use super::super::GelfTcpOutput;
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server that receives 4 messages
        let server_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut all_data = Vec::new();
            let mut buf = vec![0u8; 16384];

            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        all_data.extend_from_slice(&buf[..n]);
                        let null_count = all_data.iter().filter(|&&b| b == 0).count();
                        if null_count >= 4 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            // Parse all messages
            let messages: Vec<serde_json::Value> = all_data
                .split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| serde_json::from_slice(s).unwrap())
                .collect();

            messages
        });

        let tcp_output = Arc::new(GelfTcpOutput::with_address("127.0.0.1", addr.port()));
        tcp_output.connect().await.unwrap();

        // Complex routing rules (order matters - first match wins)
        let config = RouterConfig::new()
            .with_rule(StreamRule::connection("trading-bot", "trading"))
            .with_rule(StreamRule::errors("errors"))
            .with_rule(StreamRule::pattern(r"^api_.*", "api").unwrap())
            .with_rule(StreamRule::default_stream("default"))
            .add_connection_field(true);

        let mut extra = std::collections::HashMap::new();
        extra.insert("version".to_string(), "1.0.0".to_string());

        let routed = RoutedGelfTcpOutput::new(tcp_output.clone(), config, extra);

        // Send 4 events with different routing destinations
        let events = vec![
            sample_event("order_placed", "trading-bot", false), // → trading
            sample_event("error_context", "api-server", true),  // → errors
            sample_event("api_request", "api-server", false),   // → api
            sample_event("user_login", "auth-service", false),  // → default
        ];

        for event in &events {
            routed.send(event).await.unwrap();
        }
        routed.flush().await.unwrap();
        tcp_output.disconnect().await;

        let messages = server_handle.await.unwrap();
        assert_eq!(messages.len(), 4);

        // Verify routing for each message
        assert_eq!(messages[0]["_stream"], "trading");
        assert_eq!(messages[0]["_metric_name"], "order_placed");
        assert_eq!(messages[0]["_detrix_connection"], "trading-bot");

        assert_eq!(messages[1]["_stream"], "errors");
        assert_eq!(messages[1]["_is_error"], true);

        assert_eq!(messages[2]["_stream"], "api");
        assert_eq!(messages[2]["_metric_name"], "api_request");

        assert_eq!(messages[3]["_stream"], "default");
        assert_eq!(messages[3]["_metric_name"], "user_login");

        // All should have extra fields
        for msg in &messages {
            assert_eq!(msg["_version"], "1.0.0");
        }
    }
}
