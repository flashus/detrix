//! Thread-safe state management for the Detrix client.

use std::sync::{Mutex, OnceLock, RwLock};

use crate::generated::{ClientState, StatusResponse};
use crate::lldb::LldbProcess;

/// Global client state singleton.
static GLOBAL_STATE: OnceLock<RwLock<InternalState>> = OnceLock::new();

/// Wake lock to prevent concurrent wake operations.
static WAKE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Initialization flag.
static INITIALIZED: OnceLock<RwLock<bool>> = OnceLock::new();

/// Internal state of the Detrix client.
#[derive(Debug)]
pub struct InternalState {
    /// Current state machine state.
    pub state: ClientState,

    /// Connection name.
    pub name: String,

    /// Control plane host.
    pub control_host: String,

    /// Configured control port (0 = auto).
    pub control_port: u16,

    /// Actual control port after binding.
    pub actual_control_port: u16,

    /// Configured debug port (0 = auto).
    pub debug_port: u16,

    /// Actual debug port after lldb-dap starts.
    pub actual_debug_port: u16,

    /// Whether the debug port is currently active.
    pub debug_port_active: bool,

    /// Daemon URL.
    pub daemon_url: String,

    /// Connection ID from daemon registration.
    pub connection_id: Option<String>,

    /// Path to lldb-dap binary.
    pub lldb_dap_path: String,

    /// Detrix home directory.
    pub detrix_home: Option<String>,

    /// Safe mode enabled.
    pub safe_mode: bool,

    /// Health check timeout in milliseconds.
    pub health_check_timeout_ms: u64,

    /// Register timeout in milliseconds.
    pub register_timeout_ms: u64,

    /// Unregister timeout in milliseconds.
    pub unregister_timeout_ms: u64,

    /// lldb-dap start timeout in milliseconds.
    pub lldb_start_timeout_ms: u64,
}

impl Default for InternalState {
    fn default() -> Self {
        Self {
            state: ClientState::Sleeping,
            name: String::new(),
            control_host: "127.0.0.1".to_string(),
            control_port: 0,
            actual_control_port: 0,
            debug_port: 0,
            actual_debug_port: 0,
            debug_port_active: false,
            daemon_url: "http://127.0.0.1:8090".to_string(),
            connection_id: None,
            lldb_dap_path: String::new(),
            detrix_home: None,
            safe_mode: false,
            health_check_timeout_ms: 2000,
            register_timeout_ms: 5000,
            unregister_timeout_ms: 2000,
            lldb_start_timeout_ms: 10000,
        }
    }
}

impl InternalState {
    /// Create a StatusResponse from the current state.
    pub fn to_status_response(&self) -> StatusResponse {
        StatusResponse {
            state: self.state,
            name: self.name.clone(),
            control_host: self.control_host.clone(),
            control_port: self.actual_control_port as i32,
            debug_port: self.actual_debug_port as i32,
            debug_port_active: self.debug_port_active,
            daemon_url: self.daemon_url.clone(),
            connection_id: self.connection_id.clone(),
        }
    }
}

/// Get the global state, initializing if needed.
pub fn get() -> &'static RwLock<InternalState> {
    GLOBAL_STATE.get_or_init(|| RwLock::new(InternalState::default()))
}

/// Reset the global state to initial values.
pub fn reset() {
    if let Some(state) = GLOBAL_STATE.get() {
        if let Ok(mut guard) = state.write() {
            *guard = InternalState::default();
        }
    }
    if let Some(init) = INITIALIZED.get() {
        if let Ok(mut guard) = init.write() {
            *guard = false;
        }
    }
}

/// Check if the client has been initialized.
pub fn is_initialized() -> bool {
    INITIALIZED
        .get_or_init(|| RwLock::new(false))
        .read()
        .map(|guard| *guard)
        .unwrap_or(false)
}

/// Set the initialization flag.
pub fn set_initialized(value: bool) {
    let init = INITIALIZED.get_or_init(|| RwLock::new(false));
    if let Ok(mut guard) = init.write() {
        *guard = value;
    }
}

/// Acquire the wake lock.
///
/// This prevents concurrent wake operations.
pub fn acquire_wake_lock() -> std::sync::MutexGuard<'static, ()> {
    WAKE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("Wake lock poisoned")
}

/// Try to acquire the wake lock without blocking.
///
/// Returns `Some(guard)` if acquired, `None` if already held.
#[allow(dead_code)]
pub fn try_acquire_wake_lock() -> Option<std::sync::MutexGuard<'static, ()>> {
    WAKE_LOCK.get_or_init(|| Mutex::new(())).try_lock().ok()
}

/// Global lldb-dap process handle.
static LLDB_PROCESS: OnceLock<Mutex<Option<LldbProcess>>> = OnceLock::new();

/// Get the lldb-dap process handle.
pub fn get_lldb_process() -> &'static Mutex<Option<LldbProcess>> {
    LLDB_PROCESS.get_or_init(|| Mutex::new(None))
}

/// Set the lldb-dap process.
pub fn set_lldb_process(process: LldbProcess) {
    if let Ok(mut guard) = get_lldb_process().lock() {
        *guard = Some(process);
    }
}

/// Take and clear the lldb-dap process.
pub fn take_lldb_process() -> Option<LldbProcess> {
    get_lldb_process()
        .lock()
        .ok()
        .and_then(|mut guard| guard.take())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_state() {
        let state = InternalState::default();
        assert!(matches!(state.state, ClientState::Sleeping));
        assert_eq!(state.control_host, "127.0.0.1");
    }

    #[test]
    fn test_status_response() {
        let mut state = InternalState::default();
        state.name = "test-client".to_string();
        state.actual_control_port = 8091;

        let response = state.to_status_response();
        assert_eq!(response.name, "test-client");
        assert_eq!(response.control_port, 8091);
    }
}
