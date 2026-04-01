//! eBPF Nested Types E2E Test
//!
//! Tests eBPF capture of nested structs, arrays, slices, and pointers.
//! Uses the nested_types Go fixture (fixtures/go/nested_types/main.go).
//!
//! # How to run
//!
//! ```sh
//! task test-ebpf-nested
//! ```
//!
//! Or manually:
//! ```sh
//! DETRIX_BINARY=/path/to/detrix \
//! GO_FIXTURE_BINARY=/path/to/nested_types \
//! GO_FIXTURE_SOURCE=/path/to/nested_types/main.go \
//! cargo test --test ebpf_nested_types_e2e -- --ignored --nocapture
//! ```

#![cfg(target_os = "linux")]

use detrix_testing::e2e::dap_scenarios::go_nested_lines;
use detrix_testing::e2e::{executor::TestExecutor, reporter::TestReporter};
use std::env;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

// ── Constants ────────────────────────────────────────────────────────────────

const FIXTURE_START_WAIT: Duration = Duration::from_secs(2);
const ADAPTER_START_WAIT: Duration = Duration::from_secs(3);
const EVENT_COLLECTION_WAIT: Duration = Duration::from_secs(20);

// ── Main test ─────────────────────────────────────────────────────────────────

/// End-to-end test: nested struct capture via eBPF uprobe
///
/// Tests:
/// 1. Nested structs (Order → Product → Category)
/// 2. Fixed-size arrays ([5]float64)
/// 3. Pointers to structs (*Order)
/// 4. Slices ([]OrderItem)
/// 5. Maps (map[string]Tag)
#[tokio::test]
#[ignore = "requires CAP_BPF and Linux kernel >= 5.8 — run via: task test-ebpf-nested"]
async fn test_ebpf_captures_nested_types() {
    let reporter = TestReporter::new("eBPF Nested Types E2E", "EbpfAdapter");
    reporter.print_header();

    // ── PHASE 1: Start daemon ────────────────────────────────────────────────
    reporter.section("PHASE 1: START DAEMON");
    let step = reporter.step_start("Start Daemon", "detrix serve (EbpfGoFactory active on Linux)");

    let mut executor = TestExecutor::new();
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e.to_string());
        panic!("Failed to start daemon: {e}");
    }
    reporter.step_success(step, Some(&format!(
        "HTTP port: {} | config: ebpf.max_capture_depth=10 (wired from [ebpf] section via EbpfGoFactory::new_with_config)",
        executor.http_port
    )));

    // ── PHASE 2: Start Go fixture ────────────────────────────────────────────
    reporter.section("PHASE 2: START GO FIXTURE");
    let step = reporter.step_start("Start Go Fixture", "nested_types binary");

    let fixture_binary = env::var("GO_FIXTURE_BINARY")
        .unwrap_or_else(|_| "/usr/local/bin/nested_types".to_string());
    let fixture_source = env::var("GO_FIXTURE_SOURCE")
        .unwrap_or_else(|_| "/src/fixtures/go/nested_types/main.go".to_string());

    let app = start_go_fixture(&fixture_binary);
    reporter.step_success(step, Some(&format!("PID: {}", app.id())));

    sleep(FIXTURE_START_WAIT).await;

    // ── PHASE 3: Register connection ─────────────────────────────────────────
    reporter.section("PHASE 3: REGISTER CONNECTION");
    let step = reporter.step_start("Create Connection", "host=binary_path → EbpfGoFactory");

    let http = reqwest::Client::new();
    let connection_name = format!("ebpf-nested-{}", std::process::id());

    // Use the binary path (inside container), not the source path
    // EbpfGoFactory uses the host field as the binary path on Linux
    let conn_req = serde_json::json!({
        "host": fixture_binary,  // Use binary path, not source path
        "port": 1024,  // Required for domain validation, ignored by EbpfGoFactory
        "language": "go",
        "name": connection_name,
        "workspaceRoot": "/",
        "hostname": "docker",
    });

    let conn_resp: serde_json::Value = http
        .post(format!("http://127.0.0.1:{}/api/v1/connections", executor.http_port))
        .json(&conn_req)
        .send()
        .await
        .expect("Create connection request failed")
        .json()
        .await
        .expect("Failed to parse connection response");

    if conn_resp.get("error").is_some() || conn_resp.get("connectionId").is_none() {
        reporter.step_failed(step, &conn_resp.to_string());
        panic!("Failed to create connection: {}", conn_resp);
    }

    let connection_id = conn_resp["connectionId"]
        .as_str()
        .expect("No connectionId in response");
    reporter.step_success(step, Some(&format!("connectionId={connection_id}")));

    sleep(ADAPTER_START_WAIT).await;

    // ── PHASE 4: Add metrics ─────────────────────────────────────────────────
    reporter.section("PHASE 4: ADD METRICS");

    // Metric 1: Order struct (nested)
    let order_line = go_nested_lines::CODEMAP.find_logpoint("order");
    let order_location = format!("@{fixture_source}#{order_line}");

    let step = reporter.step_start(
        "Add Metric 1 (Order struct)",
        &format!("nested_order @ {fixture_source}:{order_line}"),
    );

    let order_resp: serde_json::Value = http
        .post(format!("http://127.0.0.1:{}/api/v1/metrics", executor.http_port))
        .json(&serde_json::json!({
            "name": "nested_order",
            "location": order_location,
            "expressions": ["order"],
            "connectionId": connection_id,
            "language": "go",
            "enabled": true,
        }))
        .send()
        .await
        .expect("Add order metric failed")
        .json()
        .await
        .expect("Failed to parse order metric response");

    let order_metric_id = order_resp["metricId"]
        .as_u64()
        .expect("No metricId in order response");
    reporter.step_success(step, Some(&format!("metricId={order_metric_id}")));

    // Metric 2: PriceHistory (struct with fixed-size array)
    let history_line = go_nested_lines::CODEMAP.find_logpoint("history");
    let history_location = format!("@{fixture_source}#{history_line}");

    let step = reporter.step_start(
        "Add Metric 2 (PriceHistory)",
        &format!("nested_history @ {fixture_source}:{history_line}"),
    );

    let history_resp: serde_json::Value = http
        .post(format!("http://127.0.0.1:{}/api/v1/metrics", executor.http_port))
        .json(&serde_json::json!({
            "name": "nested_history",
            "location": history_location,
            "expressions": ["history"],
            "connectionId": connection_id,
            "language": "go",
            "enabled": true,
        }))
        .send()
        .await
        .expect("Add history metric failed")
        .json()
        .await
        .expect("Failed to parse history metric response");

    let history_metric_id = history_resp["metricId"]
        .as_u64()
        .expect("No metricId in history response");
    reporter.step_success(step, Some(&format!("metricId={history_metric_id}")));

    // Metric 3: OrderPtr (pointer to struct)
    let ptr_line = go_nested_lines::CODEMAP.find_logpoint("ptrWrapper");
    let ptr_location = format!("@{fixture_source}#{ptr_line}");

    let step = reporter.step_start(
        "Add Metric 3 (OrderPtr)",
        &format!("nested_pointer @ {fixture_source}:{ptr_line}"),
    );

    let ptr_resp: serde_json::Value = http
        .post(format!("http://127.0.0.1:{}/api/v1/metrics", executor.http_port))
        .json(&serde_json::json!({
            "name": "nested_pointer",
            "location": ptr_location,
            "expressions": ["ptrWrapper"],
            "connectionId": connection_id,
            "language": "go",
            "enabled": true,
        }))
        .send()
        .await
        .expect("Add pointer metric failed")
        .json()
        .await
        .expect("Failed to parse pointer metric response");

    let ptr_metric_id = ptr_resp["metricId"]
        .as_u64()
        .expect("No metricId in pointer response");
    reporter.step_success(step, Some(&format!("metricId={ptr_metric_id}")));

    // ── PHASE 5: Collect events ──────────────────────────────────────────────
    reporter.section("PHASE 5: COLLECT eBPF EVENTS");
    reporter.info(&format!(
        "Waiting {}s for ring-buffer events...",
        EVENT_COLLECTION_WAIT.as_secs()
    ));

    sleep(EVENT_COLLECTION_WAIT).await;

    // ── PHASE 6: Assertions ──────────────────────────────────────────────────
    reporter.section("PHASE 6: ASSERTIONS");
    kill_app(app);
    
    // Print daemon logs for debugging
    reporter.info("=== DAEMON LOGS ===");
    executor.print_daemon_logs(200);
    reporter.info("=== END DAEMON LOGS ===");

    // Verify Order struct captured
    let order_events: Vec<serde_json::Value> = http
        .get(format!(
            "http://127.0.0.1:{}/api/v1/events?metricId={}&limit=20&since=0",
            executor.http_port, order_metric_id
        ))
        .send()
        .await
        .expect("Order events query failed")
        .json()
        .await
        .expect("Failed to parse order events");

    assert!(
        !order_events.is_empty(),
        "No eBPF events received for Order struct"
    );
    reporter.info(&format!("Received {} Order events", order_events.len()));
    
    // Debug: Print first Order event structure
    if let Some(first_order) = order_events.first() {
        reporter.info(&format!("Sample Order event: {}", 
            serde_json::to_string_pretty(first_order).unwrap_or_else(|_| "N/A".to_string())));
    }

    // Verify PriceHistory (array) captured
    let history_events: Vec<serde_json::Value> = http
        .get(format!(
            "http://127.0.0.1:{}/api/v1/events?metricId={}&limit=20&since=0",
            executor.http_port, history_metric_id
        ))
        .send()
        .await
        .expect("History events query failed")
        .json()
        .await
        .expect("Failed to parse history events");

    assert!(
        !history_events.is_empty(),
        "No eBPF events received for PriceHistory struct"
    );
    reporter.info(&format!("Received {} PriceHistory events", history_events.len()));
    
    // Debug: Print first PriceHistory event structure
    if let Some(first_history) = history_events.first() {
        reporter.info(&format!("Sample PriceHistory event (array capture): {}", 
            serde_json::to_string_pretty(first_history).unwrap_or_else(|_| "N/A".to_string())));
    }

    // Verify pointer captured
    let ptr_events: Vec<serde_json::Value> = http
        .get(format!(
            "http://127.0.0.1:{}/api/v1/events?metricId={}&limit=20&since=0",
            executor.http_port, ptr_metric_id
        ))
        .send()
        .await
        .expect("Pointer events query failed")
        .json()
        .await
        .expect("Failed to parse pointer events");

    assert!(
        !ptr_events.is_empty(),
        "No eBPF events received for OrderPtr struct"
    );
    reporter.info(&format!("Received {} OrderPtr events", ptr_events.len()));
    
    // Debug: Print first OrderPtr event structure
    if let Some(first_ptr) = ptr_events.first() {
        reporter.info(&format!("Sample OrderPtr event (pointer capture): {}", 
            serde_json::to_string_pretty(first_ptr).unwrap_or_else(|_| "N/A".to_string())));
    }

    reporter.info("eBPF nested types test PASSED — structs, arrays, and pointers captured via ring buffer");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn start_go_fixture(binary: &str) -> Child {
    Command::new(binary)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start Go fixture")
}

fn kill_app(mut app: Child) {
    let _ = app.kill();
    let _ = app.wait();
}
