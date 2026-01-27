//! Shared API types from .proto
//!
//! This module re-exports proto-generated types with serde support for use
//! across all API layers (gRPC, REST, WebSocket, MCP).
//!
//! # Architecture
//!
//! Proto files (`proto/*.proto`) define the canonical data contracts:
//! - **gRPC**: Uses proto types directly via tonic
//! - **REST**: Can use proto types directly (JSON via serde, camelCase)
//! - **WebSocket**: Can use proto types directly (JSON via serde)
//! - **MCP Params**: Custom types with `schemars::JsonSchema` for LLM tool schemas
//! - **MCP Responses**: Use TOON/JSON format with proto-compatible structures
//!
//! # Why MCP Params Are Different
//!
//! MCP parameter types (`AddMetricParams`, etc.) are intentionally separate because:
//! 1. LLMs need `#[schemars(description = "...")]` for tool documentation
//! 2. LLMs work better with flat structures (e.g., `mode: "stream"` vs nested `MetricMode`)
//! 3. MCP has convenience fields (`stack_trace_full`, `stack_trace_head`, `stack_trace_tail`)
//!
//! # Conversion Flow
//!
//! ```text
//! gRPC Request  ──┐
//!                 │      ┌──────────────┐      ┌───────────────┐
//! REST Request  ──┼────▶│ Proto Types  │────▶│ Core Domain   │
//!                 │      │ (wire format)│      │ (business)    │
//! MCP Request  ───┘      └──────────────┘      └───────────────┘
//! (custom params)               │                     │
//!                              ▼                    ▼
//!                       gRPC/REST/WS          Application
//!                        Response             Services
//! ```
//!
//! # Adding New Fields
//!
//! 1. Add field to `.proto` file
//! 2. Run `cargo build` to regenerate types
//! 3. Update conversion functions in `grpc/conversions.rs` (exhaustive destructuring)
//! 4. Update MCP param types in `mcp/server.rs` (exhaustive destructuring in `add_metric`)
//! 5. All gRPC/REST/WebSocket APIs automatically get the new field
//!
//! ## Exhaustive Destructuring
//!
//! Both gRPC and MCP use exhaustive destructuring to ensure compile-time safety:
//!
//! ```ignore
//! // gRPC: Core Metric → Proto MetricInfo
//! let Metric { id, name, connection_id, group, ... } = metric;
//!
//! // MCP: AddMetricParams → Core Metric
//! let AddMetricParams { name, location, expression, ... } = params;
//! ```
//!
//! If you add a new field and forget to handle it, the compiler will error.

// Re-export all proto types for convenience
pub use crate::generated::detrix::v1::{
    // Metric types
    AddMetricRequest,
    // Connection types
    CloseConnectionRequest,
    CloseConnectionResponse,
    // Config types
    ConfigResponse,
    ConnectionInfo,
    ConnectionResponse,
    ConnectionStatus,
    CreateConnectionRequest,
    CreateConnectionResponse,
    // Common types
    Empty,
    Error as ProtoError,
    GetConfigRequest,
    GetConnectionRequest,
    GetMetricRequest,
    // Streaming types
    GetValueRequest,
    // Group types
    GroupInfo,
    GroupRequest,
    GroupResponse,
    GroupStreamRequest,
    // Validation types
    InspectFileRequest,
    InspectFileResponse,
    ListActiveConnectionsRequest,
    ListConnectionsRequest,
    ListConnectionsResponse,
    ListGroupsRequest,
    ListGroupsResponse,
    ListMetricsRequest,
    ListMetricsResponse,
    Location as ProtoLocation,
    // MCP usage statistics types
    McpErrorCount,
    McpUsageSnapshot,
    McpWorkflowStats,
    MemorySnapshot,
    MetricEvent as ProtoMetricEvent,
    MetricInfo,
    MetricMode as ProtoMetricMode,
    MetricResponse,
    MetricValue,
    QueryRequest,
    QueryResponse,
    ReloadConfigRequest,
    ReloadConfigResponse,
    RemoveMetricRequest,
    RemoveMetricResponse,
    RequestMetadata,
    ResponseMetadata,
    // Status types
    SleepRequest,
    SleepResponse,
    StackFrame,
    StackTrace,
    StackTraceSlice as ProtoStackTraceSlice,
    StatusRequest,
    StatusResponse,
    StreamAllRequest,
    StreamRequest,
    TimeRange,
    ToggleMetricRequest,
    ToggleMetricResponse,
    UpdateConfigRequest,
    UpdateMetricRequest,
    ValidateConfigRequest,
    ValidateExpressionRequest,
    ValidateExpressionResponse,
    ValidationResponse,
    Variable,
    WakeRequest,
    WakeResponse,
};

