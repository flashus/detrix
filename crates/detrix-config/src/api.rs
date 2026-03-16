//! API configuration for gRPC and REST endpoints

use crate::constants::{
    DEFAULT_API_HOST, DEFAULT_CONTEXT_LINES, DEFAULT_EVENT_BUFFER_CAPACITY, DEFAULT_GRPC_PORT,
    DEFAULT_GRPC_STREAM_MAX_DURATION_MS, DEFAULT_JWKS_CACHE_TTL_SECONDS, DEFAULT_LOCALHOST_EXEMPT,
    DEFAULT_MAX_QUERY_LIMIT, DEFAULT_MAX_WS_CONNECTIONS_PER_IP, DEFAULT_PORT_FALLBACK,
    DEFAULT_PREVIEW_LINES, DEFAULT_QUERY_LIMIT, DEFAULT_RATE_LIMIT_BURST_SIZE,
    DEFAULT_RATE_LIMIT_PER_SECOND, DEFAULT_REST_PORT, DEFAULT_SYSTEM_EVENT_CHANNEL_CAPACITY,
    DEFAULT_WS_IDLE_CHECK_INTERVAL_MS, DEFAULT_WS_IDLE_TIMEOUT_MS, DEFAULT_WS_PING_INTERVAL_MS,
    DEFAULT_WS_PONG_TIMEOUT_MS, MAX_RATE_LIMIT_BURST,
};
use serde::{Deserialize, Serialize};

/// Default user_id injected when authentication is disabled.
///
/// Used both in the middleware (HTTP/gRPC) and in the auto-auth setup in `serve`.
pub const AUTO_AUTH_DEFAULT_USER_ID: &str = "default";

/// Maximum allowed length for a static bearer token.
const MAX_TOKEN_LEN: usize = 512;

// ============================================================================
// API Config
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    #[serde(default)]
    pub grpc: GrpcConfig,
    #[serde(default)]
    pub rest: RestConfig,
    /// Authentication configuration
    #[serde(default)]
    pub auth: AuthConfig,
    /// Streaming timeout configuration
    #[serde(default)]
    pub streaming: StreamingConfig,
    /// Capacity of the event broadcast channel (buffer size)
    #[serde(default = "default_event_buffer_capacity")]
    pub event_buffer_capacity: usize,
    /// Capacity of the system event broadcast channel
    /// System events include crashes, connections, metric CRUD operations
    /// Default: 1000
    #[serde(default = "default_system_event_channel_capacity")]
    pub system_event_channel_capacity: usize,
    /// Default limit for query operations
    #[serde(default = "default_query_limit")]
    pub default_query_limit: i64,
    /// Maximum limit for query operations
    #[serde(default = "default_max_query_limit")]
    pub max_query_limit: i64,
    /// If true, automatically select an available port when the preferred port is occupied.
    #[serde(default = "default_port_fallback")]
    pub port_fallback: bool,
    /// Number of lines for code context in file inspection
    /// Default: 10
    #[serde(default = "default_context_lines")]
    pub context_lines: usize,
    /// Number of preview lines for file overview in file inspection
    /// Default: 20
    #[serde(default = "default_preview_lines")]
    pub preview_lines: usize,
}

fn default_port_fallback() -> bool {
    DEFAULT_PORT_FALLBACK
}

fn default_event_buffer_capacity() -> usize {
    DEFAULT_EVENT_BUFFER_CAPACITY
}

fn default_system_event_channel_capacity() -> usize {
    DEFAULT_SYSTEM_EVENT_CHANNEL_CAPACITY
}

fn default_query_limit() -> i64 {
    DEFAULT_QUERY_LIMIT
}

fn default_max_query_limit() -> i64 {
    DEFAULT_MAX_QUERY_LIMIT
}

fn default_context_lines() -> usize {
    DEFAULT_CONTEXT_LINES
}

fn default_preview_lines() -> usize {
    DEFAULT_PREVIEW_LINES
}

