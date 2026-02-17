//! Detrix Rust Client Library
//!
//! Debug-on-demand observability for Rust applications. This client enables
//! AI-powered debugging of running Rust processes without code modifications
//! or restarts.
//!
//! # Quick Start
//!
//! ```no_run
//! use detrix_rs::Config;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Initialize client (starts control plane, stays SLEEPING)
//!     detrix_rs::init(Config::default())?;
//!
//!     // Your application code runs normally...
//!
//!     // When debugging is needed, call /detrix/wake on the control plane
//!     // Or programmatically:
//!     // detrix_rs::wake()?;
//!
//!     detrix_rs::shutdown()?;
//!     Ok(())
//! }
//! ```
//!
//! # Architecture
//!
//! The client follows the same pattern as the Python and Go clients:
//!
//! 1. **SLEEPING**: Zero overhead. Only control plane HTTP server running.
//! 2. **WAKING**: Transitional state during wake operation.
//! 3. **AWAKE**: lldb-dap attached, registered with daemon, ready for metrics.
//!
//! Unlike Python's debugpy, lldb-dap can be fully stopped on sleep,
//! providing cleaner resource management.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::panic))]

mod auth;
mod build_info;
mod config;
mod control;
mod daemon;
mod error;
mod generated;
mod lldb;
mod state;

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

pub use config::{Config, TlsConfig};
pub use error::{Error, Result};
pub use generated::{
    ClientState, DiscoverResponse, SleepResponse, SleepResponseStatus, StatusResponse,
    WakeResponse, WakeResponseStatus,
};

use control::ControlServer;
use daemon::{DaemonClient, RegisterRequest};
use lldb::LldbManager;
use state::{get, is_initialized, set_initialized};

/// Init/shutdown lock to prevent concurrent initialization races.
static INIT_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

/// Global control server instance.
static CONTROL_SERVER: std::sync::OnceLock<std::sync::Mutex<Option<ControlServer>>> =
    std::sync::OnceLock::new();

/// Global daemon client instance (resettable on shutdown for re-initialization).
static DAEMON_CLIENT: std::sync::OnceLock<std::sync::Mutex<Option<DaemonClient>>> =
    std::sync::OnceLock::new();

/// Global LLDB manager instance (resettable on shutdown for re-initialization).
static LLDB_MANAGER: std::sync::OnceLock<std::sync::Mutex<Option<LldbManager>>> =
    std::sync::OnceLock::new();

