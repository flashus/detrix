//! Tests for MetricRepository implementation
//!
//! Extracted from metric_repo.rs to reduce file size.

use super::SqliteStorage;
use detrix_application::MetricRepository;
use detrix_core::entities::{Location, MetricMode};
use detrix_core::{ConnectionId, Metric, SourceLanguage};

pub async fn create_test_storage() -> SqliteStorage {
    SqliteStorage::in_memory().await.unwrap()
}

/// Create a test metric with unique location based on name hash
/// This prevents location uniqueness constraint violations in tests
pub async fn create_test_metric(name: &str) -> Metric {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    let line = (hasher.finish() % 9000 + 1000) as u32; // Line between 1000-9999

    Metric::new(
        name.to_string(),
        ConnectionId::from("default"),
        Location {
            file: "test.py".to_string(),
            line,
        },
        "user.id".to_string(),
        SourceLanguage::Python,
    )
    .unwrap()
}

#[tokio::test]
async fn test_metric_save_and_find() {
    let storage = create_test_storage().await;
    let metric = create_test_metric("test_metric").await;

    // Save
    let id = MetricRepository::save(&storage, &metric).await.unwrap();
    assert!(id.0 > 0);

    // Find by ID
    let found = MetricRepository::find_by_id(&storage, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.name, "test_metric");
    assert_eq!(found.expression, "user.id");
    assert_eq!(found.language, SourceLanguage::Python);
    assert!(found.enabled);
    assert_eq!(found.mode, MetricMode::Stream);
}

#[tokio::test]
async fn test_metric_find_by_name() {
    let storage = create_test_storage().await;
    let metric = create_test_metric("unique_name").await;

    MetricRepository::save(&storage, &metric).await.unwrap();

    let found = MetricRepository::find_by_name(&storage, "unique_name")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.name, "unique_name");
}