impl Default for ApiConfig {
    fn default() -> Self {
        ApiConfig {
            grpc: GrpcConfig::default(),
            rest: RestConfig::default(),
            auth: AuthConfig::default(),
            streaming: StreamingConfig::default(),
            event_buffer_capacity: DEFAULT_EVENT_BUFFER_CAPACITY,
            system_event_channel_capacity: DEFAULT_SYSTEM_EVENT_CHANNEL_CAPACITY,
            default_query_limit: DEFAULT_QUERY_LIMIT,
            max_query_limit: DEFAULT_MAX_QUERY_LIMIT,
            port_fallback: DEFAULT_PORT_FALLBACK,
            context_lines: DEFAULT_CONTEXT_LINES,
            preview_lines: DEFAULT_PREVIEW_LINES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrpcConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_api_host")]
    pub host: String,
    #[serde(default = "default_grpc_port")]
    pub port: u16,
}

fn default_true() -> bool {
    true
}

fn default_api_host() -> String {
    DEFAULT_API_HOST.to_string()
}

fn default_grpc_port() -> u16 {
    DEFAULT_GRPC_PORT
}

impl Default for GrpcConfig {
    fn default() -> Self {
        GrpcConfig {
            enabled: true,
            host: DEFAULT_API_HOST.to_string(),
            port: DEFAULT_GRPC_PORT,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_api_host")]
    pub host: String,
    #[serde(default = "default_rest_port")]
    pub port: u16,
    /// Rate limiting configuration
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    /// CORS configuration
    #[serde(default)]
    pub cors: CorsConfig,
    /// Enable admin endpoints (e.g., force disconnect-all).
    /// Default: false. Only enable for trusted environments.
    #[serde(default)]
    pub admin_endpoints_enabled: bool,
}

// ============================================================================
// CORS Config
// ============================================================================

/// CORS (Cross-Origin Resource Sharing) configuration
///
/// Controls which origins are allowed to make requests to the REST API.
/// By default, only localhost origins are allowed for security.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsConfig {
    /// Allow all origins (not recommended for production)
    /// When true, ignores allowed_origins list
    #[serde(default)]
    pub allow_all: bool,
    /// List of allowed origins.
    /// Supports exact matches and wildcard patterns:
    /// - "http://localhost:3000" - exact match
    /// - "http://localhost:*" - any port on localhost
    /// - "https://*.example.com" - any subdomain
    ///
    /// Default: localhost origins only
    #[serde(default = "default_cors_allowed_origins")]
    pub allowed_origins: Vec<String>,
    /// Allow credentials (cookies, authorization headers)
    /// Default: true (needed for auth)
    #[serde(default = "default_true")]
    pub allow_credentials: bool,
    /// Maximum age for preflight cache in seconds
    /// Default: 3600 (1 hour)
    #[serde(default = "default_cors_max_age")]
    pub max_age_seconds: u64,
}

fn default_cors_allowed_origins() -> Vec<String> {
    vec![
        "http://localhost".to_string(),
        "http://127.0.0.1".to_string(),
        "http://[::1]".to_string(),
    ]
}

fn default_cors_max_age() -> u64 {
    3600 // 1 hour
}

impl Default for CorsConfig {
    fn default() -> Self {
        CorsConfig {
            allow_all: false,
            allowed_origins: default_cors_allowed_origins(),
            allow_credentials: true,
            max_age_seconds: 3600,
        }
    }
}

impl CorsConfig {
    /// Validate CORS configuration
    ///
    /// Returns an error if the configuration violates CORS security requirements.
    pub fn validate(&self) -> Result<(), String> {
        // CORS security requirement: cannot use allow_credentials with wildcard origins
        // See: https://developer.mozilla.org/en-US/docs/Web/HTTP/CORS#credentialed_requests_and_wildcards
        if self.allow_all && self.allow_credentials {
            return Err(
                "api.rest.cors: Cannot use allow_all=true with allow_credentials=true. \
                 This is a CORS security restriction."
                    .to_string(),
            );
        }
        Ok(())
    }

    /// Check if an origin is allowed
    pub fn is_origin_allowed(&self, origin: &str) -> bool {
        if self.allow_all {
            return true;
        }

        for pattern in &self.allowed_origins {
            if Self::matches_origin_pattern(pattern, origin) {
                return true;
            }
        }
        false
    }

    /// Match origin against a pattern (supports * wildcards)
    pub fn matches_origin_pattern(pattern: &str, origin: &str) -> bool {
        if pattern == origin {
            return true;
        }

        // Handle wildcard patterns like "http://localhost:*" or "https://*.example.com"
        if pattern.contains('*') {
            let parts: Vec<&str> = pattern.split('*').collect();
            if parts.len() == 2 {
                let prefix = parts[0];
                let suffix = parts[1];
                return origin.starts_with(prefix) && origin.ends_with(suffix);
            }
        }

        false
    }
}

fn default_rest_port() -> u16 {
    DEFAULT_REST_PORT
}

impl Default for RestConfig {
    fn default() -> Self {
        RestConfig {
            enabled: true,
            host: DEFAULT_API_HOST.to_string(),
            port: DEFAULT_REST_PORT,
            rate_limit: RateLimitConfig::default(),
            cors: CorsConfig::default(),
            admin_endpoints_enabled: false,
        }
    }
}

// ============================================================================
// Rate Limiting Config
// ============================================================================

/// Rate limiting configuration to prevent DoS attacks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Enable rate limiting (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum requests per second (sustained rate)
    #[serde(default = "default_rate_limit_per_second")]
    pub per_second: u64,
    /// Maximum burst size (requests allowed in burst)
    #[serde(default = "default_rate_limit_burst_size")]
    pub burst_size: u32,
    /// Exempt localhost (127.0.0.1, ::1) from rate limiting (default: true)
    /// This allows local development tools and MCP clients to connect without limits
    #[serde(default = "default_localhost_exempt")]
    pub localhost_exempt: bool,
}

fn default_rate_limit_per_second() -> u64 {
    DEFAULT_RATE_LIMIT_PER_SECOND
}

fn default_rate_limit_burst_size() -> u32 {
    DEFAULT_RATE_LIMIT_BURST_SIZE
}

fn default_localhost_exempt() -> bool {
    DEFAULT_LOCALHOST_EXEMPT
}

impl RateLimitConfig {
    /// Validate rate limit configuration
    ///
    /// Ensures that rate limiting parameters are valid for use with tower_governor.
    /// Returns an error if configuration is invalid.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(()); // Skip validation if disabled
        }

        if self.per_second == 0 {
            return Err("rate_limit.per_second must be greater than 0".to_string());
        }

        if self.burst_size == 0 {
            return Err("rate_limit.burst_size must be greater than 0".to_string());
        }

        // tower_governor requires burst_size to be reasonable relative to per_second
        // Extremely large burst sizes can cause memory issues
        if self.burst_size > MAX_RATE_LIMIT_BURST {
            return Err(format!(
                "rate_limit.burst_size ({}) exceeds maximum reasonable value ({})",
                self.burst_size, MAX_RATE_LIMIT_BURST
            ));
        }

        Ok(())
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig {
            enabled: true,
            per_second: DEFAULT_RATE_LIMIT_PER_SECOND,
            burst_size: DEFAULT_RATE_LIMIT_BURST_SIZE,
            localhost_exempt: DEFAULT_LOCALHOST_EXEMPT,
        }
    }
}

