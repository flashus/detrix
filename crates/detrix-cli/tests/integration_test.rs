//! Integration tests for Detrix CLI
//!
//! Tests the full workflow:
//! 1. Config loading
//! 2. Metric initialization
//! 3. Storage operations
//! 4. Command execution
//! 5. CLI binary execution

use anyhow::Result;
use detrix_application::{EventRepository, MetricRepository};
use detrix_config::Config;
use detrix_core::entities::{MetricMode, SafetyLevel};
use detrix_core::{ConnectionId, SourceLanguage};
use detrix_storage::{SqliteConfig, SqliteStorage};
use detrix_testing::e2e::{find_detrix_binary, get_workspace_root};
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;
use tokio::fs;

/// Helper to create a test config
fn create_test_config(temp_dir: &TempDir) -> String {
    let db_path = temp_dir.path().join("test.db");
    format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[runtime]
default_mode = "active"

[storage]
storage_type = "sqlite"
path = "{}"

[[metric]]
name = "test_metric_1"
connection_id = "test-conn"
location = {{ file = "test.py", line = 10 }}
expression = "f'value={{x}}'"
language = "python"
enabled = true
mode = {{ mode = "stream" }}
safety_level = "strict"

[[metric]]
name = "test_metric_2"
connection_id = "test-conn"
location = {{ file = "test.py", line = 20 }}
expression = "f'count={{count}}'"
language = "python"
enabled = true
mode = {{ mode = "throttle", max_per_second = 5 }}
safety_level = "strict"
"#,
        db_path.to_string_lossy()
    )
}

#[tokio::test]
async fn test_config_loading() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let config_path = temp_dir.path().join("detrix.toml");

    let config_content = create_test_config(&temp_dir);
    fs::write(&config_path, config_content).await?;

    // Load config
    let content = fs::read_to_string(&config_path).await?;
    let config: Config = toml::from_str(&content)?;

    assert_eq!(config.metric.len(), 2);
    assert_eq!(config.metric[0].name, "test_metric_1");
    assert_eq!(config.metric[1].name, "test_metric_2");

    let metric1 = &config.metric[0];
    assert_eq!(metric1.location.file, "test.py");
    assert_eq!(metric1.location.line, 10);
    assert_eq!(metric1.language, "python");
    assert_eq!(metric1.safety_level, SafetyLevel::Strict);
    assert!(matches!(metric1.mode, MetricMode::Stream));

    let metric2 = &config.metric[1];
    assert_eq!(metric2.location.file, "test.py");
    assert_eq!(metric2.location.line, 20);
    assert_eq!(metric2.language, "python");
    assert_eq!(metric2.safety_level, SafetyLevel::Strict);
    assert!(matches!(metric2.mode, MetricMode::Throttle { .. }));

    // Ensure language string parses to enum as expected (as done later in workflow).
    let parsed_language: SourceLanguage = metric1
        .language
        .parse()
        .expect("metric language should parse");
    assert_eq!(parsed_language, SourceLanguage::Python);

    Ok(())
}

#[tokio::test]
async fn test_metric_storage_initialization() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("metrics.db");

    // Initialize storage
    let sqlite_config = SqliteConfig {
        path: db_path,
        pool_size: 5,
        busy_timeout_ms: 3000,
    };

    let storage = SqliteStorage::new(&sqlite_config).await?;

    // Create a test metric
    let metric = detrix_core::Metric {
        id: None,
        name: "test_metric".to_string(),
        connection_id: ConnectionId::from("test"),
        group: Some("test".to_string()),
        location: detrix_core::Location {
            file: "test.py".to_string(),
            line: 42,
        },
        expression: "x + 1".to_string(),
        language: SourceLanguage::Python,
        mode: MetricMode::Stream,
        enabled: true,
        condition: None,
        safety_level: SafetyLevel::Strict,
        created_at: Some(chrono::Utc::now().timestamp_micros()),
        // Default values for introspection fields
        capture_stack_trace: false,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: false,
        snapshot_scope: None,
        snapshot_ttl: None,
        anchor: None,
        anchor_status: detrix_core::AnchorStatus::Unanchored,
    };

    // Save metric
    let metric_id = <SqliteStorage as MetricRepository>::save(&storage, &metric).await?;
    assert!(metric_id.0 > 0);

    // Find metric
    let found_metrics = storage.find_all().await?;
    assert_eq!(found_metrics.len(), 1);
    assert_eq!(found_metrics[0].name, "test_metric");
    assert_eq!(found_metrics[0].location.line, 42);

    Ok(())
}

