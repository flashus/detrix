//! E2E Test: Go Client Wake/Sleep Scenario
//!
//! This test simulates the EXACT scenario from clients/go/examples/test_wake:
//! 1. Start a Go app with embedded Detrix client (sleeping)
//! 2. Wake the client via control plane
//! 3. Add metrics via daemon API
//! 4. Verify BOTH logpoints AND breakpoints produce events
//!
//! This is critical for production observability - logpoints must work
//! in the wake/sleep model where the debugger attaches on-demand.

use detrix_testing::e2e::{
    executor::TestExecutor, reporter::TestReporter, require_tool, ToolDependency,
};
use regex::Regex;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Test: Go client wake/sleep with logpoints and breakpoints
///
/// Verifies:
/// - Client can wake and attach Delve on-demand
/// - Logpoints work after wake (critical for production)
/// - Breakpoints work after wake
/// - Mixed logpoints + breakpoints produce all events
#[tokio::test]
async fn test_go_client_wake_logpoints_and_breakpoints() {
    if !require_tool(ToolDependency::Delve).await {
        return;
    }

    let mut executor = TestExecutor::new();
    let reporter = TestReporter::new("Go Client Wake E2E (Logpoints + Breakpoints)", "Daemon");
    reporter.print_header();

    // ====================================================================
    // PHASE 1: Start daemon
    // ====================================================================
    reporter.section("PHASE 1: START DAEMON");
    let step = reporter.step_start("Start Daemon", "Start Detrix daemon");

    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e.to_string());
        executor.print_daemon_logs(50);
        panic!("Failed to start daemon: {}", e);
    }
    reporter.step_success(step, Some(&format!("Port: {}", executor.http_port)));

    // ====================================================================
    // PHASE 2: Build and start Go fixture app with embedded client
    // ====================================================================
    reporter.section("PHASE 2: START GO APP WITH EMBEDDED CLIENT");

    let fixture_dir = executor.workspace_root.join("fixtures/go");
    let binary_path = fixture_dir.join("detrix_example_app");

    // Build the Go app with debug symbols
    let step = reporter.step_start("Build Go App", "go build -gcflags='all=-N -l'");
    let build_output = Command::new("go")
        .args([
            "build",
            "-gcflags=all=-N -l",
            "-o",
            "detrix_example_app",
            ".",
        ])
        .current_dir(&fixture_dir)
        .output()
        .expect("Failed to build Go app");

    if !build_output.status.success() {
        reporter.step_failed(
            step,
            &format!(
                "Build failed: {}",
                String::from_utf8_lossy(&build_output.stderr)
            ),
        );
        panic!("Go build failed");
    }
    reporter.step_success(step, None);

    // Start the app with Detrix client enabled
    let step = reporter.step_start("Start Go App", "Start with DETRIX_CLIENT_ENABLED=1");
    let connection_id = format!("go-wake-test-{}", std::process::id());
    let daemon_url = format!("http://127.0.0.1:{}", executor.http_port);

    let mut app_process = Command::new(&binary_path)
        .env("DETRIX_CLIENT_ENABLED", "1")
        .env("DETRIX_DAEMON_URL", &daemon_url)
        .env("DETRIX_CLIENT_NAME", &connection_id)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start Go app");

    // Wait for control plane to start and extract port
    let control_port = wait_for_control_plane(&mut app_process).await;
    reporter.step_success(step, Some(&format!("Control plane: {}", control_port)));

    // Wait 3 seconds (simulating agent discovery)
    tokio::time::sleep(Duration::from_secs(3)).await;

    // ====================================================================
    // PHASE 3: Wake the client
    // ====================================================================
    reporter.section("PHASE 3: WAKE CLIENT");
    let step = reporter.step_start(
        "Wake Client",
        &format!("POST http://localhost:{}/detrix/wake", control_port),
    );

    let client = reqwest::Client::new();
    let wake_response = client
        .post(format!("http://127.0.0.1:{}/detrix/wake", control_port))
        .send()
        .await
        .expect("Failed to wake client");

    let wake_result: serde_json::Value = wake_response
        .json()
        .await
        .expect("Failed to parse wake response");
    let debug_port = wake_result["debug_port"]
        .as_u64()
        .expect("No debug_port in response");
    let returned_connection_id = wake_result["connection_id"]
        .as_str()
        .expect("No connection_id in response");

    reporter.step_success(
        step,
        Some(&format!(
            "Awake: debug_port={}, connection_id={}",
            debug_port, returned_connection_id
        )),
    );

    // Wait for connection to register with daemon
    tokio::time::sleep(Duration::from_secs(2)).await;

    // ====================================================================
    // PHASE 4: Add metrics (2 logpoints + 2 breakpoints)
    // ====================================================================
    reporter.section("PHASE 4: ADD METRICS (2 LOGPOINTS + 2 BREAKPOINTS)");

    let fixture_file = fixture_dir.join("detrix_example_app.go");

    // Metric 1: LOGPOINT - simple variable 'symbol' at line 118
    let step = reporter.step_start("Add Logpoint #1", "order_symbol: 'symbol' at line 118");
    let location1 = format!("@{}#118", fixture_file.display());
    let req1 = serde_json::json!({
        "name": "order_symbol",
        "location": location1,
        "expression": "symbol",
        "connectionId": returned_connection_id,
        "language": "go",
        "enabled": true,
    });

    let metric1_response: serde_json::Value = client
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
        .json(&req1)
        .send()
        .await
        .expect("Failed to add metric 1")
        .json()
        .await
        .expect("Failed to parse response");
    let metric1_id = metric1_response["metricId"].as_u64().expect("No metricId");
    reporter.step_success(step, Some(&format!("ID={}", metric1_id)));

    // Metric 2: LOGPOINT - simple variable 'pnl' at line 130
    let step = reporter.step_start("Add Logpoint #2", "pnl_value: 'pnl' at line 130");
    let location2 = format!("@{}#130", fixture_file.display());
    let req2 = serde_json::json!({
        "name": "pnl_value",
        "location": location2,
        "expression": "pnl",
        "connectionId": returned_connection_id,
        "language": "go",
        "enabled": true,
    });

    let metric2_response: serde_json::Value = client
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
        .json(&req2)
        .send()
        .await
        .expect("Failed to add metric 2")
        .json()
        .await
        .expect("Failed to parse response");
    let metric2_id = metric2_response["metricId"].as_u64().expect("No metricId");
    reporter.step_success(step, Some(&format!("ID={}", metric2_id)));

    // Metric 3: BREAKPOINT - function call 'len(symbol)' at line 122
    let step = reporter.step_start(
        "Add Breakpoint #1",
        "symbol_length: 'len(symbol)' at line 122",
    );
    let location3 = format!("@{}#122", fixture_file.display());
    let req3 = serde_json::json!({
        "name": "symbol_length",
        "location": location3,
        "expression": "len(symbol)",
        "connectionId": returned_connection_id,
        "language": "go",
        "enabled": true,
    });

    let metric3_response: serde_json::Value = client
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
        .json(&req3)
        .send()
        .await
        .expect("Failed to add metric 3")
        .json()
        .await
        .expect("Failed to parse response");
    let metric3_id = metric3_response["metricId"].as_u64().expect("No metricId");
    reporter.step_success(step, Some(&format!("ID={}", metric3_id)));

    // Metric 4: BREAKPOINT - with stack trace at line 126
    let step = reporter.step_start(
        "Add Breakpoint #2",
        "entry_price_with_stack: 'entryPrice' at line 126",
    );
    let location4 = format!("@{}#126", fixture_file.display());
    let req4 = serde_json::json!({
        "name": "entry_price_with_stack",
        "location": location4,
        "expression": "entryPrice",
        "connectionId": returned_connection_id,
        "language": "go",
        "enabled": true,
        "captureStackTrace": true,
    });

    let metric4_response: serde_json::Value = client
        .post(format!(
            "http://127.0.0.1:{}/api/v1/metrics",
            executor.http_port
        ))
        .json(&req4)
        .send()
        .await
        .expect("Failed to add metric 4")
        .json()
        .await
        .expect("Failed to parse response");
    let metric4_id = metric4_response["metricId"].as_u64().expect("No metricId");
    reporter.step_success(step, Some(&format!("ID={}", metric4_id)));

    // ====================================================================
    // PHASE 5: Wait for events and verify ALL metrics produce events
    // ====================================================================
    reporter.section("PHASE 5: VERIFY EVENTS (15 SECONDS)");

    let session_start_micros = chrono::Utc::now().timestamp_micros();

    reporter.info("Waiting 15 seconds for events to accumulate...");
    reporter.info("Expected: ALL 4 metrics should produce events (including logpoints!)");

    tokio::time::sleep(Duration::from_secs(15)).await;

    // Check events for each metric
    let metrics_to_check = vec![
        (metric1_id, "order_symbol", "LOGPOINT"),
        (metric2_id, "pnl_value", "LOGPOINT"),
        (metric3_id, "symbol_length", "BREAKPOINT"),
        (metric4_id, "entry_price_with_stack", "BREAKPOINT+STACK"),
    ];

    let mut all_passed = true;

    for (metric_id, name, mode) in metrics_to_check {
        let step = reporter.step_start(
            &format!("Check {} Events", name),
            &format!("Mode: {}", mode),
        );

        let url = format!(
            "http://127.0.0.1:{}/api/v1/events?metricId={}&limit=10&since={}",
            executor.http_port, metric_id, session_start_micros
        );

        let events_response = client
            .get(url)
            .send()
            .await
            .expect("Failed to query events");
        let events: Vec<serde_json::Value> = events_response
            .json()
            .await
            .expect("Failed to parse events");

        if events.is_empty() {
            reporter.step_failed(step, &format!("NO EVENTS! {} metrics must work!", mode));
            all_passed = false;
        } else {
            let sample_value = events[0]["result"]["valueJson"].as_str().unwrap_or("N/A");
            reporter.step_success(
                step,
                Some(&format!(
                    "{} events, sample value: {}",
                    events.len(),
                    sample_value
                )),
            );
        }
    }

    // ====================================================================
    // PHASE 6: Cleanup
    // ====================================================================
    reporter.section("PHASE 6: CLEANUP");

    // Sleep the client
    let _ = client
        .post(format!("http://127.0.0.1:{}/detrix/sleep", control_port))
        .send()
        .await;

    // Stop the app
    app_process.kill().expect("Failed to kill app");

    // Print final result
    if all_passed {
        eprintln!("\n{}", "=".repeat(70));
        eprintln!("TEST PASSED - All metrics (logpoints + breakpoints) produced events!");
        eprintln!("{}\n", "=".repeat(70));
    } else {
        eprintln!("\n{}", "=".repeat(70));
        eprintln!("TEST FAILED - Some metrics did not produce events");
        eprintln!("{}\n", "=".repeat(70));
        panic!("Test failed: Not all metrics produced events");
    }
}

/// Wait for the Go app to start and extract the control plane port
async fn wait_for_control_plane(process: &mut Child) -> u16 {
    use std::io::{BufRead, BufReader};

    let stdout = process.stdout.take().expect("Failed to capture stdout");
    let reader = BufReader::new(stdout);
    let port_re = Regex::new(r"Control plane: http://[\d.:]+:(\d+)").unwrap();

    for line in reader.lines() {
        let line = line.expect("Failed to read line");
        eprintln!("[app] {}", line);

        if let Some(captures) = port_re.captures(&line) {
            return captures[1].parse().expect("Failed to parse port");
        }
    }

    panic!("Failed to find control plane port in app output");
}
