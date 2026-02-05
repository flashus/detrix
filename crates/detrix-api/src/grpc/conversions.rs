//! Conversions between proto types and core types

use crate::generated::detrix::v1::*;
use crate::Error;
use detrix_core::{
    ConnectionId, Location as CoreLocation, Metric, MetricMode as CoreMetricMode, SafetyLevel,
    SnapshotScope, StackTraceSlice as CoreStackTraceSlice, SAFETY_STRICT, SAFETY_TRUSTED,
};

/// Convert proto Location to core Location
pub fn proto_to_core_location(loc: &Location) -> CoreLocation {
    CoreLocation {
        file: loc.file.clone(),
        line: loc.line,
    }
}

/// Convert core Location to proto Location
pub fn core_to_proto_location(loc: &CoreLocation) -> Location {
    Location {
        file: loc.file.clone(),
        line: loc.line,
    }
}

/// Convert proto MetricMode to core MetricMode
pub fn proto_to_core_mode(mode: &MetricMode) -> Result<CoreMetricMode, Error> {
    if let Some(m) = &mode.mode {
        match m {
            metric_mode::Mode::Stream(_) => Ok(CoreMetricMode::Stream),
            metric_mode::Mode::Sample(s) => Ok(CoreMetricMode::Sample { rate: s.rate }),
            metric_mode::Mode::SampleInterval(s) => {
                Ok(CoreMetricMode::SampleInterval { seconds: s.seconds })
            }
            metric_mode::Mode::First(_) => Ok(CoreMetricMode::First),
            metric_mode::Mode::Throttle(t) => Ok(CoreMetricMode::Throttle {
                max_per_second: t.max_per_second,
            }),
        }
    } else {
        Ok(CoreMetricMode::Stream) // Default
    }
}

/// Convert core MetricMode to proto MetricMode
pub fn core_to_proto_mode(mode: &CoreMetricMode) -> MetricMode {
    let proto_mode = match mode {
        CoreMetricMode::Stream => metric_mode::Mode::Stream(StreamMode {}),
        CoreMetricMode::Sample { rate } => metric_mode::Mode::Sample(SampleMode { rate: *rate }),
        CoreMetricMode::SampleInterval { seconds } => {
            metric_mode::Mode::SampleInterval(SampleIntervalMode { seconds: *seconds })
        }
        CoreMetricMode::First => metric_mode::Mode::First(FirstMode {}),
        CoreMetricMode::Throttle { max_per_second } => metric_mode::Mode::Throttle(ThrottleMode {
            max_per_second: *max_per_second,
        }),
    };

    MetricMode {
        mode: Some(proto_mode),
    }
}

/// Convert proto MetricMode to string representation
/// Returns mode name with parameters for modes that have them (e.g., "sample(10)", "throttle(100/s)")
pub fn proto_mode_to_string(mode: &Option<MetricMode>) -> Option<String> {
    mode.as_ref().and_then(|m| {
        m.mode.as_ref().map(|inner| match inner {
            metric_mode::Mode::Stream(_) => "stream".to_string(),
            metric_mode::Mode::Sample(s) => format!("sample({})", s.rate),
            metric_mode::Mode::SampleInterval(s) => format!("sample_interval({}s)", s.seconds),
            metric_mode::Mode::First(_) => "first".to_string(),
            metric_mode::Mode::Throttle(t) => format!("throttle({}/s)", t.max_per_second),
        })
    })
}

/// Convert proto safety_level string to core SafetyLevel
pub fn proto_to_safety_level(level: &str) -> SafetyLevel {
    match level.to_lowercase().as_str() {
        SAFETY_STRICT => SafetyLevel::Strict,
        SAFETY_TRUSTED | "moderate" => SafetyLevel::Trusted,
        _ => SafetyLevel::Strict, // Default to safest
    }
}

/// Convert core SafetyLevel to proto string
pub fn safety_level_to_proto(level: SafetyLevel) -> String {
    level.as_str().to_string()
}

