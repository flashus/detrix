//! DAP Broker - Manages communication with debug adapters
//!
//! The broker handles:
//! - Message framing (Content-Length headers per DAP spec)
//! - Sequence number generation
//! - Request/response correlation
//! - Event broadcasting to subscribers

use crate::{Error, Event, ProtocolMessage, Request, Response, Result};
use detrix_config::AdapterConnectionConfig;
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::{debug, info, trace, warn};

/// Channel for sending responses back to request callers
type ResponseSender = oneshot::Sender<Result<Response>>;

/// Channel for broadcasting events to subscribers (bounded for backpressure)
type EventReceiver = mpsc::Receiver<Event>;

/// DAP Broker manages communication with a debug adapter
pub struct DapBroker {
    /// Next sequence number for outgoing messages
    next_seq: Arc<Mutex<i64>>,

    /// Pending requests awaiting responses (keyed by request seq)
    pending_requests: Arc<RwLock<HashMap<i64, ResponseSender>>>,

    /// Event subscribers (bounded channels for backpressure)
    event_subscribers: Arc<RwLock<Vec<mpsc::Sender<Event>>>>,

    /// Adapter stdin (for sending messages)
    stdin: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,

    /// Reader task handle (for graceful shutdown)
    reader_task: Option<tokio::task::JoinHandle<()>>,

    /// Adapter connection configuration
    config: AdapterConnectionConfig,
}

impl DapBroker {
    /// Create a new DAP broker with the given I/O streams and config
    pub fn new_with_config<R, W>(reader: R, writer: W, config: AdapterConnectionConfig) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let next_seq = Arc::new(Mutex::new(1));
        let pending_requests = Arc::new(RwLock::new(HashMap::new()));
        let event_subscribers = Arc::new(RwLock::new(Vec::new()));
        let stdin = Arc::new(Mutex::new(
            Box::new(writer) as Box<dyn AsyncWrite + Send + Unpin>
        ));

        // Spawn reader task to process incoming messages
        let reader_task =
            Self::spawn_reader_task(reader, pending_requests.clone(), event_subscribers.clone());

