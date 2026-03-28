//! LSP Purity Resolution E2E Tests
//!
//! Tests the full validation pipeline with MockPurityAnalyzer:
//! - Phase 1: Tree-sitter classifies user-defined functions as Unknown
//! - Phase 2: LSP purity resolver queries the analyzer
//!   - Pure → metric created successfully
//!   - Impure → SafetyViolation, metric rejected
//!   - Analyzer unavailable → graceful degradation (warning only)
//!
//! These tests use MockPurityAnalyzer (no real LSP server needed).

mod test_support;

use detrix_application::services::{AdapterLifecycleManager, EventCaptureService, MetricService};
use detrix_application::{
    ConnectionRepositoryRef, EventRepositoryRef, MetricRepository, MetricRepositoryRef,
    PurityAnalyzer, PurityAnalyzerRef, VfsRef,
};
use detrix_core::{
    ConnectionId, Location, Metric, MetricEvent, MetricMode, SafetyLevel, SourceLanguage,
    SystemEvent,
};
use detrix_testing::fixtures::test_py_path;
use detrix_testing::{
    MockConnectionRepository, MockDapAdapterFactory, MockEventRepository, MockMetricRepository,
    MockPurityAnalyzer, MockVfs,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Build a MetricService with a MockPurityAnalyzer.
fn build_service_with_analyzer(
    analyzer: MockPurityAnalyzer,
) -> (Arc<MetricService>, Arc<MockMetricRepository>) {
    let metric_repo = Arc::new(MockMetricRepository::new());
    let event_repo = Arc::new(MockEventRepository::new());
    let conn_repo = Arc::new(MockConnectionRepository::new());
    let factory = Arc::new(MockDapAdapterFactory::new());
    let vfs = Arc::new(MockVfs::new()) as VfsRef;

    let (event_tx, _) = broadcast::channel::<MetricEvent>(16);
    let (system_event_tx, _) = broadcast::channel::<SystemEvent>(16);

    let event_capture = Arc::new(EventCaptureService::new(
        Arc::clone(&event_repo) as EventRepositoryRef
    ));

    let lifecycle = Arc::new(AdapterLifecycleManager::new(
        event_capture,
        event_tx,
        system_event_tx.clone(),
        factory,
        Arc::clone(&metric_repo) as MetricRepositoryRef,
        Arc::clone(&conn_repo) as ConnectionRepositoryRef,
        Arc::clone(&vfs),
    ));

    let lang = analyzer.language();
    let mut analyzers: HashMap<SourceLanguage, PurityAnalyzerRef> = HashMap::new();
    analyzers.insert(lang, Arc::new(analyzer));

    let service = MetricService::builder(
        Arc::clone(&metric_repo) as MetricRepositoryRef,
        lifecycle,
        system_event_tx,
    )
    .purity_analyzers(analyzers)
    .vfs(vfs)
    .build();

    (Arc::new(service), metric_repo)
}

/// Create a Trusted-mode metric with a user-defined function call expression.
///
/// The function name won't be in any whitelist/blacklist, so tree-sitter
/// classifies it as Unknown. Phase 2 (LSP) will query the PurityAnalyzer.
fn trusted_metric_with_call(name: &str, expression: &str) -> Metric {
    Metric {
        id: None,
        name: name.to_string(),
        connection_id: ConnectionId::from("test-conn"),
        group: None,
        location: Location {
            file: test_py_path(),
            line: 10,
        },
        expressions: vec![expression.to_string()],
        language: SourceLanguage::Python,
        mode: MetricMode::Stream,
        enabled: false, // Disabled — no adapter needed
        condition: None,
        safety_level: SafetyLevel::Trusted,
        created_at: None,
        user_id: None,
        agent_id: None,
        capture_stack_trace: false,
        stack_trace_ttl: None,
        stack_trace_slice: None,
        capture_memory_snapshot: false,
        snapshot_scope: None,
        snapshot_ttl: None,
        anchor: None,
        anchor_status: Default::default(),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

/// Unknown function resolved as Pure via LSP → metric created successfully.
#[tokio::test]
async fn test_lsp_pure_function_allows_metric_creation() {
    let analyzer = MockPurityAnalyzer::new(SourceLanguage::Python).with_pure("calculate_total");

    let (service, storage) = build_service_with_analyzer(analyzer);

    let metric = trusted_metric_with_call("pure-metric", "calculate_total(items)");
    let result = service.add_metric(metric, false, Default::default()).await;

    assert!(
        result.is_ok(),
        "Metric with pure function should be created, got: {:?}",
        result.err()
    );

    // Verify metric was persisted
    let stored = storage.find_by_name("pure-metric").await.unwrap();
    assert!(stored.is_some(), "Metric should be in storage");
}

/// Unknown function resolved as Impure via LSP → SafetyViolation, metric rejected.
#[tokio::test]
async fn test_lsp_impure_function_blocks_metric_creation() {
    let analyzer = MockPurityAnalyzer::new(SourceLanguage::Python)
        .with_impure("save_to_database", "writes to file system");

    let (service, storage) = build_service_with_analyzer(analyzer);

    let metric = trusted_metric_with_call("impure-metric", "save_to_database(data)");
    let result = service.add_metric(metric, false, Default::default()).await;

    assert!(
        result.is_err(),
        "Metric with impure function should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("impure") || err.contains("Safety violation"),
        "Error should mention impurity, got: {}",
        err
    );

    // Verify no metric was persisted
    let stored = storage.find_by_name("impure-metric").await.unwrap();
    assert!(stored.is_none(), "No metric should be stored");
}

/// Analyzer ensure_ready() fails → graceful degradation, metric still created
/// (unknown function passes with warning, same as no-analyzer behavior).
#[tokio::test]
async fn test_lsp_unavailable_degrades_gracefully() {
    let analyzer = MockPurityAnalyzer::new(SourceLanguage::Python)
        .with_ready_error("LSP server not installed");

    let (service, storage) = build_service_with_analyzer(analyzer);

    let metric = trusted_metric_with_call("degraded-metric", "some_unknown_func(x)");
    let result = service.add_metric(metric, false, Default::default()).await;

    assert!(
        result.is_ok(),
        "Metric should be created when LSP unavailable (graceful degradation), got: {:?}",
        result.err()
    );

    let stored = storage.find_by_name("degraded-metric").await.unwrap();
    assert!(stored.is_some(), "Metric should be in storage");
}

/// Strict mode: unknown functions are blocked by tree-sitter BEFORE LSP.
/// LSP analyzer should NOT be called at all.
#[tokio::test]
async fn test_strict_mode_blocks_without_lsp() {
    let analyzer =
        Arc::new(MockPurityAnalyzer::new(SourceLanguage::Python).with_pure("calculate_total"));

    let metric_repo = Arc::new(MockMetricRepository::new());
    let event_repo = Arc::new(MockEventRepository::new());
    let conn_repo = Arc::new(MockConnectionRepository::new());
    let factory = Arc::new(MockDapAdapterFactory::new());
    let vfs = Arc::new(MockVfs::new()) as VfsRef;

    let (event_tx, _) = broadcast::channel::<MetricEvent>(16);
    let (system_event_tx, _) = broadcast::channel::<SystemEvent>(16);

    let lifecycle = Arc::new(AdapterLifecycleManager::new(
        Arc::new(EventCaptureService::new(
            Arc::clone(&event_repo) as EventRepositoryRef
        )),
        event_tx,
        system_event_tx.clone(),
        factory,
        Arc::clone(&metric_repo) as MetricRepositoryRef,
        Arc::clone(&conn_repo) as ConnectionRepositoryRef,
        Arc::clone(&vfs),
    ));

    let mut analyzers: HashMap<SourceLanguage, PurityAnalyzerRef> = HashMap::new();
    analyzers.insert(
        SourceLanguage::Python,
        Arc::clone(&analyzer) as PurityAnalyzerRef,
    );

    let service = MetricService::builder(
        Arc::clone(&metric_repo) as MetricRepositoryRef,
        lifecycle,
        system_event_tx,
    )
    .purity_analyzers(analyzers)
    .vfs(vfs)
    .build();

    // Strict mode — unknown function should be blocked by tree-sitter
    let mut metric = trusted_metric_with_call("strict-metric", "calculate_total(items)");
    metric.safety_level = SafetyLevel::Strict;

    let result = service.add_metric(metric, false, Default::default()).await;

    assert!(
        result.is_err(),
        "Strict mode should block unknown function without LSP"
    );

    // Verify LSP was NOT called
    assert_eq!(
        analyzer.analyze_count(),
        0,
        "LSP should not be called in Strict mode"
    );
}
