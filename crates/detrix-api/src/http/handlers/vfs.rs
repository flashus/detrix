//! REST API handlers for Cloud VFS operations
//!
//! Following Clean Architecture: Controllers do DTO mapping ONLY.
//! Business logic lives in VirtualFileSystem trait implementations.

use crate::http::error::ApiError;
use crate::state::ApiState;
use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, instrument};

/// Request to provide file content for caching
#[derive(Debug, Deserialize, Serialize)]
pub struct ProvideFileRequest {
    pub connection_id: String,
    pub path: String,
    pub content: String,
}

/// Response from providing file content
#[derive(Debug, Serialize)]
pub struct ProvideFileResponse {
    pub success: bool,
    pub path: String,
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// File hash entry for validation
#[derive(Debug, Deserialize, Serialize)]
pub struct FileHash {
    pub path: String,
    pub hash: String,
}

/// Request to validate cache hashes
#[derive(Debug, Deserialize)]
pub struct ValidateCacheRequest {
    pub connection_id: String,
    pub files: Vec<FileHash>,
}

/// Response from cache validation
#[derive(Debug, Serialize)]
pub struct ValidateCacheResponse {
    pub evicted_paths: Vec<String>,
    pub cached: Vec<FileHash>,
}

/// Response for getting cached hashes
#[derive(Debug, Serialize)]
pub struct GetCachedHashesResponse {
    pub cached: Vec<FileHash>,
}

/// PUT /api/v1/files - Provide file content to cache in VFS
#[instrument(skip(state, request))]
pub async fn provide_file(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ProvideFileRequest>,
) -> Result<Json<ProvideFileResponse>, ApiError> {
    debug!(
        "ProvideFile REST: connection_id={}, path={}, content_len={}",
        request.connection_id,
        request.path,
        request.content.len()
    );

    // Compute hash using SHA-256
    use sha2::{Digest, Sha256};
    let hash = format!("{:x}", Sha256::digest(request.content.as_bytes()));

    // Store file in VFS
    state
        .context
        .vfs
        .store(&request.connection_id, &request.path, request.content);

    debug!("File cached: path={}, hash={}", request.path, hash);

    Ok(Json(ProvideFileResponse {
        success: true,
        path: request.path,
        hash,
        error: None,
    }))
}

/// POST /api/v1/cache/validate - Validate client hashes against server cache
#[instrument(skip(state, request))]
pub async fn validate_cache(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<ValidateCacheRequest>,
) -> Result<Json<ValidateCacheResponse>, ApiError> {
    debug!(
        "ValidateCache REST: connection_id={}, files={}",
        request.connection_id,
        request.files.len()
    );

    let client_hashes: Vec<(String, String)> = request
        .files
        .into_iter()
        .map(|fh| (fh.path, fh.hash))
        .collect();

    let evicted_paths = state
        .context
        .vfs
        .validate_hashes(&request.connection_id, &client_hashes);

    // Get current cache state for this connection
    let cached_files = state.context.vfs.cached_hashes(&request.connection_id);
    let cached: Vec<FileHash> = cached_files
        .into_iter()
        .map(|(path, hash)| FileHash { path, hash })
        .collect();

    debug!(
        "ValidateCache result: evicted={}, cached={}",
        evicted_paths.len(),
        cached.len()
    );

    Ok(Json(ValidateCacheResponse {
        evicted_paths,
        cached,
    }))
}

/// GET /api/v1/cache/:connection_id - Get all cached file hashes for a connection
#[instrument(skip(state))]
pub async fn get_cached_hashes(
    State(state): State<Arc<ApiState>>,
    Path(connection_id): Path<String>,
) -> Result<Json<GetCachedHashesResponse>, ApiError> {
    debug!("GetCachedHashes REST: connection_id={}", connection_id);

    let cached_files = state.context.vfs.cached_hashes(&connection_id);
    let cached: Vec<FileHash> = cached_files
        .into_iter()
        .map(|(path, hash)| FileHash { path, hash })
        .collect();

    debug!("GetCachedHashes result: {} files", cached.len());

    Ok(Json(GetCachedHashesResponse { cached }))
}