// ============================================================================
// Authentication Config
// ============================================================================

/// Authentication mode
///
/// - **Disabled**: No authentication required (default)
/// - **Simple**: Static bearer token from config (like Prometheus)
/// - **External**: JWT validation via JWKS endpoint (for enterprise SSO)
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    #[default]
    Disabled,
    Simple,
    External,
}

/// Role for a static user.
///
/// Intentionally has no `Default` impl — all fields of `StaticUser` must be
/// explicit in config to avoid accidentally granting the wrong access level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    User,
}

/// A statically configured user with a bearer token.
///
/// # Note
/// Intentionally no `Default` impl — all fields must be explicit.
#[derive(Clone, Serialize, Deserialize)]
pub struct StaticUser {
    /// Bearer token (constant-time compared; redacted in Debug output)
    pub token: String,
    /// User identity — becomes `metric.user_id`
    pub user_id: String,
    /// Role: admin or user
    pub role: UserRole,
}

impl std::fmt::Debug for StaticUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticUser")
            .field("token", &"[REDACTED]")
            .field("user_id", &self.user_id)
            .field("role", &self.role)
            .finish()
    }
}

/// Authentication configuration for API endpoints
///
/// Supports two modes:
/// 1. **Simple mode**: Per-user static tokens from config
/// 2. **External mode**: JWT validation via JWKS endpoint (for enterprise SSO)
///
/// When `mode` is `None` (no `[api.auth]` section in config), the daemon
/// auto-generates a token for secure-by-default operation.
/// When `mode` is `Some(Disabled)`, auth is explicitly disabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Authentication mode.
    /// - `None`: not configured (daemon auto-enables auth)
    /// - `Some(Disabled)`: explicitly disabled
    /// - `Some(Simple)`: per-user static tokens
    /// - `Some(External)`: JWT via JWKS
    #[serde(default)]
    pub mode: Option<AuthMode>,
    /// Per-user static tokens for simple mode (replaces bearer_token)
    #[serde(default)]
    pub users: Vec<StaticUser>,
    /// JWT configuration for external mode
    #[serde(default)]
    pub jwt: JwtConfig,
    /// Endpoints to exclude from authentication (paths)
    /// Default: ["/health", "/status", "/metrics", "/api/health", "/api/status"]
    #[serde(default = "default_public_endpoints")]
    pub public_endpoints: Vec<String>,
    /// gRPC methods to exclude from authentication
    /// Format: method name fragments (e.g., "GetStatus", "HealthCheck")
    /// Default: ["GetStatus"]
    #[serde(default = "default_grpc_public_methods")]
    pub grpc_public_methods: Vec<String>,

    /// Deprecated: use `[[api.auth.users]]` instead.
    ///
    /// If this field is present in config, `validate()` returns a clear migration error.
    #[serde(default, alias = "bearer_token", skip_serializing)]
    pub _bearer_token_deprecated: Option<String>,
}