#[tokio::test]
async fn test_event_storage() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path().join("events.db");

    // Initialize storage
    let sqlite_config = SqliteConfig {
        path: db_path,
        pool_size: 5,
        busy_timeout_ms: 3000,
    };

    let storage = SqliteStorage::new(&sqlite_config).await?;

    // Create and save a metric
    let metric = detrix_core::Metric {
        id: None,
        name: "event_test".to_string(),
        connection_id: ConnectionId::from("test"),
        group: None,
        location: detrix_core::Location {
            file: "test.py".to_string(),
            line: 100,
        },
        expression: "value".to_string(),
        language: SourceLanguage::Python,
        mode: MetricMode::Stream,
        enabled: true,
        condition: None,
        safety_level: SafetyLevel::Strict,
        created_at: Some(chrono::Utc::now().timestamp_micros()),
        // Default values for introspection fields
        capture_stack_trace: false,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: false,
        snapshot_scope: None,
        snapshot_ttl: None,
        anchor: None,
        anchor_status: detrix_core::AnchorStatus::Unanchored,
    };

    let metric_id = <SqliteStorage as MetricRepository>::save(&storage, &metric).await?;

    // Create test events
    let event1 = detrix_core::MetricEvent {
        id: None,
        metric_id,
        metric_name: "test_metric".to_string(),
        connection_id: ConnectionId::from("test"),
        timestamp: chrono::Utc::now().timestamp_micros(),
        thread_id: Some(12345),
        thread_name: Some("MainThread".to_string()),
        value_string: Some("test_value".to_string()),
        value_numeric: None,
        value_boolean: None,
        value_json: "\"test_value\"".to_string(),
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    };

    let event2 = detrix_core::MetricEvent {
        id: None,
        metric_id,
        metric_name: "test_metric".to_string(),
        connection_id: ConnectionId::from("test"),
        timestamp: chrono::Utc::now().timestamp_micros(),
        thread_id: Some(12345),
        thread_name: Some("MainThread".to_string()),
        value_string: None,
        value_numeric: Some(42.0),
        value_boolean: None,
        value_json: "42".to_string(),
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    };

    // Save events using qualified trait syntax
    <SqliteStorage as EventRepository>::save(&storage, &event1).await?;
    <SqliteStorage as EventRepository>::save(&storage, &event2).await?;

    // Query events
    let events = storage.find_by_metric_id(metric_id, 10).await?;
    assert_eq!(events.len(), 2);

    // Verify event values
    assert!(events
        .iter()
        .any(|e| e.value_string == Some("test_value".to_string())));
    assert!(events.iter().any(|e| e.value_numeric == Some(42.0)));

    Ok(())
}

#[tokio::test]
async fn test_full_config_to_storage_workflow() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let config_path = temp_dir.path().join("detrix.toml");

    // Create config file
    let config_content = create_test_config(&temp_dir);
    fs::write(&config_path, config_content).await?;

    // Load config
    let content = fs::read_to_string(&config_path).await?;
    let config: Config = toml::from_str(&content)?;

    // Initialize storage
    let sqlite_config = SqliteConfig {
        path: config.storage.path.clone(),
        pool_size: 30,
        busy_timeout_ms: 5000,
    };

    let storage = SqliteStorage::new(&sqlite_config).await?;

    // Save metrics from config
    let mut saved_metrics = Vec::new();
    for metric_def in &config.metric {
        let metric = detrix_core::Metric {
            id: None,
            name: metric_def.name.clone(),
            connection_id: ConnectionId::from("test"),
            group: metric_def.group.clone(),
            location: metric_def.location.clone(),
            expression: metric_def.expression.clone(),
            language: metric_def
                .language
                .parse()
                .expect("invalid language in config"),
            mode: metric_def.mode.clone(),
            enabled: metric_def.enabled,
            condition: metric_def.condition.clone(),
            safety_level: metric_def.safety_level,
            created_at: Some(chrono::Utc::now().timestamp_micros()),
            // Default values for introspection fields
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: detrix_core::AnchorStatus::Unanchored,
        };

        let saved = <SqliteStorage as MetricRepository>::save(&storage, &metric).await?;
        saved_metrics.push(saved);
    }

    // Verify all metrics are stored
    let all_metrics = storage.find_all().await?;
    assert_eq!(all_metrics.len(), 2);

    // Verify metric details
    let metric1 = all_metrics
        .iter()
        .find(|m| m.name == "test_metric_1")
        .unwrap();
    assert_eq!(metric1.location.file, "test.py");
    assert_eq!(metric1.location.line, 10);
    assert_eq!(metric1.expression, "f'value={x}'");

    let metric2 = all_metrics
        .iter()
        .find(|m| m.name == "test_metric_2")
        .unwrap();
    assert_eq!(metric2.location.line, 20);
    assert!(matches!(metric2.mode, MetricMode::Throttle { .. }));

    Ok(())
}

// ============================================================================
// CLI Binary Tests
// ============================================================================

