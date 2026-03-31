//! eBPF Integration E2E Test
//!
//! Verifies that the detrix-ebpf adapter correctly captures Go variable values
//! using Linux uprobes — without Delve or any DAP debug adapter.
//!
//! # How to run
//!
//! Build the Docker image and run with `--privileged` (macOS or Linux):
//! ```sh
//! task test-ebpf
//! ```
//!
//! Or manually on a Linux host with CAP_BPF:
//! ```sh
//! DETRIX_BINARY=/path/to/detrix \
//! GO_FIXTURE_BINARY=/path/to/detrix_example_app \
//! GO_FIXTURE_SOURCE=/path/to/detrix_example_app.go \
//! cargo test --test ebpf_e2e -- --ignored --nocapture
//! ```
//!
//! # Environment variables
//!
//! | Variable            | Description                                              |
//! |---------------------|----------------------------------------------------------|
//! | DETRIX_BINARY       | Path to the detrix daemon binary                         |
//! | GO_FIXTURE_BINARY   | Path to the compiled Go fixture binary (with DWARF)      |
//! | GO_FIXTURE_SOURCE   | DWARF source path baked into the binary at build time    |
//!
//! # Why `--privileged` / CAP_BPF?
//!
//! Loading BPF programs requires CAP_BPF (Linux 5.8+) or CAP_SYS_ADMIN on
//! older kernels. BPF ring buffer maps require kernel >= 5.8.
//! `docker run --privileged` grants all capabilities.

// Only compile and run this test on Linux — eBPF uprobes are Linux-only.
#![cfg(target_os = "linux")]

use detrix_testing::e2e::dap_scenarios::go_lines;
use detrix_testing::e2e::{executor::TestExecutor, reporter::TestReporter};
use std::env;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

// ── Constants ────────────────────────────────────────────────────────────────

/// How long to wait for the Go fixture process to be fully running before attaching.
/// No control plane or client protocol needed — eBPF attaches from the daemon side.
const FIXTURE_START_WAIT: Duration = Duration::from_secs(2);

/// How long to wait after connection creation for the eBPF adapter to start and attach.
/// eBPF startup is much faster than Delve (~100ms vs ~5s), but leave margin.
const ADAPTER_START_WAIT: Duration = Duration::from_secs(3);

/// How long to collect events after metrics are added.
const EVENT_COLLECTION_WAIT: Duration = Duration::from_secs(20);

/// Expected trading symbols produced by the fixture.
const TRADING_SYMBOLS: &[&str] = &["BTCUSD", "ETHUSD", "SOLUSD"];

// ── Main test ─────────────────────────────────────────────────────────────────

