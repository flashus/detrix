//! Configuration file loading and validation
//!
//! Provides functions to load configuration from TOML files:
//!
//! - [`load_config`] - Strict loader, errors if file missing (no side effects)
//! - [`ensure_default_config`] - Creates default config file without loading
//!
//! # Usage
//!
//! ```rust,ignore
//! use detrix_config::{load_config, ensure_default_config};
//! use std::path::Path;
//!
//! // Strict loading (for validation, services)
//! let config = load_config(Path::new("detrix.toml"))?;
//!
//! // Just create the file without loading
//! let path = ensure_default_config(Path::new("~/detrix/detrix.toml"))?;
//! ```

use crate::paths::{default_config_path, detrix_home, DEFAULT_CONFIG_FILENAME};
use crate::Config;
use std::path::{Path, PathBuf};
use tracing::debug;

// Include the generated config constants from build.rs
// This is more robust than include_str!() with relative paths because:
// 1. Cargo automatically tracks the source files for rebuild
// 2. Clear error messages if files are missing at build time
// 3. Works regardless of crate location in workspace
include!(concat!(env!("OUT_DIR"), "/embedded_configs.rs"));

/// Default configuration template (alias for MINIMAL_CONFIG for backwards compatibility)
pub const DEFAULT_CONFIG: &str = MINIMAL_CONFIG;

/// Which config template to use when creating a new config file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfigTemplate {
    /// Minimal config with only essential settings (default for `detrix init`)
    #[default]
    Minimal,
    /// Full config with all settings and extensive comments (`detrix init --full`)
    Full,
}

impl ConfigTemplate {
    /// Get the template content for this variant
    pub fn content(&self) -> &'static str {
        match self {
            ConfigTemplate::Minimal => MINIMAL_CONFIG,
            ConfigTemplate::Full => FULL_CONFIG,
        }
    }
}

/// Errors that can occur during config loading
#[derive(Debug, thiserror::Error)]
#[allow(clippy::enum_variant_names)]
pub enum ConfigError {
    #[error("Config file not found: {0}. Run 'detrix init' to create default config.")]
    NotFound(PathBuf),

    #[error("Failed to read config file: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Failed to parse config: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Validation error: {0}")]
    ValidationError(String),
}

/// Resolve config path to the actual file location.
///
/// Resolution order:
/// 1. If the path exists, use it as-is
/// 2. If the path is relative "detrix.toml" (CLI default), check ~/detrix/detrix.toml
/// 3. If ~/detrix/detrix.toml exists, use it
/// 4. Otherwise, return ~/detrix/detrix.toml as the canonical default path
pub fn resolve_config_path(path: &Path) -> PathBuf {
    // If the exact path exists, use it
    if path.exists() {
        debug!(path = %path.display(), "Config path exists, using as-is");
        return path.to_path_buf();
    }

    // If it's the default relative filename, prefer ~/detrix/detrix.toml
    if path == Path::new(DEFAULT_CONFIG_FILENAME) {
        let home_config = default_config_path();
        if home_config.exists() {
            debug!(path = %home_config.display(), "Found config in detrix home");
            return home_config;
        }
        // Return home path for creation (not CWD)
        debug!(path = %home_config.display(), "Config not found, will use detrix home path");
        return home_config;
    }

    // For other paths, return as-is (will be created there or error)
    debug!(path = %path.display(), "Using provided path as-is");
    path.to_path_buf()
}

/// Load configuration from a TOML file (strict - no side effects)
///
/// This is the preferred loader for services and validation. It:
/// - Does NOT create files if missing (returns `ConfigError::NotFound`)
/// - Does NOT print to stderr
/// - Only reads and parses the config file
///
/// Use [`ensure_default_config`] (via `detrix init`) to create a default config.
///
/// # Arguments
/// * `path` - Path to the TOML configuration file
///
/// # Returns
/// * `Ok(Config)` - Successfully loaded configuration
/// * `Err(ConfigError::NotFound)` - Config file does not exist
/// * `Err(ConfigError)` - Failed to read or parse configuration
///
/// # Example
/// ```rust,ignore
/// use detrix_config::load_config;
/// use std::path::Path;
///
/// let config = load_config(Path::new("detrix.toml"))?;
/// ```
pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    // Resolve the config path (handles default "detrix.toml" -> ~/detrix/detrix.toml)
    let resolved_path = resolve_config_path(path);

    // Strict: error if file doesn't exist
    if !resolved_path.exists() {
        return Err(ConfigError::NotFound(resolved_path));
    }

    debug!(path = %resolved_path.display(), "Loading config file");
    let content = std::fs::read_to_string(&resolved_path)?;
    load_config_from_str(&content)
}

