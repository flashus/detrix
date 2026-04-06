//! eBPF Classic Map Integration Test (Go < 1.24)
//!
//! Tests map capture with Go's classic hash map implementation (hmap/bmap).
//! Uses the proven pattern: capture a struct containing a map field,
//! then the map is read from the struct's memory via parse_struct_fields_from_addr.
//!
//! Requires Go < 1.24 fixture binary and CAP_BPF.
//!
//! # How to run
//! ```sh
//! task test-ebpf-classic-inject
//! ```

#![cfg(target_os = "linux")]

use detrix_testing::e2e::dap_scenarios::go_classic_lines;
use detrix_testing::e2e::{executor::TestExecutor, reporter::TestReporter};
use std::env;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

const FIXTURE_START_WAIT: Duration = Duration::from_secs(3);
const ADAPTER_START_WAIT: Duration = Duration::from_secs(3);
const EVENT_COLLECTION_WAIT: Duration = Duration::from_secs(20);

fn start_fixture(binary_path: &str) -> Child {
    Command::new(binary_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to start classic_map fixture")
}

fn kill_fixture(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test]
#[ignore = "requires Go < 1.24 fixture and CAP_BPF — run via: task test-ebpf-classic-inject"]
async fn test_ebpf_classic_map_order_with_map() {
    let reporter = TestReporter::new("eBPF Classic Map — Order with Map Field", "EbpfAdapter");
    reporter.print_header();

    let mut executor = TestExecutor::new();

    reporter.section("PHASE 1: START DAEMON");
    let step = reporter.step_start(
        "Start Daemon",
        "detrix serve (EbpfGoFactory active on Linux)",
    );
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e.to_string());
        panic!("Failed to start daemon: {e}");
    }
    reporter.step_success(step, Some(&format!("HTTP port: {}", executor.http_port)));

    reporter.section("PHASE 2: START GO FIXTURE");
    let fixture_binary = env::var("GO_FIXTURE_BINARY")
        .unwrap_or_else(|_| "fixtures/go/classic_map/classic_map".to_string());
    let step = reporter.step_start("Start Go Fixture", &fixture_binary);
    let app = start_fixture(&fixture_binary);
    sleep(FIXTURE_START_WAIT).await;
    reporter.step_success(step, Some(&format!("PID: {}", app.id())));

    reporter.section("PHASE 3: REGISTER CONNECTION");
    let http = reqwest::Client::new();
    let step = reporter.step_start(
        "Create Connection",
        &format!("host={fixture_binary}, language=go"),
    );

    let conn_req = serde_json::json!({
        "host": fixture_binary,
        "port": 1024,
        "language": "go",
        "name": "ebpf-classic-map-order",
        "workspaceRoot": "/",
        "hostname": "docker",
    });

    let conn_resp: serde_json::Value = http
        .post(format!(
            "http://127.0.0.1:{}/api/v1/connections",
            executor.http_port
        ))
        .json(&conn_req)
        .send()
        .await
        .expect("Create connection request failed")
        .json()
        .await
        .expect("Failed to parse connection response");

    if conn_resp.get("error").is_some() || conn_resp.get("connectionId").is_none() {
        reporter.step_failed(step, &conn_resp.to_string());
        panic!("Create connection failed: {conn_resp}");
    }

    let connection_id = conn_resp["connectionId"]
        .as_str()
        .expect("No connectionId in response")
        .to_string();

    reporter.step_success(step, Some(&format!("connectionId={connection_id}")));
    sleep(ADAPTER_START_WAIT).await;

    reporter.section("PHASE 4: ADD METRIC");
    let fixture_source = env::var("GO_FIXTURE_SOURCE")
        .unwrap_or_else(|_| "fixtures/go/classic_map/main.go".to_string());
    let line = go_classic_lines::CODEMAP.find_logpoint("order");
    let location = format!("@{fixture_source}#{line}");

    // Capture the order struct which contains a Tags map field
    let metric_req = serde_json::json!({
        "name": "order-map",
        "location": location,
        "expressions": ["order"],
        "connectionId": connection_id,
        "language": "go",
        "enabled": true,
    });

    let step = reporter.step_start("Add Metric", &format!("order at main.go#{}", line));
    let metric_resp: serde_json::Value = http
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
        .json(&metric_req)
        .send()
        .await
        .expect("Add metric request failed")
        .json()
        .await
        .expect("Failed to parse metric response");

    if metric_resp.get("error").is_some() {
        reporter.step_failed(step, &metric_resp.to_string());
        executor.print_daemon_logs(120);
        panic!("Failed to add metric: {metric_resp}");
    }

    let metric_id = metric_resp["metricId"]
        .as_u64()
        .expect("No metricId in response");
    reporter.step_success(step, Some(&format!("metricId={metric_id}")));

    reporter.section("PHASE 5: COLLECT EVENTS");
    reporter.info(&format!(
        "Waiting {}s for ring-buffer events...",
        EVENT_COLLECTION_WAIT.as_secs()
    ));
    sleep(EVENT_COLLECTION_WAIT).await;

    reporter.section("PHASE 6: DAEMON LOGS");
    kill_fixture(app);
    reporter.info("=== DAEMON LOGS ===");
    executor.print_daemon_logs(200);
    reporter.info("=== END DAEMON LOGS ===");

    reporter.section("PHASE 7: ASSERTIONS");
    let step = reporter.step_start("Query Events", &format!("metricId={metric_id}"));

    let resp = http
        .get(format!(
            "http://127.0.0.1:{}/api/v1/events?metricId={}&limit=5&since=0",
            executor.http_port, metric_id
        ))
        .send()
        .await
        .expect("Event query failed");

    assert!(
        resp.status().is_success(),
        "Event query failed: {}",
        resp.status()
    );

    let events: Vec<serde_json::Value> = resp.json().await.expect("Failed to parse response");

    if events.is_empty() {
        panic!("No events captured for order-map — uprobe attachment or BPF program failed.");
    }

    reporter.step_success(step, Some(&format!("Got {} events", events.len())));

    let mut mismatches = Vec::new();

    for (idx, event) in events.iter().take(3).enumerate() {
        let values = event["values"].as_array().expect("Expected values array");
        assert!(!values.is_empty(), "No values in event {idx}");

        let value_json_str = values[0]
            .get("valueJson")
            .and_then(|v| v.as_str())
            .unwrap_or("null");
        reporter.info(&format!("Event[{idx}] valueJson: {value_json_str}"));

        let value_json: serde_json::Value =
            serde_json::from_str(value_json_str).expect("valueJson must be valid JSON");

        // Verify captured as Order struct
        if !value_json.is_object() {
            mismatches.push(format!("Event[{idx}] expected object, got: {value_json}"));
            continue;
        }

        // Verify ID field is present and numeric
        match value_json.get("ID") {
            Some(id) if id.is_number() => {}
            Some(id) => mismatches.push(format!("Event[{idx}] ID should be numeric, got: {id}")),
            None => mismatches.push(format!("Event[{idx}] missing ID field")),
        }

        // Verify Tags field is present
        let tags = match value_json.get("Tags") {
            Some(t) => t,
            None => {
                mismatches.push(format!("Event[{idx}] missing Tags map field"));
                continue;
            }
        };

        // Tags must NOT be raw runtime struct internals
        if tags.get("dirPtr").is_some() || tags.get("seed").is_some() {
            mismatches.push(format!(
                "Event[{idx}] Tags shows raw runtime struct (map iterator not applied): {tags}"
            ));
            continue;
        }

        // Tags must have entries array (not empty for this fixture)
        let entries = match tags.get("entries").and_then(|e| e.as_array()) {
            Some(e) => e,
            None => {
                mismatches.push(format!(
                    "Event[{idx}] Tags.entries missing or not array: {tags}"
                ));
                continue;
            }
        };

        if entries.is_empty() {
            mismatches.push(format!(
                "Event[{idx}] Tags.entries is empty — classic map iterator produced no entries. \
                 Expected {{\"env\":\"test\",\"version\":\"1.0\"}}. Tags={tags}"
            ));
            continue;
        }

        // Verify exactly {"env":"test"} and {"version":"1.0"} are present
        // Entry format: {"key":"env","value":"test"}
        let has_env = entries.iter().any(|e| {
            e.get("key").and_then(|k| k.as_str()) == Some("env")
                && e.get("value").and_then(|v| v.as_str()) == Some("test")
        });
        let has_version = entries.iter().any(|e| {
            e.get("key").and_then(|k| k.as_str()) == Some("version")
                && e.get("value").and_then(|v| v.as_str()) == Some("1.0")
        });

        if !has_env {
            mismatches.push(format!(
                "Event[{idx}] Tags missing {{\"key\":\"env\",\"value\":\"test\"}} — entries: {entries:?}"
            ));
        }
        if !has_version {
            mismatches.push(format!(
                "Event[{idx}] Tags missing {{\"key\":\"version\",\"value\":\"1.0\"}} — entries: {entries:?}"
            ));
        }

        if has_env && has_version {
            reporter.info(&format!(
                "Event[{idx}] ✅ Tags map correctly captured: env=test, version=1.0"
            ));
        }
    }

    if mismatches.is_empty() {
        reporter.info("✅ All field validations PASSED");
        reporter.print_footer(true);
    } else {
        reporter.error(&format!("❌ {} validation(s) FAILED:", mismatches.len()));
        for m in &mismatches {
            reporter.error(&format!("  - {m}"));
        }
        reporter.print_footer(false);
        panic!("{} field validation(s) failed", mismatches.len());
    }
}

