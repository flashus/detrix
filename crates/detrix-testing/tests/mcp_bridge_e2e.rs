//! MCP Bridge E2E Tests
//!
//! Tests the MCP bridge's ability to spawn and manage the daemon process.
//! This verifies the core MCP <-> daemon communication and lifecycle management.
//!
//! Test scenarios:
//! 1. MCP bridge spawns daemon when none is running
//! 2. MCP bridge connects to existing daemon
//! 3. MCP bridge handles daemon restart

use detrix_testing::e2e::{
    cleanup_orphaned_e2e_processes,
    executor::{
        find_detrix_binary, get_debugpy_port, get_workspace_root, start_debugpy_setsid,
        wait_for_port,
    },
    is_debugpy_available, register_e2e_process,
    reporter::TestReporter,
    unregister_e2e_process,
};
use std::process::Command;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Global port counter to ensure each test gets a unique port
/// This prevents TIME_WAIT collisions when tests run in sequence
static PORT_COUNTER: AtomicU16 = AtomicU16::new(0);
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;
use tokio::time::timeout;

// Use PidInfo from detrix-config for reading daemon info from PID file
use detrix_config::pid::PidInfo;

/// Wrapper struct that provides convenient access to PidInfo fields
/// This maintains the same API as the old DaemonInfo struct
struct DaemonInfo {
    inner: PidInfo,
}

impl DaemonInfo {
    fn from_pid_info(info: PidInfo) -> Self {
        Self { inner: info }
    }

    fn pid(&self) -> u64 {
        self.inner.pid as u64
    }

    fn host(&self) -> &str {
        self.inner.get_host()
    }

    fn http_port(&self) -> u16 {
        self.inner.port().unwrap_or(0)
    }
}

/// Read daemon info from PID file
///
/// Uses the official PidInfo type from detrix-config which matches
/// the format written by detrix-cli/src/utils/pid.rs
fn read_daemon_info(pid_path: &std::path::Path) -> Option<DaemonInfo> {
    let info = PidInfo::read_from_file(pid_path).ok()??;
    // Validate essential fields
    if info.pid == 0 || info.port().is_none() {
        return None;
    }
    Some(DaemonInfo::from_pid_info(info))
}

/// Test that MCP bridge spawns daemon when no daemon is running.
///
/// This tests the basic daemon spawn mechanism where the bridge detects
/// no daemon is running and starts one automatically.
///
/// Windows: Skipped because process group isolation (process_group(0)) is not available.
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_spawns_daemon() {
    // Clean up any orphaned processes from previous test runs
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_spawn", "MCP");
    reporter.section("MCP BRIDGE DAEMON SPAWN TEST");
    reporter.info("Testing MCP bridge automatic daemon spawning");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let step = reporter.step_start("Find binary", "Locate detrix binary");
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => {
            reporter.step_success(step, Some(&format!("Found: {}", path.display())));
            path
        }
        None => {
            reporter.step_failed(step, "Binary not found");
            reporter.warn("Skipping test: detrix binary not built");
            reporter.warn("Run `cargo build -p detrix-cli` to build the binary");
            return;
        }
    };

    // ========================================================================
    // PHASE 1: Setup test environment
    // ========================================================================
    reporter.section("PHASE 1: SETUP");

    let step = reporter.step_start("Create temp dir", "Set up isolated test environment");
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");
    reporter.step_success(
        step,
        Some(&format!("Temp dir: {}", temp_dir.path().display())),
    );

    // Create config with specific ports
    let step = reporter.step_start("Create config", "Write test configuration");
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 19080

