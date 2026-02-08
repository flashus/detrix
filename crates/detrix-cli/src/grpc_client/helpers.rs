//! Helper functions for gRPC client type conversions

use super::types::{ConnectionInfo, MetricInfo};
use anyhow::Result;

/// Convert proto MetricInfo to CLI MetricInfo
pub fn metric_info_from_proto(m: detrix_api::generated::detrix::v1::MetricInfo) -> MetricInfo {
    MetricInfo {
        metric_id: m.metric_id,
        name: m.name,
        group: m.group,
        location_file: m.location.clone().map(|l| l.file).unwrap_or_default(),
        location_line: m.location.map(|l| l.line).unwrap_or(0),
        expressions: m.expressions,
        language: m.language,
        enabled: m.enabled,
        mode: format!("{:?}", m.mode),
        hit_count: m.hit_count,
        last_hit_at: m.last_hit_at,
    }
}

/// Convert proto ConnectionInfo to CLI ConnectionInfo
pub fn connection_info_from_proto(
    conn: detrix_api::generated::detrix::v1::ConnectionInfo,
) -> ConnectionInfo {
    // Call status() first before moving other fields
    let status = format!("{:?}", conn.status());
    ConnectionInfo {
        connection_id: conn.connection_id,
        host: conn.host,
        port: conn.port,
        language: conn.language,
        status,
        created_at: conn.created_at,
        connected_at: conn.connected_at,
        last_active_at: conn.last_active_at,
    }
}

/// Parse location string using shared parser from detrix-api.
///
/// Delegates to `parse_location_flexible` which handles:
/// - `@file.py#123` (standard format)
/// - `file.py#123` (missing @, auto-handled)
/// - `file.py` + line=123 (separate params)
/// - Windows paths like `C:/Users/test.py#10`
/// - Relative paths with `./` prefix (normalized)
pub fn parse_location(location: &str, line_override: Option<u32>) -> Result<(String, u32)> {
    // Note: rmcp::ErrorData is an external MCP protocol type without std::error::Error impl,
    // so anyhow! conversion is required (cannot use From trait due to orphan rules)
    let loc = detrix_api::common::parse_location_flexible(location, line_override)
        .map_err(|e| anyhow::anyhow!("Invalid location '{}': {}", location, e.message))?;
    Ok((loc.file, loc.line))
}
