//! WebSocket handler for real-time event streaming (PERF-04: With timeouts)
//!
//! Following Clean Architecture:
//! - Subscribe to events via ApiState broadcast channel
//! - Serialize events to JSON
//! - Send to WebSocket client
//!
//! Features:
//! - Idle timeout: Close connections with no activity
//! - Ping/pong: Keep-alive mechanism to detect dead clients
//! - Configurable via StreamingConfig
//!
//! NO business logic here - just event forwarding!

use crate::state::ApiState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use detrix_config::StreamingConfig;
use detrix_core::MetricEvent;
use futures::{sink::SinkExt, stream::StreamExt};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// WebSocket event message (JSON)
///
/// Includes all expression values and introspection data when captured.
#[derive(Debug, Serialize)]
// BREAKING CHANGE (v1.1): Field names changed from snake_case to camelCase.
// Clients must update: metric_id -> metricId, stack_trace -> stackTrace, etc.
#[serde(rename_all = "camelCase")]
struct EventMessage {
    metric_id: u64,
    metric_name: String,
    timestamp: i64,
    values: Vec<detrix_core::ExpressionValue>,
    /// Stack trace captured at the metric location (if enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    stack_trace: Option<detrix_core::CapturedStackTrace>,
    /// Memory snapshot captured at the metric location (if enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_snapshot: Option<detrix_core::MemorySnapshot>,
}

impl From<MetricEvent> for EventMessage {
    fn from(event: MetricEvent) -> Self {
        Self {
            metric_id: event.metric_id.0,
            metric_name: event.metric_name,
            timestamp: event.timestamp,
            values: event.values,
            stack_trace: event.stack_trace,
            memory_snapshot: event.memory_snapshot,
        }
    }
}

/// Global counter for active WebSocket connections (for monitoring)
///
/// # Atomic Ordering: Relaxed
///
/// `Ordering::Relaxed` is used for all operations on this counter because:
/// - This is a simple statistics counter for monitoring purposes
/// - No other memory operations depend on or synchronize with this counter
/// - Eventual consistency is acceptable - the exact count at any instant doesn't
///   affect correctness, only observability
/// - Relaxed provides the best performance for high-contention counters
///
/// If this counter were used for coordination (e.g., limiting connections),
/// we would need stronger ordering like `Acquire`/`Release` or `SeqCst`.
static ACTIVE_WS_CONNECTIONS: AtomicU64 = AtomicU64::new(0);

/// Get the number of active WebSocket connections
pub fn active_websocket_connections() -> u64 {
    // Relaxed: Statistics read - eventual consistency is acceptable
    ACTIVE_WS_CONNECTIONS.load(Ordering::Relaxed)
}

/// WebSocket upgrade handler
///
/// Upgrades the HTTP connection to WebSocket and starts streaming events
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiState>>,
) -> Response {
    info!("WebSocket: Client connecting");

    // Get streaming config from ConfigService (new connections get current config)
    let streaming_config = state
        .config_service
        .get_config()
        .await
        .api
        .streaming
        .clone();
    ws.on_upgrade(move |socket| handle_websocket(socket, state, streaming_config))
}

