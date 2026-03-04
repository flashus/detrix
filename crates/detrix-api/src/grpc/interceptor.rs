//! gRPC Authentication Interceptor
//!
//! Provides authentication for gRPC services, matching the REST API auth behavior.
//! Supports two authentication modes:
//! - Simple: Per-user static tokens from config
//! - External: JWT validation via JWKS endpoint (for enterprise SSO)

use detrix_application::JwksValidator;
use detrix_config::{AuthConfig, AuthMode, UserRole};
use std::sync::Arc;
use tonic::{Request, Status};
use tracing::{debug, error, warn};

use detrix_config::constants::{AUTHORIZATION_METADATA_KEY, BEARER_PREFIX};

/// Authenticated user identity (same as HTTP middleware version)
#[derive(Clone, Debug)]
pub struct AuthenticatedUser {
    pub user_id: String,
    pub role: UserRole,
}

/// Auth interceptor state (shared across all interceptor clones)
#[derive(Clone)]
pub struct AuthInterceptorState {
    pub config: Arc<AuthConfig>,
    /// JWT validator for external mode (optional - only needed for external auth)
    pub jwt_validator: Option<Arc<JwksValidator>>,
}

impl AuthInterceptorState {
    /// Create new auth state from config (simple mode or disabled)
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config: Arc::new(config),
            jwt_validator: None,
        }
    }

    /// Create auth state with JWT validator for external mode
    pub fn with_jwt_validator(config: AuthConfig, validator: JwksValidator) -> Self {
        Self {
            config: Arc::new(config),
            jwt_validator: Some(Arc::new(validator)),
        }
    }
}

/// Create an auth interceptor function for gRPC services
///
/// This returns a closure that can be used with tonic's `with_interceptor` method.
/// The interceptor validates Bearer tokens from the `authorization` metadata key
/// and injects `AuthenticatedUser` into request extensions on success.
pub fn create_auth_interceptor(
    state: AuthInterceptorState,
) -> impl Fn(Request<()>) -> Result<Request<()>, Status> + Clone + Send + Sync + 'static {
    move |request: Request<()>| {
        let config = &state.config;

        // When auth is disabled: inject default Admin user so handlers always have scope
        if config.effective_mode() == AuthMode::Disabled {
            let mut request = request;
            request.extensions_mut().insert(AuthenticatedUser {
                user_id: "default".to_string(),
                role: UserRole::Admin,
            });
            return Ok(request);
        }

        // Extract Authorization header from metadata
        let auth_value = request
            .metadata()
            .get(AUTHORIZATION_METADATA_KEY)
            .and_then(|v| v.to_str().ok());

        match auth_value {
            Some(header) => {
                // Extract token from "Bearer <token>" format
                let token = header.strip_prefix(BEARER_PREFIX).ok_or_else(|| {
                    warn!("Authorization header must use Bearer scheme");
                    Status::unauthenticated("Authorization header must use Bearer scheme")
                })?;

                match config.effective_mode() {
                    AuthMode::Simple => {
                        // Look up token in configured users list
                        if let Some(user) = config.find_user_by_token(token) {
                            debug!(user_id = %user.user_id, "Bearer token authentication successful");
                            let mut request = request;
                            request.extensions_mut().insert(AuthenticatedUser {
                                user_id: user.user_id.clone(),
                                role: user.role.clone(),
                            });
                            Ok(request)
                        } else {
                            warn!("Invalid Bearer token");
                            Err(Status::unauthenticated("Invalid token"))
                        }
                    }
                    AuthMode::External => {
                        // Validate JWT via JWKS
                        let validator = state.jwt_validator.as_ref().ok_or_else(|| {
                            error!("JWT validator not configured for external mode");
                            Status::unauthenticated("Authentication not configured")
                        })?;

                        // Use block_in_place to run async validation in sync interceptor
                        let result = tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current()
                                .block_on(validator.validate_token(token))
                        });

                        match result {
                            Ok(claims) => {
                                debug!(
                                    sub = ?claims.sub,
                                    email = ?claims.email,
                                    "JWT authentication successful"
                                );
                                let role =
                                    crate::http::middleware::resolve_jwt_role(&claims, &config.jwt);
                                let mut request = request;
                                request.extensions_mut().insert(AuthenticatedUser {
                                    user_id: claims
                                        .sub
                                        .clone()
                                        .unwrap_or_else(|| "anonymous".to_string()),
                                    role,
                                });
                                Ok(request)
                            }
                            Err(e) => {
                                warn!(error = %e, "JWT validation failed");
                                Err(Status::unauthenticated("Invalid token"))
                            }
                        }
                    }
                    AuthMode::Disabled => Ok(request),
                }
            }
            None => {
                warn!("Missing Authorization header");
                Err(Status::unauthenticated("Missing authorization"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_config::StaticUser;

    fn test_users() -> Vec<StaticUser> {
        vec![StaticUser {
            token: "valid-token".to_string(),
            user_id: "test-user".to_string(),
            role: UserRole::User,
        }]
    }

    #[test]
    fn test_auth_state_new() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: test_users(),
            ..Default::default()
        };

        let state = AuthInterceptorState::new(config);
        assert_eq!(state.config.mode, Some(AuthMode::Simple));
        assert_eq!(state.config.users.len(), 1);
        assert!(state.jwt_validator.is_none());
    }

    #[test]
    fn test_auth_disabled() {
        let config = AuthConfig::default();
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        let request = Request::new(());
        let result = interceptor(request);
        assert!(result.is_ok());
    }

    #[test]
    fn test_auth_explicit_disabled() {
        let config = AuthConfig {
            mode: Some(AuthMode::Disabled),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        let request = Request::new(());
        let result = interceptor(request);
        assert!(result.is_ok());
    }

    #[test]
    fn test_simple_mode_valid_token() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: test_users(),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        let mut request = Request::new(());
        request.metadata_mut().insert(
            AUTHORIZATION_METADATA_KEY,
            format!("{}valid-token", BEARER_PREFIX).parse().unwrap(),
        );

        let result = interceptor(request);
        assert!(result.is_ok());
    }

    #[test]
    fn test_simple_mode_invalid_token() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: test_users(),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        let mut request = Request::new(());
        request.metadata_mut().insert(
            AUTHORIZATION_METADATA_KEY,
            format!("{}wrong-token", BEARER_PREFIX).parse().unwrap(),
        );

        let result = interceptor(request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_missing_auth_header() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: test_users(),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        let request = Request::new(());
        let result = interceptor(request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_wrong_auth_scheme() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: test_users(),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        let mut request = Request::new(());
        request.metadata_mut().insert(
            AUTHORIZATION_METADATA_KEY,
            "Basic valid-token".parse().unwrap(),
        );

        let result = interceptor(request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_external_mode_without_validator() {
        let config = AuthConfig {
            mode: Some(AuthMode::External),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        let mut request = Request::new(());
        request.metadata_mut().insert(
            AUTHORIZATION_METADATA_KEY,
            format!("{}some-jwt", BEARER_PREFIX).parse().unwrap(),
        );

        let result = interceptor(request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }
}
