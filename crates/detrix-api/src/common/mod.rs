//! Common utilities shared across API layers (MCP, REST, gRPC)

pub mod diff_parser;
pub mod expression_extractor;
pub mod parsing;

pub use diff_parser::{parse_diff, DiffParseResult, ParsedDiffLine, UnparseableLine};
pub use expression_extractor::{
    detect_language, get_extractor, Confidence, ExpressionExtractor, ExtractedExpression,
    GoExtractor, PythonExtractor, RustExtractor,
};
pub use parsing::{
    extract_variable_names, generate_metric_name, generate_metric_name_with_prefix, parse_location,
    parse_location_flexible, parse_location_str, parse_location_with_expression,
    ParsedLocationWithExpression,
};

/// Validate that a u32 port value fits in u16 range.
///
/// Proto/MCP use u32 for port numbers but the domain uses u16.
/// This prevents silent truncation (e.g., 70000 → 4464).
pub fn validate_port(port: u32) -> Result<u16, String> {
    u16::try_from(port).map_err(|_| format!("Port {} out of valid range (0-65535)", port))
}

/// Parse a language string into a validated `SourceLanguage`.
///
/// Used by API entry points (gRPC, REST, MCP) to convert client-provided
/// language strings into the domain enum before constructing `ConnectionIdentity`.
pub fn parse_language(language: &str) -> Result<detrix_core::SourceLanguage, String> {
    language
        .try_into()
        .map_err(|e: detrix_core::ParseLanguageError| e.to_string())
}

/// Resolve machine hostname, falling back to "unknown" on failure.
///
/// Used by MCP, CLI, and test helpers when the client doesn't provide a hostname.
pub fn resolve_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Resolve workspace_root from an optional connection_id, with auto-select fallback.
///
/// If `connection_id` is provided, looks up that connection's workspace_root.
/// If not provided, auto-selects the workspace_root from the single active connection.
/// Returns `None` if no connection is found, multiple connections exist, or workspace is invalid.
///
/// Used by inspect_file handlers across MCP, REST, and gRPC to resolve relative file paths.
pub async fn resolve_workspace_root(
    state: &crate::state::ApiState,
    connection_id: Option<&str>,
) -> Option<String> {
    if let Some(conn_id_str) = connection_id {
        let conn_id = detrix_core::ConnectionId::new(conn_id_str);
        let connection = state
            .context
            .connection_service
            .get_connection(&conn_id)
            .await
            .ok()
            .flatten()?;
        connection.valid_workspace_root().map(|s| s.to_string())
    } else {
        // Auto-select if only one connection exists
        let connections = state
            .context
            .connection_service
            .list_connections()
            .await
            .unwrap_or_default();
        if connections.len() == 1 {
            connections[0].valid_workspace_root().map(|s| s.to_string())
        } else {
            None
        }
    }
}