/// End-to-end test: Go logpoint via eBPF uprobe
///
/// 1. Start detrix daemon (uses EbpfGoFactory on Linux automatically)
/// 2. Start Go fixture as a plain binary (no Detrix client, no Delve required)
/// 3. Register the connection directly via REST: host = binary path → EbpfAdapter
/// 4. Add a logpoint observing `symbol` and `quantity`
/// 5. Assert eBPF ring buffer events arrive with expected values
///
/// Unlike DAP-based tests, the fixture process needs no cooperation from the
/// Detrix client library. eBPF uprobes attach to any running binary from
/// the daemon side using only the ELF path and DWARF debug info.
#[tokio::test]
#[ignore = "requires CAP_BPF and Linux kernel >= 5.8 — run via: task test-ebpf"]
async fn test_ebpf_go_uprobe_captures_variables() {
    let reporter = TestReporter::new("eBPF Go Uprobe E2E", "EbpfAdapter");
    reporter.print_header();

    // ── PHASE 1: Start daemon ────────────────────────────────────────────────
    reporter.section("PHASE 1: START DAEMON");
    let step = reporter.step_start(
        "Start Daemon",
        "detrix serve (EbpfGoFactory active on Linux)",
    );

    // DETRIX_BIN env var is read by find_detrix_binary() inside TestExecutor.
    // Dockerfile.ebpf-test sets DETRIX_BIN=/usr/local/bin/detrix.
    // Locally it falls back to target/release/detrix as usual.
    let mut executor = TestExecutor::new();

    if let Err(e) = executor.start_daemon().await {
        reporter.step_failed(step, &e.to_string());
        panic!("Failed to start daemon: {e}");
    }
    reporter.step_success(step, Some(&format!("HTTP port: {}", executor.http_port)));

    // ── PHASE 2: Start Go fixture ────────────────────────────────────────────
    reporter.section("PHASE 2: START GO FIXTURE (plain binary — no Detrix client, no Delve)");

    let fixture_binary = env::var("GO_FIXTURE_BINARY").unwrap_or_else(|_| {
        executor
            .workspace_root
            .join("fixtures/go/detrix_example_app")
            .to_string_lossy()
            .into_owned()
    });

    // The source file path as recorded in DWARF at build time.
    // In the Docker image this is /src/fixtures/go/detrix_example_app.go.
    // Locally it is the workspace-relative path.
    let fixture_source = env::var("GO_FIXTURE_SOURCE").unwrap_or_else(|_| {
        executor
            .workspace_root
            .join("fixtures/go/detrix_example_app.go")
            .to_string_lossy()
            .into_owned()
    });

    let step = reporter.step_start("Start Go Fixture", &fixture_binary);

    // Run as a plain binary — no Detrix client, no Delve dependency.
    // eBPF uprobes attach from the daemon side; the process needs no cooperation.
    // Discard stdout/stderr to avoid pipe buffer accumulation.
    let app = Command::new(&fixture_binary)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to start Go fixture — check GO_FIXTURE_BINARY");

    let fixture_pid = app.id();

    // Brief pause so the Go runtime has fully started before uprobe attachment.
    sleep(FIXTURE_START_WAIT).await;
    reporter.step_success(step, Some(&format!("PID: {fixture_pid}")));

    let connection_name = format!("ebpf-e2e-{}", std::process::id());

    // ── PHASE 3: Register connection directly via daemon REST API ────────────
    // On Linux, EbpfGoFactory treats `host` as the ELF binary path.
    // Passing host = fixture_binary causes the daemon to create EbpfAdapter(binary_path)
    // instead of a Delve connection. Port is unused by EbpfAdapter.
    reporter.section("PHASE 3: REGISTER CONNECTION (host = binary path → EbpfGoFactory)");

    let http = reqwest::Client::new();
    let step = reporter.step_start(
        "Create Connection",
        &format!("host={fixture_binary}, language=go"),
    );

    let conn_req = serde_json::json!({
        "host": fixture_binary,
        // EbpfGoFactory ignores port on Linux (uses host as binary path instead).
        // Pass the minimum unreserved port to satisfy domain validation (port >= 1024).
        "port": 1024,
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
        executor.print_daemon_logs(80);
        kill_app(app);
        panic!("Create connection failed: {conn_resp}");
    }

    let connection_id = conn_resp["connectionId"]
        .as_str()
        .expect("No connectionId in response")
        .to_string();

    reporter.step_success(step, Some(&format!("connectionId={connection_id}")));

    // Wait for EbpfAdapter to start (DWARF parse + initial uprobe setup).
    sleep(ADAPTER_START_WAIT).await;

    // ── PHASE 4: Add logpoint ────────────────────────────────────────────────
    reporter.section("PHASE 4: ADD LOGPOINT (symbol + quantity via eBPF uprobe)");

    // Use find_logpoint (not find_decl): Go's DWARF often omits the declaration line
    // for simple assignments, so we probe at the NEXT statement where the variable
    // is already in scope. find_logpoint("quantity") = MAIN_LINE + 28 (price line).
    let logpoint_line = go_lines::CODEMAP.find_logpoint("quantity");
    let location = format!("@{fixture_source}#{logpoint_line}");

    let step = reporter.step_start(
        "Add Metric",
        &format!("ebpf_symbol_qty @ {fixture_source}:{logpoint_line}"),
    );

    let metric_req = serde_json::json!({
        "name": "ebpf_symbol_qty",
        "location": location,
        "expressions": ["symbol", "quantity"],
        "connectionId": connection_id,
        "language": "go",
        "enabled": true,
    });

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

    let metric_id = match metric_resp["metricId"].as_u64() {
        Some(id) => id,
        None => {
            reporter.step_failed(step, &metric_resp.to_string());
            executor.print_daemon_logs(120);
            kill_app(app);
            panic!("No metricId in response: {metric_resp}");
        }
    };

    reporter.step_success(step, Some(&format!("metricId={metric_id}")));

    // ── PHASE 5: Collect and verify events ───────────────────────────────────
    reporter.section("PHASE 5: COLLECT eBPF EVENTS");
    reporter.info(&format!(
        "Waiting {}s for ring-buffer events to arrive (fixture fires every 3s)...",
        EVENT_COLLECTION_WAIT.as_secs()
    ));

    sleep(EVENT_COLLECTION_WAIT).await;

    let events_url = format!(
        "http://127.0.0.1:{}/api/v1/events?metricId={metric_id}&limit=20&since=0",
        executor.http_port
    );

    let events: Vec<serde_json::Value> = http
        .get(&events_url)
        .send()
        .await
        .expect("Events query failed")
        .json()
        .await
        .expect("Failed to parse events");

    // ── PHASE 6: Assertions ──────────────────────────────────────────────────
    reporter.section("PHASE 6: ASSERTIONS");

    kill_app(app);

    // Basic: at least one event arrived
    assert!(
        !events.is_empty(),
        "No eBPF events received for metric {metric_id}. \
         Check: (1) daemon has CAP_BPF, (2) clang is on PATH, \
         (3) binary was built with -gcflags='all=-N -l'.\n\
         Daemon logs above.",
    );
    executor.print_daemon_logs(100);

    reporter.info(&format!("Received {} eBPF events", events.len()));

    // Verify `symbol` value is one of the known trading symbols
    let first_event = &events[0];
    let values = first_event["values"]
        .as_array()
        .expect("Event has no 'values' array");

    let symbol_entry = values.iter().find(|v| v["expression"] == "symbol");
    assert!(
        symbol_entry.is_some(),
        "Event values do not contain 'symbol': {values:?}"
    );

    let symbol_val = symbol_entry.unwrap()["valueJson"]
        .as_str()
        .unwrap_or("")
        .trim_matches('"');

    assert!(
        TRADING_SYMBOLS.iter().any(|&s| s == symbol_val),
        "symbol value '{symbol_val}' is not one of {TRADING_SYMBOLS:?}. \
         This may indicate DWARF variable resolution or ring buffer parsing is wrong."
    );

    let step = reporter.step_start("Verify symbol", "value in TRADING_SYMBOLS");
    reporter.step_success(step, Some(&format!("symbol={symbol_val:?}")));

    reporter.info("eBPF uprobe test PASSED — variables captured correctly via ring buffer");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Send SIGTERM to the app process (best-effort cleanup).
fn kill_app(mut app: Child) {
    let _ = app.kill();
    let _ = app.wait();
}
