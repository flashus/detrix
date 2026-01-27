//! HTTP route configuration
//!
//! Wires up handlers into an axum Router.

use super::mcp::{mcp_disconnect_handler, mcp_handler, mcp_heartbeat_handler};
use thiserror::Error;

// ============================================================================
// Router Configuration Error
// ============================================================================

/// Error type for router configuration failures
///
/// This error is returned when router creation fails due to invalid configuration.
/// Replaces panics with proper error propagation.
#[derive(Debug, Error)]
pub enum RouterConfigError {
    #[error("Invalid auth configuration: {0}")]
    Auth(String),

    #[error("Invalid rate limit configuration: {0}")]
    RateLimit(String),

    #[error("Failed to build governor configuration: {0}")]
    Governor(String),
}
use super::middleware::{auth_middleware, AuthState};
use super::paths;
use super::websocket::websocket_handler;
use crate::http::handlers::{
    add_metric, cleanup_connections, close_connection, create_connection, delete_metric,
    disable_group, disable_metric, enable_group, enable_metric, get_config, get_connection,
    get_mcp_usage, get_metric, get_metric_history, get_metric_value, health_check, inspect_file,
    list_connections, list_group_metrics, list_groups, list_metrics, prometheus_metrics,
    query_events, reload_config, sleep, status, update_config, update_metric, validate_config,
    validate_expression, wake,
};
use crate::state::ApiState;
use axum::{
    body::Body,
    extract::ConnectInfo,
    http::Request,
    middleware,
    response::Response,
    routing::{get, post},
    Router,
};
use detrix_application::JwksValidator;
use detrix_config::{AuthConfig, CorsConfig, RateLimitConfig};
use std::{
    convert::Infallible,
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Layer, Service};
use tower_governor::{
    governor::GovernorConfigBuilder,
    key_extractor::{KeyExtractor, SmartIpKeyExtractor},
    GovernorError, GovernorLayer,
};
use tower_http::{
    cors::{AllowOrigin, Any, CorsLayer},
    trace::TraceLayer,
};

// ============================================================================
// Type Aliases for Cleaner Service Definitions
// ============================================================================

/// HTTP request type used throughout the router
type HttpRequest = Request<Body>;

/// Boxed future that returns an HTTP response (used by tower services)
type ResponseFuture = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

/// Check if an IP address is localhost (loopback)
fn is_localhost(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => ipv4.is_loopback(),
        IpAddr::V6(ipv6) => ipv6.is_loopback(),
    }
}

/// Check if a request is from localhost
fn request_is_from_localhost<T>(req: &Request<T>) -> bool {
    // First, try to get the IP from ConnectInfo (direct connection)
    if let Some(connect_info) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return is_localhost(connect_info.0.ip());
    }

    // Fall back to SmartIpKeyExtractor for proxied requests
    let inner = SmartIpKeyExtractor;
    if let Ok(ip) = inner.extract(req) {
        return is_localhost(ip);
    }

    false
}

/// Simple IP-based key extractor for rate limiting
#[derive(Debug, Clone, Copy, Default)]
pub struct IpKeyExtractor;

impl KeyExtractor for IpKeyExtractor {
    type Key = IpAddr;

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        // First, try to get the IP from ConnectInfo (direct connection)
        if let Some(connect_info) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
            return Ok(connect_info.0.ip());
        }

        // Fall back to SmartIpKeyExtractor for proxied requests
        let inner = SmartIpKeyExtractor;
        inner.extract(req)
    }
}

// ============================================================================
// Conditional Rate Limiting Layer
// ============================================================================

/// Layer that conditionally applies rate limiting based on request source
///
/// When `localhost_exempt` is true, localhost requests bypass rate limiting entirely.
/// Remote requests are still rate limited normally.
#[derive(Clone)]
pub struct ConditionalRateLimitLayer<L> {
    inner_layer: L,
    localhost_exempt: bool,
}

impl<L> ConditionalRateLimitLayer<L> {
    pub fn new(inner_layer: L, localhost_exempt: bool) -> Self {
        Self {
            inner_layer,
            localhost_exempt,
        }
    }
}

impl<S, L> Layer<S> for ConditionalRateLimitLayer<L>
where
    L: Layer<S>,
    L::Service: Clone,
    S: Clone,
{
    type Service = ConditionalRateLimitService<S, L::Service>;

    fn layer(&self, service: S) -> Self::Service {
        ConditionalRateLimitService {
            plain_service: service.clone(),
            rate_limited_service: self.inner_layer.layer(service),
            localhost_exempt: self.localhost_exempt,
        }
    }
}

