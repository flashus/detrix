//! gRPC handlers for Cloud VFS operations
//!
//! Following Clean Architecture: This is a CONTROLLER that does DTO mapping ONLY.
//! ALL business logic is in VirtualFileSystem trait implementations.

use crate::generated::detrix::v1::{
    FileHash, GetCachedHashesRequest, GetCachedHashesResponse, ProvideFileRequest,
    ProvideFileResponse, ResponseMetadata, ValidateCacheRequest, ValidateCacheResponse,
};
use crate::state::ApiState;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use tracing::{debug, instrument};

/// Create response metadata from request metadata
fn create_response_metadata(
    request_metadata: Option<&crate::generated::detrix::v1::RequestMetadata>,
) -> ResponseMetadata {
    ResponseMetadata {
        timestamp: chrono::Utc::now().timestamp_micros(),
        request_id: request_metadata
            .map(|m| m.request_id.clone())
            .unwrap_or_default(),
        server_id: String::from("detrix-server"), // TODO: Make configurable
    }
}

/// Handle ProvideFile RPC - store file content in VFS
#[instrument(skip(state, request), fields(connection_id, path))]
pub async fn handle_provide_file(
    state: &Arc<ApiState>,
    request: Request<ProvideFileRequest>,
) -> Result<Response<ProvideFileResponse>, Status> {
    let req = request.into_inner();
    let connection_id = req.connection_id;
    let path = req.path;
    let content = req.content;
    let request_metadata = req.metadata.as_ref();

    debug!(
        "ProvideFile RPC: connection_id={}, path={}, content_len={}",
        connection_id,
        path,
        content.len()
    );

    // Store file in VFS (compute hash using SHA-256)
    use sha2::{Digest, Sha256};
    let hash = format!("{:x}", Sha256::digest(content.as_bytes()));

    state.context.vfs.store(&connection_id, &path, content);

    debug!("File cached: path={}, hash={}", path, hash);
    Ok(Response::new(ProvideFileResponse {
        success: true,
        path: path.clone(),
        hash,
        error: None,
        metadata: Some(create_response_metadata(request_metadata)),
    }))
}

/// Handle ValidateCache RPC - validate client hashes against server cache
#[instrument(skip(state, request), fields(connection_id))]
pub async fn handle_validate_cache(
    state: &Arc<ApiState>,
    request: Request<ValidateCacheRequest>,
) -> Result<Response<ValidateCacheResponse>, Status> {
    let req = request.into_inner();
    let connection_id = req.connection_id;
    let request_metadata = req.metadata.as_ref();
    let client_hashes: Vec<(String, String)> =
        req.files.into_iter().map(|fh| (fh.path, fh.hash)).collect();

    debug!(
        "ValidateCache RPC: connection_id={}, files={}",
        connection_id,
        client_hashes.len()
    );

    let evicted_paths = state
        .context
        .vfs
        .validate_hashes(&connection_id, &client_hashes);

    // Get current cache state for this connection
    let cached_files = state.context.vfs.cached_hashes(&connection_id);
    let cached: Vec<FileHash> = cached_files
        .into_iter()
        .map(|(path, hash)| FileHash { path, hash })
        .collect();

    debug!(
        "ValidateCache result: evicted={}, cached={}",
        evicted_paths.len(),
        cached.len()
    );

    Ok(Response::new(ValidateCacheResponse {
        evicted_paths,
        cached,
        metadata: Some(create_response_metadata(request_metadata)),
    }))
}

/// Handle GetCachedHashes RPC - get all cached file hashes for a connection
#[instrument(skip(state, request), fields(connection_id))]
pub async fn handle_get_cached_hashes(
    state: &Arc<ApiState>,
    request: Request<GetCachedHashesRequest>,
) -> Result<Response<GetCachedHashesResponse>, Status> {
    let req = request.into_inner();
    let connection_id = req.connection_id;
    let request_metadata = req.metadata.as_ref();

    debug!("GetCachedHashes RPC: connection_id={}", connection_id);

    let cached_files = state.context.vfs.cached_hashes(&connection_id);
    let cached: Vec<FileHash> = cached_files
        .into_iter()
        .map(|(path, hash)| FileHash { path, hash })
        .collect();

    debug!("GetCachedHashes result: {} files", cached.len());

    Ok(Response::new(GetCachedHashesResponse {
        cached,
        metadata: Some(create_response_metadata(request_metadata)),
    }))
}