/// Convert AddMetricRequest to core Metric
pub fn add_request_to_metric(req: &AddMetricRequest) -> Result<Metric, Error> {
    let location = req
        .location
        .as_ref()
        .ok_or_else(|| Error::InvalidRequest("Missing location".to_string()))?;

    let mode = req
        .mode
        .as_ref()
        .ok_or_else(|| Error::InvalidRequest("Missing mode".to_string()))?;

    // connection_id is required - must be provided explicitly by client
    if req.connection_id.is_empty() {
        return Err(Error::InvalidRequest(
            "Missing connection_id: must be provided explicitly".to_string(),
        ));
    }

    // Parse stack trace slice from proto
    let stack_trace_slice = req
        .stack_trace_slice
        .as_ref()
        .map(|slice| CoreStackTraceSlice {
            full: slice.full,
            head: slice.head,
            tail: slice.tail,
        });

    // Parse snapshot scope from proto
    let snapshot_scope = req.snapshot_scope.as_deref().and_then(|s| match s {
        "local" => Some(SnapshotScope::Local),
        "global" => Some(SnapshotScope::Global),
        "both" => Some(SnapshotScope::Both),
        _ => None,
    });

    // Language should be filled in by handler (derived from connection if not provided)
    let language_str = req.language.as_ref().ok_or_else(|| {
        Error::InvalidRequest("Missing language (should be derived from connection)".to_string())
    })?;

    Ok(Metric {
        id: None,
        name: req.name.clone(),
        connection_id: ConnectionId::from(req.connection_id.as_str()),
        group: req.group.clone(),
        location: proto_to_core_location(location),
        expression: req.expression.clone(),
        language: language_str.clone().try_into()?,
        mode: proto_to_core_mode(mode)?,
        enabled: req.enabled,
        condition: req.condition.clone(),
        safety_level: proto_to_safety_level(&req.safety_level),
        created_at: None,
        // Introspection fields from proto
        capture_stack_trace: req.capture_stack_trace.unwrap_or(false),
        stack_trace_ttl: req.stack_trace_ttl,
        stack_trace_slice,
        capture_memory_snapshot: req.capture_memory_snapshot.unwrap_or(false),
        snapshot_scope,
        snapshot_ttl: req.snapshot_ttl,
        // Anchor tracking (captured later by AnchorService)
        anchor: None,
        anchor_status: Default::default(),
    })
}

/// Error during conversion between proto and core types
#[derive(Debug, thiserror::Error)]
pub enum ConversionError {
    #[error("Metric '{name}' has no ID - database integrity issue")]
    MissingMetricId { name: String },
    #[error("Missing required field: {field}")]
    MissingField { field: String },
    #[error("Invalid language '{language}': {reason}")]
    InvalidLanguage { language: String, reason: String },
}

/// Convert core Metric to MetricInfo (for listing)
/// Note: hit_count and last_hit_at require async DB queries, so they're set to defaults here.
/// Use `metric_to_info_with_stats` for full stats.
///
/// Returns `Result` - caller decides how to handle missing ID (should not happen for stored metrics).
///
/// IMPORTANT: Exhaustive destructuring ensures compile-time errors when new fields are added.
pub fn metric_to_info(metric: &Metric) -> Result<MetricInfo, ConversionError> {
    // Exhaustive destructuring - compiler will error if new fields are added to Metric
    let Metric {
        id,
        name,
        connection_id,
        group,
        location,
        expression,
        language,
        mode,
        enabled,
        condition: _,    // Not included in MetricInfo
        safety_level: _, // Not included in MetricInfo
        created_at,
        capture_stack_trace,
        stack_trace_ttl: _,   // Not included in MetricInfo
        stack_trace_slice: _, // Not included in MetricInfo
        capture_memory_snapshot,
        snapshot_scope: _, // Not included in MetricInfo
        snapshot_ttl: _,   // Not included in MetricInfo
        anchor: _,         // Anchor tracking - not exposed via gRPC yet
        anchor_status: _,  // Anchor tracking - not exposed via gRPC yet
    } = metric;

    // Metric should always have ID when converting to proto (comes from storage)
    let metric_id = id
        .map(|id| id.0)
        .ok_or_else(|| ConversionError::MissingMetricId { name: name.clone() })?;

    Ok(MetricInfo {
        metric_id,
        name: name.clone(),
        group: group.clone(),
        location: Some(core_to_proto_location(location)),
        expression: expression.clone(),
        language: language.to_string(),
        enabled: *enabled,
        mode: Some(core_to_proto_mode(mode)),
        created_at: created_at.unwrap_or(0),
        hit_count: 0,      // Use metric_to_info_with_stats for actual count
        last_hit_at: None, // Use metric_to_info_with_stats for actual timestamp
        connection_id: connection_id.0.clone(),
        capture_stack_trace: *capture_stack_trace,
        capture_memory_snapshot: *capture_memory_snapshot,
    })
}

