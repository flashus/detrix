//! HTTP REST handlers
//!
//! Following Clean Architecture:
//! - Parse/validate incoming DTOs (JSON)
//! - Convert DTOs to domain types
//! - Call application services
//! - Convert results to JSON responses
//!
//! NO business logic in handlers!
//!
//! This module uses proto-generated types for responses where possible to prevent
//! sync issues. Proto types have serde support (via build.rs) and use camelCase.
//!
//! When custom request DTOs are needed (e.g., REST-specific defaults), they should
//! be documented and conversions should use exhaustive destructuring.

// Handler modules organized by domain
mod config;
mod connections;
mod diagnostic;
mod groups;
mod health;
mod lifecycle;
mod metrics;
mod operations;
mod values;
pub use config::{
    get_config, reload_config, update_config, validate_config, ConfigResponse, GetConfigQuery,
    UpdateConfigRequest, UpdateConfigResponse,
};
pub use connections::{
    cleanup_connections, close_connection, create_connection, get_connection, list_connections,
    CleanupResponse, ListConnectionsQuery,
};
pub use diagnostic::{
    get_mcp_usage, inspect_file, validate_expression, CodeLineDto, InspectFileResponse,
    VariableDefinitionDto,
};
pub use groups::{
    disable_group, enable_group, list_group_metrics, list_groups, GroupOperationResponse,
};
pub use health::{health_check, prometheus_metrics, HealthResponse};
pub use lifecycle::{sleep, status, wake, StatusResponse, WakeResponse};
pub use metrics::{
    add_metric, delete_metric, get_metric, list_metrics, query_events, CreateMetricRequest,
    CreateMetricResponse, ListMetricsQuery, PaginatedMetricsResponse, QueryEventsParams,
    StackTraceSliceDto,
};
pub use operations::{disable_metric, enable_metric, update_metric, UpdateMetricRequest};
pub use values::{
    get_metric_history, get_metric_value, MetricHistoryParams, MetricHistoryResponse,
    MetricValueResponse,
};

// ============================================================================
// Shared types and helpers used across handlers
// ============================================================================

use crate::grpc::conversions::{connection_to_info, metric_to_info};
use crate::types::{ConnectionInfo, MetricInfo};
use detrix_core::{
    Connection, Metric, MetricMode, SafetyLevel, MODE_FIRST, MODE_SAMPLE, MODE_SAMPLE_INTERVAL,
    MODE_STREAM, MODE_THROTTLE, SAFETY_STRICT, SAFETY_TRUSTED,
};

/// Convert domain Metric to proto MetricInfo for REST responses
///
/// Returns `Result` - caller decides how to handle conversion errors.
pub(crate) fn metric_to_rest_response(
    m: &Metric,
) -> Result<MetricInfo, crate::grpc::conversions::ConversionError> {
    metric_to_info(m)
}

/// Convert domain Connection to proto ConnectionInfo for REST responses
pub(crate) fn connection_to_rest_response(c: &Connection) -> ConnectionInfo {
    connection_to_info(c)
}

/// Default mode value for REST requests
pub(crate) fn default_mode() -> String {
    MODE_STREAM.to_string()
}

/// Parse mode string to MetricMode
pub(crate) fn parse_metric_mode(mode: &str) -> Result<MetricMode, String> {
    use detrix_config::{DEFAULT_MAX_PER_SECOND, DEFAULT_SAMPLE_INTERVAL_SECONDS};
    match mode.to_lowercase().as_str() {
        MODE_STREAM => Ok(MetricMode::Stream),
        MODE_FIRST => Ok(MetricMode::First),
        MODE_THROTTLE => Ok(MetricMode::Throttle {
            max_per_second: DEFAULT_MAX_PER_SECOND,
        }),
        MODE_SAMPLE | MODE_SAMPLE_INTERVAL => Ok(MetricMode::SampleInterval {
            seconds: DEFAULT_SAMPLE_INTERVAL_SECONDS,
        }),
        _ => Err(format!(
            "Invalid mode '{}'. Valid modes: {}, {}, {}, {}",
            mode, MODE_STREAM, MODE_FIRST, MODE_THROTTLE, MODE_SAMPLE
        )),
    }
}

/// Parse safety level string - returns error for invalid values
pub(crate) fn parse_safety_level(level: &str) -> Result<SafetyLevel, String> {
    level.parse().map_err(|_| {
        format!(
            "Invalid safety level '{}'. Valid values: {}, {}",
            level, SAFETY_STRICT, SAFETY_TRUSTED
        )
    })
}
