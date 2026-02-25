//! HTTP implementation of the RemoteAppControl port
//!
//! Sends HTTP POST requests to remote apps' control planes for wake/sleep.
//! This is the infrastructure adapter that implements the port trait defined
//! in `detrix-ports`.

use detrix_application::ports::{RemoteAppControl, RemoteSleepResponse, RemoteWakeResponse};
use detrix_config::constants::{AUTHORIZATION_HEADER, BEARER_PREFIX};
use detrix_core::Error;
use tracing::{debug, warn};

/// HTTP-based implementation of RemoteAppControl.
///
/// Uses `reqwest` to send POST requests to `{app_url}/detrix/wake` and
/// `{app_url}/detrix/sleep` endpoints.
pub struct HttpRemoteAppControl {
    client: reqwest::Client,
}

impl Default for HttpRemoteAppControl {
    fn default() -> Self {
        Self::new(detrix_config::constants::DEFAULT_REMOTE_APP_TIMEOUT_MS).unwrap_or_else(|e| {
            tracing::warn!("Failed to create HTTP client with TLS: {e}, using plain client");
            Self {
                client: reqwest::Client::new(),
            }
        })
    }
}

impl HttpRemoteAppControl {
    pub fn new(timeout_ms: u64) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            // Disable automatic redirect following to prevent SSRF via redirect
            // to blocked destinations (e.g., cloud metadata service).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::RemoteApp(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self { client })
    }
}

/// Validate an IPv4 address against blocked ranges.
///
/// Blocks private, link-local, and metadata service IPs.
/// Allows loopback (127.0.0.0/8) for local development.
fn validate_ipv4(ipv4: std::net::Ipv4Addr) -> detrix_core::Result<()> {
    let octets = ipv4.octets();

    // Allow loopback (127.0.0.0/8) for local development
    if octets[0] == 127 {
        return Ok(());
    }

    // Block cloud metadata service (169.254.0.0/16 — link-local)
    if octets[0] == 169 && octets[1] == 254 {
        return Err(Error::RemoteApp(format!(
            "URL host '{}' points to link-local/metadata service address, which is blocked.",
            ipv4
        )));
    }

    // Block private IP ranges
    if octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
    {
        return Err(Error::RemoteApp(format!(
            "URL host '{}' is a private IP address, which is blocked for security.",
            ipv4
        )));
    }

    Ok(())
}

/// Validate a remote app URL to prevent SSRF attacks.
///
/// Rejects:
/// - Non-HTTP/HTTPS schemes (file://, gopher://, ftp://, etc.)
/// - Cloud metadata service IPs (169.254.169.254)
/// - Private/reserved IP ranges (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
/// - IPv4-mapped IPv6 addresses (e.g., ::ffff:169.254.169.254)
/// - IPv6 link-local (fe80::/10) and unique-local (fc00::/7) addresses
///
/// Allows:
/// - localhost hostname and 127.0.0.0/8 (common for local development)
/// - Regular domain names (DNS resolution happens at request time)
fn validate_app_url(url: &str) -> detrix_core::Result<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| Error::RemoteApp(format!("Invalid URL '{}': {}", url, e)))?;

    // Check scheme
    let scheme = parsed.scheme();
    if !detrix_config::constants::ALLOWED_REMOTE_APP_URL_SCHEMES.contains(&scheme) {
        return Err(Error::RemoteApp(format!(
            "URL scheme '{}' is not allowed. Only HTTP and HTTPS are permitted.",
            scheme
        )));
    }

    // Check host for blocked IP ranges
    if let Some(host) = parsed.host_str() {
        // Allow "localhost" hostname
        if host == "localhost" {
            return Ok(());
        }

        // Parse as IP address and check blocked ranges
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            match ip {
                std::net::IpAddr::V4(ipv4) => {
                    validate_ipv4(ipv4)?;
                }
                std::net::IpAddr::V6(ipv6) => {
                    // Allow loopback (::1) for local development
                    if ipv6.is_loopback() {
                        return Ok(());
                    }

                    // Block IPv4-mapped IPv6 (e.g., ::ffff:10.0.0.1, ::ffff:169.254.169.254)
                    if let Some(mapped_v4) = ipv6.to_ipv4_mapped() {
                        return validate_ipv4(mapped_v4);
                    }

                    // Block link-local (fe80::/10)
                    if (ipv6.segments()[0] & 0xffc0) == 0xfe80 {
                        return Err(Error::RemoteApp(format!(
                            "URL host '{}' is a link-local IPv6 address, which is blocked.",
                            host
                        )));
                    }

                    // Block unique-local (fc00::/7) — IPv6 private range
                    if (ipv6.segments()[0] & 0xfe00) == 0xfc00 {
                        return Err(Error::RemoteApp(format!(
                            "URL host '{}' is a private IPv6 address, which is blocked for security.",
                            host
                        )));
                    }
                }
            }
        }
    }

    Ok(())
}