/// Handle WebSocket connection with timeouts
async fn handle_websocket(socket: WebSocket, state: Arc<ApiState>, config: StreamingConfig) {
    // Relaxed: Statistics counter - see ACTIVE_WS_CONNECTIONS doc comment
    ACTIVE_WS_CONNECTIONS.fetch_add(1, Ordering::Relaxed);

    let (sender, receiver) = socket.split();
    let sender = Arc::new(Mutex::new(sender));

    // Subscribe to event broadcast channel
    let mut event_rx = state.event_tx.subscribe();

    // Track last activity time for idle timeout
    let last_activity = Arc::new(Mutex::new(Instant::now()));
    // Track when we sent the last ping and whether we're waiting for pong
    let pending_pong = Arc::new(std::sync::atomic::AtomicBool::new(false));

    info!(
        idle_timeout_ms = config.ws_idle_timeout_ms,
        ping_interval_ms = config.ws_ping_interval_ms,
        "WebSocket: Client connected, streaming events..."
    );

    // Clone references for tasks
    let sender_for_events = Arc::clone(&sender);
    let sender_for_ping = Arc::clone(&sender);
    let last_activity_for_recv = Arc::clone(&last_activity);
    let pending_pong_for_recv = Arc::clone(&pending_pong);
    let pending_pong_for_ping = Arc::clone(&pending_pong);

    // Task 1: Forward events to WebSocket
    let mut send_task = tokio::spawn(async move {
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    let msg: EventMessage = event.into();
                    let json = match serde_json::to_string(&msg) {
                        Ok(json) => json,
                        Err(e) => {
                            error!("WebSocket: Failed to serialize event: {}", e);
                            continue;
                        }
                    };

                    let mut guard = sender_for_events.lock().await;
                    if guard.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("WebSocket: Client lagged, missed {} events", n);
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("WebSocket: Event channel closed");
                    break;
                }
            }
        }
    });

    // Task 2: Handle incoming messages (ping/pong, close)
    let mut recv_task = tokio::spawn(async move {
        let mut receiver = receiver;
        while let Some(result) = receiver.next().await {
            match result {
                Ok(msg) => {
                    // Update last activity time
                    {
                        let mut guard = last_activity_for_recv.lock().await;
                        *guard = Instant::now();
                    }

                    match msg {
                        Message::Text(text) => {
                            debug!("WebSocket: Received text message: {}", text);
                        }
                        Message::Binary(_) => {
                            warn!("WebSocket: Received unexpected binary message");
                        }
                        Message::Ping(_) => {
                            debug!("WebSocket: Received ping");
                        }
                        Message::Pong(_) => {
                            debug!("WebSocket: Received pong");
                            pending_pong_for_recv.store(false, Ordering::Release);
                        }
                        Message::Close(_) => {
                            info!("WebSocket: Client closed connection");
                            break;
                        }
                    }
                }
                Err(e) => {
                    warn!("WebSocket: Error receiving message: {}", e);
                    break;
                }
            }
        }
    });

    // Task 3: Ping/pong heartbeat (if enabled)
    let ping_interval = config.ws_ping_interval();
    let pong_timeout = config.ws_pong_timeout();

    let mut ping_task = ping_interval.map(|interval| {
        let mut interval_timer = tokio::time::interval(interval);
        tokio::spawn(async move {
            loop {
                interval_timer.tick().await;

                // Check if we're still waiting for a pong
                if pending_pong_for_ping.load(Ordering::Acquire) {
                    warn!("WebSocket: Pong timeout - closing connection");
                    return false; // Signal that we should close
                }

                // Send ping
                let mut guard = sender_for_ping.lock().await;
                if guard.send(Message::Ping(vec![].into())).await.is_err() {
                    return false;
                }
                pending_pong_for_ping.store(true, Ordering::Release);

                // Wait for pong with timeout
                drop(guard); // Release lock
                tokio::time::sleep(pong_timeout).await;

                // If still pending after timeout, connection is dead
                if pending_pong_for_ping.load(Ordering::Acquire) {
                    warn!(
                        pong_timeout_ms = pong_timeout.as_millis() as u64,
                        "WebSocket: Pong timeout exceeded"
                    );
                    return false;
                }
            }
        })
    });

    // Task 4: Idle timeout (if enabled)
    let idle_timeout = config.ws_idle_timeout();
    let last_activity_for_idle = Arc::clone(&last_activity);

    let idle_check_interval = config.ws_idle_check_interval();
    let mut idle_task = idle_timeout.map(|timeout| {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(idle_check_interval).await;

                let guard = last_activity_for_idle.lock().await;
                if guard.elapsed() > timeout {
                    warn!(
                        idle_ms = guard.elapsed().as_millis() as u64,
                        timeout_ms = timeout.as_millis() as u64,
                        "WebSocket: Idle timeout exceeded"
                    );
                    return false; // Signal that we should close
                }
            }
        })
    });

    // Wait for any task to finish
    tokio::select! {
        _ = &mut send_task => {
            info!("WebSocket: Send task finished");
        }
        _ = &mut recv_task => {
            info!("WebSocket: Receive task finished");
        }
        result = async { if let Some(ref mut t) = ping_task { t.await } else { Ok(true) } } => {
            if let Ok(false) = result {
                info!("WebSocket: Ping task triggered close");
            }
        }
        result = async { if let Some(ref mut t) = idle_task { t.await } else { Ok(true) } } => {
            if let Ok(false) = result {
                info!("WebSocket: Idle task triggered close");
            }
        }
    }

    // Cleanup all tasks
    send_task.abort();
    recv_task.abort();
    if let Some(t) = ping_task {
        t.abort();
    }
    if let Some(t) = idle_task {
        t.abort();
    }

    // Decrement connection counter
    ACTIVE_WS_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);

    info!(
        active_connections = ACTIVE_WS_CONNECTIONS.load(Ordering::Relaxed),
        "WebSocket: Connection closed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::create_test_state;
    use detrix_core::{ConnectionId, ExpressionValue, MetricEvent, MetricId};

    #[test]
    fn test_event_message_conversion() {
        let event = MetricEvent {
            id: None,
            metric_id: MetricId(123),
            metric_name: "test_metric".to_string(),
            connection_id: ConnectionId::from("default"),
            timestamp: 1234567890,
            thread_name: None,
            thread_id: None,
            values: vec![ExpressionValue::new("x", r#"{"value": 42}"#)],
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };

        let msg: EventMessage = event.into();

        assert_eq!(msg.metric_id, 123);
        assert_eq!(msg.metric_name, "test_metric");
        assert_eq!(msg.timestamp, 1234567890);
        assert_eq!(msg.values.len(), 1);
        assert_eq!(msg.values[0].value_json, r#"{"value": 42}"#);
        assert!(msg.stack_trace.is_none());
        assert!(msg.memory_snapshot.is_none());
    }

    #[test]
    fn test_event_message_conversion_with_introspection() {
        use detrix_core::{
            CapturedStackTrace, CapturedVariable, MemorySnapshot, SnapshotScope, StackFrame,
        };

        let stack_trace = CapturedStackTrace {
            frames: vec![
                StackFrame {
                    index: 0,
                    name: "main".to_string(),
                    file: Some("main.py".to_string()),
                    line: Some(10),
                    column: None,
                    module: Some("__main__".to_string()),
                },
                StackFrame {
                    index: 1,
                    name: "helper".to_string(),
                    file: Some("utils.py".to_string()),
                    line: Some(20),
                    column: None,
                    module: None,
                },
            ],
            total_frames: 2,
            truncated: false,
        };

        let memory_snapshot = MemorySnapshot {
            locals: vec![CapturedVariable {
                name: "x".to_string(),
                value: "42".to_string(),
                var_type: Some("int".to_string()),
                scope: Some("local".to_string()),
            }],
            globals: vec![CapturedVariable {
                name: "CONFIG".to_string(),
                value: "{}".to_string(),
                var_type: Some("dict".to_string()),
                scope: Some("global".to_string()),
            }],
            scope: SnapshotScope::Both,
        };

        let event = MetricEvent {
            id: None,
            metric_id: MetricId(456),
            metric_name: "introspection_metric".to_string(),
            connection_id: ConnectionId::from("test"),
            timestamp: 9876543210,
            thread_name: Some("MainThread".to_string()),
            thread_id: Some(1234),
            values: vec![ExpressionValue::new("", r#"{"result": "success"}"#)],
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: Some("req-123".to_string()),
            session_id: None,
            stack_trace: Some(stack_trace.clone()),
            memory_snapshot: Some(memory_snapshot.clone()),
        };

        let msg: EventMessage = event.into();

        assert_eq!(msg.metric_id, 456);
        assert_eq!(msg.metric_name, "introspection_metric");
        assert_eq!(msg.timestamp, 9876543210);
        assert_eq!(msg.values.len(), 1);
        assert_eq!(msg.values[0].value_json, r#"{"result": "success"}"#);

        // Verify introspection data is preserved
        assert!(msg.stack_trace.is_some());
        let trace = msg.stack_trace.unwrap();
        assert_eq!(trace.frames.len(), 2);
        assert_eq!(trace.frames[0].name, "main");
        assert_eq!(trace.frames[1].name, "helper");

        assert!(msg.memory_snapshot.is_some());
        let snapshot = msg.memory_snapshot.unwrap();
        assert_eq!(snapshot.locals.len(), 1);
        assert_eq!(snapshot.locals[0].name, "x");
        assert_eq!(snapshot.globals.len(), 1);
        assert_eq!(snapshot.globals[0].name, "CONFIG");
    }

    #[test]
    fn test_event_message_serialization_omits_none_introspection() {
        let event = MetricEvent {
            id: None,
            metric_id: MetricId(1),
            metric_name: "test".to_string(),
            connection_id: ConnectionId::from("default"),
            timestamp: 123,
            thread_name: None,
            thread_id: None,
            values: vec![ExpressionValue::new("", "{}")],
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };

        let msg: EventMessage = event.into();
        let json = serde_json::to_string(&msg).unwrap();

        // None introspection fields should be omitted (skip_serializing_if = "Option::is_none")
        assert!(!json.contains("stackTrace"));
        assert!(!json.contains("memorySnapshot"));
    }

    #[test]
    fn test_event_message_serialization_includes_introspection() {
        use detrix_core::{CapturedStackTrace, MemorySnapshot, SnapshotScope, StackFrame};

        let event = MetricEvent {
            id: None,
            metric_id: MetricId(1),
            metric_name: "test".to_string(),
            connection_id: ConnectionId::from("default"),
            timestamp: 123,
            thread_name: None,
            thread_id: None,
            values: vec![ExpressionValue::new("", "{}")],
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: Some(CapturedStackTrace {
                frames: vec![StackFrame {
                    index: 0,
                    name: "main".to_string(),
                    file: Some("main.py".to_string()),
                    line: Some(10),
                    column: None,
                    module: None,
                }],
                total_frames: 1,
                truncated: false,
            }),
            memory_snapshot: Some(MemorySnapshot {
                locals: vec![],
                globals: vec![],
                scope: SnapshotScope::Local,
            }),
        };

        let msg: EventMessage = event.into();
        let json = serde_json::to_string(&msg).unwrap();

        // Introspection fields should be included when present (camelCase)
        assert!(json.contains("stackTrace"));
        assert!(json.contains("memorySnapshot"));
        assert!(json.contains("\"name\":\"main\""));
    }

    #[tokio::test]
    async fn test_broadcast_channel_integration() {
        let (state, _temp_dir) = create_test_state().await;

        // Subscribe to events
        let mut rx = state.event_tx.subscribe();

        // Publish an event
        let event = MetricEvent {
            id: None,
            metric_id: MetricId(1),
            metric_name: "test".to_string(),
            connection_id: ConnectionId::from("default"),
            timestamp: 123,
            thread_name: None,
            thread_id: None,
            values: vec![ExpressionValue::new("", "{}")],
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        };

        let _ = state.event_tx.send(event.clone());

        // Should receive the event
        let received = rx.recv().await.unwrap();
        assert_eq!(received.metric_id, event.metric_id);
        assert_eq!(received.metric_name, event.metric_name);
    }
}