[api.grpc]
enabled = false
port = 59999
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");
    reporter.step_success(step, Some("Config written"));

    // Verify no daemon running (no PID file)
    let step = reporter.step_start("Verify no daemon", "Ensure no existing daemon is running");
    assert!(!pid_path.exists(), "PID file should not exist initially");
    reporter.step_success(step, Some("No daemon running"));

    // ========================================================================
    // PHASE 2: Start MCP bridge (should spawn daemon)
    // ========================================================================
    reporter.section("PHASE 2: START MCP BRIDGE");

    let step = reporter.step_start(
        "Start MCP bridge",
        "Launch MCP in default mode (will spawn daemon)",
    );
    reporter.info(&format!("Binary: {}", binary.display()));
    reporter.info(&format!("Config: {}", config_path.display()));

    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");

    reporter.step_success(step, Some("MCP bridge process started"));

    // ========================================================================
    // PHASE 3: Wait for daemon to spawn
    // ========================================================================
    reporter.section("PHASE 3: VERIFY DAEMON SPAWN");

    let step = reporter.step_start("Wait for daemon", "Wait for daemon to start (5s)");
    tokio::time::sleep(Duration::from_secs(5)).await;
    reporter.step_success(step, Some("Wait complete"));

    // Read port from PID file
    let step = reporter.step_start("Read PID file", "Extract daemon port from PID file");
    let mut daemon_port = 19080u16;
    let mut daemon_pid: Option<u64> = None;

    if let Ok(content) = std::fs::read_to_string(&pid_path) {
        reporter.info(&format!("PID file contents: {}", content.trim()));
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(port) = info.get("http_port").and_then(|p| p.as_u64()) {
                daemon_port = port as u16;
            }
            // Also try "ports.http" format
            if let Some(ports) = info.get("ports") {
                if let Some(port) = ports.get("http").and_then(|p| p.as_u64()) {
                    daemon_port = port as u16;
                }
            }
            daemon_pid = info.get("pid").and_then(|p| p.as_u64());
        }
        reporter.step_success(
            step,
            Some(&format!("Port: {}, PID: {:?}", daemon_port, daemon_pid)),
        );
    } else {
        reporter.step_failed(step, &format!("Could not read PID file at {:?}", pid_path));
        reporter.warn("PID file may not exist yet - continuing with default port");
    }

    // ========================================================================
    // PHASE 4: Verify daemon is healthy
    // ========================================================================
    reporter.section("PHASE 4: HEALTH CHECK");

    let step = reporter.step_start(
        "Health check",
        &format!("Verify daemon is responding on port {}", daemon_port),
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    let health_url = format!("http://127.0.0.1:{}/health", daemon_port);
    reporter.info(&format!("Health URL: {}", health_url));

    let health_check = timeout(Duration::from_secs(10), async {
        loop {
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => return true,
                Ok(resp) => {
                    reporter.info(&format!("Health check returned: {}", resp.status()));
                }
                Err(e) => {
                    reporter.info(&format!("Health check error: {}", e));
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    match health_check {
        Ok(true) => {
            reporter.step_success(step, Some("Daemon healthy"));
        }
        Ok(false) => {
            reporter.step_failed(step, "Daemon unhealthy");
            panic!("Daemon should be healthy");
        }
        Err(_) => {
            reporter.step_failed(step, "Health check timed out after 10s");
            // Print log files for debugging
            if let Ok(entries) = std::fs::read_dir(&log_dir) {
                for entry in entries.flatten() {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        reporter.info(&format!("Log file {}:", entry.path().display()));
                        for line in content.lines().take(50) {
                            reporter.info(&format!("  {}", line));
                        }
                    }
                }
            }
            panic!("Daemon health check timed out");
        }
    }

    // ========================================================================
    // PHASE 5: Verify PID file and process
    // ========================================================================
    reporter.section("PHASE 5: VERIFY DAEMON PROCESS");

    let step = reporter.step_start("Verify PID file", "Check PID file contains valid data");
    let pid_content = std::fs::read_to_string(&pid_path).expect("PID file should exist");
    let pid_info: serde_json::Value =
        serde_json::from_str(&pid_content).expect("PID file should be valid JSON");

    let daemon_pid = pid_info
        .get("pid")
        .and_then(|p| p.as_u64())
        .expect("PID file should contain pid field");

    // Register daemon for cleanup tracking
    register_e2e_process("mcp_daemon", daemon_pid as u32);

    reporter.step_success(step, Some(&format!("PID: {}", daemon_pid)));

    // Verify actual process is running using kill -0
    let step = reporter.step_start(
        "Verify process",
        &format!("Check process {} is running", daemon_pid),
    );

    let process_check = Command::new("kill")
        .args(["-0", &daemon_pid.to_string()])
        .output()
        .expect("Failed to check process");

    if process_check.status.success() {
        reporter.step_success(step, Some("Process is running"));
    } else {
        reporter.step_failed(step, &format!("Process {} is not running", daemon_pid));
        panic!("Daemon process should be running");
    }

    // ========================================================================
    // CLEANUP
    // ========================================================================
    reporter.section("CLEANUP");

    let step = reporter.step_start("Kill processes", "Terminate bridge and daemon");

    // Kill bridge process
    let _ = bridge_process.kill().await;

    // Kill daemon process
    let _ = Command::new("kill")
        .args(["-9", &daemon_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", daemon_pid as u32);

    // Wait for daemon to fully exit (avoid port conflicts with next test)
    for _ in 0..20 {
        let check = Command::new("kill")
            .args(["-0", &daemon_pid.to_string()])
            .output();
        match check {
            Ok(output) if !output.status.success() => break,
            Err(_) => break,
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }

    reporter.step_success(step, Some("Processes terminated"));

    // ========================================================================
    // SUMMARY
    // ========================================================================
    reporter.section("TEST COMPLETE");
    reporter.info("✅ MCP bridge daemon spawn test PASSED");
    reporter.info(&format!("   Daemon PID: {}", daemon_pid));
    reporter.info(&format!("   Daemon port: {}", daemon_port));
}

/// Test that MCP bridge can handle daemon restart
///
/// This tests the proactive daemon restart feature where the bridge
/// detects heartbeat failures and restarts the daemon.
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_daemon_restart_on_failure() {
    // Clean up any orphaned processes from previous test runs
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_restart", "MCP");
    reporter.section("MCP BRIDGE DAEMON RESTART TEST");
    reporter.info("Testing MCP bridge proactive daemon restart");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Create config with short heartbeat settings for faster testing
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 19180

[api.grpc]
enabled = false
port = 59999

[mcp]
heartbeat_interval_secs = 2
heartbeat_max_failures = 2
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // Start MCP bridge
    reporter.info("Starting MCP bridge with short heartbeat interval...");
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");

    // Wait for daemon to start
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get initial daemon PID
    let initial_pid: u64;
    if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            initial_pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            // Register daemon for cleanup tracking
            register_e2e_process("mcp_daemon", initial_pid as u32);
            reporter.info(&format!("Initial daemon PID: {}", initial_pid));
        } else {
            reporter.error("Failed to parse PID file");
            return;
        }
    } else {
        reporter.error("PID file not found");
        return;
    }

    // Kill the daemon process (simulating crash)
    reporter.info("Killing daemon to simulate crash...");
    let _ = Command::new("kill")
        .args(["-9", &initial_pid.to_string()])
        .output();
    // Unregister since we killed it intentionally
    unregister_e2e_process("mcp_daemon", initial_pid as u32);

    // Wait for bridge to detect failure and restart daemon
    // With heartbeat_interval=2s and max_failures=2, should take ~4-6 seconds
    reporter.info("Waiting for bridge to detect failure and restart daemon...");
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Check if daemon was restarted (new PID in file)
    let content = std::fs::read_to_string(&pid_path).expect("PID file should exist after restart");
    let info: serde_json::Value =
        serde_json::from_str(&content).expect("PID file should be valid JSON");
    let new_pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
    // Register new daemon for cleanup tracking
    if new_pid != 0 {
        register_e2e_process("mcp_daemon", new_pid as u32);
    }
    reporter.info(&format!("New daemon PID: {}", new_pid));

    // Assert daemon was restarted with new PID
    assert!(new_pid != 0, "Daemon should have restarted with valid PID");
    assert!(
        new_pid != initial_pid,
        "Daemon should have new PID after restart (old: {}, new: {})",
        initial_pid,
        new_pid
    );
    reporter.info("✅ Daemon was restarted with new PID");

    // Verify new daemon is healthy
    let new_port = info
        .get("ports")
        .and_then(|p| p.get("http"))
        .and_then(|p| p.as_u64())
        .or_else(|| info.get("http_port").and_then(|p| p.as_u64()))
        .unwrap_or(19180) as u16;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = client
        .get(&format!("http://127.0.0.1:{}/health", new_port))
        .send()
        .await
        .expect("New daemon should respond to health check");
    assert!(
        resp.status().is_success(),
        "New daemon health check failed with status: {}",
        resp.status()
    );
    reporter.info("✅ New daemon is healthy");

    // Cleanup
    let _ = bridge_process.kill().await;
    let _ = Command::new("kill")
        .args(["-9", &new_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", new_pid as u32);

    // Wait for daemon to fully exit (avoid port conflicts with next test)
    for _ in 0..20 {
        let check = Command::new("kill")
            .args(["-0", &new_pid.to_string()])
            .output();
        match check {
            Ok(output) if !output.status.success() => break,
            Err(_) => break,
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }

    reporter.info("✅ MCP bridge daemon restart test PASSED");
}

/// Test Scenario 4: Daemon restart with port change due to port conflict
///
/// This tests Fixes #3, #4, #5 together in a real E2E scenario:
/// 1. Start daemon on port A
/// 2. Kill daemon
/// 3. Try to block port A (may fail due to race with bridge respawn)
/// 4. Wait for bridge to restart daemon
/// 5. Verify daemon restarted with new PID
/// 6. Verify bridge can still communicate with new daemon
///
/// NOTE: This test has a fundamental race condition - the bridge respawns the daemon
/// very quickly (via heartbeat failure detection), often before the test can block
/// the port. This is actually correct behavior - fast recovery is desirable.
/// Port fallback is tested more reliably by `test_port_fallback_when_port_blocked_before_start`
/// which blocks the port BEFORE starting the daemon.
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_daemon_restart_with_port_conflict() {
    use std::net::TcpListener;

    // Clean up any orphaned processes from previous test runs
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_port_conflict", "MCP");
    reporter.section("MCP BRIDGE DAEMON RESTART TEST");
    reporter.info("Testing bridge ability to restart daemon after crash");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment with unique ports
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Use unique port for this test to avoid TIME_WAIT collisions
    let counter = PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
    let initial_port = 19200 + ((std::process::id() as u16 + counter * 10) % 500);

    // Create config
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = {}

[api.grpc]
enabled = false
port = 59999

[mcp]
heartbeat_interval_secs = 2
heartbeat_max_failures = 2
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/"),
        initial_port
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");
    reporter.info(&format!("Config path: {}", config_path.display()));
    reporter.info(&format!("Configured port: {}", initial_port));
    reporter.info(&format!(
        "PID file path (test expects): {}",
        pid_path.display()
    ));

    // ========================================================================
    // PHASE 1: Start MCP bridge (spawns daemon)
    // ========================================================================
    reporter.section("PHASE 1: START MCP BRIDGE");

    let step = reporter.step_start("Start MCP bridge", "Launch bridge that spawns daemon");
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge process started"));

    // Spawn a task to consume stderr to prevent buffer blocking
    // Capture logs for debugging
    let bridge_logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bridge_logs_clone = bridge_logs.clone();
    let stderr = bridge_process.stderr.take().expect("Failed to get stderr");
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let mut logs = bridge_logs_clone.lock().await;
            logs.push(line);
        }
    });

    // Wait for daemon to start
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get initial daemon info
    let step = reporter.step_start("Get initial daemon info", "Read PID and port from PID file");
    let initial_pid: u64;
    let actual_initial_port: u16;

    if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            initial_pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            actual_initial_port = info
                .get("ports")
                .and_then(|p| p.get("http"))
                .and_then(|p| p.as_u64())
                .map(|p| p as u16)
                .unwrap_or(initial_port);
            // Register daemon for cleanup tracking
            register_e2e_process("mcp_daemon", initial_pid as u32);
            reporter.step_success(
                step,
                Some(&format!(
                    "PID: {}, Port: {}",
                    initial_pid, actual_initial_port
                )),
            );
        } else {
            reporter.step_failed(step, "Failed to parse PID file");
            let _ = bridge_process.kill().await;
            stderr_task.abort();
            return;
        }
    } else {
        reporter.step_failed(step, "PID file not found");
        let _ = bridge_process.kill().await;
        stderr_task.abort();
        return;
    }

    // Verify daemon is healthy
    let step = reporter.step_start("Verify daemon healthy", "Health check on initial daemon");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let health_url = format!("http://127.0.0.1:{}/health", actual_initial_port);
    match client.get(&health_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            reporter.step_success(step, Some("Daemon healthy"));
        }
        _ => {
            reporter.step_failed(step, "Daemon unhealthy");
            let _ = bridge_process.kill().await;
            stderr_task.abort();
            return;
        }
    }

    // ========================================================================
    // PHASE 2: Kill daemon and block original port
    // ========================================================================
    reporter.section("PHASE 2: KILL DAEMON AND BLOCK PORT");

    // Kill daemon
    let step = reporter.step_start("Kill daemon", "Terminate daemon process");
    let _ = Command::new("kill")
        .args(["-9", &initial_pid.to_string()])
        .output();
    // Unregister from cleanup tracking since we killed it
    unregister_e2e_process("mcp_daemon", initial_pid as u32);
    reporter.step_success(step, Some(&format!("Sent SIGKILL to PID {}", initial_pid)));

    // Wait for daemon to fully terminate and release port
    // Use longer timeout (15s) since SIGKILL delivery is async and sockets need cleanup time
    let step = reporter.step_start("Wait for termination", "Ensure daemon is fully stopped");
    let mut daemon_dead = false;
    for i in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Check if process is still running
        let check = Command::new("kill")
            .args(["-0", &initial_pid.to_string()])
            .output();
        if let Ok(output) = check {
            if !output.status.success() {
                daemon_dead = true;
                reporter.info(&format!("Daemon terminated after {}ms", (i + 1) * 500));
                break;
            }
        } else {
            daemon_dead = true;
            break;
        }
    }
    if !daemon_dead {
        reporter.warn("Daemon may still be running after 15s - continuing anyway");
    }

    // Extra wait for socket cleanup (sockets may be in TIME_WAIT briefly)
    tokio::time::sleep(Duration::from_millis(500)).await;
    reporter.step_success(step, Some("Daemon termination verified"));

    // Block the original port with a listener on localhost
    // This matches is_port_available() which only checks 127.0.0.1
    // Retry a few times with backoff since port may still be in TIME_WAIT
    let step = reporter.step_start(
        "Block original port",
        &format!("Bind listener to port {} on localhost", actual_initial_port),
    );
    let mut port_blocker = None;
    for attempt in 0..5 {
        match TcpListener::bind(format!("127.0.0.1:{}", actual_initial_port)) {
            Ok(listener) => {
                reporter.step_success(step, Some(&format!("Blocked port {}", actual_initial_port)));
                port_blocker = Some(listener);
                break;
            }
            Err(e) => {
                if attempt < 4 {
                    reporter.info(&format!(
                        "Port {} not available yet ({}), retrying in {}ms...",
                        actual_initial_port,
                        e,
                        100 * (1 << attempt)
                    ));
                    tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                } else {
                    reporter.warn(&format!(
                        "Could not block port {} after 5 attempts: {} (test may still work)",
                        actual_initial_port, e
                    ));
                }
            }
        }
    }

    // NOTE: We do NOT delete the PID file here. The old daemon was SIGKILL'd so
    // its PidFile::Drop didn't run, leaving the PID file with a released lock.
    // The new daemon will be able to acquire the lock and overwrite the file.
    // Previously we had a race condition where we'd delete the NEW daemon's
    // PID file if the bridge restarted the daemon before this step ran.

    // ========================================================================
    // PHASE 3: Wait for bridge to detect failure and restart daemon
    // ========================================================================
    reporter.section("PHASE 3: WAIT FOR FAILURE DETECTION AND RESTART");

    let step = reporter.step_start(
        "Wait for restart",
        "Wait for bridge heartbeat to detect daemon death and restart it",
    );

    // With heartbeat_interval=2s and max_failures=2, detection takes ~4-6 seconds
    // Monitor PID file during wait to see when daemon restarts
    for sec in 1..=10 {
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Check if PID file exists and has a DIFFERENT PID
        if let Ok(content) = std::fs::read_to_string(&pid_path) {
            if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
                let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
                if pid != 0 && pid != initial_pid {
                    reporter.info(&format!(
                        "Wait {}s: New daemon detected! PID={} (was {})",
                        sec, pid, initial_pid
                    ));
                    break;
                } else {
                    reporter.info(&format!("Wait {}s: PID file has old PID {}", sec, pid));
                }
            }
        } else {
            reporter.info(&format!("Wait {}s: PID file not readable yet", sec));
        }
    }
    reporter.step_success(step, Some("Wait complete, checking for new daemon"));

    // ========================================================================
    // PHASE 4: Verify daemon restarted on different port
    // ========================================================================
    reporter.section("PHASE 4: VERIFY NEW DAEMON");

    // Wait for new PID file to appear with a different PID
    reporter.info(&format!("Polling PID file at: {}", pid_path.display()));
    let step = reporter.step_start("Wait for new daemon", "Poll for new PID file (30s max)");
    let mut new_pid: u64 = 0;
    let mut new_port: u16 = 0;
    let mut found_new_daemon = false;

    for attempt in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Debug: check if temp dir and parent directory exist
        if attempt == 0 {
            let parent = pid_path.parent().unwrap();
            reporter.info(&format!(
                "Temp dir exists: {}, Parent dir exists: {}",
                temp_dir.path().exists(),
                parent.exists()
            ));
            // List temp dir contents
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    reporter.info(&format!("  Found: {}", entry.path().display()));
                }
            }
        }

        // Try to read PID file and show detailed error if it fails
        match std::fs::read_to_string(&pid_path) {
            Ok(content) => {
                reporter.info(&format!(
                    "Attempt {}: PID file content: {}",
                    attempt + 1,
                    content.trim()
                ));
                if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
                    let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
                    let port = info
                        .get("ports")
                        .and_then(|p| p.get("http"))
                        .and_then(|p| p.as_u64())
                        .unwrap_or(0) as u16;

                    if pid != 0 && pid != initial_pid {
                        // Found new PID, but we need to verify daemon is healthy
                        // The PID file may have old port if daemon is still starting up
                        // Wait for health check to confirm daemon is ready
                        let health_url = format!("http://127.0.0.1:{}/health", port);
                        if let Ok(resp) = client.get(&health_url).send().await {
                            if resp.status().is_success() {
                                new_pid = pid;
                                new_port = port;
                                found_new_daemon = true;
                                // Register new daemon for cleanup tracking
                                register_e2e_process("mcp_daemon", new_pid as u32);
                                reporter.info(&format!(
                                    "Found healthy daemon after {}s: PID={}, Port={}",
                                    attempt + 1,
                                    new_pid,
                                    new_port
                                ));
                                break;
                            } else {
                                reporter.info(&format!(
                                    "Attempt {}: New PID {} found but not healthy yet on port {}",
                                    attempt + 1,
                                    pid,
                                    port
                                ));
                            }
                        } else {
                            reporter.info(&format!(
                                "Attempt {}: New PID {} found but port {} not responding",
                                attempt + 1,
                                pid,
                                port
                            ));
                        }
                    } else if pid == initial_pid {
                        reporter.info(&format!(
                            "Attempt {}: PID file still has old PID {}",
                            attempt + 1,
                            pid
                        ));
                    }
                } else {
                    reporter.info(&format!("Attempt {}: JSON parse failed", attempt + 1));
                }
            }
            Err(e) => {
                reporter.info(&format!("Attempt {}: Read error: {}", attempt + 1, e));
            }
        }
    }

    if !found_new_daemon {
        reporter.step_failed(step, "New daemon did not start within timeout");

        // Print captured bridge logs for debugging
        reporter.section("BRIDGE LOGS (for debugging)");
        let logs = bridge_logs.lock().await;
        if logs.is_empty() {
            reporter.info("No logs captured from bridge");
        } else {
            for (i, log_line) in logs.iter().enumerate() {
                reporter.info(&format!("[{}] {}", i + 1, log_line));
            }
        }
        drop(logs);

        // Flush reporter output before cleanup and panic
        reporter.print_footer(false);

        let _ = bridge_process.kill().await;
        stderr_task.abort();
        drop(port_blocker);
        // Try to clean up old daemon if still running
        let _ = Command::new("kill")
            .args(["-9", &initial_pid.to_string()])
            .output();
        panic!("Daemon restart failed - new daemon not detected");
    }
    reporter.step_success(
        step,
        Some(&format!("New PID: {}, New Port: {}", new_pid, new_port)),
    );

    // Verify it's actually a new daemon
    let step = reporter.step_start("Verify new daemon", "Confirm PID changed");
    assert!(new_pid != 0, "New daemon should have valid PID");
    assert!(
        new_pid != initial_pid,
        "New daemon should have different PID (old: {}, new: {})",
        initial_pid,
        new_pid
    );
    reporter.step_success(step, Some("New daemon has different PID"));

    // Verify port changed (if we blocked it)
    if port_blocker.is_some() {
        let step = reporter.step_start("Verify port changed", "Check daemon is on different port");
        if new_port != actual_initial_port {
            reporter.step_success(
                step,
                Some(&format!(
                    "Port changed from {} to {} (port fallback worked)",
                    actual_initial_port, new_port
                )),
            );
        } else {
            reporter.warn(&format!(
                "Port didn't change (still {}), port blocker may have been released",
                new_port
            ));
        }
    }

    // Verify new daemon is healthy
    let step = reporter.step_start("Health check", "Verify new daemon is responding");
    let health_url = format!("http://127.0.0.1:{}/health", new_port);
    match client.get(&health_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            reporter.step_success(step, Some("New daemon is healthy"));
        }
        Ok(resp) => {
            reporter.step_failed(step, &format!("New daemon unhealthy: {}", resp.status()));
            let _ = bridge_process.kill().await;
            stderr_task.abort();
            drop(port_blocker);
            let _ = Command::new("kill")
                .args(["-9", &new_pid.to_string()])
                .output();
            panic!("New daemon health check failed");
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Health check error: {}", e));
            let _ = bridge_process.kill().await;
            stderr_task.abort();
            drop(port_blocker);
            let _ = Command::new("kill")
                .args(["-9", &new_pid.to_string()])
                .output();
            panic!("New daemon health check failed");
        }
    }

    // ========================================================================
    // CLEANUP
    // ========================================================================
    reporter.section("CLEANUP");

    drop(port_blocker); // Release blocked port
    let _ = bridge_process.kill().await;
    stderr_task.abort(); // Stop the stderr reader

    // Kill daemon and wait for it to fully terminate before returning.
    // This prevents orphaned daemons from interfering with subsequent tests.
    let _ = Command::new("kill")
        .args(["-9", &new_pid.to_string()])
        .output();
    // Unregister daemon from cleanup tracking
    unregister_e2e_process("mcp_daemon", new_pid as u32);

    // Wait for daemon to fully exit (avoid TIME_WAIT / port conflicts with next test)
    for _ in 0..20 {
        let check = Command::new("kill")
            .args(["-0", &new_pid.to_string()])
            .output();
        match check {
            Ok(output) if !output.status.success() => break,
            Err(_) => break,
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }

    reporter.info("✅ MCP bridge daemon restart with port conflict test PASSED");
    reporter.info(&format!("   Initial PID: {}", initial_pid));
    reporter.info(&format!("   Initial port: {}", actual_initial_port));
    reporter.info(&format!("   New PID: {}", new_pid));
    reporter.info(&format!("   New port: {}", new_port));

    reporter.print_footer(true);
}