/// Service that conditionally applies rate limiting
///
/// For localhost requests (when localhost_exempt is true), bypasses rate limiting.
/// For remote requests, applies rate limiting via the inner governor service.
#[derive(Clone)]
pub struct ConditionalRateLimitService<S, R> {
    plain_service: S,
    rate_limited_service: R,
    localhost_exempt: bool,
}

/// Trait alias for HTTP services compatible with axum routers
///
/// This simplifies the verbose trait bounds needed for tower services.
/// Services must be cloneable, sendable, and return infallible responses.
trait HttpService:
    Service<HttpRequest, Response = Response, Error = Infallible, Future: Send> + Clone + Send + 'static
{
}

impl<T> HttpService for T
where
    T: Service<HttpRequest, Response = Response, Error = Infallible> + Clone + Send + 'static,
    T::Future: Send,
{
}

impl<S, R> Service<HttpRequest> for ConditionalRateLimitService<S, R>
where
    S: HttpService,
    R: HttpService,
{
    type Response = Response;
    type Error = Infallible;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.plain_service.poll_ready(cx)
    }

    fn call(&mut self, req: HttpRequest) -> Self::Future {
        if self.localhost_exempt && request_is_from_localhost(&req) {
            // Localhost: bypass rate limiting
            Box::pin(self.plain_service.call(req))
        } else {
            // Remote: apply rate limiting
            let mut service = self.rate_limited_service.clone();
            Box::pin(async move { service.call(req).await })
        }
    }
}

/// Create the HTTP router with all routes (default config)
///
/// # Errors
///
/// Returns `RouterConfigError` if configuration validation fails.
pub fn create_router(state: Arc<ApiState>) -> Result<Router, RouterConfigError> {
    create_router_with_config(
        state,
        &RateLimitConfig::default(),
        &AuthConfig::default(),
        &CorsConfig::default(),
    )
}

/// Create the HTTP router with custom rate limit configuration (backwards compatible)
///
/// # Errors
///
/// Returns `RouterConfigError` if configuration validation fails.
pub fn create_router_with_rate_limit(
    state: Arc<ApiState>,
    rate_limit: &RateLimitConfig,
) -> Result<Router, RouterConfigError> {
    create_router_with_config(
        state,
        rate_limit,
        &AuthConfig::default(),
        &CorsConfig::default(),
    )
}

/// Create the HTTP router with full configuration
///
/// # Errors
///
/// Returns `RouterConfigError` if configuration validation fails.
pub fn create_router_with_config(
    state: Arc<ApiState>,
    rate_limit: &RateLimitConfig,
    auth: &AuthConfig,
    cors: &CorsConfig,
) -> Result<Router, RouterConfigError> {
    create_router_with_jwt_validator(state, rate_limit, auth, cors, None)
}