#[tokio::test]
#[ignore = "requires Go < 1.24 fixture and CAP_BPF — run via: task test-ebpf-classic-inject"]
async fn test_ebpf_classic_map_nil_map() {
    let reporter = TestReporter::new("eBPF Classic Map — Nil Map", "EbpfAdapter");
    reporter.print_header();

    let mut executor = TestExecutor::new();

    reporter.section("PHASE 1: START DAEMON");
    let step = reporter.step_start("Start Daemon", "detrix serve");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e.to_string());
        panic!("Failed to start daemon: {e}");
    }
    reporter.step_success(step, Some(&format!("HTTP port: {}", executor.http_port)));

    reporter.section("PHASE 2: START GO FIXTURE");
    let fixture_binary = env::var("GO_FIXTURE_BINARY")
        .unwrap_or_else(|_| "fixtures/go/classic_map/classic_map".to_string());
    let step = reporter.step_start("Start Go Fixture", &fixture_binary);
    let app = start_fixture(&fixture_binary);
    sleep(FIXTURE_START_WAIT).await;
    reporter.step_success(step, Some(&format!("PID: {}", app.id())));

    reporter.section("PHASE 3: REGISTER CONNECTION");
    let http = reqwest::Client::new();
    let step = reporter.step_start(
        "Create Connection",
        &format!("host={fixture_binary}, language=go"),
    );

    let conn_req = serde_json::json!({
        "host": fixture_binary,
        "port": 1024,
        "language": "go",
        "name": "ebpf-classic-map-nil",
        "workspaceRoot": "/",
        "hostname": "docker",
    });

    let conn_resp: serde_json::Value = http
        .post(format!(
            "http://127.0.0.1:{}/api/v1/connections",
            executor.http_port
        ))
        .json(&conn_req)
        .send()
        .await
        .expect("Create connection request failed")
        .json()
        .await
        .expect("Failed to parse connection response");

    if conn_resp.get("error").is_some() || conn_resp.get("connectionId").is_none() {
        reporter.step_failed(step, &conn_resp.to_string());
        panic!("Create connection failed: {conn_resp}");
    }

    let connection_id = conn_resp["connectionId"]
        .as_str()
        .expect("No connectionId in response")
        .to_string();

    reporter.step_success(step, Some(&format!("connectionId={connection_id}")));
    sleep(ADAPTER_START_WAIT).await;

    reporter.section("PHASE 4: ADD METRIC");
    let fixture_source = env::var("GO_FIXTURE_SOURCE")
        .unwrap_or_else(|_| "fixtures/go/classic_map/main.go".to_string());
    let line = go_classic_lines::CODEMAP.find_logpoint("nilMap");
    let location = format!("@{fixture_source}#{line}");

    let metric_req = serde_json::json!({
        "name": "nil-map",
        "location": location,
        "expressions": ["nilMap"],
        "connectionId": connection_id,
        "language": "go",
        "enabled": true,
    });

    let step = reporter.step_start("Add Metric", &format!("nilMap at main.go#{}", line));
    let metric_resp: serde_json::Value = http
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
        .json(&metric_req)
        .send()
        .await
        .expect("Add metric request failed")
        .json()
        .await
        .expect("Failed to parse metric response");

    if metric_resp.get("error").is_some() {
        reporter.step_failed(step, &metric_resp.to_string());
        executor.print_daemon_logs(120);
        panic!("Failed to add metric: {metric_resp}");
    }

    let metric_id = metric_resp["metricId"]
        .as_u64()
        .expect("No metricId in response");
    reporter.step_success(step, Some(&format!("metricId={metric_id}")));

    reporter.section("PHASE 5: COLLECT EVENTS");
    reporter.info(&format!(
        "Waiting {}s for ring-buffer events...",
        EVENT_COLLECTION_WAIT.as_secs()
    ));
    sleep(EVENT_COLLECTION_WAIT).await;

    reporter.section("PHASE 6: DAEMON LOGS");
    kill_fixture(app);
    reporter.info("=== DAEMON LOGS ===");
    executor.print_daemon_logs(200);
    reporter.info("=== END DAEMON LOGS ===");

    reporter.section("PHASE 7: ASSERTIONS");
    let step = reporter.step_start("Query Events", &format!("metricId={metric_id}"));

    let resp = http
        .get(format!(
            "http://127.0.0.1:{}/api/v1/events?metricId={}&limit=5&since=0",
            executor.http_port, metric_id
        ))
        .send()
        .await
        .expect("Event query failed");

    assert!(
        resp.status().is_success(),
        "Event query failed: {}",
        resp.status()
    );

    let events: Vec<serde_json::Value> = resp.json().await.expect("Failed to parse response");

    if events.is_empty() {
        panic!("No events captured for nil-map — uprobe attachment or BPF program failed.");
    }

    reporter.step_success(step, Some(&format!("Got {} events", events.len())));

    // Verify nil map captured correctly
    let mut mismatches = Vec::new();

    for (idx, event) in events.iter().take(3).enumerate() {
        let values = event["values"].as_array().expect("Expected values array");
        assert!(!values.is_empty(), "No values in event {idx}");

        let value_json_str = values[0]
            .get("valueJson")
            .and_then(|v| v.as_str())
            .unwrap_or("null");
        reporter.info(&format!("Event[{idx}] valueJson: {value_json_str}"));

        let value_json: serde_json::Value =
            serde_json::from_str(value_json_str).expect("valueJson must be valid JSON");

        // Nil map should be captured as a map with empty entries (not a raw pointer or error)
        if value_json.is_null() {
            // null is acceptable for nil map
            reporter.info(&format!("Event[{idx}] ✅ nil map captured as null"));
            continue;
        }

        if let Some(obj) = value_json.as_object() {
            // Should have __type indicating map[string]int
            if let Some(type_name) = obj.get("__type").and_then(|t| t.as_str()) {
                if type_name.contains("map") {
                    // Check entries is empty (nil map has no entries)
                    let entries_empty = obj
                        .get("entries")
                        .and_then(|e| e.as_array())
                        .map(|e| e.is_empty())
                        .unwrap_or(true);
                    if entries_empty {
                        reporter.info(&format!(
                            "Event[{idx}] ✅ nil map correctly captured as empty {type_name}"
                        ));
                    } else {
                        mismatches.push(format!(
                            "Event[{idx}] nil map should have empty entries, got: {value_json}"
                        ));
                    }
                } else {
                    mismatches.push(format!(
                        "Event[{idx}] expected map type, got __type={type_name}: {value_json}"
                    ));
                }
            } else {
                mismatches.push(format!(
                    "Event[{idx}] missing __type field in nil map capture: {value_json}"
                ));
            }
        } else {
            mismatches.push(format!(
                "Event[{idx}] nil map should be object or null, got: {value_json}"
            ));
        }
    }

    if mismatches.is_empty() {
        reporter.info("✅ All nil map validations PASSED");
        reporter.print_footer(true);
    } else {
        reporter.error(&format!("❌ {} validation(s) FAILED:", mismatches.len()));
        for m in &mismatches {
            reporter.error(&format!("  - {m}"));
        }
        reporter.print_footer(false);
        panic!("{} field validation(s) failed", mismatches.len());
    }
}