        Self {
            next_seq,
            pending_requests,
            event_subscribers,
            stdin,
            reader_task: Some(reader_task),
            config,
        }
    }

    /// Create a new DAP broker with default config
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::new_with_config(reader, writer, AdapterConnectionConfig::default())
    }

    /// Get next sequence number (thread-safe)
    pub async fn next_sequence(&self) -> i64 {
        let mut seq = self.next_seq.lock().await;
        let current = *seq;
        *seq += 1;
        current
    }

    /// Send a message without waiting for response
    pub async fn send_message_no_wait(&self, message: ProtocolMessage) -> Result<()> {
        self.send_message(message).await
    }

    /// Send a request and wait for response
    ///
    /// Uses tracing instrumentation for performance monitoring:
    /// - Tracks lock acquisition time for pending_requests RwLock
    /// - Records request timeout and overall latency
    #[tracing::instrument(skip(self, command, arguments), fields(seq))]
    pub async fn send_request(
        &self,
        command: impl Into<String>,
        arguments: Option<serde_json::Value>,
    ) -> Result<Response> {
        use std::time::Instant;

        let cmd: String = command.into();
        let seq = self.next_sequence().await;
        tracing::Span::current().record("seq", seq);
        debug!(command = %cmd, "Sending DAP request");

        let request = Request {
            seq,
            command: cmd,
            arguments,
        };

        // Register pending request before sending (track lock acquisition time)
        let (tx, rx) = oneshot::channel();
        {
            let lock_start = Instant::now();
            let mut pending = self.pending_requests.write().await;
            let lock_duration_us = lock_start.elapsed().as_micros();
            if lock_duration_us > 1000 {
                // Log if lock acquisition took > 1ms (potential contention)
                warn!(
                    lock_duration_us = lock_duration_us,
                    pending_count = pending.len(),
                    "Slow lock acquisition on pending_requests (register)"
                );
            }
            pending.insert(seq, tx);
        }

        // Send request
        self.send_message(ProtocolMessage::Request(request)).await?;

        // Wait for response (with timeout from config)
        let timeout_ms = self.config.request_timeout_ms;
        let response_start = Instant::now();
        match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), rx).await {
            Ok(Ok(response)) => {
                tracing::debug!(
                    response_time_ms = response_start.elapsed().as_millis() as u64,
                    "DAP request completed"
                );
                response
            }
            Ok(Err(_)) => Err(Error::Communication("Response channel closed".to_string())),
            Err(_) => {
                // Timeout - clean up pending request (track lock acquisition time)
                let lock_start = Instant::now();
                let mut pending = self.pending_requests.write().await;
                let lock_duration_us = lock_start.elapsed().as_micros();
                if lock_duration_us > 1000 {
                    warn!(
                        lock_duration_us = lock_duration_us,
                        pending_count = pending.len(),
                        "Slow lock acquisition on pending_requests (cleanup)"
                    );
                }
                pending.remove(&seq);
                warn!(timeout_ms = timeout_ms, "DAP request timed out");
                Err(Error::Timeout(timeout_ms))
            }
        }
    }

    /// Subscribe to events (returns bounded receiver with configurable capacity)
    ///
    /// The channel has bounded capacity to provide backpressure when consumers
    /// are slow. If the channel is full, events will be dropped with a warning.
    ///
    /// NOTE: This method cleans up any closed subscribers before adding the new one.
    /// This prevents accumulation of stale subscribers when called multiple times
    /// (e.g., on reconnect).
    pub async fn subscribe_events(&self) -> EventReceiver {
        let capacity = self.config.event_channel_capacity;
        let (tx, rx) = mpsc::channel(capacity);
        let mut subscribers = self.event_subscribers.write().await;

        // Clean up any closed subscribers before adding new one
        // This prevents accumulation of stale subscribers on reconnect
        let before = subscribers.len();
        subscribers.retain(|existing_tx| !existing_tx.is_closed());
        let removed = before - subscribers.len();
        if removed > 0 {
            debug!(
                "Cleaned up {} stale subscriber(s) before new subscription",
                removed
            );
        }

        subscribers.push(tx);
        debug!(
            "New event subscriber registered (capacity: {}, total subscribers: {})",
            capacity,
            subscribers.len()
        );
        rx
    }

    /// Get current subscriber count
    pub async fn subscriber_count(&self) -> usize {
        self.event_subscribers.read().await.len()
    }

    /// Clean up dropped subscribers (removes senders whose receivers have been dropped)
    ///
    /// Note: Subscribers are automatically cleaned up when events are broadcast,
    /// but this method can be called explicitly for eager cleanup when no events
    /// are being sent.
    pub async fn cleanup_dropped_subscribers(&self) -> usize {
        let mut subscribers = self.event_subscribers.write().await;
        let before = subscribers.len();
        subscribers.retain(|tx| !tx.is_closed());
        before - subscribers.len()
    }

    /// Clean up orphaned pending requests (removes entries whose receivers have been dropped)
    ///
    /// This can happen when a caller abandons a request future before timeout.
    /// The pending request entry stays in the map until this cleanup runs,
    /// a response arrives, or the connection closes.
    ///
    /// Returns the number of orphaned requests that were cleaned up.
    pub async fn cleanup_orphaned_requests(&self) -> usize {
        let mut pending = self.pending_requests.write().await;
        let before = pending.len();
        pending.retain(|_, tx| !tx.is_closed());
        let removed = before - pending.len();
        if removed > 0 {
            debug!(
                "Cleaned up {} orphaned pending request(s) (receiver dropped)",
                removed
            );
        }
        removed
    }

    /// Get current pending request count
    pub async fn pending_request_count(&self) -> usize {
        self.pending_requests.read().await.len()
    }

    /// Check if the broker's reader task is still alive
    ///
    /// Returns false if the reader task has exited (connection died, adapter crashed, etc.)
    /// This can be used by upper layers to detect zombie connections and trigger reconnection.
    pub fn is_alive(&self) -> bool {
        match &self.reader_task {
            Some(handle) => !handle.is_finished(),
            None => false,
        }
    }

    /// Send a protocol message (frames with Content-Length header)
    async fn send_message(&self, message: ProtocolMessage) -> Result<()> {
        let json = serde_json::to_string(&message)?;
        let content = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);

        let mut writer = self.stdin.lock().await;
        writer.write_all(content.as_bytes()).await?;
        writer.flush().await?;

        trace!("Sent message: {}", json);
        Ok(())
    }

    /// Spawn background task to read messages from the adapter
    fn spawn_reader_task<R>(
        reader: R,
        pending_requests: Arc<RwLock<HashMap<i64, ResponseSender>>>,
        event_subscribers: Arc<RwLock<Vec<mpsc::Sender<Event>>>>,
    ) -> tokio::task::JoinHandle<()>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        tokio::spawn(async move {
            debug!("Reader task started");
            let mut buf_reader = BufReader::new(reader);

            loop {
                match Self::read_message(&mut buf_reader).await {
                    Ok(Some(message)) => {
                        debug!("Received message: {:?}", message);

                        // Check for session-ending events - these signal the debug session has ended
                        // We need to proactively close the connection since the debugger may
                        // keep the TCP socket open even after sending these events
                        // - "terminated": Debug session ended (always signals end)
                        // - "exited": Debuggee process exited (typically followed by terminated,
                        //             but some debuggers may only send exited)
                        let session_end_event = match &message {
                            ProtocolMessage::Event(e)
                                if e.event == "terminated" || e.event == "exited" =>
                            {
                                Some(e.event.clone())
                            }
                            _ => None,
                        };

                        Self::handle_message(message, &pending_requests, &event_subscribers).await;

                        if let Some(event_name) = session_end_event {
                            info!(
                                "Received '{}' event - closing connection proactively",
                                event_name
                            );
                            break;
                        }
                    }
                    Ok(None) => {
                        info!("Adapter connection closed (EOF received)");
                        break;
                    }
                    Err(e) => {
                        info!("Adapter connection error (triggering cleanup): {}", e);
                        break;
                    }
                }
            }

            // Clean up on exit - notify all pending requests
            let mut pending = pending_requests.write().await;
            for (_, tx) in pending.drain() {
                let _ = tx.send(Err(Error::Communication(
                    "Adapter disconnected".to_string(),
                )));
            }

            // Clear event subscribers to close their channels
            // This signals to listeners (like AdapterLifecycleManager) that the connection is closed
            let mut subscribers = event_subscribers.write().await;
            let subscriber_count = subscribers.len();
            subscribers.clear();
            info!(
                "Cleared {} event subscribers on connection close - channels closed",
                subscriber_count
            );
        })
    }

    /// Read a single message from the stream (handles Content-Length framing)
    async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Option<ProtocolMessage>>
    where
        R: AsyncRead + Unpin,
    {
        // Read headers until we find Content-Length
        let mut content_length: Option<usize> = None;
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).await?;

            if bytes_read == 0 {
                // EOF
                return Ok(None);
            }

            let line = line.trim();

            if line.is_empty() {
                // Empty line signals end of headers
                break;
            }

            if line.starts_with("Content-Length:") {
                let length_str = line.trim_start_matches("Content-Length:").trim();
                content_length = Some(length_str.parse().map_err(|_| {
                    Error::Protocol(format!("Invalid Content-Length: {}", length_str))
                })?);
            }
        }

        let length = content_length
            .ok_or_else(|| Error::Protocol("Missing Content-Length header".to_string()))?;

        // Read content
        let mut buffer = vec![0u8; length];
        reader.read_exact(&mut buffer).await?;

        let content = String::from_utf8(buffer)?;

        trace!("Received message: {}", content);

        // Parse JSON
        let message: ProtocolMessage = serde_json::from_str(&content)?;
        Ok(Some(message))
    }

    /// Handle an incoming message (response or event)
    async fn handle_message(
        message: ProtocolMessage,
        pending_requests: &Arc<RwLock<HashMap<i64, ResponseSender>>>,
        event_subscribers: &Arc<RwLock<Vec<mpsc::Sender<Event>>>>,
    ) {
        match message {
            ProtocolMessage::Response(response) => {
                // Find pending request and send response
                let mut pending = pending_requests.write().await;
                if let Some(tx) = pending.remove(&response.request_seq) {
                    if tx.send(Ok(response)).is_err() {
                        warn!("Failed to send response - receiver dropped");
                    }
                } else {
                    warn!(
                        "Received response for unknown request seq: {}",
                        response.request_seq
                    );
                }
            }
            ProtocolMessage::Event(event) => {
                // Broadcast to all subscribers using try_send (non-blocking)
                // If a subscriber's channel is full, we drop the event for that subscriber
                // and log a warning. This prevents slow consumers from blocking the broker.
                let subscriber_count = event_subscribers.read().await.len();
                trace!(
                    "Broadcasting event '{}' to {} subscribers",
                    event.event,
                    subscriber_count
                );
                let mut subscribers = event_subscribers.write().await;
                let mut dropped_count = 0;
                subscribers.retain(|tx| {
                    match tx.try_send(event.clone()) {
                        Ok(()) => true,
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            // Channel full - drop event for this subscriber
                            dropped_count += 1;
                            true // Keep subscriber, just couldn't deliver this event
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            // Receiver dropped - remove subscriber
                            false
                        }
                    }
                });
                if dropped_count > 0 {
                    warn!(
                        "Dropped event '{}' for {} slow subscriber(s) - channel(s) full",
                        event.event, dropped_count
                    );
                }
            }
            ProtocolMessage::Request(_) => {
                warn!(
                    "Received unexpected request from adapter (reverse requests not yet supported)"
                );
            }
        }
    }
}