/// Create the HTTP router with full configuration and optional JWT validator
///
/// This is the most complete constructor that supports external JWT authentication.
/// The jwt_validator should be provided when auth.mode is External.
///
/// # Errors
///
/// Returns `RouterConfigError` if:
/// - Auth configuration is invalid (when auth is enabled)
/// - Rate limit configuration is invalid (when rate limiting is enabled)
/// - Governor config fails to build
pub fn create_router_with_jwt_validator(
    state: Arc<ApiState>,
    rate_limit: &RateLimitConfig,
    auth: &AuthConfig,
    cors_config: &CorsConfig,
    jwt_validator: Option<JwksValidator>,
) -> Result<Router, RouterConfigError> {
    use tracing::info;

    info!("Creating HTTP router with all routes");

    // Build router first
    let router = Router::new()
        // Health check
        .route(paths::HEALTH, get(health_check))
        // System status (public endpoint)
        .route(paths::STATUS, get(status))
        // Prometheus metrics endpoint (Prometheus format)
        .route(paths::PROMETHEUS_METRICS, get(prometheus_metrics))
        // System control endpoints
        .route(paths::API_V1_WAKE, post(wake))
        .route(paths::API_V1_SLEEP, post(sleep))
        .route(paths::API_V1_STATUS, get(status))
        // REST API v1 - Metrics
        .route(paths::API_V1_METRICS, get(list_metrics).post(add_metric))
        .route(
            paths::API_V1_METRIC_BY_ID,
            get(get_metric).put(update_metric).delete(delete_metric),
        )
        .route(paths::API_V1_METRIC_ENABLE, post(enable_metric))
        .route(paths::API_V1_METRIC_DISABLE, post(disable_metric))
        .route(paths::API_V1_METRIC_VALUE, get(get_metric_value))
        .route(paths::API_V1_METRIC_HISTORY, get(get_metric_history))
        // REST API v1 - Events
        .route(paths::API_V1_EVENTS, get(query_events))
        // REST API v1 - Groups
        .route(paths::API_V1_GROUPS, get(list_groups))
        .route(paths::API_V1_GROUP_METRICS, get(list_group_metrics))
        .route(paths::API_V1_GROUP_ENABLE, post(enable_group))
        .route(paths::API_V1_GROUP_DISABLE, post(disable_group))
        // Connection management REST API
        .route(
            paths::API_V1_CONNECTIONS,
            get(list_connections).post(create_connection),
        )
        .route(
            paths::API_V1_CONNECTION_BY_ID,
            get(get_connection).delete(close_connection),
        )
        .route(paths::API_V1_CONNECTIONS_CLEANUP, post(cleanup_connections))
        // Config management REST API
        .route(paths::API_V1_CONFIG, get(get_config).put(update_config))
        .route(paths::API_V1_CONFIG_RELOAD, post(reload_config))
        .route(paths::API_V1_CONFIG_VALIDATE, post(validate_config))
        // Diagnostic endpoints
        .route(paths::API_V1_VALIDATE_EXPRESSION, post(validate_expression))
        .route(paths::API_V1_INSPECT_FILE, post(inspect_file))
        // MCP usage statistics
        .route(paths::API_V1_MCP_USAGE, get(get_mcp_usage))
        // WebSocket endpoint for real-time events
        .route(paths::WS, get(websocket_handler))
        // MCP HTTP endpoint for JSON-RPC over HTTP
        .route(paths::MCP, post(mcp_handler))
        // MCP heartbeat endpoint for client keep-alive
        .route(paths::MCP_HEARTBEAT, post(mcp_heartbeat_handler))
        // MCP disconnect endpoint for graceful client shutdown
        .route(paths::MCP_DISCONNECT, post(mcp_disconnect_handler));

    info!("Router configured with REST API v1 endpoints");

    // CORS configuration from config
    // See: https://developer.mozilla.org/en-US/docs/Web/HTTP/CORS
    let cors = build_cors_layer(cors_config);
    info!(
        allow_all = cors_config.allow_all,
        origins = ?cors_config.allowed_origins,
        "CORS configured"
    );

    // Auth middleware state
    let auth_state = match jwt_validator {
        Some(validator) => AuthState::with_jwt_validator(auth.clone(), validator),
        None => AuthState::new(auth.clone()),
    };

    if auth.is_enabled() {
        // Validate auth configuration before starting
        auth.validate()
            .map_err(|e| RouterConfigError::Auth(e.to_string()))?;
        info!(
            mode = ?auth.mode,
            public_endpoints = ?auth.public_endpoints,
            "API authentication enabled"
        );
    } else {
        info!("API authentication disabled (all endpoints public)");
    }

    // Apply state and middleware
    // Note: Order matters - auth should run after rate limiting
    // Rate limiting first to prevent auth from being DoS'd
    let router = if rate_limit.enabled {
        // Validate rate limiting configuration before building governor config
        rate_limit
            .validate()
            .map_err(|e| RouterConfigError::RateLimit(e.to_string()))?;

        let localhost_exempt = rate_limit.localhost_exempt;
        info!(
            per_second = rate_limit.per_second,
            burst_size = rate_limit.burst_size,
            localhost_exempt = localhost_exempt,
            "Rate limiting enabled"
        );

        // Rate limiting configuration from config
        // Prevents DoS attacks by limiting requests per client
        let key_extractor = IpKeyExtractor;
        let governor_config = Arc::new(
            GovernorConfigBuilder::default()
                .per_second(rate_limit.per_second)
                .burst_size(rate_limit.burst_size)
                .key_extractor(key_extractor)
                .finish()
                .ok_or_else(|| {
                    RouterConfigError::Governor(
                        "Failed to build governor config with provided settings".to_string(),
                    )
                })?,
        );

        // Use conditional rate limiting layer that bypasses rate limiting for localhost
        // when localhost_exempt is true
        let governor_layer = GovernorLayer::new(governor_config);
        let conditional_rate_limit =
            ConditionalRateLimitLayer::new(governor_layer, localhost_exempt);

        router
            .with_state(state)
            .layer(middleware::from_fn_with_state(auth_state, auth_middleware))
            .layer(conditional_rate_limit)
            .layer(cors)
            .layer(TraceLayer::new_for_http())
    } else {
        info!("Rate limiting disabled");
        router
            .with_state(state)
            .layer(middleware::from_fn_with_state(auth_state, auth_middleware))
            .layer(cors)
            .layer(TraceLayer::new_for_http())
    };

    info!("Router configured with state and middleware");

    Ok(router)
}

