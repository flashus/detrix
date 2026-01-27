//! Tests for PID file management

use super::core::PidFile;
use super::io::{parse_pid_info, PidInfo};
use detrix_config::constants::{DEFAULT_GRPC_PORT, DEFAULT_REST_PORT};
use detrix_config::{PortRegistry, ServiceType};
use std::fs::File;
use std::io::Write;
use std::process;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_acquire_new_pid_file() {
    let temp_dir = std::env::temp_dir();
    let pid_path = temp_dir.join(format!("detrix_test_{}.pid", process::id()));

    // Clean up any existing file
    let _ = std::fs::remove_file(&pid_path);

    // Acquire lock
    let pid_file = PidFile::acquire(&pid_path).expect("Failed to acquire PID file");

    // Verify file exists
    assert!(pid_path.exists());

    // Verify PID is written (no ports yet)
    let contents = std::fs::read_to_string(&pid_path).expect("Failed to read PID file");
    let info = parse_pid_info(contents.trim()).expect("Failed to parse PID info");
    assert_eq!(info.pid, process::id());
    assert!(info.ports.is_empty());

    // Clean up
    drop(pid_file);
    assert!(!pid_path.exists());
}

#[test]
fn test_set_port() {
    let temp_dir = std::env::temp_dir();
    let pid_path = temp_dir.join(format!("detrix_test_port_{}.pid", process::id()));

    // Clean up any existing file
    let _ = std::fs::remove_file(&pid_path);

    // Acquire lock and set port
    let mut pid_file = PidFile::acquire(&pid_path).expect("Failed to acquire PID file");
    pid_file
        .set_port(DEFAULT_REST_PORT)
        .expect("Failed to set port");

    // Verify JSON format with HTTP port
    let contents = std::fs::read_to_string(&pid_path).expect("Failed to read PID file");
    let info = parse_pid_info(contents.trim()).expect("Failed to parse PID info");
    assert_eq!(info.pid, process::id());
    assert_eq!(info.port(), Some(DEFAULT_REST_PORT));
    assert_eq!(info.get_port(ServiceType::Http), Some(DEFAULT_REST_PORT));

    // Verify getter
    assert_eq!(pid_file.port(), Some(DEFAULT_REST_PORT));

    // Clean up
    drop(pid_file);
    let _ = std::fs::remove_file(&pid_path);
}

#[test]
fn test_set_multiple_ports() {
    let temp_dir = std::env::temp_dir();
    let pid_path = temp_dir.join(format!("detrix_test_multiport_{}.pid", process::id()));

    // Clean up any existing file
    let _ = std::fs::remove_file(&pid_path);

    // Acquire lock and set multiple ports
    let mut pid_file = PidFile::acquire(&pid_path).expect("Failed to acquire PID file");
    pid_file
        .set_port(DEFAULT_REST_PORT)
        .expect("Failed to set HTTP port");
    pid_file
        .set_service_port(ServiceType::Grpc, DEFAULT_GRPC_PORT)
        .expect("Failed to set gRPC port");

    // Verify JSON format with multiple ports
    let contents = std::fs::read_to_string(&pid_path).expect("Failed to read PID file");
    let info = parse_pid_info(contents.trim()).expect("Failed to parse PID info");
    assert_eq!(info.pid, process::id());
    assert_eq!(info.get_port(ServiceType::Http), Some(DEFAULT_REST_PORT));
    assert_eq!(info.get_port(ServiceType::Grpc), Some(DEFAULT_GRPC_PORT));

    // Verify getters
    assert_eq!(
        pid_file.get_port(ServiceType::Http),
        Some(DEFAULT_REST_PORT)
    );
    assert_eq!(
        pid_file.get_port(ServiceType::Grpc),
        Some(DEFAULT_GRPC_PORT)
    );

    // Clean up
    drop(pid_file);
    let _ = std::fs::remove_file(&pid_path);
}

#[test]
fn test_read_info_with_port() {
    let temp_dir = std::env::temp_dir();
    let pid_path = temp_dir.join(format!("detrix_test_readinfo_{}.pid", process::id()));

    // Clean up any existing file
    let _ = std::fs::remove_file(&pid_path);

    // Acquire lock and set port
    let mut pid_file = PidFile::acquire(&pid_path).expect("Failed to acquire PID file");
    pid_file.set_port(9090).expect("Failed to set port");

    // Read info from another process perspective
    let info = PidFile::read_info(&pid_path)
        .expect("read_info failed")
        .expect("Expected PidInfo");
    assert_eq!(info.pid, process::id());
    assert_eq!(info.port(), Some(9090));

    // Clean up
    drop(pid_file);
    let _ = std::fs::remove_file(&pid_path);
}

#[test]
fn test_acquire_fails_when_locked() {
    let temp_dir = std::env::temp_dir();
    let pid_path = temp_dir.join(format!("detrix_test_locked_{}.pid", process::id()));

    // Clean up any existing file
    let _ = std::fs::remove_file(&pid_path);

    // Acquire first lock
    let _pid_file1 = PidFile::acquire(&pid_path).expect("Failed to acquire first PID file");

    // Try to acquire second lock - should fail
    let result = PidFile::acquire(&pid_path);
    assert!(result.is_err());

    // Check error message contains "already running"
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("already running"));

    // Clean up
    drop(_pid_file1);
    let _ = std::fs::remove_file(&pid_path);
}

