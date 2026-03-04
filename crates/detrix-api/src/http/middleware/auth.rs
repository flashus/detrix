//! Authentication middleware
//!
//! Bearer token authentication for API endpoints.
//! Supports two authentication modes:
//! - Simple: Per-user static tokens from config
//! - External: JWT validation via JWKS endpoint (for enterprise SSO)

use axum::{
    body::Body,
    extract::State,
    http::{header::AUTHORIZATION, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use detrix_application::JwksValidator;
use detrix_config::{AuthConfig, AuthMode, UserRole};
use std::sync::Arc;
use tracing::{debug, error, warn};

#[cfg(test)]
use detrix_config::constants::AUTHORIZATION_HEADER;
use detrix_config::constants::BEARER_PREFIX;

/// Authenticated user identity extracted from token or JWT.
///
/// Injected into request extensions after successful authentication.
/// Handlers can extract this using `Extension<AuthenticatedUser>`.
#[derive(Clone, Debug)]
pub struct AuthenticatedUser {
    /// User identity (from StaticUser.user_id or JWT sub claim)
    pub user_id: String,
    /// User role (Admin or User)
    pub role: UserRole,
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
/// - For Simple mode: looks up token in configured users list
/// - For External mode: validates JWT via JWKS
/// - Returns 401 Unauthorized if token is missing or invalid
/// - Injects `AuthenticatedUser` into request extensions on success
///
/// When authentication is disabled:
/// - All requests pass through without validation
pub async fn auth_middleware(
    State(auth_state): State<AuthState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let config = &auth_state.config;

    // When auth is disabled: inject default Admin user so handlers always have scope
    if config.effective_mode() == AuthMode::Disabled {
        let mut request = request;
        request.extensions_mut().insert(AuthenticatedUser {
            user_id: "default".to_string(),
            role: UserRole::Admin,
        });
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

            match config.effective_mode() {
                AuthMode::Simple => {
                    // Look up token in configured users list
                    if let Some(user) = config.find_user_by_token(token) {
                        debug!(path = %path, user_id = %user.user_id, "Bearer token authentication successful");
                        let authenticated = AuthenticatedUser {
                            user_id: user.user_id.clone(),
                            role: user.role.clone(),
                        };
                        let mut request = request;
                        request.extensions_mut().insert(authenticated);
                        return Ok(next.run(request).await);
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

                            // Determine role from JWT claims
                            let role = resolve_jwt_role(&claims, &config.jwt);

                            let authenticated = AuthenticatedUser {
                                user_id: claims
                                    .sub
                                    .clone()
                                    .unwrap_or_else(|| "anonymous".to_string()),
                                role,
                            };

                            let mut request = request;
                            request.extensions_mut().insert(authenticated);

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

/// Determine user role from JWT claims based on config
pub fn resolve_jwt_role(
    claims: &detrix_application::JwtClaims,
    jwt_config: &detrix_config::JwtConfig,
) -> UserRole {
    if let (Some(ref claim_name), Some(ref claim_value)) =
        (&jwt_config.admin_role_claim, &jwt_config.admin_role_value)
    {
        // Check the standard "roles" field if claim_name is "roles"
        if claim_name == "roles" && claims.roles.iter().any(|r| r == claim_value) {
            return UserRole::Admin;
        }
    }
    UserRole::User
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get, Router};
    use detrix_config::StaticUser;
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
            mode: Some(AuthMode::Simple),
            users: vec![StaticUser {
                token: token.to_string(),
                user_id: "test-user".to_string(),
                role: UserRole::User,
            }],
            public_endpoints: vec!["/health".to_string()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_auth_disabled_allows_all() {
        let config = AuthConfig::default(); // mode = None → effective_mode() = Disabled
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
    async fn test_auth_explicit_disabled_allows_all() {
        let config = AuthConfig {
            mode: Some(AuthMode::Disabled),
            ..Default::default()
        };
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

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(AUTHORIZATION_HEADER, format!("{}secret", BEARER_PREFIX))
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

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(
                        AUTHORIZATION_HEADER,
                        format!("{}wrong-token", BEARER_PREFIX),
                    )
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

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(AUTHORIZATION_HEADER, "Basic secret")
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
            mode: Some(AuthMode::External),
            ..Default::default()
        };
        let auth_state = AuthState::new(config);
        let router = create_test_router(auth_state);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(
                        AUTHORIZATION_HEADER,
                        format!("{}some-jwt-token", BEARER_PREFIX),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_simple_mode_multi_user_lookup() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: vec![
                StaticUser {
                    token: "alice-token".to_string(),
                    user_id: "alice".to_string(),
                    role: UserRole::User,
                },
                StaticUser {
                    token: "admin-token".to_string(),
                    user_id: "admin".to_string(),
                    role: UserRole::Admin,
                },
            ],
            public_endpoints: vec!["/health".to_string()],
            ..Default::default()
        };
        let auth_state = AuthState::new(config);
        let router = create_test_router(auth_state);

        // Alice's token should work
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(
                        AUTHORIZATION_HEADER,
                        format!("{}alice-token", BEARER_PREFIX),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Admin token should work
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(
                        AUTHORIZATION_HEADER,
                        format!("{}admin-token", BEARER_PREFIX),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Unknown token should fail
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header(
                        AUTHORIZATION_HEADER,
                        format!("{}unknown-token", BEARER_PREFIX),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
