//! Test file server for Docker Cloud E2E tests.
//!
//! Wraps `FileServingService` (detrix-application) with path remapping so
//! container-absolute paths (`/app/main.py`) are translated to host paths
//! before serving. Binds to `0.0.0.0:0` so Docker containers can reach it
//! via `host.docker.internal`.

use axum::{
    extract::Json, extract::State, http::StatusCode, response::IntoResponse, routing::post, Router,
};
use detrix_application::services::file_serving::{
    FileServingError, FileServingService, ReadFileRequest,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinHandle;

struct TestFileServerState {
    service: FileServingService,
    path_mappings: HashMap<String, PathBuf>,
    git_repo: Option<PathBuf>,
}

/// Start a test file server that remaps container paths to host paths.
///
/// # Arguments
/// * `path_mappings` - Map of container prefix → host directory (e.g. `"/app" → "./fixtures/python"`)
/// * `git_repo` - If set, overrides `workspace_root` in requests for git-pinned serving
///
/// # Returns
/// `(port, join_handle)` — the port the server is listening on and a handle to the background task.
pub async fn start_test_file_server(
    path_mappings: HashMap<String, PathBuf>,
    git_repo: Option<PathBuf>,
) -> Result<(u16, JoinHandle<()>), Box<dyn std::error::Error + Send + Sync>> {
    let state = Arc::new(TestFileServerState {
        service: FileServingService::new(),
        path_mappings,
        git_repo,
    });

    let app = Router::new()
        .route("/detrix/files/read", post(handle_read))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let port = listener.local_addr()?.port();

    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("Test file server error: {e}");
        }
    });

    Ok((port, handle))
}

async fn handle_read(
    State(state): State<Arc<TestFileServerState>>,
    Json(mut req): Json<ReadFileRequest>,
) -> impl IntoResponse {
    // Remap container paths to host paths
    for (container_prefix, host_path) in &state.path_mappings {
        if req.path.starts_with(container_prefix.as_str()) {
            let suffix = &req.path[container_prefix.len()..];
            req.path = host_path
                .join(suffix.trim_start_matches('/'))
                .to_string_lossy()
                .to_string();
            break;
        }
    }

    // Override workspace_root for git-pinned serving
    if let Some(ref git_repo) = state.git_repo {
        req.workspace_root = Some(git_repo.to_string_lossy().to_string());
    }

    match state.service.read_file(&req).await {
        Ok(resp) => (StatusCode::OK, serde_json::to_string(&resp).unwrap()),
        Err(FileServingError::NotFound) => (StatusCode::NOT_FOUND, String::new()),
        Err(FileServingError::EmptyPath) => (StatusCode::BAD_REQUEST, "Missing path".to_string()),
        Err(FileServingError::RelativePath) => {
            (StatusCode::BAD_REQUEST, "Path must be absolute".to_string())
        }
        Err(FileServingError::TooLarge { .. }) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "File too large".to_string())
        }
        Err(FileServingError::ReadError(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}
