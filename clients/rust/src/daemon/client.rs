//! HTTP client for the Detrix daemon.

use std::error::Error as StdError;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::{Error, Result};

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

impl Default for DaemonClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DaemonClient {
    /// Create a new daemon client.
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Check if the daemon is reachable.
    pub fn health_check(&self, daemon_url: &str, timeout: Duration) -> Result<()> {
        let url = format!("{}/health", daemon_url);

        let response = self.client.get(&url).timeout(timeout).send().map_err(|e| {
            Error::DaemonUnreachable {
                url: daemon_url.to_string(),
                message: e.to_string(),
            }
        })?;

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
            .map_err(|e| {
                // Get detailed error information
                let mut details = format!("{}", e);
                let err: &dyn StdError = &e;
                if let Some(source) = err.source() {
                    details.push_str(&format!(" (caused by: {})", source));
                    if let Some(inner) = source.source() {
                        details.push_str(&format!(" (inner: {})", inner));
                    }
                }
                Error::RegistrationFailed(details)
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(Error::RegistrationFailed(format!(
                "status {}: {}",
                status, body
            )));
        }

        let reg_response: RegisterResponse = response
            .json()
            .map_err(|e| Error::RegistrationFailed(format!("failed to parse response: {}", e)))?;

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