/// Initialize the Detrix client.
///
/// This starts the control plane HTTP server but does NOT contact the daemon.
/// The client starts in SLEEPING state with zero overhead.
///
/// # Configuration
///
/// Configuration can be provided via the `Config` struct or environment variables:
/// - `DETRIX_NAME` - Connection name
/// - `DETRIX_DAEMON_URL` - Daemon URL
/// - `DETRIX_CONTROL_HOST` - Control plane bind host
/// - `DETRIX_CONTROL_PORT` - Control plane port
/// - `DETRIX_DEBUG_PORT` - Debug adapter port
/// - `DETRIX_LLDB_DAP_PATH` - Path to lldb-dap binary
/// - `DETRIX_HOME` - Detrix home directory
/// - `DETRIX_HEALTH_CHECK_TIMEOUT` - Health check timeout (seconds, e.g. "2.0")
/// - `DETRIX_REGISTER_TIMEOUT` - Registration timeout (seconds, e.g. "5.0")
/// - `DETRIX_UNREGISTER_TIMEOUT` - Unregistration timeout (seconds, e.g. "2.0")
///
/// # Errors
///
/// Returns an error if:
/// - Client is already initialized
/// - lldb-dap binary not found
/// - Failed to start control plane
///
/// # Example
///
/// ```no_run
/// use detrix_rs::{self, Config};
///
/// detrix_rs::init(Config {
///     name: Some("my-service".to_string()),
///     ..Config::default()
/// })?;
/// # Ok::<(), detrix_rs::Error>(())
/// ```
pub fn init(config: Config) -> Result<()> {
    let _init_guard = INIT_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .map_err(|_| Error::ControlPlaneError("init lock poisoned".to_string()))?;

    if is_initialized() {
        return Err(Error::AlreadyInitialized);
    }

    // Apply environment variable overrides
    let config = config.with_env_overrides();

    // Find lldb-dap binary
    let lldb_dap_path = lldb::find_lldb_dap(config.lldb_dap_path.as_deref())?;
    debug!("Found lldb-dap at {:?}", lldb_dap_path);

    // Initialize global state
    {
        let state = get();
        let mut guard = state.write()?;

        guard.name = config.connection_name();
        guard.control_host = config.control_host.clone();
        guard.advertise_host = config.advertise_host.clone();
        guard.control_port = config.control_port;
        guard.debug_port = config.debug_port;
        guard.daemon_url = config.daemon_url.clone();
        guard.lldb_dap_path = lldb_dap_path.to_string_lossy().to_string();
        guard.detrix_home = config
            .detrix_home_path()
            .map(|p| p.to_string_lossy().to_string());
        guard.workspace_root = config.workspace_root.clone();
        guard.safe_mode = config.safe_mode;
        guard.build_commit = config.build_commit.clone();
        guard.build_tag = config.build_tag.clone();
        guard.health_check_timeout_ms = config
            .health_check_timeout
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        guard.register_timeout_ms = config
            .register_timeout
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        guard.unregister_timeout_ms = config
            .unregister_timeout
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        guard.lldb_start_timeout_ms = config
            .lldb_start_timeout
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        guard.state = ClientState::Sleeping;
    }

    // Discover auth token before creating daemon client
    let auth_token = auth::discover_token(config.detrix_home_path().as_deref());

    // Initialize daemon client (resettable via Mutex<Option<T>>)
    let daemon_client = DaemonClient::new(None, auth_token.clone())?;
    let dc_holder = DAEMON_CLIENT.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut guard) = dc_holder.lock() {
        *guard = Some(daemon_client);
    }

    // Initialize LLDB manager (resettable via Mutex<Option<T>>)
    let lldb_manager = LldbManager::new(lldb_dap_path.clone(), config.lldb_start_timeout);
    let lm_holder = LLDB_MANAGER.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut guard) = lm_holder.lock() {
        *guard = Some(lldb_manager);
    }

    // Create callbacks for control server
    let status_callback = Arc::new(status_provider);
    let wake_callback =
        Arc::new(|daemon_url: Option<String>| wake_handler(daemon_url).map_err(|e| e.to_string()));
    let sleep_callback = Arc::new(|| sleep_handler().map_err(|e| e.to_string()));
    let discover_callback = Arc::new(discover_provider);

    // Start control server
    let server = ControlServer::start(
        &config.control_host,
        config.control_port,
        auth_token,
        status_callback,
        wake_callback,
        sleep_callback,
        discover_callback,
    )?;

    let actual_port = server.port();

    // Store the server
    let server_holder = CONTROL_SERVER.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut guard) = server_holder.lock() {
        *guard = Some(server);
    }

    // Update state with actual port
    {
        let state = get();
        if let Ok(mut guard) = state.write() {
            guard.actual_control_port = actual_port;
        }
    }

    set_initialized(true);

    info!(
        "Detrix client initialized. Control plane: http://{}:{}",
        config.control_host, actual_port
    );

    Ok(())
}

/// Get the current client status.
///
/// This never blocks on network I/O - it returns immediately with the
/// current state snapshot.
///
/// # Returns
///
/// A `StatusResponse` containing:
/// - `state`: Current state (sleeping, waking, or awake)
/// - `name`: Connection name
/// - `control_port`: Actual control plane port
/// - `debug_port`: Debug adapter port (0 if never started)
/// - `connection_id`: Daemon connection ID (None if not registered)
pub fn status() -> StatusResponse {
    let state = get();
    match state.read() {
        Ok(guard) => guard.to_status_response(),
        Err(_) => StatusResponse {
            state: ClientState::Sleeping,
            name: "unknown".to_string(),
            control_host: "127.0.0.1".to_string(),
            control_port: 0,
            debug_port: 0,
            debug_port_active: false,
            daemon_url: "http://127.0.0.1:8090".to_string(),
            connection_id: None,
        },
    }
}

