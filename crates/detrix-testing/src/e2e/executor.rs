//! Test Executor - Manages test infrastructure (daemon, debugpy)
//!
//! Provides utilities for starting/stopping the Detrix daemon and debugpy
//! processes needed for E2E tests.
//!
//! # Binary Location
//!
//! The detrix binary is searched for in this order:
//! 1. `DETRIX_BIN` env var (explicit path to binary)
//! 2. `CARGO_TARGET_DIR` env var (set by Cargo)
//! 3. `~/detrix/target` (from .cargo/config.toml convention)
//! 4. `workspace_root/target` (default Cargo location)
//!
//! # Workspace Root
//!
//! Use [`get_workspace_root()`] to get the workspace root directory.
//! This function computes the root based on `CARGO_MANIFEST_DIR`.

use detrix_config::DEFAULT_DAEMON_POLL_INTERVAL_MS;
use std::fs;
use std::io::Write as IoWrite;
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Directory for storing test process PID files
const E2E_PID_DIR: &str = "/tmp/detrix_e2e_pids";

/// Track if PID cleanup has been run this session
static E2E_CLEANUP_DONE: AtomicBool = AtomicBool::new(false);

/// Get the workspace root directory
///
/// Computes workspace root by going up two levels from CARGO_MANIFEST_DIR.
/// This is the standard pattern for a workspace with `crates/` subdirectory.
///
/// # Example
/// ```
/// use detrix_testing::e2e::executor::get_workspace_root;
/// let root = get_workspace_root();
/// // root will be something like /path/to/detrix
/// ```
pub fn get_workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or(manifest_dir)
}

/// Clean up orphaned test processes from previous runs.
///
/// This should be called at the start of each test session. It reads PID files
/// from previous runs and kills any processes that are still running.
///
/// # Safety
///
/// Only kills processes that have PID files in the tracking directory, ensuring
/// we only clean up our own test processes.
#[cfg(unix)]
pub fn cleanup_orphaned_e2e_processes() {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    // Only run cleanup once per test session
    if E2E_CLEANUP_DONE.swap(true, Ordering::SeqCst) {
        return;
    }

    let pid_dir = PathBuf::from(E2E_PID_DIR);
    if !pid_dir.exists() {
        // Create the directory for future use
        fs::create_dir_all(&pid_dir).ok();
        return;
    }

    // Read all PID files and kill orphaned processes
    if let Ok(entries) = fs::read_dir(&pid_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "pid") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(pid) = content.trim().parse::<i32>() {
                        let nix_pid = Pid::from_raw(pid);

                        // Check if process is still running using nix (silent, no stderr output)
                        // Signal 0 doesn't kill but checks if process exists
                        let exists = kill(nix_pid, None).is_ok();

                        if exists {
                            eprintln!(
                                "[e2e cleanup] Killing orphaned process {} from {:?}",
                                pid,
                                path.file_name()
                            );
                            // Use nix for silent kill (no "No such process" stderr)
                            let _ = kill(nix_pid, Signal::SIGKILL);
                        }
                    }
                }
                // Remove the PID file regardless (prevents future attempts)
                fs::remove_file(&path).ok();
            }
        }
    }
}

/// Clean up orphaned test processes (Windows stub - no-op)
#[cfg(windows)]
pub fn cleanup_orphaned_e2e_processes() {
    // Only run cleanup once per test session
    if E2E_CLEANUP_DONE.swap(true, Ordering::SeqCst) {
        return;
    }

    let pid_dir = PathBuf::from(E2E_PID_DIR);
    fs::create_dir_all(&pid_dir).ok();
}

/// Register a test process for cleanup tracking.
///
/// Creates a PID file that will be used to clean up orphaned processes
/// from crashed test runs.
pub fn register_e2e_process(name: &str, pid: u32) {
    let pid_dir = PathBuf::from(E2E_PID_DIR);
    fs::create_dir_all(&pid_dir).ok();

    let pid_file = pid_dir.join(format!("{}_{}.pid", name, pid));
    if let Ok(mut file) = fs::File::create(&pid_file) {
        writeln!(file, "{}", pid).ok();
    }
}

/// Unregister a test process (remove its PID file).
///
/// Call this when the process is intentionally killed to prevent
/// future cleanup attempts.
pub fn unregister_e2e_process(name: &str, pid: u32) {
    let pid_file = PathBuf::from(E2E_PID_DIR).join(format!("{}_{}.pid", name, pid));
    fs::remove_file(&pid_file).ok();
}

/// Kill a process and unregister it from E2E cleanup tracking.
///
/// Use this for processes returned by `restart_delve` or `restart_lldb`.
/// The `name` should match what was used during registration ("delve" or "lldb").
pub fn kill_and_unregister_process(name: &str, process: &mut Option<Child>) {
    if let Some(mut p) = process.take() {
        let pid = p.id();
        let _ = p.kill();
        let _ = p.wait();
        unregister_e2e_process(name, pid);
    }
}

/// Async version of `kill_and_unregister_process` for `Arc<Mutex<Option<Child>>>`.
///
/// Use this in async test code where the process is wrapped in `Arc<Mutex>`.
/// The `name` should match what was used during registration ("delve" or "lldb").
pub async fn kill_and_unregister_process_async(
    name: &str,
    process: &std::sync::Arc<tokio::sync::Mutex<Option<Child>>>,
) {
    let mut guard = process.lock().await;
    if let Some(mut p) = guard.take() {
        let pid = p.id();
        let _ = p.kill();
        let _ = p.wait();
        unregister_e2e_process(name, pid);
    }
}

/// Get candidate target directories for finding binaries
///
/// Returns a list of potential target directories in priority order:
/// 1. `CARGO_TARGET_DIR` env var (set by Cargo itself)
/// 2. `~/detrix/target` (from .cargo/config.toml convention)
/// 3. `workspace_root/target` (default Cargo location)
pub fn get_target_candidates(workspace_root: &std::path::Path) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(3);

    // 1. Check CARGO_TARGET_DIR env var (Cargo sets this)
    if let Ok(target_dir) = std::env::var("CARGO_TARGET_DIR") {
        candidates.push(PathBuf::from(target_dir));
    }

    // 2. Expand ~/detrix/target (from .cargo/config.toml)
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("detrix/target"));
    }

    // 3. Fallback to workspace_root/target
    candidates.push(workspace_root.join("target"));

    candidates
}

/// Find the detrix binary in candidate target directories
///
/// Searches for detrix binary in order:
/// 1. `DETRIX_BIN` env var (explicit path)
/// 2. release/detrix in each target candidate
/// 3. debug/detrix in each target candidate
pub fn find_detrix_binary(workspace_root: &std::path::Path) -> Option<PathBuf> {
    // 1. Check explicit DETRIX_BIN env var first
    if let Ok(bin_path) = std::env::var(detrix_config::constants::ENV_DETRIX_BIN) {
        let path = PathBuf::from(bin_path);
        if path.exists() {
            return Some(path);
        }
    }

    // 2. Search in target candidates
    let candidates = get_target_candidates(workspace_root);

    // Prefer release builds, then debug
    for target_dir in &candidates {
        let release_path = target_dir.join("release/detrix");
        if release_path.exists() {
            return Some(release_path);
        }
    }

    for target_dir in &candidates {
        let debug_path = target_dir.join("debug/detrix");
        if debug_path.exists() {
            return Some(debug_path);
        }
    }

    None
}

/// Global cache for built Rust binary - builds once and reuses across all tests
static RUST_TRADING_BOT_PATH: OnceLock<Result<PathBuf, String>> = OnceLock::new();

