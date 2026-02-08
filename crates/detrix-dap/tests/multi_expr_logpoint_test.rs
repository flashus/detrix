//! Multi-expression logpoint proof-of-concept test
//!
//! Verifies that DAP logpoints can contain multiple `{expression}` blocks
//! and that all debuggers (starting with Delve) interpolate each one independently.
//!
//! This is a prerequisite test for the "multiple expressions per metric" feature.
//! It talks directly to DAP without going through Detrix's metric layer.
//!
//! Run with: cargo test --package detrix-dap --test multi_expr_logpoint_test

mod common;

use common::{get_test_port, wait_for_port};
use detrix_dap::{AdapterProcess, GoAdapter, SetBreakpointsArguments, Source, SourceBreakpoint};
use detrix_testing::e2e::{require_tool, ToolDependency};
use std::process::Stdio;
use tokio::process::Command;

fn go_program_multi_vars() -> String {
    r#"package main

import (
	"fmt"
	"time"
)

func main() {
	fmt.Println("started")
	for i := 0; i < 50; i++ {
		symbol := "BTCUSD"
		quantity := i + 1
		price := 100.5
		total := float64(quantity) * price
		_ = fmt.Sprintf("%s %d %f %f", symbol, quantity, price, total)
		time.Sleep(200 * time.Millisecond)
	}
}
"#
    .to_string()
}

