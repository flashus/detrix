//! E2E tests for localhost rate limit exemption
//!
//! Tests that localhost connections are properly exempted from rate limiting
//! when `localhost_exempt = true` (the default), while ensuring remote connections
//! are still rate limited.
//!
//! These tests verify the ConditionalRateLimitLayer behavior in production conditions.

use detrix_testing::e2e::executor::find_detrix_binary;
use reqwest::StatusCode;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tempfile::TempDir;

/// Global port counter to ensure each test gets a unique port
static PORT_COUNTER: AtomicU16 = AtomicU16::new(100);

/// Test executor specialized for localhost exemption testing
struct LocalhostExemptTestExecutor {
    temp_dir: TempDir,
    http_port: u16,
    daemon_process: Option<Child>,
    daemon_log_path: PathBuf,
    workspace_root: PathBuf,
}

impl LocalhostExemptTestExecutor {
    fn new() -> Self {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| manifest_dir.clone());

        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Use a unique port for this test
        let counter = PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
        let http_port = 19500 + ((std::process::id() as u16 + counter) % 500);

        let daemon_log_path = temp_dir.path().join("daemon.log");

        Self {
            temp_dir,
            http_port,
            daemon_process: None,
            daemon_log_path,
            workspace_root,
        }
    }

    /// Start daemon with localhost_exempt setting
    async fn start_daemon(
        &mut self,
        per_second: u64,
        burst_size: u32,
        localhost_exempt: bool,
    ) -> Result<(), String> {
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
localhost_exempt = {}

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
            localhost_exempt,
        );

        let config_path = self.temp_dir.path().join("detrix.toml");
        std::fs::write(&config_path, config_content).map_err(|e| e.to_string())?;

        // Find binary
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

impl Drop for LocalhostExemptTestExecutor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Wait for HTTP server to be ready
async fn wait_for_port(port: u16, timeout_secs: u64) -> bool {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("Failed to create HTTP client");

    let url = format!("http://127.0.0.1:{}/health", port);

    while start.elapsed() < timeout {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 429 => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                return true;
            }
            _ => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    false
}

// ============================================================================
// Localhost Exemption Tests
// ============================================================================

/// Test that localhost is NOT rate limited when localhost_exempt = true (default)
///
/// This test makes many rapid requests from localhost and verifies that
/// ALL of them succeed (no 429 responses).
#[tokio::test]
async fn test_localhost_exempt_allows_unlimited_requests() {
    let mut executor = LocalhostExemptTestExecutor::new();

    // Start daemon with very restrictive rate limit but localhost_exempt = true
    // Rate limit: 2 requests per second, burst of 1
    // Without exemption, this would quickly result in 429s
    if let Err(e) = executor.start_daemon(2, 1, true).await {
        panic!("Failed to start daemon: {}", e);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Make many rapid requests - ALL should succeed because we're from localhost
    let num_requests = 100;
    let mut success_count = 0;
    let mut rate_limited_count = 0;
    let mut error_count = 0;

    println!(
        "Making {} rapid requests from localhost (exempt)...",
        num_requests
    );

    for _ in 0..num_requests {
        match client.get(format!("{}/health", base_url)).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status == StatusCode::OK {
                    success_count += 1;
                } else if status == StatusCode::TOO_MANY_REQUESTS {
                    rate_limited_count += 1;
                } else {
                    error_count += 1;
                }
            }
            Err(_) => {
                error_count += 1;
            }
        }
    }

    println!("\n=== Localhost Exempt Test Results ===");
    println!("  Total requests: {}", num_requests);
    println!("  Successful (200): {}", success_count);
    println!("  Rate limited (429): {}", rate_limited_count);
    println!("  Errors: {}", error_count);
    println!("=====================================\n");

    // ALL requests from localhost should succeed when localhost_exempt = true
    if rate_limited_count > 0 {
        executor.print_daemon_logs(100);
        panic!(
            "Localhost should be exempt from rate limiting!\n\
             Expected 0 rate-limited requests, got {}",
            rate_limited_count
        );
    }

    assert_eq!(
        success_count, num_requests,
        "All localhost requests should succeed when exempt"
    );
    assert_eq!(
        rate_limited_count, 0,
        "Localhost should never be rate limited when exempt"
    );
    assert_eq!(error_count, 0, "No errors expected");

    println!(
        "✓ Localhost exemption working correctly - {} requests succeeded!",
        num_requests
    );
}

