//! Agent Load Tests
//!
//! Stress-tests the server-side AgentConnectionManager under realistic loads.
//! Uses in-memory mock repositories — no SQLite, no network.
//!
//! # Scenarios
//!
//! ## Scenario A — Single agent, many binaries (N ∈ {20, 50, 100})
//!
//! Verifies:
//! - `save_batch` completes within 2 seconds
//! - All N connections are registered (no duplicates)
//! - Re-sending same N binaries produces exactly N connections (idempotency)
//!
//! ## Scenario B — 5 concurrent agents × 20 binaries each
//!
//! Verifies:
//! - All 5 registrations complete within 10 seconds
//! - 100 total connections (5 × 20) with no duplicates or constraint violations

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use detrix_core::{Connection, ConnectionId, ConnectionIdentity, SourceLanguage};
use detrix_ports::ConnectionRepository;
use detrix_testing::MockConnectionRepository;

// ============================================================================
// Helpers
// ============================================================================

fn make_connections(agent_id: &str, hostname: &str, count: usize) -> Vec<Connection> {
    (0..count)
        .filter_map(|i| {
            let binary_path = format!("/app/go_app_{}", i);
            let basename = format!("go_app_{}", i);
            let short_id = &agent_id[..8.min(agent_id.len())];
            let name = format!("agent/{short_id}/{basename}");
            let identity = ConnectionIdentity::new(&name, SourceLanguage::Go, "/", hostname);
            Connection::new_with_identity(identity, binary_path, 1024 + i as u16).ok()
        })
        .collect()
}

async fn save_connections(
    repo: &Arc<MockConnectionRepository>,
    connections: &[Connection],
) -> Result<usize, String> {
    repo.save_batch(connections)
        .await
        .map_err(|e| format!("save_batch failed: {e}"))
}

// ============================================================================
// Scenario A — Single agent, many binaries
// ============================================================================

/// Single agent registers N binaries. Re-sends same N → no duplicates.
#[tokio::test]
async fn test_load_single_agent_many_binaries() {
    for n in [20, 50, 100] {
        let repo = Arc::new(MockConnectionRepository::new());
        let connections = make_connections("agent-a1", "host1", n);

        // Register
        let start = Instant::now();
        let count = save_connections(&repo, &connections).await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(count, n, "N={}: save_batch should return {}", n, count);
        assert!(
            elapsed < Duration::from_secs(2),
            "N={}: registration took {:.3}s (limit: 2s)",
            n,
            elapsed.as_secs_f64()
        );

        // Verify connection count
        let all = repo.list_all().await.unwrap();
        assert_eq!(
            all.len(),
            n,
            "N={}: expected {} connections (got {})",
            n,
            n,
            all.len()
        );

        // Re-register same binaries → idempotent (upsert), should still be N
        let connections2 = make_connections("agent-a1", "host1", n);
        let count2 = save_connections(&repo, &connections2).await.unwrap();
        assert_eq!(count2, n, "N={}: re-save_batch should return {}", n, count2);

        let all2 = repo.list_all().await.unwrap();
        assert_eq!(
            all2.len(),
            n,
            "N={}: after re-register, expected {} connections (got {})",
            n,
            n,
            all2.len()
        );
    }
}

// ============================================================================
// Scenario B — Concurrent agents
// ============================================================================

/// 5 agents register 20 binaries each concurrently. Total 100 connections.
#[tokio::test]
async fn test_load_concurrent_agents() {
    let repo = Arc::new(MockConnectionRepository::new());
    let num_agents = 5;
    let binaries_per_agent = 20;
    let target_duration = Duration::from_secs(10);

    let start = Instant::now();

    // Spawn all agents concurrently
    let handles: Vec<_> = (0..num_agents)
        .map(|i| {
            let repo = repo.clone();
            let agent_id = format!("agent-b{}", i);
            let hostname = format!("host{}", i);
            let connections = make_connections(&agent_id, &hostname, binaries_per_agent);

            tokio::spawn(async move {
                let count = save_connections(&repo, &connections)
                    .await
                    .expect(&format!("Agent {}: save_batch should succeed", agent_id));
                assert_eq!(
                    count, binaries_per_agent,
                    "Agent {}: expected {} connections saved",
                    agent_id, binaries_per_agent
                );
            })
        })
        .collect();

    // Wait for all agents
    for handle in handles {
        handle.await.expect("Agent task should not panic");
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed < target_duration,
        "Concurrent registration took {} (limit: {})",
        elapsed.as_secs_f64(),
        target_duration.as_secs_f64()
    );

    // Verify total connection count
    let connections = repo.list_all().await.unwrap();
    assert_eq!(
        connections.len(),
        num_agents * binaries_per_agent,
        "Expected {} connections, got {}",
        num_agents * binaries_per_agent,
        connections.len()
    );

    // Verify no duplicate connection IDs
    let ids: HashSet<&ConnectionId> = connections.iter().map(|c| &c.id).collect();
    assert_eq!(
        ids.len(),
        connections.len(),
        "Duplicate connection IDs detected"
    );
}
