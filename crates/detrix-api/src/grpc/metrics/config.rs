//! Config handlers: reload_config, validate_config, get_config, update_config

use crate::generated::detrix::v1::{
    ConfigResponse, GetConfigRequest, ReloadConfigRequest, ReloadConfigResponse,
    UpdateConfigRequest, ValidateConfigRequest, ValidationResponse,
};
use crate::state::ApiState;
use std::sync::Arc;
use tonic::{Request, Response, Status};

/// Handle reload_config request
pub async fn handle_reload_config(
    state: &Arc<ApiState>,
    request: Request<ReloadConfigRequest>,
) -> Result<Response<ReloadConfigResponse>, Status> {
    let req = request.into_inner();

    // Use request path or fall back to ConfigService's configured path
    let config_path = match req.config_path {
        Some(path) => std::path::PathBuf::from(path),
        None => state
            .config_service
            .config_path()
            .cloned()
            .ok_or_else(|| Status::failed_precondition("No config path configured"))?,
    };

    // Reload config and apply to runtime via ConfigService
    match state.config_service.reload_from_file(&config_path).await {
        Ok(config) => {
            let metrics_count = config.metric.len() as u32;
            tracing::info!(
                "Config reloaded and applied from: {} ({} metrics)",
                config_path.display(),
                metrics_count
            );

            Ok(Response::new(ReloadConfigResponse {
                success: true,
                metrics_loaded: metrics_count,
                errors: vec![],
                metadata: None,
            }))
        }
        Err(e) => Ok(Response::new(ReloadConfigResponse {
            success: false,
            metrics_loaded: 0,
            errors: vec![e.to_string()],
            metadata: None,
        })),
    }
}

/// Handle validate_config request
pub async fn handle_validate_config(
    _state: &Arc<ApiState>,
    request: Request<ValidateConfigRequest>,
) -> Result<Response<ValidationResponse>, Status> {
    let req = request.into_inner();

    // Try to parse and validate the TOML content
    match detrix_config::load_config_from_str(&req.config_toml) {
        Ok(_config) => Ok(Response::new(ValidationResponse {
            valid: true,
            errors: vec![],
            warnings: vec![],
            metadata: None,
        })),
        Err(e) => {
            // Return validation errors
            Ok(Response::new(ValidationResponse {
                valid: false,
                errors: vec![e.to_string()],
                warnings: vec![],
                metadata: None,
            }))
        }
    }
}

/// Handle get_config request
pub async fn handle_get_config(
    state: &Arc<ApiState>,
    request: Request<GetConfigRequest>,
) -> Result<Response<ConfigResponse>, Status> {
    let req = request.into_inner();

    // Config version as timestamp (using uptime as a pseudo-version for now)
    let config_version = state.start_time.elapsed().as_secs() as i64;

    // Get config from ConfigService (redacted for API response)
    let config = state.config_service.get_config().await.redacted();

    // If a specific path is requested, extract that part of config
    if let Some(path) = req.path {
        // Return specific config section as TOML
        let toml_content = match path.as_str() {
            "api" => toml::to_string_pretty(&config.api)
                .map_err(|e| Status::internal(format!("Failed to serialize config: {}", e)))?,
            "limits" => toml::to_string_pretty(&config.limits)
                .map_err(|e| Status::internal(format!("Failed to serialize config: {}", e)))?,
            "safety" => toml::to_string_pretty(&config.safety)
                .map_err(|e| Status::internal(format!("Failed to serialize config: {}", e)))?,
            "storage" => toml::to_string_pretty(&config.storage)
                .map_err(|e| Status::internal(format!("Failed to serialize config: {}", e)))?,
            _ => {
                return Err(Status::not_found(format!(
                    "Config path '{}' not found. Available paths: api, limits, safety, storage",
                    path
                )))
            }
        };

        return Ok(Response::new(ConfigResponse {
            config_toml: toml_content,
            version: config_version,
            metadata: None,
        }));
    }

    // Return the full config as TOML (redacted for security)
    let toml_content = toml::to_string_pretty(&config)
        .map_err(|e| Status::internal(format!("Failed to serialize config: {}", e)))?;

    let config_path = state.config_service.config_path();
    Ok(Response::new(ConfigResponse {
        config_toml: format!(
            "# Detrix Configuration (runtime snapshot)\n# Package Version: {}\n# Config path: {:?}\n\n{}",
            env!("CARGO_PKG_VERSION"),
            config_path,
            toml_content
        ),
        version: config_version,
        metadata: None,
    }))
}

/// Handle update_config request
pub async fn handle_update_config(
    _state: &Arc<ApiState>,
    _request: Request<UpdateConfigRequest>,
) -> Result<Response<ConfigResponse>, Status> {
    // Runtime config updates require careful state management
    // For safety, we only support reload from file
    Err(Status::unimplemented(
        "Runtime config updates not supported. Use reload_config to reload from file.",
    ))
}
