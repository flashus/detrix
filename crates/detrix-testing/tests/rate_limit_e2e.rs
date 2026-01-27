//! E2E tests for rate limiting from non-localhost IPs
//!
//! Tests that the REST API rate limiter correctly rejects requests
//! when the configured limit is exceeded for non-localhost clients.
//! Uses X-Forwarded-For header to simulate requests from external IPs.
//!
//! These tests complement localhost_exempt_e2e.rs which tests localhost exemption.
//!
//! Test Matrix:
//! - localhost_exempt_e2e.rs: Tests 127.0.0.1 → unlimited (exempt)
//! - rate_limit_e2e.rs: Tests external IPs via X-Forwarded-For → rate limited (DDoS protection)
//!
//! Note: These tests must run serially as they use the daemon process.

use detrix_testing::e2e::executor::find_detrix_binary;
use reqwest::StatusCode;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tempfile::TempDir;

/// TEST-NET-3 IP (RFC 5737) for simulating external requests
/// This IP is reserved for documentation and testing, will never route to a real host
/// https://datatracker.ietf.org/doc/html/rfc5737
const EXTERNAL_TEST_IP: &str = "203.0.113.1";

/// Global port counter to ensure each test gets a unique port
static PORT_COUNTER: AtomicU16 = AtomicU16::new(0);

/// Test executor specialized for rate limit testing
struct RateLimitTestExecutor {
    temp_dir: TempDir,
    http_port: u16,
    daemon_process: Option<Child>,
    daemon_log_path: PathBuf,
    workspace_root: PathBuf,
}

impl RateLimitTestExecutor {
    fn new() -> Self {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| manifest_dir.clone());

        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Use a unique port for this test - combine PID with atomic counter
        let counter = PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
        let http_port = 19000 + ((std::process::id() as u16 + counter) % 500);

        let daemon_log_path = temp_dir.path().join("daemon.log");

        Self {
            temp_dir,
            http_port,
            daemon_process: None,
            daemon_log_path,
            workspace_root,
        }
    }

    /// Start daemon with custom rate limit settings
    async fn start_daemon_with_rate_limit(
        &mut self,
        per_second: u64,
        burst_size: u32,
    ) -> Result<(), String> {
        // Create config file with low rate limit
        let db_path = self.temp_dir.path().join("detrix.db");
        let config_content = format!(
            r#"
[metadata]
version = "1.0"

[project]
base_path = "{}"

[storage]
storage_type = "sqlite"
path = "{}"

[api]
port_fallback = true

[api.rest]
enabled = true
host = "127.0.0.1"
port = {}

[api.rest.rate_limit]
enabled = true
per_second = {}
burst_size = {}
localhost_exempt = false

[api.grpc]
enabled = false

[safety]
enable_ast_analysis = false
"#,
            self.workspace_root.display(),
            db_path.display(),
            self.http_port,
            per_second,
            burst_size,
        );

        let config_path = self.temp_dir.path().join("detrix.toml");
        std::fs::write(&config_path, config_content).map_err(|e| e.to_string())?;

        // Find binary using standard search paths
        let binary_path = match find_detrix_binary(&self.workspace_root) {
            Some(p) => p,
            None => {
                return Err(
                    "detrix binary not found. Set DETRIX_BIN env var or run `cargo build -p detrix-cli`"
                        .to_string(),
                )
            }
        };

        // Write daemon output to log file
        let daemon_log_file =
            std::fs::File::create(&self.daemon_log_path).map_err(|e| e.to_string())?;
        let daemon_log_stderr = daemon_log_file.try_clone().map_err(|e| e.to_string())?;

        // Start daemon
        let process = Command::new(&binary_path)
            .args(["serve", "--config", config_path.to_str().unwrap()])
            .current_dir(&self.workspace_root)
            .env("RUST_LOG", "debug")
            .stdin(Stdio::null())
            .stdout(Stdio::from(daemon_log_file))
            .stderr(Stdio::from(daemon_log_stderr))
            .spawn();

        match process {
            Ok(p) => {
                self.daemon_process = Some(p);
                // Wait for HTTP server
                if !wait_for_port(self.http_port, 30).await {
                    return Err(format!(
                        "Daemon HTTP not responding on port {}",
                        self.http_port
                    ));
                }
                Ok(())
            }
            Err(e) => Err(format!("Could not spawn daemon: {}", e)),
        }
    }

    fn stop(&mut self) {
        if let Some(mut p) = self.daemon_process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }

    fn print_daemon_logs(&self, last_n_lines: usize) {
        println!("\n=== DAEMON LOG (last {} lines) ===", last_n_lines);
        if let Ok(content) = std::fs::read_to_string(&self.daemon_log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read daemon log)");
        }
        println!("=================================\n");
    }
}

