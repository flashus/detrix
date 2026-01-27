//! Connection management handlers
//!
//! Endpoints for creating, listing, and closing debug adapter connections.
//!
//! Uses proto CreateConnectionRequest/Response.

use super::connection_to_rest_response;
use crate::constants::status;
use crate::http::error::{HttpError, ToHttpOption, ToHttpResult};
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
#[instrument(skip(state, payload), fields(host = %payload.host, port = payload.port, language = %payload.language))]
pub async fn create_connection(
    State(state): State<Arc<ApiState>>,
    Json(payload): Json<CreateConnectionRequest>,
) -> Result<(StatusCode, Json<CreateConnectionResponse>), HttpError> {
    info!(
        "REST: create_connection (host={}, port={}, language={}, program={:?})",
        payload.host, payload.port, payload.language, payload.program
    );

    let connection_service = &state.context.connection_service;

    // ConnectionService.create_connection now handles:
    // 1. Saving connection to repository
    // 2. Starting adapter via AdapterLifecycleManager
    // 3. Setting up event listeners
    // 4. Updating connection status
    // Proto uses u32 for port, service expects u16
    let port = payload.port as u16;

    let connection_id = connection_service
        .create_connection(
            payload.host.clone(),
            port,
            payload.language.clone(),
            payload.connection_id,
            payload.program, // Optional program path for Rust direct lldb-dap
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
            connection_id: connection_id.0.clone(),
            status: status::CREATED.to_string(),
            connection: Some(connection_to_rest_response(&connection)),
            metadata: None,
        }),
    ))
}

/// Close and remove a connection.
///
/// Disconnects from the debug adapter, removes all associated metrics' logpoints,
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
    Path(id): Path<String>,
) -> Result<StatusCode, HttpError> {
    info!("REST: close_connection (id={})", id);

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
) -> Result<Json<CleanupResponse>, HttpError> {
    info!("REST: cleanup_connections");

    let connection_service = &state.context.connection_service;

    let deleted = connection_service
        .cleanup_stale_connections()
        .await
        .http_context("Failed to cleanup connections")?;

    Ok(Json(CleanupResponse { deleted }))
}