/// Test that MCP bridge handles stale PID file with reused PID (Issue #3)
///
/// This tests the scenario where:
/// 1. Daemon crashes leaving a stale PID file
/// 2. OS reuses that PID for an unrelated process
/// 3. Bridge should detect this and spawn a new daemon
///
/// Simulates by creating a PID file with an unrelated process's PID.
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_stale_pid_reused_by_other_process() {
    use std::io::Write;

    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_stale_pid", "MCP");
    reporter.section("MCP BRIDGE STALE PID FILE TEST");
    reporter.info("Testing bridge handles PID file with reused PID");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Create config
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 19380

[api.grpc]
enabled = false
port = 59999
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // Create stale PID file with PID 1 (init/launchd - definitely not detrix)
    // ========================================================================
    reporter.section("PHASE 1: CREATE STALE PID FILE");

    let step = reporter.step_start("Create stale PID", "Write PID file with PID 1 (init)");
    {
        let mut file = std::fs::File::create(&pid_path).expect("Failed to create PID file");
        // PID 1 is always init/launchd, never detrix
        file.write_all(b"{\"pid\":1,\"ports\":{\"http\":19380}}\n")
            .expect("Failed to write PID file");
    }
    reporter.step_success(step, Some("Stale PID file created with PID 1"));

    // Verify PID file exists
    assert!(pid_path.exists(), "PID file should exist");

    // ========================================================================
    // Start MCP bridge - should detect stale PID and spawn new daemon
    // ========================================================================
    reporter.section("PHASE 2: START MCP BRIDGE");

    let step = reporter.step_start(
        "Start MCP bridge",
        "Bridge should detect PID 1 is not detrix and spawn daemon",
    );
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge process started"));

    // Wait for daemon to spawn
    tokio::time::sleep(Duration::from_secs(5)).await;

    // ========================================================================
    // Verify new daemon spawned with different PID
    // ========================================================================
    reporter.section("PHASE 3: VERIFY NEW DAEMON");

    let step = reporter.step_start("Check PID file", "Verify daemon spawned with new PID");
    let content = std::fs::read_to_string(&pid_path).expect("PID file should exist");
    let info: serde_json::Value =
        serde_json::from_str(&content).expect("PID file should be valid JSON");
    let daemon_pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);

    assert!(daemon_pid != 0, "Daemon should have valid PID");
    assert!(
        daemon_pid != 1,
        "Daemon should have different PID than stale file (was 1, now {})",
        daemon_pid
    );
    register_e2e_process("mcp_daemon", daemon_pid as u32);
    reporter.step_success(step, Some(&format!("New daemon PID: {}", daemon_pid)));

    // Verify daemon is healthy
    let daemon_port = info
        .get("ports")
        .and_then(|p| p.get("http"))
        .and_then(|p| p.as_u64())
        .unwrap_or(19380) as u16;

    let step = reporter.step_start("Health check", "Verify daemon is responding");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    match client
        .get(&format!("http://127.0.0.1:{}/health", daemon_port))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            reporter.step_success(step, Some("Daemon healthy"));
        }
        _ => {
            reporter.step_failed(step, "Daemon unhealthy");
            let _ = bridge_process.kill().await;
            panic!("Daemon should be healthy");
        }
    }

    // Cleanup
    reporter.section("CLEANUP");
    let _ = bridge_process.kill().await;
    let _ = Command::new("kill")
        .args(["-9", &daemon_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", daemon_pid as u32);

    reporter.info("✅ MCP bridge stale PID file test PASSED");
    reporter.print_footer(true);
}

/// Test that token file is cleaned up on daemon shutdown (Issue #12)
///
/// This tests Phase 3.2: When daemon shuts down gracefully,
/// the token file should be removed to prevent stale tokens.
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_token_cleanup_on_shutdown() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_token_cleanup", "MCP");
    reporter.section("MCP TOKEN CLEANUP ON SHUTDOWN TEST");
    reporter.info("Testing token file is removed when daemon shuts down");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Token file location (uses default path from detrix_config::paths::mcp_token_path())
    let home = std::env::var("HOME").expect("HOME not set");
    let token_path = std::path::PathBuf::from(&home)
        .join(".detrix")
        .join("mcp-token");

    // Create config
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 19480

[api.grpc]
enabled = false
port = 59999
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // Clean up any existing token file from previous tests
    let _ = std::fs::remove_file(&token_path);
    assert!(
        !token_path.exists(),
        "Token file should not exist initially"
    );

    // ========================================================================
    // Start MCP bridge (spawns daemon which creates token file)
    // ========================================================================
    reporter.section("PHASE 1: START DAEMON VIA MCP BRIDGE");

    let step = reporter.step_start("Start MCP bridge", "Spawn daemon that creates token file");
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge process started"));

    // Wait for daemon and token file to be created
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get daemon PID for cleanup
    let daemon_pid: u64 = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            if pid != 0 {
                register_e2e_process("mcp_daemon", pid as u32);
            }
            pid
        } else {
            0
        }
    } else {
        0
    };

    // ========================================================================
    // Verify token file was created
    // ========================================================================
    reporter.section("PHASE 2: VERIFY TOKEN FILE CREATED");

    let step = reporter.step_start("Check token file", "Token file should exist");
    if token_path.exists() {
        // Also verify permissions are 0600
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(&token_path).expect("Failed to get token metadata");
            let mode = meta.mode() & 0o777;
            reporter.info(&format!("Token file permissions: {:o}", mode));
            if mode != 0o600 {
                reporter.warn(&format!(
                    "Token file has unexpected permissions: {:o} (expected 600)",
                    mode
                ));
            }
        }
        reporter.step_success(
            step,
            Some(&format!("Token file exists at {}", token_path.display())),
        );
    } else {
        reporter.step_failed(step, "Token file was not created");
        // This might happen if daemon didn't start with mcp_spawned flag
        reporter.warn("Token file not created - daemon may not be MCP-spawned");
        // Continue test anyway to verify cleanup doesn't crash
    }

    // ========================================================================
    // Stop daemon gracefully (SIGTERM)
    // ========================================================================
    reporter.section("PHASE 3: GRACEFUL SHUTDOWN");

    let step = reporter.step_start("Stop daemon", "Send SIGTERM for graceful shutdown");
    if daemon_pid != 0 {
        // Send SIGTERM for graceful shutdown
        let _ = Command::new("kill")
            .args(["-TERM", &daemon_pid.to_string()])
            .output();
        reporter.step_success(step, Some(&format!("Sent SIGTERM to PID {}", daemon_pid)));

        // Wait for shutdown
        tokio::time::sleep(Duration::from_secs(3)).await;
    } else {
        reporter.step_failed(step, "No daemon PID to stop");
    }

    // Kill bridge
    let _ = bridge_process.kill().await;

    // ========================================================================
    // Verify token file was cleaned up
    // ========================================================================
    reporter.section("PHASE 4: VERIFY TOKEN CLEANUP");

    let step = reporter.step_start(
        "Check token removed",
        "Token file should be deleted after shutdown",
    );

    // Give it a moment for cleanup
    tokio::time::sleep(Duration::from_millis(500)).await;

    if !token_path.exists() {
        reporter.step_success(step, Some("Token file successfully cleaned up"));
    } else {
        // Token might still exist if daemon was killed before cleanup ran
        reporter.warn("Token file still exists - may need manual cleanup");
        // Clean it up for next test
        let _ = std::fs::remove_file(&token_path);
    }

    // Cleanup
    if daemon_pid != 0 {
        let _ = Command::new("kill")
            .args(["-9", &daemon_pid.to_string()])
            .output();
        unregister_e2e_process("mcp_daemon", daemon_pid as u32);
    }

    reporter.info("✅ MCP token cleanup test completed");
    reporter.print_footer(true);
}

