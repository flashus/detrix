//! Bridge file source — fetches files from the MCP bridge's file server.
//!
//! The bridge URL is set per-session via the `X-Detrix-File-Server-Url` header
//! when an MCP client connects through the HTTP bridge. This source uses a
//! shared URL that can be updated at runtime.

use async_trait::async_trait;
use detrix_application::{FetchResult, FileSource};
use detrix_core::{Connection, Result};
use std::sync::RwLock;
use std::time::Duration;
use tracing::debug;

pub struct BridgeSource {
    client: reqwest::Client,
    /// Bridge file server URL, set per MCP session. Protected by RwLock
    /// for safe concurrent access from multiple handlers.
    bridge_url: RwLock<Option<String>>,
    max_size: usize,
}

impl BridgeSource {
    pub fn new(timeout: Duration, max_size: usize) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            client,
            bridge_url: RwLock::new(None),
            max_size,
        }
    }

    /// Set the bridge file server URL (called when MCP session starts via bridge).
    pub fn set_bridge_url(&self, url: Option<String>) {
        if let Ok(mut guard) = self.bridge_url.write() {
            *guard = url;
        }
    }

    /// Get the current bridge URL.
    pub fn bridge_url(&self) -> Option<String> {
        self.bridge_url.read().ok().and_then(|g| g.clone())
    }
}

#[async_trait]
impl FileSource for BridgeSource {
    fn name(&self) -> &str {
        "bridge"
    }

    async fn fetch(&self, connection: &Connection, file_path: &str) -> Result<Option<FetchResult>> {
        let Some(url) = self.bridge_url() else {
            return Ok(None);
        };

        let endpoint = format!("{}/detrix/files/read", url.trim_end_matches('/'));

        debug!(
            endpoint = %endpoint,
            file = file_path,
            "Fetching file from bridge"
        );

        let body = super::build_file_request_body(connection, file_path);

        let resp = match self.client.post(&endpoint).json(&body).send().await {
            Ok(r) => r,
            Err(e) => {
                debug!(error = %e, "Bridge request failed");
                return Ok(None);
            }
        };

        super::handle_fetch_response(resp, "bridge", self.max_size, "Bridge").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_core::{ConnectionIdentity, SourceLanguage};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_connection() -> Connection {
        let identity = ConnectionIdentity::new(
            "test-app",
            SourceLanguage::Python,
            "/workspace",
            "test-host",
        );
        Connection::new_with_identity(identity, "127.0.0.1".into(), 5678).unwrap()
    }

    #[tokio::test]
    async fn test_no_url_returns_none() {
        let source = BridgeSource::new(Duration::from_secs(5), 10 * 1024 * 1024);
        let conn = test_connection();
        let result = source.fetch(&conn, "/app/main.py").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_success_200_raw_string() {
        // Old bridge returns raw string (backward compat)
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(ResponseTemplate::new(200).set_body_string("bridge content"))
            .mount(&mock_server)
            .await;

        let source = BridgeSource::new(Duration::from_secs(5), 10 * 1024 * 1024);
        source.set_bridge_url(Some(mock_server.uri()));

        let conn = test_connection();
        let result = source.fetch(&conn, "/app/main.py").await.unwrap().unwrap();
        assert_eq!(result.content, "bridge content");
        assert_eq!(result.metadata.source_kind, "bridge");
    }

    #[tokio::test]
    async fn test_success_200_json_response() {
        // New bridge returns JSON with metadata
        let mock_server = MockServer::start().await;
        let json_body = serde_json::json!({
            "content": "pinned content",
            "source": "git",
            "commit": "abc123",
            "differs_from_local": true,
        });
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(json_body.to_string())
                    .insert_header("content-type", "application/json"),
            )
            .mount(&mock_server)
            .await;

        let source = BridgeSource::new(Duration::from_secs(5), 10 * 1024 * 1024);
        source.set_bridge_url(Some(mock_server.uri()));

        let conn = test_connection();
        let result = source.fetch(&conn, "/app/main.py").await.unwrap().unwrap();
        assert_eq!(result.content, "pinned content");
        assert_eq!(result.metadata.source_kind, "git");
        assert_eq!(result.metadata.commit.as_deref(), Some("abc123"));
        assert_eq!(result.metadata.differs_from_local, Some(true));
    }

    #[tokio::test]
    async fn test_not_found_404() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let source = BridgeSource::new(Duration::from_secs(5), 10 * 1024 * 1024);
        source.set_bridge_url(Some(mock_server.uri()));

        let conn = test_connection();
        let result = source.fetch(&conn, "/no/such/file.py").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_url_update() {
        let server1 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(ResponseTemplate::new(200).set_body_string("from server1"))
            .mount(&server1)
            .await;

        let server2 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(ResponseTemplate::new(200).set_body_string("from server2"))
            .mount(&server2)
            .await;

        let source = BridgeSource::new(Duration::from_secs(5), 10 * 1024 * 1024);
        let conn = test_connection();

        source.set_bridge_url(Some(server1.uri()));
        let result = source.fetch(&conn, "/app/main.py").await.unwrap().unwrap();
        assert_eq!(result.content, "from server1");

        // Update URL to server2
        source.set_bridge_url(Some(server2.uri()));
        let result = source.fetch(&conn, "/app/main.py").await.unwrap().unwrap();
        assert_eq!(result.content, "from server2");
    }

    #[tokio::test]
    async fn test_clear_url() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/detrix/files/read"))
            .respond_with(ResponseTemplate::new(200).set_body_string("content"))
            .mount(&mock_server)
            .await;

        let source = BridgeSource::new(Duration::from_secs(5), 10 * 1024 * 1024);
        let conn = test_connection();

        source.set_bridge_url(Some(mock_server.uri()));
        let result = source.fetch(&conn, "/app/main.py").await.unwrap();
        assert!(result.is_some());

        // Clear URL
        source.set_bridge_url(None);
        let result = source.fetch(&conn, "/app/main.py").await.unwrap();
        assert!(
            result.is_none(),
            "Should return None after clearing bridge URL"
        );
    }

    #[test]
    fn test_name() {
        let source = BridgeSource::new(Duration::from_secs(5), 10 * 1024 * 1024);
        assert_eq!(source.name(), "bridge");
    }
}
