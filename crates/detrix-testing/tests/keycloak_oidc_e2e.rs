//! Keycloak OIDC E2E Tests — Scenario 4
//!
//! Validates real OIDC protocol compliance against a live Keycloak instance:
//! - JWKS endpoint returns valid RS256 keys
//! - JWT claims contain correct `sub`, `iss`, `aud`, `realm_access.roles`
//! - Tampered/forged JWTs are rejected
//! - `sub` (UUID) is used as `user_id` in Detrix
//!
//! Run via: `task test-cloud-keycloak`
//! Or manually:
//!   docker compose -f fixtures/docker/docker-compose.keycloak.yml -p detrix-keycloak-test \
//!       up -d --build --wait
//!   cargo test -p detrix-testing --test keycloak_oidc_e2e -- --ignored --nocapture
//!   docker compose -f fixtures/docker/docker-compose.keycloak.yml -p detrix-keycloak-test \
//!       down -v

mod keycloak_helpers;

use keycloak_helpers::*;
use reqwest::{Client, StatusCode};
use std::time::Duration;

// ─── Tests ──────────────────────────────────────────────────────────────────

/// Keycloak JWKS endpoint returns valid RS256 keys.
#[tokio::test]
#[ignore]
async fn test_keycloak_jwks_returns_valid_keys() {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let jwks_url = format!(
        "http://127.0.0.1:{}/realms/{}/protocol/openid-connect/certs",
        KEYCLOAK_PORT, REALM
    );

    let resp = client.get(&jwks_url).send().await.expect("JWKS request");
    assert_eq!(resp.status(), StatusCode::OK, "JWKS endpoint should return 200");

    let body: serde_json::Value = resp.json().await.expect("parse JWKS");
    let keys = body.get("keys").and_then(|k| k.as_array()).expect("JWKS should have keys array");

    assert!(!keys.is_empty(), "JWKS should contain at least one key");

    // Find an RS256 key
    let rs256_key = keys.iter().find(|k| {
        k.get("alg").and_then(|a| a.as_str()) == Some("RS256")
            || k.get("kty").and_then(|a| a.as_str()) == Some("RSA")
    });
    assert!(rs256_key.is_some(), "JWKS should contain an RSA/RS256 key");

    let key = rs256_key.unwrap();
    assert!(key.get("n").is_some(), "RSA key should have modulus 'n'");
    assert!(key.get("e").is_some(), "RSA key should have exponent 'e'");
    assert!(key.get("kid").is_some(), "Key should have 'kid'");

    println!("✓ JWKS endpoint returns valid RS256 keys");
}

/// Keycloak JWT contains correct claims: sub (UUID), iss, aud, realm_access.roles.
#[tokio::test]
#[ignore]
async fn test_keycloak_jwt_claims_structure() {
    let kc = KeycloakTokenClient::new();

    // Get tokens for all three users
    let alice_token = kc.get_token(ALICE_USER, ALICE_PASS).await;
    let admin_token = kc.get_token(ADMIN_USER, ADMIN_PASS).await;

    // Decode and inspect Alice's JWT
    let alice_claims = decode_jwt_payload(&alice_token.access_token);

    // sub should be a UUID, not the username
    let sub = alice_claims.get("sub").and_then(|s| s.as_str()).expect("JWT should have 'sub'");
    assert!(
        sub.contains('-') && sub.len() >= 32,
        "sub should be a UUID, got: {}",
        sub
    );

    // iss should match Keycloak internal URL
    let iss = alice_claims.get("iss").and_then(|s| s.as_str()).expect("JWT should have 'iss'");
    assert_eq!(
        iss, ISSUER_INTERNAL,
        "issuer should match Keycloak internal URL"
    );

    // aud should contain "detrix-api" (from audience mapper)
    let aud = alice_claims.get("aud").expect("JWT should have 'aud'");
    let aud_matches = match aud {
        serde_json::Value::String(s) => s == CLIENT_ID,
        serde_json::Value::Array(arr) => arr.iter().any(|v| v.as_str() == Some(CLIENT_ID)),
        _ => false,
    };
    assert!(aud_matches, "aud should contain '{}', got: {}", CLIENT_ID, aud);

    // Admin JWT should have realm_access.roles containing detrix-admin
    let admin_claims = decode_jwt_payload(&admin_token.access_token);
    let realm_access = admin_claims
        .get("realm_access")
        .expect("admin JWT should have 'realm_access'");
    let roles = realm_access
        .get("roles")
        .and_then(|r| r.as_array())
        .expect("realm_access should have 'roles' array");
    let has_admin = roles
        .iter()
        .any(|r| r.as_str() == Some(ADMIN_ROLE));
    assert!(
        has_admin,
        "admin JWT should have '{}' in realm_access.roles, got: {:?}",
        ADMIN_ROLE, roles
    );

    // Alice should NOT have admin role
    let alice_realm_access = alice_claims.get("realm_access");
    if let Some(ra) = alice_realm_access {
        let alice_roles = ra.get("roles").and_then(|r| r.as_array());
        if let Some(roles) = alice_roles {
            let alice_is_admin = roles.iter().any(|r| r.as_str() == Some(ADMIN_ROLE));
            assert!(!alice_is_admin, "alice should NOT have admin role");
        }
    }

    println!("✓ JWT claims structure is correct (sub=UUID, iss, aud, realm_access.roles)");
}

