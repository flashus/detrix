//! E2E Test: Multi-Expression Metrics
//!
//! Tests that a single metric with multiple expressions captures all values
//! in a single event for each supported language (Python, Go, Rust).
//!
//! Each test:
//! 1. Starts daemon + language debugger
//! 2. Creates a connection
//! 3. Adds ONE metric with 3 expressions: symbol, quantity, price
//! 4. Waits for events
//! 5. Verifies each event has `values` array with all 3 expression values
//!
//! This validates the end-to-end flow of the multi-expression feature:
//! DAP logpoint → `{symbol}\x1F{quantity}\x1F{price}` → parsed into Vec<ExpressionValue>

use detrix_testing::e2e::{
    executor::TestExecutor, reporter::TestReporter, require_tool, ToolDependency,
};
use std::time::Duration;

/// Configuration for a multi-expression E2E test
struct MultiExprTestConfig {
    /// Display label for the test (e.g., "Multi-Expression Python")
    label: &'static str,
    /// Language name for REST API (e.g., "python")
    language: &'static str,
    /// Tool dependency to check before running
    tool: ToolDependency,
    /// Relative fixture path from workspace root
    fixture_path: &'static str,
    /// Line number to set the logpoint at
    line: u32,
    /// How long to wait for events (seconds)
    wait_secs: u64,
}

/// Create an HTTP client for raw REST API calls
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client")
}

/// Register a connection via REST API, returns connection_id
async fn create_connection(
    client: &reqwest::Client,
    http_port: u16,
    host: &str,
    debugger_port: u16,
    language: &str,
) -> String {
    let resp: serde_json::Value = client
        .post(format!("http://127.0.0.1:{}/api/v1/connections", http_port))
        .json(&serde_json::json!({
            "host": host,
            "port": debugger_port,
            "language": language,
            "name": format!("multi-expr-e2e-{}", language),
            "workspaceRoot": "/e2e-test",
            "hostname": "localhost",
        }))
        .send()
        .await
        .expect("Failed to create connection")
        .json()
        .await
        .expect("Failed to parse connection response");

    resp["connection"]["id"]
        .as_str()
        .or_else(|| resp["connectionId"].as_str())
        .expect("No connection ID in response")
        .to_string()
}

/// Add a metric with multiple expressions via REST API, returns metric_id
async fn add_multi_expr_metric(
    client: &reqwest::Client,
    http_port: u16,
    name: &str,
    file_path: &str,
    line: u32,
    expressions: &[&str],
    connection_id: &str,
    language: &str,
) -> u64 {
    let resp: serde_json::Value = client
        .post(format!("http://127.0.0.1:{}/api/v1/metrics", http_port))
        .json(&serde_json::json!({
            "name": name,
            "connectionId": connection_id,
            "location": {
                "file": file_path,
                "line": line,
            },
            "expressions": expressions,
            "language": language,
            "enabled": true,
        }))
        .send()
        .await
        .expect("Failed to add metric")
        .json()
        .await
        .expect("Failed to parse metric response");

    resp["metricId"].as_u64().expect("No metricId in response")
}

/// Query events for a metric, returns raw JSON array
async fn query_events(
    client: &reqwest::Client,
    http_port: u16,
    metric_id: u64,
    since_micros: i64,
) -> Vec<serde_json::Value> {
    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/v1/events?metricId={}&limit=20&since={}",
            http_port, metric_id, since_micros
        ))
        .send()
        .await
        .expect("Failed to query events");

    resp.json().await.expect("Failed to parse events response")
}