/// Convert core Metric to MetricInfo with hit statistics
///
/// Returns `Result` - caller decides how to handle missing ID (should not happen for stored metrics).
///
/// IMPORTANT: Exhaustive destructuring ensures compile-time errors when new fields are added.
pub fn metric_to_info_with_stats(
    metric: &Metric,
    hit_count_val: u64,
    last_hit_at_val: Option<i64>,
) -> Result<MetricInfo, ConversionError> {
    // Exhaustive destructuring - compiler will error if new fields are added to Metric
    let Metric {
        id,
        name,
        connection_id,
        group,
        location,
        expression,
        language,
        mode,
        enabled,
        condition: _,    // Not included in MetricInfo
        safety_level: _, // Not included in MetricInfo
        created_at,
        capture_stack_trace,
        stack_trace_ttl: _,   // Not included in MetricInfo
        stack_trace_slice: _, // Not included in MetricInfo
        capture_memory_snapshot,
        snapshot_scope: _, // Not included in MetricInfo
        snapshot_ttl: _,   // Not included in MetricInfo
        anchor: _,         // Anchor tracking - not exposed via gRPC yet
        anchor_status: _,  // Anchor tracking - not exposed via gRPC yet
    } = metric;

    // Metric should always have ID when converting to proto (comes from storage)
    let metric_id = id
        .map(|id| id.0)
        .ok_or_else(|| ConversionError::MissingMetricId { name: name.clone() })?;

    Ok(MetricInfo {
        metric_id,
        name: name.clone(),
        group: group.clone(),
        location: Some(core_to_proto_location(location)),
        expression: expression.clone(),
        language: language.to_string(),
        enabled: *enabled,
        mode: Some(core_to_proto_mode(mode)),
        created_at: created_at.unwrap_or(0),
        hit_count: hit_count_val,
        last_hit_at: last_hit_at_val,
        connection_id: connection_id.0.clone(),
        capture_stack_trace: *capture_stack_trace,
        capture_memory_snapshot: *capture_memory_snapshot,
    })
}

/// Parse mode string and configuration to proto MetricMode
///
/// Shared by REST and MCP adapters to ensure consistent mode parsing.
///
/// # Arguments
/// * `mode` - Mode string: "stream", "sample", "sample_interval", "first", "throttle"
/// * `sample_rate` - For "sample" mode: capture every Nth hit
/// * `sample_interval_seconds` - For "sample_interval" mode: capture every N seconds
/// * `max_per_second` - For "throttle" mode: max captures per second
pub fn parse_mode_string_to_proto(
    mode: &str,
    sample_rate: Option<u32>,
    sample_interval_seconds: Option<u32>,
    max_per_second: Option<u32>,
) -> MetricMode {
    use detrix_config::{
        DEFAULT_MAX_PER_SECOND, DEFAULT_SAMPLE_INTERVAL_SECONDS, DEFAULT_SAMPLE_RATE,
    };

    let proto_mode = match mode.to_lowercase().as_str() {
        "stream" => metric_mode::Mode::Stream(StreamMode {}),

        "sample" => metric_mode::Mode::Sample(SampleMode {
            rate: sample_rate.unwrap_or(DEFAULT_SAMPLE_RATE),
        }),

        "sample_interval" => metric_mode::Mode::SampleInterval(SampleIntervalMode {
            seconds: sample_interval_seconds.unwrap_or(DEFAULT_SAMPLE_INTERVAL_SECONDS),
        }),

        "first" => metric_mode::Mode::First(FirstMode {}),

        "throttle" => metric_mode::Mode::Throttle(ThrottleMode {
            max_per_second: max_per_second.unwrap_or(DEFAULT_MAX_PER_SECOND),
        }),

        _ => metric_mode::Mode::Stream(StreamMode {}), // Safe default
    };

    MetricMode {
        mode: Some(proto_mode),
    }
}

