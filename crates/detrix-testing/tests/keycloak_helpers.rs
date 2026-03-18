//! Shared helpers for Keycloak E2E tests.
//!
//! Provides `KeycloakTokenClient` for ROPC token acquisition and
//! `DetrixClient` for authenticated REST API calls against the
//! detrix-keycloak Docker service.

// Each test file includes this module independently via `mod keycloak_helpers;`,
// so not every test uses every helper — suppress dead_code warnings.
#![allow(dead_code)]

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ─── Constants ──────────────────────────────────────────────────────────────
//
// Port mappings come from: fixtures/docker/docker-compose.keycloak.yml
// User credentials come from: fixtures/docker/keycloak/detrix-realm.json

/// Host port for the Keycloak server (docker-compose.keycloak.yml: 18080:8080).
pub const KEYCLOAK_PORT: u16 = 18080;

/// Host port for the detrix-keycloak daemon REST API (docker-compose.keycloak.yml: 8098:8090).
pub const DETRIX_HTTP_PORT: u16 = 8098;

/// Host port for the detrix-keycloak daemon gRPC (docker-compose.keycloak.yml: 50067:50061).
pub const DETRIX_GRPC_PORT: u16 = 50067;

/// Keycloak realm name (detrix-realm.json: "realm": "detrix").
pub const REALM: &str = "detrix";

/// Keycloak OIDC client ID (detrix-realm.json: clients[].clientId).
pub const CLIENT_ID: &str = "detrix-api";

/// Internal issuer URL (as seen inside Docker network, matches JWT `iss` claim).
pub const ISSUER_INTERNAL: &str = "http://keycloak:8080/realms/detrix";

/// User credentials (detrix-realm.json: users[].username / credentials[].value).
pub const ALICE_USER: &str = "alice";
pub const ALICE_PASS: &str = "alice-pass";
pub const BOB_USER: &str = "bob";
pub const BOB_PASS: &str = "bob-pass";
pub const ADMIN_USER: &str = "admin-user";
pub const ADMIN_PASS: &str = "admin-pass";

/// Admin role value configured in detrix-keycloak.toml (jwt.admin_role_value).
pub const ADMIN_ROLE: &str = "detrix-admin";

/// Go test app control plane URL (Docker-internal, used by daemon).
pub const GO_APP_URL: &str = "http://test-app-go:8091";

/// Go fixture file path inside the container.
pub const GO_FILE: &str = "/src/fixtures/go/detrix_example_app.go";

/// Go logpoint line — `symbol` variable, first safe line in scope.
/// go_lines::MAIN_LINE (101) + offset 27 = line 128.
pub const GO_LOGPOINT_LINE: u32 = 128;

// ─── Token types ────────────────────────────────────────────────────────────

/// Token response from Keycloak ROPC grant.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
    pub token_type: String,
}

// ─── KeycloakTokenClient ────────────────────────────────────────────────────

/// Acquires tokens from Keycloak via Resource Owner Password Credentials grant.
pub struct KeycloakTokenClient {
    client: Client,
    token_url: String,
}

impl KeycloakTokenClient {
    pub fn new() -> Self {
        let token_url = format!(
            "http://127.0.0.1:{}/realms/{}/protocol/openid-connect/token",
            KEYCLOAK_PORT, REALM
        );
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("build reqwest client"),
            token_url,
        }
    }

    /// Acquire a token via ROPC grant.
    pub async fn get_token(&self, username: &str, password: &str) -> TokenResponse {
        let resp = self
            .client
            .post(&self.token_url)
            .form(&[
                ("grant_type", "password"),
                ("client_id", CLIENT_ID),
                ("username", username),
                ("password", password),
            ])
            .send()
            .await
            .expect("ROPC token request failed");

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        assert_eq!(
            status,
            StatusCode::OK,
            "ROPC token request for '{}' failed ({}): {}",
            username,
            status,
            body
        );

        serde_json::from_str::<TokenResponse>(&body).expect("parse token response")
    }

    /// Refresh an access token using a refresh token.
    pub async fn refresh_token(&self, refresh_token: &str) -> TokenResponse {
        let resp = self
            .client
            .post(&self.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .expect("refresh token request failed");

        resp.json::<TokenResponse>()
            .await
            .expect("parse refresh token response")
    }
}

// ─── DetrixClient ───────────────────────────────────────────────────────────

/// Connection info from list connections response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionResponse {
    pub connection_id: String,
    pub language: String,
    /// Status can be a string ("connected") or integer (3).
    #[serde(deserialize_with = "deserialize_status")]
    pub status: String,
}

fn deserialize_status<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: serde_json::Value = serde::Deserialize::deserialize(deserializer)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        _ => Ok(v.to_string()),
    }
}

/// Authenticated HTTP client for Detrix REST API.
pub struct DetrixClient {
    client: Client,
    base_url: String,
    token: String,
}

/// Metric creation request body (matches REST API camelCase).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMetricBody {
    pub name: String,
    pub connection_id: String,
    pub location: LocationBody,
    pub expressions: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct LocationBody {
    pub file: String,
    pub line: u32,
}