// Port counters for parallel tests - using random offset to avoid collisions between test runs
static HTTP_PORT_COUNTER: AtomicU16 = AtomicU16::new(0);
static GRPC_PORT_COUNTER: AtomicU16 = AtomicU16::new(0);
static DEBUGPY_PORT_COUNTER: AtomicU16 = AtomicU16::new(0);
static DELVE_PORT_COUNTER: AtomicU16 = AtomicU16::new(0);
static LLDB_PORT_COUNTER: AtomicU16 = AtomicU16::new(0);
static CODELLDB_PORT_COUNTER: AtomicU16 = AtomicU16::new(0);

/// Initialize port counters with random offsets to avoid collisions between test runs
fn init_port_counters() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // Use random offset based on process ID and time to avoid collisions
        let seed = (std::process::id() as u64).wrapping_mul(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(12345),
        );
        let offset = ((seed % 1000) as u16) * 10; // 0-9990 in steps of 10

        // Port ranges must not overlap! With 34 tests incrementing by 10 each,
        // we need at least 340 port gap between ranges.
        // NOTE: Avoid port 7000 which is used by macOS AirPlay Receiver
        HTTP_PORT_COUNTER.store(8000 + offset, Ordering::SeqCst);
        GRPC_PORT_COUNTER.store(50000 + offset, Ordering::SeqCst);
        DEBUGPY_PORT_COUNTER.store(5000 + offset, Ordering::SeqCst);
        DELVE_PORT_COUNTER.store(7100 + offset, Ordering::SeqCst); // Start above 7000 to avoid macOS AirPlay
        LLDB_PORT_COUNTER.store(7600 + offset, Ordering::SeqCst); // 500 gap from delve
        CODELLDB_PORT_COUNTER.store(8100 + offset, Ordering::SeqCst); // 500 gap from lldb
    });
}

/// Check if a port is available for binding
pub fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Ports known to be used by system services that should be skipped
/// - 7000: macOS AirPlay Receiver (ControlCenter)
/// - 5000: macOS AirPlay (older versions)
const SYSTEM_RESERVED_PORTS: &[u16] = &[5000, 7000];

/// Check if a port should be skipped (system reserved)
fn is_system_reserved_port(port: u16) -> bool {
    SYSTEM_RESERVED_PORTS.contains(&port)
}

/// Find an available port starting from the given base, incrementing until one is found.
///
/// This function:
/// - Searches up to 100 ports from the base
/// - Skips known system-reserved ports (e.g., 7000 for macOS AirPlay)
/// - Panics with clear error if no port is available (test infrastructure issue)
fn find_available_port(base: u16, max_attempts: u16) -> u16 {
    // Use larger search range for robustness
    let search_range = max_attempts.max(100);

    for offset in 0..search_range {
        let port = base.saturating_add(offset);

        // Skip system-reserved ports
        if is_system_reserved_port(port) {
            continue;
        }

        if is_port_available(port) {
            return port;
        }
    }

    // Panic with clear message - this is a test infrastructure issue
    panic!(
        "Could not find available port in range {}-{}. \
         Ports may be exhausted or blocked by firewall. \
         Try closing other applications or restarting.",
        base,
        base.saturating_add(search_range)
    );
}

/// Get unique HTTP port for tests (ensures port is actually available)
pub fn get_http_port() -> u16 {
    init_port_counters();
    let base = HTTP_PORT_COUNTER.fetch_add(10, Ordering::SeqCst);
    find_available_port(base, 10)
}

/// Get unique gRPC port for tests (ensures port is actually available)
pub fn get_grpc_port() -> u16 {
    init_port_counters();
    let base = GRPC_PORT_COUNTER.fetch_add(10, Ordering::SeqCst);
    find_available_port(base, 10)
}

/// Get unique debugpy port for tests (ensures port is actually available)
pub fn get_debugpy_port() -> u16 {
    init_port_counters();
    let base = DEBUGPY_PORT_COUNTER.fetch_add(10, Ordering::SeqCst);
    find_available_port(base, 10)
}

/// Get unique delve port for tests (ensures port is actually available)
pub fn get_delve_port() -> u16 {
    init_port_counters();
    let base = DELVE_PORT_COUNTER.fetch_add(10, Ordering::SeqCst);
    find_available_port(base, 10)
}

/// Get unique lldb-dap port for tests (ensures port is actually available)
pub fn get_lldb_port() -> u16 {
    init_port_counters();
    let base = LLDB_PORT_COUNTER.fetch_add(10, Ordering::SeqCst);
    find_available_port(base, 10)
}

/// Get unique CodeLLDB port for tests (ensures port is actually available)
pub fn get_codelldb_port() -> u16 {
    init_port_counters();
    let base = CODELLDB_PORT_COUNTER.fetch_add(10, Ordering::SeqCst);
    find_available_port(base, 10)
}

/// Kill any process listening on the specified port
/// This is a standalone version for use in callbacks
#[cfg(unix)]
pub fn kill_listening_process_on_port(port: u16) {
    if let Ok(output) = Command::new("lsof")
        .args(["-sTCP:LISTEN", "-i", &format!("TCP:{}", port)])
        .output()
    {
        if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);
            for line in output_str.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let command = parts[0].to_lowercase();
                    let pid_str = parts[1];

                    // Only kill processes we expect
                    let is_our_process = command.starts_with("detrix")
                        || command.starts_with("python")
                        || command.starts_with("dlv")
                        || command.starts_with("lldb");

                    if is_our_process {
                        if let Ok(pid) = pid_str.parse::<i32>() {
                            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).output();
                        }
                    }
                }
            }
        }
    }
}

/// Kill any process listening on the specified port (Windows stub - not implemented)
#[cfg(windows)]
pub fn kill_listening_process_on_port(_port: u16) {
    // Windows implementation not available - E2E tests require Unix
}

/// Restart delve debugger on a given port
/// Used for reconnection tests where delve needs to be restarted between cycles
///
/// The returned process is registered for E2E cleanup tracking. Use
/// `kill_and_unregister_process` to properly clean up when done.
pub async fn restart_delve(port: u16, binary_path: &str) -> Result<Child, String> {
    // Kill any existing process
    kill_listening_process_on_port(port);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify port is available
    if !is_port_available(port) {
        return Err(format!("Port {} still in use after cleanup", port));
    }

    // Start new delve instance
    match start_delve_dap(port, binary_path) {
        Ok(process) => {
            // Register for E2E cleanup tracking
            register_e2e_process("delve", process.id());

            // Wait for it to be ready
            if wait_for_port(port, 15).await {
                Ok(process)
            } else {
                Err(format!(
                    "delve not listening on port {} after restart",
                    port
                ))
            }
        }
        Err(e) => Err(format!("Failed to restart delve: {}", e)),
    }
}

/// Restart lldb-serve on a given port
/// Used for reconnection tests where lldb needs to be restarted between cycles
///
/// The returned process is registered for E2E cleanup tracking. Use
/// `kill_and_unregister_process` to properly clean up when done.
///
/// # Arguments
/// * `port` - Port for lldb-serve to listen on
/// * `binary_path` - Path to the Rust binary to debug
/// * `workspace_root` - Workspace root for finding detrix binary
pub async fn restart_lldb(
    port: u16,
    binary_path: &str,
    workspace_root: &std::path::Path,
) -> Result<Child, String> {
    // Kill any existing process
    kill_listening_process_on_port(port);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify port is available
    if !is_port_available(port) {
        return Err(format!("Port {} still in use after cleanup", port));
    }

    // Get the detrix binary path for running lldb-serve
    let detrix_binary = find_detrix_binary(workspace_root)
        .ok_or_else(|| "Failed to find detrix binary".to_string())?;

    // Start new lldb-serve instance
    match start_lldb_serve(port, binary_path, &detrix_binary) {
        Ok(process) => {
            // Register for E2E cleanup tracking
            register_e2e_process("lldb", process.id());

            // Wait for it to be ready
            if wait_for_port(port, 15).await {
                Ok(process)
            } else {
                Err(format!(
                    "lldb-serve not listening on port {} after restart",
                    port
                ))
            }
        }
        Err(e) => Err(format!("Failed to restart lldb-serve: {}", e)),
    }
}