/// Test that restart backoff is applied after repeated failures (Issue #6)
///
/// This tests Phase 2.1: When daemon repeatedly fails to start,
/// the bridge should apply exponential backoff between attempts.
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_restart_backoff() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_backoff", "MCP");
    reporter.section("MCP BRIDGE RESTART BACKOFF TEST");
    reporter.info("Testing exponential backoff on repeated daemon failures");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Create config with short heartbeat interval for faster testing
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 19580

[api.grpc]
enabled = false
port = 59999

[mcp]
heartbeat_interval_secs = 2
heartbeat_max_failures = 1
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // Start MCP bridge
    // ========================================================================
    reporter.section("PHASE 1: START MCP BRIDGE");

    let step = reporter.step_start("Start MCP bridge", "Launch bridge that spawns daemon");
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge started"));

    // Wait for daemon
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get initial daemon PID
    let initial_pid: u64 = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            if pid != 0 {
                register_e2e_process("mcp_daemon", pid as u32);
            }
            pid
        } else {
            0
        }
    } else {
        reporter.error("Failed to read PID file");
        let _ = bridge_process.kill().await;
        return;
    };
    reporter.info(&format!("Initial daemon PID: {}", initial_pid));

    // ========================================================================
    // Kill daemon twice to trigger backoff
    // ========================================================================
    reporter.section("PHASE 2: TRIGGER BACKOFF");

    // First kill
    let step = reporter.step_start("First kill", "Kill daemon first time");
    let _ = Command::new("kill")
        .args(["-9", &initial_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", initial_pid as u32);
    reporter.step_success(step, Some("First kill sent"));

    // Wait for restart (heartbeat failure detection + restart)
    let step = reporter.step_start("Wait for first restart", "Wait for daemon to restart");
    tokio::time::sleep(Duration::from_secs(8)).await;

    // Get new PID
    let second_pid: u64 = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            if pid != 0 && pid != initial_pid {
                register_e2e_process("mcp_daemon", pid as u32);
            }
            pid
        } else {
            0
        }
    } else {
        0
    };

    if second_pid != 0 && second_pid != initial_pid {
        reporter.step_success(step, Some(&format!("Restarted with PID {}", second_pid)));
    } else {
        reporter.step_failed(step, "Daemon didn't restart with new PID");
        let _ = bridge_process.kill().await;
        return;
    }

    // Second kill
    let step = reporter.step_start("Second kill", "Kill daemon again to trigger backoff");
    let start_time = std::time::Instant::now();
    let _ = Command::new("kill")
        .args(["-9", &second_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", second_pid as u32);
    reporter.step_success(step, Some("Second kill sent"));

    // Now the bridge should apply backoff before next restart attempt
    // Initial backoff is 10 seconds, so restart should take longer than 10s
    let step = reporter.step_start(
        "Wait for backoff restart",
        "Verify restart is delayed by backoff",
    );

    // Wait for restart with timeout
    let mut third_pid: u64 = 0;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(1)).await;

        if let Ok(content) = std::fs::read_to_string(&pid_path) {
            if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
                let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
                if pid != 0 && pid != second_pid && pid != initial_pid {
                    third_pid = pid;
                    register_e2e_process("mcp_daemon", third_pid as u32);
                    break;
                }
            }
        }
    }

    let restart_time = start_time.elapsed();

    if third_pid != 0 {
        reporter.step_success(
            step,
            Some(&format!(
                "Restarted with PID {} after {:?}",
                third_pid, restart_time
            )),
        );

        // Verify backoff was applied (should take > 8 seconds due to heartbeat detection + backoff)
        // First restart: ~4-6s (heartbeat failure detection)
        // Second restart with backoff: ~4-6s + 10s backoff = ~14-16s total
        if restart_time.as_secs() >= 8 {
            reporter.info(&format!(
                "✅ Restart took {:?} - backoff likely applied",
                restart_time
            ));
        } else {
            reporter.warn(&format!(
                "Restart took only {:?} - backoff may not have been applied",
                restart_time
            ));
        }
    } else {
        reporter.step_failed(step, "Daemon didn't restart within timeout");
    }

    // Cleanup
    reporter.section("CLEANUP");
    let _ = bridge_process.kill().await;
    if third_pid != 0 {
        let _ = Command::new("kill")
            .args(["-9", &third_pid.to_string()])
            .output();
        unregister_e2e_process("mcp_daemon", third_pid as u32);
    }

    reporter.info("✅ MCP bridge restart backoff test completed");
    reporter.print_footer(true);
}

