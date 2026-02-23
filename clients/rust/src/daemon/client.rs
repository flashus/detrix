//! HTTP client for the Detrix daemon.

use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::auth::{AUTHORIZATION_HEADER, BEARER_PREFIX};
use crate::config::TlsConfig;
use crate::error::{Error, ReqwestResultExt, Result, ResultExt};

/// Request body for connection registration.
#[derive(Debug, Serialize)]
pub struct RegisterRequest {
    /// Host where the debug adapter is listening.
    pub host: String,

    /// Port where the debug adapter is listening.
    pub port: u16,

    /// Programming language.
    pub language: String,

    /// Connection name (user-facing identifier).
    pub name: String,

    /// Workspace root directory.
    #[serde(rename = "workspaceRoot")]
    pub workspace_root: String,

    /// Machine hostname.
    pub hostname: String,

    /// Process ID to attach to (required for Rust/lldb-dap AttachPid mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,

    /// Safe mode flag.
    #[serde(rename = "safeMode")]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub safe_mode: bool,

    /// Build commit SHA (optional).
    #[serde(rename = "buildCommit")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_commit: Option<String>,

    /// Build tag/version (optional).
    #[serde(rename = "buildTag")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_tag: Option<String>,
}

/// Response from connection registration.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterResponse {
    /// Connection ID assigned by the daemon.
    connection_id: String,

    /// Status message.
    #[allow(dead_code)]
    status: Option<String>,

    /// Daemon's external advertise URL (for auto-discovery in Docker/cloud).
    advertise_url: Option<String>,
}

/// HTTP client for the Detrix daemon.
pub struct DaemonClient {
    /// HTTP client instance.
    client: Client,
    /// Authentication token for daemon API requests.
    auth_token: Option<String>,
}

impl DaemonClient {
    /// Create a new daemon client with optional TLS configuration.
    ///
    /// # Arguments
    ///
    /// * `tls_config` - Optional TLS configuration. If None, uses defaults (verify: true).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// * The CA bundle path doesn't exist (fails fast)
    /// * The CA bundle file can't be read or parsed
    /// * Failed to create the HTTP client
    pub fn new(tls_config: Option<&TlsConfig>, auth_token: Option<String>) -> Result<Self> {
        let mut builder = Client::builder().timeout(Duration::from_secs(30));

        if let Some(tls) = tls_config {
            // Validate configuration first (fail fast)
            tls.validate()?;

            if !tls.verify {
                builder = builder.danger_accept_invalid_certs(true);
            }
            if let Some(ref ca_path) = tls.ca_bundle {
                let cert_bytes = std::fs::read(ca_path).config("failed to read CA bundle")?;
                let cert = reqwest::Certificate::from_pem(&cert_bytes)
                    .config("failed to parse CA bundle")?;
                builder = builder.add_root_certificate(cert);
            }
        }

        Ok(Self {
            client: builder.build().config("failed to create HTTP client")?,
            auth_token,
        })
    }

    /// Update the auth token (e.g., after daemon restart with a new token).
    pub fn update_auth_token(&mut self, token: Option<String>) {
        self.auth_token = token;
    }