/// Convert core MetricEvent to proto MetricEvent
pub fn core_event_to_proto(
    event: &detrix_core::MetricEvent,
) -> crate::generated::detrix::v1::MetricEvent {
    core_event_to_proto_with_location(event, None)
}

/// Convert core MetricEvent to proto MetricEvent with optional location
///
/// IMPORTANT: Exhaustive destructuring ensures compile-time errors when new fields are added.
pub fn core_event_to_proto_with_location(
    event: &detrix_core::MetricEvent,
    location_override: Option<&detrix_core::Location>,
) -> crate::generated::detrix::v1::MetricEvent {
    use crate::generated::detrix::v1::{
        metric_event, MemorySnapshot, MetricEvent, StackFrame, StackTrace, Variable,
    };

    // Exhaustive destructuring - compiler will error if new fields are added to core::MetricEvent
    let detrix_core::MetricEvent {
        id: _, // DB-generated, not included in proto
        metric_id,
        metric_name,
        connection_id,
        timestamp,
        value_json,
        value_numeric: _, // Decomposed from value_json
        value_string: _,  // Decomposed from value_json
        value_boolean: _, // Decomposed from value_json
        thread_id,
        thread_name,
        is_error,
        error_type,
        error_message,
        request_id,
        session_id,
        stack_trace: core_stack_trace,
        memory_snapshot: core_memory_snapshot,
    } = event;

    // Determine the result (value or error)
    let result = if *is_error {
        Some(metric_event::Result::Error(
            crate::generated::detrix::v1::ErrorResult {
                error: true,
                error_code: "EVAL_ERROR".to_string(),
                error_type: error_type.clone().unwrap_or_default(),
                error_message: error_message.clone().unwrap_or_default(),
                traceback: None,
            },
        ))
    } else {
        Some(metric_event::Result::ValueJson(value_json.clone()))
    };

    // Convert stack trace if present
    let stack_trace = core_stack_trace.as_ref().map(|st| StackTrace {
        frames: st
            .frames
            .iter()
            .map(|f| StackFrame {
                index: f.index,
                name: f.name.clone(),
                file: f.file.clone(),
                line: f.line,
                module: f.module.clone(),
            })
            .collect(),
    });

    // Convert memory snapshot if present
    let memory_snapshot = core_memory_snapshot.as_ref().map(|snap| MemorySnapshot {
        locals: snap
            .locals
            .iter()
            .map(|v| Variable {
                name: v.name.clone(),
                value: v.value.clone(),
                r#type: v.var_type.clone().unwrap_or_default(),
            })
            .collect(),
        globals: snap
            .globals
            .iter()
            .map(|v| Variable {
                name: v.name.clone(),
                value: v.value.clone(),
                r#type: v.var_type.clone().unwrap_or_default(),
            })
            .collect(),
    });

    MetricEvent {
        metric_id: metric_id.0,
        metric_name: metric_name.clone(),
        location: location_override.map(core_to_proto_location),
        timestamp: *timestamp,
        thread_name: thread_name.clone(),
        thread_id: *thread_id,
        request_id: request_id.clone(),
        session_id: session_id.clone(),
        result,
        connection_id: connection_id.0.clone(),
        stack_trace,
        memory_snapshot,
    }
}

