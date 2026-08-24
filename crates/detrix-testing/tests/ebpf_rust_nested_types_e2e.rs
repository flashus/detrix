//! Comprehensive Rust eBPF aggregate capture test.
//!
//! This is the Rust counterpart to `ebpf_nested_types_e2e`: one snapshot
//! contains nested structs, fixed arrays, Vecs, borrowed slices, strings,
//! HashMaps, Box/Option-like indirection, and an enum nested in a struct.
//!
//! Run in the privileged playground with:
//! `task tests:test-ebpf-rust-nested-inject`

#![cfg(target_os = "linux")]

use detrix_testing::e2e::executor::TestExecutor;
use serde_json::Value;
use std::env;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

const SOURCE: &str = "/src/fixtures/rust/src/nested_types.rs";
const OBSERVE_LINE: u32 = 95;

fn fixture_binary() -> String {
    env::var("RUST_NESTED_FIXTURE_BINARY")
        .unwrap_or_else(|_| "/usr/local/bin/rust_nested_types_app".to_string())
}

fn start_fixture(binary: &str) -> Child {
    Command::new(binary)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|error| panic!("failed to start Rust nested fixture {binary}: {error}"))
}

async fn wait_for_events(http: &reqwest::Client, port: u16, metric_id: u64) -> Vec<Value> {
    let url =
        format!("http://127.0.0.1:{port}/api/v1/events?metricId={metric_id}&limit=20&since=0");
    for _ in 0..40 {
        if let Ok(response) = http.get(&url).send().await {
            if let Ok(events) = response.json::<Vec<Value>>().await {
                if !events.is_empty() {
                    return events;
                }
            }
        }
        sleep(Duration::from_millis(500)).await;
    }
    Vec::new()
}

fn event_value(event: &Value, name: &str) -> Option<Value> {
    event
        .get("values")
        .and_then(Value::as_array)?
        .iter()
        .find(|value| value.get("expression").and_then(Value::as_str) == Some(name))
        .and_then(|value| value.get("valueJson"))
        .and_then(Value::as_str)
        .and_then(|value| serde_json::from_str(value).ok())
}

/// Render an event in the same shape as the Go nested-types test.
///
/// `valueJson` is itself a JSON-encoded string in the events API, so decode it
/// before printing. This keeps the output useful for inspecting every nested
/// field instead of showing escaped JSON on one line.
fn pretty_print_event(event: &Value) -> String {
    let mut output = String::new();

    if let Some(metric_name) = event.get("metricName").and_then(Value::as_str) {
        output.push_str(&format!("  Metric: {metric_name}\n"));
    }
    if let Some(thread_id) = event.get("threadId").and_then(Value::as_i64) {
        output.push_str(&format!("  Thread: {thread_id}\n"));
    }
    if let Some(timestamp) = event.get("timestamp") {
        output.push_str(&format!("  Timestamp: {timestamp}\n"));
    }

    if let Some(values) = event.get("values").and_then(Value::as_array) {
        for captured in values {
            let expression = captured
                .get("expression")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            output.push_str(&format!("\n  [{expression}]\n"));

            match captured.get("valueJson").and_then(Value::as_str) {
                Some(raw) => match serde_json::from_str::<Value>(raw) {
                    Ok(value) => match serde_json::to_string_pretty(&value) {
                        Ok(pretty) => {
                            for line in pretty.lines() {
                                output.push_str("    ");
                                output.push_str(line);
                                output.push('\n');
                            }
                        }
                        Err(_) => output.push_str(&format!("    {raw}\n")),
                    },
                    Err(_) => output.push_str(&format!("    {raw}\n")),
                },
                None => output.push_str("    <missing valueJson>\n"),
            }
        }
    }

    output
}

fn contains_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key(key) || object.values().any(|value| contains_key(value, key))
        }
        Value::Array(values) => values.iter().any(|value| contains_key(value, key)),
        _ => false,
    }
}

fn object_field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .as_object()
        .and_then(|object| object.get(key))
        .unwrap_or_else(|| panic!("missing field `{key}` in {value}"))
}

fn type_name(value: &Value) -> &str {
    object_field(value, "__type")
        .as_str()
        .unwrap_or_else(|| panic!("`__type` is not a string: {value}"))
}

fn elements(value: &Value) -> &Vec<Value> {
    object_field(value, "elements")
        .as_array()
        .or_else(|| {
            object_field(value, "elements")
                .as_object()
                .and_then(|object| object.get("elements"))
                .and_then(Value::as_array)
        })
        .unwrap_or_else(|| panic!("missing elements array in {value}"))
}

fn map_entries(value: &Value) -> &Vec<Value> {
    object_field(value, "entries")
        .as_array()
        .unwrap_or_else(|| panic!("missing map entries in {value}"))
}

fn assert_clean_capture(value: &Value) {
    match value {
        Value::String(text) => assert!(
            !text.contains("<read-") && !text.contains("<error:"),
            "capture contains read error: {text}"
        ),
        Value::Object(object) => {
            if let Some(type_name) = object.get("__type").and_then(Value::as_str) {
                assert!(
                    !type_name.contains("unknown"),
                    "unknown captured type: {value}"
                );
            }
            for child in object.values() {
                assert_clean_capture(child);
            }
        }
        Value::Array(values) => values.iter().for_each(assert_clean_capture),
        _ => {}
    }
}