/// Get path to the compiled detrix binary
/// Searches in this order:
/// 1. DETRIX_BIN env var (explicit path to binary)
/// 2. Relative path from .cargo/config.toml target-dir (../../../../../detrix/target)
/// 3. ~/detrix/target (custom target-dir resolved)
/// 4. workspace/target (default Cargo location)
fn get_binary_path() -> PathBuf {
    // Use shared binary discovery from detrix-testing
    let workspace_root = get_workspace_root();
    find_detrix_binary(&workspace_root).unwrap_or_else(|| {
        // Fallback to workspace target/debug if not found
        workspace_root.join("target/debug/detrix")
    })
}

#[test]
fn test_cli_help() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    let output = Command::new(&binary)
        .arg("--help")
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("detrix"));
    assert!(stdout.contains("serve"));
    assert!(stdout.contains("metric")); // 'list' is now 'metric list'
    assert!(stdout.contains("event")); // 'stream' is now 'event'
}

#[test]
fn test_cli_missing_config_in_readonly_path() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    // Use a path that doesn't exist and can't be created (root-only on Unix)
    // This tests the fallback to embedded config when file creation fails
    #[cfg(unix)]
    let invalid_path = "/nonexistent_root_path_12345/detrix.toml";
    #[cfg(windows)]
    let invalid_path = "Z:\\nonexistent_drive_12345\\detrix.toml";

    let output = Command::new(&binary)
        .arg("--config")
        .arg(invalid_path)
        .args(["daemon", "status"]) // Use daemon status which doesn't need daemon
        .output()
        .expect("Failed to execute command");

    // Strict config policy: command should fail if config cannot be loaded.
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "Command should fail. stderr: {}",
        stderr
    );
    assert!(
        stderr.contains("detrix init") || stderr.contains("Check your config file"),
        "Error should suggest running detrix init (preferred) or show generic config error. stderr: {}",
        stderr
    );
}

#[test]
fn test_cli_invalid_config() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("invalid.toml");

    // Create invalid TOML
    std::fs::write(&config_path, "this is not valid toml {{{{").unwrap();

    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    let output = Command::new(&binary)
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .args(["daemon", "status"]) // Use daemon status which doesn't need daemon
        .output()
        .expect("Failed to execute command");

    // Should fail with non-zero exit code due to invalid TOML
    assert!(!output.status.success());
}

// ============================================================================
// Config Auto-Creation Tests
// ============================================================================

#[test]
fn test_cli_auto_creates_default_config() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    // Create a temp directory for the config
    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("detrix.toml");

    // Verify config doesn't exist yet
    assert!(!config_path.exists(), "Config should not exist before test");

    // Strict config policy: config should be created explicitly via `detrix init`.
    let output = Command::new(&binary)
        .args(["init", "--path"])
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "Command should succeed creating default config via init. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify config file was created
    assert!(
        config_path.exists(),
        "Config file should be auto-created at: {}",
        config_path.display()
    );

    // Read and verify the config is valid TOML
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !config_content.is_empty(),
        "Config file should not be empty"
    );

    // Parse as TOML to verify it's valid
    let parsed: Result<Config, _> = toml::from_str(&config_content);
    assert!(
        parsed.is_ok(),
        "Auto-created config should be valid TOML. Error: {:?}",
        parsed.err()
    );

    // Verify some expected sections exist in the auto-created config
    assert!(
        config_content.contains("[storage]"),
        "Config should have [storage] section"
    );
    assert!(
        config_content.contains("[api.grpc]") || config_content.contains("[api.rest]"),
        "Config should have API sections"
    );
    assert!(
        config_content.contains("[safety]"),
        "Config should have [safety] section"
    );
}

#[test]
fn test_cli_auto_creates_config_in_subdirectory() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    // Create a temp directory with a nested path
    let temp_dir = tempfile::tempdir().unwrap();
    let nested_dir = temp_dir.path().join("subdir").join("config");
    let config_path = nested_dir.join("detrix.toml");

    // Verify nested dir doesn't exist
    assert!(
        !nested_dir.exists(),
        "Nested directory should not exist before test"
    );

    // Strict config policy: config should be created explicitly via `detrix init`.
    let output = Command::new(&binary)
        .args(["init", "--path"])
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "Command should succeed creating default config in subdirectory via init. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify parent directories were created
    assert!(
        nested_dir.exists(),
        "Parent directories should be auto-created"
    );

    // Verify config file was created
    assert!(
        config_path.exists(),
        "Config file should be created in subdirectory"
    );

    // Verify it's valid TOML
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    let parsed: Result<Config, _> = toml::from_str(&config_content);
    assert!(
        parsed.is_ok(),
        "Created config in subdirectory should be valid TOML"
    );
}

