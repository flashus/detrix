//! Shared authentication logic for HTTP and gRPC layers.
//!
//! Contains the core token validation that is duplicated between the HTTP
//! middleware and the gRPC interceptor.

use crate::common::AuthenticatedUser;
use detrix_application::{JwksValidator, JwtClaims};
use detrix_config::{AuthConfig, AuthMode, JwtConfig, UserRole};
use std::sync::Arc;
use tracing::{debug, error, warn};

/// Shared authentication state used by both HTTP middleware and gRPC interceptor.
#[derive(Clone)]
pub struct AuthState {
    pub config: Arc<AuthConfig>,
    /// JWT validator for external mode (optional — only needed for external auth).
    pub jwt_validator: Option<Arc<JwksValidator>>,
}

impl AuthState {
    /// Create auth state from config (simple mode or disabled).
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config: Arc::new(config),
            jwt_validator: None,
        }
    }

    /// Create auth state with JWT validator for external mode.
    pub fn with_jwt_validator(config: AuthConfig, validator: JwksValidator) -> Self {
        Self {
            config: Arc::new(config),
            jwt_validator: Some(Arc::new(validator)),
        }
    }
}

/// Error type for authentication failures.
#[derive(Debug)]
pub enum AuthError {
    /// Missing or malformed credentials (HTTP 401 / gRPC UNAUTHENTICATED).
    Unauthenticated(String),
    /// Server-side misconfiguration (HTTP 500 / gRPC INTERNAL).
    Internal(String),
}

/// Authenticate a bearer token against the given auth state.
///
/// This is the **single source of truth** for token validation logic.
/// Both the HTTP middleware and the gRPC interceptor delegate here.
///
/// Returns `AuthenticatedUser` on success, or `AuthError` describing the failure.
pub async fn authenticate_token(
    token: &str,
    state: &AuthState,
) -> Result<AuthenticatedUser, AuthError> {
    let config = &state.config;

    match config.effective_mode() {
        AuthMode::Simple => {
            if let Some(user) = config.find_user_by_token(token) {
                debug!(user_id = %user.user_id, "Bearer token authentication successful");
                Ok(AuthenticatedUser {
                    user_id: user.user_id.clone(),
                    role: user.role.clone(),
                })
            } else {
                warn!("Invalid Bearer token");
                Err(AuthError::Unauthenticated("Invalid token".into()))
            }
        }
        AuthMode::External => {
            let validator = state.jwt_validator.as_ref().ok_or_else(|| {
                error!("JWT validator not configured for external mode");
                AuthError::Internal("Authentication not configured".into())
            })?;

            match validator.validate_token(token).await {
                Ok(claims) => {
                    let user_id = claims.sub.clone().ok_or_else(|| {
                        warn!("JWT missing 'sub' claim — rejecting");
                        AuthError::Unauthenticated("JWT missing 'sub' claim".into())
                    })?;

                    debug!(sub = %user_id, email = ?claims.email, "JWT authentication successful");

                    let role = resolve_jwt_role(&claims, &config.jwt);

                    Ok(AuthenticatedUser { user_id, role })
                }
                Err(e) => {
                    warn!(error = %e, "JWT validation failed");
                    Err(AuthError::Unauthenticated("JWT validation failed".into()))
                }
            }
        }
        AuthMode::Disabled => {
            // Should not reach here — caller should check mode first.
            Ok(AuthenticatedUser::default_admin())
        }
    }
}

/// Traverse a dot-separated path in a JSON value (e.g. `"realm_access.roles"`).
fn get_claim_value<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.').try_fold(root, |node, key| node.get(key))
}

/// Determine user role from JWT claims based on config.
///
/// Supports dot-path notation for nested claims, e.g. `admin_role_claim = "realm_access.roles"`.
pub fn resolve_jwt_role(claims: &JwtClaims, jwt_config: &JwtConfig) -> UserRole {
    let (Some(ref claim_name), Some(ref claim_value)) =
        (&jwt_config.admin_role_claim, &jwt_config.admin_role_value)
    else {
        return UserRole::User;
    };

    // Fast path: the well-known top-level "roles" array has a dedicated typed field.
    if claim_name == "roles" {
        if claims.roles.iter().any(|r| r == claim_value) {
            return UserRole::Admin;
        }
        return UserRole::User;
    }

    // Generic path: traverse dot-separated path in extra claims.
    let extra_root = serde_json::Value::Object(
        claims
            .extra
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    if let Some(node) = get_claim_value(&extra_root, claim_name) {
        let is_admin = match node {
            serde_json::Value::String(s) => s == claim_value,
            serde_json::Value::Array(arr) => arr
                .iter()
                .any(|v| v.as_str().is_some_and(|s| s == claim_value)),
            _ => false,
        };
        if is_admin {
            return UserRole::Admin;
        }
    }

    UserRole::User
}
