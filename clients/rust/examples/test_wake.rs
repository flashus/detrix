//! Agent simulation test - wake a running Rust app and observe it.
//!
//! This script simulates what an AI agent typically does with Detrix:
//!  1. Starts a target application (detrix_example_app)
//!  2. Waits for it to initialize
//!  3. Wakes the Detrix client in the app via control plane
//!  4. Sets metrics on the running process via daemon API
//!  5. Receives and displays captured events
//!
//! Usage:
//!
//!  1. Make sure the Detrix daemon is running:
//!     $ detrix serve --daemon
//!
//!  2. Run this test (from clients/rust directory):
//!     $ cargo run --example test_wake
//!
//!     Or with custom daemon port:
//!     $ cargo run --example test_wake -- --daemon-port 9999

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use regex::Regex;
use reqwest::blocking::Client;
use serde_json::{json, Value};

// ============================================================================
// MAIN LINE: The line number of `fn main()` in the fixture
// Update this if you add/remove lines before main() in fixtures/rust/src/main.rs
// ============================================================================
const MAIN_LINE: i32 = 76;

// Metric line offsets from MAIN_LINE
// NOTE: LLDB evaluates expressions BEFORE the line executes, so we must
// observe variables on lines AFTER their assignment to see values.
// Fixture line numbers (from fixtures/rust/src/main.rs):
//   107: let symbol = ...
//   109: let quantity = ...
//   111: let price = ...
//   114: let order_id = place_order(...)
//   117: let entry_price = ...
//   119: let current_price = ...
//   121: let pnl = ...
//   128: log!(...)
//   130: stderr().flush()
//   132: thread::sleep
const OFFSET_SYMBOL: i32 = 32; // observe at 108 (after assignment on 107)
const OFFSET_QUANTITY: i32 = 34; // observe at 110 (after assignment on 109)
const OFFSET_PRICE: i32 = 36; // observe at 112 (after assignment on 111)
const OFFSET_ORDER_ID: i32 = 39; // observe at 115 (after call on 114)
const OFFSET_ENTRY_PRICE: i32 = 42; // observe at 118 (after assignment on 117)
const OFFSET_CURRENT_PRICE: i32 = 44; // observe at 120 (after assignment on 119)
const OFFSET_PNL: i32 = 46; // observe at 122 (after assignment on 121)
const OFFSET_PRINTLN: i32 = 52; // log! at line 128
const OFFSET_FLUSH: i32 = 54; // stderr().flush() at line 130
const OFFSET_SLEEP: i32 = 56; // thread::sleep at line 132

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let daemon_port = parse_arg(&args, "--daemon-port", 8090);
    let daemon_url = parse_string_arg(&args, "--daemon-url")
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", daemon_port));
    let fixture_dir = parse_string_arg(&args, "--fixture-dir")
        .map(PathBuf::from)
        .unwrap_or_else(find_fixture_dir);

    std::process::exit(run(&daemon_url, &fixture_dir));
}

fn parse_arg(args: &[String], name: &str, default: i32) -> i32 {
    for i in 0..args.len() - 1 {
        if args[i] == name {
            return args[i + 1].parse().unwrap_or(default);
        }
    }
    default
}

fn parse_string_arg(args: &[String], name: &str) -> Option<String> {
    for i in 0..args.len() - 1 {
        if args[i] == name {
            return Some(args[i + 1].clone());
        }
    }
    None
}

fn find_fixture_dir() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    let candidates = [
        cwd.join("../../fixtures/rust"), // from clients/rust
        cwd.join("fixtures/rust"),       // from detrix-release
        cwd.join("../fixtures/rust"),    // from clients/
    ];

    for p in &candidates {
        let abs = p.canonicalize().unwrap_or_else(|_| p.clone());
        if abs.join("Cargo.toml").exists() {
            return abs;
        }
    }

    eprintln!("ERROR: Could not find fixtures/rust directory");
    eprintln!("Please specify with --fixture-dir flag");
    std::process::exit(1);
}