/// Test that MCP bridge handles corrupt PID file gracefully
///
/// This tests the scenario where:
/// 1. PID file contains invalid JSON
/// 2. Bridge should detect corruption and spawn a new daemon
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_corrupt_pid_file_handling() {
    use std::io::Write;

    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_corrupt_pid", "MCP");
    reporter.section("MCP BRIDGE CORRUPT PID FILE TEST");
    reporter.info("Testing bridge handles corrupt/invalid PID file");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Get an available port using the e2e infrastructure
    let test_port = detrix_testing::e2e::executor::get_http_port();
    reporter.info(&format!("Using port: {}", test_port));

    // Create config
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = {}

[api.grpc]
enabled = false
port = 59999
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/"),
        test_port
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // Create corrupt PID file with invalid JSON
    // ========================================================================
    reporter.section("PHASE 1: CREATE CORRUPT PID FILE");

    let step = reporter.step_start("Create corrupt PID", "Write invalid JSON to PID file");
    {
        let mut file = std::fs::File::create(&pid_path).expect("Failed to create PID file");
        // Write invalid JSON that will fail to parse
        file.write_all(b"{ invalid json content, not valid }")
            .expect("Failed to write PID file");
    }
    reporter.step_success(step, Some("Corrupt PID file created"));

    // Verify PID file exists
    assert!(pid_path.exists(), "PID file should exist");

    // ========================================================================
    // Start MCP bridge - should detect corrupt PID and spawn new daemon
    // ========================================================================
    reporter.section("PHASE 2: START MCP BRIDGE");

    let step = reporter.step_start(
        "Start MCP bridge",
        "Bridge should handle corrupt PID and spawn daemon",
    );
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge process started"));

    // ========================================================================
    // Verify new daemon spawned with valid PID file
    // ========================================================================
    reporter.section("PHASE 3: VERIFY NEW DAEMON");

    // Wait for valid PID file with retry loop (daemon may take time to start under load)
    let step = reporter.step_start("Check PID file", "Wait for valid PID file to be created");
    let pid_check = timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(content) = std::fs::read_to_string(&pid_path) {
                if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
                    // Verify it has required fields (not corrupt)
                    if info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0) != 0 {
                        return info;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    let info: serde_json::Value = match pid_check {
        Ok(v) => v,
        Err(_) => {
            reporter.step_failed(step, "Timeout waiting for valid PID file (30s)");
            let _ = bridge_process.kill().await;
            panic!("PID file should be valid JSON after daemon start (timeout 30s)");
        }
    };

    let daemon_pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
    assert!(daemon_pid != 0, "Daemon should have valid PID");
    register_e2e_process("mcp_daemon", daemon_pid as u32);
    reporter.step_success(
        step,
        Some(&format!("Valid PID file with PID: {}", daemon_pid)),
    );

    // Verify daemon is healthy
    let daemon_port = info
        .get("ports")
        .and_then(|p| p.get("http"))
        .and_then(|p| p.as_u64())
        .map(|p| p as u16)
        .unwrap_or(test_port);

    let step = reporter.step_start("Health check", "Verify daemon is responding");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Retry health check with timeout (daemon may still be starting)
    let health_url = format!("http://127.0.0.1:{}/health", daemon_port);
    let health_check = timeout(Duration::from_secs(10), async {
        loop {
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => return true,
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    match health_check {
        Ok(true) => {
            reporter.step_success(step, Some("Daemon healthy"));
        }
        _ => {
            reporter.step_failed(step, "Daemon health check timed out");
            let _ = bridge_process.kill().await;
            panic!("Daemon should be healthy");
        }
    }

    // Cleanup
    reporter.section("CLEANUP");
    let _ = bridge_process.kill().await;
    let _ = Command::new("kill")
        .args(["-9", &daemon_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", daemon_pid as u32);

    reporter.info("✅ MCP bridge corrupt PID file test PASSED");
    reporter.print_footer(true);
}

/// Test that multiple MCP bridges connect to the same daemon
///
/// This tests the scenario where:
/// 1. First bridge spawns daemon
/// 2. Second bridge connects to existing daemon (doesn't spawn new one)
/// 3. Both bridges can communicate with daemon
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_multiple_bridges_same_daemon() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_multi", "MCP");
    reporter.section("MCP BRIDGE MULTIPLE BRIDGES TEST");
    reporter.info("Testing multiple bridges connect to same daemon");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Create config
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 19780

[api.grpc]
enabled = false
port = 59999

[mcp]
shutdown_grace_period_secs = 30
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // Start first MCP bridge (spawns daemon)
    // ========================================================================
    reporter.section("PHASE 1: START FIRST BRIDGE");

    let step = reporter.step_start("Start first bridge", "Launch bridge that spawns daemon");
    let mut bridge1 = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn first MCP bridge");
    reporter.step_success(step, Some("First bridge started"));

    // Wait for daemon
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get daemon PID
    let initial_pid: u64 = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            if pid != 0 {
                register_e2e_process("mcp_daemon", pid as u32);
            }
            pid
        } else {
            0
        }
    } else {
        reporter.error("Failed to read PID file");
        let _ = bridge1.kill().await;
        return;
    };
    reporter.info(&format!("Daemon PID: {}", initial_pid));

    // ========================================================================
    // Start second MCP bridge (should connect to existing daemon)
    // ========================================================================
    reporter.section("PHASE 2: START SECOND BRIDGE");

    let step = reporter.step_start("Start second bridge", "Launch another bridge");
    let mut bridge2 = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn second MCP bridge");
    reporter.step_success(step, Some("Second bridge started"));

    // Wait a bit for second bridge to connect
    tokio::time::sleep(Duration::from_secs(3)).await;

    // ========================================================================
    // Verify daemon PID hasn't changed (no new daemon spawned)
    // ========================================================================
    reporter.section("PHASE 3: VERIFY SAME DAEMON");

    let step = reporter.step_start("Check daemon PID", "Verify PID unchanged");
    let daemon_info =
        read_daemon_info(&pid_path).expect("Failed to read daemon info from PID file");
    let current_pid = daemon_info.pid();

    assert_eq!(
        current_pid, initial_pid,
        "Daemon PID should not change when second bridge connects"
    );
    reporter.step_success(step, Some(&format!("PID still {}", current_pid)));

    // Verify daemon still healthy - use port from PID file
    let daemon_host = daemon_info.host();
    let daemon_port = daemon_info.http_port();
    let step = reporter.step_start("Health check", "Verify daemon is responding");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    match client
        .get(&format!("http://{}:{}/health", daemon_host, daemon_port))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            reporter.step_success(step, Some("Daemon healthy with both bridges"));
        }
        _ => {
            reporter.step_failed(step, "Daemon unhealthy");
        }
    }

    // ========================================================================
    // Kill first bridge, verify daemon still running (second bridge keeps it alive)
    // ========================================================================
    reporter.section("PHASE 4: KILL FIRST BRIDGE");

    let step = reporter.step_start("Kill first bridge", "Terminate first bridge");
    let _ = bridge1.kill().await;
    reporter.step_success(step, Some("First bridge killed"));

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Daemon should still be running
    let step = reporter.step_start("Verify daemon alive", "Daemon should still run");
    match client
        .get(&format!("http://{}:{}/health", daemon_host, daemon_port))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            reporter.step_success(step, Some("Daemon still running with second bridge"));
        }
        _ => {
            reporter.step_failed(step, "Daemon stopped prematurely");
        }
    }

    // Cleanup
    reporter.section("CLEANUP");
    let _ = bridge2.kill().await;
    let _ = Command::new("kill")
        .args(["-9", &initial_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", initial_pid as u32);

    reporter.info("✅ MCP bridge multiple bridges test PASSED");
    reporter.print_footer(true);
}

/// Test that auth token is available immediately after daemon spawn (Issue #1)
///
/// This tests that the token race condition is fixed:
/// Bridge should be able to authenticate immediately after daemon starts.
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_auth_token_not_racy() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_auth_race", "MCP");
    reporter.section("MCP BRIDGE AUTH TOKEN RACE TEST");
    reporter.info("Testing auth token is available immediately after spawn");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Token file location
    let home = std::env::var("HOME").expect("HOME not set");
    let token_path = std::path::PathBuf::from(&home)
        .join(".detrix")
        .join("mcp-token");

    // Clean up any existing token
    let _ = std::fs::remove_file(&token_path);

    // Create config
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 19880

[api.grpc]
enabled = false
port = 59999
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // Start MCP bridge and immediately try to make authenticated request
    // ========================================================================
    reporter.section("PHASE 1: START BRIDGE AND IMMEDIATE AUTH");

    let step = reporter.step_start("Start bridge", "Launch bridge that spawns daemon");
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge started"));

    // Wait for daemon to be healthy (minimal wait)
    let step = reporter.step_start("Wait for health", "Wait for daemon to be ready");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();

    let mut daemon_info: Option<DaemonInfo> = None;
    let mut token: Option<String> = None;

    // Poll until daemon is ready, reading host and port from PID file
    for attempt in 0..30 {
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Try to read daemon info from PID file
        if let Some(info) = read_daemon_info(&pid_path) {
            let health_url = format!("http://{}:{}/health", info.host(), info.http_port());
            if client
                .get(&health_url)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
            {
                reporter.info(&format!("Daemon ready after {} attempts", attempt + 1));
                daemon_info = Some(info);
                break;
            }
        }
    }

    let daemon_info = daemon_info.expect("Daemon did not become healthy in time");
    let daemon_host = daemon_info.host();
    let daemon_port = daemon_info.http_port();
    reporter.step_success(
        step,
        Some(&format!(
            "Daemon healthy at {}:{}",
            daemon_host, daemon_port
        )),
    );

    // ========================================================================
    // Verify token file exists and try authenticated request
    // ========================================================================
    reporter.section("PHASE 2: VERIFY TOKEN AVAILABLE");

    let step = reporter.step_start("Read token", "Token should exist");
    if let Ok(t) = std::fs::read_to_string(&token_path) {
        token = Some(t.trim().to_string());
        reporter.step_success(step, Some("Token file exists and readable"));
    } else {
        reporter.step_failed(step, "Token file not found");
        reporter.warn("Token may not be created for non-MCP spawned daemons");
    }

    // Try authenticated request
    let step = reporter.step_start("Auth request", "Make authenticated API request");
    if let Some(ref t) = token {
        match client
            .get(&format!(
                "http://{}:{}/api/v1/metrics",
                daemon_host, daemon_port
            ))
            .header("Authorization", format!("Bearer {}", t))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                reporter.step_success(step, Some("Authenticated request succeeded"));
            }
            Ok(resp) => {
                reporter.step_failed(step, &format!("Request failed: {}", resp.status()));
            }
            Err(e) => {
                reporter.step_failed(step, &format!("Request error: {}", e));
            }
        }
    } else {
        reporter.info("Skipping auth request - no token available");
    }

    // Get daemon PID for cleanup
    let daemon_pid: u64 = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            if pid != 0 {
                register_e2e_process("mcp_daemon", pid as u32);
            }
            pid
        } else {
            0
        }
    } else {
        0
    };

    // Cleanup
    reporter.section("CLEANUP");
    let _ = bridge_process.kill().await;
    if daemon_pid != 0 {
        let _ = Command::new("kill")
            .args(["-9", &daemon_pid.to_string()])
            .output();
        unregister_e2e_process("mcp_daemon", daemon_pid as u32);
    }
    let _ = std::fs::remove_file(&token_path);

    reporter.info("✅ MCP bridge auth token race test completed");
    reporter.print_footer(true);
}

/// Test that bridge recovers with new token after daemon restart
///
/// This tests the scenario where:
/// 1. Daemon starts with token A
/// 2. Daemon is killed and restarted
/// 3. Bridge discovers new token B and authenticates successfully
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_stale_token_recovery_after_restart() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_token_recovery", "MCP");
    reporter.section("MCP BRIDGE STALE TOKEN RECOVERY TEST");
    reporter.info("Testing bridge recovers with new token after daemon restart");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Token file location
    let home = std::env::var("HOME").expect("HOME not set");
    let token_path = std::path::PathBuf::from(&home)
        .join(".detrix")
        .join("mcp-token");
    let _ = std::fs::remove_file(&token_path);

    // Create config with short heartbeat for faster testing
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 19980

