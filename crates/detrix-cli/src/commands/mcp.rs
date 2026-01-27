//! MCP (Model Context Protocol) server command
//!
//! Starts the Detrix MCP server for LLM integration via stdio transport.
//!
//! ## Modes
//!
//! - **Bridge mode** (default): Proxies stdio to daemon's HTTP `/mcp` endpoint
//!   - Auto-spawns daemon if not running
//!   - Centralized state management
//!   - Shared metrics and connections
//!
//! - **Direct mode** (`--no-daemon`): Runs full MCP server with its own storage/adapter
//!   - Useful for testing or when you don't want a persistent daemon
//!
//! Bridge mode is the default because it provides better resource sharing
//! and consistent state across multiple MCP clients.

use anyhow::{Context, Result};
use detrix_api::mcp::DetrixServer;
use detrix_api::ApiState;
use detrix_application::{EventRepositoryRef, McpUsageRepositoryRef, SystemEventRepositoryRef};
use detrix_config::{load_config, resolve_config_path, Config};
use detrix_logging::{debug, info, warn};
use rmcp::transport::stdio;
use rmcp::ServiceExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt as UnixCommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::utils::daemon_discovery::{DaemonDiscovery, DiscoveryMethod};
use crate::utils::init::{init_infrastructure, InitOptions};
use crate::utils::output::build_gelf_output;
use crate::utils::pid::PidFile;

use super::mcp_bridge;
use super::ps::is_daemon_healthy;

/// Run the MCP server or bridge
///
/// By default, runs in daemon mode:
/// 1. Check if daemon is running via PID file
/// 2. If running, read the actual port from PID file
/// 3. If not running, auto-spawn daemon and wait for port
/// 4. Run in bridge mode using the actual port
///
/// With --no-daemon: Run in direct mode (own storage/adapter)
pub async fn run(
    config_path: &str,
    transport: &str,
    no_daemon: bool,
    daemon_port: u16,
) -> Result<()> {
    // Note: Logging is already initialized by main.rs with config from detrix.toml
    // (including use_utc timestamp setting and file logging to ~/detrix/log/mcp_{pid}.log)

    // Log startup info for debugging (useful when run from IDEs)
    debug!(
        config = %config_path,
        transport = %transport,
        no_daemon = no_daemon,
        daemon_port = daemon_port,
        cwd = ?std::env::current_dir().ok(),
        "MCP starting"
    );

    // Only stdio transport is currently supported
    if transport != "stdio" {
        anyhow::bail!("Only stdio transport is currently supported. Use --transport stdio");
    }

    // Resolve config path to absolute path for daemon spawning
    // This ensures the daemon gets the correct config regardless of working directory
    let config_path = resolve_config_path(Path::new(config_path));
    debug!("Resolved config path: {}", config_path.display());

    // Load config to get daemon settings (needed for PID file path and direct mode)
    // Strict: MCP should not create config files. Use `detrix init` explicitly.
    let config = load_config(&config_path).with_context(|| {
        format!(
            "Failed to load config from {}. Run 'detrix init' to create a default config.",
            config_path.display()
        )
    })?;
    let config_path_str = config_path.to_string_lossy();

    // If --no-daemon is specified, run direct mode
    if no_daemon {
        info!("Starting MCP server in direct mode (--no-daemon)");
        return run_direct(&config_path_str, &config).await;
    }

    // Use centralized daemon discovery (PID file + port probe fallback)
    let discovery = DaemonDiscovery::new()
        .with_pid_file(&config.daemon.pid_file)
        .with_probe_host(&config.api.rest.host)
        .with_probe_ports(config.api.rest.port, config.api.grpc.port);

    let (actual_host, actual_port) = match discovery.find_daemon() {
        Ok(Some(daemon_info)) => {
            let method = match daemon_info.discovery_method {
                DiscoveryMethod::PidFile => "PID file",
                DiscoveryMethod::PortProbe => "port probe",
            };
            info!(
                "Found running daemon via {} (PID: {}, endpoint: {})",
                method,
                daemon_info
                    .pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                daemon_info.http_endpoint()
            );

            // Verify daemon is actually healthy before using it
            if is_daemon_healthy(&daemon_info.host, daemon_info.http_port).await {
                (daemon_info.host, daemon_info.http_port)
            } else {
                // Daemon found but not healthy yet - wait for it to become ready
                // This handles the case where daemon is still starting up
                info!(
                    "Daemon found via {} but not responding yet, waiting for it to become healthy...",
                    method
                );
                match wait_for_daemon_healthy(
                    &daemon_info.host,
                    daemon_info.http_port,
                    &config.daemon.pid_file,
                )
                .await
                {
                    Ok(port) => (daemon_info.host, port),
                    Err(_) => {
                        // Timeout waiting - daemon may have crashed, respawn
                        warn!("Daemon didn't become healthy, respawning...");
                        let port = spawn_daemon_for_mcp(
                            &config.api.rest.host,
                            &config_path_str,
                            &config.daemon.pid_file,
                            daemon_port,
                        )
                        .await?;
                        (config.api.rest.host.clone(), port)
                    }
                }
            }
        }
        Ok(None) => {
            // Daemon not running - spawn it
            info!(
                "Daemon not running, auto-spawning (config: {})...",
                config_path_str
            );
            let port = spawn_daemon_for_mcp(
                &config.api.rest.host,
                &config_path_str,
                &config.daemon.pid_file,
                daemon_port,
            )
            .await?;
            (config.api.rest.host.clone(), port)
        }
        Err(e) => {
            // Error during discovery - try spawning anyway
            warn!("Error discovering daemon: {}, trying to spawn...", e);
            let port = spawn_daemon_for_mcp(
                &config.api.rest.host,
                &config_path_str,
                &config.daemon.pid_file,
                daemon_port,
            )
            .await?;
            (config.api.rest.host.clone(), port)
        }
    };

    info!(
        "Starting MCP bridge to daemon on {}:{}",
        actual_host, actual_port
    );
    mcp_bridge::run_bridge(
        &actual_host,
        actual_port,
        Some(config_path.clone()),
        Some(config.daemon.pid_file.clone()),
        &config.mcp,
    )
    .await
}