/// Convert core GroupInfo to proto GroupInfo
pub fn core_group_info_to_proto(
    group_info: &detrix_core::GroupInfo,
) -> crate::generated::detrix::v1::GroupInfo {
    crate::generated::detrix::v1::GroupInfo {
        name: group_info.name.clone(),
        metric_count: group_info.metric_count,
        enabled_count: group_info.enabled_count,
    }
}

/// Convert core Connection to proto ConnectionInfo
///
/// IMPORTANT: Uses exhaustive destructuring - compiler will error if new fields added.
///
/// Core Connection fields:
///   id, name, host, port, language, status, auto_reconnect, safe_mode,
///   created_at, last_connected_at, last_active
pub fn connection_to_info(
    connection: &detrix_core::Connection,
) -> crate::generated::detrix::v1::ConnectionInfo {
    use crate::generated::detrix::v1::{ConnectionInfo, ConnectionStatus as ProtoStatus};

    // Exhaustive destructuring - compiler will error if new fields are added to Connection
    let detrix_core::Connection {
        id,
        name: _, // Not in proto ConnectionInfo
        host,
        port,
        language,
        status,
        auto_reconnect,
        safe_mode,
        created_at,
        last_connected_at,
        last_active,
    } = connection;

    // Convert core ConnectionStatus to proto enum value
    let proto_status = match status {
        detrix_core::ConnectionStatus::Disconnected => ProtoStatus::Disconnected as i32,
        detrix_core::ConnectionStatus::Connecting => ProtoStatus::Connecting as i32,
        detrix_core::ConnectionStatus::Reconnecting => ProtoStatus::Connecting as i32, // Maps to Connecting
        detrix_core::ConnectionStatus::Connected => ProtoStatus::Connected as i32,
        detrix_core::ConnectionStatus::Failed(_) => ProtoStatus::Failed as i32,
    };

    ConnectionInfo {
        connection_id: id.0.clone(),
        host: host.clone(),
        port: *port as u32,
        language: language.to_string(),
        status: proto_status,
        created_at: *created_at,
        connected_at: *last_connected_at,
        last_active_at: Some(*last_active),
        auto_reconnect: *auto_reconnect,
        safe_mode: *safe_mode,
        // Runtime state not tracked in core Connection - use defaults
        reconnect_attempts: 0,
        max_reconnect_attempts: 0, // 0 = unlimited retries
    }
}

// ============================================================================
// Proto to Core Conversions (Client-side)
// ============================================================================
// Used by gRPC clients (CLI, TUI) to convert proto responses to core types.
// Also used by E2E tests to convert proto responses.

/// Convert proto StackTrace to core CapturedStackTrace
///
/// Used by gRPC clients and E2E tests to convert stack trace data.
pub fn proto_to_core_stack_trace(
    proto: &crate::generated::detrix::v1::StackTrace,
) -> detrix_core::CapturedStackTrace {
    use detrix_core::{CapturedStackTrace, StackFrame};

    let frames: Vec<StackFrame> = proto
        .frames
        .iter()
        .map(|f| StackFrame {
            index: f.index,
            name: f.name.clone(),
            // Proto has file, line, module as Option
            file: f.file.clone().filter(|s| !s.is_empty()),
            line: f.line,
            column: None,
            module: f.module.clone().filter(|s| !s.is_empty()),
        })
        .collect();
    let total_frames = frames.len() as u32;
    CapturedStackTrace {
        frames,
        total_frames,
        truncated: false,
    }
}