/// JWT configuration for external authentication mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtConfig {
    /// JWKS endpoint URL (e.g., `https://auth.example.com/.well-known/jwks.json`)
    #[serde(default)]
    pub jwks_url: Option<String>,
    /// Expected JWT issuer (iss claim)
    #[serde(default)]
    pub issuer: Option<String>,
    /// Expected JWT audience (aud claim)
    #[serde(default)]
    pub audience: Option<String>,
    /// JWKS cache TTL in seconds (default: 300 = 5 minutes)
    #[serde(default = "default_jwks_cache_ttl")]
    pub cache_ttl_seconds: u64,
    /// JWT claim name containing user roles (e.g. "roles", "realm_access.roles")
    #[serde(default)]
    pub admin_role_claim: Option<String>,
    /// Value within the role claim that grants admin access (e.g. "detrix-admin")
    #[serde(default)]
    pub admin_role_value: Option<String>,
}

impl Default for JwtConfig {
    fn default() -> Self {
        JwtConfig {
            jwks_url: None,
            issuer: None,
            audience: None,
            cache_ttl_seconds: DEFAULT_JWKS_CACHE_TTL_SECONDS,
            admin_role_claim: None,
            admin_role_value: None,
        }
    }
}

fn default_jwks_cache_ttl() -> u64 {
    DEFAULT_JWKS_CACHE_TTL_SECONDS
}

fn default_public_endpoints() -> Vec<String> {
    vec![
        "/health".to_string(),
        "/status".to_string(),
        "/metrics".to_string(),
        "/api/health".to_string(),
        "/api/status".to_string(),
    ]
}

fn default_grpc_public_methods() -> Vec<String> {
    vec!["GetStatus".to_string()]
}

impl AuthConfig {
    /// Returns the effective auth mode (`None` is treated as `Disabled`).
    pub fn effective_mode(&self) -> AuthMode {
        self.mode.clone().unwrap_or(AuthMode::Disabled)
    }

    /// Validate authentication configuration
    ///
    /// Ensures that valid credentials are provided when authentication is enabled.
    pub fn validate(&self) -> Result<(), String> {
        // Detect deprecated bearer_token migration scenario
        if self._bearer_token_deprecated.is_some() && self.users.is_empty() {
            return Err("Config error: 'bearer_token' is no longer supported. \
                 Use [[api.auth.users]] instead. See CHANGELOG.md."
                .to_string());
        }

        // Warn if users are configured but mode is not Simple (they will be ignored)
        if matches!(
            self.mode,
            Some(AuthMode::External) | Some(AuthMode::Disabled)
        ) && !self.users.is_empty()
        {
            tracing::warn!(
                mode = ?self.mode,
                "api.auth.users configured but mode is not 'simple'; static tokens will be ignored"
            );
        }

        match self.effective_mode() {
            AuthMode::Disabled => Ok(()),
            AuthMode::Simple => {
                if self.users.is_empty() {
                    return Err(
                        "api.auth.users must contain at least one user in simple mode".to_string(),
                    );
                }

                let mut seen_tokens = std::collections::HashSet::new();
                let mut seen_user_ids = std::collections::HashSet::new();

                for (i, user) in self.users.iter().enumerate() {
                    if user.token.is_empty() {
                        return Err(format!("api.auth.users[{}].token cannot be empty", i));
                    }
                    if user.token.len() > MAX_TOKEN_LEN {
                        return Err(format!(
                            "api.auth.users[{}].token exceeds maximum length of {} characters",
                            i, MAX_TOKEN_LEN
                        ));
                    }
                    if user.user_id.is_empty() {
                        return Err(format!("api.auth.users[{}].user_id cannot be empty", i));
                    }
                    if !seen_tokens.insert(user.token.as_str()) {
                        return Err(format!(
                            "api.auth.users[{}].token is a duplicate (tokens must be unique)",
                            i
                        ));
                    }
                    if !seen_user_ids.insert(user.user_id.as_str()) {
                        return Err(format!(
                            "api.auth.users[{}].user_id '{}' is a duplicate (user_ids must be unique)",
                            i, user.user_id
                        ));
                    }
                }
                Ok(())
            }
            AuthMode::External => {
                if self.jwt.jwks_url.is_none() {
                    return Err("api.auth.jwt.jwks_url is required in external mode".to_string());
                }
                Ok(())
            }
        }
    }

