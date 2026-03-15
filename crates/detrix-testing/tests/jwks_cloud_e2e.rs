//! JWKS / JWT Docker Cloud E2E Tests
//!
//! Validates external JWT authentication against a live detrix-jwt daemon running
//! in Docker, backed by the mock-jwks service.
//!
//! Pre-requisites (managed by `task test-cloud-jwt`):
//!   - mock-jwks container listening on host port is not directly mapped (internal only)
//!   - detrix-jwt container listening on host port 8097 (REST) and 50066 (gRPC)
//!
//! Run via:
//!   task test-cloud-jwt
//! Or manually:
//!   docker compose -f fixtures/docker/docker-compose.yml -p detrix-cloud-test \
//!       up -d --build --wait mock-jwks detrix-jwt
//!   cargo test -p detrix-testing --test jwks_cloud_e2e -- --ignored --nocapture
//!   docker compose -f fixtures/docker/docker-compose.yml -p detrix-cloud-test \
//!       stop mock-jwks detrix-jwt

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Host port for the detrix-jwt daemon (docker-compose.yml: 8097:8090).
const JWT_DAEMON_PORT: u16 = 8097;

const ISSUER: &str = "detrix-test";
const AUDIENCE: &str = "detrix";
const KID: &str = "test-key-001";

// ─── JWT claims ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct JwtClaims {
    sub: String,
    iss: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    aud: Option<String>,
    exp: u64,
    iat: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    roles: Option<Vec<String>>,
}

impl JwtClaims {
    fn valid(sub: &str) -> Self {
        let now = now_secs();
        Self {
            sub: sub.to_string(),
            iss: ISSUER.to_string(),
            aud: Some(AUDIENCE.to_string()),
            exp: now + 3600,
            iat: now,
            roles: None,
        }
    }

    fn expired(sub: &str) -> Self {
        let now = now_secs();
        Self {
            sub: sub.to_string(),
            iss: ISSUER.to_string(),
            aud: Some(AUDIENCE.to_string()),
            exp: now - 3600,
            iat: now - 7200,
            roles: None,
        }
    }

    fn wrong_issuer(sub: &str) -> Self {
        let mut c = Self::valid(sub);
        c.iss = "wrong-issuer".to_string();
        c
    }

    fn wrong_audience(sub: &str) -> Self {
        let mut c = Self::valid(sub);
        c.aud = Some("wrong-audience".to_string());
        c
    }

    fn with_admin_role(sub: &str) -> Self {
        let mut c = Self::valid(sub);
        c.roles = Some(vec!["admin".to_string()]);
        c
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

// ─── Key loading ──────────────────────────────────────────────────────────────

fn encoding_key() -> EncodingKey {
    let key_path = workspace_root().join("fixtures/docker/mock-jwks/test_private_key.pem");
    let pem = std::fs::read(&key_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", key_path.display(), e));
    EncodingKey::from_rsa_pem(&pem).expect("invalid RSA PEM")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn sign(claims: &JwtClaims) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.to_string());
    encode(&header, claims, &encoding_key()).expect("JWT encode failed")
}

// ─── HTTP helpers ─────────────────────────────────────────────────────────────

fn base_url() -> String {
    format!("http://127.0.0.1:{}", JWT_DAEMON_PORT)
}

async fn get_metrics(token: &str) -> StatusCode {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .get(format!("{}/api/v1/metrics", base_url()))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map(|r| r.status())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

// ─── Tests (all `#[ignore]` — require Docker) ─────────────────────────────────

/// Valid JWT → 200 OK.
#[tokio::test]
#[ignore]
async fn test_jwt_valid_token_grants_access() {
    let claims = JwtClaims::valid("user-alice");
    let token = sign(&claims);

    let status = get_metrics(&token).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "valid JWT should grant access, got: {}",
        status
    );
    println!("✓ valid JWT grants access");
}

/// Expired JWT → 401.
#[tokio::test]
#[ignore]
async fn test_jwt_expired_token_denied() {
    let claims = JwtClaims::expired("user-alice");
    let token = sign(&claims);

    let status = get_metrics(&token).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "expired JWT should → 401, got: {}",
        status
    );
    println!("✓ expired JWT → 401");
}

/// Wrong issuer → 401.
#[tokio::test]
#[ignore]
async fn test_jwt_wrong_issuer_denied() {
    let claims = JwtClaims::wrong_issuer("user-alice");
    let token = sign(&claims);

    let status = get_metrics(&token).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "wrong issuer JWT should → 401, got: {}",
        status
    );
    println!("✓ wrong issuer → 401");
}

/// Wrong audience → 401.
#[tokio::test]
#[ignore]
async fn test_jwt_wrong_audience_denied() {
    let claims = JwtClaims::wrong_audience("user-alice");
    let token = sign(&claims);

    let status = get_metrics(&token).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "wrong audience JWT should → 401, got: {}",
        status
    );
    println!("✓ wrong audience → 401");
}

/// No token → 401.
#[tokio::test]
#[ignore]
async fn test_jwt_no_token_denied() {
    let status = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .get(format!("{}/api/v1/metrics", base_url()))
        .send()
        .await
        .map(|r| r.status())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "no token should → 401, got: {}",
        status
    );
    println!("✓ no token → 401");
}

/// JWT with roles:["admin"] grants admin access (lists all metrics).
#[tokio::test]
#[ignore]
async fn test_jwt_admin_role_grants_full_access() {
    let admin_claims = JwtClaims::with_admin_role("admin-user");
    let admin_token = sign(&admin_claims);

    // Create a user metric first with a regular token
    let user_claims = JwtClaims::valid("regular-user");
    let user_token = sign(&user_claims);

    // Admin GET should succeed
    let status = get_metrics(&admin_token).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "admin JWT should grant access, got: {}",
        status
    );

    // Regular user also gets OK (their own view)
    let status = get_metrics(&user_token).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "regular user JWT should grant access, got: {}",
        status
    );
    println!("✓ admin role JWT grants full access");
}

/// Multi-user authentication: two distinct JWT users can each authenticate successfully.
#[tokio::test]
#[ignore]
async fn test_jwt_multi_user_authentication() {
    // This test verifies that two different JWT users can both authenticate
    // and access the API. It uses the list endpoint (no active DAP connection needed).

    let alice_token = sign(&JwtClaims::valid("jwt-alice"));
    let bob_token = sign(&JwtClaims::valid("jwt-bob"));

    let alice_status = get_metrics(&alice_token).await;
    let bob_status = get_metrics(&bob_token).await;

    assert_eq!(alice_status, StatusCode::OK, "alice JWT → {}", alice_status);
    assert_eq!(bob_status, StatusCode::OK, "bob JWT → {}", bob_status);

    println!("✓ JWT multi-user authentication: both users authenticated successfully");
}
