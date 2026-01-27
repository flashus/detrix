//! Client Context - shared state for all CLI and TUI commands
//!
//! Created once at startup, passed to all commands.
//! Contains loaded config and discovered daemon endpoints.

use detrix_api::grpc::DaemonEndpoints;
use detrix_config::{load_config, paths::discover_config_path, Config};
use std::path::PathBuf;
use std::sync::Arc;

/// Shared context for CLI/TUI commands
///
/// Created once at startup using `ClientContext::new()`.
/// Contains the loaded configuration and discovered daemon endpoints.
///
/// Config is stored in `Arc<Config>` to enable cheap cloning across commands
/// while maintaining a single loaded configuration.
#[derive(Debug, Clone)]
pub struct ClientContext {
    /// Loaded configuration (shared via Arc for cheap cloning)
    pub config: Arc<Config>,
    /// Path where config was loaded from
    pub config_path: PathBuf,
    /// Source of config path (CLI arg, env var, or default)
    pub config_source: &'static str,
    /// Discovered daemon endpoints for connecting to running daemon
    pub endpoints: DaemonEndpoints,
}

impl ClientContext {
    /// Create client context by discovering config and endpoints
    ///
    /// Config discovery order:
    /// 1. `--config <path>` CLI argument
    /// 2. `DETRIX_CONFIG` environment variable
    /// 3. Default location (`~/detrix/detrix.toml`)
    ///
    /// Endpoint discovery order:
    /// 1. PID file (actual running daemon)
    /// 2. Environment variables
    /// 3. Config file values
    /// 4. Hardcoded defaults
    pub fn new() -> Result<Self, ClientContextError> {
        // Discover and load config
        let (config_path, config_source) = discover_config_path();

        let config = load_config(&config_path).map_err(|e| ClientContextError::ConfigLoad {
            path: config_path.clone(),
            reason: e.to_string(),
        })?;

        // Discover endpoints using loaded config (including PID file path)
        let endpoints = DaemonEndpoints::discover_with_config(
            None,
            Some(&config.api),
            Some(&config.daemon.pid_file),
        );

        Ok(Self {
            config: Arc::new(config),
            config_path,
            config_source,
            endpoints,
        })
    }

    /// Create client context with explicit config path override
    ///
    /// Used when `--config` is passed explicitly to a command.
    pub fn with_config_path(config_path: PathBuf) -> Result<Self, ClientContextError> {
        let config = load_config(&config_path).map_err(|e| ClientContextError::ConfigLoad {
            path: config_path.clone(),
            reason: e.to_string(),
        })?;

        let endpoints = DaemonEndpoints::discover_with_config(
            None,
            Some(&config.api),
            Some(&config.daemon.pid_file),
        );

        Ok(Self {
            config: Arc::new(config),
            config_path,
            config_source: "explicit path",
            endpoints,
        })
    }
}

/// Errors that can occur when creating client context
#[derive(Debug, thiserror::Error)]
pub enum ClientContextError {
    #[error("Failed to load config from {path}: {reason}\n\nRun 'detrix init' to create default configuration.")]
    ConfigLoad { path: PathBuf, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Test that ClientContext::with_config_path loads config correctly
    #[test]
    fn test_client_context_with_config_path() {
        // Create a minimal config file using TempDir + fs::write (more reliable than NamedTempFile)
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = temp_dir.path().join("test_config.toml");

        std::fs::write(
            &config_path,
            r#"
[metadata]
version = "1.0"

[api.rest]
host = "127.0.0.1"
port = 8888

[api.grpc]
host = "127.0.0.1"
port = 50099
"#,
        )
        .expect("Failed to write config");

        // Create context with explicit path
        let ctx =
            ClientContext::with_config_path(config_path.clone()).expect("Failed to create context");

        // Verify config was loaded from file
        assert_eq!(ctx.config.api.rest.port, 8888);
        assert_eq!(ctx.config.api.grpc.port, 50099);
        assert_eq!(ctx.config_path, config_path);
        assert_eq!(ctx.config_source, "explicit path");

        // Verify endpoints are discovered
        // Note: If a daemon is running, endpoints come from PID file (takes priority over config)
        // Only check config fallback when no daemon is running
        if !ctx.endpoints.from_pid_file {
            assert_eq!(ctx.endpoints.host, "127.0.0.1");
            assert_eq!(ctx.endpoints.http_port, 8888);
            assert_eq!(ctx.endpoints.grpc_port, 50099);
        }
    }

    /// Test that ClientContext::with_config_path handles non-existent paths gracefully
    ///
    /// Note: strict load_config does not auto-create files.
    #[test]
    fn test_client_context_nonexistent_path_errors() {
        // This path doesn't exist and can't be created
        let result =
            ClientContext::with_config_path(PathBuf::from("/nonexistent/path/to/config.toml"));

        assert!(result.is_err());
    }

    /// Test that ClientContext clones correctly (needed for passing to async commands)
    #[test]
    fn test_client_context_clone() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config_path = temp_dir.path().join("test_config.toml");

        std::fs::write(
            &config_path,
            r#"
[metadata]
version = "1.0"
"#,
        )
        .expect("Failed to write config");

        let ctx = ClientContext::with_config_path(config_path).expect("Failed to create context");

        // Clone and verify both have same values
        let ctx_clone = ctx.clone();
        assert_eq!(ctx.config.api.rest.port, ctx_clone.config.api.rest.port);
        assert_eq!(ctx.config_path, ctx_clone.config_path);
        assert_eq!(ctx.config_source, ctx_clone.config_source);
    }
}
