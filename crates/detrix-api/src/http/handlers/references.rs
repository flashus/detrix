//! Connection reference handlers for multi-user safety
//!
//! Endpoints for attaching to, releasing from, and listing references on connections.
//! References enable multi-user safety: connections are only disconnected when their
//! last reference is removed.

use crate::http::error::{HttpError, ToHttpResult};
use crate::http::middleware::AuthenticatedUser;
use crate::state::ApiState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Extension, Json,
};
use detrix_core::connection_reference::ClientIdentity;
use detrix_core::ConnectionId;
use serde::Serialize;
use std::sync::Arc;
use tracing::{info, instrument};

/// Extract client identity from request headers.
/// Returns 400 Bad Request if header is missing or contains reserved value.
///
/// Unlike `extract_client_id()` in mod.rs (optional), this ALWAYS requires the header.
/// Used by reference counting endpoints where client identity is mandatory.
fn extract_client_identity(headers: &HeaderMap) -> Result<ClientIdentity, HttpError> {
    use crate::common::CLIENT_ID_HEADER;

    let client_id = headers
        .get(CLIENT_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            HttpError::bad_request(format!("Missing required header: {}", CLIENT_ID_HEADER))
        })?;

    crate::common::validate_client_id(client_id)
        .map(ClientIdentity::bridge)
        .map_err(|e| HttpError::bad_request(e.to_string()))
}

/// Response for attach operation
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachResponse {
    pub status: String,
    pub connection_id: String,
    pub client_id: String,
}

/// Response for release operations
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseResponse {
    pub released: bool,
    pub disconnected: bool,
    pub connection_id: String,
}

/// Response for release-all operations
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAllResponse {
    pub references_released: u64,
    pub connections_disconnected: u64,
}

/// Response for listing references
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceInfo {
    pub client_id: String,
    pub kind: String,
    pub created_at: i64,
    pub last_active: i64,
}

/// Response for admin disconnect-all
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDisconnectAllResponse {
    pub status: String,
    pub connections_disconnected: u32,
}

/// Attach to a connection (create a Client reference).
///
/// Requires `X-Detrix-Client-Id` header.
#[instrument(skip(state, headers))]
pub async fn attach_connection(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<AttachResponse>), HttpError> {
    let client_identity = extract_client_identity(&headers)?;

    info!(
        "REST: attach_connection (id={}, client={})",
        id,
        client_identity.as_str()
    );

    let connection_id = ConnectionId::from(id.as_str());

    state
        .context
        .connection_service
        .attach_to_connection(&connection_id, &client_identity)
        .await
        .http_context("Failed to attach to connection")?;

    Ok((
        StatusCode::OK,
        Json(AttachResponse {
            status: "attached".to_string(),
            connection_id: id,
            client_id: client_identity.as_str().to_string(),
        }),
    ))
}

