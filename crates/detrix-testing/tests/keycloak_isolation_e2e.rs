//! Keycloak Multi-User Data Isolation E2E Tests — Scenario 1
//!
//! Full cloud-like E2E: Keycloak (OIDC provider) + Detrix daemon + Go test app.
//! Validates that metrics created by different JWT users are properly isolated:
//! - Alice sees only her metrics
//! - Bob sees only his metrics
//! - Admin sees all metrics
//! - Cross-user mutation is denied (404, not 403)
//! - Admin can delete any user's metric
//!
//! Run via: `task test-cloud-keycloak`

mod keycloak_helpers;

use keycloak_helpers::*;
use reqwest::StatusCode;
use std::time::Duration;

// ─── Tests ──────────────────────────────────────────────────────────────────

/// Full multi-user data isolation scenario with real Go app connection.
///
/// 1. Wake Go app (real DAP connection via Delve)
/// 2. Alice creates 2 metrics on that connection
/// 3. Bob creates 1 metric
/// 4. Each user lists only their own metrics
/// 5. Admin lists all 3
/// 6. Alice cannot delete Bob's metric (404)
/// 7. Admin can delete Alice's metric
#[tokio::test]
#[ignore]
async fn test_keycloak_multi_user_data_isolation() {
    let kc = KeycloakTokenClient::new();

    // Get admin token for initial setup (wake + connection polling)
    let admin_token = kc.get_token(ADMIN_USER, ADMIN_PASS).await;
    let admin = DetrixClient::new(&admin_token.access_token);

    // Verify daemon is healthy
    assert_eq!(admin.health().await, StatusCode::OK, "daemon not healthy");
    println!("  Daemon healthy");

    // Wake Go app and get connection ID (admin has visibility into all connections)
    let (status, _) = admin.wake(GO_APP_URL).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "wake go app failed: {}",
        status
    );
    println!("  Woke Go app");

    let go_conn = admin
        .poll_for_connection("go", Duration::from_secs(30))
        .await
        .expect("Go connection not found within 30s");
    println!("  Go connection: {}", go_conn);

    // Fresh tokens — initial admin token may have expired during wake/poll (30s TTL)
    let alice_token = kc.get_token(ALICE_USER, ALICE_PASS).await;
    let bob_token = kc.get_token(BOB_USER, BOB_PASS).await;
    let admin_token = kc.get_token(ADMIN_USER, ADMIN_PASS).await;
    let alice = DetrixClient::new(&alice_token.access_token);
    let bob = DetrixClient::new(&bob_token.access_token);
    let admin = DetrixClient::new(&admin_token.access_token);

    let alice_sub = decode_jwt_payload(&alice_token.access_token)
        .get("sub")
        .and_then(|s| s.as_str())
        .unwrap()
        .to_string();
    let bob_sub = decode_jwt_payload(&bob_token.access_token)
        .get("sub")
        .and_then(|s| s.as_str())
        .unwrap()
        .to_string();
    println!("  alice sub: {}, bob sub: {}", alice_sub, bob_sub);

    // Clean up any pre-existing metrics from prior runs
    let (_, existing) = admin.list_metrics().await;
    for m in &existing {
        let _ = admin.delete_metric_by_name(&m.name).await;
    }

    // Step 1: Alice creates 2 metrics on the real Go connection
    let (status, _) = alice
        .create_metric(&CreateMetricBody {
            name: "alice-metric-1".to_string(),
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
        "alice create metric 1 failed: {}",
        status
    );

    let (status, _) = alice
        .create_metric(&CreateMetricBody {
            name: "alice-metric-2".to_string(),
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
        "alice create metric 2 failed: {}",
        status
    );
    println!("  Alice created 2 metrics");

    // Step 2: Bob creates 1 metric
    let (status, _) = bob
        .create_metric(&CreateMetricBody {
            name: "bob-metric-1".to_string(),
            connection_id: go_conn.clone(),
            location: LocationBody {
                file: GO_FILE.to_string(),
                line: GO_LOGPOINT_LINE + 2,
            },
            expressions: vec!["price".to_string()],
            enabled: false,
        })
        .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "bob create metric failed: {}",
        status
    );
    println!("  Bob created 1 metric");

    // Step 3: Alice lists → sees only her 2 metrics
    let (status, alice_metrics) = alice.list_metrics().await;
    assert_eq!(status, StatusCode::OK);
    let alice_names: Vec<&str> = alice_metrics.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        alice_metrics.len(),
        2,
        "alice should see exactly 2 metrics, got: {:?}",
        alice_names
    );
    assert!(
        alice_names.contains(&"alice-metric-1") && alice_names.contains(&"alice-metric-2"),
        "alice should see her metrics, got: {:?}",
        alice_names
    );
    println!("  Alice lists: sees only her 2 metrics ✓");

    // Step 4: Bob lists → sees only his 1 metric
    let (status, bob_metrics) = bob.list_metrics().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        bob_metrics.len(),
        1,
        "bob should see exactly 1 metric, got: {}",
        bob_metrics.len()
    );
    assert_eq!(bob_metrics[0].name, "bob-metric-1");
    println!("  Bob lists: sees only his 1 metric ✓");

    // Step 5: Admin lists → sees all 3
    let (status, admin_metrics) = admin.list_metrics().await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        admin_metrics.len() >= 3,
        "admin should see at least 3 metrics, got: {}",
        admin_metrics.len()
    );
    let admin_names: Vec<&str> = admin_metrics.iter().map(|m| m.name.as_str()).collect();
    assert!(
        admin_names.contains(&"alice-metric-1")
            && admin_names.contains(&"alice-metric-2")
            && admin_names.contains(&"bob-metric-1"),
        "admin should see all metrics, got: {:?}",
        admin_names
    );
    println!("  Admin lists: sees all 3 metrics ✓");

    // Step 6: Alice tries to delete Bob's metric → 404 (not 403, info leakage prevention)
    let status = alice.delete_metric_by_name("bob-metric-1").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "cross-user delete should return 404, got: {}",
        status
    );
    println!("  Alice delete Bob's metric → 404 (not 403) ✓");

    // Step 6b: Alice tries to delete Bob's metric by numeric ID → 403
    // First, admin queries to find Bob's metric ID
    let (_, admin_all) = admin.list_metrics().await;
    let bob_metric_id = admin_all
        .iter()
        .find(|m| m.name == "bob-metric-1")
        .and_then(|m| m.metric_id.as_ref())
        .and_then(|v| v.as_u64())
        .expect("admin should see bob's metric with numeric ID");

    let status = alice.delete_metric(bob_metric_id).await;
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
        "cross-user delete by numeric ID should → 403 or 404, got: {}",
        status
    );
    println!(
        "  Alice delete Bob's metric by ID ({}) → {} ✓",
        bob_metric_id, status
    );

    // Step 7: Admin deletes Alice's metric → 200
    let status = admin.delete_metric_by_name("alice-metric-1").await;
    assert!(
        status == StatusCode::OK || status == StatusCode::NO_CONTENT,
        "admin delete should succeed, got: {}",
        status
    );

    // Verify Alice now sees only 1 metric
    let (_, alice_metrics_after) = alice.list_metrics().await;
    assert_eq!(
        alice_metrics_after.len(),
        1,
        "alice should see 1 metric after admin delete, got: {}",
        alice_metrics_after.len()
    );
    println!("  Admin deletes Alice's metric → 200, Alice now sees 1 ✓");

    // Cleanup
    let _ = alice.delete_metric_by_name("alice-metric-2").await;
    let _ = bob.delete_metric_by_name("bob-metric-1").await;

    println!("✓ Multi-user data isolation test passed");
}