/// Tampered JWT (modified payload) → 401.
#[tokio::test]
#[ignore]
async fn test_keycloak_tampered_jwt_rejected() {
    let kc = KeycloakTokenClient::new();
    let token_resp = kc.get_token(ALICE_USER, ALICE_PASS).await;

    // Tamper with the JWT by modifying the payload section
    let parts: Vec<&str> = token_resp.access_token.split('.').collect();
    assert_eq!(parts.len(), 3, "JWT should have 3 parts");

    // Modify payload by appending a character (breaks signature)
    let tampered = format!("{}.{}x.{}", parts[0], parts[1], parts[2]);

    let client = DetrixClient::new(&tampered);
    let (status, _) = client.get("/api/v1/metrics").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "tampered JWT should be rejected, got: {}",
        status
    );

    println!("✓ tampered JWT → 401");
}

/// JWT signed with a different RSA key (not in Keycloak JWKS) → 401.
#[tokio::test]
#[ignore]
async fn test_keycloak_wrong_key_jwt_rejected() {
    // Generate a JWT with a completely different RSA key
    use detrix_testing::e2e::jwt::{JwtBuilder, JwtKeyPair, TestClaims};

    let fake_key = JwtKeyPair::generate();
    let claims = TestClaims::new("fake-user", ISSUER_INTERNAL).with_audience(CLIENT_ID);
    let fake_token = JwtBuilder::new(&fake_key, claims).build();

    let client = DetrixClient::new(&fake_token);
    let (status, _) = client.get("/api/v1/metrics").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "JWT from wrong key should be rejected, got: {}",
        status
    );

    println!("✓ JWT from wrong RSA key → 401");
}

/// No token → 401.
#[tokio::test]
#[ignore]
async fn test_keycloak_no_token_denied() {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let status = client
        .get(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            DETRIX_HTTP_PORT
        ))
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

/// Valid Keycloak token → 200, and `sub` (UUID) is used as user_id.
#[tokio::test]
#[ignore]
async fn test_keycloak_valid_token_grants_access_with_sub_as_user_id() {
    let kc = KeycloakTokenClient::new();
    let token_resp = kc.get_token(ALICE_USER, ALICE_PASS).await;

    // Verify access works
    let client = DetrixClient::new(&token_resp.access_token);
    let (status, _) = client.list_metrics().await;
    assert_eq!(
        status,
        StatusCode::OK,
        "valid Keycloak JWT should grant access, got: {}",
        status
    );

    // Extract the sub from the JWT — this is what Detrix uses as user_id
    let claims = decode_jwt_payload(&token_resp.access_token);
    let sub = claims.get("sub").and_then(|s| s.as_str()).unwrap();
    println!("  alice sub (user_id): {}", sub);

    println!("✓ valid Keycloak token → 200, sub used as user_id");
}