/// Wake the client: start debugger and register with daemon.
///
/// This spawns an lldb-dap process to attach to the current process,
/// then registers the connection with the Detrix daemon.
///
/// # Errors
///
/// Returns an error if:
/// - Client not initialized
/// - Wake already in progress
/// - Daemon not reachable
/// - Failed to start lldb-dap
/// - Failed to register with daemon
///
/// # Example
///
/// ```no_run
/// use detrix_rs;
///
/// let response = detrix_rs::wake()?;
/// println!("Debug port: {}", response.debug_port);
/// println!("Connection ID: {}", response.connection_id);
/// # Ok::<(), detrix_rs::Error>(())
/// ```
pub fn wake() -> Result<WakeResponse> {
    wake_with_url(None)
}

/// Wake the client with a daemon URL override.
///
/// Same as `wake()` but allows overriding the daemon URL for this operation.
pub fn wake_with_url(daemon_url: impl Into<Option<String>>) -> Result<WakeResponse> {
    wake_handler(daemon_url.into())
}

/// Put the client to sleep: stop debugger and unregister from daemon.
///
/// Unlike Python's debugpy, lldb-dap can be fully stopped, providing
/// cleaner resource management.
///
/// # Errors
///
/// Returns an error if the client is not initialized.
///
/// # Example
///
/// ```no_run
/// use detrix_rs;
///
/// let response = detrix_rs::sleep()?;
/// assert_eq!(response.status, detrix_rs::SleepResponseStatus::Sleeping);
/// # Ok::<(), detrix_rs::Error>(())
/// ```
pub fn sleep() -> Result<SleepResponse> {
    sleep_handler()
}

/// Shutdown the client and clean up resources.
///
/// This will:
/// 1. Sleep if currently awake
/// 2. Stop the control plane server
/// 3. Reset global state
///
/// # Errors
///
/// Returns Ok(()) even if not initialized.
pub fn shutdown() -> Result<()> {
    let _init_guard = INIT_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .map_err(|_| Error::ControlPlaneError("init lock poisoned".to_string()))?;

    if !is_initialized() {
        return Ok(());
    }

    // Sleep first to unregister and stop lldb-dap
    let _ = sleep();

    // Stop control server
    let server_holder = CONTROL_SERVER.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut guard) = server_holder.lock() {
        if let Some(mut server) = guard.take() {
            let _ = server.stop();
        }
    }

    // Clear daemon client and lldb manager for re-initialization
    if let Some(holder) = DAEMON_CLIENT.get() {
        if let Ok(mut guard) = holder.lock() {
            *guard = None;
        }
    }
    if let Some(holder) = LLDB_MANAGER.get() {
        if let Ok(mut guard) = holder.lock() {
            *guard = None;
        }
    }

    // Reset state
    state::reset();

    info!("Detrix client shutdown complete");
    Ok(())
}

// ============================================================================
// Internal handlers
// ============================================================================

fn status_provider() -> StatusResponse {
    status()
}

fn discover_provider() -> DiscoverResponse {
    let state = get();
    let guard = state.read().unwrap_or_else(|e| e.into_inner());
    let daemon_url = guard
        .daemon_advertise_url
        .clone()
        .unwrap_or_else(|| guard.daemon_url.clone());
    DiscoverResponse {
        daemon_url,
        name: guard.name.clone(),
    }
}