/// Run the MCP server directly (without daemon)
///
/// Accepts pre-loaded config to avoid redundant config loading.
async fn run_direct(config_path: &str, config: &Config) -> Result<()> {
    info!("Starting Detrix MCP server (direct mode)");

    info!("✓ Configuration loaded from {}", config_path);
    info!("📁 Database path: {:?}", config.storage.path);

    // Initialize GELF output if configured
    let gelf_output = build_gelf_output(&config.output.gelf)
        .await
        .context("Failed to initialize GELF output")?;

    // Base directory for resolving relative paths
    let config_dir = Path::new(config_path).parent().unwrap_or(Path::new("."));

    // Initialize infrastructure using centralized initialization
    let infra = init_infrastructure(config, config_dir, InitOptions::from_config(config)).await?;
    info!("Adapter factory initialized with reconnection middleware");

    // Create application context from infrastructure components
    let ctx = infra.into_app_context(
        &config.api,
        &config.safety,
        &config.storage,
        &config.daemon,
        &config.adapter,
        &config.anchor,
        &config.limits,
        gelf_output.clone(),
    );

    // Create API state from the pre-configured AppContext
    let api_state = Arc::new(
        ApiState::builder(
            ctx.app_context,
            Arc::clone(&ctx.storage) as EventRepositoryRef,
        )
        .full_config(config.clone())
        .system_event_repository(Arc::clone(&ctx.storage) as SystemEventRepositoryRef)
        .mcp_usage_repository(ctx.storage as McpUsageRepositoryRef)
        .build(),
    );

    // Create MCP server
    let mcp_server = DetrixServer::new(api_state);

    info!("MCP server created, starting stdio transport...");
    info!("Server ready. Awaiting MCP client connection via stdio.");

    // Start server with stdio transport
    let service = mcp_server.serve(stdio()).await?;

    // Wait for server to complete with signal handling
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm =
            signal(SignalKind::terminate()).context("Failed to install SIGTERM handler")?;
        let mut sigint =
            signal(SignalKind::interrupt()).context("Failed to install SIGINT handler")?;

        tokio::select! {
            result = service.waiting() => {
                result?;
            }
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down MCP server");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT (Ctrl+C), shutting down MCP server");
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::select! {
            result = service.waiting() => {
                result?;
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Received Ctrl+C, shutting down MCP server");
            }
        }
    }

    Ok(())
}

/// Wait for a daemon to become healthy
///
/// Used when daemon is found via PID file but not responding yet (still starting up).
/// Also checks PID file for updated port information in case daemon chooses a different port.
///
/// Returns the actual port once daemon is healthy.
async fn wait_for_daemon_healthy(
    host: &str,
    initial_port: u16,
    pid_file: &std::path::PathBuf,
) -> Result<u16> {
    use detrix_config::constants::{
        DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS, DEFAULT_MCP_DAEMON_SPAWN_TIMEOUT_SECS,
    };
    let max_iterations =
        (DEFAULT_MCP_DAEMON_SPAWN_TIMEOUT_SECS * 1000) / DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS;

    for i in 0..max_iterations {
        tokio::time::sleep(Duration::from_millis(DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS)).await;

        // Check if PID file has been updated with a port
        let port_to_check = if let Ok(Some(pid_info)) = PidFile::read_info(pid_file) {
            pid_info.port().unwrap_or(initial_port)
        } else {
            initial_port
        };

        // Check if daemon is healthy on this port
        if is_daemon_healthy(host, port_to_check).await {
            info!(
                "Daemon became healthy on port {} after {}ms",
                port_to_check,
                (i + 1) * DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS
            );
            return Ok(port_to_check);
        }

        // Log progress every 5 seconds
        if i % 50 == 49 {
            debug!(
                "Waiting for daemon to become healthy... ({}/{} seconds)",
                (i + 1) / 10,
                DEFAULT_MCP_DAEMON_SPAWN_TIMEOUT_SECS
            );
        }
    }

    anyhow::bail!(
        "Daemon did not become healthy within {} seconds",
        DEFAULT_MCP_DAEMON_SPAWN_TIMEOUT_SECS
    )
}