/// Convert proto MemorySnapshot to core MemorySnapshot
///
/// Used by gRPC clients and E2E tests to convert memory snapshot data.
pub fn proto_to_core_memory_snapshot(
    proto: &crate::generated::detrix::v1::MemorySnapshot,
) -> detrix_core::MemorySnapshot {
    use detrix_core::{CapturedVariable, MemorySnapshot, SnapshotScope};

    MemorySnapshot {
        locals: proto
            .locals
            .iter()
            .map(|v| CapturedVariable {
                name: v.name.clone(),
                value: v.value.clone(),
                var_type: if v.r#type.is_empty() {
                    None
                } else {
                    Some(v.r#type.clone())
                },
                scope: Some("local".to_string()),
            })
            .collect(),
        globals: proto
            .globals
            .iter()
            .map(|v| CapturedVariable {
                name: v.name.clone(),
                value: v.value.clone(),
                var_type: if v.r#type.is_empty() {
                    None
                } else {
                    Some(v.r#type.clone())
                },
                scope: Some("global".to_string()),
            })
            .collect(),
        scope: SnapshotScope::Local,
    }
}

/// Convert proto MetricEvent to core MetricEvent
///
/// Used by gRPC streaming clients to convert received events.
pub fn proto_to_core_event(
    proto: &crate::generated::detrix::v1::MetricEvent,
) -> detrix_core::MetricEvent {
    use crate::generated::detrix::v1::metric_event;
    use detrix_core::{ConnectionId, MetricEvent, MetricId};

    // Extract value/error from result oneof
    let (value_json, is_error, error_type, error_message) = match &proto.result {
        Some(metric_event::Result::ValueJson(v)) => (v.clone(), false, None, None),
        Some(metric_event::Result::Error(e)) => (
            String::new(),
            true,
            Some(e.error_type.clone()),
            Some(e.error_message.clone()),
        ),
        None => (String::new(), false, None, None),
    };

    // Convert stack trace and memory snapshot using extracted functions
    let stack_trace = proto.stack_trace.as_ref().map(proto_to_core_stack_trace);
    let memory_snapshot = proto
        .memory_snapshot
        .as_ref()
        .map(proto_to_core_memory_snapshot);

    MetricEvent {
        id: None,
        metric_id: MetricId(proto.metric_id),
        metric_name: proto.metric_name.clone(),
        connection_id: ConnectionId::from(proto.connection_id.as_str()),
        timestamp: proto.timestamp,
        thread_name: proto.thread_name.clone(),
        thread_id: proto.thread_id,
        value_json,
        value_numeric: None,
        value_string: None,
        value_boolean: None,
        is_error,
        error_type,
        error_message,
        request_id: proto.request_id.clone(),
        session_id: proto.session_id.clone(),
        stack_trace,
        memory_snapshot,
    }
}

/// Convert proto MetricInfo to core Metric
///
/// Used by gRPC clients to convert listed metrics.
///
/// Returns `Result` - caller decides how to handle conversion errors.
///
/// NOTE: Proto → core conversions cannot use exhaustive destructuring since proto types
/// are generated. When adding new fields to MetricInfo proto, update this function.
pub fn proto_to_core_metric(
    proto: &crate::generated::detrix::v1::MetricInfo,
) -> Result<detrix_core::Metric, ConversionError> {
    use detrix_core::{
        ConnectionId, Location, Metric, MetricId, MetricMode, SafetyLevel, SourceLanguage,
    };

    let location = proto
        .location
        .as_ref()
        .map(|l| Location {
            file: l.file.clone(),
            line: l.line,
        })
        .ok_or_else(|| ConversionError::MissingField {
            field: "location".to_string(),
        })?;

    let language: SourceLanguage =
        proto
            .language
            .clone()
            .try_into()
            .map_err(|e| ConversionError::InvalidLanguage {
                language: proto.language.clone(),
                reason: format!("{}", e),
            })?;

    Ok(Metric {
        id: Some(MetricId(proto.metric_id)),
        name: proto.name.clone(),
        connection_id: ConnectionId::from(proto.connection_id.as_str()),
        group: proto.group.clone(),
        location,
        expression: proto.expression.clone(),
        language,
        enabled: proto.enabled,
        mode: MetricMode::Stream, // Default mode (proto has mode field but not fully mapped here)
        condition: None,          // Not in proto MetricInfo
        safety_level: SafetyLevel::Strict, // Not in proto MetricInfo
        created_at: Some(proto.created_at),
        capture_stack_trace: proto.capture_stack_trace,
        stack_trace_ttl: None,   // Not in proto MetricInfo
        stack_trace_slice: None, // Not in proto MetricInfo
        capture_memory_snapshot: proto.capture_memory_snapshot,
        snapshot_scope: None, // Not in proto MetricInfo
        snapshot_ttl: None,   // Not in proto MetricInfo
        // Anchor tracking (not in proto MetricInfo)
        anchor: None,
        anchor_status: Default::default(),
    })
}

