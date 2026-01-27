//! DAP Protocol Tests
//!
//! These tests verify the DAP protocol implementation independent of any
//! specific language adapter. They test:
//! - Message framing (Content-Length headers)
//! - Event subscription and broadcasting
//! - Connection mode serialization
//! - Adapter configuration
//!
//! Run with: cargo test --package detrix-dap --test protocol_tests

use detrix_dap::{
    AdapterConfig, AdapterProcess, AdapterState, ConnectionMode, DapBroker, Event, ProtocolMessage,
    Response,
};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

// ============================================================================
// Connection Mode Tests
// ============================================================================

#[test]
fn test_connection_mode_attach_serialization() {
    let mode = ConnectionMode::Attach {
        host: "127.0.0.1".to_string(),
        port: 5678,
    };

    let json = serde_json::to_string(&mode).unwrap();
    assert!(json.contains("attach"));
    assert!(json.contains("127.0.0.1"));
    assert!(json.contains("5678"));

    let deserialized: ConnectionMode = serde_json::from_str(&json).unwrap();
    match deserialized {
        ConnectionMode::Attach { host, port } => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, 5678);
        }
        _ => panic!("Expected Attach mode"),
    }
}

#[test]
fn test_connection_mode_launch_serialization() {
    let mode = ConnectionMode::Launch;

    let json = serde_json::to_string(&mode).unwrap();
    assert!(json.contains("launch"));

    let deserialized: ConnectionMode = serde_json::from_str(&json).unwrap();
    assert!(matches!(deserialized, ConnectionMode::Launch));
}

// ============================================================================
// Adapter Config Tests
// ============================================================================

#[test]
fn test_adapter_config_attach_mode() {
    let config = AdapterConfig::new("python", "python").attach("192.168.1.100", 9999);

    match &config.connection_mode {
        ConnectionMode::Attach { host, port } => {
            assert_eq!(host, "192.168.1.100");
            assert_eq!(*port, 9999);
        }
        _ => panic!("Expected Attach mode"),
    }
}

#[test]
fn test_adapter_config_with_env() {
    let config = AdapterConfig::new("python", "python")
        .env("PYTHONPATH", "/app/src")
        .env("DEBUG", "1");

    assert_eq!(config.env.len(), 2);
    assert!(config
        .env
        .iter()
        .any(|(k, v)| k == "PYTHONPATH" && v == "/app/src"));
    assert!(config.env.iter().any(|(k, v)| k == "DEBUG" && v == "1"));
}

#[test]
fn test_adapter_config_with_cwd() {
    let config = AdapterConfig::new("python", "python").cwd("/app/project");

    assert_eq!(config.cwd, Some("/app/project".to_string()));
}

// ============================================================================
// Adapter State Machine Tests
// ============================================================================

#[tokio::test]
async fn test_adapter_state_transitions() {
    let config = AdapterConfig::new("test", "echo");
    let adapter = AdapterProcess::new(config);

    // Initial state
    assert_eq!(adapter.state().await, AdapterState::Stopped);

    // After failed start (echo won't work as DAP adapter)
    let _ = adapter.start().await;
    let state = adapter.state().await;
    assert!(state == AdapterState::Failed || state == AdapterState::Stopped);
}

#[tokio::test]
async fn test_adapter_stop_idempotent() {
    let config = AdapterConfig::new("test", "echo");
    let adapter = AdapterProcess::new(config);

    // Stop when already stopped should be ok
    assert!(adapter.stop().await.is_ok());
    assert!(adapter.stop().await.is_ok());
    assert!(adapter.stop().await.is_ok());

    assert_eq!(adapter.state().await, AdapterState::Stopped);
}

// ============================================================================
// DAP Protocol Tests
// ============================================================================

#[tokio::test]
async fn test_dap_message_framing() {
    // Create a mock server
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Spawn server task
    let server_task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();

        // Read the request
        let mut buf_reader = BufReader::new(&mut socket);
        let mut headers = String::new();

        // Read headers until empty line
        loop {
            let mut line = String::new();
            buf_reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line == "\n" {
                break;
            }
            headers.push_str(&line);
        }

        // Parse Content-Length
        let content_length: usize = headers
            .lines()
            .find(|l| l.starts_with("Content-Length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|s| s.trim().parse().ok())
            .unwrap();

        // Read body
        let mut body = vec![0u8; content_length];
        tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut body)
            .await
            .unwrap();

        let request: ProtocolMessage = serde_json::from_slice(&body).unwrap();

        // Verify it's an initialize request
        if let ProtocolMessage::Request(req) = request {
            assert_eq!(req.command, "initialize");

            // Send response
            let response = Response {
                seq: 1,
                request_seq: req.seq,
                command: "initialize".to_string(),
                success: true,
                message: None,
                body: Some(serde_json::json!({"supportsLogPoints": true})),
            };

            let response_msg = ProtocolMessage::Response(response);
            let json = serde_json::to_string(&response_msg).unwrap();
            let framed = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);

            #[allow(unused_mut)]
            let mut socket = buf_reader.into_inner();
            socket.write_all(framed.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
        }
    });

    // Connect client
    let stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .unwrap();
    let (reader, writer) = tokio::io::split(stream);

    let broker = DapBroker::new(reader, writer);

    // Send initialize request
    let response = timeout(
        Duration::from_secs(5),
        broker.send_request("initialize", None),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(response.success);
    assert_eq!(response.command, "initialize");

    server_task.await.unwrap();
}

#[tokio::test]
async fn test_dap_event_subscription() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Spawn server that sends events
    let server_task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();

        // Send multiple events
        for i in 0..3 {
            let event = Event {
                seq: i + 1,
                event: "output".to_string(),
                body: Some(serde_json::json!({"output": format!("message {}", i)})),
            };

            let event_msg = ProtocolMessage::Event(event);
            let json = serde_json::to_string(&event_msg).unwrap();
            let framed = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);

            socket.write_all(framed.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();

            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    // Connect client
    let stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .unwrap();
    let (reader, writer) = tokio::io::split(stream);

    let broker = DapBroker::new(reader, writer);
    let mut events = broker.subscribe_events().await;

    // Receive events
    let mut received = Vec::new();
    for _ in 0..3 {
        if let Ok(Some(event)) = timeout(Duration::from_secs(2), events.recv()).await {
            received.push(event);
        }
    }

    assert_eq!(received.len(), 3);
    assert!(received.iter().all(|e| e.event == "output"));

    server_task.await.unwrap();
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[tokio::test]
async fn test_adapter_invalid_command() {
    // Use Launch mode with a nonexistent command to test error handling
    // (In Attach mode, the command is not used - we connect via TCP)
    let config = AdapterConfig::new("test", "/nonexistent/command");
    // Default is Launch mode, which will try to spawn the nonexistent command

    let adapter = AdapterProcess::new(config);
    let result = adapter.start().await;

    // Should fail because the command doesn't exist
    assert!(result.is_err());
    assert_eq!(adapter.state().await, AdapterState::Failed);
}

#[tokio::test]
async fn test_broker_not_connected() {
    let config = AdapterConfig::new("test", "test");
    let adapter = AdapterProcess::new(config);

    // Try to get broker without connecting
    let result = adapter.broker().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_health_check_not_running() {
    let config = AdapterConfig::new("test", "test");
    let adapter = AdapterProcess::new(config);

    let result = adapter.health_check().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_is_ready_state() {
    let config = AdapterConfig::new("test", "echo");
    let adapter = AdapterProcess::new(config);

    // Not ready when stopped
    assert!(!adapter.is_ready(), "Should not be ready when stopped");
}