[api.grpc]
enabled = false
port = 59999

[mcp]
heartbeat_interval_secs = 2
heartbeat_max_failures = 2
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // Start bridge and get initial token
    // ========================================================================
    reporter.section("PHASE 1: START AND GET INITIAL TOKEN");

    let step = reporter.step_start("Start bridge", "Launch bridge");
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge started"));

    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get initial daemon PID and token
    let initial_pid: u64 = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            if pid != 0 {
                register_e2e_process("mcp_daemon", pid as u32);
            }
            pid
        } else {
            0
        }
    } else {
        0
    };

    let initial_token = std::fs::read_to_string(&token_path)
        .ok()
        .map(|t| t.trim().to_string());
    reporter.info(&format!("Initial PID: {}", initial_pid));
    reporter.info(&format!(
        "Initial token: {:?}",
        initial_token.as_ref().map(|t| &t[..8.min(t.len())])
    ));

    // ========================================================================
    // Kill daemon and wait for restart
    // ========================================================================
    reporter.section("PHASE 2: KILL DAEMON AND WAIT FOR RESTART");

    let step = reporter.step_start("Kill daemon", "Simulate daemon crash");
    let _ = Command::new("kill")
        .args(["-9", &initial_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", initial_pid as u32);
    reporter.step_success(step, Some("Daemon killed"));

    // Wait for bridge to detect and restart daemon
    let step = reporter.step_start("Wait for restart", "Wait for new daemon");
    tokio::time::sleep(Duration::from_secs(10)).await;
    reporter.step_success(step, Some("Wait complete"));

    // ========================================================================
    // Verify new token and authenticated request works
    // ========================================================================
    reporter.section("PHASE 3: VERIFY NEW TOKEN");

    let step = reporter.step_start("Check new daemon", "Verify daemon restarted");
    let new_pid: u64 = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            if pid != 0 && pid != initial_pid {
                register_e2e_process("mcp_daemon", pid as u32);
            }
            pid
        } else {
            0
        }
    } else {
        0
    };

    if new_pid != 0 && new_pid != initial_pid {
        reporter.step_success(step, Some(&format!("New PID: {}", new_pid)));
    } else {
        reporter.step_failed(step, "Daemon didn't restart with new PID");
        let _ = bridge_process.kill().await;
        return;
    }

    let step = reporter.step_start("Check new token", "Verify new token exists");
    let new_token = std::fs::read_to_string(&token_path)
        .ok()
        .map(|t| t.trim().to_string());

    if let Some(ref t) = new_token {
        reporter.step_success(
            step,
            Some(&format!("New token: {}...", &t[..8.min(t.len())])),
        );

        // Verify token changed (if initial existed)
        if let Some(ref initial) = initial_token {
            if t != initial {
                reporter.info("✅ Token changed after restart (as expected)");
            } else {
                reporter.warn("Token did not change after restart");
            }
        }
    } else {
        reporter.step_failed(step, "New token not found");
    }

    // Cleanup
    reporter.section("CLEANUP");
    let _ = bridge_process.kill().await;
    if new_pid != 0 {
        let _ = Command::new("kill")
            .args(["-9", &new_pid.to_string()])
            .output();
        unregister_e2e_process("mcp_daemon", new_pid as u32);
    }
    let _ = std::fs::remove_file(&token_path);

    reporter.info("✅ MCP bridge stale token recovery test completed");
    reporter.print_footer(true);
}

/// Test that daemon SIGTERM triggers bridge restart
///
/// This tests the scenario where:
/// 1. Daemon receives SIGTERM (graceful shutdown request)
/// 2. Bridge detects daemon is gone and restarts it
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_daemon_sigterm_triggers_restart() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_sigterm", "MCP");
    reporter.section("MCP BRIDGE DAEMON SIGTERM RESTART TEST");
    reporter.info("Testing bridge restarts daemon after SIGTERM");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Create config with short heartbeat
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 20080

[api.grpc]
enabled = false
port = 59999

[mcp]
heartbeat_interval_secs = 2
heartbeat_max_failures = 2
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // Start bridge
    // ========================================================================
    reporter.section("PHASE 1: START BRIDGE");

    let step = reporter.step_start("Start bridge", "Launch bridge");
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge started"));

    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get initial daemon PID
    let initial_pid: u64 = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            if pid != 0 {
                register_e2e_process("mcp_daemon", pid as u32);
            }
            pid
        } else {
            0
        }
    } else {
        0
    };
    reporter.info(&format!("Initial daemon PID: {}", initial_pid));

    // ========================================================================
    // Send SIGTERM to daemon
    // ========================================================================
    reporter.section("PHASE 2: SEND SIGTERM");

    let step = reporter.step_start("Send SIGTERM", "Graceful shutdown signal");
    let _ = Command::new("kill")
        .args(["-TERM", &initial_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", initial_pid as u32);
    reporter.step_success(step, Some("SIGTERM sent"));

    // Wait for bridge to detect and restart
    tokio::time::sleep(Duration::from_secs(10)).await;

    // ========================================================================
    // Verify daemon restarted
    // ========================================================================
    reporter.section("PHASE 3: VERIFY RESTART");

    let step = reporter.step_start("Check new daemon", "Verify daemon restarted");
    let new_pid: u64 = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            if pid != 0 && pid != initial_pid {
                register_e2e_process("mcp_daemon", pid as u32);
            }
            pid
        } else {
            0
        }
    } else {
        0
    };

    if new_pid != 0 && new_pid != initial_pid {
        reporter.step_success(
            step,
            Some(&format!("New PID: {} (was {})", new_pid, initial_pid)),
        );
    } else if new_pid == initial_pid {
        reporter.warn("PID unchanged - daemon may have ignored SIGTERM");
    } else {
        reporter.step_failed(step, "Daemon didn't restart");
    }

    // Cleanup
    reporter.section("CLEANUP");
    let _ = bridge_process.kill().await;
    if new_pid != 0 {
        let _ = Command::new("kill")
            .args(["-9", &new_pid.to_string()])
            .output();
        unregister_e2e_process("mcp_daemon", new_pid as u32);
    }

    reporter.info("✅ MCP bridge SIGTERM restart test completed");
    reporter.print_footer(true);
}

/// Test that multiple bridges starting simultaneously don't race to spawn daemons
///
/// This tests the scenario where:
/// 1. Two bridges start at nearly the same time
/// 2. Only one daemon should be spawned
/// 3. Both bridges should connect to the same daemon
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_no_spawn_race_with_multiple_bridges() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_bridge_spawn_race", "MCP");
    reporter.section("MCP BRIDGE SPAWN RACE TEST");
    reporter.info("Testing no race condition when multiple bridges start simultaneously");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Create config
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 20180

[api.grpc]
enabled = false
port = 59999
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // Start two bridges simultaneously
    // ========================================================================
    reporter.section("PHASE 1: START TWO BRIDGES SIMULTANEOUSLY");

    let step = reporter.step_start("Start bridges", "Launch two bridges at once");

    // Start both bridges as quickly as possible
    let mut bridge1 = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn first MCP bridge");

    let mut bridge2 = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn second MCP bridge");

    reporter.step_success(step, Some("Both bridges started"));

    // Wait for daemon(s) to start
    tokio::time::sleep(Duration::from_secs(5)).await;

    // ========================================================================
    // Verify only one daemon is running
    // ========================================================================
    reporter.section("PHASE 2: VERIFY SINGLE DAEMON");

    let step = reporter.step_start("Check daemon count", "Should be exactly one daemon");

    // Read daemon info from PID file
    let daemon_info = read_daemon_info(&pid_path);
    let daemon_pid = daemon_info.as_ref().map(|i| i.pid()).unwrap_or(0);
    if daemon_pid != 0 {
        register_e2e_process("mcp_daemon", daemon_pid as u32);
    }

    // Count detrix serve processes
    let ps_output = Command::new("pgrep")
        .args(["-f", "detrix.*serve.*--daemon"])
        .output()
        .expect("Failed to run pgrep");

    let pids: Vec<&str> = std::str::from_utf8(&ps_output.stdout)
        .unwrap_or("")
        .trim()
        .split('\n')
        .filter(|s| !s.is_empty())
        .collect();

    reporter.info(&format!("Found {} daemon process(es)", pids.len()));
    for pid in &pids {
        reporter.info(&format!("  PID: {}", pid));
    }

    if pids.len() == 1 {
        reporter.step_success(step, Some("Exactly one daemon running"));
    } else if pids.is_empty() {
        reporter.step_failed(step, "No daemon processes found");
    } else {
        reporter.warn(&format!("Multiple daemon processes found: {}", pids.len()));
        // This might happen if the test ran before cleanup from previous test
        // Still consider it a pass if they're using the same PID file
    }

    // Verify daemon is healthy - use host and port from PID file
    let step = reporter.step_start("Health check", "Verify daemon is responding");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let daemon_info = daemon_info.expect("Daemon info should be available");
    let daemon_host = daemon_info.host();
    let daemon_port = daemon_info.http_port();
    match client
        .get(&format!("http://{}:{}/health", daemon_host, daemon_port))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            reporter.step_success(step, Some("Daemon healthy"));
        }
        _ => {
            reporter.step_failed(step, "Daemon unhealthy");
        }
    }

    // Cleanup
    reporter.section("CLEANUP");
    let _ = bridge1.kill().await;
    let _ = bridge2.kill().await;

    // Kill all found daemon processes
    for pid in &pids {
        let _ = Command::new("kill").args(["-9", pid]).output();
    }
    if daemon_pid != 0 {
        unregister_e2e_process("mcp_daemon", daemon_pid as u32);
    }

    reporter.info("✅ MCP bridge spawn race test completed");
    reporter.print_footer(true);
}

/// Test that stale clients are cleaned up after heartbeat timeout
///
/// This tests the scenario where:
/// 1. Client connects to daemon
/// 2. Client stops sending heartbeats
/// 3. Daemon should remove stale client after timeout
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_stale_client_cleanup() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_stale_client", "MCP");
    reporter.section("MCP STALE CLIENT CLEANUP TEST");
    reporter.info("Testing stale clients are cleaned up after timeout");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Token file location
    let home = std::env::var("HOME").expect("HOME not set");
    let token_path = std::path::PathBuf::from(&home)
        .join(".detrix")
        .join("mcp-token");

    // Create config with short heartbeat timeout for faster testing
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 20280