/// Wait for TCP port using lsof (doesn't consume the connection)
#[cfg(unix)]
pub async fn wait_for_port(port: u16, timeout_secs: u64) -> bool {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout {
        let output = Command::new("lsof")
            .args(["-i", &format!(":{}", port), "-sTCP:LISTEN"])
            .output();

        if let Ok(out) = output {
            if out.status.success() && !out.stdout.is_empty() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// Wait for TCP port (Windows version - uses TcpStream connection attempt)
#[cfg(windows)]
pub async fn wait_for_port(port: u16, timeout_secs: u64) -> bool {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// Start debugpy in a new process session using setsid
///
/// This is required for DAP protocol to work correctly - creates a new
/// process group/session, which is what bash does with background processes.
#[cfg(unix)]
pub fn start_debugpy_setsid(port: u16, script_path: &str) -> Result<Child, std::io::Error> {
    let log_file = format!("/tmp/debugpy_e2e_{}.log", port);

    // Open log file for stdout/stderr redirection
    let log = std::fs::File::create(&log_file)?;
    let log_stderr = log.try_clone()?;

    let mut cmd = Command::new("python");
    cmd.args([
        "-Xfrozen_modules=off", // Disable frozen modules to prevent debugpy breakpoint issues
        "-m",
        "debugpy",
        "--listen",
        &port.to_string(),
        "--wait-for-client",
        script_path,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::from(log))
    .stderr(Stdio::from(log_stderr))
    .process_group(0); // Create new process group to isolate from parent signals

    cmd.spawn()
}

/// Start debugpy (Windows stub - E2E tests not supported on Windows)
#[cfg(windows)]
pub fn start_debugpy_setsid(_port: u16, _script_path: &str) -> Result<Child, std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "E2E tests with debugpy are not supported on Windows",
    ))
}

/// Start delve in headless mode for Go programs
///
/// Uses `dlv exec --headless` to start Delve with the binary.
/// The Go binary must be built with debug symbols: `go build -gcflags='all=-N -l'`
///
/// NOTE: We use `dlv exec --headless` instead of `dlv dap` because:
/// - `dlv dap` expects the binary to be launched via DAP launch request
/// - `dlv exec --headless` launches the binary immediately and accepts DAP connections
#[cfg(unix)]
pub fn start_delve_dap(port: u16, binary_path: &str) -> Result<Child, std::io::Error> {
    let log_file = format!("/tmp/delve_e2e_{}.log", port);
    let log = std::fs::File::create(&log_file)?;
    let log_stderr = log.try_clone()?;

    let mut cmd = Command::new("dlv");
    cmd.args([
        "exec",
        binary_path,
        "--headless",
        &format!("--listen=:{}", port),
        "--api-version=2",
        "--accept-multiclient",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::from(log))
    .stderr(Stdio::from(log_stderr));

    // Set working directory to binary's directory
    if let Some(parent) = std::path::Path::new(binary_path).parent() {
        cmd.current_dir(parent);
    }

    // Create new process group to isolate from parent signals
    cmd.process_group(0);

    cmd.spawn()
}

/// Start delve (Windows stub - E2E tests not supported on Windows)
#[cfg(windows)]
pub fn start_delve_dap(_port: u16, _binary_path: &str) -> Result<Child, std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "E2E tests with delve are not supported on Windows",
    ))
}

/// Start lldb-serve for Rust/C++ debugging (recommended default)
///
/// Uses `detrix lldb-serve` which provides a consistent debugging experience
/// similar to debugpy and delve. The lldb-serve command handles:
/// - Starting lldb-dap as a subprocess
/// - Launching the program through the debugger
/// - Proper type formatters for Rust types
/// - CodeLLDB detection and configuration
///
/// # Architecture
/// ```text
/// Detrix daemon → lldb-serve → lldb-dap (stdio)
/// ```
///
/// # Arguments
/// * `port` - Port for lldb-serve to listen on
/// * `binary_path` - Path to the Rust binary to debug
/// * `detrix_binary` - Path to the detrix binary (for lldb-serve command)
#[cfg(unix)]
pub fn start_lldb_serve(
    port: u16,
    binary_path: &str,
    detrix_binary: &std::path::Path,
) -> Result<Child, std::io::Error> {
    let log_file = format!("/tmp/lldb_dap_e2e_{}.log", port);
    let log = std::fs::File::create(&log_file)?;
    let log_stderr = log.try_clone()?;

    // Start lldb-serve which handles lldb-dap as a subprocess
    let mut cmd = Command::new(detrix_binary);
    cmd.args([
        "lldb-serve",
        binary_path,
        "--listen",
        &format!("127.0.0.1:{}", port),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::from(log))
    .stderr(Stdio::from(log_stderr))
    .process_group(0); // Create new process group to isolate from parent signals

    cmd.spawn()
}

/// Start lldb-serve (Windows)
#[cfg(windows)]
pub fn start_lldb_serve(
    port: u16,
    binary_path: &str,
    detrix_binary: &std::path::Path,
) -> Result<Child, std::io::Error> {
    use std::os::windows::process::CommandExt;

    let log_file = format!("lldb_dap_e2e_{}.log", port);
    let log = std::fs::File::create(&log_file)?;
    let log_stderr = log.try_clone()?;

    let mut cmd = Command::new(detrix_binary);
    cmd.args([
        "lldb-serve",
        binary_path,
        "--listen",
        &format!("127.0.0.1:{}", port),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::from(log))
    .stderr(Stdio::from(log_stderr))
    .creation_flags(0x08000000); // CREATE_NO_WINDOW

    cmd.spawn()
}

/// Find CodeLLDB adapter binary path
///
/// Searches in order:
/// 1. VSCode extensions directories
/// 2. Detrix's auto-download location
pub fn find_codelldb_path() -> Option<PathBuf> {
    // Check VSCode extensions directories
    #[cfg(target_os = "windows")]
    let home_var = "USERPROFILE";
    #[cfg(not(target_os = "windows"))]
    let home_var = "HOME";

    if let Ok(home) = std::env::var(home_var) {
        #[cfg(target_os = "linux")]
        let extension_dirs = [
            format!("{}/.vscode/extensions", home),
            format!("{}/.vscode-insiders/extensions", home),
            format!("{}/.vscode-oss/extensions", home),
        ];

        #[cfg(not(target_os = "linux"))]
        let extension_dirs = [
            format!("{}/.vscode/extensions", home),
            format!("{}/.vscode-insiders/extensions", home),
        ];

        for ext_dir in &extension_dirs {
            let ext_path = PathBuf::from(ext_dir);
            if !ext_path.exists() {
                continue;
            }

            if let Ok(entries) = std::fs::read_dir(&ext_path) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with("vadimcn.vscode-lldb") {
                        #[cfg(target_os = "windows")]
                        let binary_name = "codelldb.exe";
                        #[cfg(not(target_os = "windows"))]
                        let binary_name = "codelldb";

                        let adapter_path = entry.path().join("adapter").join(binary_name);
                        if adapter_path.exists() {
                            return Some(adapter_path);
                        }
                    }
                }
            }
        }
    }

    // Check Detrix's auto-download location
    #[cfg(target_os = "windows")]
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("USERPROFILE")
                .map(|h| PathBuf::from(h).join("AppData").join("Local"))
                .unwrap_or_else(|_| PathBuf::from("."))
        });

    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
        .unwrap_or_else(|_| PathBuf::from("."));

    #[cfg(target_os = "linux")]
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|_| PathBuf::from("."));

    let adapters_dir = base.join("detrix").join("debug_adapters").join("CodeLLDB");
    if adapters_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&adapters_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    #[cfg(target_os = "windows")]
                    let binary_name = "codelldb.exe";
                    #[cfg(not(target_os = "windows"))]
                    let binary_name = "codelldb";

                    let binary_path = path.join("extension").join("adapter").join(binary_name);
                    if binary_path.exists() {
                        return Some(binary_path);
                    }
                }
            }
        }
    }

    None
}

