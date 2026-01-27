//! Daemon lifecycle management commands
//!
//! Implements `detrix daemon start/stop/restart/status` commands for managing
//! the single-instance Detrix daemon process.

use crate::context::ClientContext;
use crate::utils::daemon_discovery::{DaemonDiscovery, DaemonInfo, DiscoveryMethod};
use crate::utils::pid::PidFile;
use anyhow::{Context, Result};
use clap::Subcommand;
use detrix_config::DaemonConfig;
use std::fs::OpenOptions;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

/// Daemon subcommands
#[derive(Subcommand)]
pub enum DaemonAction {
    /// Start the daemon in background
    Start,
    /// Stop the daemon gracefully
    Stop,
    /// Restart the daemon
    Restart,
    /// Check daemon status
    Status,
}

/// Start the Detrix daemon in background
///
/// Creates a detached background process and writes PID file.
/// Logs are written to the configured log file (default: ~/detrix/log/detrix_daemon.log).
/// Returns error if daemon is already running.
pub async fn start(
    config_path: &str,
    pid_file_path: &PathBuf,
    daemon_config: &DaemonConfig,
) -> Result<()> {
    let logging_config = &daemon_config.logging;
    let poll_interval_ms = daemon_config.poll_interval_ms;

    // Check if already running using robust discovery (PID file + port probe)
    let discovery = DaemonDiscovery::new().with_pid_file(pid_file_path);
    if let Some(info) = discovery
        .find_daemon()
        .context("Failed to check if daemon is running")?
    {
        let method = match info.discovery_method {
            DiscoveryMethod::PidFile => "PID file",
            DiscoveryMethod::PortProbe => "port probe",
        };
        anyhow::bail!(
            "Daemon already running (discovered via {}): PID={}, endpoint={}",
            method,
            info.pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            info.grpc_endpoint()
        );
    }

    println!("Starting Detrix daemon...");

    // Get current executable path
    let exe_path = std::env::current_exe().context("Failed to get current executable path")?;

    // Set up log file output if file logging is enabled
    // This captures stdout/stderr of the daemon subprocess (for startup errors, panics, etc.)
    // The daemon's tracing logs go to detrix_daemon.log.{date} via daily rotation
    let (stdout_handle, stderr_handle) = if logging_config.file_logging_enabled {
        let log_path = logging_config.daemon_startup_log_path();

        // Ensure log directory exists
        detrix_config::paths::ensure_parent_dir(&log_path)
            .context("Failed to create log directory")?;

        // Open log file in append mode
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .context(format!("Failed to open log file: {:?}", log_path))?;

        // Clone file handle for stderr (both stdout and stderr go to the same file)
        let log_file_stderr = log_file
            .try_clone()
            .context("Failed to clone log file handle")?;

        println!("  Log file: {}", log_path.display());

        (Stdio::from(log_file), Stdio::from(log_file_stderr))
    } else {
        (Stdio::null(), Stdio::null())
    };

    // Start daemon as detached background process
    // On Unix: use nohup to detach from terminal
    // On Windows: use CREATE_NO_WINDOW flag
    #[cfg(unix)]
    let child = Command::new("nohup")
        .arg(&exe_path)
        .arg("serve")
        .arg("--config")
        .arg(config_path)
        .arg("--daemon")
        .arg("--pid-file")
        .arg(pid_file_path)
        .stdin(Stdio::null())
        .stdout(stdout_handle)
        .stderr(stderr_handle)
        .spawn()
        .context("Failed to spawn daemon process")?;

    #[cfg(windows)]
    let child = Command::new(&exe_path)
        .arg("serve")
        .arg("--config")
        .arg(config_path)
        .arg("--daemon")
        .arg("--pid-file")
        .arg(pid_file_path)
        .stdin(Stdio::null())
        .stdout(stdout_handle)
        .stderr(stderr_handle)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .spawn()
        .context("Failed to spawn daemon process")?;

    let daemon_pid = child.id();

    // Wait for PID file to appear (up to 5 seconds)
    for _ in 0..50 {
        if pid_file_path.exists() {
            // Verify daemon is actually running
            if let Some(running_pid) = PidFile::is_running(pid_file_path)? {
                println!("✓ Daemon started successfully (PID: {})", running_pid);
                println!("  PID file: {}", pid_file_path.display());
                return Ok(());
            }
        }
        sleep(Duration::from_millis(poll_interval_ms)).await;
    }

    // Timeout - daemon didn't start
    anyhow::bail!(
        "Daemon process spawned (PID: {}) but failed to initialize within 5 seconds. \
         Check logs for errors.",
        daemon_pid
    );
}