#[test]
fn test_is_running() {
    let temp_dir = std::env::temp_dir();
    let pid_path = temp_dir.join(format!("detrix_test_running_{}.pid", process::id()));

    // Clean up any existing file
    let _ = std::fs::remove_file(&pid_path);

    // Check before acquiring - should be None
    let status = PidFile::is_running(&pid_path).expect("is_running failed");
    assert_eq!(status, None);

    // Acquire lock
    let _pid_file = PidFile::acquire(&pid_path).expect("Failed to acquire PID file");

    // Check while locked - should return current PID
    let status = PidFile::is_running(&pid_path).expect("is_running failed");
    assert_eq!(status, Some(process::id()));

    // Clean up
    drop(_pid_file);

    // Check after release - should be None
    let status = PidFile::is_running(&pid_path).expect("is_running failed");
    assert_eq!(status, None);
}

#[test]
fn test_stale_pid_file_overwrite() {
    let temp_dir = std::env::temp_dir();
    let pid_path = temp_dir.join(format!("detrix_test_stale_{}.pid", process::id()));

    // Clean up any existing file
    let _ = std::fs::remove_file(&pid_path);

    // Create stale PID file (without lock) - JSON format
    {
        let mut file = File::create(&pid_path).expect("Failed to create stale file");
        file.write_all(b"{\"pid\":99999,\"ports\":{}}\n")
            .expect("Failed to write stale PID");
    }

    // Acquire lock - should succeed and overwrite stale PID
    let pid_file = PidFile::acquire(&pid_path).expect("Failed to acquire PID file");

    // Verify PID is overwritten with current PID
    let contents = std::fs::read_to_string(&pid_path).expect("Failed to read PID file");
    let info = parse_pid_info(contents.trim()).expect("Failed to parse PID info");
    assert_eq!(info.pid, process::id());

    // Clean up
    drop(pid_file);
    let _ = std::fs::remove_file(&pid_path);
}

#[test]
fn test_parse_pid_info_json() {
    // PID only (empty ports)
    let info = parse_pid_info(r#"{"pid":12345,"ports":{}}"#).expect("parse failed");
    assert_eq!(info.pid, 12345);
    assert!(info.ports.is_empty());
    assert_eq!(info.port(), None);

    // PID with HTTP port
    let http_json = format!(
        r#"{{"pid":12345,"ports":{{"http":{}}}}}"#,
        DEFAULT_REST_PORT
    );
    let info = parse_pid_info(&http_json).expect("parse failed");
    assert_eq!(info.pid, 12345);
    assert_eq!(info.port(), Some(DEFAULT_REST_PORT));

    // PID with multiple ports
    let both_json = format!(
        r#"{{"pid":12345,"ports":{{"http":{},"grpc":{}}}}}"#,
        DEFAULT_REST_PORT, DEFAULT_GRPC_PORT
    );
    let info = parse_pid_info(&both_json).expect("parse failed");
    assert_eq!(info.pid, 12345);
    assert_eq!(info.get_port(ServiceType::Http), Some(DEFAULT_REST_PORT));
    assert_eq!(info.get_port(ServiceType::Grpc), Some(DEFAULT_GRPC_PORT));

    // Invalid JSON
    assert!(parse_pid_info("invalid").is_err());
    assert!(parse_pid_info("12345").is_err());
    assert!(parse_pid_info("12345:8080").is_err());
}

#[test]
fn test_pid_info_from_registry() {
    let mut registry = PortRegistry::new();
    registry.register(ServiceType::Http, 60001, true);
    registry.register(ServiceType::Grpc, 60002, true);
    registry.allocate(ServiceType::Http).unwrap();
    registry.allocate(ServiceType::Grpc).unwrap();

    let info = PidInfo::from_registry(12345, &registry);
    assert_eq!(info.pid, 12345);
    assert!(info.get_port(ServiceType::Http).is_some());
    assert!(info.get_port(ServiceType::Grpc).is_some());
}

#[test]
fn test_concurrent_acquire() {
    let temp_dir = std::env::temp_dir();
    let pid_path = Arc::new(temp_dir.join(format!("detrix_test_concurrent_{}.pid", process::id())));

    // Clean up any existing file
    let _ = std::fs::remove_file(pid_path.as_ref());

    // Spawn 5 threads trying to acquire lock simultaneously
    let mut handles = vec![];

    for i in 0..5 {
        let path = Arc::clone(&pid_path);
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(i * 10));
            PidFile::acquire(path.as_ref())
        });
        handles.push(handle);
    }

    // Collect results
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // Exactly one should succeed
    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results.iter().filter(|r| r.is_err()).count();

    assert_eq!(successes, 1, "Exactly one thread should acquire lock");
    assert_eq!(failures, 4, "Four threads should fail");

    // Clean up (the successful PidFile will be dropped here)
    let _ = std::fs::remove_file(pid_path.as_ref());
}

#[test]
fn test_creates_parent_directory() {
    let temp_dir = std::env::temp_dir();
    let nested_path = temp_dir.join(format!("detrix_test_nested_{}", process::id()));
    let pid_path = nested_path.join("subdir").join("daemon.pid");

    // Clean up any existing directory
    let _ = std::fs::remove_dir_all(&nested_path);

    // Acquire lock - should create parent directories
    let pid_file = PidFile::acquire(&pid_path).expect("Failed to acquire PID file");

    // Verify file exists
    assert!(pid_path.exists());

    // Clean up
    drop(pid_file);
    let _ = std::fs::remove_dir_all(&nested_path);
}