    /// Look up a static user by token using constant-time comparison.
    ///
    /// Hash-then-compare prevents timing side-channels from variable-length tokens:
    /// SHA-256 normalizes all tokens to 32 bytes before `ct_eq`, and full iteration
    /// prevents early-exit leaks.
    pub fn find_user_by_token(&self, token: &str) -> Option<&StaticUser> {
        use sha2::{Digest, Sha256};
        use subtle::ConstantTimeEq;
        let input_hash = Sha256::digest(token.as_bytes());
        let mut result: Option<&StaticUser> = None;
        for user in &self.users {
            let stored_hash = Sha256::digest(user.token.as_bytes());
            if bool::from(input_hash.ct_eq(&stored_hash)) {
                result = Some(user);
            }
        }
        result
    }

    /// Check if a path is a public endpoint (no auth required)
    pub fn is_public_endpoint(&self, path: &str) -> bool {
        self.public_endpoints.iter().any(|p| path.starts_with(p))
    }

    /// Check if a gRPC method is public (no auth required)
    ///
    /// The method path is typically in the format "/package.Service/Method"
    /// This checks if any configured public method name is contained in the path.
    pub fn is_grpc_public_method(&self, method_path: &str) -> bool {
        self.grpc_public_methods
            .iter()
            .any(|m| method_path.contains(m))
    }

    /// Check if authentication is enabled (mode is Simple or External).
    pub fn is_enabled(&self) -> bool {
        matches!(self.mode, Some(AuthMode::Simple) | Some(AuthMode::External))
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            mode: None,
            users: Vec::new(),
            jwt: JwtConfig::default(),
            public_endpoints: default_public_endpoints(),
            grpc_public_methods: default_grpc_public_methods(),
            _bearer_token_deprecated: None,
        }
    }
}

// ============================================================================
// Streaming Config
// ============================================================================

/// Streaming timeout configuration for WebSocket and gRPC streams
///
/// These timeouts help prevent resource exhaustion from idle or stale connections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingConfig {
    /// WebSocket idle timeout in milliseconds (0 = disabled)
    /// Connections with no activity for this duration will be closed.
    #[serde(default = "default_ws_idle_timeout_ms")]
    pub ws_idle_timeout_ms: u64,
    /// WebSocket idle check interval in milliseconds
    /// How often to check for idle WebSocket connections.
    /// Default: 10000ms (10 seconds)
    #[serde(default = "default_ws_idle_check_interval_ms")]
    pub ws_idle_check_interval_ms: u64,
    /// WebSocket ping interval in milliseconds (0 = disabled)
    /// Server will send pings at this interval to keep connections alive.
    #[serde(default = "default_ws_ping_interval_ms")]
    pub ws_ping_interval_ms: u64,
    /// WebSocket pong timeout in milliseconds
    /// If no pong is received within this time after a ping, connection is closed.
    #[serde(default = "default_ws_pong_timeout_ms")]
    pub ws_pong_timeout_ms: u64,
    /// gRPC stream maximum duration in milliseconds (0 = unlimited)
    /// Streams will be terminated after this duration.
    #[serde(default = "default_grpc_stream_max_duration_ms")]
    pub grpc_stream_max_duration_ms: u64,
    /// Maximum number of concurrent WebSocket connections per client IP
    #[serde(default = "default_max_ws_connections_per_ip")]
    pub max_ws_connections_per_ip: usize,
}

fn default_ws_idle_timeout_ms() -> u64 {
    DEFAULT_WS_IDLE_TIMEOUT_MS
}

fn default_ws_idle_check_interval_ms() -> u64 {
    DEFAULT_WS_IDLE_CHECK_INTERVAL_MS
}

fn default_ws_ping_interval_ms() -> u64 {
    DEFAULT_WS_PING_INTERVAL_MS
}

fn default_ws_pong_timeout_ms() -> u64 {
    DEFAULT_WS_PONG_TIMEOUT_MS
}

fn default_grpc_stream_max_duration_ms() -> u64 {
    DEFAULT_GRPC_STREAM_MAX_DURATION_MS
}

fn default_max_ws_connections_per_ip() -> usize {
    DEFAULT_MAX_WS_CONNECTIONS_PER_IP
}

