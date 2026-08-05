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

/// Create an auth interceptor function for the AgentService.
///
/// Unlike the main gRPC auth interceptor, this one validates agent bearer tokens
/// by checking SHA-256(token) against the configured agent token hashes.
/// This allows agents to authenticate independently of user/JWT tokens.
///
/// When `dev_mode` is true and `agent_token_hashes` is empty, all connections are
/// accepted (development only). When `dev_mode` is false and no tokens are configured,
/// all connections are rejected.
pub fn create_agent_auth_interceptor(
    agent_token_hashes: Vec<String>,
    dev_mode: bool,
) -> impl Fn(Request<()>) -> Result<Request<()>, Status> + Clone + Send + Sync + 'static {
    use sha2::{Digest, Sha256};
    use subtle::ConstantTimeEq;

    // Log configuration warnings once at construction, not on every request.
    if agent_token_hashes.is_empty() {
        if dev_mode {
            warn!("Agent auth is DISABLED (agent.dev_mode=true) — all connections accepted. Not safe for production.");
        } else {
            warn!("Agent gRPC endpoint active with NO tokens configured and dev_mode=false — all agent connections will be rejected.");
        }
    }

    move |request: Request<()>| {
        if agent_token_hashes.is_empty() {
            if dev_mode {
                return Ok(request);
            }
            return Err(Status::unauthenticated(
                "Agent tokens not configured; set agent.agent_token_hashes or enable agent.dev_mode",
            ));
        }

        let auth_value = request
            .metadata()
            .get(AUTHORIZATION_METADATA_KEY)
            .and_then(|v| v.to_str().ok());

        let token = match auth_value {
            Some(header) => header
                .strip_prefix(BEARER_PREFIX)
                .ok_or_else(|| Status::unauthenticated("Must use Bearer scheme"))?,
            None => return Err(Status::unauthenticated("Missing authorization")),
        };

        // Both sides are always 64-char lowercase hex (SHA-256 of ASCII token).
        // ct_eq is constant-time only when lengths match — the length equality is
        // guaranteed here since both are fixed-length SHA-256 hex digests.
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        let found = agent_token_hashes
            .iter()
            .any(|h| h.as_bytes().ct_eq(hash.as_bytes()).into());
        if found {
            Ok(request)
        } else {
            Err(Status::unauthenticated("Invalid agent token"))
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

                // Synchronous validation — no nested runtime required.
                // JWT keys are pre-fetched at daemon startup (force_refresh),
                // so validation is pure CPU (cache read + signature verify).
                let auth_result = crate::common::authenticate_token_sync(token, &state);

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
    use sha2::{Digest, Sha256};

    fn token_hash(token: &str) -> String {
        format!("{:x}", Sha256::digest(token.as_bytes()))
    }

    fn agent_request_with_token(token: &str) -> Request<()> {
        let mut req = Request::new(());
        req.metadata_mut().insert(
            AUTHORIZATION_METADATA_KEY,
            format!("{}{}", BEARER_PREFIX, token).parse().unwrap(),
        );
        req
    }

    #[test]
    fn test_agent_auth_valid_token() {
        let hash = token_hash("my-secret-token");
        let interceptor = create_agent_auth_interceptor(vec![hash], false);
        let result = interceptor(agent_request_with_token("my-secret-token"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_agent_auth_invalid_token() {
        let hash = token_hash("my-secret-token");
        let interceptor = create_agent_auth_interceptor(vec![hash], false);
        let result = interceptor(agent_request_with_token("wrong-token"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_agent_auth_missing_header() {
        let hash = token_hash("my-secret-token");
        let interceptor = create_agent_auth_interceptor(vec![hash], false);
        let result = interceptor(Request::new(()));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_agent_auth_dev_mode_no_tokens_accepted() {
        let interceptor = create_agent_auth_interceptor(vec![], true);
        let result = interceptor(Request::new(()));
        assert!(result.is_ok());
    }

    #[test]
    fn test_agent_auth_no_dev_mode_no_tokens_rejected() {
        let interceptor = create_agent_auth_interceptor(vec![], false);
        let result = interceptor(Request::new(()));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_agent_auth_dev_mode_with_tokens_still_validates() {
        let hash = token_hash("my-secret-token");
        // dev_mode=true but tokens ARE configured → token must still be validated
        let interceptor = create_agent_auth_interceptor(vec![hash], true);
        let wrong = interceptor(agent_request_with_token("wrong-token"));
        assert!(wrong.is_err());
        let right = interceptor(agent_request_with_token("my-secret-token"));
        assert!(right.is_ok());
    }

    fn test_users() -> Vec<StaticUser> {
        vec![StaticUser::new(
            "dtx_valid_token_xxxx".to_string(),
            "test-user".to_string(),
            UserRole::User,
        )]
    }

    /// Create an AuthConfig with test users (token hashes computed in `StaticUser::new()`).
    fn test_auth_config() -> AuthConfig {
        AuthConfig {
            mode: Some(AuthMode::Simple),
            users: test_users(),
            ..Default::default()
        }
    }

    #[test]
    fn test_auth_state_new() {
        let config = test_auth_config();

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
        let state = AuthInterceptorState::new(test_auth_config());
        let interceptor = create_auth_interceptor(state);

        let mut request = Request::new(());
        request.metadata_mut().insert(
            AUTHORIZATION_METADATA_KEY,
            format!("{}dtx_valid_token_xxxx", BEARER_PREFIX)
                .parse()
                .unwrap(),
        );

        let result = interceptor(request);
        assert!(result.is_ok());
    }

    #[test]
    fn test_simple_mode_invalid_token() {
        let state = AuthInterceptorState::new(test_auth_config());
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
        let state = AuthInterceptorState::new(test_auth_config());
        let interceptor = create_auth_interceptor(state);

        let request = Request::new(());
        let result = interceptor(request);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_wrong_auth_scheme() {
        let state = AuthInterceptorState::new(test_auth_config());
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
