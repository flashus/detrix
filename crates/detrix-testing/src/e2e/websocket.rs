//! WebSocket Client for Event Streaming
//!
//! Provides a client for connecting to the Detrix WebSocket endpoint
//! and receiving real-time metric events.

use detrix_core::{CapturedStackTrace, MemorySnapshot};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// WebSocket event message received from the server
#[derive(Debug, Clone, Deserialize)]
pub struct EventMessage {
    pub metric_id: i64,
    pub metric_name: String,
    pub timestamp: i64,
    pub value_json: String,
    /// Captured stack trace (if introspection enabled)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stack_trace: Option<CapturedStackTrace>,
    /// Captured memory snapshot (if introspection enabled)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub memory_snapshot: Option<MemorySnapshot>,
}

/// WebSocket streaming client
pub struct WebSocketClient {
    url: String,
}

impl WebSocketClient {
    /// Create a new WebSocket client
    pub fn new(http_port: u16) -> Self {
        Self {
            url: format!("ws://127.0.0.1:{}/ws", http_port),
        }
    }

    /// Create a new WebSocket client with custom URL
    pub fn with_url(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    /// Connect and receive events for a specified duration
    ///
    /// Returns all events received during the connection period.
    pub async fn receive_events(
        &self,
        duration: Duration,
        max_events: Option<usize>,
    ) -> Result<Vec<EventMessage>, WebSocketError> {
        // Connect to WebSocket using the URL string directly
        let (ws_stream, _) = timeout(Duration::from_secs(10), connect_async(&self.url))
            .await
            .map_err(|_| WebSocketError::new("Connection timeout"))?
            .map_err(|e| WebSocketError::new(format!("Connection failed: {}", e)))?;

        let (mut write, mut read) = ws_stream.split();

        let mut events = Vec::new();
        let max_events = max_events.unwrap_or(usize::MAX);

        // Receive events for the specified duration
        let deadline = tokio::time::Instant::now() + duration;

        loop {
            if tokio::time::Instant::now() >= deadline {
                break;
            }

            if events.len() >= max_events {
                break;
            }

            match timeout(Duration::from_millis(100), read.next()).await {
                Ok(Some(Ok(msg))) => match msg {
                    Message::Text(text) => {
                        if let Ok(event) = serde_json::from_str::<EventMessage>(&text) {
                            events.push(event);
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                },
                Ok(Some(Err(e))) => {
                    return Err(WebSocketError::new(format!("Read error: {}", e)));
                }
                Ok(None) => break,  // Stream ended
                Err(_) => continue, // Timeout, continue waiting
            }
        }

        // Close connection gracefully
        let _ = write.send(Message::Close(None)).await;

        Ok(events)
    }

    /// Create a streaming channel for events
    ///
    /// Returns a receiver that can be used to consume events as they arrive.
    pub async fn stream_events(
        &self,
    ) -> Result<(mpsc::Receiver<EventMessage>, WebSocketHandle), WebSocketError> {
        // Connect to WebSocket using the URL string directly
        let (ws_stream, _) = timeout(Duration::from_secs(10), connect_async(&self.url))
            .await
            .map_err(|_| WebSocketError::new("Connection timeout"))?
            .map_err(|e| WebSocketError::new(format!("Connection failed: {}", e)))?;

        let (mut write, mut read) = ws_stream.split();

        let (tx, rx) = mpsc::channel(100);
        let (close_tx, mut close_rx) = mpsc::channel::<()>(1);

        // Spawn task to forward events
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = read.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(event) = serde_json::from_str::<EventMessage>(&text) {
                                    if tx.send(event).await.is_err() {
                                        break; // Receiver dropped
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Err(_)) => break,
                            _ => {}
                        }
                    }
                    _ = close_rx.recv() => {
                        let _ = write.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
        });

        Ok((rx, WebSocketHandle { close_tx }))
    }
}

/// Handle for controlling a WebSocket connection
pub struct WebSocketHandle {
    close_tx: mpsc::Sender<()>,
}

impl WebSocketHandle {
    /// Close the WebSocket connection
    pub async fn close(self) {
        let _ = self.close_tx.send(()).await;
    }
}

/// WebSocket error
#[derive(Debug)]
pub struct WebSocketError {
    pub message: String,
}

impl WebSocketError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WebSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WebSocketError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_message_deserialize() {
        let json =
            r#"{"metric_id": 1, "metric_name": "test", "timestamp": 123, "value_json": "{}"}"#;
        let event: EventMessage = serde_json::from_str(json).unwrap();
        assert_eq!(event.metric_id, 1);
        assert_eq!(event.metric_name, "test");
        assert_eq!(event.timestamp, 123);
        assert_eq!(event.value_json, "{}");
    }
}