/// Verify that events contain multi-expression values
fn verify_multi_expr_events(
    events: &[serde_json::Value],
    expected_expressions: &[&str],
    reporter: &std::sync::Arc<TestReporter>,
) -> bool {
    if events.is_empty() {
        reporter.error("No events captured!");
        return false;
    }

    let mut all_ok = true;

    for (i, event) in events.iter().enumerate().take(3) {
        let values = event["values"].as_array();
        if values.is_none() {
            reporter.error(&format!("Event {} has no 'values' array", i));
            all_ok = false;
            continue;
        }
        let values = values.unwrap();

        if values.len() != expected_expressions.len() {
            reporter.error(&format!(
                "Event {}: expected {} values, got {}",
                i,
                expected_expressions.len(),
                values.len()
            ));
            all_ok = false;
            continue;
        }

        for (j, expected_expr) in expected_expressions.iter().enumerate() {
            let val = &values[j];
            let expr = val["expression"].as_str().unwrap_or("<missing>");
            let value_json = val["valueJson"].as_str().unwrap_or("<missing>");

            if expr != *expected_expr {
                reporter.error(&format!(
                    "Event {}, value {}: expected expression '{}', got '{}'",
                    i, j, expected_expr, expr
                ));
                all_ok = false;
            }

            if value_json.is_empty() || value_json == "<missing>" {
                reporter.error(&format!(
                    "Event {}, value {}: empty valueJson for '{}'",
                    i, j, expected_expr
                ));
                all_ok = false;
            } else {
                reporter.info(&format!(
                    "  Event {}, {} = {}",
                    i, expected_expr, value_json
                ));
            }
        }
    }

    all_ok
}

/// Shared test runner for all multi-expression E2E tests.
///
/// Handles the full lifecycle: daemon startup, debugger launch, connection
/// creation, metric addition, event capture, and verification.
async fn run_multi_expr_test(config: MultiExprTestConfig) {
    if !require_tool(config.tool).await {
        return;
    }

    let mut executor = TestExecutor::new();
    let reporter = TestReporter::new(config.label, "REST");
    reporter.print_header();

    // Start daemon
    reporter.section("SETUP");
    let step = reporter.step_start("Start Daemon", "Start Detrix daemon");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e.to_string());
        executor.print_daemon_logs(50);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some(&format!("HTTP port: {}", executor.http_port)));

    // Start language debugger
    let source_path = executor.workspace_root.join(config.fixture_path);
    let source_str = source_path.to_str().unwrap();
    let (debugger_port, debugger_label) = match config.language {
        "python" => {
            let step = reporter.step_start("Start debugpy", "Start Python debugger");
            if let Err(e) = executor.start_debugpy(source_str).await {
                reporter.step_failed(step, &e.to_string());
                executor.print_debugpy_logs(50);
                panic!("Failed to start debugpy: {}", e);
            }
            reporter.step_success(
                step,
                Some(&format!("debugpy port: {}", executor.debugpy_port)),
            );
            (executor.debugpy_port, "debugpy")
        }
        "go" => {
            let step = reporter.step_start("Start Delve", "Build Go binary and start DAP");
            if let Err(e) = executor.start_delve(source_str).await {
                reporter.step_failed(step, &e.to_string());
                executor.print_delve_logs(50);
                panic!("Failed to start delve: {}", e);
            }
            reporter.step_success(step, Some(&format!("Delve port: {}", executor.delve_port)));
            (executor.delve_port, "Delve")
        }
        "rust" => {
            let step = reporter.step_start("Start lldb-dap", "Build Rust binary and start DAP");
            if let Err(e) = executor.start_lldb(source_str).await {
                reporter.step_failed(step, &e.to_string());
                executor.print_lldb_logs(50);
                panic!("Failed to start lldb: {}", e);
            }
            reporter.step_success(step, Some(&format!("lldb port: {}", executor.lldb_port)));
            (executor.lldb_port, "lldb-dap")
        }
        other => panic!("Unsupported language: {}", other),
    };

    let client = http_client();

    // Create connection
    reporter.section("CONNECTION");
    let step = reporter.step_start(
        "Create Connection",
        &format!("Register {} connection", debugger_label),
    );
    let connection_id = create_connection(
        &client,
        executor.http_port,
        "127.0.0.1",
        debugger_port,
        config.language,
    )
    .await;
    reporter.step_success(step, Some(&format!("ID: {}", connection_id)));

    // Wait for DAP handshake
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Add multi-expression metric
    reporter.section("ADD MULTI-EXPRESSION METRIC");
    let expressions = ["symbol", "quantity", "price"];
    let step = reporter.step_start(
        "Add Metric",
        &format!("expressions: {:?} at line {}", &expressions, config.line),
    );

    let session_start = chrono::Utc::now().timestamp_micros();
    let metric_name = format!("multi_expr_{}", config.language);

    let metric_id = add_multi_expr_metric(
        &client,
        executor.http_port,
        &metric_name,
        source_str,
        config.line,
        &expressions,
        &connection_id,
        config.language,
    )
    .await;
    reporter.step_success(step, Some(&format!("Metric ID: {}", metric_id)));

    // Wait for events
    reporter.section("VERIFY EVENTS");
    reporter.info(&format!(
        "Waiting {} seconds for logpoint events...",
        config.wait_secs
    ));
    tokio::time::sleep(Duration::from_secs(config.wait_secs)).await;

    let events = query_events(&client, executor.http_port, metric_id, session_start).await;
    let step = reporter.step_start("Check Events", &format!("{} events captured", events.len()));

    let ok = verify_multi_expr_events(&events, &expressions, &reporter);

    if ok {
        reporter.step_success(
            step,
            Some(&format!(
                "{} events, each with {} expression values",
                events.len(),
                expressions.len()
            )),
        );
    } else {
        reporter.step_failed(step, "Multi-expression verification failed");
        executor.print_daemon_logs(100);
        // Print language-specific debugger logs on failure
        match config.language {
            "python" => executor.print_debugpy_logs(50),
            "go" => executor.print_delve_logs(50),
            "rust" => executor.print_lldb_logs(50),
            _ => {}
        }
        reporter.print_footer(false);
        panic!("Multi-expression {} test failed", config.label);
    }

    reporter.print_footer(true);
    assert!(
        !events.is_empty(),
        "Expected at least 1 multi-expression event"
    );
}