/// Stop the Detrix daemon gracefully
///
/// Sends SIGTERM (Unix) or taskkill (Windows) to the daemon process.
/// Waits up to 10 seconds for graceful shutdown.
pub async fn stop(pid_file_path: &PathBuf, poll_interval_ms: u64) -> Result<()> {
    // Check if daemon is running using robust discovery
    let discovery = DaemonDiscovery::new().with_pid_file(pid_file_path);
    let info = match discovery
        .find_daemon()
        .context("Failed to check if daemon is running")?
    {
        Some(info) => info,
        None => {
            println!("Daemon is not running");
            return Ok(());
        }
    };

    // We need a PID to stop the daemon
    let pid = match info.pid {
        Some(pid) => pid,
        None => {
            // Found via port probe but no PID - daemon is running but we can't stop it
            println!(
                "⚠ Daemon detected at {} but PID is unknown (stale PID file?)",
                info.grpc_endpoint()
            );
            println!(
                "  Try stopping it manually or remove the PID file: {}",
                pid_file_path.display()
            );
            return Ok(());
        }
    };

    println!("Stopping Detrix daemon (PID: {})...", pid);

    // Send SIGTERM (Unix) or taskkill (Windows)
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        kill(Pid::from_raw(pid as i32), Signal::SIGTERM)
            .context("Failed to send SIGTERM to daemon")?;
    }

    #[cfg(windows)]
    {
        Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/F")
            .output()
            .context("Failed to kill daemon process")?;
    }

    // Wait for daemon to exit (up to 10 seconds)
    for i in 0..100 {
        if PidFile::is_running(pid_file_path)?.is_none() {
            println!("✓ Daemon stopped successfully");
            return Ok(());
        }

        if i % 10 == 0 && i > 0 {
            println!("  Waiting for daemon to stop... ({}/10 seconds)", i / 10);
        }

        sleep(Duration::from_millis(poll_interval_ms)).await;
    }

    // Timeout - force kill
    println!("⚠ Daemon did not stop gracefully, force killing...");

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        kill(Pid::from_raw(pid as i32), Signal::SIGKILL)
            .context("Failed to send SIGKILL to daemon")?;
    }

    #[cfg(windows)]
    {
        // taskkill /F already force kills on Windows
    }

    // Wait a bit more for OS to clean up
    sleep(Duration::from_millis(poll_interval_ms * 5)).await;

    if PidFile::is_running(pid_file_path)?.is_none() {
        println!("✓ Daemon force killed successfully");
        Ok(())
    } else {
        anyhow::bail!("Failed to kill daemon process (PID: {})", pid);
    }
}

/// Restart the Detrix daemon with state preservation
///
/// Stops the daemon gracefully, then starts it again.
/// The new daemon instance will reload state from the database.
pub async fn restart(
    config_path: &str,
    pid_file_path: &PathBuf,
    daemon_config: &DaemonConfig,
) -> Result<()> {
    println!("Restarting Detrix daemon...");

    // Stop if running (use discovery for robust detection)
    let discovery = DaemonDiscovery::new().with_pid_file(pid_file_path);
    if discovery.find_daemon()?.is_some() {
        stop(pid_file_path, daemon_config.poll_interval_ms).await?;
        sleep(Duration::from_millis(daemon_config.restart_delay_ms)).await;
    }

    // Start again
    start(config_path, pid_file_path, daemon_config).await?;

    println!("✓ Daemon restarted successfully");
    Ok(())
}