/// Metric info from list/get responses (subset of fields we care about).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricResponse {
    pub metric_id: Option<serde_json::Value>,
    pub name: String,
    #[serde(default)]
    pub owner_id: Option<String>,
}

impl DetrixClient {
    pub fn new(token: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("build reqwest client"),
            base_url: format!("http://127.0.0.1:{}", DETRIX_HTTP_PORT),
            token: token.to_string(),
        }
    }

    /// Update the bearer token (e.g. after refresh).
    pub fn set_token(&mut self, token: &str) {
        self.token = token.to_string();
    }

    /// GET /api/v1/metrics — returns list of metrics visible to the authenticated user.
    pub async fn list_metrics(&self) -> (StatusCode, Vec<MetricResponse>) {
        let resp = self
            .client
            .get(format!("{}/api/v1/metrics", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .expect("list_metrics request failed");

        let status = resp.status();
        if status != StatusCode::OK {
            return (status, vec![]);
        }

        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        // REST API returns array of metrics at top level
        let metrics: Vec<MetricResponse> =
            serde_json::from_value(body.get("metrics").cloned().unwrap_or_else(|| {
                // Might be a direct array
                if body.is_array() {
                    body.clone()
                } else {
                    serde_json::Value::Array(vec![])
                }
            }))
            .unwrap_or_default();

        (status, metrics)
    }

    /// POST /api/v1/metrics — create a metric. Returns (status, metric_id_or_error).
    pub async fn create_metric(&self, body: &CreateMetricBody) -> (StatusCode, String) {
        let resp = self
            .client
            .post(format!("{}/api/v1/metrics", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .json(body)
            .send()
            .await
            .expect("create_metric request failed");

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        // On success, response contains metricId
        if status == StatusCode::OK || status == StatusCode::CREATED {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            let id = v
                .get("metricId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (status, id)
        } else {
            (status, text)
        }
    }

    /// DELETE /api/v1/metrics/:id — delete a metric by numeric ID.
    pub async fn delete_metric(&self, id: u64) -> StatusCode {
        let resp = self
            .client
            .delete(format!("{}/api/v1/metrics/{}", self.base_url, id))
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .expect("delete_metric request failed");

        resp.status()
    }

    /// Find a metric by name and delete it. Returns the status code (or 404 if not found).
    pub async fn delete_metric_by_name(&self, name: &str) -> StatusCode {
        let (status, metrics) = self.list_metrics().await;
        if status != StatusCode::OK {
            return status;
        }
        if let Some(m) = metrics.iter().find(|m| m.name == name) {
            if let Some(id) = m.metric_id.as_ref().and_then(|v| v.as_u64()) {
                return self.delete_metric(id).await;
            }
        }
        StatusCode::NOT_FOUND
    }

    /// GET /health — check if the daemon is healthy (no auth needed).
    pub async fn health(&self) -> StatusCode {
        self.client
            .get(format!("{}/health", self.base_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map(|r| r.status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Generic GET with auth.
    pub async fn get(&self, path: &str) -> (StatusCode, String) {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .expect("GET request failed");

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        (status, text)
    }

    /// POST /api/v1/wake — wake an app (triggers debugger start + connection creation).
    pub async fn wake(&self, app_url: &str) -> (StatusCode, String) {
        let resp = self
            .client
            .post(format!("{}/api/v1/wake", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&serde_json::json!({"appUrl": app_url}))
            .send()
            .await
            .expect("wake request failed");

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        (status, text)
    }

    /// GET /api/v1/connections — list all connections.
    pub async fn list_connections(&self) -> (StatusCode, Vec<ConnectionResponse>) {
        let resp = self
            .client
            .get(format!("{}/api/v1/connections", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .expect("list_connections request failed");

        let status = resp.status();
        if status != StatusCode::OK {
            return (status, vec![]);
        }

        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        let connections: Vec<ConnectionResponse> = if body.is_array() {
            serde_json::from_value(body).unwrap_or_default()
        } else {
            serde_json::from_value(
                body.get("connections")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(vec![])),
            )
            .unwrap_or_default()
        };

        (status, connections)
    }

    /// Poll for a connected connection of the given language. Returns connection_id.
    pub async fn poll_for_connection(&self, language: &str, timeout: Duration) -> Option<String> {
        let start = std::time::Instant::now();
        loop {
            let (status, connections) = self.list_connections().await;
            if status == StatusCode::OK {
                if let Some(conn) = connections.iter().find(|c| {
                    c.language == language && (c.status == "connected" || c.status == "3")
                }) {
                    return Some(conn.connection_id.clone());
                }
            }
            if start.elapsed() > timeout {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

/// Decode a JWT payload without verification (for inspecting claims in tests).
pub fn decode_jwt_payload(token: &str) -> serde_json::Value {
    let parts: Vec<&str> = token.split('.').collect();
    assert!(parts.len() >= 2, "Invalid JWT format");
    let payload = base64_url_decode(parts[1]);
    serde_json::from_slice(&payload).expect("parse JWT payload")
}

fn base64_url_decode(input: &str) -> Vec<u8> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD
        .decode(input)
        .expect("base64url decode failed")
}