/// Start lldb-serve with CodeLLDB as the debug adapter (Unix)
///
/// CodeLLDB has better PDB support on Windows compared to standard lldb-dap.
/// On macOS/Linux, it can be used as an alternative to lldb-dap.
///
/// Uses lldb-serve as a proxy because CodeLLDB's native TCP mode uses WebSocket
/// protocol which is different from standard DAP over TCP.
///
/// # Arguments
/// * `port` - Port for lldb-serve to listen on
/// * `binary_path` - Path to the Rust binary to debug
/// * `detrix_binary` - Path to the detrix binary (for lldb-serve command)
#[cfg(unix)]
pub fn start_lldb_serve_with_codelldb(
    port: u16,
    binary_path: &str,
    detrix_binary: &std::path::Path,
) -> Result<Child, std::io::Error> {
    let codelldb_path = find_codelldb_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "CodeLLDB not found. Install the CodeLLDB VSCode extension or let Detrix auto-download it.",
        )
    })?;

    let log_file = format!("/tmp/codelldb_e2e_{}.log", port);
    let log = std::fs::File::create(&log_file)?;
    let log_stderr = log.try_clone()?;

    // Start lldb-serve with CodeLLDB as the adapter
    // lldb-serve handles the protocol translation and adds proper settings for Rust
    let mut cmd = Command::new(detrix_binary);
    cmd.args([
        "lldb-serve",
        binary_path,
        "--listen",
        &format!("127.0.0.1:{}", port),
        "--lldb-dap-path",
        codelldb_path.to_str().unwrap(),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::from(log))
    .stderr(Stdio::from(log_stderr))
    .process_group(0); // Create new process group to isolate from parent signals

    cmd.spawn()
}

/// Start lldb-serve with CodeLLDB (Windows)
#[cfg(windows)]
pub fn start_lldb_serve_with_codelldb(
    port: u16,
    binary_path: &str,
    detrix_binary: &std::path::Path,
) -> Result<Child, std::io::Error> {
    use std::os::windows::process::CommandExt;

    let codelldb_path = find_codelldb_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "CodeLLDB not found. Install the CodeLLDB VSCode extension or let Detrix auto-download it.",
        )
    })?;

    let log_file = format!("codelldb_e2e_{}.log", port);
    let log = std::fs::File::create(&log_file)?;
    let log_stderr = log.try_clone()?;

    let mut cmd = Command::new(detrix_binary);
    cmd.args([
        "lldb-serve",
        binary_path,
        "--listen",
        &format!("127.0.0.1:{}", port),
        "--lldb-dap-path",
        codelldb_path.to_str().unwrap(),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::from(log))
    .stderr(Stdio::from(log_stderr))
    .creation_flags(0x08000000); // CREATE_NO_WINDOW

    cmd.spawn()
}

/// Supported debugger languages
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebuggerLanguage {
    Python,
    Go,
    Rust,
}

/// Test executor for managing daemon and debugger processes
pub struct TestExecutor {
    pub temp_dir: tempfile::TempDir,
    pub http_port: u16,
    pub grpc_port: u16,
    pub debugpy_port: u16,
    pub delve_port: u16,
    pub lldb_port: u16,
    pub codelldb_port: u16,
    pub debugpy_process: Option<Child>,
    pub delve_process: Option<Child>,
    pub lldb_process: Option<Child>,
    pub codelldb_process: Option<Child>,
    pub daemon_process: Option<Child>,
    pub daemon_log_path: Option<PathBuf>,
    /// Path to daemon PID file (for killing forked daemon)
    pub daemon_pid_path: Option<PathBuf>,
    pub workspace_root: PathBuf,
    /// PIDs of child processes spawned by debuggers (detrix_example_app, etc.)
    /// These need to be explicitly killed during cleanup
    child_pids: Vec<u32>,
    /// Path to Rust binary for lldb-dap direct connection mode.
    /// With direct TCP, the daemon sends this path in the launch request.
    pub rust_binary_path: Option<PathBuf>,
}

impl TestExecutor {
    pub fn new() -> Self {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| manifest_dir.clone());

        // Clean up orphaned processes from previous test runs using PID files
        // This is the primary cleanup mechanism for crashed tests
        cleanup_orphaned_e2e_processes();

        // Also clean up stale orphaned processes (PPID=1) as a fallback
        // This catches processes not tracked by PID files
        Self::cleanup_stale_orphans();

