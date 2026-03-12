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
use detrix_config::{AuthConfig, AuthMode, UserRole, AUTO_AUTH_DEFAULT_USER_ID};
use std::sync::Arc;
use tracing::{debug, error, warn};

#[cfg(test)]
use detrix_config::constants::AUTHORIZATION_HEADER;
use detrix_config::constants::BEARER_PREFIX;

/// Re-export the shared `AuthenticatedUser` type from `crate::common`.
pub use crate::common::AuthenticatedUser;

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
            user_id: AUTO_AUTH_DEFAULT_USER_ID.to_string(),
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
                            // Reject JWTs without `sub` — all un-identified callers
                            // sharing "anonymous" would break tenant isolation.
                            let user_id = match claims.sub.clone() {
                                Some(sub) => sub,
                                None => {
                                    warn!(path = %path, "JWT missing 'sub' claim — rejecting");
                                    return Err(StatusCode::UNAUTHORIZED);
                                }
                            };

                            debug!(
                                path = %path,
                                sub = %user_id,
                                email = ?claims.email,
                                "JWT authentication successful"
                            );

                            // Determine role from JWT claims
                            let role = resolve_jwt_role(&claims, &config.jwt);

                            let authenticated = AuthenticatedUser { user_id, role };

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

/// Traverse a dot-separated path in a JSON value (e.g. `"realm_access.roles"`).
fn get_claim_value<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.').try_fold(root, |node, key| node.get(key))
}

/// Determine user role from JWT claims based on config.
///
/// Supports dot-path notation for nested claims, e.g. `admin_role_claim = "realm_access.roles"`.
pub fn resolve_jwt_role(
    claims: &detrix_application::JwtClaims,
    jwt_config: &detrix_config::JwtConfig,
) -> UserRole {
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