#[tokio::test]
async fn test_metric_find_all() {
    let storage = create_test_storage().await;

    MetricRepository::save(&storage, &create_test_metric("metric1").await)
        .await
        .unwrap();
    MetricRepository::save(&storage, &create_test_metric("metric2").await)
        .await
        .unwrap();
    MetricRepository::save(&storage, &create_test_metric("metric3").await)
        .await
        .unwrap();

    let all = MetricRepository::find_all(&storage).await.unwrap();
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn test_metric_update() {
    let storage = create_test_storage().await;
    let metric = create_test_metric("updatable").await;

    let id = MetricRepository::save(&storage, &metric).await.unwrap();

    let mut found = MetricRepository::find_by_id(&storage, id)
        .await
        .unwrap()
        .unwrap();
    found.expression = "order.total".to_string();
    found.enabled = false;

    MetricRepository::update(&storage, &found).await.unwrap();

    let updated = MetricRepository::find_by_id(&storage, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.expression, "order.total");
    assert!(!updated.enabled);
}

#[tokio::test]
async fn test_metric_delete() {
    let storage = create_test_storage().await;
    let metric = create_test_metric("deletable").await;

    let id = MetricRepository::save(&storage, &metric).await.unwrap();
    MetricRepository::delete(&storage, id).await.unwrap();

    let found = MetricRepository::find_by_id(&storage, id).await.unwrap();
    assert!(found.is_none());
}

#[tokio::test]
async fn test_metric_exists_by_name() {
    let storage = create_test_storage().await;
    let metric = create_test_metric("exists_test").await;

    assert!(!MetricRepository::exists_by_name(&storage, "exists_test")
        .await
        .unwrap());

    MetricRepository::save(&storage, &metric).await.unwrap();

    assert!(MetricRepository::exists_by_name(&storage, "exists_test")
        .await
        .unwrap());
}

#[tokio::test]
async fn test_metric_duplicate_name_fails_by_default() {
    let storage = create_test_storage().await;
    let metric1 = create_test_metric("duplicate").await;

    // Save first metric
    MetricRepository::save(&storage, &metric1).await.unwrap();

    // Create second metric with same name
    let metric2 = create_test_metric("duplicate").await;

    // Default save (upsert=false) should fail due to UNIQUE constraint
    let result = MetricRepository::save(&storage, &metric2).await;
    assert!(result.is_err(), "Duplicate name should fail by default");
}

#[tokio::test]
async fn test_metric_duplicate_name_upserts_when_enabled() {
    let storage = create_test_storage().await;
    let metric1 = create_test_metric("duplicate").await;

    // Save first metric
    let id1 = MetricRepository::save(&storage, &metric1).await.unwrap();
    assert!(id1.0 > 0);

    // Create second metric with same name but different expression
    let mut metric2 = create_test_metric("duplicate").await;
    metric2.expression = "updated.expression".to_string();

    // Explicit upsert should succeed and return same ID
    let id2 = MetricRepository::save_with_options(&storage, &metric2, true)
        .await
        .unwrap();
    assert_eq!(id1, id2, "Upsert should return same ID for duplicate name");

    // Verify the expression was updated
    let found = MetricRepository::find_by_id(&storage, id1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        found.expression, "updated.expression",
        "Expression should be updated by upsert"
    );

    // Verify only one metric exists
    let all = MetricRepository::find_all(&storage).await.unwrap();
    assert_eq!(all.len(), 1, "Upsert should not create duplicate entries");
}

#[tokio::test]
async fn test_metric_find_by_group() {
    let storage = create_test_storage().await;

    let mut metric1 = create_test_metric("metric1").await;
    metric1.group = Some("auth".to_string());

    let mut metric2 = create_test_metric("metric2").await;
    metric2.group = Some("auth".to_string());

    let mut metric3 = create_test_metric("metric3").await;
    metric3.group = Some("orders".to_string());

    MetricRepository::save(&storage, &metric1).await.unwrap();
    MetricRepository::save(&storage, &metric2).await.unwrap();
    MetricRepository::save(&storage, &metric3).await.unwrap();

    let auth_metrics = MetricRepository::find_by_group(&storage, "auth")
        .await
        .unwrap();
    assert_eq!(auth_metrics.len(), 2);

    let order_metrics = MetricRepository::find_by_group(&storage, "orders")
        .await
        .unwrap();
    assert_eq!(order_metrics.len(), 1);
}

#[tokio::test]
async fn test_metric_anchor_persistence() {
    use detrix_core::entities::{AnchorStatus, MetricAnchor};

    let storage = create_test_storage().await;
    let mut metric = create_test_metric("anchored_metric").await;

    // Set anchor data
    metric.anchor = Some(MetricAnchor {
        enclosing_symbol: Some("process_payment".to_string()),
        symbol_kind: Some(12), // Function
        symbol_range: Some((10, 50)),
        offset_in_symbol: Some(5),
        context_fingerprint: Some("abc123def456".to_string()),
        context_before: Some("    amount = calculate_total()".to_string()),
        source_line: Some("    user.balance -= amount".to_string()),
        context_after: Some("    log.info('Payment processed')".to_string()),
        last_verified_at: Some(1700000000000000),
        original_location: Some("@payment.py#15".to_string()),
    });
    metric.anchor_status = AnchorStatus::Verified;

    // Save
    let id = MetricRepository::save(&storage, &metric).await.unwrap();
    assert!(id.0 > 0);

    // Retrieve and verify anchor data
    let found = MetricRepository::find_by_id(&storage, id)
        .await
        .unwrap()
        .unwrap();

    assert!(found.anchor.is_some());
    let anchor = found.anchor.unwrap();
    assert_eq!(anchor.enclosing_symbol, Some("process_payment".to_string()));
    assert_eq!(anchor.symbol_kind, Some(12));
    assert_eq!(anchor.symbol_range, Some((10, 50)));
    assert_eq!(anchor.offset_in_symbol, Some(5));
    assert_eq!(anchor.context_fingerprint, Some("abc123def456".to_string()));
    assert_eq!(
        anchor.context_before,
        Some("    amount = calculate_total()".to_string())
    );
    assert_eq!(
        anchor.source_line,
        Some("    user.balance -= amount".to_string())
    );
    assert_eq!(
        anchor.context_after,
        Some("    log.info('Payment processed')".to_string())
    );
    assert_eq!(anchor.last_verified_at, Some(1700000000000000));
    assert_eq!(anchor.original_location, Some("@payment.py#15".to_string()));
    assert_eq!(found.anchor_status, AnchorStatus::Verified);
}

#[tokio::test]
async fn test_metric_anchor_update() {
    use detrix_core::entities::{AnchorStatus, MetricAnchor};

    let storage = create_test_storage().await;
    let metric = create_test_metric("updatable_anchor").await;

    // Save without anchor
    let id = MetricRepository::save(&storage, &metric).await.unwrap();

    // Retrieve and add anchor
    let mut found = MetricRepository::find_by_id(&storage, id)
        .await
        .unwrap()
        .unwrap();

    assert!(found.anchor.is_none());
    assert_eq!(found.anchor_status, AnchorStatus::Unanchored);

    // Add anchor data
    found.anchor = Some(MetricAnchor {
        enclosing_symbol: Some("login".to_string()),
        symbol_kind: Some(6), // Method
        symbol_range: Some((100, 150)),
        offset_in_symbol: Some(10),
        context_fingerprint: Some("xyz789".to_string()),
        context_before: None,
        source_line: Some("session.start()".to_string()),
        context_after: None,
        last_verified_at: None,
        original_location: Some("@auth.py#110".to_string()),
    });
    found.anchor_status = AnchorStatus::Relocated;

    // Update
    MetricRepository::update(&storage, &found).await.unwrap();

    // Verify update
    let updated = MetricRepository::find_by_id(&storage, id)
        .await
        .unwrap()
        .unwrap();

    assert!(updated.anchor.is_some());
    let anchor = updated.anchor.unwrap();
    assert_eq!(anchor.enclosing_symbol, Some("login".to_string()));
    assert_eq!(anchor.symbol_kind, Some(6));
    assert_eq!(anchor.symbol_range, Some((100, 150)));
    assert_eq!(updated.anchor_status, AnchorStatus::Relocated);
}

#[tokio::test]
async fn test_metric_find_paginated() {
    let storage = create_test_storage().await;

    // Create 5 metrics
    for i in 1..=5 {
        let metric = create_test_metric(&format!("metric_{}", i)).await;
        MetricRepository::save(&storage, &metric).await.unwrap();
    }

    // Test first page
    let (metrics, total) = MetricRepository::find_paginated(&storage, 2, 0)
        .await
        .unwrap();
    assert_eq!(metrics.len(), 2);
    assert_eq!(total, 5);

    // Test second page
    let (metrics, total) = MetricRepository::find_paginated(&storage, 2, 2)
        .await
        .unwrap();
    assert_eq!(metrics.len(), 2);
    assert_eq!(total, 5);

    // Test last page (partial)
    let (metrics, total) = MetricRepository::find_paginated(&storage, 2, 4)
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(total, 5);

    // Test beyond end
    let (metrics, total) = MetricRepository::find_paginated(&storage, 2, 10)
        .await
        .unwrap();
    assert_eq!(metrics.len(), 0);
    assert_eq!(total, 5);
}

#[tokio::test]
async fn test_metric_count_all() {
    let storage = create_test_storage().await;

    // Empty database
    let count = MetricRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 0);

    // Add metrics
    for i in 1..=3 {
        let metric = create_test_metric(&format!("count_test_{}", i)).await;
        MetricRepository::save(&storage, &metric).await.unwrap();
    }

    let count = MetricRepository::count_all(&storage).await.unwrap();
    assert_eq!(count, 3);
}

