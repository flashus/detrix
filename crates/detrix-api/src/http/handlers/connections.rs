//! Connection management handlers
//!
//! Endpoints for creating, listing, and closing debug adapter connections.
//!
//! Uses proto CreateConnectionRequest/Response.

use super::connection_to_rest_response;
use crate::constants::status;
use crate::http::error::{HttpError, ToHttpBadRequest, ToHttpOption, ToHttpResult};
use crate::state::ApiState;
use crate::types::{ConnectionInfo, CreateConnectionRequest, CreateConnectionResponse};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use detrix_core::ConnectionId;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, instrument};

/// Query parameters for listing connections
/// Uses camelCase to match proto JSON conventions
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListConnectionsQuery {
    pub active_only: Option<bool>,
}

/// List all configured connections with optional filtering.
///
/// # Query Parameters
/// - `activeOnly`: If true, only return connections with active adapters
///
/// # Response
/// Returns JSON array of `ConnectionInfo` objects containing host, port,
/// language, and connection status.
#[instrument(skip(state))]
pub async fn list_connections(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<ListConnectionsQuery>,
) -> Result<Json<Vec<ConnectionInfo>>, HttpError> {
    info!(
        "REST: list_connections (active_only={:?})",
        params.active_only
    );

    let connection_service = &state.context.connection_service;

    let connections = if params.active_only.unwrap_or(false) {
        connection_service.list_active_connections().await
    } else {
        connection_service.list_connections().await
    }
    .http_context("Failed to list connections")?;

    // Convert to proto ConnectionInfo
    let dtos: Vec<ConnectionInfo> = connections
        .iter()
        .map(connection_to_rest_response)
        .collect();
    Ok(Json(dtos))
}