/// Check daemon status and print information
///
/// Shows whether daemon is running, its PID, config path, and PID file location.
/// Uses robust discovery with port probing fallback (probes ports from config, not defaults).
pub async fn status(
    pid_file_path: &PathBuf,
    config_path: &Path,
    config: &detrix_config::Config,
) -> Result<()> {
    // Always show config path
    println!("  Config: {}", config_path.display());

    // Use robust discovery (PID file + port probe with config ports)
    // Port probe uses configured ports, not defaults, to avoid finding unrelated daemons
    let discovery = DaemonDiscovery::new()
        .with_pid_file(pid_file_path)
        .with_probe_host(&config.api.rest.host)
        .with_probe_ports(config.api.rest.port, config.api.grpc.port);

    match discovery
        .find_daemon()
        .context("Failed to check daemon status")?
    {
        Some(info) => {
            println!("✓ Daemon is running");
            print_daemon_info(&info, pid_file_path);
        }
        None => {
            println!("✗ Daemon is not running");
            if pid_file_path.exists() {
                println!(
                    "  PID file exists but is stale: {}",
                    pid_file_path.display()
                );
            }
        }
    }

    Ok(())
}

/// Print detailed daemon information
fn print_daemon_info(info: &DaemonInfo, pid_file_path: &Path) {
    // Show discovery method
    let method = match info.discovery_method {
        DiscoveryMethod::PidFile => "PID file",
        DiscoveryMethod::PortProbe => "port probe (PID file stale)",
    };
    println!("  Discovery: {}", method);

    // Show PID if known
    if let Some(pid) = info.pid {
        println!("  PID: {}", pid);

        // Try to get additional info from /proc (Unix only)
        #[cfg(target_os = "linux")]
        {
            if let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{}/cmdline", pid)) {
                let cmd = cmdline.replace('\0', " ");
                println!("  Command: {}", cmd);
            }
        }
    } else {
        println!("  PID: unknown");
    }

    // Show endpoints
    println!("  gRPC endpoint: {}", info.grpc_endpoint());
    println!("  HTTP endpoint: {}", info.http_endpoint());
    println!("  PID file: {}", pid_file_path.display());
}

/// Execute daemon subcommand
///
/// Uses pre-loaded config from ClientContext to avoid redundant config loading.
pub async fn run(
    ctx: &ClientContext,
    action: DaemonAction,
    pid_file_path: Option<PathBuf>,
) -> Result<()> {
    // Get config path as string for subprocess spawning
    let config_path = ctx.config_path.to_string_lossy();

    // Use CLI override, or config setting, or fallback to default
    let pid_file_path = pid_file_path.unwrap_or(ctx.config.daemon.pid_file.clone());
    let daemon_config = &ctx.config.daemon;

    match action {
        DaemonAction::Start => start(&config_path, &pid_file_path, daemon_config).await,
        DaemonAction::Stop => stop(&pid_file_path, daemon_config.poll_interval_ms).await,
        DaemonAction::Restart => restart(&config_path, &pid_file_path, daemon_config).await,
        DaemonAction::Status => status(&pid_file_path, &ctx.config_path, &ctx.config).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    #[tokio::test]
    async fn test_status_not_running() {
        let temp_dir = std::env::temp_dir();
        let pid_file = temp_dir.join(format!("detrix_test_status_{}.pid", process::id()));
        let config_path = Path::new("/tmp/test_detrix.toml");
        let config = detrix_config::Config::default();

        // Clean up
        let _ = std::fs::remove_file(&pid_file);

        // Should succeed even if daemon not running
        let result = status(&pid_file, config_path, &config).await;
        assert!(result.is_ok());

        // Clean up
        let _ = std::fs::remove_file(&pid_file);
    }

    #[tokio::test]
    async fn test_stop_when_not_running() {
        let temp_dir = std::env::temp_dir();
        let pid_file = temp_dir.join(format!("detrix_test_stop_{}.pid", process::id()));

        // Clean up
        let _ = std::fs::remove_file(&pid_file);

        // Should succeed (no-op) if daemon not running
        // Use default poll interval for tests
        let poll_interval_ms = detrix_config::constants::DEFAULT_DAEMON_POLL_INTERVAL_MS;
        let result = stop(&pid_file, poll_interval_ms).await;
        assert!(result.is_ok());

        // Clean up
        let _ = std::fs::remove_file(&pid_file);
    }
}
