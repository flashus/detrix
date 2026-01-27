//! Tests for ConfigService
//!
//! Following TDD approach - these tests define the expected behavior.

use super::config_service::{ConfigService, RESTART_REQUIRED_FIELDS};
use detrix_config::Config;
use tempfile::TempDir;

#[tokio::test]
async fn test_get_config_returns_current_config() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config.clone(), None);

    // Act
    let result = service.get_config().await;

    // Assert
    assert_eq!(result.metadata.version, config.metadata.version);
}

#[tokio::test]
async fn test_update_config_with_valid_partial_toml() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None);

    let partial_toml = r#"
[limits]
max_metrics_total = 500
max_metrics_per_group = 100
"#;

    // Act
    let result = service.update_config(partial_toml, false).await;

    // Assert
    assert!(result.is_ok());
    let update_result = result.unwrap();
    assert_eq!(update_result.config.limits.max_metrics_total, 500);
    assert_eq!(update_result.config.limits.max_metrics_per_group, 100);
    assert!(!update_result.persisted);
}

#[tokio::test]
async fn test_update_config_rejects_restart_required_field() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None);

    let partial_toml = r#"
[api.rest]
port = 9999
"#;

    // Act
    let result = service.update_config(partial_toml, false).await;

    // Assert
    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(err_msg.contains("api.rest.port"));
    assert!(err_msg.contains("restart"));
}

#[tokio::test]
async fn test_update_config_rejects_storage_path() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None);

    let partial_toml = r#"
[storage]
path = "/new/path/to/db.sqlite"
"#;

    // Act
    let result = service.update_config(partial_toml, false).await;

    // Assert
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("storage.path"));
}

#[tokio::test]
async fn test_update_config_with_invalid_toml() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None);

    let invalid_toml = "this is not valid toml { }";

    // Act
    let result = service.update_config(invalid_toml, false).await;

    // Assert
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid TOML"));
}

#[tokio::test]
async fn test_update_config_persists_to_file() {
    // Arrange
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("detrix.toml");

    // Create initial config file
    let initial_content = r#"
[metadata]
version = "1.0"

[limits]
max_metrics_total = 100
"#;
    std::fs::write(&config_path, initial_content).unwrap();

    let config = Config::default();
    let service = ConfigService::new(config, Some(config_path.clone()));

    let partial_toml = r#"
[limits]
max_metrics_total = 200
"#;

    // Act
    let result = service.update_config(partial_toml, true).await;

    // Assert
    assert!(result.is_ok());
    let update_result = result.unwrap();
    assert!(update_result.persisted);
    assert_eq!(update_result.config_path, Some(config_path.clone()));

    // Verify file was updated
    let file_content = std::fs::read_to_string(&config_path).unwrap();
    assert!(file_content.contains("max_metrics_total = 200"));
}

#[tokio::test]
async fn test_update_config_no_persist_does_not_write_file() {
    // Arrange
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("detrix.toml");

    let initial_content = r#"
[limits]
max_metrics_total = 100
"#;
    std::fs::write(&config_path, initial_content).unwrap();

    let config = Config::default();
    let service = ConfigService::new(config, Some(config_path.clone()));

    let partial_toml = r#"
[limits]
max_metrics_total = 999
"#;

    // Act
    let result = service.update_config(partial_toml, false).await;

    // Assert
    assert!(result.is_ok());
    let update_result = result.unwrap();
    assert!(!update_result.persisted);

    // File should still have original value
    let file_content = std::fs::read_to_string(&config_path).unwrap();
    assert!(file_content.contains("max_metrics_total = 100"));
}

#[tokio::test]
async fn test_update_config_deep_merge() {
    // Arrange
    let config = Config::default();
    let original_host = config.api.rest.host.clone();
    let service = ConfigService::new(config, None);

    // Only update default_query_limit, not other api fields
    let partial_toml = r#"
[api]
default_query_limit = 50
"#;

    // Act
    let result = service.update_config(partial_toml, false).await;

    // Assert
    assert!(result.is_ok());
    let update_result = result.unwrap();
    assert_eq!(update_result.config.api.default_query_limit, 50);
    // Other fields should be preserved
    assert_eq!(update_result.config.api.rest.host, original_host);
}