/// Test that localhost IS rate limited when localhost_exempt = false
///
/// This test makes many rapid requests from localhost with exemption disabled
/// and verifies that rate limiting is applied with proper error response.
#[tokio::test]
async fn test_localhost_not_exempt_is_rate_limited() {
    let mut executor = LocalhostExemptTestExecutor::new();

    // Start daemon with restrictive rate limit and localhost_exempt = false
    if let Err(e) = executor.start_daemon(3, 2, false).await {
        panic!("Failed to start daemon: {}", e);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    let num_requests = 20;
    let mut success_count = 0;
    let mut rate_limited_count = 0;
    let mut error_count = 0;
    let mut has_retry_after = false;

    println!(
        "Making {} rapid requests from localhost (NOT exempt)...",
        num_requests
    );

    for _ in 0..num_requests {
        match client.get(format!("{}/health", base_url)).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status == StatusCode::OK {
                    success_count += 1;
                } else if status == StatusCode::TOO_MANY_REQUESTS {
                    rate_limited_count += 1;
                    // Check for Retry-After header on 429 responses
                    if resp.headers().contains_key("retry-after") {
                        has_retry_after = true;
                    }
                } else {
                    error_count += 1;
                }
            }
            Err(_) => {
                error_count += 1;
            }
        }
    }

    println!("\n=== Localhost NOT Exempt Test Results ===");
    println!("  Total requests: {}", num_requests);
    println!("  Successful (200): {}", success_count);
    println!("  Rate limited (429): {}", rate_limited_count);
    println!("  Has Retry-After header: {}", has_retry_after);
    println!("  Errors: {}", error_count);
    println!("=========================================\n");

    // When localhost_exempt = false, localhost SHOULD be rate limited
    if rate_limited_count == 0 {
        executor.print_daemon_logs(100);
        panic!(
            "Localhost should be rate limited when localhost_exempt = false!\n\
             Expected some 429 responses, got none"
        );
    }

    assert!(success_count > 0, "Some requests should succeed");
    assert!(
        rate_limited_count > 0,
        "Localhost should be rate limited when exempt = false"
    );
    assert_eq!(error_count, 0, "No errors expected");
    // Note: Retry-After header depends on tower-governor configuration

    println!("✓ Localhost rate limiting working correctly when exempt = false!");
}

/// Test concurrent requests from localhost with exemption enabled
///
/// This test spawns many concurrent requests and verifies ALL succeed.
#[tokio::test]
async fn test_localhost_exempt_concurrent_requests() {
    let mut executor = LocalhostExemptTestExecutor::new();

    // Start daemon with very restrictive rate limit but localhost_exempt = true
    if let Err(e) = executor.start_daemon(1, 1, true).await {
        panic!("Failed to start daemon: {}", e);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Spawn many concurrent requests
    let num_concurrent = 50;
    let mut handles = Vec::new();

    println!(
        "Spawning {} concurrent requests from localhost (exempt)...",
        num_concurrent
    );

    for _ in 0..num_concurrent {
        let client = client.clone();
        let url = format!("{}/health", base_url);
        handles.push(tokio::spawn(async move { client.get(&url).send().await }));
    }

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

    println!("\n=== Concurrent Localhost Exempt Test Results ===");
    println!("  Concurrent requests: {}", num_concurrent);
    println!("  Successful (200): {}", success_count);
    println!("  Rate limited (429): {}", rate_limited_count);
    println!("  Errors: {}", error_count);
    println!("================================================\n");

    // ALL concurrent requests should succeed when localhost_exempt = true
    if rate_limited_count > 0 {
        executor.print_daemon_logs(100);
        panic!(
            "Concurrent localhost requests should not be rate limited!\n\
             Got {} rate-limited responses",
            rate_limited_count
        );
    }

    assert_eq!(
        success_count, num_concurrent,
        "All concurrent localhost requests should succeed"
    );
    assert_eq!(rate_limited_count, 0, "No rate limiting for localhost");

    println!("✓ Concurrent localhost exemption working correctly!");
}

/// Test that rate limiting still works for different endpoints
///
/// This test verifies that localhost exemption works across different API endpoints.
#[tokio::test]
async fn test_localhost_exempt_different_endpoints() {
    let mut executor = LocalhostExemptTestExecutor::new();

    // Start daemon with restrictive rate limit but localhost_exempt = true
    if let Err(e) = executor.start_daemon(2, 1, true).await {
        panic!("Failed to start daemon: {}", e);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Test multiple endpoints
    let endpoints = vec![
        "/health",
        "/api/v1/metrics",
        "/api/v1/connections",
        "/status",
    ];

    let requests_per_endpoint = 10;
    let mut total_success = 0;
    let mut total_rate_limited = 0;

    println!(
        "Testing {} endpoints with {} requests each...",
        endpoints.len(),
        requests_per_endpoint
    );

    for endpoint in &endpoints {
        for _ in 0..requests_per_endpoint {
            match client.get(format!("{}{}", base_url, endpoint)).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    // Accept both 200 and 404 (endpoint may not exist) as "not rate limited"
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        total_rate_limited += 1;
                    } else {
                        total_success += 1;
                    }
                }
                Err(_) => {}
            }
        }
    }

    println!("\n=== Multi-Endpoint Test Results ===");
    println!(
        "  Total requests: {}",
        endpoints.len() * requests_per_endpoint
    );
    println!("  Not rate limited: {}", total_success);
    println!("  Rate limited (429): {}", total_rate_limited);
    println!("===================================\n");

    // No requests should be rate limited from localhost
    assert_eq!(
        total_rate_limited, 0,
        "No endpoint should be rate limited for localhost"
    );

    println!("✓ Localhost exemption works across all endpoints!");
}