/// Build CORS layer from configuration
fn build_cors_layer(config: &CorsConfig) -> CorsLayer {
    use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
    use axum::http::HeaderValue;
    use axum::http::Method;
    use std::time::Duration;

    // When allow_credentials is true, we cannot use Any for methods/origins
    // This is a CORS security requirement enforced by tower-http
    let cors = if config.allow_credentials {
        CorsLayer::new().allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::OPTIONS,
        ])
    } else {
        CorsLayer::new().allow_methods(Any)
    };

    let mut cors = cors
        .allow_headers([AUTHORIZATION, CONTENT_TYPE])
        .max_age(Duration::from_secs(config.max_age_seconds));

    // Configure allowed origins
    if config.allow_all {
        cors = cors.allow_origin(Any);
    } else {
        // Build list of allowed origins
        // Note: For wildcard patterns, we need to use a predicate
        let has_wildcards = config.allowed_origins.iter().any(|o| o.contains('*'));

        if has_wildcards {
            // Use predicate for wildcard matching
            let allowed_origins = config.allowed_origins.clone();
            cors = cors.allow_origin(AllowOrigin::predicate(move |origin, _| {
                let origin_str = origin.to_str().unwrap_or("");
                for pattern in &allowed_origins {
                    if CorsConfig::matches_origin_pattern(pattern, origin_str) {
                        return true;
                    }
                }
                false
            }));
        } else {
            // Use exact match list for better performance
            let origins: Vec<HeaderValue> = config
                .allowed_origins
                .iter()
                .filter_map(|o| o.parse().ok())
                .collect();
            cors = cors.allow_origin(origins);
        }
    }

    // Configure credentials
    if config.allow_credentials {
        cors = cors.allow_credentials(true);
    }

    cors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    // ========================================================================
    // is_localhost tests
    // ========================================================================

    #[test]
    fn test_is_localhost_ipv4_loopback() {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(is_localhost(ip));
    }

    #[test]
    fn test_is_localhost_ipv4_127_0_0_1() {
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert!(is_localhost(ip));
    }

    #[test]
    fn test_is_localhost_ipv4_127_x_x_x() {
        // All 127.x.x.x addresses are loopback
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
        assert!(is_localhost(ip));

        let ip = IpAddr::V4(Ipv4Addr::new(127, 255, 255, 255));
        assert!(is_localhost(ip));
    }

    #[test]
    fn test_is_localhost_ipv6_loopback() {
        let ip = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert!(is_localhost(ip));
    }

    #[test]
    fn test_is_localhost_ipv6_1() {
        let ip = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1));
        assert!(is_localhost(ip));
    }

    #[test]
    fn test_is_not_localhost_ipv4_remote() {
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        assert!(!is_localhost(ip));

        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        assert!(!is_localhost(ip));

        let ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        assert!(!is_localhost(ip));
    }

    #[test]
    fn test_is_not_localhost_ipv6_remote() {
        let ip = IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888));
        assert!(!is_localhost(ip));
    }

    #[test]
    fn test_is_not_localhost_ipv4_unspecified() {
        let ip = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        assert!(!is_localhost(ip));
    }

    // ========================================================================
    // request_is_from_localhost tests
    // ========================================================================

    #[test]
    fn test_request_is_from_localhost_with_connect_info() {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        // Add localhost ConnectInfo
        let socket_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(socket_addr));

        assert!(request_is_from_localhost(&req));
    }

    #[test]
    fn test_request_is_from_localhost_ipv6() {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        // Add IPv6 localhost ConnectInfo
        let socket_addr: SocketAddr = "[::1]:12345".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(socket_addr));

        assert!(request_is_from_localhost(&req));
    }

    #[test]
    fn test_request_is_not_from_localhost_remote_ip() {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        // Add remote IP ConnectInfo
        let socket_addr: SocketAddr = "192.168.1.100:12345".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(socket_addr));

        assert!(!request_is_from_localhost(&req));
    }

    #[test]
    fn test_request_is_not_from_localhost_public_ip() {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        // Add public IP ConnectInfo
        let socket_addr: SocketAddr = "8.8.8.8:12345".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(socket_addr));

        assert!(!request_is_from_localhost(&req));
    }

    #[test]
    fn test_request_is_from_localhost_no_connect_info() {
        let req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        // No ConnectInfo extension - should return false
        assert!(!request_is_from_localhost(&req));
    }

    // ========================================================================
    // IpKeyExtractor tests
    // ========================================================================

    #[test]
    fn test_ip_key_extractor_extracts_ipv4() {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let socket_addr: SocketAddr = "192.168.1.100:12345".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(socket_addr));

        let extractor = IpKeyExtractor;
        let key = extractor.extract(&req).unwrap();

        assert_eq!(key, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
    }

    #[test]
    fn test_ip_key_extractor_extracts_ipv6() {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let socket_addr: SocketAddr = "[2001:db8::1]:12345".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(socket_addr));

        let extractor = IpKeyExtractor;
        let key = extractor.extract(&req).unwrap();

        assert_eq!(key, IpAddr::V6("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn test_ip_key_extractor_extracts_localhost() {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

        let socket_addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(socket_addr));

        let extractor = IpKeyExtractor;
        let key = extractor.extract(&req).unwrap();

        assert_eq!(key, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    // ========================================================================
    // ConditionalRateLimitLayer tests
    // ========================================================================

    #[test]
    fn test_conditional_rate_limit_layer_creation() {
        // Test that the layer can be created with both settings
        let _layer_exempt =
            ConditionalRateLimitLayer::new(tower::layer::util::Identity::new(), true);
        let _layer_not_exempt =
            ConditionalRateLimitLayer::new(tower::layer::util::Identity::new(), false);
    }

    // ========================================================================
    // Integration-style tests for rate limiting logic
    // ========================================================================

    /// Test helper to create a request with a specific IP
    fn create_request_with_ip(ip: &str, port: u16) -> HttpRequest {
        let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();
        let socket_addr: SocketAddr = format!("{}:{}", ip, port).parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(socket_addr));
        req
    }

    #[test]
    fn test_localhost_detection_various_addresses() {
        // All these should be detected as localhost
        let localhost_addresses = vec!["127.0.0.1", "127.0.0.2", "127.255.255.255", "[::1]"];

        for addr in localhost_addresses {
            let req = create_request_with_ip(addr, 12345);
            assert!(
                request_is_from_localhost(&req),
                "Expected {} to be detected as localhost",
                addr
            );
        }
    }

    #[test]
    fn test_remote_detection_various_addresses() {
        // None of these should be detected as localhost
        let remote_addresses = vec![
            "192.168.1.1",
            "10.0.0.1",
            "172.16.0.1",
            "8.8.8.8",
            "1.1.1.1",
            "203.0.113.1",
            "[2001:db8::1]",
            "[fe80::1]",
        ];

        for addr in remote_addresses {
            let req = create_request_with_ip(addr, 12345);
            assert!(
                !request_is_from_localhost(&req),
                "Expected {} to NOT be detected as localhost",
                addr
            );
        }
    }

    #[test]
    fn test_localhost_exempt_flag_behavior() {
        // When localhost_exempt is true, localhost requests should bypass rate limiting
        // When localhost_exempt is false, all requests should be rate limited

        // This is tested by checking that the ConditionalRateLimitService
        // correctly identifies localhost requests based on the flag

        let localhost_req = create_request_with_ip("127.0.0.1", 12345);
        let remote_req = create_request_with_ip("192.168.1.100", 12345);

        // Test localhost detection
        assert!(request_is_from_localhost(&localhost_req));
        assert!(!request_is_from_localhost(&remote_req));

        // The actual rate limiting behavior is tested in the E2E tests
        // because it requires the full tower service stack
    }

    #[test]
    fn test_different_ports_same_ip_detected_correctly() {
        // Different ports from the same IP should be detected the same way
        for port in [80, 443, 8080, 50051, 65535] {
            let localhost_req = create_request_with_ip("127.0.0.1", port);
            assert!(
                request_is_from_localhost(&localhost_req),
                "Port {} should not affect localhost detection",
                port
            );

            let remote_req = create_request_with_ip("8.8.8.8", port);
            assert!(
                !request_is_from_localhost(&remote_req),
                "Port {} should not affect remote detection",
                port
            );
        }
    }
}
