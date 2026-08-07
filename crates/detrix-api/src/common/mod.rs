//! Common utilities shared across API layers (MCP, REST, gRPC)

pub mod auth;
pub mod diff_parser;
pub mod expression_extractor;
pub mod parsing;

pub use auth::{authenticate_token, authenticate_token_sync, AuthError, AuthState};

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

/// Authenticated user identity, shared across HTTP and gRPC authentication layers.
///
/// Injected into request extensions after successful authentication.
/// HTTP handlers extract it via `Extension<AuthenticatedUser>`;
/// gRPC handlers extract it via `request.extensions().get::<AuthenticatedUser>()`.
#[derive(Clone, Debug)]
pub struct AuthenticatedUser {
    /// User identity (from StaticUser.user_id or JWT `sub` claim)
    pub user_id: String,
    /// User role (Admin or User)
    pub role: detrix_config::UserRole,
}

impl AuthenticatedUser {
    /// Create the default admin user (used when auth is disabled or for public endpoints).
    pub fn default_admin() -> Self {
        Self {
            user_id: detrix_config::AUTO_AUTH_DEFAULT_USER_ID.to_string(),
            role: detrix_config::UserRole::Admin,
        }
    }

    /// Build a `MetricScope` from this user, optionally scoped to a client_id.
    pub fn scope(&self, client_id: Option<String>) -> detrix_application::MetricScope {
        detrix_application::extract_scope(&self.user_id, &self.role, client_id)
    }
}

/// HTTP header / gRPC metadata key for client identity.
///
/// Used across REST, gRPC, and MCP to identify the originator of mutations.
/// HTTP headers are case-insensitive; gRPC metadata keys must be lowercase.
pub const CLIENT_ID_HEADER: &str = "x-detrix-client-id";

/// Validate a client ID string.
///
/// Rejects empty strings and the reserved `DAEMON_IDENTITY` value.
/// Returns the validated string on success, or a static error message.
pub fn validate_client_id(value: &str) -> Result<String, &'static str> {
    if value.is_empty() {
        return Err("client ID must not be empty");
    }
    if value == detrix_core::DAEMON_IDENTITY {
        return Err("reserved '__daemon__' cannot be used via API");
    }
    Ok(value.to_string())
}

/// Extract and validate client_id from HTTP headers.
///
/// Returns `Ok(None)` if the header is absent (backwards compatible).
/// Returns `Ok(Some(id))` if the header is present and valid.
/// Returns `Err` for present-but-invalid values (empty, reserved).
///
/// This is a lower-level helper usable by any HTTP-based handler (REST, MCP).
pub fn extract_client_id_from_headers(
    headers: &axum::http::HeaderMap,
) -> Result<Option<String>, String> {
    match headers.get(CLIENT_ID_HEADER).and_then(|v| v.to_str().ok()) {
        Some(value) => Ok(Some(validate_client_id(value)?)),
        None => Ok(None),
    }
}

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

/// Pre-fetch a file into the VFS cache using the file source chain.
///
/// Resolves the connection from `connection_id` (or auto-selects single connection)
/// and calls `ensure_available()` to transparently fetch the file if not cached.
///
/// Failures are silently ignored — if the file can't be fetched, downstream code
/// will handle the "file not found" error naturally.
pub async fn ensure_file_cached(
    state: &crate::state::ApiState,
    connection_id: Option<&str>,
    file_path: &str,
) {
    let connection = if let Some(conn_id_str) = connection_id {
        let conn_id = detrix_core::ConnectionId::new(conn_id_str);
        state
            .context
            .connection_service
            .get_connection(&conn_id)
            .await
            .ok()
            .flatten()
    } else {
        // Auto-select if only one connection exists
        let connections = state
            .context
            .connection_service
            .list_connections()
            .await
            .unwrap_or_default();
        if connections.len() == 1 {
            connections.into_iter().next()
        } else {
            None
        }
    };

    if let Some(conn) = connection {
        if let Err(e) = state
            .context
            .file_source_chain
            .ensure_available(&conn, file_path)
            .await
        {
            tracing::debug!("Pre-fetch skipped for '{file_path}': {e}");
        }
    }
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