// ========================================================================
// Tests for find_filtered() - N+1 Query Optimization
// ========================================================================

#[tokio::test]
async fn test_find_filtered_no_filters_returns_all() {
    use detrix_application::ports::MetricFilter;

    let storage = create_test_storage().await;

    // Create metrics with different properties
    for i in 1..=5 {
        let metric = create_test_metric(&format!("unfiltered_{}", i)).await;
        MetricRepository::save(&storage, &metric).await.unwrap();
    }

    // Query with no filters
    let filter = MetricFilter::default();
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 10, 0)
        .await
        .unwrap();

    assert_eq!(metrics.len(), 5);
    assert_eq!(total, 5);
}

#[tokio::test]
async fn test_find_filtered_by_connection_id() {
    use detrix_application::ports::MetricFilter;

    let storage = create_test_storage().await;

    // Create metrics for different connections
    let mut m1 = create_test_metric("conn1_metric1").await;
    m1.connection_id = ConnectionId::from("conn-alpha");
    MetricRepository::save(&storage, &m1).await.unwrap();

    let mut m2 = create_test_metric("conn1_metric2").await;
    m2.connection_id = ConnectionId::from("conn-alpha");
    MetricRepository::save(&storage, &m2).await.unwrap();

    let mut m3 = create_test_metric("conn2_metric1").await;
    m3.connection_id = ConnectionId::from("conn-beta");
    MetricRepository::save(&storage, &m3).await.unwrap();

    // Filter by conn-alpha
    let filter = MetricFilter {
        connection_id: Some(ConnectionId::from("conn-alpha")),
        enabled: None,
        group: None,
    };
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 10, 0)
        .await
        .unwrap();

    assert_eq!(metrics.len(), 2);
    assert_eq!(total, 2);
    assert!(metrics.iter().all(|m| m.connection_id.0 == "conn-alpha"));

    // Filter by conn-beta
    let filter = MetricFilter {
        connection_id: Some(ConnectionId::from("conn-beta")),
        enabled: None,
        group: None,
    };
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 10, 0)
        .await
        .unwrap();

    assert_eq!(metrics.len(), 1);
    assert_eq!(total, 1);
    assert_eq!(metrics[0].connection_id.0, "conn-beta");
}

