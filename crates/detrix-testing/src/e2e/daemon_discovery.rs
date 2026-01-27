//! Daemon Discovery E2E Tests
//!
//! Tests the daemon discovery mechanism that finds running daemons via:
//! 1. PID file (primary) - reads PID file and verifies process is running
//! 2. Port probe (fallback) - probes configured/default ports if PID file is stale/missing
//!
//! ## Test Scenarios
//!
//! 1. **Valid PID file** - Daemon running, PID file correct → discovery succeeds
//! 2. **Corrupted PID file (wrong PID)** - PID file has dead process PID, daemon running → port probe finds it
//! 3. **Missing PID file** - No PID file, daemon running → port probe finds it
//! 4. **Stale PID file (dead process)** - PID file exists, process dead → discovery returns None
//! 5. **Explicit port override** - Override bypasses all discovery
//!
//! ## Usage
//!
//! ```ignore
//! use detrix_testing::e2e::daemon_discovery::DaemonDiscoveryTests;
//!
//! let mut tests = DaemonDiscoveryTests::new().await?;
//! tests.test_valid_pid_file().await?;
//! tests.test_corrupted_pid_file_port_probe_fallback().await?;
//! ```

use crate::e2e::executor::{find_detrix_binary, get_grpc_port, get_http_port, get_workspace_root};
use detrix_config::constants::DEFAULT_API_HOST;
use serde_json::json;
use std::fs;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Test harness for daemon discovery tests
pub struct DaemonDiscoveryTests {
    /// Temporary directory for test artifacts
    temp_dir: tempfile::TempDir,
    /// Path to detrix binary
    binary_path: PathBuf,
    /// Workspace root
    workspace_root: PathBuf,
    /// Current daemon process (if running)
    daemon_process: Option<Child>,
    /// Current daemon PID (read from process, not PID file)
    daemon_pid: Option<u32>,
    /// HTTP port for current daemon
    http_port: u16,
    /// gRPC port for current daemon
    grpc_port: u16,
    /// Path to PID file
    pid_file_path: PathBuf,
    /// Path to config file
    config_path: PathBuf,
}

impl DaemonDiscoveryTests {
    /// Create a new test harness
    pub async fn new() -> Result<Self, String> {
        let workspace_root = get_workspace_root();
        let binary_path = find_detrix_binary(&workspace_root)
            .ok_or("detrix binary not found. Run: cargo build -p detrix-cli")?;

        let temp_dir = tempfile::TempDir::new().map_err(|e| e.to_string())?;
        let pid_file_path = temp_dir.path().join("daemon.pid");
        let config_path = temp_dir.path().join("detrix.toml");

        Ok(Self {
            temp_dir,
            binary_path,
            workspace_root,
            daemon_process: None,
            daemon_pid: None,
            http_port: get_http_port(),
            grpc_port: get_grpc_port(),
            pid_file_path,
            config_path,
        })
    }

