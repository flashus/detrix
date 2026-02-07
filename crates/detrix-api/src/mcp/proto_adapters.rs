//! MCP Parameters → Proto Types Adapters
//!
//! This module provides thin conversion functions from MCP-specific parameter types
//! to proto types (canonical DTOs). This eliminates code duplication by allowing MCP
//! to reuse the shared proto → core conversions from grpc/conversions.rs.
//!
//! ## Architecture
//!
//! ```text
//! MCP Params → Proto Types (this module) → Core Types (grpc/conversions.rs)
//! ```
//!
//! ## Benefits
//!
//! - DRY: Single source of truth for proto → core conversions
//! - Maintainability: Adding new fields only requires updating proto and shared conversions
//! - Consistency: All protocols use the same business logic

use crate::generated::detrix::v1::{
    AddMetricRequest, Location as ProtoLocation, MetricMode as ProtoMetricMode,
    StackTraceSlice as ProtoStackTraceSlice,
};
use crate::mcp::params::AddMetricParams;

/// Convert MCP AddMetricParams to proto AddMetricRequest
///
/// This is a thin format conversion that maps MCP's LLM-friendly flat structure
/// to proto's more structured format. Business logic happens later in
/// grpc/conversions.rs when converting proto → core.
///
/// ## Location Parsing
///
/// The location is parsed separately (via parse_location_flexible) before calling this.
/// Pass the parsed file and line here.
///
/// The `safety_level` parameter is the resolved safety level from SafetyConfig.default_safety_level().
pub fn mcp_params_to_add_metric_request(
    params: &AddMetricParams,
    parsed_file: String,
    parsed_line: u32,
    connection_id: String,
    safety_level: &str,
) -> AddMetricRequest {
    // Convert location
    let location = Some(ProtoLocation {
        file: parsed_file,
        line: parsed_line,
    });

    // Convert mode
    let mode = Some(mcp_mode_to_proto_mode(
        params.mode.as_deref(),
        params.sample_rate,
        params.sample_interval_seconds,
        params.max_per_second,
    ));

    // Convert stack trace slice
    let stack_trace_slice = mcp_stack_trace_to_proto_slice(
        params.stack_trace_full,
        params.stack_trace_head,
        params.stack_trace_tail,
        params.capture_stack_trace,
    );

    // Convert snapshot scope (just pass through the string)
    let snapshot_scope = params.snapshot_scope.clone();

    AddMetricRequest {
        name: params.name.clone(),
        connection_id,
        group: params.group.clone(),
        location,
        expression: params.expression.clone(),
        language: None, // Will be filled from connection
        enabled: params.enabled,
        mode,
        condition: None,                        // MCP doesn't support conditions yet
        safety_level: safety_level.to_string(), // Uses SafetyConfig.default_safety_level()
        metadata: None,                         // MCP doesn't use metadata
        replace: params.replace,
        // Stack trace options
        capture_stack_trace: params.capture_stack_trace,
        stack_trace_ttl: params.stack_trace_ttl,
        stack_trace_slice,
        // Memory snapshot options
        capture_memory_snapshot: params.capture_memory_snapshot,
        snapshot_scope,
        snapshot_ttl: params.snapshot_ttl,
    }
}

/// Convert MCP mode string and related params to proto MetricMode
fn mcp_mode_to_proto_mode(
    mode: Option<&str>,
    sample_rate: Option<u32>,
    sample_interval_seconds: Option<u32>,
    max_per_second: Option<u32>,
) -> ProtoMetricMode {
    // Use shared mode parser from grpc/conversions.rs
    crate::grpc::conversions::parse_mode_string_to_proto(
        mode.unwrap_or("stream"),
        sample_rate,
        sample_interval_seconds,
        max_per_second,
    )
}