fn assert_tag_vec(value: &Value) {
    assert_eq!(type_name(value), "[]Tag");
    assert_eq!(object_field(value, "len"), &Value::from(2));
    assert_eq!(object_field(value, "cap"), &Value::from(2));
    let tags = elements(value);
    assert_eq!(tags.len(), 2);
    assert_eq!(
        tags.iter()
            .map(|tag| {
                (
                    object_field(tag, "key").as_str().unwrap().to_owned(),
                    object_field(tag, "value").as_str().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            ("kind".into(), "instrument".into()),
            ("source".into(), "fixture".into())
        ]
    );
}

fn assert_line_item(value: &Value, sku: &str, quantity: u64, first_price: f64) {
    assert_eq!(type_name(value), "LineItem");
    assert_eq!(object_field(value, "sku"), sku);
    assert_eq!(object_field(value, "quantity"), &Value::from(quantity));
    let prices = object_field(value, "prices");
    assert_eq!(type_name(prices), "[4]f64");
    assert_eq!(
        elements(prices),
        &vec![
            Value::from(first_price),
            Value::from(101.2),
            Value::from(99.8),
            Value::from(102.1)
        ]
    );
    assert_tag_vec(object_field(value, "tags"));
}

fn assert_order(value: &Value, id: u64, state_variant: &str, state_discriminant: u64) {
    assert_eq!(type_name(value), "Order");
    assert_eq!(object_field(value, "id"), &Value::from(id));
    assert_eq!(object_field(value, "customer"), "detrix");
    assert_eq!(object_field(value, "note"), "BTCUSD");

    assert_line_item(
        object_field(value, "primary"),
        &format!("SKU-{}", id - 1000),
        (id - 1000) % 9 + 1,
        100.5 + (id - 1000) as f64,
    );
    let lines = object_field(value, "lines");
    assert_eq!(type_name(lines), "[]LineItem");
    assert_eq!(object_field(lines, "len"), &Value::from(2));
    assert_eq!(object_field(lines, "cap"), &Value::from(2));
    let line_values = elements(lines);
    assert_eq!(line_values.len(), 2);
    for (index, line) in line_values.iter().enumerate() {
        let seed = id - 1000 + index as u64 + 1;
        assert_line_item(
            line,
            &format!("SKU-{seed}"),
            seed % 9 + 1,
            100.5 + seed as f64,
        );
    }

    let line_slice = object_field(value, "line_slice");
    assert_eq!(type_name(line_slice), "[]u64");
    assert_eq!(object_field(line_slice, "len"), &Value::from(3));
    assert_eq!(object_field(line_slice, "cap"), &Value::from(3));
    assert_eq!(
        elements(line_slice),
        &vec![Value::from(7), Value::from(11), Value::from(13)]
    );

    let tags = object_field(value, "tags");
    assert_eq!(type_name(tags), "map[String]Tag");
    let mut tag_pairs = map_entries(tags)
        .iter()
        .map(|entry| {
            let key = object_field(entry, "key").as_str().unwrap().to_owned();
            let tag = object_field(entry, "value");
            assert_eq!(object_field(tag, "key").as_str(), Some(key.as_str()));
            (key, object_field(tag, "value").as_str().unwrap().to_owned())
        })
        .collect::<Vec<_>>();
    tag_pairs.sort();
    assert_eq!(
        tag_pairs,
        vec![
            ("desk".into(), "alpha".into()),
            ("risk".into(), "low".into())
        ]
    );

    let state = object_field(value, "state");
    assert_eq!(type_name(state), "OrderState");
    assert_eq!(object_field(state, "variant"), state_variant);
    assert_eq!(
        object_field(state, "discriminant"),
        &Value::from(state_discriminant)
    );
}

#[tokio::test]
#[ignore = "requires CAP_BPF and Linux kernel >= 5.8 — run via task tests:test-ebpf-rust-nested-inject"]
async fn test_ebpf_captures_rust_nested_types() {
    let mut executor = TestExecutor::new();
    executor
        .start_daemon()
        .await
        .expect("failed to start Detrix daemon");

    let binary = fixture_binary();
    let mut fixture = start_fixture(&binary);
    sleep(Duration::from_secs(2)).await;

    let http = reqwest::Client::new();
    let connection: Value = http
        .post(format!(
            "http://127.0.0.1:{}/api/v1/connections",
            executor.http_port
        ))
        .json(&serde_json::json!({
            "host": binary,
            "port": 1024,
            "language": "rust",
            "name": "ebpf-rust-nested",
            "workspaceRoot": "/",
            "hostname": "docker"
        }))
        .send()
        .await
        .expect("connection request failed")
        .json()
        .await
        .expect("invalid connection response");
    let connection_id = connection["connectionId"]
        .as_str()
        .expect("Rust connection was not created");

    // `snapshot` is an aggregate value at the black_box observation point.
    let metric_response = http
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
        .json(&serde_json::json!({
            "name": "rust_nested_snapshot",
            "location": format!("@{SOURCE}#{OBSERVE_LINE}"),
            "expressions": ["snapshot"],
            "connectionId": connection_id,
            "language": "rust",
            "enabled": true
        }))
        .send()
        .await
        .expect("metric request failed");
    let metric_status = metric_response.status();
    let metric_body = metric_response
        .text()
        .await
        .expect("invalid metric response body");
    if !metric_status.is_success() {
        executor.print_daemon_logs(240);
    }
    assert!(
        metric_status.is_success(),
        "metric creation failed ({}): {}",
        metric_status,
        metric_body
    );
    let metric: Value = serde_json::from_str(&metric_body)
        .unwrap_or_else(|error| panic!("invalid metric JSON ({error}): {metric_body}"));
    let metric_id = metric["metricId"].as_u64().expect("metric was not created");

    let events = wait_for_events(&http, executor.http_port, metric_id).await;
    if events.is_empty() {
        executor.print_daemon_logs(240);
    }
    assert!(!events.is_empty(), "Rust nested eBPF produced no events");

    println!("\n==========================================================");
    println!("  Rust eBPF Nested Types E2E");
    println!("==========================================================\n");
    println!("Received {} rust_nested_snapshot events", events.len());
    println!("Sample Rust nested event (all captured values):");
    println!("{}", pretty_print_event(&events[0]));
    executor.print_daemon_logs(240);

    let snapshot = event_value(&events[0], "snapshot").expect("snapshot value missing");
    assert_clean_capture(&snapshot);
    assert!(
        contains_key(&snapshot, "order"),
        "nested order missing: {snapshot}"
    );
    assert!(
        contains_key(&snapshot, "primary"),
        "nested primary item missing: {snapshot}"
    );
    assert!(
        contains_key(&snapshot, "prices"),
        "fixed array missing: {snapshot}"
    );
    assert!(
        contains_key(&snapshot, "lines"),
        "Vec<LineItem> missing: {snapshot}"
    );
    assert!(
        contains_key(&snapshot, "line_slice"),
        "borrowed slice missing: {snapshot}"
    );
    assert!(
        contains_key(&snapshot, "tags"),
        "nested map/tags missing: {snapshot}"
    );
    assert!(
        contains_key(&snapshot, "state"),
        "nested enum missing: {snapshot}"
    );
    assert!(
        contains_key(&snapshot, "attributes"),
        "map field missing: {snapshot}"
    );

    // Exact-value/type checks: a successful event must contain decoded Rust
    // layouts, not raw pointer words, `unknown` element types, or read errors.
    let order = object_field(&snapshot, "order");
    assert_order(order, 1001, "Pending", 0);

    let order_ptr = object_field(&snapshot, "order_ptr");
    assert_order(order_ptr, 1001, "Pending", 0);

    let orders = object_field(&snapshot, "orders");
    assert_eq!(type_name(orders), "[]Order");
    assert_eq!(object_field(orders, "len"), &Value::from(2));
    assert_eq!(object_field(orders, "cap"), &Value::from(2));
    let order_elements = elements(orders);
    assert_eq!(order_elements.len(), 2);
    assert_order(&order_elements[0], 1001, "Pending", 0);
    assert_order(&order_elements[1], 1002, "Settled", 1);

    let labels = object_field(&snapshot, "labels");
    assert_eq!(type_name(labels), "[]String");
    assert_eq!(object_field(labels, "len"), &Value::from(3));
    assert_eq!(object_field(labels, "cap"), &Value::from(3));
    assert_eq!(
        elements(labels),
        &vec![Value::from("btc"), Value::from("usd"), Value::from("spot")]
    );

    let label_slice = object_field(&snapshot, "label_slice");
    assert!(type_name(label_slice).starts_with("[]"));
    assert_eq!(object_field(label_slice, "len"), &Value::from(3));
    assert_eq!(object_field(label_slice, "cap"), &Value::from(3));
    assert_eq!(
        elements(label_slice),
        &vec![Value::from("btc"), Value::from("usd"), Value::from("spot")]
    );

    let prices = object_field(&snapshot, "prices");
    assert_eq!(type_name(prices), "[5]f64");
    assert_eq!(
        elements(prices),
        &vec![
            Value::from(100.5),
            Value::from(101.2),
            Value::from(99.8),
            Value::from(102.1),
            Value::from(100)
        ]
    );

    let attributes = object_field(&snapshot, "attributes");
    let mut attributes_pairs = map_entries(attributes)
        .iter()
        .map(|entry| {
            (
                object_field(entry, "key").as_str().unwrap().to_owned(),
                object_field(entry, "value").as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    attributes_pairs.sort();
    assert_eq!(
        attributes_pairs,
        vec![
            ("account".into(), "paper".into()),
            ("venue".into(), "test".into())
        ]
    );

    let _ = fixture.kill();
    let _ = fixture.wait();
    executor.stop_daemon();
}