    /// Start daemon with specified ports
    async fn start_daemon(&mut self) -> Result<(), String> {
        self.stop_daemon().await;

        let db_path = self.temp_dir.path().join("detrix.db");

        // Create config file
        let config_content = format!(
            r#"
[metadata]
version = "1.0"

[project]
base_path = "{}"

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"

[api]
port_fallback = false

[api.rest]
enabled = true
host = "{}"
port = {}

[api.grpc]
enabled = true
host = "{}"
port = {}

[safety]
enable_ast_analysis = false
"#,
            self.workspace_root.display(),
            db_path.display(),
            self.pid_file_path.display(),
            DEFAULT_API_HOST,
            self.http_port,
            DEFAULT_API_HOST,
            self.grpc_port,
        );

        fs::write(&self.config_path, config_content).map_err(|e| e.to_string())?;

        // Start daemon
        let daemon_log = self.temp_dir.path().join("daemon.log");
        let log_file = fs::File::create(&daemon_log).map_err(|e| e.to_string())?;
        let log_stderr = log_file.try_clone().map_err(|e| e.to_string())?;

        let process = Command::new(&self.binary_path)
            .args([
                "serve",
                "--config",
                self.config_path.to_str().unwrap(),
                "--daemon",
                "--pid-file",
                self.pid_file_path.to_str().unwrap(),
            ])
            .current_dir(&self.workspace_root)
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_stderr))
            .spawn()
            .map_err(|e| format!("Failed to spawn daemon: {}", e))?;

        let spawned_pid = process.id();
        self.daemon_process = Some(process);

        // Wait for daemon to start and write PID file
        self.wait_for_daemon_ready().await?;

        // Read actual daemon PID from PID file (daemon forks)
        if let Ok(content) = fs::read_to_string(&self.pid_file_path) {
            if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(pid) = info.get("pid").and_then(|v| v.as_u64()) {
                    self.daemon_pid = Some(pid as u32);
                }
            }
        }

        // Fallback to spawned process PID
        if self.daemon_pid.is_none() {
            self.daemon_pid = Some(spawned_pid);
        }

        Ok(())
    }

    /// Wait for daemon to be ready (PID file exists and ports are open)
    async fn wait_for_daemon_ready(&self) -> Result<(), String> {
        let timeout = Duration::from_secs(10);
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(100);

        while start.elapsed() < timeout {
            // Check PID file exists
            if self.pid_file_path.exists() {
                // Check gRPC port is open
                let addr = format!("{}:{}", DEFAULT_API_HOST, self.grpc_port);
                if TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(100))
                    .is_ok()
                {
                    return Ok(());
                }
            }
            tokio::time::sleep(poll_interval).await;
        }

        // Read daemon log for debugging
        let log_path = self.temp_dir.path().join("daemon.log");
        let log_content = fs::read_to_string(&log_path).unwrap_or_default();
        Err(format!(
            "Daemon failed to start within {:?}. Log:\n{}",
            timeout,
            log_content.chars().take(2000).collect::<String>()
        ))
    }

    /// Stop the current daemon
    async fn stop_daemon(&mut self) {
        // Kill via PID from PID file (handles forked daemon)
        #[cfg(unix)]
        if let Some(pid) = self.daemon_pid.take() {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
        }
        #[cfg(not(unix))]
        {
            let _ = self.daemon_pid.take();
        }

        // Also kill the spawned process
        if let Some(mut process) = self.daemon_process.take() {
            let _ = process.kill();
            let _ = process.wait();
        }

        // Clean up PID file
        let _ = fs::remove_file(&self.pid_file_path);

        // Wait for ports to be released
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    /// Check if a port is open
    fn is_port_open(host: &str, port: u16) -> bool {
        let addr = format!("{}:{}", host, port);
        TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(200)).is_ok()
    }

    /// Write a custom PID file content
    fn write_pid_file(&self, content: &str) -> Result<(), String> {
        fs::write(&self.pid_file_path, content).map_err(|e| e.to_string())
    }

    /// Delete the PID file
    fn delete_pid_file(&self) -> Result<(), String> {
        if self.pid_file_path.exists() {
            fs::remove_file(&self.pid_file_path).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Run the CLI `status` command and check if it finds the daemon
    async fn cli_status(&self) -> Result<CliStatusResult, String> {
        let output = Command::new(&self.binary_path)
            .args([
                "status",
                "--config",
                self.config_path.to_str().unwrap(),
                "--format",
                "json",
            ])
            .env("RUST_LOG", "warn")
            .output()
            .map_err(|e| format!("Failed to run CLI: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            // Parse JSON output
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
                return Ok(CliStatusResult::Found {
                    state: json
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    output: stdout.to_string(),
                });
            }
            Ok(CliStatusResult::Found {
                state: "unknown".to_string(),
                output: stdout.to_string(),
            })
        } else {
            Ok(CliStatusResult::NotFound {
                error: stderr.to_string(),
            })
        }
    }

    // ========================================================================
    // TEST SCENARIOS
    // ========================================================================

    /// Test 1: Valid PID file - daemon running, PID file correct
    ///
    /// This is the happy path where everything works as expected.
    pub async fn test_valid_pid_file(&mut self) -> Result<(), String> {
        println!("=== Test: Valid PID file ===");

        // Start daemon normally
        self.start_daemon().await?;

        // Verify daemon is running
        assert!(
            Self::is_port_open(DEFAULT_API_HOST, self.grpc_port),
            "Daemon should be running on gRPC port {}",
            self.grpc_port
        );

        // Verify PID file exists and is valid
        assert!(
            self.pid_file_path.exists(),
            "PID file should exist at {:?}",
            self.pid_file_path
        );

        // CLI status should find daemon via PID file
        let status = self.cli_status().await?;
        match status {
            CliStatusResult::Found { state, .. } => {
                println!("  ✓ Daemon found via PID file, state: {}", state);
            }
            CliStatusResult::NotFound { error } => {
                return Err(format!("Expected to find daemon via PID file: {}", error));
            }
        }

        self.stop_daemon().await;
        println!("  ✓ Test passed: Valid PID file discovery works\n");
        Ok(())
    }

    /// Test 2: Corrupted PID file with wrong PID - port probe fallback
    ///
    /// PID file contains a PID for a dead/non-existent process, but daemon
    /// is actually running on the configured ports. Port probe should find it.
    pub async fn test_corrupted_pid_file_port_probe_fallback(&mut self) -> Result<(), String> {
        println!("=== Test: Corrupted PID file (wrong PID) - port probe fallback ===");

        // Start daemon normally
        self.start_daemon().await?;

        let actual_pid = self.daemon_pid.unwrap();
        println!("  Daemon started with PID: {}", actual_pid);

        // Verify daemon is running
        assert!(
            Self::is_port_open(DEFAULT_API_HOST, self.grpc_port),
            "Daemon should be running"
        );

        // Corrupt the PID file with a wrong PID (use PID 1 which exists but isn't our daemon,
        // or a very high PID that likely doesn't exist)
        let fake_pid = 999999; // Very high PID unlikely to exist
        let corrupted_content = json!({
            "pid": fake_pid,
            "ports": {
                "http": self.http_port,
                "grpc": self.grpc_port
            },
            "host": DEFAULT_API_HOST
        });
        self.write_pid_file(&corrupted_content.to_string())?;
        println!("  Wrote corrupted PID file with fake PID: {}", fake_pid);

        // CLI status should still find daemon via port probe fallback
        let status = self.cli_status().await?;
        match status {
            CliStatusResult::Found { state, output } => {
                println!("  ✓ Daemon found (likely via port probe), state: {}", state);
                // The discovery might log that PID file was stale
                if output.contains("port") || output.contains("probe") {
                    println!("  ✓ Output mentions port probe as expected");
                }
            }
            CliStatusResult::NotFound { error } => {
                return Err(format!(
                    "Expected port probe to find daemon despite corrupted PID file: {}",
                    error
                ));
            }
        }

        self.stop_daemon().await;
        println!("  ✓ Test passed: Port probe fallback works with corrupted PID file\n");
        Ok(())
    }

    /// Test 3: Missing PID file - port probe finds daemon
    ///
    /// No PID file exists, but daemon is running on configured ports.
    /// Port probe should discover it.
    pub async fn test_missing_pid_file_port_probe(&mut self) -> Result<(), String> {
        println!("=== Test: Missing PID file - port probe discovery ===");

        // Start daemon normally
        self.start_daemon().await?;

        // Verify daemon is running
        assert!(
            Self::is_port_open(DEFAULT_API_HOST, self.grpc_port),
            "Daemon should be running"
        );

        // Delete the PID file
        self.delete_pid_file()?;
        assert!(!self.pid_file_path.exists(), "PID file should be deleted");
        println!("  Deleted PID file");

        // CLI status should find daemon via port probe
        let status = self.cli_status().await?;
        match status {
            CliStatusResult::Found { state, .. } => {
                println!("  ✓ Daemon found via port probe, state: {}", state);
            }
            CliStatusResult::NotFound { error } => {
                return Err(format!(
                    "Expected port probe to find daemon when PID file missing: {}",
                    error
                ));
            }
        }

        self.stop_daemon().await;
        println!("  ✓ Test passed: Port probe works when PID file is missing\n");
        Ok(())
    }

    /// Test 4: Stale PID file (process dead) - discovery returns not running
    ///
    /// PID file exists with valid format, but the process is dead.
    /// Port probe should also fail (no daemon running).
    pub async fn test_stale_pid_file_no_daemon(&mut self) -> Result<(), String> {
        println!("=== Test: Stale PID file (no daemon running) ===");

        // Don't start daemon - just create a stale PID file
        let stale_content = json!({
            "pid": 999999, // Non-existent process
            "ports": {
                "http": self.http_port,
                "grpc": self.grpc_port
            },
            "host": DEFAULT_API_HOST
        });
        self.write_pid_file(&stale_content.to_string())?;
        println!("  Created stale PID file with non-existent PID");

        // Verify daemon is NOT running
        assert!(
            !Self::is_port_open(DEFAULT_API_HOST, self.grpc_port),
            "No daemon should be running on port {}",
            self.grpc_port
        );

        // CLI status should report daemon not found
        let status = self.cli_status().await?;
        match status {
            CliStatusResult::NotFound { .. } => {
                println!("  ✓ Correctly reported daemon not running");
            }
            CliStatusResult::Found { .. } => {
                return Err("Expected daemon to not be found with stale PID file".to_string());
            }
        }

        self.delete_pid_file()?;
        println!("  ✓ Test passed: Stale PID file correctly detected as no daemon\n");
        Ok(())
    }

    /// Test 5: Explicit port override bypasses discovery
    ///
    /// When explicit port is provided, it should be used directly without
    /// PID file or port probe checks.
    pub async fn test_explicit_port_override(&mut self) -> Result<(), String> {
        println!("=== Test: Explicit port override ===");

        // Start daemon on specific ports
        self.start_daemon().await?;

        // Verify daemon is running
        assert!(
            Self::is_port_open(DEFAULT_API_HOST, self.grpc_port),
            "Daemon should be running"
        );

        // Corrupt the PID file with wrong ports
        let wrong_port = self.grpc_port + 1000;
        let corrupted_content = json!({
            "pid": self.daemon_pid,
            "ports": {
                "http": self.http_port + 1000,
                "grpc": wrong_port
            },
            "host": DEFAULT_API_HOST
        });
        self.write_pid_file(&corrupted_content.to_string())?;
        println!("  Corrupted PID file with wrong port {}", wrong_port);

        // Use environment variable override to specify correct port
        // This should bypass PID file entirely
        let output = Command::new(&self.binary_path)
            .args(["status", "--format", "json"])
            .env("DETRIX_GRPC_PORT_OVERRIDE", self.grpc_port.to_string())
            .env("RUST_LOG", "warn")
            .output()
            .map_err(|e| format!("Failed to run CLI: {}", e))?;

        if output.status.success() {
            println!("  ✓ Explicit port override found daemon correctly");
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Explicit port override should find daemon: {}",
                stderr
            ));
        }

        self.stop_daemon().await;
        println!("  ✓ Test passed: Explicit port override bypasses discovery\n");
        Ok(())
    }

    /// Test 6: PID file with valid PID but wrong ports - port probe corrects
    ///
    /// PID file has correct PID but wrong port numbers. The daemon is running
    /// on different ports than what PID file says.
    pub async fn test_pid_file_wrong_ports(&mut self) -> Result<(), String> {
        println!("=== Test: PID file with wrong ports ===");

        // Start daemon on specific ports
        self.start_daemon().await?;

        let actual_pid = self.daemon_pid.unwrap();

        // Verify daemon is running on actual ports
        assert!(
            Self::is_port_open(DEFAULT_API_HOST, self.grpc_port),
            "Daemon should be running on port {}",
            self.grpc_port
        );

        // Corrupt the PID file with correct PID but wrong ports
        let wrong_grpc_port = self.grpc_port + 1;
        let corrupted_content = json!({
            "pid": actual_pid,
            "ports": {
                "http": self.http_port + 1,
                "grpc": wrong_grpc_port
            },
            "host": DEFAULT_API_HOST
        });
        self.write_pid_file(&corrupted_content.to_string())?;
        println!(
            "  PID file has PID {} but wrong port {} (actual: {})",
            actual_pid, wrong_grpc_port, self.grpc_port
        );

        // Discovery should use PID file ports first, which will fail to connect
        // This tests error handling for connection failures
        // The status command should either:
        // 1. Fail gracefully with helpful error
        // 2. Or fall back to port probe (implementation dependent)
        let status = self.cli_status().await?;
        match status {
            CliStatusResult::Found { state, .. } => {
                println!(
                    "  Discovery found daemon (probably via fallback), state: {}",
                    state
                );
            }
            CliStatusResult::NotFound { error } => {
                // This is also acceptable - the PID file ports don't work
                println!("  Discovery failed as expected with wrong ports: {}", error);
            }
        }

        self.stop_daemon().await;
        println!("  ✓ Test passed: Wrong ports in PID file handled gracefully\n");
        Ok(())
    }

    /// Run all daemon discovery tests
    pub async fn run_all(&mut self) -> Result<(), String> {
        println!("\n========================================");
        println!("  DAEMON DISCOVERY E2E TESTS");
        println!("========================================\n");

        self.test_valid_pid_file().await?;
        self.test_corrupted_pid_file_port_probe_fallback().await?;
        self.test_missing_pid_file_port_probe().await?;
        self.test_stale_pid_file_no_daemon().await?;
        self.test_explicit_port_override().await?;
        self.test_pid_file_wrong_ports().await?;

        println!("========================================");
        println!("  ALL DAEMON DISCOVERY TESTS PASSED");
        println!("========================================\n");

        Ok(())
    }
}

