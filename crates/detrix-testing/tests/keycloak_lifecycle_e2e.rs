//! Keycloak Auth Lifecycle E2E Tests — Scenario 2
//!
//! Validates the full authentication lifecycle against a real OIDC provider:
//! - ROPC token acquisition → 200
//! - Token expiry (30s TTL) → 401
//! - Token refresh → new access token → 200
//! - Full re-authentication via ROPC → 200
//!
//! Run via: `task test-cloud-keycloak`
//! Or manually:
//!   docker compose -f fixtures/docker/docker-compose.keycloak.yml -p detrix-keycloak-test \
//!       up -d --build --wait
//!   cargo test -p detrix-testing --test keycloak_lifecycle_e2e -- --ignored --nocapture
//!   docker compose -f fixtures/docker/docker-compose.keycloak.yml -p detrix-keycloak-test \
//!       down -v

mod keycloak_helpers;

use keycloak_helpers::*;
use reqwest::StatusCode;

// ─── Tests ──────────────────────────────────────────────────────────────────

/// Full auth lifecycle: acquire → use → expire → refresh → re-auth.
///
/// Note: This test takes ~40s due to the token expiry wait.
/// Keycloak is configured with 30s access token lifespan.
#[tokio::test]
#[ignore]
async fn test_keycloak_token_lifecycle() {
    let kc = KeycloakTokenClient::new();

    // Step 1: Acquire token via ROPC
    let token_resp = kc.get_token(ALICE_USER, ALICE_PASS).await;
    assert!(
        token_resp.expires_in <= 30,
        "token should have ~30s TTL, got: {}s",
        token_resp.expires_in
    );
    assert!(
        token_resp.refresh_token.is_some(),
        "should receive a refresh token"
    );
    let refresh_token = token_resp.refresh_token.as_ref().unwrap().clone();
    println!(
        "  Step 1: acquired token (expires_in: {}s)",
        token_resp.expires_in
    );

    // Step 2: Use token → 200
    let client = DetrixClient::new(&token_resp.access_token);
    let (status, _) = client.list_metrics().await;
    assert_eq!(
        status,
        StatusCode::OK,
        "fresh token should work, got: {}",
        status
    );
    println!("  Step 2: fresh token → 200 ✓");

    // Step 3: Wait for expiry
    // Token TTL is 30s, but jsonwebtoken has 60s leeway by default.
    // Wait 30 (TTL) + 60 (leeway) + 5 (buffer) = 95s.
    println!("  Step 3: waiting 95s for token expiry (30s TTL + 60s leeway + 5s buffer)...");
    tokio::time::sleep(std::time::Duration::from_secs(95)).await;

    // Same token should now be rejected
    let (status, _) = client.list_metrics().await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "expired token should → 401, got: {}",
        status
    );
    println!("  Step 3: expired token → 401 ✓");

    // Step 4: Refresh token → new access token → 200
    let refreshed = kc.refresh_token(&refresh_token).await;
    let mut client = DetrixClient::new(&refreshed.access_token);
    let (status, _) = client.list_metrics().await;
    assert_eq!(
        status,
        StatusCode::OK,
        "refreshed token should work, got: {}",
        status
    );
    println!("  Step 4: refreshed token → 200 ✓");

    // Step 5: Full re-authentication via ROPC → new token → 200
    let reauth = kc.get_token(ALICE_USER, ALICE_PASS).await;
    client.set_token(&reauth.access_token);
    let (status, _) = client.list_metrics().await;
    assert_eq!(
        status,
        StatusCode::OK,
        "re-authenticated token should work, got: {}",
        status
    );
    println!("  Step 5: re-authenticated token → 200 ✓");

    println!("✓ Token lifecycle test passed");
}