fn run(daemon_url: &str, fixture_dir: &PathBuf) -> i32 {
    println!("{}", "=".repeat(70));
    println!("Agent Simulation Test - Wake and Observe a Running Rust Application");
    println!("{}", "=".repeat(70));
    println!();
    println!("Daemon URL: {}", daemon_url);
    println!("Fixture dir: {}", fixture_dir.display());
    println!("Main line: {} (offsets calculated from this)", MAIN_LINE);
    println!();

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client");

    // Step 1: Check daemon is running
    println!("1. Checking Detrix daemon...");
    if !check_daemon_health(&client, daemon_url) {
        println!("   ERROR: Detrix daemon is not running!");
        println!("   Start it with: detrix serve --daemon");
        return 1;
    }
    println!("   Daemon is healthy");
    println!();

    // Step 2: Build and start the Rust fixture app
    println!("2. Building and starting Rust fixture app...");

    // Build with debug symbols and client feature
    let status = Command::new("cargo")
        .args(["build", "--features", "client"])
        .current_dir(fixture_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    if status.map(|s| !s.success()).unwrap_or(true) {
        println!("   ERROR: Failed to build fixture app");
        return 1;
    }
    println!("   Built fixture app");

    // Start the app with Detrix client enabled
    let mut child = match Command::new("cargo")
        .args(["run", "--features", "client"])
        .current_dir(fixture_dir)
        .env("DETRIX_CLIENT_ENABLED", "1")
        .env("DETRIX_DAEMON_URL", daemon_url)
        .env("DETRIX_CLIENT_NAME", "trade-bot")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            println!("   ERROR: Failed to start app: {}", e);
            return 1;
        }
    };

    // Wait for control plane URL in output
    let control_url = match wait_for_control_url(&mut child) {
        Some(url) => url,
        None => {
            println!("   ERROR: Could not find control plane URL in output");
            let _ = child.kill();
            return 1;
        }
    };

    println!("   Found control plane: {}", control_url);
    println!();

    // Step 3: Wait 3 seconds (simulating agent discovering the app)
    println!("3. Waiting 3 seconds (simulating agent discovery time)...");
    for i in (1..=3).rev() {
        print!("   {}... ", i);
        std::io::Write::flush(&mut std::io::stdout()).ok();
        std::thread::sleep(Duration::from_secs(1));
    }
    println!();
    println!();

    // Step 4: Wake the client via control plane
    println!("4. Waking Detrix client...");

    // First check if control plane is healthy
    let health_url = format!("{}/detrix/health", control_url);
    println!("   Checking health at {}...", health_url);
    match client.get(&health_url).send() {
        Ok(resp) => {
            println!(
                "   Health check response: {} {:?}",
                resp.status(),
                resp.text().unwrap_or_default()
            );
        }
        Err(e) => {
            println!("   Health check FAILED: {:?}", e);
            let _ = child.kill();
            return 1;
        }
    }

    let wake_resp = match api_post(&client, &format!("{}/detrix/wake", control_url), json!({})) {
        Ok(resp) => resp,
        Err(e) => {
            println!("   ERROR: Failed to wake client: {}", e);
            let _ = child.kill();
            return 1;
        }
    };

    let status = wake_resp
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let debug_port = wake_resp
        .get("debug_port")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let connection_id = wake_resp
        .get("connection_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    println!("   Status: {}", status);
    println!("   Debug port: {}", debug_port);
    println!("   Connection ID: {}", connection_id);
    println!();

    // Step 5: Verify connection on daemon
    println!("5. Verifying connection registered with daemon...");
    std::thread::sleep(Duration::from_secs(1));

    if let Some(conn) = get_connection_by_id(&client, daemon_url, connection_id) {
        let status = conn.get("status").and_then(|v| v.as_i64()).unwrap_or(0);
        let status_name = match status {
            1 => "disconnected",
            2 => "connecting",
            3 => "connected",
            _ => "unknown",
        };
        println!("   Connection: {}", connection_id);
        println!("   Status: {}", status_name);
        println!(
            "   Port: {}",
            conn.get("port").and_then(|v| v.as_i64()).unwrap_or(0)
        );
    } else {
        println!("   WARNING: Connection not found on daemon");
    }
    println!();

    // Step 6: Add metrics with dynamic line numbers
    println!("6. Adding metrics to observe the running application...");

    let fixture_file = fixture_dir.join("src/main.rs");
    let fixture_path = fixture_file.to_string_lossy();

    // Metric 1: symbol (simple variable)
    let line_symbol = MAIN_LINE + OFFSET_SYMBOL;
    let metric1 = add_metric(
        &client,
        daemon_url,
        connection_id,
        &fixture_path,
        line_symbol,
        "symbol",
        "order_symbol",
    );
    print_metric_result("symbol", line_symbol, &metric1);

    // Metric 2: pnl (calculated value)
    let line_pnl = MAIN_LINE + OFFSET_PNL;
    let metric2 = add_metric(
        &client,
        daemon_url,
        connection_id,
        &fixture_path,
        line_pnl,
        "pnl",
        "pnl_value",
    );
    print_metric_result("pnl", line_pnl, &metric2);

    // Metric 3: quantity (with stack trace)
    let line_println = MAIN_LINE + OFFSET_PRINTLN;
    let metric3 = add_metric_with_introspection(
        &client,
        daemon_url,
        connection_id,
        &fixture_path,
        line_println,
        "quantity",
        "quantity_with_stack",
    );
    print_metric_result("quantity (with stack trace)", line_println, &metric3);

    println!();

    // Step 7: Wait and collect events
    println!("7. Waiting for events (15 seconds)...");
    println!();

    let mut events_received = 0;
    let mut seen_events: HashSet<String> = HashSet::new();

    let metric_ids: Vec<(i64, &str)> = vec![
        (
            metric1
                .as_ref()
                .and_then(|m| m.get("metricId"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            "order_symbol",
        ),
        (
            metric2
                .as_ref()
                .and_then(|m| m.get("metricId"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            "pnl_value",
        ),
        (
            metric3
                .as_ref()
                .and_then(|m| m.get("metricId"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            "quantity_with_stack",
        ),
    ];

    for _ in 0..15 {
        std::thread::sleep(Duration::from_secs(1));

        for (metric_id, name) in &metric_ids {
            if *metric_id <= 0 {
                continue;
            }

            if let Some(events) = get_events(&client, daemon_url, *metric_id, 10) {
                for event in events.as_array().unwrap_or(&vec![]) {
                    let ts = event.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
                    let event_key = format!("{}-{}", metric_id, ts);
                    if seen_events.contains(&event_key) {
                        continue;
                    }
                    seen_events.insert(event_key);
                    events_received += 1;

                    let value = event
                        .get("result")
                        .and_then(|r| r.get("valueJson"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("null");
                    let time_str = format_timestamp(ts);

                    println!("   [EVENT] {}: {} ({})", name, value, time_str);

                    // Check for stack trace
                    if let Some(stack) = event.get("stackTrace") {
                        if let Some(frames) = stack.get("frames").and_then(|f| f.as_array()) {
                            if !frames.is_empty() {
                                println!("           Stack trace ({} frames):", frames.len());
                                for (i, frame) in frames.iter().enumerate().take(5) {
                                    let name =
                                        frame.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                    let file =
                                        frame.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                                    let file_name = std::path::Path::new(file)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("?");
                                    let line =
                                        frame.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
                                    println!(
                                        "             [{}] {} ({}:{})",
                                        i, name, file_name, line
                                    );
                                }
                                if frames.len() > 5 {
                                    println!(
                                        "             ... and {} more frames",
                                        frames.len() - 5
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    println!();
    println!("   Total events received: {}", events_received);
    println!();

    // Step 8: Cleanup
    println!("8. Cleaning up...");
    let _ = child.kill();
    let _ = child.wait();
    println!("   App stopped");
    println!();

    println!("{}", "=".repeat(70));
    if events_received > 0 {
        println!("TEST PASSED - Successfully observed running Rust application!");
    } else {
        println!("TEST COMPLETED - No events captured (check metric expressions)");
    }
    println!("{}", "=".repeat(70));

    0
}

fn wait_for_control_url(child: &mut Child) -> Option<String> {
    let stdout = child.stdout.take()?;
    let reader = BufReader::new(stdout);
    let re = Regex::new(r"Control plane: (http://[\d.:]+)").ok()?;

    let start = Instant::now();
    let timeout = Duration::from_secs(30);

    // Collect lines in a Vec so we can iterate and then spawn a drain thread
    let mut lines_iter = reader.lines();
    let mut control_url = None;

    while let Some(line_result) = lines_iter.next() {
        if start.elapsed() > timeout {
            break;
        }

        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };

        println!("   [app] {}", line);

        if let Some(caps) = re.captures(&line) {
            control_url = caps.get(1).map(|m| m.as_str().to_string());
            break;
        }
    }

    // Spawn a thread to drain remaining stdout to prevent broken pipe
    if control_url.is_some() {
        std::thread::spawn(move || {
            for line in lines_iter {
                if let Ok(l) = line {
                    println!("   [app] {}", l);
                }
            }
        });
    }

    control_url
}

fn check_daemon_health(client: &Client, daemon_url: &str) -> bool {
    let url = format!("{}/health", daemon_url);
    match client.get(&url).send() {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<Value>() {
                body.get("service").and_then(|v| v.as_str()) == Some("detrix")
            } else {
                false
            }
        }
        _ => false,
    }
}

fn api_post(client: &Client, url: &str, body: Value) -> Result<Value, String> {
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .map_err(|e| format!("{:?}", e))?; // Use Debug format for more details

    if !resp.status().is_success() {
        return Err(format!(
            "HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        ));
    }

    resp.json().map_err(|e| format!("{:?}", e))
}

fn get_connection_by_id(client: &Client, daemon_url: &str, connection_id: &str) -> Option<Value> {
    let url = format!("{}/api/v1/connections", daemon_url);
    let resp = client.get(&url).send().ok()?;
    let connections: Vec<Value> = resp.json().ok()?;

    connections
        .into_iter()
        .find(|c| c.get("connectionId").and_then(|v| v.as_str()) == Some(connection_id))
}

fn add_metric(
    client: &Client,
    daemon_url: &str,
    connection_id: &str,
    file_path: &str,
    line: i32,
    expression: &str,
    name: &str,
) -> Option<Value> {
    let url = format!("{}/api/v1/metrics", daemon_url);
    let body = json!({
        "name": name,
        "connectionId": connection_id,
        "location": {
            "file": file_path,
            "line": line,
        },
        "expression": expression,
        "language": "rust",
        "enabled": true,
    });

    match api_post(client, &url, body) {
        Ok(resp) => Some(resp),
        Err(e) => {
            println!("   Error adding metric: {}", e);
            None
        }
    }
}

fn add_metric_with_introspection(
    client: &Client,
    daemon_url: &str,
    connection_id: &str,
    file_path: &str,
    line: i32,
    expression: &str,
    name: &str,
) -> Option<Value> {
    let url = format!("{}/api/v1/metrics", daemon_url);
    let body = json!({
        "name": name,
        "connectionId": connection_id,
        "location": {
            "file": file_path,
            "line": line,
        },
        "expression": expression,
        "language": "rust",
        "enabled": true,
        "captureStackTrace": true,
    });

    match api_post(client, &url, body) {
        Ok(resp) => Some(resp),
        Err(e) => {
            println!("   Error adding metric with introspection: {}", e);
            None
        }
    }
}

fn print_metric_result(name: &str, line: i32, result: &Option<Value>) {
    if let Some(resp) = result {
        let id = resp.get("metricId").and_then(|v| v.as_i64()).unwrap_or(0);
        println!("   Metric ({} @ line {}): ID={}", name, line, id);
    } else {
        println!("   WARNING: Failed to add metric {} @ line {}", name, line);
    }
}

fn get_events(client: &Client, daemon_url: &str, metric_id: i64, limit: i32) -> Option<Value> {
    let url = format!(
        "{}/api/v1/events?metricId={}&limit={}",
        daemon_url, metric_id, limit
    );
    client.get(&url).send().ok()?.json().ok()
}

fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return "?".to_string();
    }
    // ts is in microseconds
    let secs = ts / 1_000_000;
    let micros = ts % 1_000_000;
    let time = std::time::UNIX_EPOCH
        + Duration::from_secs(secs as u64)
        + Duration::from_micros(micros as u64);

    // Format as HH:MM:SS
    let datetime = chrono::DateTime::<chrono::Local>::from(time);
    datetime.format("%H:%M:%S").to_string()
}
