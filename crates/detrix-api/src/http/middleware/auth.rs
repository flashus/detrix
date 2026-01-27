//! Authentication middleware
//!
//! Bearer token authentication for API endpoints.
//! Supports two authentication modes:
//! - Simple: Static bearer token from config (like Prometheus)
//! - External: JWT validation via JWKS endpoint (for enterprise SSO)

use axum::{
    body::Body,
    extract::State,
    http::{header::AUTHORIZATION, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use detrix_application::JwksValidator;
use detrix_config::{AuthConfig, AuthMode};
use std::sync::Arc;
use tracing::{debug, error, warn};

use crate::grpc::BEARER_PREFIX;

/// Authenticated user claims extracted from JWT
///
/// Injected into request extensions after successful JWT validation.
/// Handlers can extract this using `Extension<AuthClaims>`.
///
/// Note: Fields are intentionally public for handler access via request extensions,
/// even though they're not read directly in this module.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct AuthClaims {
    /// Subject (user ID)
    pub sub: Option<String>,
    /// User email
    pub email: Option<String>,
    /// User display name
    pub name: Option<String>,
    /// User roles for authorization
    pub roles: Vec<String>,
}

/// Authentication middleware state
#[derive(Clone)]
pub struct AuthState {
    pub config: Arc<AuthConfig>,
    /// JWT validator for external mode (optional - only needed for external auth)
    pub jwt_validator: Option<Arc<JwksValidator>>,
}

impl AuthState {
    /// Create auth state from config (simple mode or disabled)
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

/// Authentication middleware
///
/// When authentication is enabled:
/// - Checks if the endpoint is public (no auth required)
/// - Validates the Authorization header contains a valid Bearer token
/// - For Simple mode: validates against static bearer_token from config
/// - For External mode: validates JWT via JWKS
/// - Returns 401 Unauthorized if token is missing or invalid
///
/// When authentication is disabled:
/// - All requests pass through without validation
pub async fn auth_middleware(
    State(auth_state): State<AuthState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let config = &auth_state.config;

    // Skip auth if disabled
    if config.mode == AuthMode::Disabled {
        return Ok(next.run(request).await);
    }

    // Own the path to avoid borrow issues with request mutation
    let path = request.uri().path().to_string();

    // Skip auth for public endpoints
    if config.is_public_endpoint(&path) {
        debug!(path = %path, "Skipping auth for public endpoint");
        return Ok(next.run(request).await);
    }

    // Extract and validate Bearer token
    let auth_header = request.headers().get(AUTHORIZATION).cloned();

    match auth_header {
        Some(value) => {
            let header_str = value.to_str().map_err(|_| {
                warn!(path = %path, "Invalid Authorization header encoding");
                StatusCode::UNAUTHORIZED
            })?;

            // Extract token from "Bearer <token>" format
            let token = header_str.strip_prefix(BEARER_PREFIX).ok_or_else(|| {
                warn!(path = %path, "Authorization header must use Bearer scheme");
                StatusCode::UNAUTHORIZED
            })?;

            match config.mode {
                AuthMode::Simple => {
                    // Validate against static bearer_token
                    if let Some(expected_token) = config.bearer_token.as_deref() {
                        if token == expected_token {
                            debug!(path = %path, "Bearer token authentication successful");
                            return Ok(next.run(request).await);
                        }
                    }
                    warn!(path = %path, "Invalid bearer token");
                    Err(StatusCode::UNAUTHORIZED)
                }
                AuthMode::External => {
                    // Validate JWT via JWKS
                    let validator = auth_state.jwt_validator.as_ref().ok_or_else(|| {
                        error!(path = %path, "JWT validator not configured for external mode");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;

                    match validator.validate_token(token).await {
                        Ok(claims) => {
                            debug!(
                                path = %path,
                                sub = ?claims.sub,
                                email = ?claims.email,
                                "JWT authentication successful"
                            );

                            // Inject claims into request extensions for use by handlers
                            let auth_claims = AuthClaims {
                                sub: claims.sub.clone(),
                                email: claims.email.clone(),
                                name: claims.name.clone(),
                                roles: claims.roles.clone(),
                            };

                            let mut request = request;
                            request.extensions_mut().insert(auth_claims);

                            Ok(next.run(request).await)
                        }
                        Err(e) => {
                            warn!(path = %path, error = %e, "JWT validation failed");
                            Err(StatusCode::UNAUTHORIZED)
                        }
                    }
                }
                AuthMode::Disabled => {
                    // Should not reach here, but handle gracefully
                    Ok(next.run(request).await)
                }
            }
        }
        None => {
            warn!(path = %path, "Missing Authorization header");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get, Router};
    use tower::ServiceExt;

    async fn test_handler() -> &'static str {
        "ok"
    }

    fn create_test_router(auth_state: AuthState) -> Router {
        Router::new()
            .route("/protected", get(test_handler))
            .route("/health", get(test_handler))
            .layer(axum::middleware::from_fn_with_state(
                auth_state,
                auth_middleware,
            ))
    }

    fn test_config_simple(token: &str) -> AuthConfig {
        AuthConfig {
            mode: AuthMode::Simple,
            bearer_token: Some(token.to_string()),
            public_endpoints: vec!["/health".to_string()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_auth_disabled_allows_all() {
        let config = AuthConfig::default(); // mode = Disabled by default
        let auth_state = AuthState::new(config);
        let router = create_test_router(auth_state);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_enabled_public_endpoint() {
        let config = test_config_simple("secret");
        let auth_state = AuthState::new(config);
        let router = create_test_router(auth_state);

        // Public endpoint should work without auth
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_enabled_protected_without_token() {
        let config = test_config_simple("secret");
        let auth_state = AuthState::new(config);
        let router = create_test_router(auth_state);

        // Protected endpoint without token should fail
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_enabled_protected_with_valid_token() {
        let config = test_config_simple("secret");
        let auth_state = AuthState::new(config);
        let router = create_test_router(auth_state);

        // Protected endpoint with valid token should succeed
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_enabled_protected_with_invalid_token() {
        let config = test_config_simple("secret");
        let auth_state = AuthState::new(config);
        let router = create_test_router(auth_state);

        // Protected endpoint with invalid token should fail
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_enabled_wrong_scheme() {
        let config = test_config_simple("secret");
        let auth_state = AuthState::new(config);
        let router = create_test_router(auth_state);

        // Using Basic auth instead of Bearer should fail
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Basic secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_external_mode_without_validator_returns_error() {
        let config = AuthConfig {
            mode: AuthMode::External,
            ..Default::default()
        };
        // Create without validator - should return 500 for external mode
        let auth_state = AuthState::new(config);
        let router = create_test_router(auth_state);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer some-jwt-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return 500 because validator is not configured
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