#[tokio::test]
async fn test_find_filtered_by_enabled() {
    use detrix_application::ports::MetricFilter;

    let storage = create_test_storage().await;

    // Create enabled and disabled metrics
    let m1 = create_test_metric("enabled_metric").await;
    let _id1 = MetricRepository::save(&storage, &m1).await.unwrap();

    let m2 = create_test_metric("disabled_metric").await;
    let id2 = MetricRepository::save(&storage, &m2).await.unwrap();

    // Disable one metric
    let mut disabled = MetricRepository::find_by_id(&storage, id2)
        .await
        .unwrap()
        .unwrap();
    disabled.enabled = false;
    MetricRepository::update(&storage, &disabled).await.unwrap();

    // Filter enabled only
    let filter = MetricFilter {
        connection_id: None,
        enabled: Some(true),
        group: None,
    };
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 10, 0)
        .await
        .unwrap();

    assert_eq!(metrics.len(), 1);
    assert_eq!(total, 1);
    assert!(metrics[0].enabled);

    // Filter disabled only
    let filter = MetricFilter {
        connection_id: None,
        enabled: Some(false),
        group: None,
    };
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 10, 0)
        .await
        .unwrap();

    assert_eq!(metrics.len(), 1);
    assert_eq!(total, 1);
    assert!(!metrics[0].enabled);
}

#[tokio::test]
async fn test_find_filtered_by_group() {
    use detrix_application::ports::MetricFilter;

    let storage = create_test_storage().await;

    // Create metrics in different groups
    let mut m1 = create_test_metric("auth_metric1").await;
    m1.group = Some("authentication".to_string());
    MetricRepository::save(&storage, &m1).await.unwrap();

    let mut m2 = create_test_metric("auth_metric2").await;
    m2.group = Some("authentication".to_string());
    MetricRepository::save(&storage, &m2).await.unwrap();

    let mut m3 = create_test_metric("payment_metric").await;
    m3.group = Some("payments".to_string());
    MetricRepository::save(&storage, &m3).await.unwrap();

    let m4 = create_test_metric("ungrouped_metric").await;
    MetricRepository::save(&storage, &m4).await.unwrap();

    // Filter by authentication group
    let filter = MetricFilter {
        connection_id: None,
        enabled: None,
        group: Some("authentication".to_string()),
    };
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 10, 0)
        .await
        .unwrap();

    assert_eq!(metrics.len(), 2);
    assert_eq!(total, 2);
    assert!(metrics
        .iter()
        .all(|m| m.group == Some("authentication".to_string())));

    // Filter by payments group
    let filter = MetricFilter {
        connection_id: None,
        enabled: None,
        group: Some("payments".to_string()),
    };
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 10, 0)
        .await
        .unwrap();

    assert_eq!(metrics.len(), 1);
    assert_eq!(total, 1);
}

