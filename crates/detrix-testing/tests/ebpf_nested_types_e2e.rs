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

// ── Helper Functions ─────────────────────────────────────────────────────────

/// Pretty-print a captured event with formatted JSON values
/// Removes escape noise and shows type information clearly
fn pretty_print_event(event: &serde_json::Value) -> String {
    let mut output = String::new();

    // Print event metadata
    if let Some(metric_name) = event["metricName"].as_str() {
        output.push_str(&format!("  Metric: {}\n", metric_name));
    }
    if let Some(thread_id) = event["threadId"].as_i64() {
        output.push_str(&format!("  Thread: {}\n", thread_id));
    }

    // Print captured values
    if let Some(values) = event["values"].as_array() {
        for val in values {
            if let Some(expr) = val["expression"].as_str() {
                output.push_str(&format!("\n  [{}]\n", expr));

                // Pretty-print valueJson
                if let Some(value_json_str) = val["valueJson"].as_str() {
                    // Parse the nested JSON
                    if let Ok(value_json) =
                        serde_json::from_str::<serde_json::Value>(value_json_str)
                    {
                        let pretty = pretty_print_value(&value_json, 1);
                        // Indent the pretty-printed JSON
                        for line in pretty.lines() {
                            output.push_str(&format!("    {}\n", line));
                        }
                    } else {
                        output.push_str(&format!("    {}\n", value_json_str));
                    }
                }

                // Print typed value if available
                if !val["typedValue"].is_null() {
                    output.push_str(&format!("  Typed: {}\n", val["typedValue"]));
                }
            }
        }
    }

    output
}

/// Recursively pretty-print a JSON value with smart formatting
/// - Simple arrays (scalars only) are printed inline
/// - Complex types (structs, nested arrays) are pretty-printed
/// - time.Time is converted to human-readable format
/// - Go map internals are annotated
fn pretty_print_value(value: &serde_json::Value, indent_level: usize) -> String {
    let indent = "  ".repeat(indent_level);
    let next_indent = "  ".repeat(indent_level + 1);

    match value {
        serde_json::Value::Object(obj) => {
            // Check for special types
            if let Some(type_name) = obj.get("__type").and_then(|v| v.as_str()) {
                // Handle time.Time specially
                if type_name == "time.Time" {
                    let wall_opt = obj.get("wall");
                    let ext_opt = obj.get("ext").and_then(|v| v.as_i64());

                    if let Some(ext) = ext_opt {
                        // Convert Unix timestamp to human-readable format
                        let secs = ext;
                        let nanos = wall_opt.and_then(|v| v.as_i64()).unwrap_or(0);

                        // Format as RFC3339-like string
                        let datetime = format_timestamp(secs, nanos);
                        let wall_str = wall_opt
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "0".to_string());
                        return format!(
                            "{{\n{}  \"__type\": \"{}\",\n{}  \"wall\": {},\n{}  \"ext\": {},\n{}  \"str_value\": \"{}\"\n{}}}",
                            indent, type_name,
                            indent, wall_str,
                            indent, ext,
                            indent, datetime,
                            indent
                        );
                    }
                }

                // Annotate Go map internals (runtime.hmap bucket directory)
                if type_name.contains("table<") || type_name.contains("hmap") {
                    // This is Go's internal map runtime structure - will be annotated below
                }
            }

            // Check if this is a simple object (few fields, no nested objects)
            let is_simple = obj.len() <= 3
                && obj
                    .values()
                    .all(|v| v.is_number() || v.is_string() || v.is_boolean());

            if is_simple {
                // Print simple objects inline
                let pairs: Vec<String> = obj
                    .iter()
                    .map(|(k, v)| format!("\"{}\": {}", k, format_simple_value(v)))
                    .collect();
                return format!("{{ {} }}", pairs.join(", "));
            }

            // Pretty-print complex objects
            let fields: Vec<String> = obj
                .iter()
                .map(|(k, v)| {
                    // Special handling for Go map internals (runtime.hmap bucket directory)
                    if k == "dirPtr" || k == "buckets" {
                        format!(
                            "{}\"{}\": {} // Go map internal: runtime.hmap bucket directory pointer",
                            next_indent, k, pretty_print_value(v, indent_level + 1)
                        )
                    } else {
                        format!(
                            "{}\"{}\": {}",
                            next_indent, k, pretty_print_value(v, indent_level + 1)
                        )
                    }
                })
                .collect();
            format!("{{\n{}\n{}}}", fields.join(",\n"), indent)
        }

        serde_json::Value::Array(arr) => {
            // Check if array contains only simple scalars
            let is_simple = arr
                .iter()
                .all(|v| v.is_number() || v.is_string() || v.is_boolean());

            if is_simple && arr.len() <= 10 {
                // Print simple arrays inline
                let items: Vec<String> = arr.iter().map(|v| format_simple_value(v)).collect();
                return format!("[{}]", items.join(", "));
            }

            // Pretty-print complex arrays
            let items: Vec<String> = arr
                .iter()
                .map(|v| format!("{}{}", next_indent, pretty_print_value(v, indent_level + 1)))
                .collect();
            format!("[\n{}\n{}]", items.join(",\n"), indent)
        }

        _ => format_simple_value(value),
    }
}