        Self {
            temp_dir: tempfile::TempDir::new().expect("Failed to create temp dir"),
            http_port: get_http_port(),
            grpc_port: get_grpc_port(),
            debugpy_port: get_debugpy_port(),
            delve_port: get_delve_port(),
            lldb_port: get_lldb_port(),
            codelldb_port: get_codelldb_port(),
            debugpy_process: None,
            delve_process: None,
            lldb_process: None,
            codelldb_process: None,
            daemon_process: None,
            daemon_log_path: None,
            daemon_pid_path: None,
            workspace_root,
            child_pids: Vec::new(),
            rust_binary_path: None,
        }
    }

    /// Clean up stale orphaned processes from previous test runs
    ///
    /// Only cleans up processes that are orphaned (parent = 1) to avoid
    /// interfering with currently running tests.
    #[cfg(unix)]
    fn cleanup_stale_orphans() {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        // Find orphaned detrix_example_app processes (parent PID = 1)
        // These are safe to kill because they're from previous test runs
        if let Ok(output) = Command::new("pgrep")
            .args(["-f", "detrix_example_app"])
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if let Ok(pid) = line.trim().parse::<u32>() {
                        // Check if this process is orphaned (PPID = 1)
                        if let Ok(ppid_out) = Command::new("ps")
                            .args(["-o", "ppid=", "-p", &pid.to_string()])
                            .output()
                        {
                            let ppid = String::from_utf8_lossy(&ppid_out.stdout)
                                .trim()
                                .parse::<u32>()
                                .unwrap_or(0);
                            if ppid == 1 {
                                // Orphaned - safe to kill
                                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Clean up stale orphaned processes (Windows stub - no-op)
    #[cfg(windows)]
    fn cleanup_stale_orphans() {
        // Not implemented on Windows
    }

    /// Get path to detrix_example_app.py example
    pub fn detrix_example_app_path(&self) -> Option<PathBuf> {
        let script_path = self
            .workspace_root
            .join("fixtures/python/detrix_example_app.py");
        if script_path.exists() {
            Some(script_path)
        } else {
            None
        }
    }

    /// Get path to Go detrix_example_app example
    pub fn go_detrix_example_app_path(&self) -> Option<PathBuf> {
        let script_path = self
            .workspace_root
            .join("fixtures/go/detrix_example_app.go");
        if script_path.exists() {
            Some(script_path)
        } else {
            None
        }
    }

    /// Get path to Rust detrix_example_app example
    pub fn rust_detrix_example_app_path(&self) -> Option<PathBuf> {
        let script_path = self.workspace_root.join("fixtures/rust/src/main.rs");
        if script_path.exists() {
            Some(script_path)
        } else {
            None
        }
    }

    /// Get the port for a specific debugger language
    pub fn debugger_port(&self, language: DebuggerLanguage) -> u16 {
        match language {
            DebuggerLanguage::Python => self.debugpy_port,
            DebuggerLanguage::Go => self.delve_port,
            DebuggerLanguage::Rust => self.lldb_port,
        }
    }

    /// Print daemon logs (for debugging test failures)
    pub fn print_daemon_logs(&self, last_n_lines: usize) {
        if let Some(ref log_path) = self.daemon_log_path {
            println!("\n=== DAEMON LOG (last {} lines) ===", last_n_lines);
            if let Ok(content) = std::fs::read_to_string(log_path) {
                let lines: Vec<&str> = content.lines().collect();
                // Print first 30 lines (startup) plus last N lines
                println!("--- STARTUP (first 30 lines) ---");
                for line in lines.iter().take(30) {
                    println!("{}", line);
                }
                println!("--- ... ---");
                let start = lines.len().saturating_sub(last_n_lines);
                for line in &lines[start..] {
                    println!("{}", line);
                }
                // Also copy to fixed location for debugging
                let fixed_log = self.workspace_root.join("logs/daemon_test.log");
                if let Some(parent) = fixed_log.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(log_path, &fixed_log);
                println!("--- Log copied to: {} ---", fixed_log.display());
            } else {
                println!("   (could not read daemon log)");
            }
            println!("=================================\n");
        }
    }

    /// Kill debugpy process and unregister from cleanup tracking.
    ///
    /// Use this instead of directly killing `debugpy_process` to ensure
    /// the PID file is properly cleaned up.
    pub fn kill_debugpy(&mut self) {
        if let Some(mut p) = self.debugpy_process.take() {
            let pid = p.id();
            let _ = p.kill();
            let _ = p.wait();
            unregister_e2e_process("debugpy", pid);
        }
    }

    /// Kill delve process and unregister from cleanup tracking.
    ///
    /// Use this instead of directly killing `delve_process` to ensure
    /// the PID file is properly cleaned up.
    pub fn kill_delve(&mut self) {
        if let Some(mut p) = self.delve_process.take() {
            let pid = p.id();

            // Check if delve is still running
            let delve_alive = matches!(p.try_wait(), Ok(None));

            if delve_alive {
                self.track_child_pids(pid);
                self.kill_tracked_children();
                Self::kill_process_tree(pid);
                let _ = p.kill();
                let _ = p.wait();
            }
            unregister_e2e_process("delve", pid);
        }
    }

    /// Kill lldb process and unregister from cleanup tracking.
    ///
    /// Use this instead of directly killing `lldb_process` to ensure
    /// the PID file is properly cleaned up.
    pub fn kill_lldb(&mut self) {
        if let Some(mut p) = self.lldb_process.take() {
            let pid = p.id();
            Self::kill_process_tree(pid);
            let _ = p.kill();
            let _ = p.wait();
            unregister_e2e_process("lldb", pid);
        }
    }

    /// Print debugpy logs (for debugging test failures)
    pub fn print_debugpy_logs(&self, last_n_lines: usize) {
        let log_path = format!("/tmp/debugpy_e2e_{}.log", self.debugpy_port);
        println!("\n=== DEBUGPY LOG (last {} lines) ===", last_n_lines);
        if let Ok(content) = std::fs::read_to_string(&log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read debugpy log)");
        }
        println!("=================================\n");
    }

    /// Start debugpy with --wait-for-client using setsid
    pub async fn start_debugpy(&mut self, script_path: &str) -> Result<(), String> {
        // Kill any process on this port (more reliable than pattern matching)
        self.kill_process_on_port(self.debugpy_port);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Verify port is now available
        if !is_port_available(self.debugpy_port) {
            return Err(format!(
                "Port {} is still in use after cleanup",
                self.debugpy_port
            ));
        }

        match start_debugpy_setsid(self.debugpy_port, script_path) {
            Ok(process) => {
                // Register process for cleanup tracking
                register_e2e_process("debugpy", process.id());
                self.debugpy_process = Some(process);

                // Wait for port to be open
                if wait_for_port(self.debugpy_port, 10).await {
                    Ok(())
                } else {
                    Err(format!(
                        "debugpy not listening on port {} after spawn",
                        self.debugpy_port
                    ))
                }
            }
            Err(e) => Err(format!("Could not spawn debugpy: {}", e)),
        }
    }

    /// Build and start Go debugger (delve)
    ///
    /// Note: The Go source file must be built first with debug symbols:
    /// `go build -gcflags='all=-N -l' -o detrix_example_app detrix_example_app.go`
    pub async fn start_delve(&mut self, source_path: &str) -> Result<(), String> {
        // Kill any process on this port (more reliable than pattern matching)
        self.kill_process_on_port(self.delve_port);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Verify port is now available
        if !is_port_available(self.delve_port) {
            return Err(format!(
                "Port {} is still in use after cleanup",
                self.delve_port
            ));
        }

        // Build Go binary with debug symbols
        let source_file = std::path::Path::new(source_path);
        let parent_dir = source_file.parent().ok_or("Invalid source path")?;
        let binary_name = source_file
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("Invalid file name")?;
        let binary_path = parent_dir.join(binary_name);

        let build_output = Command::new("go")
            .args([
                "build",
                "-gcflags=all=-N -l",
                "-o",
                binary_path.to_str().unwrap(),
                source_path,
            ])
            .current_dir(parent_dir)
            .output()
            .map_err(|e| format!("Failed to build Go binary: {}", e))?;

        if !build_output.status.success() {
            return Err(format!(
                "Go build failed: {}",
                String::from_utf8_lossy(&build_output.stderr)
            ));
        }

        match start_delve_dap(self.delve_port, binary_path.to_str().unwrap()) {
            Ok(process) => {
                // Register process for cleanup tracking
                register_e2e_process("delve", process.id());
                self.delve_process = Some(process);

                // Wait for port to be open
                if wait_for_port(self.delve_port, 15).await {
                    // Note: child processes (detrix_example_app) are tracked at cleanup time
                    // because they're only spawned after DAP client connects
                    Ok(())
                } else {
                    Err(format!(
                        "delve not listening on port {} after spawn",
                        self.delve_port
                    ))
                }
            }
            Err(e) => Err(format!("Could not spawn delve: {}", e)),
        }
    }

    /// Build and start Rust debugger via lldb-serve (recommended default)
    ///
    /// Compiles the standalone Rust source file with `rustc -g` for debug symbols,
    /// then starts lldb-serve which handles lldb-dap as a subprocess.
    ///
    /// # Architecture
    /// ```text
    /// Detrix daemon → lldb-serve → lldb-dap (stdio)
    /// ```
    ///
    /// This is the recommended approach because:
    /// - Consistent with debugpy and delve architecture
    /// - lldb-serve handles type formatters and CodeLLDB detection
    /// - Works identically across platforms
    pub async fn start_lldb(&mut self, source_path: &str) -> Result<(), String> {
        // Kill any process on this port (more reliable than pattern matching)
        self.kill_process_on_port(self.lldb_port);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Verify port is now available
        if !is_port_available(self.lldb_port) {
            return Err(format!(
                "Port {} is still in use after cleanup",
                self.lldb_port
            ));
        }

        // Build Rust binary once and reuse across all tests
        // The fixture is a Cargo project at fixtures/rust/
        let binary_path = RUST_TRADING_BOT_PATH
            .get_or_init(|| {
                let source_file = std::path::Path::new(source_path);
                // Go from fixtures/rust/src/main.rs -> fixtures/rust
                let fixture_dir = source_file
                    .parent() // src
                    .and_then(|p| p.parent()) // rust
                    .ok_or_else(|| "Invalid source path".to_string())?;
                let binary_path = fixture_dir.join("target/debug/detrix_example_app");

                eprintln!("Building Rust fixture at {:?}", fixture_dir);

                let build_output = Command::new("cargo")
                    .args(["build"])
                    .current_dir(fixture_dir)
                    .output()
                    .map_err(|e| format!("Failed to build Rust binary: {}", e))?;

                if !build_output.status.success() {
                    return Err(format!(
                        "Rust build failed: {}",
                        String::from_utf8_lossy(&build_output.stderr)
                    ));
                }

                // Verify binary was created
                if !binary_path.exists() {
                    return Err(format!(
                        "Rust build succeeded but binary not found at {:?}. Stdout: {}",
                        binary_path,
                        String::from_utf8_lossy(&build_output.stdout)
                    ));
                }

                eprintln!("Rust fixture built successfully: {:?}", binary_path);
                Ok(binary_path)
            })
            .as_ref()
            .map_err(|e| e.clone())?
            .clone();

        // Verify binary still exists (might have been deleted between test runs)
        if !binary_path.exists() {
            return Err(format!(
                "Rust binary not found at {:?}. Run 'cargo build' in fixtures/rust/",
                binary_path
            ));
        }

        // Store binary path for reference
        self.rust_binary_path = Some(binary_path.clone());

        // Get the detrix binary path for running lldb-serve
        let detrix_binary = find_detrix_binary(&self.workspace_root)
            .ok_or_else(|| "Failed to find detrix binary".to_string())?;

        let binary_path_str = binary_path.to_str().ok_or("Invalid binary path")?;

        // Start lldb-serve which handles lldb-dap as a subprocess
        match start_lldb_serve(self.lldb_port, binary_path_str, &detrix_binary) {
            Ok(process) => {
                // Register process for cleanup tracking
                register_e2e_process("lldb", process.id());
                self.lldb_process = Some(process);

                // Wait for port to be open
                if wait_for_port(self.lldb_port, 15).await {
                    Ok(())
                } else {
                    Err(format!(
                        "lldb-serve not listening on port {} after spawn",
                        self.lldb_port
                    ))
                }
            }
            Err(e) => Err(format!("Could not spawn lldb-serve: {}", e)),
        }
    }

    /// Start CodeLLDB debug adapter for Rust debugging (alternative to lldb-dap)
    ///
    /// CodeLLDB (vadimcn's adapter) has better PDB support on Windows.
    /// On macOS/Linux, it can be used as an alternative to standard lldb-dap.
    ///
    /// # Arguments
    /// * `source_path` - Path to Rust source file (will compile it)
    ///
    /// # Returns
    /// * `Ok(())` if CodeLLDB started successfully
    /// * `Err(String)` with error message on failure
    pub async fn start_codelldb(&mut self, source_path: &str) -> Result<(), String> {
        // Kill any process on this port (more reliable than pattern matching)
        self.kill_process_on_port(self.codelldb_port);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Verify port is now available
        if !is_port_available(self.codelldb_port) {
            return Err(format!(
                "Port {} is still in use after cleanup",
                self.codelldb_port
            ));
        }

        // Build Rust binary once and reuse across all tests
        // The fixture is a Cargo project at fixtures/rust/
        let binary_path = RUST_TRADING_BOT_PATH
            .get_or_init(|| {
                let source_file = std::path::Path::new(source_path);
                // Go from fixtures/rust/src/main.rs -> fixtures/rust
                let fixture_dir = source_file
                    .parent() // src
                    .and_then(|p| p.parent()) // rust
                    .ok_or_else(|| "Invalid source path".to_string())?;
                let binary_path = fixture_dir.join("target/debug/detrix_example_app");

                eprintln!("Building Rust fixture at {:?}", fixture_dir);

                let build_output = Command::new("cargo")
                    .args(["build"])
                    .current_dir(fixture_dir)
                    .output()
                    .map_err(|e| format!("Failed to build Rust binary: {}", e))?;

                if !build_output.status.success() {
                    return Err(format!(
                        "Rust build failed: {}",
                        String::from_utf8_lossy(&build_output.stderr)
                    ));
                }

                // Verify binary was created
                if !binary_path.exists() {
                    return Err(format!(
                        "Rust build succeeded but binary not found at {:?}. Stdout: {}",
                        binary_path,
                        String::from_utf8_lossy(&build_output.stdout)
                    ));
                }

                eprintln!("Rust fixture built successfully: {:?}", binary_path);
                Ok(binary_path)
            })
            .as_ref()
            .map_err(|e| e.clone())?
            .clone();

        // Verify binary still exists (might have been deleted between test runs)
        if !binary_path.exists() {
            return Err(format!(
                "Rust binary not found at {:?}. Run 'cargo build' in fixtures/rust/",
                binary_path
            ));
        }

        // Store binary path for use in connection creation
        self.rust_binary_path = Some(binary_path.clone());

        // Get the detrix binary path for running lldb-serve
        let detrix_binary = find_detrix_binary(&self.workspace_root)
            .ok_or_else(|| "Failed to find detrix binary".to_string())?;

        let binary_path_str = binary_path.to_str().ok_or("Invalid binary path")?;

        // Start lldb-serve with CodeLLDB as the debug adapter
        // lldb-serve handles the protocol translation and adds proper settings for Rust
        match start_lldb_serve_with_codelldb(self.codelldb_port, binary_path_str, &detrix_binary) {
            Ok(process) => {
                // Register process for cleanup tracking
                register_e2e_process("codelldb", process.id());
                self.codelldb_process = Some(process);

                // Wait for port to be open
                if wait_for_port(self.codelldb_port, 15).await {
                    Ok(())
                } else {
                    Err(format!(
                        "lldb-serve (CodeLLDB) not listening on port {} after spawn",
                        self.codelldb_port
                    ))
                }
            }
            Err(e) => Err(format!("Could not spawn CodeLLDB: {}", e)),
        }
    }

    /// Print CodeLLDB logs (for debugging test failures)
    pub fn print_codelldb_logs(&self, last_n_lines: usize) {
        let log_path = format!("/tmp/codelldb_e2e_{}.log", self.codelldb_port);
        println!("\n=== CODELLDB LOG (last {} lines) ===", last_n_lines);
        if let Ok(content) = std::fs::read_to_string(&log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read codelldb log)");
        }
        println!("====================================\n");
    }

    /// Print delve logs (for debugging test failures)
    pub fn print_delve_logs(&self, last_n_lines: usize) {
        let log_path = format!("/tmp/delve_e2e_{}.log", self.delve_port);
        println!("\n=== DELVE LOG (last {} lines) ===", last_n_lines);
        if let Ok(content) = std::fs::read_to_string(&log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read delve log)");
        }
        println!("=================================\n");
    }

    /// Print lldb-dap logs (for debugging test failures)
    pub fn print_lldb_logs(&self, last_n_lines: usize) {
        let log_path = format!("/tmp/lldb_dap_e2e_{}.log", self.lldb_port);
        println!("\n=== LLDB-DAP LOG (last {} lines) ===", last_n_lines);
        if let Ok(content) = std::fs::read_to_string(&log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read lldb-dap log)");
        }
        println!("=================================\n");
    }

    /// Stop the daemon process only (keep debugger running)
    ///
    /// This is useful for testing daemon restart scenarios where you want
    /// to verify auto-reconnect functionality.
    pub fn stop_daemon(&mut self) {
        // Kill daemon spawner process if any
        if let Some(mut p) = self.daemon_process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }

        // Kill the actual forked daemon by reading PID from file
        if let Some(ref pid_path) = self.daemon_pid_path {
            if let Ok(content) = std::fs::read_to_string(pid_path) {
                if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(pid) = info.get("pid").and_then(|v| v.as_u64()) {
                        Self::kill_daemon_pid(pid as u32);
                    }
                }
            }
            // Remove PID file so daemon can create a fresh one on restart
            let _ = std::fs::remove_file(pid_path);
        }
    }

    /// Restart daemon after it was stopped
    ///
    /// Uses the same configuration (temp_dir, ports, database) as the original start.
    /// This enables testing auto-reconnect functionality.
    pub async fn restart_daemon(&mut self) -> Result<(), String> {
        // Start daemon using the same temp directory (preserves database state)
        self.start_daemon().await
    }

    /// Start Detrix daemon
    pub async fn start_daemon(&mut self) -> Result<(), String> {
        // Create config file
        let db_path = self.temp_dir.path().join("detrix.db");
        let pid_path = self.temp_dir.path().join("daemon.pid");

        // Helper to convert paths for TOML compatibility
        // On Windows, backslashes need to be converted to forward slashes
        // (backslashes are escape characters in TOML strings)
        #[cfg(windows)]
        fn to_toml_path(p: &std::path::Path) -> String {
            p.to_string_lossy().replace('\\', "/")
        }
        #[cfg(not(windows))]
        fn to_toml_path(p: &std::path::Path) -> String {
            p.to_string_lossy().to_string()
        }

        let workspace_path_str = to_toml_path(&self.workspace_root);
        let db_path_str = to_toml_path(&db_path);
        let pid_path_str = to_toml_path(&pid_path);

        // Use port_fallback = true to let daemon find available ports if requested ones are busy
        // We'll read actual ports from PID file after daemon starts
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
port_fallback = true

[api.rest]
enabled = true
host = "127.0.0.1"
port = {}

[api.grpc]
enabled = true
host = "127.0.0.1"
port = {}

[safety]
enable_ast_analysis = false
"#,
            workspace_path_str, db_path_str, pid_path_str, self.http_port, self.grpc_port,
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

        // Log binary path and build time for debugging binary version issues
        let binary_metadata = std::fs::metadata(&binary_path).ok();
        let build_time = binary_metadata
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let duration = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());

        println!("=== DAEMON BINARY INFO ===");
        println!("  Path: {}", binary_path.display());
        println!("  Build time: {}", build_time);
        println!("==========================");

        // Write daemon output to log file
        let daemon_log_path = self.temp_dir.path().join("daemon.log");
        let daemon_log_file = std::fs::File::create(&daemon_log_path).map_err(|e| e.to_string())?;
        let daemon_log_stderr = daemon_log_file.try_clone().map_err(|e| e.to_string())?;

        self.daemon_log_path = Some(daemon_log_path);

        // Start daemon from project root (required for relative paths to work)
        // Don't pass --grpc flag - use config instead (api.grpc.enabled = true)
        // This ensures config's port and port_fallback are used correctly
        // Use --daemon --pid-file to enable PID file writing for port discovery
        let process = Command::new(&binary_path)
            .args([
                "serve",
                "--config",
                config_path.to_str().unwrap(),
                "--daemon",
                "--pid-file",
                pid_path.to_str().unwrap(),
            ])
            .current_dir(&self.workspace_root)
            .env(
                "RUST_LOG",
                "detrix_cli=debug,detrix_dap=debug,detrix_application=debug,detrix_api=debug,info",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::from(daemon_log_file))
            .stderr(Stdio::from(daemon_log_stderr))
            .spawn();

        match process {
            Ok(p) => {
                self.daemon_process = Some(p);
                self.daemon_pid_path = Some(pid_path.clone());

                // Wait for PID file to be created with port information
                // The daemon writes actual allocated ports to PID file
                let (http_port, grpc_port) =
                    Self::wait_for_pid_file_ports(&pid_path, self.http_port, self.grpc_port)
                        .await?;

                // Register the forked daemon PID for E2E cleanup tracking
                // Read from the daemon's own PID file
                if let Ok(content) = std::fs::read_to_string(&pid_path) {
                    if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(pid) = info.get("pid").and_then(|v| v.as_u64()) {
                            register_e2e_process("daemon", pid as u32);
                        }
                    }
                }

                // Update ports with actual allocated values
                self.http_port = http_port;
                self.grpc_port = grpc_port;

                // Verify ports are actually listening
                if !wait_for_port(self.http_port, 30).await {
                    return Err(format!(
                        "Daemon HTTP not responding on port {}",
                        self.http_port
                    ));
                }
                if !wait_for_port(self.grpc_port, 10).await {
                    return Err(format!(
                        "Daemon gRPC not responding on port {}",
                        self.grpc_port
                    ));
                }
                Ok(())
            }
            Err(e) => Err(format!("Could not spawn daemon: {}", e)),
        }
    }

    /// Wait for PID file to be created and contain port information
    ///
    /// Returns (http_port, grpc_port) read from the PID file.
    /// Falls back to requested ports if PID file doesn't contain port info within timeout.
    async fn wait_for_pid_file_ports(
        pid_path: &std::path::Path,
        fallback_http: u16,
        fallback_grpc: u16,
    ) -> Result<(u16, u16), String> {
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(30);

        while start.elapsed() < timeout {
            if pid_path.exists() {
                if let Ok(content) = std::fs::read_to_string(pid_path) {
                    // Parse JSON format: {"pid":12345,"ports":{"http":8080,"grpc":50051}}
                    if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(ports) = info.get("ports") {
                            // Check if BOTH ports are present in the PID file
                            // The daemon writes ports AFTER servers start, so we need to wait
                            // for both to be available
                            let http_port = ports.get("http").and_then(|v| v.as_u64());
                            let grpc_port = ports.get("grpc").and_then(|v| v.as_u64());

                            // Only return if we have BOTH ports (indicating servers are fully started)
                            if let (Some(http), Some(grpc)) = (http_port, grpc_port) {
                                return Ok((http as u16, grpc as u16));
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(DEFAULT_DAEMON_POLL_INTERVAL_MS)).await;
        }

        // Timeout - fall back to original ports (this shouldn't happen normally)
        eprintln!(
            "Warning: PID file ports not found within timeout, using fallback ports: http={}, grpc={}",
            fallback_http, fallback_grpc
        );
        Ok((fallback_http, fallback_grpc))
    }

    /// Stop all processes spawned by this executor
    ///
    /// Only kills processes we have Child handles for - doesn't do port-based
    /// cleanup which could interfere with other parallel tests.
    ///
    /// For debugger processes (delve, lldb), also kills child processes (detrix_example_app)
    /// that were spawned by the debugger.
    pub fn stop_all(&mut self) {
        // Kill daemon process - when using --daemon flag, the process forks,
        // so we need to read the actual PID from the PID file
        if let Some(mut p) = self.daemon_process.take() {
            let _ = p.kill();
            let _ = p.wait(); // Wait to reap spawner zombie
        }

        // Kill the actual forked daemon by reading PID from file
        if let Some(pid_path) = self.daemon_pid_path.take() {
            if let Ok(content) = std::fs::read_to_string(&pid_path) {
                if let Ok(info) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(pid) = info.get("pid").and_then(|v| v.as_u64()) {
                        Self::kill_daemon_pid(pid as u32);
                        // Unregister from E2E cleanup tracking
                        unregister_e2e_process("daemon", pid as u32);
                    }
                }
            }
            // Remove PID file
            let _ = std::fs::remove_file(&pid_path);
        }

        // Kill debugpy process and wait
        if let Some(mut p) = self.debugpy_process.take() {
            let pid = p.id();
            let _ = p.kill();
            let _ = p.wait();
            // Unregister from E2E cleanup tracking
            unregister_e2e_process("debugpy", pid);
        }

        // Kill delve process and its children (detrix_example_app)
        // Delve spawns the debugged binary as a child process
        if let Some(mut p) = self.delve_process.take() {
            let pid = p.id();

            // Check if delve is still running (may have exited when DAP disconnected)
            let delve_alive = match p.try_wait() {
                Ok(None) => true, // Still running
                _ => false,       // Already exited
            };

            if delve_alive {
                // Track children NOW (detrix_example_app is only spawned after DAP connect)
                self.track_child_pids(pid);
                // Kill tracked children first
                self.kill_tracked_children();
                // Then kill delve process tree
                Self::kill_process_tree(pid);
                let _ = p.kill();
                let _ = p.wait();
            }
            // Unregister from E2E cleanup tracking
            unregister_e2e_process("delve", pid);
            // Note: If delve already exited, detrix_example_app becomes orphaned.
            // We don't use pkill here because it would kill detrix_example_app from
            // other parallel tests. The orphaned process will be cleaned up
            // when the test suite finishes.
        }

        // Kill lldb-dap process and its children
        if let Some(mut p) = self.lldb_process.take() {
            let pid = p.id();
            Self::kill_process_tree(pid);
            let _ = p.kill();
            let _ = p.wait();
            // Unregister from E2E cleanup tracking
            unregister_e2e_process("lldb", pid);
        }

        // NOTE: We intentionally don't use kill_process_on_port here because:
        // 1. We already have Child handles for all processes we spawned
        // 2. Port-based killing can interfere with other parallel tests
        // 3. The Child.kill() approach is more reliable and targeted
    }

    /// Kill a single process by PID (used for forked daemon)
    #[cfg(unix)]
    fn kill_daemon_pid(pid: u32) {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let pid = Pid::from_raw(pid as i32);
        // Send SIGTERM first for graceful shutdown
        let _ = kill(pid, Signal::SIGTERM);
        // Brief wait for graceful shutdown (200ms max)
        // Keep it short so debugpy can quickly detect the disconnect
        for _ in 0..2 {
            std::thread::sleep(std::time::Duration::from_millis(
                DEFAULT_DAEMON_POLL_INTERVAL_MS,
            ));
            if kill(pid, None).is_err() {
                return; // Process exited gracefully
            }
        }
        // Force kill if still running
        let _ = kill(pid, Signal::SIGKILL);
    }

    #[cfg(windows)]
    fn kill_daemon_pid(pid: u32) {
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    }

    /// Kill a process and all its descendants (child processes)
    ///
    /// Uses negative PID to kill the entire process group. This is necessary because
    /// debuggers like delve use `setsid()` to create a new session, and their child
    /// processes (like detrix_example_app) won't be killed by `pkill -P`.
    #[cfg(unix)]
    fn kill_process_tree(pid: u32) {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        // Kill the entire process group (negative PID kills process group)
        // This works even across session boundaries created by setsid()
        let pgid = Pid::from_raw(-(pid as i32));
        let _ = kill(pgid, Signal::SIGTERM);

        // Give processes time to terminate gracefully
        std::thread::sleep(std::time::Duration::from_millis(
            DEFAULT_DAEMON_POLL_INTERVAL_MS,
        ));

        // Also try pkill -P for direct children (backup approach)
        let _ = Command::new("pkill")
            .args(["-TERM", "-P", &pid.to_string()])
            .output();

        std::thread::sleep(std::time::Duration::from_millis(
            DEFAULT_DAEMON_POLL_INTERVAL_MS,
        ));

        // Force kill with SIGKILL
        let _ = kill(pgid, Signal::SIGKILL);
        let _ = Command::new("pkill")
            .args(["-KILL", "-P", &pid.to_string()])
            .output();
    }

    /// Kill a process tree (Windows stub - uses taskkill)
    #[cfg(windows)]
    fn kill_process_tree(pid: u32) {
        // Use taskkill /T to kill process tree on Windows
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }

    /// Track child processes of a parent PID
    ///
    /// Uses `pgrep -P` to find direct children of the given PID and stores them
    /// for later cleanup.
    #[cfg(unix)]
    fn track_child_pids(&mut self, parent_pid: u32) {
        if let Ok(output) = Command::new("pgrep")
            .args(["-P", &parent_pid.to_string()])
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if let Ok(pid) = line.trim().parse::<u32>() {
                        self.child_pids.push(pid);
                    }
                }
            }
        }
    }

    /// Track child processes (Windows stub - not implemented)
    #[cfg(windows)]
    fn track_child_pids(&mut self, _parent_pid: u32) {
        // Not implemented on Windows
    }

    /// Kill all tracked child PIDs
    #[cfg(unix)]
    fn kill_tracked_children(&mut self) {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        // Send SIGTERM first
        for pid in &self.child_pids {
            let _ = kill(Pid::from_raw(*pid as i32), Signal::SIGTERM);
        }

        std::thread::sleep(std::time::Duration::from_millis(
            DEFAULT_DAEMON_POLL_INTERVAL_MS,
        ));

        // Force kill any remaining with SIGKILL
        for pid in self.child_pids.drain(..) {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
        }
    }

    /// Kill all tracked child PIDs (Windows stub)
    #[cfg(windows)]
    fn kill_tracked_children(&mut self) {
        for pid in self.child_pids.drain(..) {
            let _ = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output();
        }
    }

    /// Kill any detrix/debugpy/dlv process LISTENING on the specified port
    ///
    /// Safety: Only kills processes matching our known process names to avoid
    /// accidentally killing unrelated system processes.
    #[cfg(unix)]
    fn kill_process_on_port(&self, port: u16) {
        // Use lsof to find processes listening on this port, with process name
        if let Ok(output) = Command::new("lsof")
            .args(["-sTCP:LISTEN", "-i", &format!("TCP:{}", port)])
            .output()
        {
            if output.status.success() {
                let output_str = String::from_utf8_lossy(&output.stdout);
                for line in output_str.lines().skip(1) {
                    // Skip header
                    // lsof output format: COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        let command = parts[0].to_lowercase();
                        let pid_str = parts[1];

                        // Only kill processes we expect (detrix, python/debugpy, dlv, lldb-dap)
                        let is_our_process = command.starts_with("detrix")
                            || command.starts_with("python")
                            || command.starts_with("dlv")
                            || command.starts_with("lldb");

                        if !is_our_process {
                            continue;
                        }

                        if let Ok(pid_num) = pid_str.parse::<i32>() {
                            // Don't kill ourselves
                            if pid_num == std::process::id() as i32 {
                                continue;
                            }
                            // Send SIGKILL to the process
                            let _ = Command::new("kill")
                                .args(["-9", &pid_num.to_string()])
                                .output();
                        }
                    }
                }
            }
        }
    }

    /// Kill process on port (Windows stub - not implemented)
    #[cfg(windows)]
    fn kill_process_on_port(&self, _port: u16) {
        // Not implemented on Windows - E2E tests require Unix
    }
}

impl Default for TestExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TestExecutor {
    fn drop(&mut self) {
        self.stop_all();
    }
}