/// Convert proto ConnectionInfo to core Connection
///
/// Used by gRPC clients to convert listed connections.
///
/// Returns `Result` - caller decides how to handle conversion errors.
pub fn proto_to_core_connection(
    proto: &crate::generated::detrix::v1::ConnectionInfo,
) -> Result<detrix_core::Connection, ConversionError> {
    use detrix_core::{Connection, ConnectionId, ConnectionStatus, SourceLanguage};

    // Convert proto status enum to core status
    // Proto enum values from connections.proto:
    //   0 = UNSPECIFIED, 1 = DISCONNECTED, 2 = CONNECTING, 3 = CONNECTED, 4 = FAILED
    let status = match proto.status {
        0 => ConnectionStatus::Disconnected, // UNSPECIFIED
        1 => ConnectionStatus::Disconnected, // DISCONNECTED
        2 => ConnectionStatus::Connecting,   // CONNECTING
        3 => ConnectionStatus::Connected,    // CONNECTED
        4 => ConnectionStatus::Failed("Connection failed".to_string()), // FAILED
        _ => ConnectionStatus::Disconnected,
    };

    let language: SourceLanguage =
        proto
            .language
            .clone()
            .try_into()
            .map_err(|e| ConversionError::InvalidLanguage {
                language: proto.language.clone(),
                reason: format!("{}", e),
            })?;

    Ok(Connection {
        id: ConnectionId::from(proto.connection_id.as_str()),
        name: None,
        host: proto.host.clone(),
        port: proto.port as u16,
        language,
        status,
        auto_reconnect: proto.auto_reconnect,
        safe_mode: proto.safe_mode,
        created_at: proto.created_at,
        last_connected_at: proto.connected_at,
        last_active: proto.last_active_at.unwrap_or(0),
    })
}

/// Convert core ConnectionStatus to proto ConnectionStatus (i32)
///
/// Used by gRPC servers to convert connection status for responses.
pub fn core_status_to_proto(status: &detrix_core::ConnectionStatus) -> i32 {
    use crate::generated::detrix::v1::ConnectionStatus;
    match status {
        detrix_core::ConnectionStatus::Disconnected => ConnectionStatus::Disconnected as i32,
        detrix_core::ConnectionStatus::Connecting => ConnectionStatus::Connecting as i32,
        detrix_core::ConnectionStatus::Connected => ConnectionStatus::Connected as i32,
        detrix_core::ConnectionStatus::Reconnecting => ConnectionStatus::Connecting as i32, // Map reconnecting to connecting
        detrix_core::ConnectionStatus::Failed(_) => ConnectionStatus::Failed as i32,
    }
}