#[test]
fn test_cli_does_not_overwrite_existing_config() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");

    // Create a custom config with a unique marker
    let custom_config = format!(
        r#"# Custom config - should not be overwritten
[metadata]
version = "1.0"
description = "UNIQUE_MARKER_12345"

[storage]
path = "{}"
"#,
        db_path.to_string_lossy()
    );
    std::fs::write(&config_path, &custom_config).unwrap();

    // Run detrix with daemon status (works without running daemon)
    let output = Command::new(&binary)
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .args(["daemon", "status"])
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success(), "Command failed: {:?}", output);

    // Verify the original config is preserved
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        config_content.contains("UNIQUE_MARKER_12345"),
        "Existing config should not be overwritten. Content: {}",
        config_content
    );
}

// ============================================================================
// CLI Command Handler Tests (Phase 4.3)
// ============================================================================
// These tests verify CLI command behavior without requiring a running daemon.
// They focus on: argument parsing, local validation, and error handling.

#[test]
fn test_cli_config_validate_local_valid() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("detrix.toml");

    // Create a valid config file
    let valid_config = r#"
[metadata]
version = "1.0"

[storage]
path = "/tmp/test.db"

[api.grpc]
port = 50061

[api.rest]
port = 8090
"#;
    std::fs::write(&config_path, valid_config).unwrap();

    // Validate the config locally (no daemon required)
    let output = Command::new(&binary)
        .args(["config", "validate", "--file"])
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "Config validation should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("valid") || stdout.contains("✓"),
        "Output should indicate config is valid"
    );
}

#[test]
fn test_cli_config_validate_local_invalid_toml() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("invalid.toml");

    // Create an invalid TOML file
    std::fs::write(&config_path, "this is not valid { TOML syntax").unwrap();

    // Validate the config locally
    let output = Command::new(&binary)
        .args(["config", "validate", "--file"])
        .arg(config_path.to_str().unwrap())
        .output()
        .expect("Failed to execute command");

    // Should fail due to parse error
    assert!(
        !output.status.success() || {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.contains("Parse error") || stdout.contains("invalid") || stdout.contains("error")
        },
        "Should report parse error for invalid TOML"
    );
}

#[test]
fn test_cli_config_validate_missing_file() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    // Validate a non-existent config file
    let output = Command::new(&binary)
        .args([
            "config",
            "validate",
            "--file",
            "/nonexistent/path/config.toml",
        ])
        .output()
        .expect("Failed to execute command");

    // Should fail or report file not found
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    assert!(
        !output.status.success()
            || combined.contains("not found")
            || combined.contains("Not found")
            || combined.contains("No such file"),
        "Should report file not found. stdout: {}, stderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn test_cli_metric_add_missing_required_args() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");

    // Create a minimal valid config
    let config = format!(
        r#"
[metadata]
version = "1.0"

[storage]
path = "{}"
"#,
        db_path.to_string_lossy()
    );
    std::fs::write(&config_path, config).unwrap();

    // Try to add a metric without required arguments
    let output = Command::new(&binary)
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .args(["metric", "add", "test_metric"])
        .output()
        .expect("Failed to execute command");

    // Should fail due to missing required arguments
    assert!(
        !output.status.success(),
        "Should fail when required arguments are missing"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--location") || stderr.contains("required") || stderr.contains("missing"),
        "Error should mention missing required argument. stderr: {}",
        stderr
    );
}

#[test]
fn test_cli_connection_create_missing_required_args() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");

    // Create a minimal valid config
    let config = format!(
        r#"
[metadata]
version = "1.0"

[storage]
path = "{}"
"#,
        db_path.to_string_lossy()
    );
    std::fs::write(&config_path, config).unwrap();

    // Try to create a connection without required arguments
    let output = Command::new(&binary)
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .args(["connection", "create"])
        .output()
        .expect("Failed to execute command");

    // Should fail due to missing required arguments
    assert!(
        !output.status.success(),
        "Should fail when required arguments are missing"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--port") || stderr.contains("required") || stderr.contains("missing"),
        "Error should mention missing required argument. stderr: {}",
        stderr
    );
}

#[test]
fn test_cli_daemon_status_without_daemon() {
    let binary = get_binary_path();
    if !binary.exists() {
        eprintln!("Skipping test: binary not built at {:?}", binary);
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");

    // Create a minimal valid config
    let config = format!(
        r#"
[metadata]
version = "1.0"

[storage]
path = "{}"
"#,
        db_path.to_string_lossy()
    );
    std::fs::write(&config_path, config).unwrap();

    // Check daemon status (should work without daemon running)
    let output = Command::new(&binary)
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .args(["daemon", "status"])
        .output()
        .expect("Failed to execute command");

    // daemon status should succeed and report status
    assert!(
        output.status.success(),
        "daemon status should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