#[tokio::test]
async fn test_find_filtered_combined_filters() {
    use detrix_application::ports::MetricFilter;

    let storage = create_test_storage().await;

    // Create a matrix of metrics: 2 connections x 2 groups x 2 enabled states
    for conn in ["conn-1", "conn-2"] {
        for group in ["group-a", "group-b"] {
            for (i, enabled) in [true, false].iter().enumerate() {
                let mut m = create_test_metric(&format!("{}_{}_{}", conn, group, i)).await;
                m.connection_id = ConnectionId::from(conn);
                m.group = Some(group.to_string());
                let id = MetricRepository::save(&storage, &m).await.unwrap();

                if !enabled {
                    let mut saved = MetricRepository::find_by_id(&storage, id)
                        .await
                        .unwrap()
                        .unwrap();
                    saved.enabled = false;
                    MetricRepository::update(&storage, &saved).await.unwrap();
                }
            }
        }
    }

    // Total should be 8 metrics
    let filter = MetricFilter::default();
    let (_, total) = MetricRepository::find_filtered(&storage, &filter, 100, 0)
        .await
        .unwrap();
    assert_eq!(total, 8);

    // Filter: conn-1, group-a, enabled
    let filter = MetricFilter {
        connection_id: Some(ConnectionId::from("conn-1")),
        enabled: Some(true),
        group: Some("group-a".to_string()),
    };
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 10, 0)
        .await
        .unwrap();

    assert_eq!(metrics.len(), 1);
    assert_eq!(total, 1);
    assert_eq!(metrics[0].connection_id.0, "conn-1");
    assert_eq!(metrics[0].group, Some("group-a".to_string()));
    assert!(metrics[0].enabled);
}

#[tokio::test]
async fn test_find_filtered_pagination() {
    use detrix_application::ports::MetricFilter;

    let storage = create_test_storage().await;

    // Create 10 metrics
    for i in 1..=10 {
        let metric = create_test_metric(&format!("paginated_{:02}", i)).await;
        MetricRepository::save(&storage, &metric).await.unwrap();
    }

    let filter = MetricFilter::default();

    // First page (3 items)
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 3, 0)
        .await
        .unwrap();
    assert_eq!(metrics.len(), 3);
    assert_eq!(total, 10);

    // Second page (3 items)
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 3, 3)
        .await
        .unwrap();
    assert_eq!(metrics.len(), 3);
    assert_eq!(total, 10);

    // Last page (1 item)
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 3, 9)
        .await
        .unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(total, 10);

    // Beyond end
    let (metrics, total) = MetricRepository::find_filtered(&storage, &filter, 3, 15)
        .await
        .unwrap();
    assert_eq!(metrics.len(), 0);
    assert_eq!(total, 10);
}

// ========================================================================
// Tests for get_group_summaries() - GROUP BY Aggregation
// ========================================================================

#[tokio::test]
async fn test_get_group_summaries_empty_database() {
    let storage = create_test_storage().await;

    let summaries = MetricRepository::get_group_summaries(&storage)
        .await
        .unwrap();

    assert!(summaries.is_empty());
}

#[tokio::test]
async fn test_get_group_summaries_single_group() {
    let storage = create_test_storage().await;

    // Create 3 metrics in one group
    for i in 1..=3 {
        let mut m = create_test_metric(&format!("single_group_{}", i)).await;
        m.group = Some("api-calls".to_string());
        MetricRepository::save(&storage, &m).await.unwrap();
    }

    let summaries = MetricRepository::get_group_summaries(&storage)
        .await
        .unwrap();

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].name, Some("api-calls".to_string()));
    assert_eq!(summaries[0].metric_count, 3);
    assert_eq!(summaries[0].enabled_count, 3); // All enabled by default
}

#[tokio::test]
async fn test_get_group_summaries_multiple_groups() {
    let storage = create_test_storage().await;

    // Group A: 2 metrics
    for i in 1..=2 {
        let mut m = create_test_metric(&format!("group_a_{}", i)).await;
        m.group = Some("group-a".to_string());
        MetricRepository::save(&storage, &m).await.unwrap();
    }

    // Group B: 3 metrics
    for i in 1..=3 {
        let mut m = create_test_metric(&format!("group_b_{}", i)).await;
        m.group = Some("group-b".to_string());
        MetricRepository::save(&storage, &m).await.unwrap();
    }

    let summaries = MetricRepository::get_group_summaries(&storage)
        .await
        .unwrap();

    assert_eq!(summaries.len(), 2);

    let group_a = summaries
        .iter()
        .find(|s| s.name == Some("group-a".to_string()));
    let group_b = summaries
        .iter()
        .find(|s| s.name == Some("group-b".to_string()));

    assert!(group_a.is_some());
    assert!(group_b.is_some());
    assert_eq!(group_a.unwrap().metric_count, 2);
    assert_eq!(group_b.unwrap().metric_count, 3);
}

