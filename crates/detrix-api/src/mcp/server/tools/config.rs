//! Configuration: get, update, validate, reload
//!
//! These tools manage Detrix runtime configuration.

use crate::mcp::params::{GetConfigParams, UpdateConfigParams, ValidateConfigParams};
use crate::state::ApiState;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

// ============================================================================
// get_config Implementation
// ============================================================================

/// Result of get_config operation
pub struct GetConfigResult {
    pub config_toml: String,
}

impl GetConfigResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> &'static str {
        "Current configuration:"
    }
}

/// Core get_config implementation
pub async fn get_config_impl(
    state: &Arc<ApiState>,
    _params: GetConfigParams,
) -> Result<GetConfigResult, McpError> {
    // Redact sensitive fields before returning
    let config = state.config_service.get_config().await.redacted();

    let config_toml = toml::to_string_pretty(&config).map_err(|e| {
        McpError::internal_error(format!("Failed to serialize config: {}", e), None)
    })?;

    Ok(GetConfigResult { config_toml })
}

// ============================================================================
// update_config Implementation
// ============================================================================

/// Result of update_config operation
pub struct UpdateConfigResult {
    pub persisted: bool,
    pub warnings: Vec<String>,
}

impl UpdateConfigResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!(
            "✅ Configuration updated{}",
            if self.persisted {
                " and saved to file"
            } else {
                " (in-memory only)"
            }
        )
    }

    /// Build warnings message if any
    pub fn build_warnings(&self) -> Option<String> {
        if self.warnings.is_empty() {
            None
        } else {
            Some(format!("⚠️ Warnings: {}", self.warnings.join(", ")))
        }
    }
}

/// Core update_config implementation
pub async fn update_config_impl(
    state: &Arc<ApiState>,
    params: UpdateConfigParams,
) -> Result<UpdateConfigResult, McpError> {
    let persist = params.persist.unwrap_or(true);

    let result = state
        .config_service
        .update_config(&params.config_toml, persist)
        .await
        .map_err(|e| McpError::invalid_params(e.to_string(), None))?;

    Ok(UpdateConfigResult {
        persisted: result.persisted,
        warnings: result.warnings,
    })
}

// ============================================================================
// validate_config Implementation
// ============================================================================

/// Result of validate_config operation
pub enum ValidateConfigResult {
    Valid,
    Invalid(String),
}

impl ValidateConfigResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        match self {
            ValidateConfigResult::Valid => "✅ Configuration is valid".to_string(),
            ValidateConfigResult::Invalid(e) => format!("❌ Configuration invalid: {}", e),
        }
    }
}

/// Core validate_config implementation
pub fn validate_config_impl(
    state: &Arc<ApiState>,
    params: ValidateConfigParams,
) -> ValidateConfigResult {
    match state.config_service.validate_config(&params.config_toml) {
        Ok(_) => ValidateConfigResult::Valid,
        Err(e) => ValidateConfigResult::Invalid(e.to_string()),
    }
}

// ============================================================================
// reload_config Implementation
// ============================================================================

/// Result of reload_config operation
pub struct ReloadConfigResult {
    pub config_path: String,
}

impl ReloadConfigResult {
    /// Build human-readable message for MCP response
    pub fn build_message(&self) -> String {
        format!("✅ Configuration reloaded from {}", self.config_path)
    }
}

/// Core reload_config implementation
pub async fn reload_config_impl(state: &Arc<ApiState>) -> Result<ReloadConfigResult, McpError> {
    let config_path = state.config_service.config_path();

    match config_path {
        Some(path) => {
            state
                .config_service
                .reload_from_file(path)
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to reload config: {}", e), None)
                })?;

            Ok(ReloadConfigResult {
                config_path: path.display().to_string(),
            })
        }
        None => Err(McpError::internal_error(
            "No config file path configured".to_string(),
            None,
        )),
    }
}