impl Drop for RateLimitTestExecutor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Wait for HTTP server to be ready by making actual health check requests
async fn wait_for_port(port: u16, timeout_secs: u64) -> bool {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("Failed to create HTTP client for health check");

    let url = format!("http://127.0.0.1:{}/health", port);

    while start.elapsed() < timeout {
        // Try actual HTTP health check - this is more reliable than lsof
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 429 => {
                // Server is responding (either 200 OK or 429 rate limited means it's up)
                // Wait a moment for rate limiter to reset after health check requests
                tokio::time::sleep(Duration::from_millis(600)).await;
                return true;
            }
            _ => {
                // Server not ready yet, wait and retry
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    false
}

/// Test that rate limiting correctly returns 429 when limit is exceeded for external IPs
///
/// Uses X-Forwarded-For header to simulate requests from external (non-localhost) IP.
/// This tests DDoS protection for requests coming through a reverse proxy.
///
/// Run with: `cargo test --package detrix-testing --test rate_limit_e2e -- --test-threads=1`
#[tokio::test]
async fn test_rate_limit_external_ip_returns_429() {
    let mut executor = RateLimitTestExecutor::new();

    // Start daemon with low rate limit: 5 requests per second, burst of 3
    // This allows a few requests through initially, then limits subsequent rapid requests
    if let Err(e) = executor.start_daemon_with_rate_limit(5, 3).await {
        panic!("Failed to start daemon: {}", e);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Make many rapid requests to the health endpoint
    // Using X-Forwarded-For to simulate requests from external IP
    let num_requests = 20;
    let mut success_count = 0;
    let mut rate_limited_count = 0;
    let mut error_count = 0;

    println!(
        "Making {} rapid requests with X-Forwarded-For: {} to test rate limiting...",
        num_requests, EXTERNAL_TEST_IP
    );

    for i in 0..num_requests {
        let response = client
            .get(format!("{}/health", base_url))
            .header("X-Forwarded-For", EXTERNAL_TEST_IP)
            .send()
            .await;

        match response {
            Ok(resp) => {
                let status = resp.status();
                if status == StatusCode::OK {
                    success_count += 1;
                } else if status == StatusCode::TOO_MANY_REQUESTS {
                    rate_limited_count += 1;
                } else {
                    println!("Request {}: unexpected status {}", i, status);
                    error_count += 1;
                }
            }
            Err(e) => {
                println!("Request {} failed: {}", i, e);
                error_count += 1;
            }
        }
    }

    println!("\n=== Rate Limit Test Results ===");
    println!("  Total requests: {}", num_requests);
    println!("  Successful (200): {}", success_count);
    println!("  Rate limited (429): {}", rate_limited_count);
    println!("  Errors: {}", error_count);
    println!("================================\n");

    // We expect SOME requests to succeed (the first ones within burst)
    // and SOME to be rate limited (when limit exceeded)
    if rate_limited_count == 0 {
        executor.print_daemon_logs(150);
        panic!(
            "Rate limiting not working! Expected some 429 responses, but got:\n\
             - Success: {}\n\
             - Rate limited: {}\n\
             - Errors: {}",
            success_count, rate_limited_count, error_count
        );
    }

    assert!(
        success_count > 0,
        "Expected some successful requests, got none"
    );
    assert!(
        rate_limited_count > 0,
        "Expected some rate-limited requests, got none"
    );
    assert!(error_count == 0, "Expected no errors, got {}", error_count);

    println!("✓ Rate limiting is working correctly!");
}

/// Test rate limiting with concurrent requests from external IP
///
/// Uses X-Forwarded-For header to simulate concurrent requests from external IP.
/// This tests DDoS protection against concurrent attack patterns.
#[tokio::test]
async fn test_rate_limit_external_ip_concurrent_requests() {
    let mut executor = RateLimitTestExecutor::new();

    // Start daemon with low rate limit
    if let Err(e) = executor.start_daemon_with_rate_limit(3, 2).await {
        panic!("Failed to start daemon: {}", e);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Spawn many concurrent requests with X-Forwarded-For header
    let num_concurrent = 15;
    let mut handles = Vec::new();

    println!(
        "Spawning {} concurrent requests with X-Forwarded-For: {}...",
        num_concurrent, EXTERNAL_TEST_IP
    );

    for _ in 0..num_concurrent {
        let client = client.clone();
        let url = format!("{}/health", base_url);
        handles.push(tokio::spawn(async move {
            client
                .get(&url)
                .header("X-Forwarded-For", EXTERNAL_TEST_IP)
                .send()
                .await
        }));
    }

    // Collect results
    let mut success_count = 0;
    let mut rate_limited_count = 0;
    let mut error_count = 0;

    for handle in handles {
        match handle.await {
            Ok(Ok(resp)) => {
                let status = resp.status();
                if status == StatusCode::OK {
                    success_count += 1;
                } else if status == StatusCode::TOO_MANY_REQUESTS {
                    rate_limited_count += 1;
                } else {
                    error_count += 1;
                }
            }
            _ => {
                error_count += 1;
            }
        }
    }

    println!("\n=== Concurrent Rate Limit Test Results ===");
    println!("  Concurrent requests: {}", num_concurrent);
    println!("  Successful (200): {}", success_count);
    println!("  Rate limited (429): {}", rate_limited_count);
    println!("  Errors: {}", error_count);
    println!("==========================================\n");

    // With rate limit of 3/sec and burst of 2, concurrent requests should trigger limiting
    if rate_limited_count == 0 {
        executor.print_daemon_logs(50);
        panic!(
            "Rate limiting not working for concurrent requests!\n\
             Expected some 429 responses with {} concurrent requests",
            num_concurrent
        );
    }

    assert!(success_count > 0, "Expected some successful requests");
    assert!(
        rate_limited_count > 0,
        "Expected some rate-limited requests"
    );

    println!("✓ Rate limiting works correctly for concurrent requests!");
}

/// Test that high rate limit thresholds effectively allow all requests
///
/// Uses X-Forwarded-For but with very high limits, verifying that even
/// external IPs are not rate limited when thresholds are generous.
#[tokio::test]
async fn test_rate_limit_high_threshold_allows_requests() {
    let mut executor = RateLimitTestExecutor::new();

    // Test verifies high limits effectively allow all requests through

    // Start daemon with very high rate limit (effectively disabled)
    if let Err(e) = executor.start_daemon_with_rate_limit(10000, 1000).await {
        panic!("Failed to start daemon: {}", e);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Make many requests with X-Forwarded-For header
    // None should be rate limited with high limits
    let num_requests = 50;
    let mut success_count = 0;
    let mut rate_limited_count = 0;

    for _ in 0..num_requests {
        if let Ok(resp) = client
            .get(format!("{}/health", base_url))
            .header("X-Forwarded-For", EXTERNAL_TEST_IP)
            .send()
            .await
        {
            if resp.status() == StatusCode::OK {
                success_count += 1;
            } else if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                rate_limited_count += 1;
            }
        }
    }

    println!(
        "\n=== High Threshold Test Results (X-Forwarded-For: {}) ===",
        EXTERNAL_TEST_IP
    );
    println!("  Requests: {}", num_requests);
    println!("  Successful: {}", success_count);
    println!("  Rate limited: {}", rate_limited_count);
    println!("===========================================================\n");

    // With high limits, no requests should be rate limited
    assert_eq!(
        rate_limited_count, 0,
        "With high rate limits, no requests should be throttled"
    );
    assert_eq!(
        success_count, num_requests,
        "All requests should succeed with high limits"
    );

    println!("✓ High rate limits effectively disable throttling!");
}
