//! Runtime configuration management service
//!
//! This service handles runtime configuration updates with validation,
//! persistence, and restart-required field detection.

use crate::{
    error::{ConfigIoResultExt, ConfigTomlResultExt},
    Error,
};
use detrix_config::Config;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Fields that require a server restart to take effect (dotted path format)
pub const RESTART_REQUIRED_FIELDS: &[&str] = &[
    "api.rest.host",
    "api.rest.port",
    "api.grpc.host",
    "api.grpc.port",
    "storage.storage_type",
    "storage.path",
    "storage.pool_size",
    "daemon.pid_file",
    "daemon.log_file",
    "daemon.working_dir",
];

/// Result of a config update operation
#[derive(Debug, Clone)]
pub struct ConfigUpdateResult {
    /// Updated configuration
    pub config: Config,
    /// Whether changes were persisted to disk
    pub persisted: bool,
    /// Path where config was persisted (if persisted)
    pub config_path: Option<PathBuf>,
    /// Warnings about the update
    pub warnings: Vec<String>,
}

/// Protocol-agnostic configuration management service
///
/// Handles:
/// - Runtime configuration updates with partial TOML merging
/// - Validation of restart-required fields
/// - Optional persistence to config file
pub struct ConfigService {
    /// Runtime configuration (mutable via RwLock)
    config: Arc<RwLock<Config>>,
    /// Path to config file for persistence
    config_path: Option<PathBuf>,
}

impl ConfigService {
    /// Create a new ConfigService
    pub fn new(config: Config, config_path: Option<PathBuf>) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            config_path,
        }
    }

    /// Get current configuration (read-only)
    pub async fn get_config(&self) -> Config {
        self.config.read().await.clone()
    }

    /// Get the config path
    pub fn config_path(&self) -> Option<&PathBuf> {
        self.config_path.as_ref()
    }

    /// Get a reference to the RwLock for direct access (for API state)
    pub fn config_ref(&self) -> Arc<RwLock<Config>> {
        Arc::clone(&self.config)
    }

    /// Update configuration with partial TOML
    ///
    /// # Arguments
    /// - `partial_toml`: Partial TOML string with settings to update
    /// - `persist`: Whether to save changes to disk
    ///
    /// # Errors
    /// - Returns error if partial_toml contains restart-required fields
    /// - Returns error if TOML is invalid
    /// - Returns error if merged config is invalid
    pub async fn update_config(
        &self,
        partial_toml: &str,
        persist: bool,
    ) -> Result<ConfigUpdateResult, Error> {
        let mut warnings = Vec::new();

        // Parse the incoming partial TOML
        let partial_config: toml::Value = toml::from_str(partial_toml).toml_parse_partial()?;

        // Check for restart-required fields
        Self::check_restart_required_fields(&partial_config, "")?;

        // Get mutable access to current config
        let mut config = self.config.write().await;

        // Serialize current config to TOML value for merging
        let current_toml_str = toml::to_string(&*config).toml_serialize_current()?;
        let mut current_config: toml::Value =
            toml::from_str(&current_toml_str).toml_parse_current()?;

        // Deep merge the partial config into the current config
        Self::merge_toml_values(&mut current_config, &partial_config);

        // Parse the merged config back into Config struct
        let merged_config: Config = current_config.clone().try_into()?;

        // Persist to file if requested
        let mut persisted = false;
        let mut persisted_path = None;

        if persist {
            if let Some(ref path) = self.config_path {
                let config_str =
                    toml::to_string_pretty(&merged_config).toml_serialize_for_file()?;

                let full_content = format!(
                    "# Detrix Configuration\n# Updated at: {}\n\n{}",
                    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
                    config_str
                );

                tokio::fs::write(path, &full_content)
                    .await
                    .config_write(path)?;

                persisted = true;
                persisted_path = Some(path.clone());
                info!("Config persisted to: {}", path.display());
            } else {
                warnings.push("Config path not set - changes not persisted to disk".to_string());
            }
        }

        // Update the in-memory config
        *config = merged_config.clone();
        drop(config); // Release the write lock

        Ok(ConfigUpdateResult {
            config: merged_config,
            persisted,
            config_path: persisted_path,
            warnings,
        })
    }

    /// Check if the partial config contains any restart-required fields
    fn check_restart_required_fields(value: &toml::Value, prefix: &str) -> Result<(), Error> {
        if let toml::Value::Table(table) = value {
            for (key, val) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", prefix, key)
                };

                // Check if this path is a restart-required field
                if RESTART_REQUIRED_FIELDS.contains(&path.as_str()) {
                    return Err(crate::error::ConfigError::RestartRequired {
                        field: path.clone(),
                    }
                    .into());
                }

                // Recurse into nested tables
                if val.is_table() {
                    Self::check_restart_required_fields(val, &path)?;
                }
            }
        }
        Ok(())
    }

    /// Deep merge two TOML values (source into target)
    fn merge_toml_values(target: &mut toml::Value, source: &toml::Value) {
        if let (toml::Value::Table(target_table), toml::Value::Table(source_table)) =
            (target, source)
        {
            for (key, source_val) in source_table {
                if let Some(target_val) = target_table.get_mut(key) {
                    // Both have the key - recurse if both are tables, otherwise replace
                    if target_val.is_table() && source_val.is_table() {
                        Self::merge_toml_values(target_val, source_val);
                    } else {
                        *target_val = source_val.clone();
                    }
                } else {
                    // Target doesn't have this key - insert it
                    target_table.insert(key.clone(), source_val.clone());
                }
            }
        }
    }

    /// Validate a TOML config string without applying it
    pub fn validate_config(&self, config_toml: &str) -> Result<(Config, Vec<String>), Error> {
        let mut warnings = Vec::new();

        // Parse the TOML
        let config: Config = toml::from_str(config_toml).toml_parse_partial()?;

        // Check for restart-required fields (for informational warnings)
        let partial_config: toml::Value = toml::from_str(config_toml).toml_parse_partial()?;

        if let Err(e) = Self::check_restart_required_fields(&partial_config, "") {
            warnings.push(format!("Warning: {}", e));
        }

        Ok((config, warnings))
    }

    /// Reload configuration from file
    pub async fn reload_from_file(&self, path: &PathBuf) -> Result<Config, Error> {
        let content = tokio::fs::read_to_string(path).await.config_read(path)?;

        let new_config: Config = toml::from_str(&content).toml_parse_current()?;

        let mut config = self.config.write().await;
        *config = new_config.clone();

        info!("Config reloaded from: {}", path.display());
        Ok(new_config)
    }
}