/// Create configuration file at the specified path using the given template
///
/// Creates the config file if it doesn't exist. Does nothing if the file
/// already exists (unless you delete it first). Use this for `detrix init`.
///
/// # Arguments
/// * `path` - Path where config file should be created
/// * `template` - Which template to use (Minimal or Full)
///
/// # Returns
/// * `Ok(PathBuf)` - Path to the config file (existing or newly created)
/// * `Err(std::io::Error)` - Failed to create file or directories
///
/// # Example
/// ```rust,ignore
/// use detrix_config::{create_config, ConfigTemplate};
/// use std::path::Path;
///
/// // Create minimal config (default)
/// let path = create_config(Path::new("~/detrix/detrix.toml"), ConfigTemplate::Minimal)?;
///
/// // Create full config with all options
/// let path = create_config(Path::new("~/detrix/detrix.toml"), ConfigTemplate::Full)?;
/// ```
pub fn create_config(path: &Path, template: ConfigTemplate) -> Result<PathBuf, std::io::Error> {
    // If file already exists, just return the path
    if path.exists() {
        debug!(path = %path.display(), "Config file already exists");
        return Ok(path.to_path_buf());
    }

    // Ensure detrix home directory exists
    let home = detrix_home();
    if !home.exists() {
        debug!(path = %home.display(), "Creating detrix home directory");
        std::fs::create_dir_all(&home)?;
    }

    // Ensure parent directory exists
    crate::paths::ensure_parent_dir(path)?;

    // Write config using the selected template
    debug!(path = %path.display(), template = ?template, "Writing config file");
    std::fs::write(path, template.content())?;

    Ok(path.to_path_buf())
}

/// Create default (minimal) configuration file at the specified path
///
/// This is a convenience wrapper around `create_config` that uses the
/// minimal template. For full config, use `create_config` with `ConfigTemplate::Full`.
///
/// # Arguments
/// * `path` - Path where config file should be created
///
/// # Returns
/// * `Ok(PathBuf)` - Path to the config file (existing or newly created)
/// * `Err(std::io::Error)` - Failed to create file or directories
pub fn ensure_default_config(path: &Path) -> Result<PathBuf, std::io::Error> {
    create_config(path, ConfigTemplate::Minimal)
}

/// Load configuration from a TOML string
///
/// # Arguments
/// * `content` - TOML content as a string
///
/// # Returns
/// * `Ok(Config)` - Successfully parsed configuration
/// * `Err(ConfigError)` - Failed to parse configuration
pub fn load_config_from_str(content: &str) -> Result<Config, ConfigError> {
    let config: Config = toml::from_str(content)?;
    validate_config(&config)?;
    Ok(config)
}

/// Validate configuration values
fn validate_config(config: &Config) -> Result<(), ConfigError> {
    let mut all_errors = Vec::new();

    // Validate storage config
    if config.storage.pool_size == 0 {
        all_errors.push("storage.pool_size must be greater than 0".to_string());
    }

    // Validate limits
    if config.limits.max_expression_length == 0 {
        all_errors.push("limits.max_expression_length must be greater than 0".to_string());
    }

    // Validate API config
    if config.api.event_buffer_capacity == 0 {
        all_errors.push("api.event_buffer_capacity must be greater than 0".to_string());
    }

    // Validate MCP bridge config internal constraints
    all_errors.extend(config.mcp.validate());

    // Validate cross-config constraints: MCP bridge timeout vs adapter connection timeout
    all_errors.extend(
        config
            .mcp
            .validate_against_connection_timeout(config.adapter.connection_timeout_ms),
    );

    // Validate rate limiting config
    if let Err(e) = config.api.rest.rate_limit.validate() {
        all_errors.push(e);
    }

    // Validate streaming config (WebSocket timeout constraints)
    if let Err(e) = config.api.streaming.validate() {
        all_errors.push(e);
    }

    // Validate authentication config (security: ensure credentials when auth enabled)
    if let Err(e) = config.api.auth.validate() {
        all_errors.push(e);
    }

    // Validate CORS config (security: credentials + wildcard origins not allowed)
    if let Err(e) = config.api.rest.cors.validate() {
        all_errors.push(e);
    }

    if all_errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::ValidationError(all_errors.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // === Strict load_config tests ===

    #[test]
    fn test_load_config_strict_fails_on_missing_file() {
        let result = load_config(Path::new("/nonexistent/path/config.toml"));
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(matches!(err, ConfigError::NotFound(_)));
        assert!(err.to_string().contains("Config file not found"));
        assert!(err.to_string().contains("detrix init"));
    }

    #[test]
    fn test_load_config_strict_loads_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("test_config.toml");

        std::fs::write(
            &config_path,
            r#"
[metadata]
version = "1.0"

[api.rest]
port = 9999
"#,
        )
        .unwrap();

        let config = load_config(&config_path).unwrap();
        assert_eq!(config.api.rest.port, 9999);
    }

    // === ensure_default_config tests ===

    #[test]
    fn test_ensure_default_config_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("new_config.toml");

        assert!(!config_path.exists());

        let result = ensure_default_config(&config_path).unwrap();
        assert_eq!(result, config_path);
        assert!(config_path.exists());

        // Verify it's valid TOML that can be parsed
        let content = std::fs::read_to_string(&config_path).unwrap();
        let _config: Config = toml::from_str(&content).unwrap();
    }

    #[test]
    fn test_ensure_default_config_noop_when_exists() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("existing_config.toml");

        // Create a custom config first
        let custom_content = r#"
