// gRPC service implementations

pub mod auth;
pub mod client;
pub mod connections;
pub mod conversions;
pub mod interceptor;
pub mod metrics;
pub mod streaming;

pub use client::{
    build_grpc_endpoint, connect_to_daemon_grpc, read_auth_token, AuthChannel, AuthInterceptor,
    DaemonConnectionError, DaemonEndpoints, EndpointDiscoveryMethod,
};
pub use connections::ConnectionServiceImpl;
pub use conversions::{
    proto_mode_to_string, proto_to_core_connection, proto_to_core_event,
    proto_to_core_memory_snapshot, proto_to_core_metric, proto_to_core_stack_trace,
    ConversionError,
};
pub use interceptor::{create_auth_interceptor, AuthInterceptorState};
pub use metrics::MetricsServiceImpl;
pub use streaming::StreamingServiceImpl;

pub use auth::{AUTHORIZATION_METADATA_KEY, BEARER_PREFIX};
