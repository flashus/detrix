//! Pluggable file source implementations
//!
//! Each source implements `FileSource` from `detrix-ports`. The server tries
//! each in priority order (configured via `vfs.source_priority`) until one
//! returns file content.

mod bridge;
mod control_plane;
mod disk;

pub use bridge::BridgeSource;
pub use control_plane::ControlPlaneSource;
pub use disk::DiskSource;

use detrix_application::{FetchResult, SourceMetadata};

/// Parse an HTTP response body into a `FetchResult`.
///
/// Tries JSON first (new format with `content`, `source`, `commit`,
/// `differs_from_local` fields). Falls back to treating the whole body as
/// raw file content for backward compatibility with older bridges / control
/// planes.
///
/// `fallback_source_kind` is used when the response is not valid JSON or the
/// JSON does not contain a `source` field.
fn parse_fetch_response(text: String, fallback_source_kind: &str) -> (String, SourceMetadata) {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
        let content = json["content"].as_str().unwrap_or(&text).to_string();
        let source = json["source"]
            .as_str()
            .unwrap_or(fallback_source_kind)
            .to_string();
        let commit = json["commit"].as_str().map(|s| s.to_string());
        let differs = json["differs_from_local"].as_bool();
        (
            content,
            SourceMetadata {
                source_kind: source,
                commit,
                differs_from_local: differs,
            },
        )
    } else {
        (
            text,
            SourceMetadata {
                source_kind: fallback_source_kind.into(),
                ..Default::default()
            },
        )
    }
}

/// Handle an HTTP response from a remote file source.
///
/// Reads the body, parses it via [`parse_fetch_response`], and enforces the
/// max-size limit. Returns `Ok(None)` for 404 or non-200 status codes.
async fn handle_fetch_response(
    resp: reqwest::Response,
    fallback_source_kind: &str,
    max_size: usize,
    source_label: &str,
) -> detrix_core::Result<Option<FetchResult>> {
    match resp.status().as_u16() {
        200 => {
            let text = resp.text().await.map_err(|e| {
                detrix_core::Error::Io(format!("Failed to read response body: {}", e))
            })?;

            let (content, metadata) = parse_fetch_response(text, fallback_source_kind);

            if content.len() > max_size {
                tracing::debug!(
                    size = content.len(),
                    max = max_size,
                    "File exceeds max size, skipping"
                );
                return Ok(None);
            }
            Ok(Some(FetchResult { content, metadata }))
        }
        404 => Ok(None),
        status => {
            tracing::warn!(
                status,
                source = source_label,
                "{} returned unexpected status — file not fetched (401=auth mismatch, 403=IP blocked, 500=server error)",
                source_label
            );
            Ok(None)
        }
    }
}