fn wake_handler(daemon_url: Option<String>) -> Result<WakeResponse> {
    if !is_initialized() {
        return Err(Error::NotInitialized);
    }

    // Acquire wake lock
    let _wake_guard = state::acquire_wake_lock()?;

    // Read current state
    let (
        current_state,
        target_daemon_url,
        debug_host,
        advertise_host,
        debug_port,
        name,
        detrix_home,
        workspace_root_override,
        safe_mode,
        build_commit_override,
        build_tag_override,
        health_timeout,
        register_timeout,
    ) = {
        let state = get();
        let guard = state.read()?;

        let target_url = daemon_url.unwrap_or_else(|| guard.daemon_url.clone());
        (
            guard.state,
            target_url,
            guard.control_host.clone(),
            guard.advertise_host.clone(),
            guard.debug_port,
            guard.name.clone(),
            guard.detrix_home.clone(),
            guard.workspace_root.clone(),
            guard.safe_mode,
            guard.build_commit.clone(),
            guard.build_tag.clone(),
            Duration::from_millis(guard.health_check_timeout_ms),
            Duration::from_millis(guard.register_timeout_ms),
        )
    };

    // Check if already awake
    if matches!(current_state, ClientState::Awake) {
        let state = get();
        let guard = state.read()?;
        return Ok(WakeResponse {
            status: WakeResponseStatus::AlreadyAwake,
            debug_port: i32::from(guard.actual_debug_port),
            connection_id: guard.connection_id.clone().unwrap_or_default(),
            daemon_url: guard.daemon_advertise_url.clone(),
        });
    }

    // Check if wake in progress
    if matches!(current_state, ClientState::Waking) {
        return Err(Error::WakeInProgress);
    }

    // Transition to WAKING
    {
        let state = get();
        let mut guard = state.write()?;
        guard.state = ClientState::Waking;
    }

    // Helper to revert state on error
    let revert_state = || {
        let state = get();
        if let Ok(mut guard) = state.write() {
            guard.state = ClientState::Sleeping;
        }
    };

    // Acquire daemon client and lldb manager locks for the operation.
    // Safe to hold for the duration because wake_lock already serializes access.
    let dc_holder = DAEMON_CLIENT.get().ok_or(Error::NotInitialized)?;
    let mut dc_guard = dc_holder
        .lock()
        .map_err(|_| Error::ControlPlaneError("daemon client lock poisoned".to_string()))?;

    // Re-discover token (daemon may have restarted with a new one)
    let fresh_token = auth::discover_token(detrix_home.as_ref().map(std::path::Path::new));
    if let Some(ref mut dc) = *dc_guard {
        dc.update_auth_token(fresh_token.clone());
    }

    // Update control server token too (so it accepts requests with the new token)
    if let Some(cs_holder) = CONTROL_SERVER.get() {
        if let Ok(cs_guard) = cs_holder.lock() {
            if let Some(ref cs) = *cs_guard {
                cs.update_token(fresh_token);
            }
        }
    }
    let daemon_client = dc_guard.as_ref().ok_or(Error::NotInitialized)?;

    let lm_holder = LLDB_MANAGER.get().ok_or(Error::NotInitialized)?;
    let lm_guard = lm_holder
        .lock()
        .map_err(|_| Error::ControlPlaneError("lldb manager lock poisoned".to_string()))?;
    let lldb_manager = lm_guard.as_ref().ok_or(Error::NotInitialized)?;

    // Check daemon health
    if let Err(e) = daemon_client.health_check(&target_daemon_url, health_timeout) {
        revert_state();
        return Err(e);
    }

    // Spawn lldb-dap and attach
    let lldb_process = match lldb_manager.spawn_and_attach(&debug_host, debug_port) {
        Ok(p) => p,
        Err(e) => {
            revert_state();
            return Err(e);
        }
    };

    let actual_debug_port = lldb_process.port;

    // Store lldb process
    state::set_lldb_process(lldb_process);

    // Get workspace root and hostname for identity
    let workspace_root = workspace_root_override.unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(String::from))
            .unwrap_or_else(|| {
                warn!("Failed to get current directory, using /unknown");
                "/unknown".to_string()
            })
    });

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| {
            warn!("Failed to get hostname, using unknown");
            "unknown".to_string()
        });

    // Detect build information
    let build_commit = build_info::detect_build_commit(build_commit_override);
    let build_tag = build_info::detect_build_tag(build_tag_override);

    // Register with daemon
    // Use advertise_host if set (for Docker/cloud), otherwise use control_host
    // Pass our PID so the daemon can use AttachPid mode with lldb-dap
    let registration_host = advertise_host.unwrap_or(debug_host);
    let (connection_id, advertise_url) = match daemon_client.register(
        &target_daemon_url,
        RegisterRequest {
            host: registration_host,
            port: actual_debug_port,
            language: "rust".to_string(),
            name: name.clone(),
            workspace_root,
            hostname,
            pid: Some(std::process::id()),
            safe_mode,
            build_commit,
            build_tag,
        },
        register_timeout,
    ) {
        Ok(result) => result,
        Err(e) => {
            // Kill lldb and revert state
            if let Some(mut process) = state::take_lldb_process() {
                let _ = lldb_manager.kill(&mut process);
            }
            revert_state();
            return Err(e);
        }
    };

    // Update state to AWAKE
    {
        let state = get();
        let mut guard = state.write()?;
        guard.state = ClientState::Awake;
        guard.actual_debug_port = actual_debug_port;
        guard.debug_port_active = true;
        guard.connection_id = Some(connection_id.clone());
        guard.daemon_advertise_url = advertise_url.clone();
    }

    info!(
        "Detrix client awake. Debug port: {}, Connection ID: {}",
        actual_debug_port, connection_id
    );

    Ok(WakeResponse {
        status: WakeResponseStatus::Awake,
        debug_port: i32::from(actual_debug_port),
        connection_id,
        daemon_url: advertise_url,
    })
}

