// gRPC service implementations

pub mod client;
pub mod connections;
pub mod conversions;
pub mod interceptor;
pub mod metrics;
pub mod streaming;

pub use client::{
    build_grpc_endpoint, connect_to_daemon_grpc, read_auth_token, request_with_machine_client_id,
    AuthChannel, AuthInterceptor, DaemonConnectionError, DaemonEndpoints, EndpointDiscoveryMethod,
};
pub use connections::ConnectionServiceImpl;
pub use conversions::{
    parse_mode_string_to_proto, proto_mode_to_string, proto_to_core_connection,
    proto_to_core_event, proto_to_core_memory_snapshot, proto_to_core_metric,
    proto_to_core_stack_trace, ConversionError,
};
pub use interceptor::{create_auth_interceptor, AuthInterceptorState};
pub use metrics::MetricsServiceImpl;
pub use streaming::StreamingServiceImpl;

/// Extract AuthenticatedUser from gRPC request extensions.
/// Always present — the interceptor injects a default Admin when auth is disabled.
pub(crate) fn extract_user<T>(
    request: &tonic::Request<T>,
) -> Result<interceptor::AuthenticatedUser, tonic::Status> {
    request
        .extensions()
        .get::<interceptor::AuthenticatedUser>()
        .cloned()
        .ok_or_else(|| tonic::Status::unauthenticated("Missing authentication"))
}

/// Extract and validate client_id from `x-detrix-client-id` gRPC metadata.
///
/// Returns `Ok(None)` if the metadata key is absent (backwards compatible).
/// Returns `Ok(Some(id))` if the key is present and valid.
/// Returns `Err(Status)` if the key value is present but invalid.
///
/// **CRITICAL:** Must be called BEFORE `request.into_inner()` which consumes the Request.
pub(crate) fn extract_client_id<T>(
    request: &tonic::Request<T>,
) -> Result<Option<String>, tonic::Status> {
    match request
        .metadata()
        .get(crate::common::CLIENT_ID_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        Some(value) => crate::common::validate_client_id(value)
            .map(Some)
            .map_err(tonic::Status::invalid_argument),
        None => Ok(None),
    }
}