[metadata]
version = "custom"
"#;
        std::fs::write(&config_path, custom_content).unwrap();

        // ensure_default_config should NOT overwrite
        let result = ensure_default_config(&config_path).unwrap();
        assert_eq!(result, config_path);

        // Content should be unchanged
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("custom"));
    }

    // === Original tests ===

    #[test]
    fn test_load_minimal_config() {
        let toml = r#"
[metadata]
version = "1.0"
"#;
        let config = load_config_from_str(toml).unwrap();
        assert_eq!(config.metadata.version, "1.0");
        // Defaults should be applied
        assert_eq!(config.storage.storage_type, "sqlite");
    }

    #[test]
    fn test_load_full_config() {
        let toml = r#"
[metadata]
version = "1.0"
description = "Test config"

[storage]
storage_type = "sqlite"
path = "./test.db"
pool_size = 5

[api.grpc]
enabled = true
port = 50052
"#;
        let config = load_config_from_str(toml).unwrap();
        assert_eq!(config.storage.path, std::path::PathBuf::from("./test.db"));
        assert_eq!(config.storage.pool_size, 5);
        assert_eq!(config.api.grpc.port, 50052);
    }

    #[test]
    fn test_validation_error_pool_size() {
        let toml = r#"
[storage]
pool_size = 0
"#;
        let result = load_config_from_str(toml);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("pool_size must be greater than 0"));
    }

    #[test]
    fn test_load_with_metrics() {
        let toml = r#"
[[metric]]
name = "test_metric"
connection_id = "test-conn"
expression = "x + 1"
language = "python"
location = { file = "test.py", line = 10 }
"#;
        let config = load_config_from_str(toml).unwrap();
        assert_eq!(config.metric.len(), 1);
        assert_eq!(config.metric[0].name, "test_metric");
        assert_eq!(config.metric[0].connection_id, "test-conn");
        assert_eq!(config.metric[0].location.file, "test.py");
        assert_eq!(config.metric[0].location.line, 10);
    }

    // === Embedded config template tests ===

    #[test]
    fn test_embedded_full_config_parses() {
        // FULL_CONFIG is embedded from detrix.toml at compile time
        let result = load_config_from_str(FULL_CONFIG);
        assert!(
            result.is_ok(),
            "FULL_CONFIG (detrix.toml) failed to parse: {:?}",
            result.err()
        );

        let config = result.unwrap();
        // Verify key fields from full config
        assert_eq!(config.storage.pool_size, 5);
        assert_eq!(config.storage.busy_timeout_ms, 5000);
        assert!(config.safety.enable_ast_analysis);
    }

    #[test]
    fn test_embedded_minimal_config_parses() {
        // MINIMAL_CONFIG is embedded from detrix.minimal.toml at compile time
        let result = load_config_from_str(MINIMAL_CONFIG);
        assert!(
            result.is_ok(),
            "MINIMAL_CONFIG (detrix.minimal.toml) failed to parse: {:?}",
            result.err()
        );

        let config = result.unwrap();
        // Verify it has the required safety section
        assert!(config.safety.enable_ast_analysis);
    }

    #[test]
    fn test_config_template_content() {
        // Verify ConfigTemplate returns correct content
        assert_eq!(ConfigTemplate::Minimal.content(), MINIMAL_CONFIG);
        assert_eq!(ConfigTemplate::Full.content(), FULL_CONFIG);
    }
}
