//! gRPC Authentication Interceptor
//!
//! Provides authentication for gRPC services, matching the REST API auth behavior.
//! Supports two authentication modes:
//! - Simple: Per-user static tokens from config
//! - External: JWT validation via JWKS endpoint (for enterprise SSO)

use detrix_config::AuthMode;
use tonic::{Request, Status};
use tracing::{debug, warn};

use detrix_config::constants::{AUTHORIZATION_METADATA_KEY, BEARER_PREFIX};

/// Re-export the shared `AuthenticatedUser` type from `crate::common`.
pub use crate::common::AuthenticatedUser;

/// Type alias — gRPC interceptor uses the same auth state as HTTP middleware.
pub type AuthInterceptorState = crate::common::auth::AuthState;

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
            request
                .extensions_mut()
                .insert(AuthenticatedUser::default_admin());
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

                // Delegate to shared authenticate_token via a scoped thread
                // (tonic interceptors are sync; authenticate_token is async).
                let state_clone = state.clone();
                let token_owned = token.to_string();
                #[allow(clippy::expect_used)]
                let auth_result = std::thread::scope(|s| {
                    s.spawn(|| {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("tokio runtime construction cannot fail")
                            .block_on(crate::common::authenticate_token(
                                &token_owned,
                                &state_clone,
                            ))
                    })
                    .join()
                    .expect("Auth thread panicked")
                });

                match auth_result {
                    Ok(authenticated) => {
                        debug!(user_id = %authenticated.user_id, "gRPC authentication successful");
                        let mut request = request;
                        request.extensions_mut().insert(authenticated);
                        Ok(request)
                    }
                    Err(crate::common::AuthError::Internal(msg)) => Err(Status::internal(msg)),
                    Err(crate::common::AuthError::Unauthenticated(msg)) => {
                        Err(Status::unauthenticated(msg))
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
    use detrix_config::{AuthConfig, StaticUser, UserRole};

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
        assert_eq!(result.unwrap_err().code(), tonic::Code::Internal);
    }
}