#[tokio::test]
async fn test_get_group_summaries_with_ungrouped() {
    let storage = create_test_storage().await;

    // Grouped metrics
    let mut m1 = create_test_metric("grouped_1").await;
    m1.group = Some("my-group".to_string());
    MetricRepository::save(&storage, &m1).await.unwrap();

    // Ungrouped metrics (group = None)
    let m2 = create_test_metric("ungrouped_1").await;
    MetricRepository::save(&storage, &m2).await.unwrap();

    let m3 = create_test_metric("ungrouped_2").await;
    MetricRepository::save(&storage, &m3).await.unwrap();

    let summaries = MetricRepository::get_group_summaries(&storage)
        .await
        .unwrap();

    assert_eq!(summaries.len(), 2);

    // Find ungrouped (name = None, represented as "default" in API)
    let ungrouped = summaries.iter().find(|s| s.name.is_none());
    let grouped = summaries
        .iter()
        .find(|s| s.name == Some("my-group".to_string()));

    assert!(ungrouped.is_some());
    assert!(grouped.is_some());
    assert_eq!(ungrouped.unwrap().metric_count, 2);
    assert_eq!(grouped.unwrap().metric_count, 1);
}

#[tokio::test]
async fn test_get_group_summaries_enabled_count() {
    let storage = create_test_storage().await;

    // Create 4 metrics in same group, disable 2
    for i in 1..=4 {
        let m = create_test_metric(&format!("count_test_{}", i)).await;
        let mut saved = m.clone();
        saved.group = Some("test-group".to_string());
        let id = MetricRepository::save(&storage, &saved).await.unwrap();

        // Disable metrics 3 and 4
        if i > 2 {
            let mut metric = MetricRepository::find_by_id(&storage, id)
                .await
                .unwrap()
                .unwrap();
            metric.enabled = false;
            MetricRepository::update(&storage, &metric).await.unwrap();
        }
    }

    let summaries = MetricRepository::get_group_summaries(&storage)
        .await
        .unwrap();

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].metric_count, 4);
    assert_eq!(summaries[0].enabled_count, 2); // Only 2 are enabled
}

#[tokio::test]
async fn test_get_group_summaries_integration_with_list_groups() {
    // This test verifies that get_group_summaries returns data
    // consistent with what the API would expect for list_groups
    let storage = create_test_storage().await;

    // Setup: Multiple groups with various enabled states
    let groups_config = [
        ("auth", 3, 2),     // 3 total, 2 enabled
        ("payments", 2, 2), // 2 total, 2 enabled
        ("logging", 4, 1),  // 4 total, 1 enabled
    ];

    for (group_name, total, enabled) in groups_config.iter() {
        for i in 0..*total {
            let mut m = create_test_metric(&format!("{}_{}", group_name, i)).await;
            m.group = Some(group_name.to_string());
            let id = MetricRepository::save(&storage, &m).await.unwrap();

            // Disable metrics beyond enabled count
            if i >= *enabled {
                let mut metric = MetricRepository::find_by_id(&storage, id)
                    .await
                    .unwrap()
                    .unwrap();
                metric.enabled = false;
                MetricRepository::update(&storage, &metric).await.unwrap();
            }
        }
    }

    let summaries = MetricRepository::get_group_summaries(&storage)
        .await
        .unwrap();

    assert_eq!(summaries.len(), 3);

    // Verify each group
    for (group_name, expected_total, expected_enabled) in groups_config.iter() {
        let summary = summaries
            .iter()
            .find(|s| s.name == Some(group_name.to_string()))
            .unwrap_or_else(|| panic!("Group {} should exist", group_name));

        assert_eq!(
            summary.metric_count, *expected_total as u64,
            "Group {} total count mismatch",
            group_name
        );
        assert_eq!(
            summary.enabled_count, *expected_enabled as u64,
            "Group {} enabled count mismatch",
            group_name
        );
    }
}