fn sleep_handler() -> Result<SleepResponse> {
    if !is_initialized() {
        return Err(Error::NotInitialized);
    }

    // Read current state
    let (current_state, daemon_url, connection_id, unregister_timeout) = {
        let state = get();
        let guard = state.read()?;

        (
            guard.state,
            guard.daemon_url.clone(),
            guard.connection_id.clone(),
            Duration::from_millis(guard.unregister_timeout_ms),
        )
    };

    // Check if already sleeping
    if matches!(current_state, ClientState::Sleeping) {
        return Ok(SleepResponse {
            status: SleepResponseStatus::AlreadySleeping,
        });
    }

    // If waking, wait for it to complete
    if matches!(current_state, ClientState::Waking) {
        let _wake_guard = state::acquire_wake_lock()?;
        // Re-check state after acquiring lock
        let state = get();
        if let Ok(guard) = state.read() {
            if matches!(guard.state, ClientState::Sleeping) {
                return Ok(SleepResponse {
                    status: SleepResponseStatus::AlreadySleeping,
                });
            }
        }
    }

    // Unregister from daemon (best effort)
    if let Some(conn_id) = connection_id {
        if let Some(holder) = DAEMON_CLIENT.get() {
            if let Ok(guard) = holder.lock() {
                if let Some(daemon_client) = guard.as_ref() {
                    daemon_client.unregister(&daemon_url, &conn_id, unregister_timeout);
                }
            }
        }
    }

    // Kill lldb-dap process
    if let Some(mut process) = state::take_lldb_process() {
        if let Some(holder) = LLDB_MANAGER.get() {
            if let Ok(guard) = holder.lock() {
                if let Some(lldb_manager) = guard.as_ref() {
                    if let Err(e) = lldb_manager.kill(&mut process) {
                        warn!("Failed to kill lldb-dap: {}", e);
                    }
                }
            }
        }
    }

    // Update state to SLEEPING
    {
        let state = get();
        let mut guard = state.write()?;
        guard.state = ClientState::Sleeping;
        guard.connection_id = None;
        guard.debug_port_active = false;
    }

    info!("Detrix client sleeping");

    Ok(SleepResponse {
        status: SleepResponseStatus::Sleeping,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Most tests require external resources (lldb-dap, daemon)
    // and are better suited for integration tests.

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.name.is_none());
        assert_eq!(config.control_host, "127.0.0.1");
        assert_eq!(config.control_port, 0);
    }

    #[test]
    fn test_status_not_initialized() {
        // Reset any previous state
        state::reset();

        let status = status();
        assert!(matches!(status.state, ClientState::Sleeping));
    }

    #[test]
    fn test_init_lock_exists() {
        // Verify that INIT_LOCK can be acquired and prevents concurrent access
        let lock = INIT_LOCK.get_or_init(|| std::sync::Mutex::new(()));
        let guard = lock.lock();
        assert!(guard.is_ok(), "INIT_LOCK should be acquirable");
        // While held, try_lock should fail
        let second = lock.try_lock();
        assert!(second.is_err(), "INIT_LOCK should not be re-entrant");
    }
}