/// Test: Delve logpoint with multiple `{expression}` blocks in a single logMessage.
///
/// Sends a logpoint like: `"MULTI:{symbol}|{quantity}|{price}|{total}"`
/// Expects output like: `"> [Go 1]: MULTI:BTCUSD|1|100.5|100.5"`
///
/// This proves that DAP logpoints can carry multiple variable values
/// in a single non-blocking observation point.
#[tokio::test]
async fn test_delve_multi_expression_logpoint() {
    if !require_tool(ToolDependency::Delve).await {
        return;
    }

    let port = get_test_port();
    eprintln!("=== Multi-expression logpoint test on port {} ===", port);

    // Write Go source
    let script_path = std::env::temp_dir().join(format!("detrix_multi_expr_{}.go", port));
    std::fs::write(&script_path, go_program_multi_vars()).unwrap();

    // Build with debug info (no optimizations)
    let binary_path = format!("/tmp/detrix_multi_expr_{}", port);
    let build = Command::new("go")
        .args([
            "build",
            "-gcflags",
            "all=-N -l",
            "-o",
            &binary_path,
            script_path.to_str().unwrap(),
        ])
        .output()
        .await
        .expect("go build failed");

    if !build.status.success() {
        eprintln!(
            "Go build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        std::fs::remove_file(&script_path).ok();
        panic!("Go build failed");
    }

    // Start Delve in headless DAP mode
    let mut delve = Command::new("dlv")
        .args([
            "exec",
            &binary_path,
            "--headless",
            "--api-version=2",
            "--accept-multiclient",
            "--listen",
            &format!("127.0.0.1:{}", port),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start Delve");

    if !wait_for_port(port, 15).await {
        delve.kill().await.ok();
        std::fs::remove_file(&script_path).ok();
        std::fs::remove_file(&binary_path).ok();
        panic!("Delve did not start on port {}", port);
    }
    eprintln!("Delve listening on port {}", port);

    // Connect via AdapterProcess (handles init/attach/configurationDone/continue)
    let config = GoAdapter::default_config(port);
    let adapter = AdapterProcess::new(config);

    let start_result = adapter.start().await;
    assert!(
        start_result.is_ok(),
        "Failed to connect: {:?}",
        start_result
    );
    eprintln!("DAP session established");

    // Verify logpoint support
    let caps = adapter.capabilities().await.expect("No capabilities");
    assert!(
        caps.supports_log_points.unwrap_or(false),
        "Delve must support logpoints"
    );

    // Get the broker for raw DAP communication
    let broker = adapter.broker().await.expect("No broker");

    // Subscribe to events BEFORE setting the logpoint
    let mut events = broker.subscribe_events().await;

    // === THE KEY TEST: Set a logpoint with MULTIPLE {expr} blocks ===
    //
    // Line 15 in our program: `total := float64(quantity) * price`
    // At this line: symbol, quantity, price are in scope. total is being assigned.
    // We use line 16 (`_ = fmt.Sprintf(...)`) where total is also in scope.
    let multi_expr_logmessage = "MULTI:{symbol}|{quantity}|{price}|{total}";

    let source = Source {
        path: Some(script_path.to_str().unwrap().to_string()),
        name: Some("test.go".to_string()),
        source_reference: None,
    };

    let args = SetBreakpointsArguments {
        source,
        breakpoints: Some(vec![SourceBreakpoint::logpoint(
            16, // `_ = fmt.Sprintf(...)` line - all vars in scope
            multi_expr_logmessage,
        )]),
        source_modified: Some(false),
    };

    let response = broker
        .send_request("setBreakpoints", Some(serde_json::to_value(&args).unwrap()))
        .await
        .expect("setBreakpoints failed");

    assert!(response.success, "setBreakpoints rejected: {:?}", response);
    eprintln!("Logpoint set with message: {:?}", multi_expr_logmessage);

    // Parse response to check if breakpoint was verified
    if let Some(body) = &response.body {
        eprintln!("setBreakpoints response body: {}", body);
    }

    // === Collect output events ===
    // Wait for logpoint to fire (program loops every 200ms)
    let mut collected_outputs: Vec<String> = Vec::new();
    let timeout = tokio::time::Duration::from_secs(10);
    let deadline = tokio::time::Instant::now() + timeout;

    eprintln!("Waiting for logpoint output events...");

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Some(evt) if evt.event == "output" => {
                        if let Some(body) = &evt.body {
                            let output = body.get("output")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            eprintln!("  OUTPUT event: {:?}", output.trim());

                            // Look for our MULTI: prefix in the output
                            if output.contains("MULTI:") {
                                collected_outputs.push(output.trim().to_string());

                                // We only need a few hits to prove it works
                                if collected_outputs.len() >= 3 {
                                    break;
                                }
                            }
                        }
                    }
                    Some(evt) => {
                        eprintln!("  Other event: {}", evt.event);
                    }
                    None => {
                        eprintln!("Event channel closed");
                        break;
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                eprintln!("Timeout waiting for events");
                break;
            }
        }
    }

    // Cleanup
    adapter.stop().await.ok();
    delve.kill().await.ok();
    std::fs::remove_file(&script_path).ok();
    std::fs::remove_file(&binary_path).ok();

    // === ASSERTIONS ===
    eprintln!("\n=== RESULTS ===");
    eprintln!("Collected {} output events", collected_outputs.len());
    for (i, output) in collected_outputs.iter().enumerate() {
        eprintln!("  [{}] {}", i, output);
    }

    assert!(
        !collected_outputs.is_empty(),
        "Expected at least one MULTI: logpoint output event, got none. \
         This means Delve did not fire the logpoint or the output format is unexpected."
    );

    // Verify each output contains the pipe-delimited multi-expression result
    for output in &collected_outputs {
        // Strip Delve's goroutine prefix: "> [Go N]: MULTI:..."
        let multi_part = if let Some(idx) = output.find("MULTI:") {
            &output[idx..]
        } else {
            panic!("Output doesn't contain MULTI: prefix: {}", output);
        };

        // Expected format: "MULTI:BTCUSD|<int>|100.5|<float>"
        let after_prefix = multi_part.strip_prefix("MULTI:").unwrap();
        let parts: Vec<&str> = after_prefix.split('|').collect();

        eprintln!("  Parsed {} parts: {:?}", parts.len(), parts);

        assert_eq!(
            parts.len(),
            4,
            "Expected 4 pipe-delimited values (symbol|quantity|price|total), got {}: {:?}",
            parts.len(),
            parts
        );

        // Verify each expression was independently interpolated
        // Note: Delve quotes string values, so symbol comes as "BTCUSD" (with quotes)
        let symbol = parts[0].trim_matches('"');
        assert_eq!(
            symbol, "BTCUSD",
            "symbol should be BTCUSD (got raw: {})",
            parts[0]
        );

        let quantity: i64 = parts[1]
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("quantity should be integer, got: '{}'", parts[1]));
        assert!(quantity >= 1, "quantity should be >= 1, got {}", quantity);

        let price: f64 = parts[2]
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("price should be float, got: '{}'", parts[2]));
        assert!(
            (price - 100.5).abs() < 0.01,
            "price should be ~100.5, got {}",
            price
        );

        let total: f64 = parts[3]
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("total should be float, got: '{}'", parts[3]));
        assert!(total > 0.0, "total should be > 0, got {}", total);

        // Verify total = quantity * price
        let expected_total = quantity as f64 * price;
        assert!(
            (total - expected_total).abs() < 0.01,
            "total ({}) should equal quantity ({}) * price ({})",
            total,
            quantity,
            price
        );
    }

    eprintln!("\n=== MULTI-EXPRESSION LOGPOINT TEST PASSED ===");
    eprintln!(
        "Delve successfully interpolated 4 independent {{expr}} blocks in a single logpoint!"
    );
}