/// Format a simple JSON value (number, string, boolean, null)
fn format_simple_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("\"{}\"", s),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        _ => value.to_string(),
    }
}

/// Convert Unix timestamp to human-readable datetime string
fn format_timestamp(secs: i64, _nanos: i64) -> String {
    // Handle zero/invalid timestamps
    if secs == 0 {
        return "1970-01-01 00:00:00".to_string();
    }

    // Go's time.Time stores ext as seconds since year 1 for some representations
    // Unix epoch (1970-01-01) is approximately 62135596800 seconds after year 1
    // If ext is very large (> 1 billion), it's likely seconds since year 1
    let unix_secs = if secs > 1_000_000_000 {
        // Convert from seconds since year 1 to Unix timestamp
        // Year 1 to 1970 is approximately 62135596800 seconds
        secs - 62_135_596_800
    } else {
        // Already a Unix timestamp
        secs
    };

    // Handle negative or very old timestamps
    if unix_secs < 0 {
        return format!("{} (year < 1970)", secs);
    }

    // Simple conversion without external dependencies
    let days_since_epoch = unix_secs / 86400;
    let remaining_secs = unix_secs % 86400;
    let hours = remaining_secs / 3600;
    let minutes = (remaining_secs % 3600) / 60;
    let seconds = remaining_secs % 60;

    // Approximate date calculation (ignoring leap years for simplicity)
    let mut year = 1970;
    let mut days = days_since_epoch;
    while days >= 365 {
        let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days_in_year = if is_leap { 366 } else { 365 };
        if days >= days_in_year {
            days -= days_in_year;
            year += 1;
        } else {
            break;
        }
    }

    // Month calculation
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mut month = 1;
    let mut day = days + 1;
    for (i, &days_in_month) in month_days.iter().enumerate() {
        let actual_days = if i == 1 && is_leap { 29 } else { days_in_month };
        if day > actual_days {
            day -= actual_days;
            month += 1;
        } else {
            break;
        }
    }

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hours, minutes, seconds
    )
}

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
    let step = reporter.step_start(
        "Start Daemon",
        "detrix serve (EbpfGoFactory active on Linux)",
    );

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

    let fixture_binary =
        env::var("GO_FIXTURE_BINARY").unwrap_or_else(|_| "/usr/local/bin/nested_types".to_string());
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
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
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
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
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
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
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

    // Debug: Print first Order event with pretty-printed values
    if let Some(first_order) = order_events.first() {
        reporter.info("Sample Order event:");
        reporter.info(&pretty_print_event(first_order));
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
    reporter.info(&format!(
        "Received {} PriceHistory events",
        history_events.len()
    ));

    // Debug: Print first PriceHistory event with pretty-printed values
    if let Some(first_history) = history_events.first() {
        reporter.info("Sample PriceHistory event (array capture):");
        reporter.info(&pretty_print_event(first_history));
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

    // Debug: Print first OrderPtr event with pretty-printed values
    if let Some(first_ptr) = ptr_events.first() {
        reporter.info("Sample OrderPtr event (pointer capture):");
        reporter.info(&pretty_print_event(first_ptr));
    }

    // ── PHASE 7: Comprehensive Field Validation ──────────────────────────────
    reporter.section("PHASE 7: COMPREHENSIVE FIELD VALIDATION");

    let mut mismatches = Vec::new();

    // Expected values from deterministic fixture (iteration 1)
    // ID=1, Product.Name="Laptop", Trader.Name="Alice", Status="PENDING"
    // Price=50.25, Total=100.5, Country="USA"
    // Prices array: [100.5, 101.2, 99.8, 102.1, 100.0], Avg=100.72
    // OrderPtr.Count=1, Order.ID=1

    // Validate Order events
    for (idx, event) in order_events.iter().take(3).enumerate() {
        let values = event["values"]
            .as_array()
            .expect("No values in Order event");
        if let Some(val) = values.first() {
            let value_json = val["valueJson"].as_str().unwrap_or("");

            // Check ID field - should be numeric
            if !value_json.contains("\"ID\":") {
                mismatches.push(format!("Order[{}] missing ID field", idx));
            }

            // Check Product.SKU - deterministic format SKU-XXXX
            if !value_json.contains("\"SKU\":\"SKU-") {
                mismatches.push(format!(
                    "Order[{}] Product.SKU should be SKU-XXXX format",
                    idx
                ));
            }

            // Check Product.Name - should be one of the deterministic names
            let valid_product_names = [
                "Laptop",
                "Phone",
                "Tablet",
                "Monitor",
                "Keyboard",
                "Mouse",
                "Headphones",
                "Camera",
            ];
            let has_valid_name = valid_product_names
                .iter()
                .any(|name| value_json.contains(&format!("\"Name\":\"{}\"", name)));
            if !has_valid_name {
                mismatches.push(format!(
                    "Order[{}] Product.Name should be one of {:?}",
                    idx, valid_product_names
                ));
            }

            // Check Product.Price (float) - should be a numeric value, not a struct
            if !value_json.contains("\"Price\":") {
                mismatches.push(format!("Order[{}] missing Product.Price", idx));
            }
            // Price should be numeric, not a struct
            if value_json.contains("\"Price\":{\"") {
                mismatches.push(format!(
                    "Order[{}] Product.Price should be numeric, not struct",
                    idx
                ));
            }

            // Check Product.Category.Name - should be one of the deterministic names
            let valid_category_names = [
                "Electronics",
                "Accessories",
                "Computing",
                "Audio",
                "Photography",
            ];
            let has_valid_category = valid_category_names
                .iter()
                .any(|name| value_json.contains(&format!("\"Name\":\"{}\"", name)));
            if !has_valid_category {
                mismatches.push(format!(
                    "Order[{}] Category.Name should be one of {:?}",
                    idx, valid_category_names
                ));
            }

            // Check Trader.Name - should be one of the deterministic names
            let valid_trader_names = [
                "Alice", "Bob", "Charlie", "Diana", "Eve", "Frank", "Grace", "Henry",
            ];
            let has_valid_trader = valid_trader_names
                .iter()
                .any(|name| value_json.contains(&format!("\"Name\":\"{}\"", name)));
            if !has_valid_trader {
                mismatches.push(format!(
                    "Order[{}] Trader.Name should be one of {:?}",
                    idx, valid_trader_names
                ));
            }

            // Check Address fields
            if !value_json.contains("\"Street\":") {
                mismatches.push(format!("Order[{}] missing Address.Street", idx));
            }
            if !value_json.contains("\"City\":") {
                mismatches.push(format!("Order[{}] missing Address.City", idx));
            }
            if !value_json.contains("\"Country\":\"USA\"") {
                mismatches.push(format!("Order[{}] missing Address.Country:USA", idx));
            }
            if !value_json.contains("\"Zip\":") {
                mismatches.push(format!("Order[{}] missing Address.Zip", idx));
            }

            // Check Items slice - should have len and cap
            // Note: Full element capture requires depth-2 pointer following (not yet implemented)
            if !value_json.contains("\"Items\"") {
                mismatches.push(format!("Order[{}] missing Items slice", idx));
            }

            // Check Tags map (should be captured with Swiss Table iterator)
            if !value_json.contains("\"Tags\":") {
                mismatches.push(format!("Order[{}] missing Tags", idx));
            }
            // Tags must NOT show raw runtime internals (dirPtr/seed/used) — that indicates
            // the map iterator was not applied and the map was parsed as a plain struct.
            if value_json.contains("\"dirPtr\"") {
                mismatches.push(format!("Order[{}] Tags shows raw runtime struct (map iterator not applied)", idx));
            }

            // Check Total (float) - should be a numeric value
            if !value_json.contains("\"Total\":") {
                mismatches.push(format!("Order[{}] missing Total", idx));
            }
            // Total should be numeric, not a struct
            if value_json.contains("\"Total\":{\"") {
                mismatches.push(format!(
                    "Order[{}] Total should be numeric, not struct",
                    idx
                ));
            }

            // Check Timestamp - should have wall and ext fields (time.Time struct)
            if !value_json.contains("\"Timestamp\":") {
                mismatches.push(format!("Order[{}] missing Timestamp", idx));
            }
            // Timestamp should be a struct with wall/ext, not a slice
            if value_json.contains("\"Timestamp\":{\"len\":") {
                mismatches.push(format!(
                    "Order[{}] Timestamp should be time.Time struct, not slice",
                    idx
                ));
            }
            // Timestamp should have str_value with human-readable date (1970s for our fixture)
            if !value_json.contains("\"str_value\":") {
                mismatches.push(format!(
                    "Order[{}] Timestamp should have str_value field with human-readable date",
                    idx
                ));
            }
            // Check that str_value contains a reasonable date (2026 for our fixture)
            // Fixture uses: time.Unix(1775102400+iteration*1000, 0) = 2026-04-02 19:20:00 + offset
            if !value_json.contains("\"str_value\":\"2026-") {
                mismatches.push(format!(
                    "Order[{}] Timestamp str_value should be 2026 date, got: {}",
                    idx, value_json
                ));
            }

            // Check Status (OrderStatus string alias - should be a string value, not a struct)
            if !value_json.contains("\"Status\":") {
                mismatches.push(format!("Order[{}] missing Status", idx));
            }
            // Status should be a string value like "PENDING", "DELIVERED", etc.
            let valid_statuses = ["PENDING", "CONFIRMED", "SHIPPED", "DELIVERED"];
            let has_valid_status = valid_statuses
                .iter()
                .any(|status| value_json.contains(&format!("\"Status\":\"{}\"", status)));
            if !has_valid_status {
                mismatches.push(format!(
                    "Order[{}] Status should be one of {:?}, got: {}",
                    idx, valid_statuses, value_json
                ));
            }
            // Status should NOT be a struct with str/len fields
            if value_json.contains("\"Status\":{\"__type\":\"main.OrderStatus\",\"str\":") {
                mismatches.push(format!(
                    "Order[{}] Status should be string value, not struct with str/len",
                    idx
                ));
            }
        }
    }

    // Validate PriceHistory events
    for (idx, event) in history_events.iter().take(3).enumerate() {
        let values = event["values"]
            .as_array()
            .expect("No values in PriceHistory event");
        if let Some(val) = values.first() {
            let value_json = val["valueJson"].as_str().unwrap_or("");

            // Check Prices array with elements
            if !value_json.contains("\"Prices\":") {
                mismatches.push(format!("PriceHistory[{}] missing Prices", idx));
            }
            if !value_json.contains("\"elements\":[") {
                mismatches.push(format!(
                    "PriceHistory[{}] missing Prices.elements array",
                    idx
                ));
            }
            // Check for exact expected float values [100.5, 101.2, 99.8, 102.1, 100]
            if !value_json.contains("100.5") {
                mismatches.push(format!("PriceHistory[{}] missing 100.5 in Prices", idx));
            }
            if !value_json.contains("101.2") {
                mismatches.push(format!("PriceHistory[{}] missing 101.2 in Prices", idx));
            }
            if !value_json.contains("99.8") {
                mismatches.push(format!("PriceHistory[{}] missing 99.8 in Prices", idx));
            }
            if !value_json.contains("102.1") {
                mismatches.push(format!("PriceHistory[{}] missing 102.1 in Prices", idx));
            }

            // Check Avg (float) - exact value
            if !value_json.contains("\"Avg\":100.72") {
                mismatches.push(format!("PriceHistory[{}] missing Avg:100.72", idx));
            }
        }
    }

    // Validate OrderPtr events - pointer dereferencing test
    for (idx, event) in ptr_events.iter().take(3).enumerate() {
        let values = event["values"]
            .as_array()
            .expect("No values in OrderPtr event");
        if let Some(val) = values.first() {
            let value_json = val["valueJson"].as_str().unwrap_or("");
            let expression = val["expression"].as_str().unwrap_or("");

            // Check that we're capturing the ptrWrapper expression
            if expression != "ptrWrapper" {
                mismatches.push(format!(
                    "OrderPtr[{}] expected expression 'ptrWrapper', got '{}'",
                    idx, expression
                ));
            }

            // Check Order pointer was dereferenced (should capture full nested struct)
            if !value_json.contains("\"Order\":") {
                mismatches.push(format!("OrderPtr[{}] missing Order field", idx));
            }

            // Check Order.ID - should be numeric
            if !value_json.contains("\"ID\":") {
                mismatches.push(format!("OrderPtr[{}] missing Order.ID", idx));
            }

            // Check Order.Product
            if !value_json.contains("\"Product\":") {
                mismatches.push(format!("OrderPtr[{}] missing Order.Product", idx));
            }

            // Check Order.Trader
            if !value_json.contains("\"Trader\":") {
                mismatches.push(format!("OrderPtr[{}] missing Order.Trader", idx));
            }

            // Check Order.Items - should now have full elements (not just len/cap header)
            if !value_json.contains("\"Items\":") {
                mismatches.push(format!("OrderPtr[{}] missing Order.Items", idx));
            }
            if !value_json.contains("\"elements\":") {
                mismatches.push(format!("OrderPtr[{}] Order.Items missing elements (only has header)", idx));
            }

            // Check Count field - should be numeric (iteration number)
            if !value_json.contains("\"Count\":") {
                mismatches.push(format!("OrderPtr[{}] missing Count", idx));
            }

            // Validate that ptrWrapper is NOT captured as raw bytes/string
            // It should be a struct with nested fields, not a string value.
            // Note: value_json.contains("<read-fai") is intentionally NOT checked here
            // because groups:"<read-failed>" is expected for Go map internals.
            if value_json.starts_with("\"\\t") {
                mismatches.push(format!(
                    "OrderPtr[{}] captured as raw bytes instead of dereferenced struct",
                    idx
                ));
            }

            // Validate OrderPtr is a proper struct, not raw pointer bytes
            if !value_json.contains("\"__type\":\"main.OrderPtr\"") {
                mismatches.push(format!(
                    "OrderPtr[{}] should have __type main.OrderPtr",
                    idx
                ));
            }
        }
    }

    // Report all mismatches
    if mismatches.is_empty() {
        reporter.info("✅ All field validations PASSED");
    } else {
        reporter.error(&format!(
            "❌ {} field validation(s) FAILED:",
            mismatches.len()
        ));
        for mismatch in &mismatches {
            reporter.error(&format!("  - {}", mismatch));
        }
    }

    // Final summary
    if mismatches.is_empty() {
        reporter.info(
            "eBPF nested types test PASSED — structs, arrays, and pointers captured via ring buffer",
        );
    } else {
        panic!("{} field validation(s) failed", mismatches.len());
    }
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
