//! HTTP client for the Detrix daemon.

use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

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

    /// Connection name.
    #[serde(rename = "connectionId")]
    pub name: String,

    /// Process ID to attach to (required for Rust/lldb-dap AttachPid mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,

    /// Authentication token (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,

    /// Safe mode flag.
    #[serde(rename = "safeMode")]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub safe_mode: bool,
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
}

/// HTTP client for the Detrix daemon.
pub struct DaemonClient {
    /// HTTP client instance.
    client: Client,
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
    pub fn new(tls_config: Option<&TlsConfig>) -> Result<Self> {
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
        })
    }

    /// Check if the daemon is reachable.
    pub fn health_check(&self, daemon_url: &str, timeout: Duration) -> Result<()> {
        let url = format!("{}/health", daemon_url);

        let response = self
            .client
            .get(&url)
            .timeout(timeout)
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
    pub fn register(
        &self,
        daemon_url: &str,
        request: RegisterRequest,
        timeout: Duration,
    ) -> Result<String> {
        let url = format!("{}/api/v1/connections", daemon_url);

        debug!(
            "Registering connection {} at {}:{}",
            request.name, request.host, request.port
        );

        let response = self
            .client
            .post(&url)
            .timeout(timeout)
            .json(&request)
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
            "Registered with connection ID: {}",
            reg_response.connection_id
        );
        Ok(reg_response.connection_id)
    }

    /// Unregister a connection from the daemon.
    ///
    /// This is best-effort and errors are logged but not propagated.
    pub fn unregister(&self, daemon_url: &str, connection_id: &str, timeout: Duration) {
        let url = format!("{}/api/v1/connections/{}", daemon_url, connection_id);

        debug!("Unregistering connection: {}", connection_id);

        let result = self.client.delete(&url).timeout(timeout).send();

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
            pid: Some(12345),
            token: Some("secret".to_string()),
            safe_mode: true,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"host\":\"127.0.0.1\""));
        assert!(json.contains("\"port\":5678"));
        assert!(json.contains("\"language\":\"rust\""));
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
            pid: None,
            token: None,
            safe_mode: false,
        };

        let json = serde_json::to_string(&request).unwrap();
        // Token should be omitted
        assert!(!json.contains("token"));
        // pid should be omitted when None
        assert!(!json.contains("pid"));
        // safeMode should be omitted when false
        assert!(!json.contains("safeMode"));
    }
}