#[tokio::test]
async fn test_update_config_allows_safety_config_changes() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None);

    let partial_toml = r#"
[safety]
max_recursion_depth = 20
max_memory_mb = 256
"#;

    // Act
    let result = service.update_config(partial_toml, false).await;

    // Assert
    assert!(result.is_ok());
    let update_result = result.unwrap();
    assert_eq!(update_result.config.safety.max_recursion_depth, 20);
    assert_eq!(update_result.config.safety.max_memory_mb, 256);
}

#[tokio::test]
async fn test_update_config_allows_defaults_changes() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None);

    let partial_toml = r#"
[defaults]
enabled = false
max_eval_time_ms = 500
"#;

    // Act
    let result = service.update_config(partial_toml, false).await;

    // Assert
    assert!(result.is_ok());
    let update_result = result.unwrap();
    assert!(!update_result.config.defaults.enabled);
    assert_eq!(update_result.config.defaults.max_eval_time_ms, 500);
}

#[tokio::test]
async fn test_update_config_allows_runtime_changes() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None);

    let partial_toml = r#"
[runtime]
auto_sleep_after_seconds = 3600
"#;

    // Act
    let result = service.update_config(partial_toml, false).await;

    // Assert
    assert!(result.is_ok());
    let update_result = result.unwrap();
    assert_eq!(update_result.config.runtime.auto_sleep_after_seconds, 3600);
}

#[tokio::test]
async fn test_update_config_warns_when_no_path_set() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None); // No path

    let partial_toml = r#"
[limits]
max_metrics_total = 200
"#;

    // Act - try to persist without a path
    let result = service.update_config(partial_toml, true).await;

    // Assert
    assert!(result.is_ok());
    let update_result = result.unwrap();
    assert!(!update_result.persisted);
    assert!(update_result
        .warnings
        .iter()
        .any(|w| w.contains("not persisted")));
}

#[tokio::test]
async fn test_validate_config_valid() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None);

    let config_toml = r#"
[metadata]
version = "1.0"

[limits]
max_metrics_total = 100
"#;

    // Act
    let result = service.validate_config(config_toml);

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_validate_config_invalid() {
    // Arrange
    let config = Config::default();
    let service = ConfigService::new(config, None);

    let config_toml = "invalid toml {{{";

    // Act
    let result = service.validate_config(config_toml);

    // Assert
    assert!(result.is_err());
}

#[tokio::test]
async fn test_reload_from_file() {
    // Arrange
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("detrix.toml");

    let file_content = r#"
[metadata]
version = "2.0"

[limits]
max_metrics_total = 999
"#;
    std::fs::write(&config_path, file_content).unwrap();

    let config = Config::default();
    let service = ConfigService::new(config, Some(config_path.clone()));

    // Act
    let result = service.reload_from_file(&config_path).await;

    // Assert
    assert!(result.is_ok());
    let new_config = result.unwrap();
    assert_eq!(new_config.limits.max_metrics_total, 999);

    // Verify in-memory config was updated
    let current = service.get_config().await;
    assert_eq!(current.limits.max_metrics_total, 999);
}

#[test]
fn test_restart_required_fields_contains_expected() {
    // Verify all the documented restart-required fields are in the list
    assert!(RESTART_REQUIRED_FIELDS.contains(&"api.rest.host"));
    assert!(RESTART_REQUIRED_FIELDS.contains(&"api.rest.port"));
    assert!(RESTART_REQUIRED_FIELDS.contains(&"api.grpc.host"));
    assert!(RESTART_REQUIRED_FIELDS.contains(&"api.grpc.port"));
    assert!(RESTART_REQUIRED_FIELDS.contains(&"storage.path"));
    assert!(RESTART_REQUIRED_FIELDS.contains(&"storage.storage_type"));
    assert!(RESTART_REQUIRED_FIELDS.contains(&"storage.pool_size"));
    assert!(RESTART_REQUIRED_FIELDS.contains(&"daemon.pid_file"));
}
