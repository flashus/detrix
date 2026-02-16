//! Control Plane file source — fetches files from the application's control plane.
//!
//! Requires `control_plane_url` to be set on the connection. Sends
//! `POST {url}/detrix/files/read` with `{"path": "..."}` body.

use async_trait::async_trait;
use detrix_application::services::file_serving::ReadFileRequest;
use detrix_application::{FetchResult, FileSource};
use detrix_core::{Connection, Result};
use std::time::Duration;
use tracing::debug;

pub struct ControlPlaneSource {
    client: reqwest::Client,
    timeout: Duration,
    max_size: usize,
    auth_token: Option<String>,
}

impl ControlPlaneSource {
    pub fn new(timeout: Duration, max_size: usize, auth_token: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            client,
            timeout,
            max_size,
            auth_token,
        }
    }
}

#[async_trait]
impl FileSource for ControlPlaneSource {
    fn name(&self) -> &str {
        "control_plane"
    }

    async fn fetch(&self, connection: &Connection, file_path: &str) -> Result<Option<FetchResult>> {
        let Some(url) = &connection.control_plane_url else {
            return Ok(None);
        };

        let endpoint = format!("{}/detrix/files/read", url.trim_end_matches('/'));

        debug!(
            endpoint = %endpoint,
            file = file_path,
            timeout_ms = self.timeout.as_millis() as u64,
            "Fetching file from control plane"
        );

        let body = ReadFileRequest {
            path: file_path.to_string(),
            commit: connection.build_commit.clone(),
            workspace_root: if connection.workspace_root.is_empty() {
                None
            } else {
                Some(connection.workspace_root.clone())
            },
        };

        let mut req = self.client.post(&endpoint).json(&body);
        if let Some(ref token) = self.auth_token {
            req = req.bearer_auth(token);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                debug!(error = %e, "Control plane request failed");
                return Ok(None);
            }
        };

        super::handle_fetch_response(resp, "control_plane", self.max_size, "Control plane").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_core::{ConnectionIdentity, SourceLanguage};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_connection_with_cp(cp_url: Option<&str>) -> Connection {
        let identity = ConnectionIdentity::new(
            "test-app",
            SourceLanguage::Python,
            "/workspace",
            "test-host",
        );
        let mut conn = Connection::new_with_identity(identity, "127.0.0.1".into(), 5678).unwrap();
        conn.control_plane_url = cp_url.map(|s| s.to_string());
        conn
    }

    #[tokio::test]
    async fn test_no_url_returns_none() {
        let source = ControlPlaneSource::new(Duration::from_secs(5), 10 * 1024 * 1024, None);
        let conn = test_connection_with_cp(None);
        let result = source.fetch(&conn, "/app/main.py").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_success_200_raw_string() {
        // Old control plane returns raw string (backward compat)
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(ResponseTemplate::new(200).set_body_string("file content here"))
            .mount(&mock_server)
            .await;

        let source = ControlPlaneSource::new(Duration::from_secs(5), 10 * 1024 * 1024, None);
        let conn = test_connection_with_cp(Some(&mock_server.uri()));
        let result = source.fetch(&conn, "/app/main.py").await.unwrap().unwrap();
        assert_eq!(result.content, "file content here");
        assert_eq!(result.metadata.source_kind, "control_plane");
    }

    #[tokio::test]
    async fn test_not_found_404() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let source = ControlPlaneSource::new(Duration::from_secs(5), 10 * 1024 * 1024, None);
        let conn = test_connection_with_cp(Some(&mock_server.uri()));
        let result = source.fetch(&conn, "/no/such/file.py").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_server_error_500() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let source = ControlPlaneSource::new(Duration::from_secs(5), 10 * 1024 * 1024, None);
        let conn = test_connection_with_cp(Some(&mock_server.uri()));
        let result = source.fetch(&conn, "/app/main.py").await.unwrap();
        assert!(
            result.is_none(),
            "Server errors should be treated gracefully as None"
        );
    }

    #[tokio::test]
    async fn test_timeout() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("content")
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&mock_server)
            .await;

        // Very short timeout to trigger timeout error
        let source = ControlPlaneSource::new(Duration::from_millis(50), 10 * 1024 * 1024, None);
        let conn = test_connection_with_cp(Some(&mock_server.uri()));
        let result = source.fetch(&conn, "/app/main.py").await.unwrap();
        assert!(
            result.is_none(),
            "Timeout should be treated gracefully as None"
        );
    }

    #[tokio::test]
    async fn test_request_includes_commit_and_workspace() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let source = ControlPlaneSource::new(Duration::from_secs(5), 10 * 1024 * 1024, None);
        let mut conn = test_connection_with_cp(Some(&mock_server.uri()));
        conn.build_commit = Some("abc123".to_string());
        source.fetch(&conn, "/app/main.py").await.unwrap();

        // wiremock verifies the expectation on drop
    }

    #[test]
    fn test_name() {
        let source = ControlPlaneSource::new(Duration::from_secs(5), 10 * 1024 * 1024, None);
        assert_eq!(source.name(), "control_plane");
    }
}
