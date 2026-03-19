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
use detrix_config::AuthMode;
use tracing::{debug, warn};

#[cfg(test)]
use detrix_config::constants::AUTHORIZATION_HEADER;
use detrix_config::constants::BEARER_PREFIX;

/// Re-export the shared `AuthState` from `crate::common::auth`.
pub use crate::common::auth::AuthState;
/// Re-export the shared `AuthenticatedUser` type from `crate::common`.
pub use crate::common::AuthenticatedUser;

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
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let config = &auth_state.config;

    // When auth is disabled: inject default Admin user so handlers always have scope
    if config.effective_mode() == AuthMode::Disabled {
        let mut request = request;
        request
            .extensions_mut()
            .insert(AuthenticatedUser::default_admin());
        return Ok(next.run(request).await);
    }

    // Own the path to avoid borrow issues with request mutation
    let path = request.uri().path().to_string();

    // Skip auth for public endpoints — inject default Admin user so that
    // any downstream handler using Extension<AuthenticatedUser> still works.
    if config.is_public_endpoint(&path) {
        debug!(path = %path, "Skipping auth for public endpoint");
        request
            .extensions_mut()
            .insert(AuthenticatedUser::default_admin());
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

            // Delegate to shared authenticate_token logic
            match crate::common::authenticate_token(token, &auth_state).await {
                Ok(authenticated) => {
                    debug!(path = %path, user_id = %authenticated.user_id, "Authentication successful");
                    request.extensions_mut().insert(authenticated);
                    Ok(next.run(request).await)
                }
                Err(crate::common::AuthError::Internal(msg)) => {
                    warn!(path = %path, "Auth internal error: {}", msg);
                    Err(StatusCode::INTERNAL_SERVER_ERROR)
                }
                Err(crate::common::AuthError::Unauthenticated(msg)) => {
                    warn!(path = %path, "Auth failed: {}", msg);
                    Err(StatusCode::UNAUTHORIZED)
                }
            }
        }
        None => {
            warn!(path = %path, "Missing Authorization header");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

/// Re-export `resolve_jwt_role` from `crate::common::auth` for backwards compatibility.
pub use crate::common::auth::resolve_jwt_role;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get, Router};
    use detrix_config::{AuthConfig, StaticUser, UserRole};
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

    #[test]
    fn test_resolve_jwt_role_with_standard_roles_claim() {
        let claims = detrix_application::JwtClaims {
            roles: vec!["admin".to_string()],
            ..Default::default()
        };
        let config = detrix_config::JwtConfig {
            admin_role_claim: Some("roles".to_string()),
            admin_role_value: Some("admin".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_jwt_role(&claims, &config), UserRole::Admin);
    }

    #[test]
    fn test_resolve_jwt_role_with_custom_claim_name() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("groups".to_string(), serde_json::json!(["admin", "users"]));
        let claims = detrix_application::JwtClaims {
            extra,
            ..Default::default()
        };
        let config = detrix_config::JwtConfig {
            admin_role_claim: Some("groups".to_string()),
            admin_role_value: Some("admin".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_jwt_role(&claims, &config), UserRole::Admin);
    }

    #[test]
    fn test_resolve_jwt_role_nested_dot_path() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "realm_access".to_string(),
            serde_json::json!({"roles": ["detrix-admin", "user"]}),
        );
        let claims = detrix_application::JwtClaims {
            extra,
            ..Default::default()
        };
        let config = detrix_config::JwtConfig {
            admin_role_claim: Some("realm_access.roles".to_string()),
            admin_role_value: Some("detrix-admin".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_jwt_role(&claims, &config), UserRole::Admin);
    }

    #[test]
    fn test_resolve_jwt_role_no_match_returns_user() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("groups".to_string(), serde_json::json!(["viewer"]));
        let claims = detrix_application::JwtClaims {
            extra,
            ..Default::default()
        };
        let config = detrix_config::JwtConfig {
            admin_role_claim: Some("groups".to_string()),
            admin_role_value: Some("admin".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_jwt_role(&claims, &config), UserRole::User);
    }
}
