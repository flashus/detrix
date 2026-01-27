//! Tests for Adapter Process Management

use super::config::AdapterConfig;
use super::process::{AdapterProcess, AdapterState};

#[test]
fn test_adapter_config_builder() {
    let config = AdapterConfig::new("python", "python")
        .arg("-m")
        .arg("debugpy")
        .arg("--listen")
        .arg("127.0.0.1:{port}")
        .port(5678)
        .cwd("/app")
        .env("PYTHONPATH", "/app/src");

    assert_eq!(config.adapter_id, "python");
    assert_eq!(config.command, "python");
    assert_eq!(config.args.len(), 4);
    assert_eq!(config.port, Some(5678));
    assert_eq!(config.cwd, Some("/app".to_string()));
    assert_eq!(config.env.len(), 1);
}

#[test]
fn test_adapter_config_port_substitution() {
    let config = AdapterConfig::new("python", "python")
        .arg("--listen")
        .arg("127.0.0.1:{port}")
        .arg("--port")
        .arg("{port}")
        .port(5678);

    let args = config.substitute_port();
    assert_eq!(args, vec!["--listen", "127.0.0.1:5678", "--port", "5678"]);
}

#[test]
fn test_adapter_config_no_port() {
    let config = AdapterConfig::new("python", "python")
        .arg("--listen")
        .arg("127.0.0.1:5678");

    let args = config.substitute_port();
    assert_eq!(args, vec!["--listen", "127.0.0.1:5678"]);
}

#[tokio::test]
async fn test_adapter_initial_state() {
    let config = AdapterConfig::new("python", "python");
    let adapter = AdapterProcess::new(config);

    assert_eq!(adapter.state().await, AdapterState::Stopped);
    assert!(!adapter.is_running().await);
    assert!(adapter.capabilities().await.is_none());
}

#[tokio::test]
async fn test_adapter_start_invalid_command() {
    let config = AdapterConfig::new("python", "nonexistent_command_12345");
    let adapter = AdapterProcess::new(config);

    let result = adapter.start().await;
    assert!(result.is_err());

    // State should be Failed (reset from Starting after connection error)
    // This allows retry attempts to work correctly
    let state = adapter.state().await;
    assert_eq!(state, AdapterState::Failed);
}

#[tokio::test]
async fn test_adapter_stop_when_stopped() {
    let config = AdapterConfig::new("python", "python");
    let adapter = AdapterProcess::new(config);

    // Should be ok to stop when already stopped
    let result = adapter.stop().await;
    assert!(result.is_ok());
    assert_eq!(adapter.state().await, AdapterState::Stopped);
}

#[tokio::test]
async fn test_adapter_health_check_when_stopped() {
    let config = AdapterConfig::new("python", "python");
    let adapter = AdapterProcess::new(config);

    let result = adapter.health_check().await;
    assert!(result.is_err());

    if let Err(crate::Error::Communication(msg)) = result {
        assert!(msg.contains("not running"));
    } else {
        panic!("Expected Communication error");
    }
}

#[tokio::test]
async fn test_adapter_broker_when_not_connected() {
    let config = AdapterConfig::new("python", "python");
    let adapter = AdapterProcess::new(config);

    let result = adapter.broker().await;
    assert!(result.is_err());

    if let Err(crate::Error::Communication(msg)) = result {
        assert!(msg.contains("not connected"));
    } else {
        panic!("Expected Communication error");
    }
}