/// Get a single connection by its string ID.
///
/// # Path Parameters
/// - `id`: Connection ID (string, e.g., "python-main" or auto-generated UUID)
///
/// # Response
/// Returns `ConnectionInfo` with connection details including status and metrics.
///
/// # Errors
/// - 404 Not Found: Connection with given ID does not exist
pub async fn get_connection(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<ConnectionInfo>, HttpError> {
    info!("REST: get_connection (id={})", id);

    let connection_service = &state.context.connection_service;

    let connection_id = ConnectionId::new(&id);
    let connection = connection_service
        .get_connection(&connection_id)
        .await
        .http_context("Failed to get connection")?
        .http_not_found(&format!("Connection {}", id))?;

    Ok(Json(connection_to_rest_response(&connection)))
}

/// Create a new connection to a debug adapter.
///
/// Establishes a connection to a running process with a debug adapter (e.g., debugpy).
/// Automatically starts the appropriate DAP adapter for the specified language.
///
/// # Request Body
/// - `host`: Hostname or IP of the debug adapter (e.g., "127.0.0.1")
/// - `port`: Port number of the debug adapter (e.g., 5678 for debugpy)
/// - `language`: Language type ('python', 'go', 'rust')
/// - `connectionId`: Optional custom connection ID (auto-generated if not provided)
/// - `program`: Optional program path for launch mode (Rust direct lldb-dap)
///
/// # Response
/// Returns 201 Created with `CreateConnectionResponse` containing the connection ID
/// and full connection details.
///
/// # Errors
/// - 400 Bad Request: Invalid language or connection parameters
/// - 409 Conflict: Connection with same ID already exists
#[instrument(skip(state, headers, payload), fields(host = %payload.host, port = payload.port, language = %payload.language))]
pub async fn create_connection(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<CreateConnectionRequest>,
) -> Result<(StatusCode, Json<CreateConnectionResponse>), HttpError> {
    // Use X-Detrix-Client-Id header for ownership tracking (client UUID).
    // Falls back to None if header not provided (backwards compat for local CLI callers).
    let created_by = super::extract_client_id(&headers)?;
    info!(
        "REST: create_connection (host={}, port={}, language={}, program={:?}, pid={:?})",
        payload.host, payload.port, payload.language, payload.program, payload.pid
    );

    let connection_service = &state.context.connection_service;

    // Build connection identity from request
    // Note: name, workspace_root, hostname are required fields
    let port = crate::common::validate_port(payload.port).http_bad_request()?;
    let language = crate::common::parse_language(&payload.language).http_bad_request()?;
    let identity = detrix_core::ConnectionIdentity::new(
        payload.name,
        language,
        payload.workspace_root,
        payload.hostname,
    );

    let connection_id = connection_service
        .create_connection_with_metadata(
            payload.host,
            port,
            identity,
            payload.program,   // Optional program path for Rust direct lldb-dap
            payload.pid,       // Optional PID for Rust client AttachPid mode
            payload.safe_mode, // SafeMode: only allow logpoints
            payload.control_plane_url,
            payload.build_commit,
            payload.build_tag,
            created_by, // Client identity from X-Detrix-Client-Id header
        )
        .await
        .http_context("Failed to create connection")?;

    info!("Created connection {} with adapter", connection_id.0);

    // Fetch the created connection for full response
    let connection = connection_service
        .get_connection(&connection_id)
        .await
        .http_context("Connection created but could not be retrieved")?
        .http_not_found("Connection after creation")?;

    Ok((
        StatusCode::CREATED,
        Json(CreateConnectionResponse {
            connection_id: connection.id.0.clone(),
            status: status::CREATED.to_string(),
            connection: Some(connection_to_rest_response(&connection)),
            metadata: None,
            advertise_url: state.advertise_url.clone(),
        }),
    ))
}

/// Close and remove a connection.
///
/// Disconnects from the debug adapter, deletes all associated metrics (cascade delete),
/// and deletes the connection from storage.
///
/// # Path Parameters
/// - `id`: Connection ID (string)
///
/// # Response
/// Returns 204 No Content on success.
///
/// # Errors
/// - 404 Not Found: Connection with given ID does not exist
#[instrument(skip(state), fields(connection_id = %id))]
pub async fn close_connection(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, HttpError> {
    let client_id = super::extract_client_id(&headers)?;
    info!(
        "REST: close_connection (id={}, client_id={:?})",
        id, client_id
    );

    let connection_service = &state.context.connection_service;

    let connection_id = ConnectionId::new(&id);
    connection_service
        .disconnect(&connection_id)
        .await
        .http_context("Failed to close connection")?;

    Ok(StatusCode::NO_CONTENT)
}

/// Cleanup stale connections response
#[derive(Debug, Serialize)]
pub struct CleanupResponse {
    /// Number of connections that were removed
    pub deleted: u64,
}

/// Remove all stale (disconnected/failed) connections.
///
/// This cleans up connections that are no longer active, which can accumulate
/// over time as debuggers are started and stopped.
///
/// # Response
/// Returns JSON with the count of deleted connections.
#[instrument(skip(state))]
pub async fn cleanup_connections(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<CleanupResponse>, HttpError> {
    let client_id = super::extract_client_id(&headers)?;
    info!("REST: cleanup_connections (client_id={:?})", client_id);

    let connection_service = &state.context.connection_service;

    // API cleanup: remove all inactive connections (ttl_days=0)
    let deleted = connection_service
        .cleanup_stale_connections(0)
        .await
        .http_context("Failed to cleanup connections")?;

    Ok(Json(CleanupResponse { deleted }))
}

/// Touch connections request
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TouchConnectionsRequest {
    /// List of connection IDs to touch
    pub connection_ids: Vec<String>,
}

/// Touch connections response
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TouchConnectionsResponse {
    /// Number of connections updated
    pub updated: u32,
}

/// Update last_active timestamp for multiple connections.
///
/// This endpoint is used by MCP bridge and clients to mark connections as active,
/// preventing them from being cleaned up by TTL logic.
///
/// # Request Body
/// - `connectionIds`: Array of connection ID strings to touch
///
/// # Response
/// Returns JSON with the count of successfully updated connections.
///
/// # Security
/// This endpoint is intentionally public (no authentication required).
/// The MCP bridge heartbeat calls this endpoint every 10 seconds, including
/// scenarios where the token may be unavailable (e.g. daemon restart, token
/// refresh in progress). Authentication would break liveness tracking.
#[instrument(skip(state))]
pub async fn touch_connections(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<TouchConnectionsRequest>,
) -> Result<Json<TouchConnectionsResponse>, HttpError> {
    info!(
        "REST: touch_connections (count={})",
        payload.connection_ids.len()
    );

    let connection_service = &state.context.connection_service;
    let mut updated = 0;

    for connection_id_str in &payload.connection_ids {
        let connection_id = ConnectionId::new(connection_id_str);
        let result = connection_service.touch_connection(&connection_id).await;

        match result {
            Ok(_) => updated += 1,
            Err(_) => {
                tracing::debug!(connection_id = %connection_id_str, "Failed to touch connection (may not exist)");
            }
        }
    }

    Ok(Json(TouchConnectionsResponse { updated }))
}