#[tonic::async_trait]
impl RemoteAppControl for HttpRemoteAppControl {
    async fn wake_app(
        &self,
        app_url: &str,
        daemon_url: Option<&str>,
        auth_token: Option<&str>,
    ) -> detrix_core::Result<RemoteWakeResponse> {
        validate_app_url(app_url)?;

        let app_url = app_url.trim_end_matches('/');
        let wake_url = format!("{}/detrix/wake", app_url);

        // Build request body
        let mut body = serde_json::Map::new();
        if let Some(daemon) = daemon_url {
            body.insert(
                "daemon_url".to_string(),
                serde_json::Value::String(daemon.to_string()),
            );
        }

        let mut request = self
            .client
            .post(&wake_url)
            .json(&serde_json::Value::Object(body));

        if let Some(token) = auth_token {
            request = request.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token));
        }

        let response = request.send().await.map_err(|e| {
            Error::RemoteApp(format!(
                "Failed to reach app at {}: {}. Is the app running and accessible?",
                wake_url, e
            ))
        })?;

        let status_code = response.status();
        let response_text = response.text().await.unwrap_or_default();

        debug!(
            "wake: Response from {}: status={}, body={}",
            wake_url, status_code, response_text
        );

        if !status_code.is_success() {
            return Err(Error::RemoteApp(format!(
                "App at {} returned error {}: {}",
                wake_url, status_code, response_text
            )));
        }

        let wake_response: serde_json::Value = match serde_json::from_str(&response_text) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    url = %wake_url,
                    error = %e,
                    "wake: Failed to parse JSON response, treating as empty"
                );
                serde_json::Value::default()
            }
        };

        let connection_id = wake_response
            .get("connection_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let debug_port = wake_response
            .get("debug_port")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);
        let status = wake_response
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("ok")
            .to_string();
        let daemon_url = wake_response
            .get("daemon_url")
            .and_then(|v| v.as_str())
            .map(String::from);

        Ok(RemoteWakeResponse {
            app_url: app_url.to_string(),
            status,
            connection_id,
            debug_port,
            daemon_url,
        })
    }

    async fn sleep_app(
        &self,
        app_url: &str,
        auth_token: Option<&str>,
    ) -> detrix_core::Result<RemoteSleepResponse> {
        validate_app_url(app_url)?;

        let app_url = app_url.trim_end_matches('/');
        let sleep_url = format!("{}/detrix/sleep", app_url);

        let mut request = self.client.post(&sleep_url);

        if let Some(token) = auth_token {
            request = request.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token));
        }

        let response = request.send().await.map_err(|e| {
            Error::RemoteApp(format!(
                "Failed to reach app at {}: {}. Is the app running and accessible?",
                sleep_url, e
            ))
        })?;

        let status_code = response.status();
        let response_text = response.text().await.unwrap_or_default();

        debug!(
            "sleep: Response from {}: status={}, body={}",
            sleep_url, status_code, response_text
        );

        if !status_code.is_success() {
            return Err(Error::RemoteApp(format!(
                "App at {} returned error {}: {}",
                sleep_url, status_code, response_text
            )));
        }

        let sleep_response: serde_json::Value = match serde_json::from_str(&response_text) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    url = %sleep_url,
                    error = %e,
                    "sleep: Failed to parse JSON response, treating as empty"
                );
                serde_json::Value::default()
            }
        };

        let status = sleep_response
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("sleeping")
            .to_string();

        Ok(RemoteSleepResponse {
            app_url: app_url.to_string(),
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_application::ports::RemoteAppControl;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_wake_url_construction() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/wake"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let result = ctrl.wake_app(&mock_server.uri(), None, None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status, "ok");
    }

    #[tokio::test]
    async fn test_wake_trailing_slash_trimmed() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/wake"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let url_with_slash = format!("{}/", mock_server.uri());
        let result = ctrl.wake_app(&url_with_slash, None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wake_with_auth_token() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/wake"))
            .and(header(
                AUTHORIZATION_HEADER,
                &format!("{}test-token-123", BEARER_PREFIX),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let result = ctrl
            .wake_app(&mock_server.uri(), None, Some("test-token-123"))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wake_with_daemon_url() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/wake"))
            .and(body_json(
                serde_json::json!({"daemon_url": "http://localhost:8090"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let result = ctrl
            .wake_app(&mock_server.uri(), Some("http://localhost:8090"), None)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wake_parses_response_fields() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/wake"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "started",
                "connection_id": "abc123",
                "debug_port": 5678
            })))
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let result = ctrl.wake_app(&mock_server.uri(), None, None).await.unwrap();

        assert_eq!(result.status, "started");
        assert_eq!(result.connection_id.as_deref(), Some("abc123"));
        assert_eq!(result.debug_port, Some(5678));
        assert!(result.daemon_url.is_none());
    }

    #[tokio::test]
    async fn test_wake_parses_daemon_url() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/wake"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "awake",
                "connection_id": "abc123",
                "debug_port": 5678,
                "daemon_url": "http://localhost:8095"
            })))
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let result = ctrl.wake_app(&mock_server.uri(), None, None).await.unwrap();

        assert_eq!(result.daemon_url.as_deref(), Some("http://localhost:8095"));
    }

    #[tokio::test]
    async fn test_wake_error_on_http_500() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/wake"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let result = ctrl.wake_app(&mock_server.uri(), None, None).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, detrix_core::Error::RemoteApp(_)));
    }

    #[tokio::test]
    async fn test_sleep_url_construction() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/sleep"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "sleeping"})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let result = ctrl.sleep_app(&mock_server.uri(), None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status, "sleeping");
    }

    #[tokio::test]
    async fn test_sleep_with_auth_token() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/sleep"))
            .and(header(
                AUTHORIZATION_HEADER,
                &format!("{}my-secret", BEARER_PREFIX),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "sleeping"})),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let result = ctrl.sleep_app(&mock_server.uri(), Some("my-secret")).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_sleep_error_on_http_failure() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/detrix/sleep"))
            .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
            .mount(&mock_server)
            .await;

        let ctrl = HttpRemoteAppControl::default();
        let result = ctrl.sleep_app(&mock_server.uri(), None).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, detrix_core::Error::RemoteApp(_)));
    }
}