/// Spawn the daemon for MCP auto-start
///
/// Spawns a daemon in the background and waits for it to become healthy.
/// Returns the actual port the daemon is running on (read from PID file).
/// The daemon is spawned with a special marker to indicate it was started by MCP.
///
/// Key features:
/// - Uses `process_group(0)` to isolate daemon from MCP bridge's process group
/// - Redirects stdout/stderr to log file in detrix_home/log/
/// - This prevents SIGTERM propagation when MCP bridge is killed
pub async fn spawn_daemon_for_mcp(
    host: &str,
    config_path: &str,
    pid_file: &std::path::PathBuf,
    fallback_port: u16,
) -> Result<u16> {
    use std::fs::OpenOptions;
    use std::process::{Command, Stdio};

    // Get current executable path
    let exe_path = std::env::current_exe().context("Failed to get current executable path")?;

    // Set up log file for daemon stdout/stderr (for startup errors, panics, etc.)
    // The daemon's tracing logs go to detrix_daemon.log.{date} via daily rotation
    let log_dir = detrix_config::paths::default_log_dir();
    detrix_config::paths::ensure_parent_dir(&log_dir.join("placeholder"))
        .context("Failed to create log directory")?;
    let daemon_log_path = detrix_config::paths::default_daemon_startup_log_path();

    info!(
        "Spawning daemon from MCP (exe: {}, config: {}, log: {})",
        exe_path.display(),
        config_path,
        daemon_log_path.display()
    );

    // Open log file for stdout/stderr
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&daemon_log_path)
        .context("Failed to open daemon log file")?;
    let log_file_stderr = log_file
        .try_clone()
        .context("Failed to clone log file handle")?;

    // Spawn daemon as detached background process
    // CRITICAL: Use process_group(0) to create a new process group
    // This prevents SIGTERM from propagating when MCP bridge is killed
    #[cfg(unix)]
    let _child = {
        let mut cmd = Command::new(&exe_path);
        cmd.arg("serve")
            .arg("--config")
            .arg(config_path)
            .arg("--daemon")
            .arg("--pid-file")
            .arg(pid_file)
            .arg("--mcp-spawned") // Marker for MCP-spawned daemon
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_file_stderr))
            .process_group(0); // Create new process group to isolate from parent signals

        cmd.spawn().context("Failed to spawn daemon process")?
    };

    #[cfg(windows)]
    let _child = Command::new(&exe_path)
        .arg("serve")
        .arg("--config")
        .arg(config_path)
        .arg("--daemon")
        .arg("--pid-file")
        .arg(pid_file)
        .arg("--mcp-spawned")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_stderr))
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .spawn()
        .context("Failed to spawn daemon process")?;

    // Wait for daemon to become healthy and get the actual port from PID file
    use detrix_config::constants::{
        DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS, DEFAULT_MCP_DAEMON_SPAWN_TIMEOUT_SECS,
    };
    let max_iterations =
        (DEFAULT_MCP_DAEMON_SPAWN_TIMEOUT_SECS * 1000) / DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS;

    for i in 0..max_iterations {
        tokio::time::sleep(Duration::from_millis(DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS)).await;

        // Check PID file for actual port
        if let Ok(Some(pid_info)) = PidFile::read_info(pid_file) {
            if let Some(port) = pid_info.port() {
                // Port is in PID file - verify daemon is healthy
                if is_daemon_healthy(host, port).await {
                    info!(
                        "Daemon started on port {} (PID: {}) after {}ms",
                        port,
                        pid_info.pid,
                        (i + 1) * DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS
                    );
                    return Ok(port);
                }
            }
        }

        // Fallback: also check the expected port (in case PID file is slow to update)
        if is_daemon_healthy(host, fallback_port).await {
            info!(
                "Daemon healthy on fallback port {} after {}ms",
                fallback_port,
                (i + 1) * DEFAULT_MCP_DAEMON_POLL_INTERVAL_MS
            );
            return Ok(fallback_port);
        }

        // Log progress every 5 seconds
        if i % 50 == 49 {
            debug!(
                "Waiting for daemon to become healthy... ({}/{} seconds)",
                (i + 1) / 10,
                DEFAULT_MCP_DAEMON_SPAWN_TIMEOUT_SECS
            );
        }
    }

    anyhow::bail!(
        "Daemon spawned but failed to become healthy within {} seconds. \
         Check logs for errors. Config: {}",
        DEFAULT_MCP_DAEMON_SPAWN_TIMEOUT_SECS,
        config_path
    )
}