/// Release a single connection reference.
///
/// If this was the last reference, the connection is disconnected.
/// Requires `X-Detrix-Client-Id` header.
#[instrument(skip(state, headers))]
pub async fn release_connection(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ReleaseResponse>, HttpError> {
    let client_identity = extract_client_identity(&headers)?;

    info!(
        "REST: release_connection (id={}, client={})",
        id,
        client_identity.as_str()
    );

    let connection_id = ConnectionId::from(id.as_str());

    let (released, disconnected) = state
        .context
        .connection_service
        .release_reference(&connection_id, &client_identity)
        .await
        .http_context("Failed to release connection reference")?;

    Ok(Json(ReleaseResponse {
        released,
        disconnected,
        connection_id: id,
    }))
}

/// Release ALL of the caller's connection references.
///
/// Connections with zero remaining references are disconnected.
/// Requires `X-Detrix-Client-Id` header.
#[instrument(skip(state, headers))]
pub async fn release_connections(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
) -> Result<Json<ReleaseAllResponse>, HttpError> {
    let client_identity = extract_client_identity(&headers)?;

    info!(
        "REST: release_connections (client={})",
        client_identity.as_str()
    );

    let (released, disconnected) = state
        .context
        .connection_service
        .release_all_client_references(&client_identity)
        .await
        .http_context("Failed to release connection references")?;

    Ok(Json(ReleaseAllResponse {
        references_released: released,
        connections_disconnected: disconnected,
    }))
}

/// List all references for a connection.
#[instrument(skip(state))]
pub async fn list_references(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ReferenceInfo>>, HttpError> {
    let connection_id = ConnectionId::from(id.as_str());

    let references = state
        .context
        .connection_service
        .list_references(&connection_id)
        .await
        .http_context("Failed to list connection references")?;

    let infos: Vec<ReferenceInfo> = references
        .iter()
        .map(|r| ReferenceInfo {
            client_id: r.client_identity.as_str().to_string(),
            kind: r.kind.as_str().to_string(),
            created_at: r.created_at,
            last_active: r.last_active,
        })
        .collect();

    Ok(Json(infos))
}

/// Admin: Force disconnect all connections, ignoring reference counts.
///
/// This is the old behavior of disconnect_all. Gated by `api.rest.admin_endpoints_enabled` config.
/// Returns 403 Forbidden if admin endpoints are not enabled.
#[instrument(skip(state))]
pub async fn admin_disconnect_all(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<AuthenticatedUser>,
) -> Result<Json<AdminDisconnectAllResponse>, HttpError> {
    // Gate by config flag
    let config = state.config_service.get_config().await;
    if !config.api.rest.admin_endpoints_enabled {
        return Err(HttpError::with_code(
            axum::http::StatusCode::FORBIDDEN,
            "Admin endpoints are disabled. Set api.rest.admin_endpoints_enabled = true in config.",
            detrix_core::ErrorCode::Unauthorized,
        ));
    }

    // Require admin role
    if user.role != detrix_config::UserRole::Admin {
        return Err(HttpError::with_code(
            StatusCode::FORBIDDEN,
            "Admin role required",
            detrix_core::ErrorCode::Forbidden,
        ));
    }

    info!("REST: admin_disconnect_all");

    let disconnected = state
        .context
        .connection_service
        .admin_disconnect_all()
        .await
        .http_context("Failed to force disconnect all")?;

    Ok(Json(AdminDisconnectAllResponse {
        status: "disconnected".to_string(),
        connections_disconnected: disconnected as u32,
    }))
}

/// Request body for disable-metrics-by-owner
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisableMetricsByOwnerRequest {
    pub client_identity: String,
}

/// Response for disable-metrics-by-owner
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisableMetricsByOwnerResponse {
    pub disabled: u64,
}

/// Admin: Disable all enabled metrics owned by a client identity.
///
/// Bulk-disables metrics whose `user_id` matches the given client identity.
/// Used for user-scoped cleanup when a bridge session ends.
/// Gated by `api.rest.admin_endpoints_enabled` config.
#[instrument(skip(state))]
pub async fn admin_disable_metrics_by_owner(
    State(state): State<Arc<ApiState>>,
    Extension(user): Extension<AuthenticatedUser>,
    axum::Json(req): axum::Json<DisableMetricsByOwnerRequest>,
) -> Result<axum::Json<DisableMetricsByOwnerResponse>, HttpError> {
    // Gate by config flag
    let config = state.config_service.get_config().await;
    if !config.api.rest.admin_endpoints_enabled {
        return Err(HttpError::with_code(
            axum::http::StatusCode::FORBIDDEN,
            "Admin endpoints are disabled. Set api.rest.admin_endpoints_enabled = true in config.",
            detrix_core::ErrorCode::Unauthorized,
        ));
    }

    // Require admin role
    if user.role != detrix_config::UserRole::Admin {
        return Err(HttpError::with_code(
            StatusCode::FORBIDDEN,
            "Admin role required",
            detrix_core::ErrorCode::Forbidden,
        ));
    }

    info!(
        "REST: admin_disable_metrics_by_owner client_identity={}",
        req.client_identity
    );

    let disabled = state
        .context
        .metric_service
        .disable_metrics_by_owner(&req.client_identity)
        .await
        .http_context("Failed to disable metrics by owner")?;

    Ok(axum::Json(DisableMetricsByOwnerResponse { disabled }))
}