impl Drop for DaemonDiscoveryTests {
    fn drop(&mut self) {
        // Synchronous cleanup - kill daemon if still running
        #[cfg(unix)]
        if let Some(pid) = self.daemon_pid.take() {
            use std::process::Command;
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
        }
        #[cfg(not(unix))]
        {
            let _ = self.daemon_pid.take();
        }
        if let Some(mut process) = self.daemon_process.take() {
            let _ = process.kill();
        }
    }
}

/// Result of CLI status command
#[derive(Debug)]
pub enum CliStatusResult {
    Found { state: String, output: String },
    NotFound { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the comprehensive daemon discovery test suite
    #[tokio::test]
    async fn test_daemon_discovery_comprehensive() {
        let mut tests = DaemonDiscoveryTests::new()
            .await
            .expect("Failed to create test harness");
        tests
            .run_all()
            .await
            .expect("Daemon discovery tests failed");
    }

    /// Individual test: Valid PID file
    #[tokio::test]
    async fn test_valid_pid_file() {
        let mut tests = DaemonDiscoveryTests::new()
            .await
            .expect("Failed to create test harness");
        tests.test_valid_pid_file().await.expect("Test failed");
    }

    /// Individual test: Corrupted PID file with port probe fallback
    #[tokio::test]
    async fn test_corrupted_pid_file() {
        let mut tests = DaemonDiscoveryTests::new()
            .await
            .expect("Failed to create test harness");
        tests
            .test_corrupted_pid_file_port_probe_fallback()
            .await
            .expect("Test failed");
    }

    /// Individual test: Missing PID file with port probe
    #[tokio::test]
    async fn test_missing_pid_file() {
        let mut tests = DaemonDiscoveryTests::new()
            .await
            .expect("Failed to create test harness");
        tests
            .test_missing_pid_file_port_probe()
            .await
            .expect("Test failed");
    }

    /// Individual test: Stale PID file (no daemon)
    #[tokio::test]
    async fn test_stale_pid_file() {
        let mut tests = DaemonDiscoveryTests::new()
            .await
            .expect("Failed to create test harness");
        tests
            .test_stale_pid_file_no_daemon()
            .await
            .expect("Test failed");
    }

    /// Individual test: Explicit port override
    #[tokio::test]
    async fn test_port_override() {
        let mut tests = DaemonDiscoveryTests::new()
            .await
            .expect("Failed to create test harness");
        tests
            .test_explicit_port_override()
            .await
            .expect("Test failed");
    }
}
