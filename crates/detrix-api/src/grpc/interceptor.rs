//! gRPC Authentication Interceptor
//!
//! Provides authentication for gRPC services, matching the REST API auth behavior.
//! Supports two authentication modes:
//! - Simple: Static bearer token from config (like Prometheus)
//! - External: JWT validation via JWKS endpoint (for enterprise SSO)

use detrix_application::JwksValidator;
use detrix_config::{AuthConfig, AuthMode};
use std::sync::Arc;
use tonic::{Request, Status};
use tracing::{debug, error, warn};

use super::auth::{AUTHORIZATION_METADATA_KEY, BEARER_PREFIX};

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
/// The interceptor validates Bearer tokens from the `authorization` metadata key.
///
/// # Note on External Mode
///
/// For External mode (JWT validation), the interceptor uses blocking validation
/// within the tokio runtime. This works well for moderate traffic but for very
/// high-throughput scenarios, consider implementing a tower-based async layer.
///
/// # Example
///
/// ```ignore
/// let auth_state = AuthInterceptorState::new(config);
/// let interceptor = create_auth_interceptor(auth_state);
///
/// Server::builder()
///     .add_service(MetricsServiceServer::with_interceptor(metrics_service, interceptor))
/// ```
pub fn create_auth_interceptor(
    state: AuthInterceptorState,
) -> impl Fn(Request<()>) -> Result<Request<()>, Status> + Clone + Send + Sync + 'static {
    move |request: Request<()>| {
        let config = &state.config;

        // Skip auth if disabled
        if config.mode == AuthMode::Disabled {
            return Ok(request);
        }

        // NOTE: gRPC public endpoint check is configured via `grpc_public_methods` in config,
        // but the actual method path is not easily accessible in tonic interceptors.
        // The HTTP/2 :path pseudo-header is not exposed through standard metadata.
        // For now, all gRPC methods require auth when auth is enabled.
        // Future: Consider using a tower layer for more fine-grained path-based auth.

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

                match config.mode {
                    AuthMode::Simple => {
                        // Validate against static bearer_token
                        let expected_token = config.bearer_token.as_deref().ok_or_else(|| {
                            warn!("Auth enabled but no bearer_token configured");
                            // Return 401 rather than 500 - this is an auth failure from client perspective
                            Status::unauthenticated("Authentication not configured")
                        })?;

                        if token == expected_token {
                            debug!("Bearer token authentication successful");
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
                            // Return 401 rather than 500 - this is an auth failure from client perspective
                            Status::unauthenticated("Authentication not configured")
                        })?;

                        // Use block_in_place to run async validation in sync interceptor
                        // This is safe because we're already in a tokio runtime context
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
                                Ok(request)
                            }
                            Err(e) => {
                                warn!(error = %e, "JWT validation failed");
                                Err(Status::unauthenticated("Invalid token"))
                            }
                        }
                    }
                    AuthMode::Disabled => {
                        // Should not reach here, but handle gracefully
                        Ok(request)
                    }
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

    #[test]
    fn test_auth_state_new() {
        let config = AuthConfig {
            mode: AuthMode::Simple,
            bearer_token: Some("test-token".to_string()),
            ..Default::default()
        };

        let state = AuthInterceptorState::new(config.clone());
        assert_eq!(state.config.mode, AuthMode::Simple);
        assert_eq!(state.config.bearer_token, Some("test-token".to_string()));
        assert!(state.jwt_validator.is_none());
    }

    #[test]
    fn test_auth_disabled() {
        let config = AuthConfig::default(); // mode = Disabled by default
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        // Request without auth should pass when auth is disabled
        let request = Request::new(());
        let result = interceptor(request);
        assert!(result.is_ok());
    }

    #[test]
    fn test_simple_mode_valid_token() {
        let config = AuthConfig {
            mode: AuthMode::Simple,
            bearer_token: Some("valid-token".to_string()),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        // Request with valid token should pass
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
            mode: AuthMode::Simple,
            bearer_token: Some("valid-token".to_string()),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        // Request with invalid token should fail
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
            mode: AuthMode::Simple,
            bearer_token: Some("valid-token".to_string()),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        // Request without auth header should fail
        let request = Request::new(());
        let result = interceptor(request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_wrong_auth_scheme() {
        let config = AuthConfig {
            mode: AuthMode::Simple,
            bearer_token: Some("valid-token".to_string()),
            ..Default::default()
        };
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        // Request with Basic auth instead of Bearer should fail
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
            mode: AuthMode::External,
            ..Default::default()
        };
        // Create without validator
        let state = AuthInterceptorState::new(config);
        let interceptor = create_auth_interceptor(state);

        // Request should fail with unauthenticated (401) error, not internal (500)
        let mut request = Request::new(());
        request.metadata_mut().insert(
            AUTHORIZATION_METADATA_KEY,
            format!("{}some-jwt", BEARER_PREFIX).parse().unwrap(),
        );

        let result = interceptor(request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    // NOTE: Tests for gRPC public method bypass are not included because
    // tonic interceptors don't have access to the HTTP/2 :path pseudo-header.
    // The `grpc_public_methods` config option is reserved for future use
    // when implementing this via a tower layer.
}
