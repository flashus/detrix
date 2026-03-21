//! Keycloak MCP Bridge Per-User Scoping E2E Tests — Scenario 3
//!
//! Full cloud-like E2E: Keycloak + Detrix daemon + Go test app + MCP bridge.
//! Validates that MCP bridge processes inherit JWT-based user scoping:
//! - Metrics created via REST with user-specific JWTs
//! - MCP bridge spawned with `DETRIX_TOKEN=<jwt>` sees only that user's metrics
//! - Admin MCP bridge sees all metrics
//!
//! Run via: `task test-cloud-keycloak`

mod keycloak_helpers;

use detrix_testing::e2e::mcp_bridge::McpBridgeProcess;
use keycloak_helpers::*;
use reqwest::StatusCode;
use std::time::Duration;

// Type alias for brevity in this test file.
type McpBridge = McpBridgeProcess;

// ─── Tests ──────────────────────────────────────────────────────────────────

/// MCP bridge per-user scoping: each bridge sees only its user's metrics.
///
/// Uses real Go app connection for metric creation, then validates
/// MCP bridge isolation with per-user JWT tokens.
#[tokio::test]
#[ignore]
async fn test_keycloak_mcp_bridge_per_user_scoping() {
    let kc = KeycloakTokenClient::new();

    // Get admin token for wake + connection setup
    let admin_token = kc.get_token(ADMIN_USER, ADMIN_PASS).await;
    let daemon_url = format!("http://127.0.0.1:{}", DETRIX_HTTP_PORT);
    let admin_rest = DetrixClient::new(&admin_token.access_token);

    // Wake Go app and get connection via admin
    let (status, _) = admin_rest.wake(GO_APP_URL).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "wake go app failed: {}",
        status
    );

    let go_conn = admin_rest
        .poll_for_connection("go", Duration::from_secs(30))
        .await
        .expect("Go connection not found within 30s");
    println!("  Go connection: {}", go_conn);

    // Fresh tokens after wake/poll (30s TTL may have expired)
    let alice_token = kc.get_token(ALICE_USER, ALICE_PASS).await;
    let bob_token = kc.get_token(BOB_USER, BOB_PASS).await;
    let admin_token = kc.get_token(ADMIN_USER, ADMIN_PASS).await;
    let admin_rest = DetrixClient::new(&admin_token.access_token);
    let alice_rest = DetrixClient::new(&alice_token.access_token);
    let bob_rest = DetrixClient::new(&bob_token.access_token);

    // Clean up pre-existing metrics
    let (_, existing) = admin_rest.list_metrics().await;
    for m in &existing {
        let _ = admin_rest.delete_metric_by_name(&m.name).await;
    }

    // Step 1: Create metrics via REST with user-specific tokens
    let (status, _) = alice_rest
        .create_metric(&CreateMetricBody {
            name: "mcp-alice-metric".to_string(),
            connection_id: go_conn.clone(),
            location: LocationBody {
                file: GO_FILE.to_string(),
                line: GO_LOGPOINT_LINE,
            },
            expressions: vec!["symbol".to_string()],
            enabled: false,
        })
        .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "alice create metric failed: {}",
        status
    );

    let (status, _) = bob_rest
        .create_metric(&CreateMetricBody {
            name: "mcp-bob-metric".to_string(),
            connection_id: go_conn.clone(),
            location: LocationBody {
                file: GO_FILE.to_string(),
                line: GO_LOGPOINT_LINE + 1,
            },
            expressions: vec!["quantity".to_string()],
            enabled: false,
        })
        .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "bob create metric failed: {}",
        status
    );
    println!("  Setup: created metrics for alice and bob via REST");

    // Step 2: Spawn MCP bridges with per-user JWT tokens
    let mut alice_bridge = McpBridge::spawn(&daemon_url, &alice_token.access_token).await;
    println!("  Alice MCP bridge spawned");

    let mut bob_bridge = McpBridge::spawn(&daemon_url, &bob_token.access_token).await;
    println!("  Bob MCP bridge spawned");

    let mut admin_bridge = McpBridge::spawn(&daemon_url, &admin_token.access_token).await;
    println!("  Admin MCP bridge spawned");

    // Step 3: Alice's bridge sees only her metrics
    let alice_names = alice_bridge
        .list_metric_names()
        .await
        .expect("alice list_metric_names failed");
    assert!(
        alice_names.contains(&"mcp-alice-metric".to_string()),
        "alice bridge should see mcp-alice-metric, got: {:?}",
        alice_names
    );
    assert!(
        !alice_names.contains(&"mcp-bob-metric".to_string()),
        "alice bridge should NOT see bob's metric, got: {:?}",
        alice_names
    );
    println!("  Alice bridge: sees only her metric ✓");

    // Step 4: Bob's bridge sees only his metrics
    let bob_names = bob_bridge
        .list_metric_names()
        .await
        .expect("bob list_metric_names failed");
    assert!(
        bob_names.contains(&"mcp-bob-metric".to_string()),
        "bob bridge should see mcp-bob-metric, got: {:?}",
        bob_names
    );
    assert!(
        !bob_names.contains(&"mcp-alice-metric".to_string()),
        "bob bridge should NOT see alice's metric, got: {:?}",
        bob_names
    );
    println!("  Bob bridge: sees only his metric ✓");

    // Step 5: Admin bridge sees all metrics
    let admin_names = admin_bridge
        .list_metric_names()
        .await
        .expect("admin list_metric_names failed");
    assert!(
        admin_names.contains(&"mcp-alice-metric".to_string())
            && admin_names.contains(&"mcp-bob-metric".to_string()),
        "admin bridge should see all metrics, got: {:?}",
        admin_names
    );
    println!("  Admin bridge: sees all metrics ✓");

    // Cleanup
    alice_bridge.kill().await;
    bob_bridge.kill().await;
    admin_bridge.kill().await;
    let _ = alice_rest.delete_metric_by_name("mcp-alice-metric").await;
    let _ = bob_rest.delete_metric_by_name("mcp-bob-metric").await;

    println!("✓ MCP bridge per-user scoping test passed");
}