impl StreamingConfig {
    /// Validate streaming configuration
    pub fn validate(&self) -> Result<(), String> {
        // If ping interval is set, pong timeout must be less than ping interval
        if self.ws_ping_interval_ms > 0
            && self.ws_pong_timeout_ms > 0
            && self.ws_pong_timeout_ms >= self.ws_ping_interval_ms
        {
            return Err(format!(
                "ws_pong_timeout_ms ({}) must be less than ws_ping_interval_ms ({})",
                self.ws_pong_timeout_ms, self.ws_ping_interval_ms
            ));
        }

        // If idle timeout is set, it should be greater than ping interval
        if self.ws_idle_timeout_ms > 0
            && self.ws_ping_interval_ms > 0
            && self.ws_idle_timeout_ms <= self.ws_ping_interval_ms
        {
            return Err(format!(
                "ws_idle_timeout_ms ({}) should be greater than ws_ping_interval_ms ({})",
                self.ws_idle_timeout_ms, self.ws_ping_interval_ms
            ));
        }

        if self.max_ws_connections_per_ip == 0 {
            return Err("max_ws_connections_per_ip must be greater than 0".to_string());
        }

        Ok(())
    }

    /// Get WebSocket idle timeout as Duration, or None if disabled
    pub fn ws_idle_timeout(&self) -> Option<std::time::Duration> {
        if self.ws_idle_timeout_ms == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(self.ws_idle_timeout_ms))
        }
    }

    /// Get WebSocket idle check interval as Duration
    pub fn ws_idle_check_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.ws_idle_check_interval_ms)
    }

    /// Get WebSocket ping interval as Duration, or None if disabled
    pub fn ws_ping_interval(&self) -> Option<std::time::Duration> {
        if self.ws_ping_interval_ms == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(self.ws_ping_interval_ms))
        }
    }

    /// Get WebSocket pong timeout as Duration
    pub fn ws_pong_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.ws_pong_timeout_ms)
    }

    /// Get gRPC stream max duration as Duration, or None if unlimited
    pub fn grpc_stream_max_duration(&self) -> Option<std::time::Duration> {
        if self.grpc_stream_max_duration_ms == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(
                self.grpc_stream_max_duration_ms,
            ))
        }
    }
}

