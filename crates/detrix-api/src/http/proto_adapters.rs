//! REST DTOs → Proto Types Adapters
//!
//! This module provides thin conversion functions from REST-specific DTOs
//! to proto types (canonical DTOs). This eliminates code duplication by allowing REST
//! to reuse the shared proto → core conversions from grpc/conversions.rs.
//!
//! ## Architecture
//!
//! ```text
//! REST DTOs → Proto Types (this module) → Core Types (grpc/conversions.rs)
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
use crate::http::handlers::{CreateMetricRequest, StackTraceSliceDto};

/// Convert REST CreateMetricRequest to proto AddMetricRequest
///
/// This is a thin format conversion that maps REST's JSON structure with defaults
/// to proto's structured format. Business logic happens later in
/// grpc/conversions.rs when converting proto → core.
///
/// The `safety_level` parameter is the resolved safety level (either from the request
/// or from SafetyConfig.default_safety_level()).
pub fn rest_request_to_add_metric_request(
    req: &CreateMetricRequest,
    safety_level: &str,
    language: detrix_core::SourceLanguage,
) -> AddMetricRequest {
    // Convert location
    let location = Some(ProtoLocation {
        file: req.location.file.clone(),
        line: req.location.line,
    });

    // Convert mode string to proto MetricMode with configuration parameters
    let mode = Some(rest_mode_to_proto_mode(
        &req.mode,
        req.sample_rate,
        req.sample_interval_seconds,
        req.max_per_second,
    ));

    // Convert stack trace slice DTO to proto with smart defaults
    let stack_trace_slice = if let Some(ref dto) = req.stack_trace_slice {
        Some(rest_stack_trace_to_proto_slice(dto))
    } else if req.capture_stack_trace {
        // Default: capture first 5 frames (same as MCP)
        Some(ProtoStackTraceSlice {
            full: false,
            head: Some(5),
            tail: None,
        })
    } else {
        None
    };

    AddMetricRequest {
        name: req.name.clone(),
        connection_id: req.connection_id.clone(),
        group: req.group.clone(),
        location,
        expressions: req.expressions.clone(),
        language: Some(language.to_string()), // Use connection's language (parameter)
        enabled: req.enabled,
        mode,
        condition: None, // REST doesn't support conditions yet
        safety_level: safety_level.to_string(),
        metadata: None, // REST doesn't use metadata
        replace: Some(req.replace),
        // Stack trace options
        capture_stack_trace: Some(req.capture_stack_trace),
        stack_trace_ttl: req.stack_trace_ttl,
        stack_trace_slice,
        // Memory snapshot options
        capture_memory_snapshot: Some(req.capture_memory_snapshot),
        snapshot_scope: req.snapshot_scope.clone(),
        snapshot_ttl: req.snapshot_ttl,
    }
}

/// Convert REST mode string to proto MetricMode
fn rest_mode_to_proto_mode(
    mode: &str,
    sample_rate: Option<u32>,
    sample_interval_seconds: Option<u32>,
    max_per_second: Option<u32>,
) -> ProtoMetricMode {
    // Use shared mode parser from grpc/conversions.rs
    crate::grpc::conversions::parse_mode_string_to_proto(
        mode,
        sample_rate,
        sample_interval_seconds,
        max_per_second,
    )
}

/// Convert REST StackTraceSliceDto to proto StackTraceSlice
fn rest_stack_trace_to_proto_slice(dto: &StackTraceSliceDto) -> ProtoStackTraceSlice {
    match dto {
        StackTraceSliceDto::HeadTail(head, tail) => ProtoStackTraceSlice {
            full: false,
            head: Some(*head),
            tail: Some(*tail),
        },
        StackTraceSliceDto::Full { full } => ProtoStackTraceSlice {
            full: *full,
            head: None,
            tail: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::detrix::v1::metric_mode::Mode;
    use detrix_config::{
        DEFAULT_MAX_PER_SECOND, DEFAULT_SAMPLE_INTERVAL_SECONDS, DEFAULT_SAMPLE_RATE,
    };

    #[test]
    fn test_rest_mode_stream() {
        let mode = rest_mode_to_proto_mode("stream", None, None, None);
        assert!(matches!(mode.mode, Some(Mode::Stream(_))));
    }

    #[test]
    fn test_rest_mode_first() {
        let mode = rest_mode_to_proto_mode("first", None, None, None);
        assert!(matches!(mode.mode, Some(Mode::First(_))));
    }

    #[test]
    fn test_rest_mode_throttle_with_default() {
        let mode = rest_mode_to_proto_mode("throttle", None, None, None);
        if let Some(Mode::Throttle(t)) = mode.mode {
            assert_eq!(t.max_per_second, DEFAULT_MAX_PER_SECOND);
        } else {
            panic!("Expected Throttle mode");
        }
    }

    #[test]
    fn test_rest_mode_throttle_with_custom_max() {
        let mode = rest_mode_to_proto_mode("throttle", None, None, Some(50));
        if let Some(Mode::Throttle(t)) = mode.mode {
            assert_eq!(t.max_per_second, 50);
        } else {
            panic!("Expected Throttle mode with max_per_second=50");
        }
    }

    #[test]
    fn test_rest_mode_sample_with_default() {
        let mode = rest_mode_to_proto_mode("sample", None, None, None);
        if let Some(Mode::Sample(s)) = mode.mode {
            assert_eq!(s.rate, DEFAULT_SAMPLE_RATE);
        } else {
            panic!("Expected Sample mode with default rate");
        }
    }

    #[test]
    fn test_rest_mode_sample_with_custom_rate() {
        let mode = rest_mode_to_proto_mode("sample", Some(50), None, None);
        if let Some(Mode::Sample(s)) = mode.mode {
            assert_eq!(s.rate, 50);
        } else {
            panic!("Expected Sample mode with rate=50");
        }
    }

    #[test]
    fn test_rest_mode_sample_interval_with_default() {
        let mode = rest_mode_to_proto_mode("sample_interval", None, None, None);
        if let Some(Mode::SampleInterval(s)) = mode.mode {
            assert_eq!(s.seconds, DEFAULT_SAMPLE_INTERVAL_SECONDS);
        } else {
            panic!("Expected SampleInterval mode with default seconds");
        }
    }

    #[test]
    fn test_rest_mode_sample_interval_with_custom_seconds() {
        let mode = rest_mode_to_proto_mode("sample_interval", None, Some(30), None);
        if let Some(Mode::SampleInterval(s)) = mode.mode {
            assert_eq!(s.seconds, 30);
        } else {
            panic!("Expected SampleInterval mode with seconds=30");
        }
    }

    #[test]
    fn test_stack_trace_head_tail() {
        let dto = StackTraceSliceDto::HeadTail(10, 5);
        let slice = rest_stack_trace_to_proto_slice(&dto);
        assert!(!slice.full);
        assert_eq!(slice.head, Some(10));
        assert_eq!(slice.tail, Some(5));
    }

    #[test]
    fn test_stack_trace_full() {
        let dto = StackTraceSliceDto::Full { full: true };
        let slice = rest_stack_trace_to_proto_slice(&dto);
        assert!(slice.full);
        assert!(slice.head.is_none());
        assert!(slice.tail.is_none());
    }
}