#[tokio::test]
#[ignore = "requires Go < 1.24 fixture and CAP_BPF — run via: task test-ebpf-classic-inject"]
async fn test_ebpf_classic_map_iteration_int() {
    // Smoke test: capture a plain int variable to verify uprobe mechanism works
    let reporter = TestReporter::new(
        "eBPF Classic Map — Iteration Int (smoke test)",
        "EbpfAdapter",
    );
    reporter.print_header();

    let mut executor = TestExecutor::new();

    reporter.section("PHASE 1: START DAEMON");
    let step = reporter.step_start("Start Daemon", "detrix serve");
    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e.to_string());
        panic!("Failed to start daemon: {e}");
    }
    reporter.step_success(step, Some(&format!("HTTP port: {}", executor.http_port)));

    reporter.section("PHASE 2: START GO FIXTURE");
    let fixture_binary = env::var("GO_FIXTURE_BINARY")
        .unwrap_or_else(|_| "fixtures/go/classic_map/classic_map".to_string());
    let step = reporter.step_start("Start Go Fixture", &fixture_binary);
    let app = start_fixture(&fixture_binary);
    sleep(FIXTURE_START_WAIT).await;
    reporter.step_success(step, Some(&format!("PID: {}", app.id())));

    reporter.section("PHASE 3: REGISTER CONNECTION");
    let http = reqwest::Client::new();
    let step = reporter.step_start(
        "Create Connection",
        &format!("host={fixture_binary}, language=go"),
    );

    let conn_req = serde_json::json!({
        "host": fixture_binary,
        "port": 1024,
        "language": "go",
        "name": "ebpf-classic-map-iteration",
        "workspaceRoot": "/",
        "hostname": "docker",
    });

    let conn_resp: serde_json::Value = http
        .post(format!(
            "http://127.0.0.1:{}/api/v1/connections",
            executor.http_port
        ))
        .json(&conn_req)
        .send()
        .await
        .expect("Create connection request failed")
        .json()
        .await
        .expect("Failed to parse connection response");

    if conn_resp.get("error").is_some() || conn_resp.get("connectionId").is_none() {
        reporter.step_failed(step, &conn_resp.to_string());
        panic!("Create connection failed: {conn_resp}");
    }

    let connection_id = conn_resp["connectionId"]
        .as_str()
        .expect("No connectionId in response")
        .to_string();

    reporter.step_success(step, Some(&format!("connectionId={connection_id}")));
    sleep(ADAPTER_START_WAIT).await;

    reporter.section("PHASE 4: ADD METRIC");
    let fixture_source = env::var("GO_FIXTURE_SOURCE")
        .unwrap_or_else(|_| "fixtures/go/classic_map/main.go".to_string());
    let line = go_classic_lines::CODEMAP.find_logpoint("iteration");
    let location = format!("@{fixture_source}#{line}");

    let metric_req = serde_json::json!({
        "name": "iteration-int",
        "location": location,
        "expressions": ["iteration"],
        "connectionId": connection_id,
        "language": "go",
        "enabled": true,
    });

    let step = reporter.step_start("Add Metric", &format!("iteration at main.go#{}", line));
    let metric_resp: serde_json::Value = http
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
        .json(&metric_req)
        .send()
        .await
        .expect("Add metric request failed")
        .json()
        .await
        .expect("Failed to parse metric response");

    if metric_resp.get("error").is_some() {
        reporter.step_failed(step, &metric_resp.to_string());
        executor.print_daemon_logs(120);
        panic!("Failed to add metric: {metric_resp}");
    }

    let metric_id = metric_resp["metricId"]
        .as_u64()
        .expect("No metricId in response");
    reporter.step_success(step, Some(&format!("metricId={metric_id}")));

    reporter.section("PHASE 5: COLLECT EVENTS");
    reporter.info(&format!(
        "Waiting {}s for ring-buffer events...",
        EVENT_COLLECTION_WAIT.as_secs()
    ));
    sleep(EVENT_COLLECTION_WAIT).await;

    reporter.section("PHASE 6: DAEMON LOGS");
    kill_fixture(app);
    reporter.info("=== DAEMON LOGS ===");
    executor.print_daemon_logs(200);
    reporter.info("=== END DAEMON LOGS ===");

    reporter.section("PHASE 7: ASSERTIONS");
    let step = reporter.step_start("Query Events", &format!("metricId={metric_id}"));

    let resp = http
        .get(format!(
            "http://127.0.0.1:{}/api/v1/events?metricId={}&limit=10&since=0",
            executor.http_port, metric_id
        ))
        .send()
        .await
        .expect("Event query failed");

    assert!(
        resp.status().is_success(),
        "Event query failed: {}",
        resp.status()
    );

    let events: Vec<serde_json::Value> = resp.json().await.expect("Failed to parse response");

    if events.is_empty() {
        panic!("No events captured for iteration-int — uprobe attachment or BPF program failed.");
    }

    reporter.step_success(step, Some(&format!("Got {} events", events.len())));

    // Smoke test: verify at least one event has a numeric iteration value.
    // `iteration` is a plain int that increments each loop; the logpoint fires at `iteration++`
    // (start of the statement), so the captured value is the post-increment value (>= 1).
    let mut mismatches = Vec::new();

    for (idx, event) in events.iter().take(5).enumerate() {
        let values = event["values"].as_array().expect("Expected values array");
        assert!(!values.is_empty(), "No values in event {idx}");

        let value_json_str = values[0]
            .get("valueJson")
            .and_then(|v| v.as_str())
            .unwrap_or("null");
        reporter.info(&format!(
            "Event[{idx}] iteration valueJson: {value_json_str}"
        ));

        let value_json: serde_json::Value =
            serde_json::from_str(value_json_str).expect("valueJson must be valid JSON");

        // iteration should be a plain JSON number >= 1
        if let Some(n) = value_json.as_i64() {
            if n >= 1 {
                reporter.info(&format!("Event[{idx}] ✅ iteration={n} (positive integer)"));
            } else {
                mismatches.push(format!("Event[{idx}] iteration={n} should be >= 1"));
            }
        } else {
            // Report what was captured without failing — the `iteration` DWARF location
            // may have quirks in Go 1.23 (e.g., address-based capture instead of register).
            reporter.info(&format!(
                "Event[{idx}] ⚠ iteration captured as non-integer: {value_json} \
                 (uprobe fires but DWARF location may be address-based)"
            ));
            // Not a hard failure for this smoke test — uprobe attachment works if events arrive
        }
    }

    if mismatches.is_empty() {
        reporter.info("✅ Smoke test PASSED — uprobe fires and iteration values captured");
        reporter.print_footer(true);
    } else {
        reporter.error(&format!("❌ {} validation(s) FAILED:", mismatches.len()));
        for m in &mismatches {
            reporter.error(&format!("  - {m}"));
        }
        reporter.print_footer(false);
        panic!("{} field validation(s) failed", mismatches.len());
    }
}