/// Convert MCP stack trace parameters to proto StackTraceSlice
fn mcp_stack_trace_to_proto_slice(
    stack_trace_full: Option<bool>,
    stack_trace_head: Option<u32>,
    stack_trace_tail: Option<u32>,
    capture_stack_trace: Option<bool>,
) -> Option<ProtoStackTraceSlice> {
    if stack_trace_full.unwrap_or(false) {
        Some(ProtoStackTraceSlice {
            full: true,
            head: None,
            tail: None,
        })
    } else if stack_trace_head.is_some() || stack_trace_tail.is_some() {
        Some(ProtoStackTraceSlice {
            full: false,
            head: stack_trace_head,
            tail: stack_trace_tail,
        })
    } else if capture_stack_trace.unwrap_or(false) {
        // Default: capture first 5 frames
        Some(ProtoStackTraceSlice {
            full: false,
            head: Some(5),
            tail: None,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::detrix::v1::metric_mode::Mode;

    #[test]
    fn test_mcp_mode_stream() {
        let mode = mcp_mode_to_proto_mode(None, None, None, None);
        assert!(matches!(mode.mode, Some(Mode::Stream(_))));
    }

    #[test]
    fn test_mcp_mode_sample() {
        let mode = mcp_mode_to_proto_mode(Some("sample"), Some(10), None, None);
        if let Some(Mode::Sample(s)) = mode.mode {
            assert_eq!(s.rate, 10);
        } else {
            panic!("Expected Sample mode");
        }
    }

    #[test]
    fn test_mcp_mode_first() {
        let mode = mcp_mode_to_proto_mode(Some("first"), None, None, None);
        assert!(matches!(mode.mode, Some(Mode::First(_))));
    }

    #[test]
    fn test_mcp_mode_sample_interval() {
        let mode = mcp_mode_to_proto_mode(Some("sample_interval"), None, Some(30), None);
        if let Some(Mode::SampleInterval(s)) = mode.mode {
            assert_eq!(s.seconds, 30);
        } else {
            panic!("Expected SampleInterval mode with seconds=30");
        }
    }

    #[test]
    fn test_mcp_mode_sample_interval_with_default() {
        let mode = mcp_mode_to_proto_mode(Some("sample_interval"), None, None, None);
        if let Some(Mode::SampleInterval(s)) = mode.mode {
            assert_eq!(s.seconds, detrix_config::DEFAULT_SAMPLE_INTERVAL_SECONDS);
        } else {
            panic!("Expected SampleInterval mode with default seconds");
        }
    }

    #[test]
    fn test_mcp_mode_throttle() {
        let mode = mcp_mode_to_proto_mode(Some("throttle"), None, None, Some(50));
        if let Some(Mode::Throttle(t)) = mode.mode {
            assert_eq!(t.max_per_second, 50);
        } else {
            panic!("Expected Throttle mode");
        }
    }

    #[test]
    fn test_stack_trace_full() {
        let slice = mcp_stack_trace_to_proto_slice(Some(true), None, None, None);
        assert!(slice.is_some());
        let slice = slice.unwrap();
        assert!(slice.full);
        assert!(slice.head.is_none());
        assert!(slice.tail.is_none());
    }

    #[test]
    fn test_stack_trace_head_tail() {
        let slice = mcp_stack_trace_to_proto_slice(None, Some(10), Some(5), None);
        assert!(slice.is_some());
        let slice = slice.unwrap();
        assert!(!slice.full);
        assert_eq!(slice.head, Some(10));
        assert_eq!(slice.tail, Some(5));
    }

    #[test]
    fn test_stack_trace_default() {
        let slice = mcp_stack_trace_to_proto_slice(None, None, None, Some(true));
        assert!(slice.is_some());
        let slice = slice.unwrap();
        assert!(!slice.full);
        assert_eq!(slice.head, Some(5)); // Default: first 5 frames
        assert!(slice.tail.is_none());
    }

    #[test]
    fn test_stack_trace_none() {
        let slice = mcp_stack_trace_to_proto_slice(None, None, None, None);
        assert!(slice.is_none());
    }
}