// Import core types for conversions
use detrix_core::{Location, Metric, MetricEvent};

// ============================================================================
// Core -> Proto Conversions
// ============================================================================

/// Convert core Metric to proto MetricInfo
///
/// Returns `Result` - caller decides how to handle conversion errors.
///
/// IMPORTANT: Uses exhaustive destructuring - compiler will error if new fields added.
pub fn metric_to_proto(
    metric: &Metric,
) -> Result<MetricInfo, crate::grpc::conversions::ConversionError> {
    crate::grpc::conversions::metric_to_info(metric)
}

/// Convert core MetricEvent to proto MetricEvent
pub fn event_to_proto(event: &MetricEvent) -> ProtoMetricEvent {
    crate::grpc::conversions::core_event_to_proto(event)
}

/// Convert core Location to proto Location
pub fn location_to_proto(loc: &Location) -> ProtoLocation {
    ProtoLocation {
        file: loc.file.clone(),
        line: loc.line,
    }
}

// ============================================================================
// Proto -> Core Conversions
// ============================================================================

// NOTE: Proto → Core conversions are in grpc/conversions.rs.
// Use add_request_to_metric() from grpc/conversions.rs
// which is the main dto's source for all protocols (gRPC, REST, MCP).

/// Convert proto Location to core Location
pub fn proto_to_location(loc: &ProtoLocation) -> Location {
    Location {
        file: loc.file.clone(),
        line: loc.line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::detrix::v1::{metric_mode, Location, MetricMode, StreamMode};

    /// Test that proto MetricInfo correctly serializes and deserializes to/from JSON.
    /// This verifies that production types can be used in tests for response parsing.
    #[test]
    fn test_metric_info_json_round_trip() {
        let metric = MetricInfo {
            metric_id: 123,
            name: "test_metric".to_string(),
            group: Some("test_group".to_string()),
            location: Some(Location {
                file: "test.py".to_string(),
                line: 42,
            }),
            expression: "x + y".to_string(),
            language: "python".to_string(),
            enabled: true,
            mode: Some(MetricMode {
                mode: Some(metric_mode::Mode::Stream(StreamMode {})),
            }),
            created_at: 1234567890,
            hit_count: 10,
            last_hit_at: Some(1234567899),
            connection_id: "conn-1".to_string(),
            capture_stack_trace: false,
            capture_memory_snapshot: false,
        };

        // Serialize to JSON
        let json = serde_json::to_string(&metric).expect("Serialization should succeed");

        // Verify camelCase field names
        assert!(json.contains("\"metricId\":123"));
        assert!(json.contains("\"testMetric\"") || json.contains("\"name\":\"test_metric\""));
        assert!(json.contains("\"connectionId\":\"conn-1\""));
        assert!(json.contains("\"captureStackTrace\":false"));

        // Verify mode serialization - prost uses the oneof field name
        assert!(json.contains("\"mode\":{"));
        assert!(json.contains("\"stream\":{}"));

        // Deserialize back
        let parsed: MetricInfo =
            serde_json::from_str(&json).expect("Deserialization should succeed");

        // Verify round-trip
        assert_eq!(parsed.metric_id, 123);
        assert_eq!(parsed.name, "test_metric");
        assert_eq!(parsed.connection_id, "conn-1");
        assert!(parsed.mode.is_some());

        // Verify mode round-trip
        let mode = parsed.mode.as_ref().unwrap();
        assert!(matches!(mode.mode, Some(metric_mode::Mode::Stream(_))));
    }

    #[test]
    fn test_metric_info_deserialize_from_api_response() {
        // First, let's see what format prost generates for the mode field
        let metric = MetricInfo {
            metric_id: 123,
            name: "test".to_string(),
            group: None,
            location: None,
            expression: "x".to_string(),
            language: "python".to_string(),
            enabled: true,
            mode: Some(MetricMode {
                mode: Some(metric_mode::Mode::Stream(StreamMode {})),
            }),
            created_at: 0,
            hit_count: 0,
            last_hit_at: None,
            connection_id: "conn".to_string(),
            capture_stack_trace: false,
            capture_memory_snapshot: false,
        };

        // Print the actual serialized format
        let serialized = serde_json::to_string_pretty(&metric).unwrap();
        println!("Prost serialized format:\n{}", serialized);

        // Now parse using the ACTUAL format prost uses
        let parsed: MetricInfo =
            serde_json::from_str(&serialized).expect("Should parse prost format");

        // Verify mode parsed correctly
        let mode = parsed.mode.expect("Mode should be present");
        println!("Parsed mode: {:?}", mode);
        assert!(matches!(mode.mode, Some(metric_mode::Mode::Stream(_))));
    }
}
