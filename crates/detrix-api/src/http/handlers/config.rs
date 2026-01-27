//! Configuration management handlers
//!
//! Endpoints for reading, updating, reloading, and validating configuration.
//!
//! Uses proto-generated types where possible.

use crate::generated::detrix::v1::{
    ReloadConfigRequest, ReloadConfigResponse, ValidateConfigRequest, ValidationResponse,
};
use crate::http::error::{HttpError, ToHttpResult};
use crate::state::ApiState;
use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

/// Reload configuration from disk and apply to runtime.
///
/// Reloads the configuration file from disk and applies it to the running daemon.
/// Uses the config path from request, or falls back to ConfigService's configured path.
///
/// # Request Body
/// - `configPath`: Optional path to config file (uses daemon's config path if not specified)
///
/// # Response
/// Returns `ReloadConfigResponse` with success status and metrics count.
pub async fn reload_config(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<ReloadConfigRequest>,
) -> Result<Json<ReloadConfigResponse>, HttpError> {
    // Use request path or fall back to ConfigService's path
    let config_path = match req.config_path {
        Some(path) => std::path::PathBuf::from(path),
        None => state
            .config_service
            .config_path()
            .cloned()
            .ok_or_else(|| HttpError::bad_request("No config path configured"))?,
    };

    info!("REST: reload_config (path={})", config_path.display());

    match state.config_service.reload_from_file(&config_path).await {
        Ok(config) => {
            let metrics_count = config.metric.len() as u32;
            info!(
                "Config reloaded and applied successfully with {} metrics",
                metrics_count
            );
            Ok(Json(ReloadConfigResponse {
                success: true,
                metrics_loaded: metrics_count,
                errors: vec![],
                metadata: None,
            }))
        }
        Err(e) => {
            warn!("Config reload failed: {}", e);
            Ok(Json(ReloadConfigResponse {
                success: false,
                metrics_loaded: 0,
                errors: vec![e.to_string()],
                metadata: None,
            }))
        }
    }
}

/// Validate TOML configuration content without applying it.
///
/// Parses and validates the provided TOML content against the config schema.
/// Use this to check configuration before applying.
///
/// # Request Body
/// - `configToml`: TOML configuration content to validate
///
/// # Response
/// Returns `ValidationResponse` with `valid` status and any errors/warnings.
pub async fn validate_config(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<ValidateConfigRequest>,
) -> Result<Json<ValidationResponse>, HttpError> {
    info!(
        "REST: validate_config (content_length={})",
        req.config_toml.len()
    );

    match detrix_config::load_config_from_str(&req.config_toml) {
        Ok(_config) => {
            info!("Config validation successful");
            Ok(Json(ValidationResponse {
                valid: true,
                errors: vec![],
                warnings: vec![],
                metadata: None,
            }))
        }
        Err(e) => {
            warn!("Config validation failed: {}", e);
            Ok(Json(ValidationResponse {
                valid: false,
                errors: vec![e.to_string()],
                warnings: vec![],
                metadata: None,
            }))
        }
    }
}

/// Query parameters for get_config
#[derive(Debug, Deserialize)]
pub struct GetConfigQuery {
    /// Optional path to specific config section
    pub path: Option<String>,
}

/// Response for get_config
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigResponse {
    pub config_toml: String,
    pub version: String,
}

/// Get current configuration as TOML
///
/// Returns the full runtime configuration. Use `path` query parameter to
/// get a specific section (e.g., `?path=api` or `?path=limits`).
pub async fn get_config(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<GetConfigQuery>,
) -> Result<Json<ConfigResponse>, HttpError> {
    info!("REST: get_config (path={:?})", params.path);

    // Get uptime for version info
    let uptime = state.start_time.elapsed().as_secs();
    let config_version = format!("runtime-{}s", uptime);

    // Get config from ConfigService (redacted for API response)
    let config = state.config_service.get_config().await.redacted();

    // Serialize the redacted config to TOML
    let toml_content = toml::to_string_pretty(&config)
        .map_err(|e| HttpError::internal(format!("Failed to serialize config: {}", e)))?;

    // Build response with header comment
    let config_path = state.config_service.config_path();
    let full_config = format!(
        "# Detrix Configuration (runtime snapshot)\n# Generated at uptime: {}s\n# Config path: {:?}\n\n{}",
        uptime, config_path, toml_content
    );

    Ok(Json(ConfigResponse {
        config_toml: full_config,
        version: config_version,
    }))
}

/// Request body for update_config
/// Uses camelCase to match proto JSON conventions
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConfigRequest {
    /// Partial TOML content with settings to update.
    /// Only runtime-safe settings can be modified (e.g., defaults, limits, safety).
    /// Settings that require restart (e.g., api.rest.port, storage.path) will be rejected.
    pub config_toml: String,

    /// Whether to persist changes to the config file on disk (default: true).
    /// Set to false for temporary/test configuration changes.
    #[serde(default = "default_persist")]
    pub persist: bool,
}

fn default_persist() -> bool {
    true
}

/// Response for update_config
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConfigResponse {
    /// Updated configuration as TOML
    pub config_toml: String,
    /// Configuration version identifier
    pub version: String,
    /// Whether changes were persisted to disk
    pub persisted: bool,
    /// Path where config was persisted (if persisted)
    pub config_path: Option<String>,
    /// Warnings about skipped/rejected fields
    pub warnings: Vec<String>,
}

/// Update configuration at runtime with optional persistence
///
/// This endpoint allows partial updates to runtime-safe configuration settings.
/// Changes to fields that require restart (ports, storage path, etc.) are rejected.
///
/// # Request Body
/// - `config_toml`: Partial TOML with settings to update
/// - `persist`: Whether to write changes to config file (default: true)
///
/// # Example
/// ```json
/// {
///   "config_toml": "[limits]\nmax_metrics_total = 200",
///   "persist": true
/// }
/// ```
pub async fn update_config(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<UpdateConfigRequest>,
) -> Result<Json<UpdateConfigResponse>, HttpError> {
    info!("REST: update_config (persist={})", req.persist);

    // Delegate to ConfigService (Clean Architecture - handler does DTO mapping only)
    let result = state
        .config_service
        .update_config(&req.config_toml, req.persist)
        .await
        .http_context("Failed to update config")?;

    // Serialize updated config for response
    let config_toml = toml::to_string_pretty(&result.config)
        .map_err(|e| HttpError::internal(format!("Failed to serialize updated config: {}", e)))?;

    let uptime = state.start_time.elapsed().as_secs();
    let version = format!("runtime-{}s", uptime);

    Ok(Json(UpdateConfigResponse {
        config_toml,
        version,
        persisted: result.persisted,
        config_path: result.config_path.map(|p| p.display().to_string()),
        warnings: result.warnings,
    }))
}