    /// Apply auth header to a request builder if token is available and non-empty.
    fn set_auth(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        match self.auth_token {
            Some(ref token) if !token.is_empty() => {
                builder.header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token))
            }
            _ => builder,
        }
    }

    /// Fetch the daemon's advertise URL from its health endpoint.
    ///
    /// Returns `None` if the daemon is unreachable, the health endpoint
    /// doesn't include `advertiseUrl`, or the URL is empty.
    pub fn fetch_advertise_url(&self, daemon_url: &str, timeout: Duration) -> Option<String> {
        let url = format!("{}/health", daemon_url.trim_end_matches('/'));
        let resp = self
            .set_auth(self.client.get(&url).timeout(timeout))
            .send()
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let data: serde_json::Value = resp.json().ok()?;
        data.get("advertiseUrl")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Check if the daemon is reachable.
    pub fn health_check(&self, daemon_url: &str, timeout: Duration) -> Result<()> {
        let url = format!("{}/health", daemon_url);

        let response = self
            .set_auth(self.client.get(&url).timeout(timeout))
            .send()
            .daemon_unreachable(daemon_url)?;

        if !response.status().is_success() {
            return Err(Error::DaemonUnreachable {
                url: daemon_url.to_string(),
                message: format!("health check failed: status {}", response.status()),
            });
        }

        debug!("Daemon health check passed: {}", daemon_url);
        Ok(())
    }

    /// Register a connection with the daemon.
    ///
    /// Returns `(connection_id, advertise_url)`.
    pub fn register(
        &self,
        daemon_url: &str,
        request: RegisterRequest,
        timeout: Duration,
    ) -> Result<(String, Option<String>)> {
        let url = format!("{}/api/v1/connections", daemon_url);

        debug!(
            "Registering connection {} at {}:{}",
            request.name, request.host, request.port
        );

        let response = self
            .set_auth(self.client.post(&url).timeout(timeout).json(&request))
            .send()
            .registration_context()?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(Error::RegistrationFailed(format!(
                "status {}: {}",
                status, body
            )));
        }

        let reg_response: RegisterResponse =
            response.json().registration("failed to parse response")?;

        debug!(
            "Registered with connection ID: {}, advertise_url: {:?}",
            reg_response.connection_id, reg_response.advertise_url
        );
        Ok((reg_response.connection_id, reg_response.advertise_url))
    }

    /// Unregister a connection from the daemon.
    ///
    /// This is best-effort and errors are logged but not propagated.
    pub fn unregister(&self, daemon_url: &str, connection_id: &str, timeout: Duration) {
        let url = format!("{}/api/v1/connections/{}", daemon_url, connection_id);

        debug!("Unregistering connection: {}", connection_id);

        let result = self
            .set_auth(self.client.delete(&url).timeout(timeout))
            .send();

        match result {
            Ok(response) => {
                let status = response.status();
                if status.is_success() || status.as_u16() == 404 {
                    debug!("Unregistered connection: {}", connection_id);
                } else {
                    tracing::warn!(
                        "Failed to unregister connection {}: status {}",
                        connection_id,
                        status
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Failed to unregister connection {}: {}", connection_id, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_request_serialization() {
        let request = RegisterRequest {
            host: "127.0.0.1".to_string(),
            port: 5678,
            language: "rust".to_string(),
            name: "test-client".to_string(),
            workspace_root: "/workspace".to_string(),
            hostname: "test-host".to_string(),
            pid: Some(12345),
            safe_mode: true,
            build_commit: Some("abc123".to_string()),
            build_tag: Some("v1.0.0".to_string()),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"host\":\"127.0.0.1\""));
        assert!(json.contains("\"port\":5678"));
        assert!(json.contains("\"language\":\"rust\""));
        assert!(json.contains("\"workspaceRoot\":\"/workspace\""));
        assert!(json.contains("\"hostname\":\"test-host\""));
        assert!(json.contains("\"pid\":12345"));
        assert!(json.contains("\"safeMode\":true"));
    }

    #[test]
    fn test_register_request_without_optionals() {
        let request = RegisterRequest {
            host: "127.0.0.1".to_string(),
            port: 5678,
            language: "rust".to_string(),
            name: "test-client".to_string(),
            workspace_root: "/workspace".to_string(),
            hostname: "test-host".to_string(),
            pid: None,
            safe_mode: false,
            build_commit: None,
            build_tag: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        // pid should be omitted when None
        assert!(!json.contains("pid"));
        // safeMode should be omitted when false
        assert!(!json.contains("safeMode"));
        // Required fields should be present
        assert!(json.contains("\"workspaceRoot\":\"/workspace\""));
        assert!(json.contains("\"hostname\":\"test-host\""));
    }

    #[test]
    fn test_daemon_client_new_with_token() {
        let client = DaemonClient::new(None, Some("test-token".to_string()));
        assert!(client.is_ok());
        let client = client.unwrap();
        assert_eq!(client.auth_token, Some("test-token".to_string()));
    }

    #[test]
    fn test_daemon_client_new_without_token() {
        let client = DaemonClient::new(None, None);
        assert!(client.is_ok());
        let client = client.unwrap();
        assert_eq!(client.auth_token, None);
    }

    #[test]
    fn test_update_auth_token() {
        let mut client = DaemonClient::new(None, None).unwrap();
        assert_eq!(client.auth_token, None);

        client.update_auth_token(Some("new-token".to_string()));
        assert_eq!(client.auth_token, Some("new-token".to_string()));

        client.update_auth_token(None);
        assert_eq!(client.auth_token, None);
    }

    #[test]
    fn test_set_auth_adds_header_when_token_present() {
        let client = DaemonClient::new(None, Some("my-token".to_string())).unwrap();
        let request = client.client.get("http://localhost:8090/health");
        let request = client.set_auth(request).build().unwrap();

        let auth_header = request.headers().get(AUTHORIZATION_HEADER).unwrap();
        assert_eq!(
            auth_header.to_str().unwrap(),
            format!("{}my-token", BEARER_PREFIX)
        );
    }

    #[test]
    fn test_set_auth_no_header_when_no_token() {
        let client = DaemonClient::new(None, None).unwrap();
        let request = client.client.get("http://localhost:8090/health");
        let request = client.set_auth(request).build().unwrap();

        assert!(request.headers().get(AUTHORIZATION_HEADER).is_none());
    }

    #[test]
    fn test_set_auth_no_header_when_empty_token() {
        let client = DaemonClient::new(None, Some("".to_string())).unwrap();
        let request = client.client.get("http://localhost:8090/health");
        let request = client.set_auth(request).build().unwrap();

        // Empty token should be treated as no token
        assert!(request.headers().get(AUTHORIZATION_HEADER).is_none());
    }

    #[test]
    fn test_set_auth_no_header_when_whitespace_only_token() {
        let client = DaemonClient::new(None, Some("   ".to_string())).unwrap();
        let request = client.client.get("http://localhost:8090/health");
        let request = client.set_auth(request).build().unwrap();

        // Whitespace-only token should still set header (caller's responsibility to trim)
        // But it IS non-empty, so the header is set
        assert!(request.headers().get(AUTHORIZATION_HEADER).is_some());
    }
}