/// Test sustained load from localhost
///
/// This test makes requests over a period of time to verify sustained load handling.
#[tokio::test]
async fn test_localhost_exempt_sustained_load() {
    let mut executor = LocalhostExemptTestExecutor::new();

    // Start daemon with very low rate limit but localhost_exempt = true
    if let Err(e) = executor.start_daemon(1, 1, true).await {
        panic!("Failed to start daemon: {}", e);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Make requests over 3 seconds (longer than rate limit window)
    let duration = Duration::from_secs(3);
    let start = std::time::Instant::now();
    let mut request_count = 0;
    let mut success_count = 0;
    let mut rate_limited_count = 0;

    println!("Making sustained requests for {:?}...", duration);

    while start.elapsed() < duration {
        match client.get(format!("{}/health", base_url)).send().await {
            Ok(resp) => {
                request_count += 1;
                if resp.status() == StatusCode::OK {
                    success_count += 1;
                } else if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                    rate_limited_count += 1;
                }
            }
            Err(_) => {}
        }
        // Small delay between requests
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    println!("\n=== Sustained Load Test Results ===");
    println!("  Duration: {:?}", duration);
    println!("  Total requests: {}", request_count);
    println!("  Successful (200): {}", success_count);
    println!("  Rate limited (429): {}", rate_limited_count);
    println!("===================================\n");

    // No rate limiting should occur for localhost
    assert_eq!(
        rate_limited_count, 0,
        "Localhost should never be rate limited even under sustained load"
    );
    assert!(
        request_count > 50,
        "Should have made many requests during the test period"
    );

    println!(
        "✓ Localhost handled {} sustained requests without rate limiting!",
        request_count
    );
}

// ============================================================================
// IPv6 Localhost Tests
// ============================================================================

/// Test IPv6 localhost exemption
///
/// This test verifies that IPv6 localhost (::1) is also exempt.
/// Note: This test may be skipped if IPv6 is not available on the system.
#[tokio::test]
async fn test_ipv6_localhost_exempt() {
    let mut executor = LocalhostExemptTestExecutor::new();

    // Start daemon with restrictive rate limit but localhost_exempt = true
    if let Err(e) = executor.start_daemon(2, 1, true).await {
        panic!("Failed to start daemon: {}", e);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    // Try to connect via IPv6 localhost
    let ipv6_url = format!("http://[::1]:{}/health", executor.http_port);

    let mut success_count = 0;
    let mut rate_limited_count = 0;
    let mut connection_errors = 0;

    println!("Testing IPv6 localhost (::1) exemption...");

    for _ in 0..20 {
        match client.get(&ipv6_url).send().await {
            Ok(resp) => {
                if resp.status() == StatusCode::OK {
                    success_count += 1;
                } else if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                    rate_limited_count += 1;
                }
            }
            Err(_) => {
                connection_errors += 1;
            }
        }
    }

    println!("\n=== IPv6 Localhost Test Results ===");
    println!("  Successful (200): {}", success_count);
    println!("  Rate limited (429): {}", rate_limited_count);
    println!("  Connection errors: {}", connection_errors);
    println!("===================================\n");

    // If we got connection errors, IPv6 might not be available - skip the test
    if connection_errors == 20 {
        println!("⚠ Skipping IPv6 test - IPv6 not available on this system");
        return;
    }

    // If we connected successfully, verify no rate limiting
    assert_eq!(
        rate_limited_count, 0,
        "IPv6 localhost should be exempt from rate limiting"
    );

    println!("✓ IPv6 localhost exemption working correctly!");
}