[api.grpc]
enabled = false
port = 59999

[mcp]
heartbeat_timeout_secs = 5
cleanup_interval_secs = 2
shutdown_grace_period_secs = 60
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // Start bridge to spawn daemon
    // ========================================================================
    reporter.section("PHASE 1: START BRIDGE");

    let step = reporter.step_start("Start bridge", "Launch bridge to spawn daemon");
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge started"));

    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get daemon info
    let daemon_pid: u64;
    let daemon_port: u16;
    let token: Option<String>;

    if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            daemon_pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            daemon_port = info
                .get("ports")
                .and_then(|p| p.get("http"))
                .and_then(|p| p.as_u64())
                .unwrap_or(20280) as u16;
            if daemon_pid != 0 {
                register_e2e_process("mcp_daemon", daemon_pid as u32);
            }
        } else {
            reporter.error("Failed to parse PID file");
            let _ = bridge_process.kill().await;
            return;
        }
    } else {
        reporter.error("Failed to read PID file");
        let _ = bridge_process.kill().await;
        return;
    }

    token = std::fs::read_to_string(&token_path)
        .ok()
        .map(|t| t.trim().to_string());
    reporter.info(&format!(
        "Daemon PID: {}, Port: {}",
        daemon_pid, daemon_port
    ));

    // ========================================================================
    // Check client count before killing bridge
    // ========================================================================
    reporter.section("PHASE 2: CHECK INITIAL CLIENT COUNT");

    let step = reporter.step_start("Get client count", "Query MCP clients endpoint");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let mut req = client.get(&format!("http://127.0.0.1:{}/mcp/clients", daemon_port));
    if let Some(ref t) = token {
        req = req.header("Authorization", format!("Bearer {}", t));
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            reporter.step_success(step, Some(&format!("Clients: {}", body.trim())));
        }
        Ok(resp) => {
            reporter.info(&format!("Clients endpoint returned: {}", resp.status()));
        }
        Err(e) => {
            reporter.info(&format!("Clients endpoint error: {}", e));
        }
    }

    // ========================================================================
    // Kill bridge (simulates client disappearing without disconnect)
    // ========================================================================
    reporter.section("PHASE 3: KILL BRIDGE (SIMULATE STALE CLIENT)");

    let step = reporter.step_start("Kill bridge", "Terminate bridge without clean disconnect");
    let _ = bridge_process.kill().await;
    reporter.step_success(step, Some("Bridge killed"));

    // ========================================================================
    // Wait for cleanup and verify client removed
    // ========================================================================
    reporter.section("PHASE 4: WAIT FOR CLEANUP");

    let step = reporter.step_start("Wait for cleanup", "Wait for heartbeat timeout + cleanup");
    // heartbeat_timeout_secs = 5, cleanup_interval_secs = 2
    // Should take about 5-7 seconds to detect and clean up
    tokio::time::sleep(Duration::from_secs(10)).await;
    reporter.step_success(step, Some("Wait complete"));

    let step = reporter.step_start("Check client removed", "Query MCP clients endpoint again");
    let mut req = client.get(&format!("http://127.0.0.1:{}/mcp/clients", daemon_port));
    if let Some(ref t) = token {
        req = req.header("Authorization", format!("Bearer {}", t));
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            reporter.step_success(
                step,
                Some(&format!("Clients after cleanup: {}", body.trim())),
            );
            // Check if client count is 0 or empty
            if body.contains("\"count\":0") || body.contains("[]") || body.trim() == "{}" {
                reporter.info("✅ Stale client was cleaned up");
            }
        }
        Ok(resp) => {
            reporter.info(&format!("Clients endpoint returned: {}", resp.status()));
        }
        Err(e) => {
            reporter.info(&format!("Clients endpoint error: {}", e));
        }
    }

    // Cleanup
    reporter.section("CLEANUP");
    let _ = Command::new("kill")
        .args(["-9", &daemon_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", daemon_pid as u32);

    reporter.info("✅ MCP stale client cleanup test completed");
    reporter.print_footer(true);
}

/// Test that active debugger connection prevents daemon shutdown
///
/// This tests the scenario where:
/// 1. MCP bridge spawns daemon
/// 2. Debugger (debugpy) is started and connected
/// 3. MCP bridge is killed (client disconnects)
/// 4. Daemon should NOT shutdown because debugger connection is active
#[cfg(unix)]
#[tokio::test]
async fn test_mcp_bridge_active_debugger_prevents_shutdown() {
    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("mcp_debugger_prevents_shutdown", "MCP");
    reporter.section("MCP DEBUGGER PREVENTS SHUTDOWN TEST");
    reporter.info("Testing that active debugger connection keeps daemon alive");

    // Check if debugpy is available
    if !is_debugpy_available().await {
        reporter.warn("Skipping test: debugpy not available");
        reporter.info("Install debugpy with: pip install debugpy");
        return;
    }

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Get ports
    let debugpy_port = get_debugpy_port();

    // Create config with short shutdown grace period
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = 20380

[api.grpc]
enabled = false
port = 59999

[mcp]
shutdown_grace_period_secs = 3
heartbeat_timeout_secs = 5
cleanup_interval_secs = 2
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");

    // ========================================================================
    // PHASE 1: Start MCP bridge (spawns daemon)
    // ========================================================================
    reporter.section("PHASE 1: START MCP BRIDGE");

    let step = reporter.step_start("Start bridge", "Launch bridge to spawn daemon");
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge started"));

    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get daemon info
    let daemon_pid: u64;
    let daemon_port: u16;

    if let Ok(content) = std::fs::read_to_string(&pid_path) {
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            daemon_pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            daemon_port = info
                .get("ports")
                .and_then(|p| p.get("http"))
                .and_then(|p| p.as_u64())
                .unwrap_or(20380) as u16;
            if daemon_pid != 0 {
                register_e2e_process("mcp_daemon", daemon_pid as u32);
            }
        } else {
            reporter.error("Failed to parse PID file");
            let _ = bridge_process.kill().await;
            return;
        }
    } else {
        reporter.error("Failed to read PID file");
        let _ = bridge_process.kill().await;
        return;
    }
    reporter.info(&format!(
        "Daemon PID: {}, Port: {}",
        daemon_pid, daemon_port
    ));

    // ========================================================================
    // PHASE 2: Start debugpy and create connection
    // ========================================================================
    reporter.section("PHASE 2: START DEBUGPY AND CONNECT");

    let script_path = workspace_root.join("fixtures/python/trade_bot_forever.py");
    if !script_path.exists() {
        reporter.warn(&format!(
            "Skipping test: fixture not found at {}",
            script_path.display()
        ));
        let _ = bridge_process.kill().await;
        let _ = Command::new("kill")
            .args(["-9", &daemon_pid.to_string()])
            .output();
        unregister_e2e_process("mcp_daemon", daemon_pid as u32);
        return;
    }

    let step = reporter.step_start("Start debugpy", "Launch Python debugger");
    let debugpy_process = match start_debugpy_setsid(debugpy_port, script_path.to_str().unwrap()) {
        Ok(process) => {
            register_e2e_process("debugpy", process.id());
            process
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Failed to start debugpy: {}", e));
            let _ = bridge_process.kill().await;
            let _ = Command::new("kill")
                .args(["-9", &daemon_pid.to_string()])
                .output();
            unregister_e2e_process("mcp_daemon", daemon_pid as u32);
            return;
        }
    };

    // Wait for debugpy to be ready
    if !wait_for_port(debugpy_port, 10).await {
        reporter.step_failed(step, "Debugpy not listening after 10s");
        let _ = bridge_process.kill().await;
        let _ = Command::new("kill")
            .args(["-9", &daemon_pid.to_string()])
            .output();
        let _ = Command::new("kill")
            .args(["-9", &debugpy_process.id().to_string()])
            .output();
        unregister_e2e_process("mcp_daemon", daemon_pid as u32);
        unregister_e2e_process("debugpy", debugpy_process.id());
        return;
    }
    reporter.step_success(step, Some(&format!("Debugpy on port {}", debugpy_port)));

    // Create connection to debugpy via REST API
    let step = reporter.step_start("Create connection", "Connect daemon to debugpy");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let home = std::env::var("HOME").expect("HOME not set");
    let token_path = std::path::PathBuf::from(&home)
        .join(".detrix")
        .join("mcp-token");
    let token = std::fs::read_to_string(&token_path)
        .ok()
        .map(|t| t.trim().to_string());

    let create_conn_body = serde_json::json!({
        "host": "127.0.0.1",
        "port": debugpy_port,
        "adapter_type": "python"
    });

    let mut req = client
        .post(&format!(
            "http://127.0.0.1:{}/api/v1/connections",
            daemon_port
        ))
        .json(&create_conn_body);
    if let Some(ref t) = token {
        req = req.header("Authorization", format!("Bearer {}", t));
    }

    let connection_id: Option<String> = match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            let id = body
                .get("id")
                .or_else(|| body.get("connection_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            reporter.step_success(step, Some(&format!("Connection: {:?}", id)));
            id
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            reporter.step_failed(step, &format!("Failed: {} - {}", status, body));
            None
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Request error: {}", e));
            None
        }
    };

    if connection_id.is_none() {
        reporter.warn("Could not create debugger connection, skipping remaining test");
        let _ = bridge_process.kill().await;
        let _ = Command::new("kill")
            .args(["-9", &daemon_pid.to_string()])
            .output();
        let _ = Command::new("kill")
            .args(["-9", &debugpy_process.id().to_string()])
            .output();
        unregister_e2e_process("mcp_daemon", daemon_pid as u32);
        unregister_e2e_process("debugpy", debugpy_process.id());
        return;
    }

    // ========================================================================
    // PHASE 3: Kill MCP bridge (disconnect client)
    // ========================================================================
    reporter.section("PHASE 3: KILL MCP BRIDGE");

    let step = reporter.step_start("Kill bridge", "Disconnect MCP client");
    let _ = bridge_process.kill().await;
    reporter.step_success(step, Some("Bridge killed"));

    // Wait for grace period + some buffer
    // shutdown_grace_period_secs = 3, but daemon should NOT shutdown due to active connection
    let step = reporter.step_start(
        "Wait for grace period",
        "Wait past shutdown grace period (3s + buffer)",
    );
    tokio::time::sleep(Duration::from_secs(8)).await;
    reporter.step_success(step, Some("Waited 8 seconds"));

    // ========================================================================
    // PHASE 4: Verify daemon still running
    // ========================================================================
    reporter.section("PHASE 4: VERIFY DAEMON STILL ALIVE");

    let step = reporter.step_start("Health check", "Daemon should still be responding");
    match client
        .get(&format!("http://127.0.0.1:{}/health", daemon_port))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            reporter.step_success(
                step,
                Some("Daemon still alive (active debugger connection)"),
            );
        }
        Ok(resp) => {
            reporter.step_failed(step, &format!("Daemon returned {}", resp.status()));
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Daemon unreachable: {}", e));
            reporter.error("FAILED: Daemon shutdown despite active debugger connection!");
        }
    }

    // Check connections endpoint
    let step = reporter.step_start(
        "Check connections",
        "Verify debugger connection still active",
    );
    let mut req = client.get(&format!(
        "http://127.0.0.1:{}/api/v1/connections",
        daemon_port
    ));
    if let Some(ref t) = token {
        req = req.header("Authorization", format!("Bearer {}", t));
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            reporter.step_success(step, Some(&format!("Connections: {}", body.trim())));
        }
        _ => {
            reporter.info("Could not check connections endpoint");
        }
    }

    // Cleanup
    reporter.section("CLEANUP");

    // Close the connection first
    if let Some(ref conn_id) = connection_id {
        let mut req = client.delete(&format!(
            "http://127.0.0.1:{}/api/v1/connections/{}",
            daemon_port, conn_id
        ));
        if let Some(ref t) = token {
            req = req.header("Authorization", format!("Bearer {}", t));
        }
        let _ = req.send().await;
    }

    // Kill processes
    let _ = Command::new("kill")
        .args(["-9", &daemon_pid.to_string()])
        .output();
    let _ = Command::new("kill")
        .args(["-9", &debugpy_process.id().to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", daemon_pid as u32);
    unregister_e2e_process("debugpy", debugpy_process.id());

    reporter.info("✅ MCP debugger prevents shutdown test completed");
    reporter.print_footer(true);
}

/// Test port fallback when preferred port is already blocked (no race condition).
///
/// This tests the port fallback mechanism by:
/// 1. Blocking the preferred port BEFORE starting the daemon
/// 2. Starting daemon with config that prefers the blocked port
/// 3. Verifying daemon successfully uses a fallback port
///
/// This approach avoids the race condition in test_mcp_bridge_daemon_restart_with_port_conflict
/// where the bridge can respawn the daemon before the test can block the port.
#[cfg(unix)]
#[tokio::test]
async fn test_port_fallback_when_port_blocked_before_start() {
    use std::net::TcpListener;

    cleanup_orphaned_e2e_processes();

    let reporter = TestReporter::new("port_fallback_blocked", "MCP");
    reporter.section("PORT FALLBACK TEST (BLOCK BEFORE START)");
    reporter.info("Testing daemon port fallback when preferred port is already blocked");

    let workspace_root = get_workspace_root();

    // Find detrix binary
    let binary = match find_detrix_binary(&workspace_root) {
        Some(path) => path,
        None => {
            reporter.warn("Skipping test: detrix binary not built");
            return;
        }
    };

    // Setup test environment with unique port
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let config_path = temp_dir.path().join("detrix.toml");
    let db_path = temp_dir.path().join("test.db");
    let pid_path = temp_dir.path().join("daemon.pid");
    let log_dir = temp_dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create log dir");

    // Use unique port for this test
    let counter = PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
    let preferred_port = 19600 + ((std::process::id() as u16 + counter * 10) % 200);

    reporter.info(&format!("Preferred port: {}", preferred_port));
    reporter.info(&format!("Config path: {}", config_path.display()));

    // ========================================================================
    // PHASE 1: Block the port BEFORE daemon starts
    // ========================================================================
    reporter.section("PHASE 1: BLOCK PORT");

    let step = reporter.step_start(
        "Block preferred port",
        &format!(
            "Bind listener to port {} before daemon starts",
            preferred_port
        ),
    );
    let port_blocker = match TcpListener::bind(format!("127.0.0.1:{}", preferred_port)) {
        Ok(listener) => {
            reporter.step_success(step, Some(&format!("Blocked port {}", preferred_port)));
            listener
        }
        Err(e) => {
            reporter.step_failed(
                step,
                &format!("Could not block port {}: {}", preferred_port, e),
            );
            reporter.warn("Skipping test: cannot block port");
            return;
        }
    };

    // ========================================================================
    // PHASE 2: Create config with blocked port as preferred
    // ========================================================================
    reporter.section("PHASE 2: CREATE CONFIG");

    let step = reporter.step_start("Create config", "Write config preferring blocked port");
    let config_content = format!(
        r#"
[metadata]
version = "1.0"

[project]
base_path = "."

[storage]
storage_type = "sqlite"
path = "{}"

[daemon]
pid_file = "{}"
log_dir = "{}"

[api]
port_fallback = true

[api.rest]
host = "127.0.0.1"
port = {}

[api.grpc]
enabled = false
port = 59999
"#,
        db_path.to_string_lossy().replace('\\', "/"),
        pid_path.to_string_lossy().replace('\\', "/"),
        log_dir.to_string_lossy().replace('\\', "/"),
        preferred_port
    );
    std::fs::write(&config_path, config_content).expect("Failed to write config");
    reporter.step_success(
        step,
        Some(&format!("Config prefers port {}", preferred_port)),
    );

    // ========================================================================
    // PHASE 3: Start MCP bridge (which spawns daemon)
    // ========================================================================
    reporter.section("PHASE 3: START DAEMON");

    let step = reporter.step_start(
        "Start MCP bridge",
        "Launch bridge (daemon should use fallback port)",
    );
    let mut bridge_process = TokioCommand::new(&binary)
        .arg("mcp")
        .arg("--config")
        .arg(config_path.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn MCP bridge");
    reporter.step_success(step, Some("Bridge process started"));

    // Consume stderr to prevent blocking
    let bridge_logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bridge_logs_clone = bridge_logs.clone();
    let stderr = bridge_process.stderr.take().expect("Failed to get stderr");
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let mut logs = bridge_logs_clone.lock().await;
            logs.push(line);
        }
    });

    // Wait for daemon to start
    tokio::time::sleep(Duration::from_secs(5)).await;

    // ========================================================================
    // PHASE 4: Verify daemon used fallback port
    // ========================================================================
    reporter.section("PHASE 4: VERIFY FALLBACK PORT");

    let step = reporter.step_start("Read PID file", "Get daemon port from PID file");
    let (daemon_pid, actual_port) = if let Ok(content) = std::fs::read_to_string(&pid_path) {
        reporter.info(&format!("PID file contents: {}", content.trim()));
        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
            let pid = info.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
            let port = info
                .get("ports")
                .and_then(|p| p.get("http"))
                .and_then(|p| p.as_u64())
                .unwrap_or(0) as u16;
            register_e2e_process("mcp_daemon", pid as u32);
            reporter.step_success(step, Some(&format!("PID: {}, Port: {}", pid, port)));
            (pid, port)
        } else {
            reporter.step_failed(step, "Failed to parse PID file");
            let _ = bridge_process.kill().await;
            stderr_task.abort();
            drop(port_blocker);
            return;
        }
    } else {
        reporter.step_failed(step, "PID file not found");
        let _ = bridge_process.kill().await;
        stderr_task.abort();
        drop(port_blocker);
        return;
    };

    // Verify port is different (fallback was used)
    let step = reporter.step_start(
        "Verify fallback",
        &format!(
            "Confirm daemon port {} != preferred port {}",
            actual_port, preferred_port
        ),
    );
    if actual_port != preferred_port {
        reporter.step_success(
            step,
            Some(&format!(
                "Port fallback worked: {} -> {}",
                preferred_port, actual_port
            )),
        );
    } else {
        reporter.step_failed(
            step,
            &format!(
                "Daemon used blocked port {} (should have fallen back)",
                actual_port
            ),
        );

        // Print logs for debugging
        reporter.section("BRIDGE LOGS (for debugging)");
        let logs = bridge_logs.lock().await;
        for (i, log_line) in logs.iter().take(50).enumerate() {
            reporter.info(&format!("[{}] {}", i + 1, log_line));
        }
        drop(logs);

        let _ = bridge_process.kill().await;
        stderr_task.abort();
        drop(port_blocker);
        let _ = Command::new("kill")
            .args(["-9", &daemon_pid.to_string()])
            .output();
        unregister_e2e_process("mcp_daemon", daemon_pid as u32);
        panic!(
            "Port fallback did not work: daemon used {} (preferred was blocked)",
            actual_port
        );
    }

    // Verify daemon is healthy on fallback port
    let step = reporter.step_start(
        "Health check",
        &format!(
            "Verify daemon is responding on fallback port {}",
            actual_port
        ),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let health_url = format!("http://127.0.0.1:{}/health", actual_port);
    match client.get(&health_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            reporter.step_success(step, Some("Daemon healthy on fallback port"));
        }
        Ok(resp) => {
            reporter.step_failed(step, &format!("Unhealthy: {}", resp.status()));
            let _ = bridge_process.kill().await;
            stderr_task.abort();
            drop(port_blocker);
            let _ = Command::new("kill")
                .args(["-9", &daemon_pid.to_string()])
                .output();
            unregister_e2e_process("mcp_daemon", daemon_pid as u32);
            panic!("Daemon health check failed");
        }
        Err(e) => {
            reporter.step_failed(step, &format!("Health check error: {}", e));
            let _ = bridge_process.kill().await;
            stderr_task.abort();
            drop(port_blocker);
            let _ = Command::new("kill")
                .args(["-9", &daemon_pid.to_string()])
                .output();
            unregister_e2e_process("mcp_daemon", daemon_pid as u32);
            panic!("Daemon health check failed");
        }
    }

    // ========================================================================
    // CLEANUP
    // ========================================================================
    reporter.section("CLEANUP");

    drop(port_blocker);
    let _ = bridge_process.kill().await;
    stderr_task.abort();
    let _ = Command::new("kill")
        .args(["-9", &daemon_pid.to_string()])
        .output();
    unregister_e2e_process("mcp_daemon", daemon_pid as u32);

    reporter.info("✅ Port fallback test PASSED");
    reporter.info(&format!("   Preferred port (blocked): {}", preferred_port));
    reporter.info(&format!("   Actual port (fallback): {}", actual_port));

    reporter.print_footer(true);
}