// ============================================================================
// PYTHON: Multi-Expression Logpoint
// ============================================================================

/// Test multi-expression metric with Python/debugpy
///
/// Uses trade_bot_forever.py fixture:
/// - Line 59: `order_id = place_order(symbol, quantity, price)`
///   At this line, symbol (str), quantity (int), price (float) are in scope
#[tokio::test]
async fn test_multi_expr_python() {
    run_multi_expr_test(MultiExprTestConfig {
        label: "Multi-Expression Python",
        language: "python",
        tool: ToolDependency::Debugpy,
        fixture_path: "fixtures/python/trade_bot_forever.py",
        line: 59,
        wait_secs: 12,
    })
    .await;
}

// ============================================================================
// GO: Multi-Expression Logpoint
// ============================================================================

/// Test multi-expression metric with Go/Delve
///
/// Uses detrix_example_app.go fixture:
/// - Line 122: `orderID := placeOrder(symbol, quantity, price)`
///   At this line, symbol (str), quantity (int), price (float64) are in scope
#[tokio::test]
async fn test_multi_expr_go() {
    run_multi_expr_test(MultiExprTestConfig {
        label: "Multi-Expression Go",
        language: "go",
        tool: ToolDependency::Delve,
        fixture_path: "fixtures/go/detrix_example_app.go",
        line: 122,
        wait_secs: 12,
    })
    .await;
}

// ============================================================================
// RUST: Multi-Expression Logpoint
// ============================================================================

/// Test multi-expression metric with Rust/lldb-dap
///
/// Uses fixtures/rust/src/main.rs fixture:
/// - Line 114: `let order_id = place_order(symbol, quantity, price);`
///   At this line, symbol (&str), quantity (i32), price (f64) are in scope
///
/// Note: LLDB evaluates logpoints BEFORE the line executes, so we use
/// line 114 where symbol, quantity, price are already assigned (lines 107-111).
#[tokio::test]
async fn test_multi_expr_rust() {
    run_multi_expr_test(MultiExprTestConfig {
        label: "Multi-Expression Rust",
        language: "rust",
        tool: ToolDependency::LldbDap,
        fixture_path: "fixtures/rust/src/main.rs",
        line: 114,
        wait_secs: 15,
    })
    .await;
}