/// Convert core Connection to proto ConnectionInfo
///
/// Used by gRPC servers to convert connection data for responses.
pub fn core_to_proto_connection_info(conn: &detrix_core::Connection) -> ConnectionInfo {
    ConnectionInfo {
        connection_id: conn.id.0.clone(),
        host: conn.host.clone(),
        port: conn.port as u32,
        language: conn.language.to_string(),
        status: core_status_to_proto(&conn.status),
        created_at: conn.created_at,
        connected_at: conn.last_connected_at,
        last_active_at: Some(conn.last_active),
        auto_reconnect: conn.auto_reconnect,
        reconnect_attempts: 0,     // Not tracked in current Connection struct
        max_reconnect_attempts: 0, // Not tracked in current Connection struct
        safe_mode: conn.safe_mode,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_config::{
        DEFAULT_MAX_PER_SECOND, DEFAULT_SAMPLE_INTERVAL_SECONDS, DEFAULT_SAMPLE_RATE,
    };

    #[test]
    fn test_parse_mode_stream() {
        let mode = parse_mode_string_to_proto("stream", None, None, None);
        assert!(matches!(mode.mode, Some(metric_mode::Mode::Stream(_))));
    }

    #[test]
    fn test_parse_mode_sample_with_custom_rate() {
        let mode = parse_mode_string_to_proto("sample", Some(50), None, None);
        if let Some(metric_mode::Mode::Sample(s)) = mode.mode {
            assert_eq!(s.rate, 50);
        } else {
            panic!("Expected Sample mode with rate 50");
        }
    }

    #[test]
    fn test_parse_mode_sample_with_default_rate() {
        let mode = parse_mode_string_to_proto("sample", None, None, None);
        if let Some(metric_mode::Mode::Sample(s)) = mode.mode {
            assert_eq!(s.rate, DEFAULT_SAMPLE_RATE);
        } else {
            panic!("Expected Sample mode with default rate");
        }
    }

    #[test]
    fn test_parse_mode_sample_interval_with_custom_seconds() {
        let mode = parse_mode_string_to_proto("sample_interval", None, Some(30), None);
        if let Some(metric_mode::Mode::SampleInterval(s)) = mode.mode {
            assert_eq!(s.seconds, 30);
        } else {
            panic!("Expected SampleInterval mode with seconds 30");
        }
    }

    #[test]
    fn test_parse_mode_sample_interval_with_default_seconds() {
        let mode = parse_mode_string_to_proto("sample_interval", None, None, None);
        if let Some(metric_mode::Mode::SampleInterval(s)) = mode.mode {
            assert_eq!(s.seconds, DEFAULT_SAMPLE_INTERVAL_SECONDS);
        } else {
            panic!("Expected SampleInterval mode with default seconds");
        }
    }

    #[test]
    fn test_parse_mode_throttle_with_custom_max() {
        let mode = parse_mode_string_to_proto("throttle", None, None, Some(50));
        if let Some(metric_mode::Mode::Throttle(t)) = mode.mode {
            assert_eq!(t.max_per_second, 50);
        } else {
            panic!("Expected Throttle mode with max_per_second 50");
        }
    }

    #[test]
    fn test_parse_mode_throttle_with_default_max() {
        let mode = parse_mode_string_to_proto("throttle", None, None, None);
        if let Some(metric_mode::Mode::Throttle(t)) = mode.mode {
            assert_eq!(t.max_per_second, DEFAULT_MAX_PER_SECOND);
        } else {
            panic!("Expected Throttle mode with default max_per_second");
        }
    }

    #[test]
    fn test_parse_mode_first() {
        let mode = parse_mode_string_to_proto("first", None, None, None);
        assert!(matches!(mode.mode, Some(metric_mode::Mode::First(_))));
    }

    #[test]
    fn test_parse_mode_invalid_fallback_to_stream() {
        let mode = parse_mode_string_to_proto("invalid", None, None, None);
        assert!(matches!(mode.mode, Some(metric_mode::Mode::Stream(_))));
    }

    #[test]
    fn test_parse_mode_case_insensitive() {
        let mode_upper = parse_mode_string_to_proto("SAMPLE", Some(10), None, None);
        let mode_lower = parse_mode_string_to_proto("sample", Some(10), None, None);
        let mode_mixed = parse_mode_string_to_proto("SaMpLe", Some(10), None, None);

        assert!(matches!(
            mode_upper.mode,
            Some(metric_mode::Mode::Sample(_))
        ));
        assert!(matches!(
            mode_lower.mode,
            Some(metric_mode::Mode::Sample(_))
        ));
        assert!(matches!(
            mode_mixed.mode,
            Some(metric_mode::Mode::Sample(_))
        ));
    }
}