impl std::fmt::Debug for ConfigService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigService")
            .field("config_path", &self.config_path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_update_config_valid_limits() {
        let config = Config::default();
        let service = ConfigService::new(config, None);

        let result = service
            .update_config("[limits]\nmax_metrics_total = 5000", false)
            .await;

        assert!(result.is_ok());
        let updated = service.get_config().await;
        assert_eq!(updated.limits.max_metrics_total, 5000);
    }

    #[tokio::test]
    async fn test_update_config_rejects_restart_required() {
        let config = Config::default();
        let service = ConfigService::new(config, None);

        // api.rest.port is a restart-required field
        let result = service
            .update_config("[api.rest]\nport = 9999", false)
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("restart"));
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    // === Generators ===

    /// Generate TOML with restart-required paths
    fn restart_required_toml() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("[api.rest]\nhost = \"0.0.0.0\"".to_string()),
            Just("[api.rest]\nport = 9999".to_string()),
            Just("[api.grpc]\nhost = \"0.0.0.0\"".to_string()),
            Just("[api.grpc]\nport = 9998".to_string()),
            Just("[storage]\nstorage_type = \"postgres\"".to_string()),
            Just("[storage]\npath = \"/new/path\"".to_string()),
            Just("[storage]\npool_size = 20".to_string()),
            Just("[daemon]\npid_file = \"/tmp/test.pid\"".to_string()),
            Just("[daemon]\nlog_file = \"/tmp/test.log\"".to_string()),
            Just("[daemon]\nworking_dir = \"/tmp\"".to_string()),
        ]
    }

    /// Generate valid non-restart-required TOML configs
    fn valid_non_restart_toml() -> impl Strategy<Value = (String, usize)> {
        (1usize..50000)
            .prop_map(|limit| (format!("[limits]\nmax_metrics_total = {}", limit), limit))
    }

    /// Generate valid limits config TOML
    fn valid_limits_toml() -> impl Strategy<Value = String> {
        (1usize..50000, 1usize..5000, 100usize..50000).prop_map(|(total, per_group, expr_len)| {
            format!(
                "[limits]\nmax_metrics_total = {}\nmax_metrics_per_group = {}\nmax_expression_length = {}",
                total, per_group, expr_len
            )
        })
    }

    // === Properties ===

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Any TOML touching restart-required fields must return error
        #[test]
        fn proptest_restart_required_fields_rejected(toml in restart_required_toml()) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let config = Config::default();
                let service = ConfigService::new(config, None);

                let result = service.update_config(&toml, false).await;
                prop_assert!(result.is_err(), "Should reject restart-required field in: {}", toml);

                let err = result.unwrap_err();
                prop_assert!(
                    err.to_string().contains("restart"),
                    "Error should mention 'restart': {}",
                    err
                );
                Ok(())
            })?;
        }

        /// Valid limits TOML not touching restart fields succeeds
        #[test]
        fn proptest_valid_limits_accepted((toml, expected_limit) in valid_non_restart_toml()) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let config = Config::default();
                let service = ConfigService::new(config, None);

                let result = service.update_config(&toml, false).await;
                prop_assert!(result.is_ok(), "Should accept valid config: {}", toml);

                let updated = service.get_config().await;
                prop_assert_eq!(
                    updated.limits.max_metrics_total,
                    expected_limit,
                    "Limit should be updated"
                );
                Ok(())
            })?;
        }

        /// Merge idempotence: applying same config twice equals applying once
        #[test]
        fn proptest_merge_idempotence(toml in valid_limits_toml()) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let config = Config::default();
                let service = ConfigService::new(config, None);

                // First application
                let result1 = service.update_config(&toml, false).await;
                prop_assert!(result1.is_ok(), "First update should succeed");
                let after_first = service.get_config().await;

                // Second application (should be no-op)
                let result2 = service.update_config(&toml, false).await;
                prop_assert!(result2.is_ok(), "Second update should succeed");
                let after_second = service.get_config().await;

                // Limits should be identical
                prop_assert_eq!(
                    after_first.limits.max_metrics_total,
                    after_second.limits.max_metrics_total,
                    "max_metrics_total should be idempotent"
                );
                prop_assert_eq!(
                    after_first.limits.max_metrics_per_group,
                    after_second.limits.max_metrics_per_group,
                    "max_metrics_per_group should be idempotent"
                );
                prop_assert_eq!(
                    after_first.limits.max_expression_length,
                    after_second.limits.max_expression_length,
                    "max_expression_length should be idempotent"
                );
                Ok(())
            })?;
        }

        /// Invalid TOML syntax returns error, not panic
        #[test]
        fn proptest_invalid_toml_returns_error(bad_toml in "[{\\[=]{1,50}") {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let config = Config::default();
                let service = ConfigService::new(config, None);

                // May or may not be valid TOML - if invalid, should error not panic
                let _ = service.update_config(&bad_toml, false).await;
                // No assertion needed - we just verify no panic
                Ok::<(), proptest::test_runner::TestCaseError>(())
            })?;
        }

        /// Check restart-required fields function never panics
        #[test]
        fn proptest_check_restart_required_never_panics(
            key in "[a-z.]{1,30}",
            value in "[a-zA-Z0-9_/]{0,30}"
        ) {
            // Build a TOML-like value structure manually
            let toml_str = format!("{} = \"{}\"", key, value);
            if let Ok(parsed) = toml::from_str::<toml::Value>(&toml_str) {
                // Should not panic
                let _ = ConfigService::check_restart_required_fields(&parsed, "");
            }
        }

        /// Merge preserves unrelated config sections
        #[test]
        fn proptest_merge_preserves_unrelated_sections(limit in 1usize..50000) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                let mut config = Config::default();
                config.defaults.max_eval_time_ms = 999; // Set a non-default value
                let original_eval_time = config.defaults.max_eval_time_ms;

                let service = ConfigService::new(config, None);

                // Update only limits
                let toml = format!("[limits]\nmax_metrics_total = {}", limit);
                let result = service.update_config(&toml, false).await;
                prop_assert!(result.is_ok());

                let updated = service.get_config().await;

                // Limits should be updated
                prop_assert_eq!(updated.limits.max_metrics_total, limit);

                // Unrelated section should be preserved
                prop_assert_eq!(
                    updated.defaults.max_eval_time_ms,
                    original_eval_time,
                    "Unrelated config should be preserved"
                );
                Ok(())
            })?;
        }
    }
}