impl Drop for DapBroker {
    fn drop(&mut self) {
        if let Some(handle) = self.reader_task.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// Helper to create a pair of in-memory streams for testing
    fn create_test_streams() -> (DuplexStream, DuplexStream) {
        tokio::io::duplex(8192)
    }

    #[tokio::test]
    async fn test_broker_creation() {
        let (client_stream, _server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = DapBroker::new(client_read, client_write);

        // Verify initial sequence number
        let seq = broker.next_sequence().await;
        assert_eq!(seq, 1);

        let seq2 = broker.next_sequence().await;
        assert_eq!(seq2, 2);
    }

    #[tokio::test]
    async fn test_sequence_number_generation() {
        let (client_stream, _server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = Arc::new(DapBroker::new(client_read, client_write));

        // Generate multiple sequence numbers concurrently
        let mut handles = vec![];
        for _ in 0..10 {
            let broker = broker.clone();
            handles.push(tokio::spawn(async move { broker.next_sequence().await }));
        }

        let mut seqs = vec![];
        for handle in handles {
            seqs.push(handle.await.unwrap());
        }

        // Should have 10 unique sequence numbers from 1 to 10
        seqs.sort();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[tokio::test]
    async fn test_message_framing_encode() {
        let json = r#"{"seq":1,"type":"request","command":"initialize"}"#;

        let message: ProtocolMessage = serde_json::from_str(json).unwrap();
        let json_out = serde_json::to_string(&message).unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n{}", json_out.len(), json_out);

        // Should have Content-Length header
        assert!(framed.starts_with("Content-Length:"));
        assert!(framed.contains("\r\n\r\n"));
    }

    #[tokio::test]
    async fn test_message_framing_decode() {
        let json = r#"{"seq":1,"type":"request","command":"initialize"}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);

        let mut reader = BufReader::new(framed.as_bytes());
        let message = DapBroker::read_message(&mut reader).await.unwrap().unwrap();

        match message {
            ProtocolMessage::Request(req) => {
                assert_eq!(req.seq, 1);
                assert_eq!(req.command, "initialize");
            }
            _ => panic!("Expected Request message"),
        }
    }

    #[tokio::test]
    async fn test_request_response_correlation() {
        let (client_stream, mut server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = DapBroker::new(client_read, client_write);

        // Spawn task to simulate server response
        tokio::spawn(async move {
            // Read request from client
            let mut buf_reader = BufReader::new(&mut server_stream);
            let message = DapBroker::read_message(&mut buf_reader)
                .await
                .unwrap()
                .unwrap();

            if let ProtocolMessage::Request(req) = message {
                // Send response
                let response = Response {
                    seq: 100,
                    request_seq: req.seq,
                    command: req.command,
                    success: true,
                    message: None,
                    body: Some(serde_json::json!({"status": "ok"})),
                };

                let response_msg = ProtocolMessage::Response(response);
                let json = serde_json::to_string(&response_msg).unwrap();
                let framed = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);

                server_stream.write_all(framed.as_bytes()).await.unwrap();
                server_stream.flush().await.unwrap();
            }
        });

        // Send request and wait for response
        let response = broker.send_request("initialize", None).await.unwrap();

        assert_eq!(response.command, "initialize");
        assert!(response.success);
        assert_eq!(response.body.unwrap()["status"], "ok");
    }

    #[tokio::test]
    async fn test_event_broadcasting() {
        let (client_stream, mut server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = DapBroker::new(client_read, client_write);

        // Subscribe to events
        let mut sub1 = broker.subscribe_events().await;
        let mut sub2 = broker.subscribe_events().await;

        // Send event from server
        tokio::spawn(async move {
            let event = Event {
                seq: 1,
                event: "output".to_string(),
                body: Some(serde_json::json!({"output": "test message"})),
            };

            let event_msg = ProtocolMessage::Event(event);
            let json = serde_json::to_string(&event_msg).unwrap();
            let framed = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);

            server_stream.write_all(framed.as_bytes()).await.unwrap();
            server_stream.flush().await.unwrap();
        });

        // Both subscribers should receive the event
        let evt1 = tokio::time::timeout(std::time::Duration::from_secs(1), sub1.recv())
            .await
            .unwrap()
            .unwrap();
        let evt2 = tokio::time::timeout(std::time::Duration::from_secs(1), sub2.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(evt1.event, "output");
        assert_eq!(evt2.event, "output");
        assert_eq!(evt1.body.unwrap()["output"], "test message");
    }

    #[tokio::test]
    async fn test_request_timeout() {
        let (client_stream, _server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = DapBroker::new(client_read, client_write);

        // Send request but don't respond - should timeout
        let result = broker.send_request("initialize", None).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Timeout(ms) => assert_eq!(ms, 30000),
            e => panic!("Expected Timeout error, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_multiple_pending_requests() {
        let (client_stream, server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);
        let (server_read, mut server_write) = tokio::io::split(server_stream);

        let broker = Arc::new(DapBroker::new(client_read, client_write));

        // Spawn server task to respond to all requests
        tokio::spawn(async move {
            let mut buf_reader = BufReader::new(server_read);
            for i in 1..=3 {
                let message = DapBroker::read_message(&mut buf_reader)
                    .await
                    .unwrap()
                    .unwrap();

                if let ProtocolMessage::Request(req) = message {
                    let response = Response {
                        seq: 100 + i,
                        request_seq: req.seq,
                        command: req.command,
                        success: true,
                        message: None,
                        body: Some(serde_json::json!({"id": i})),
                    };

                    let response_msg = ProtocolMessage::Response(response);
                    let json = serde_json::to_string(&response_msg).unwrap();
                    let framed = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);

                    server_write.write_all(framed.as_bytes()).await.unwrap();
                    server_write.flush().await.unwrap();
                }
            }
        });

        // Send multiple requests concurrently
        let broker1 = broker.clone();
        let broker2 = broker.clone();
        let broker3 = broker.clone();

        let (r1, r2, r3) = tokio::join!(
            broker1.send_request("test1", None),
            broker2.send_request("test2", None),
            broker3.send_request("test3", None),
        );

        // All should succeed
        assert!(r1.is_ok(), "r1 failed: {:?}", r1);
        assert!(r2.is_ok(), "r2 failed: {:?}", r2);
        assert!(r3.is_ok(), "r3 failed: {:?}", r3);
    }

    #[tokio::test]
    async fn test_dropped_subscribers_cleaned_on_event() {
        let (client_stream, mut server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = DapBroker::new(client_read, client_write);

        // Create 3 subscribers
        let sub1 = broker.subscribe_events().await;
        let sub2 = broker.subscribe_events().await;
        let _sub3 = broker.subscribe_events().await; // keep this one alive

        assert_eq!(broker.subscriber_count().await, 3);

        // Drop 2 subscribers
        drop(sub1);
        drop(sub2);

        // Count should still be 3 (not cleaned yet)
        assert_eq!(broker.subscriber_count().await, 3);

        // Send an event from server to trigger cleanup via retain
        // Use oneshot channel to signal completion while keeping server_stream alive
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let event = Event {
                seq: 1,
                event: "output".to_string(),
                body: None,
            };

            let event_msg = ProtocolMessage::Event(event);
            let json = serde_json::to_string(&event_msg).unwrap();
            let framed = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);

            server_stream.write_all(framed.as_bytes()).await.unwrap();
            server_stream.flush().await.unwrap();

            // Signal that event has been sent
            let _ = tx.send(());

            // Keep server_stream alive to prevent connection close
            // Wait indefinitely (test will complete before this)
            std::future::pending::<()>().await;
        });

        // Wait for event to be sent
        let _ = rx.await;

        // Give time for event to be processed and cleanup to run
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Now count should be 1 (dropped subscribers cleaned up)
        assert_eq!(
            broker.subscriber_count().await,
            1,
            "Dropped subscribers should be cleaned up when event is broadcast"
        );
    }

    #[tokio::test]
    async fn test_explicit_subscriber_cleanup() {
        let (client_stream, _server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = DapBroker::new(client_read, client_write);

        // Create 5 subscribers
        let sub1 = broker.subscribe_events().await;
        let sub2 = broker.subscribe_events().await;
        let _sub3 = broker.subscribe_events().await;
        let sub4 = broker.subscribe_events().await;
        let _sub5 = broker.subscribe_events().await;

        assert_eq!(broker.subscriber_count().await, 5);

        // Drop 3 subscribers
        drop(sub1);
        drop(sub2);
        drop(sub4);

        // Explicit cleanup should remove dropped subscribers
        let removed = broker.cleanup_dropped_subscribers().await;
        assert_eq!(removed, 3, "Should have removed 3 dropped subscribers");
        assert_eq!(
            broker.subscriber_count().await,
            2,
            "Should have 2 subscribers remaining"
        );
    }

    #[tokio::test]
    async fn test_pending_request_cleanup() {
        let (client_stream, _server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = Arc::new(DapBroker::new(client_read, client_write));

        // Verify initially empty
        assert_eq!(broker.pending_request_count().await, 0);

        // Start a request but don't await it (simulates abandoned request)
        let broker_clone = broker.clone();
        let handle = tokio::spawn(async move {
            // This will timeout but we're going to abort before that
            broker_clone.send_request("test", None).await
        });

        // Give time for request to be registered
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Should have 1 pending request
        assert_eq!(
            broker.pending_request_count().await,
            1,
            "Should have 1 pending request"
        );

        // Abort the request (simulates caller abandoning the future)
        handle.abort();

        // Give time for abort to propagate
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Cleanup should remove the orphaned request
        let removed = broker.cleanup_orphaned_requests().await;
        assert_eq!(removed, 1, "Should have cleaned up 1 orphaned request");
        assert_eq!(
            broker.pending_request_count().await,
            0,
            "Should have 0 pending requests after cleanup"
        );
    }

    #[tokio::test]
    async fn test_subscribe_events_cleans_up_closed_subscribers() {
        let (client_stream, _server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = DapBroker::new(client_read, client_write);

        // Create first subscriber and immediately drop the receiver
        let sub1 = broker.subscribe_events().await;
        assert_eq!(broker.subscriber_count().await, 1);
        drop(sub1);

        // Create second subscriber - should clean up the closed first one
        let _sub2 = broker.subscribe_events().await;

        // Should only have 1 subscriber (the second one), not 2
        assert_eq!(
            broker.subscriber_count().await,
            1,
            "Closed subscriber should be cleaned up when new one is added"
        );
    }

    #[tokio::test]
    async fn test_subscribe_events_multiple_calls_without_cleanup() {
        let (client_stream, _server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = DapBroker::new(client_read, client_write);

        // Create multiple subscribers without dropping them
        let _sub1 = broker.subscribe_events().await;
        let _sub2 = broker.subscribe_events().await;
        let _sub3 = broker.subscribe_events().await;

        // All 3 should exist since none are closed
        assert_eq!(
            broker.subscriber_count().await,
            3,
            "All active subscribers should be kept"
        );
    }

    #[tokio::test]
    async fn test_subscribe_events_cleans_up_multiple_closed() {
        let (client_stream, _server_stream) = create_test_streams();
        let (client_read, client_write) = tokio::io::split(client_stream);

        let broker = DapBroker::new(client_read, client_write);

        // Create and drop multiple subscribers
        let sub1 = broker.subscribe_events().await;
        let sub2 = broker.subscribe_events().await;
        let sub3 = broker.subscribe_events().await;
        assert_eq!(broker.subscriber_count().await, 3);

        drop(sub1);
        drop(sub2);
        drop(sub3);

        // Create new subscriber - should clean up all 3 closed ones
        let _sub4 = broker.subscribe_events().await;

        assert_eq!(
            broker.subscriber_count().await,
            1,
            "All closed subscribers should be cleaned up"
        );
    }
}