impl Default for StreamingConfig {
    fn default() -> Self {
        StreamingConfig {
            ws_idle_timeout_ms: DEFAULT_WS_IDLE_TIMEOUT_MS,
            ws_idle_check_interval_ms: DEFAULT_WS_IDLE_CHECK_INTERVAL_MS,
            ws_ping_interval_ms: DEFAULT_WS_PING_INTERVAL_MS,
            ws_pong_timeout_ms: DEFAULT_WS_PONG_TIMEOUT_MS,
            grpc_stream_max_duration_ms: DEFAULT_GRPC_STREAM_MAX_DURATION_MS,
            max_ws_connections_per_ip: DEFAULT_MAX_WS_CONNECTIONS_PER_IP,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_streaming_config_validate_success() {
        let config = StreamingConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_streaming_config_validate_pong_timeout_too_large() {
        let config = StreamingConfig {
            ws_ping_interval_ms: 30_000,
            ws_pong_timeout_ms: 35_000, // Larger than ping interval
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("pong_timeout_ms"));
    }

    #[test]
    fn test_streaming_config_validate_idle_timeout_too_small() {
        let config = StreamingConfig {
            ws_idle_timeout_ms: 20_000, // Smaller than ping interval
            ws_ping_interval_ms: 30_000,
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("idle_timeout_ms"));
    }

    #[test]
    fn test_streaming_config_validate_max_connections_zero() {
        let config = StreamingConfig {
            max_ws_connections_per_ip: 0,
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max_ws_connections_per_ip"));
    }

    #[test]
    fn test_streaming_config_duration_helpers() {
        let config = StreamingConfig::default();

        // Should return Some for non-zero values
        assert!(config.ws_idle_timeout().is_some());
        assert!(config.ws_ping_interval().is_some());
        assert!(config.grpc_stream_max_duration().is_none()); // 0 = disabled

        // Test actual duration values
        assert_eq!(
            config.ws_idle_timeout(),
            Some(std::time::Duration::from_millis(300_000))
        );
        assert_eq!(
            config.ws_ping_interval(),
            Some(std::time::Duration::from_millis(30_000))
        );
        assert_eq!(
            config.ws_pong_timeout(),
            std::time::Duration::from_millis(10_000)
        );
    }

    #[test]
    fn test_streaming_config_disabled_timeouts() {
        let config = StreamingConfig {
            ws_idle_timeout_ms: 0,
            ws_idle_check_interval_ms: 10_000,
            ws_ping_interval_ms: 0,
            ws_pong_timeout_ms: 0,
            grpc_stream_max_duration_ms: 0,
            max_ws_connections_per_ip: 10,
        };

        assert!(config.ws_idle_timeout().is_none());
        assert!(config.ws_ping_interval().is_none());
        assert!(config.grpc_stream_max_duration().is_none());
        // Note: validate() should pass even with disabled timeouts
        assert!(config.validate().is_ok());
    }

    fn alice_user() -> StaticUser {
        StaticUser {
            token: "dtx_alice_secret".to_string(),
            user_id: "alice".to_string(),
            role: UserRole::User,
        }
    }

    fn admin_user() -> StaticUser {
        StaticUser {
            token: "dtx_admin_secret".to_string(),
            user_id: "admin".to_string(),
            role: UserRole::Admin,
        }
    }

    #[test]
    fn test_auth_config_simple_mode_with_users() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: vec![alice_user(), admin_user()],
            ..Default::default()
        };
        assert!(config.validate().is_ok());
        assert!(config.is_enabled());
    }

    #[test]
    fn test_auth_config_simple_mode_no_users() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: vec![],
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_auth_config_simple_mode_empty_token() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: vec![StaticUser {
                token: "".to_string(),
                user_id: "alice".to_string(),
                role: UserRole::User,
            }],
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_auth_config_simple_mode_empty_user_id() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: vec![StaticUser {
                token: "dtx_token".to_string(),
                user_id: "".to_string(),
                role: UserRole::User,
            }],
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_auth_config_find_user_by_token() {
        let config = AuthConfig {
            mode: Some(AuthMode::Simple),
            users: vec![alice_user(), admin_user()],
            ..Default::default()
        };
        let found = config.find_user_by_token("dtx_alice_secret");
        assert!(found.is_some());
        assert_eq!(found.unwrap().user_id, "alice");
        assert_eq!(found.unwrap().role, UserRole::User);

        let admin = config.find_user_by_token("dtx_admin_secret");
        assert!(admin.is_some());
        assert_eq!(admin.unwrap().role, UserRole::Admin);

        assert!(config.find_user_by_token("unknown").is_none());
    }

    #[test]
    fn test_auth_config_external_mode() {
        let config = AuthConfig {
            mode: Some(AuthMode::External),
            jwt: JwtConfig {
                jwks_url: Some("https://auth.example.com/.well-known/jwks.json".to_string()),
                issuer: Some("https://auth.example.com".to_string()),
                audience: Some("detrix".to_string()),
                cache_ttl_seconds: 300,
                admin_role_claim: Some("roles".to_string()),
                admin_role_value: Some("detrix-admin".to_string()),
            },
            ..Default::default()
        };
        assert!(config.validate().is_ok());
        assert!(config.is_enabled());
        assert_eq!(config.jwt.admin_role_claim, Some("roles".to_string()));
        assert_eq!(
            config.jwt.admin_role_value,
            Some("detrix-admin".to_string())
        );
    }

    #[test]
    fn test_auth_config_external_mode_missing_jwks() {
        let config = AuthConfig {
            mode: Some(AuthMode::External),
            jwt: JwtConfig::default(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_auth_config_default_mode_is_none() {
        let config = AuthConfig::default();
        assert_eq!(config.mode, None);
        assert_eq!(config.effective_mode(), AuthMode::Disabled);
        assert!(!config.is_enabled());
        assert!(config.validate().is_ok());
        assert!(config.users.is_empty());
    }

    #[test]
    fn test_auth_config_explicit_disabled_vs_none() {
        let none_config = AuthConfig::default();
        let disabled_config = AuthConfig {
            mode: Some(AuthMode::Disabled),
            ..Default::default()
        };

        assert_eq!(
            none_config.effective_mode(),
            disabled_config.effective_mode()
        );
        assert_eq!(none_config.is_enabled(), disabled_config.is_enabled());

        assert_eq!(none_config.mode, None);
        assert_eq!(disabled_config.mode, Some(AuthMode::Disabled));
    }

    #[test]
    fn test_auth_config_toml_parsing_absent_mode() {
        // Empty TOML yields None mode
        let config: AuthConfig = toml::from_str("").expect("Failed to parse empty TOML");
        assert_eq!(config.mode, None);
        assert!(!config.is_enabled());
    }

    #[test]
    fn test_rate_limit_config_validate() {
        let config = RateLimitConfig::default();
        assert!(config.validate().is_ok());

        let zero_rate = RateLimitConfig {
            enabled: true,
            per_second: 0,
            ..Default::default()
        };
        assert!(zero_rate.validate().is_err());
    }

    #[test]
    fn test_auth_config_toml_parsing_simple_with_users() {
        let config_toml = r#"
mode = "simple"

[[users]]
token = "dtx_alice_3f9a"
user_id = "alice"
role = "user"

[[users]]
token = "dtx_admin_7c2b"
user_id = "admin"
role = "admin"
"#;

        let config: AuthConfig = toml::from_str(config_toml).expect("Failed to parse TOML");

        assert_eq!(config.mode, Some(AuthMode::Simple));
        assert_eq!(config.users.len(), 2);
        assert_eq!(config.users[0].user_id, "alice");
        assert_eq!(config.users[0].role, UserRole::User);
        assert_eq!(config.users[1].user_id, "admin");
        assert_eq!(config.users[1].role, UserRole::Admin);
        assert!(config.is_enabled());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_auth_config_toml_parsing_disabled() {
        let config_toml = r#"
mode = "disabled"
"#;

        let config: AuthConfig = toml::from_str(config_toml).expect("Failed to parse TOML");

        assert_eq!(config.mode, Some(AuthMode::Disabled));
        assert!(!config.is_enabled());
    }

    #[test]
    fn test_auth_config_toml_parsing_external() {
        let config_toml = r#"
mode = "external"

[jwt]
jwks_url = "https://auth.example.com/.well-known/jwks.json"
issuer = "https://auth.example.com"
admin_role_claim = "roles"
admin_role_value = "detrix-admin"
"#;

        let config: AuthConfig = toml::from_str(config_toml).expect("Failed to parse TOML");

        assert_eq!(config.mode, Some(AuthMode::External));
        assert!(config.is_enabled());
        assert_eq!(
            config.jwt.jwks_url,
            Some("https://auth.example.com/.well-known/jwks.json".to_string())
        );
        assert_eq!(config.jwt.admin_role_claim, Some("roles".to_string()));
        assert_eq!(
            config.jwt.admin_role_value,
            Some("detrix-admin".to_string())
        );
    }

    // ========================================================================
    // CORS Config Tests
    // ========================================================================

    #[test]
    fn test_cors_config_default() {
        let config = CorsConfig::default();
        assert!(!config.allow_all);
        assert!(config.allow_credentials);
        assert_eq!(config.max_age_seconds, 3600);
        assert!(config
            .allowed_origins
            .contains(&"http://localhost".to_string()));
        assert!(config
            .allowed_origins
            .contains(&"http://127.0.0.1".to_string()));
    }

    #[test]
    fn test_cors_config_exact_match() {
        let config = CorsConfig {
            allowed_origins: vec!["http://localhost:3000".to_string()],
            ..Default::default()
        };

        assert!(config.is_origin_allowed("http://localhost:3000"));
        assert!(!config.is_origin_allowed("http://localhost:4000"));
        assert!(!config.is_origin_allowed("http://example.com"));
    }

    #[test]
    fn test_cors_config_wildcard_port() {
        let config = CorsConfig {
            allowed_origins: vec!["http://localhost:*".to_string()],
            ..Default::default()
        };

        assert!(config.is_origin_allowed("http://localhost:3000"));
        assert!(config.is_origin_allowed("http://localhost:8080"));
        assert!(config.is_origin_allowed("http://localhost:443"));
        assert!(!config.is_origin_allowed("http://example.com:3000"));
    }

    #[test]
    fn test_cors_config_wildcard_subdomain() {
        let config = CorsConfig {
            allowed_origins: vec!["https://*.example.com".to_string()],
            ..Default::default()
        };

        assert!(config.is_origin_allowed("https://api.example.com"));
        assert!(config.is_origin_allowed("https://www.example.com"));
        assert!(!config.is_origin_allowed("https://example.com"));
        assert!(!config.is_origin_allowed("https://api.other.com"));
    }

    #[test]
    fn test_cors_config_allow_all() {
        let config = CorsConfig {
            allow_all: true,
            ..Default::default()
        };

        assert!(config.is_origin_allowed("http://anything.com"));
        assert!(config.is_origin_allowed("https://anywhere.org"));
    }

    #[test]
    fn test_cors_config_toml_parsing() {
        let config_toml = r#"
allow_all = false
allowed_origins = ["http://localhost:3000", "https://*.myapp.com"]
allow_credentials = true
max_age_seconds = 7200
"#;

        let config: CorsConfig = toml::from_str(config_toml).expect("Failed to parse TOML");

        assert!(!config.allow_all);
        assert!(config.allow_credentials);
        assert_eq!(config.max_age_seconds, 7200);
        assert_eq!(config.allowed_origins.len(), 2);
        assert!(config.is_origin_allowed("http://localhost:3000"));
        assert!(config.is_origin_allowed("https://api.myapp.com"));
    }

    #[test]
    fn test_cors_config_validate_default_is_valid() {
        let config = CorsConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_cors_config_validate_allow_all_without_credentials_is_valid() {
        let config = CorsConfig {
            allow_all: true,
            allow_credentials: false,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_cors_config_validate_allow_all_with_credentials_is_invalid() {
        let config = CorsConfig {
            allow_all: true,
            allow_credentials: true,
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("allow_all"));
    }
}
