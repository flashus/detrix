//! Thread-safe state management for the Detrix client.

use std::sync::{Mutex, MutexGuard, OnceLock, RwLock};

use tracing::warn;

use crate::error::Error;
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
            control_port: i32::from(self.actual_control_port),
            debug_port: i32::from(self.actual_debug_port),
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
        match state.write() {
            Ok(mut guard) => *guard = InternalState::default(),
            Err(poisoned) => {
                warn!("Global state lock poisoned during reset, recovering");
                *poisoned.into_inner() = InternalState::default();
            }
        }
    }
    if let Some(init) = INITIALIZED.get() {
        match init.write() {
            Ok(mut guard) => *guard = false,
            Err(poisoned) => {
                warn!("Initialized lock poisoned during reset, recovering");
                *poisoned.into_inner() = false;
            }
        }
    }
}

/// Check if the client has been initialized.
pub fn is_initialized() -> bool {
    *INITIALIZED
        .get_or_init(|| RwLock::new(false))
        .read()
        .unwrap_or_else(|poisoned| {
            warn!("Initialized lock poisoned during read, recovering");
            poisoned.into_inner()
        })
}

/// Set the initialization flag.
pub fn set_initialized(value: bool) {
    let init = INITIALIZED.get_or_init(|| RwLock::new(false));
    match init.write() {
        Ok(mut guard) => *guard = value,
        Err(poisoned) => {
            warn!("Initialized lock poisoned during write, recovering");
            *poisoned.into_inner() = value;
        }
    }
}

/// Acquire the wake lock to prevent concurrent wake operations.
///
/// # Errors
///
/// Returns `Error::ControlPlaneError` if the lock is poisoned (a thread panicked
/// while holding it). Previously this would panic; now callers should handle
/// the error gracefully.
pub fn acquire_wake_lock() -> Result<MutexGuard<'static, ()>, Error> {
    Ok(WAKE_LOCK.get_or_init(|| Mutex::new(())).lock()?)
}

/// Global lldb-dap process handle.
static LLDB_PROCESS: OnceLock<Mutex<Option<LldbProcess>>> = OnceLock::new();

/// Get the lldb-dap process handle.
pub fn get_lldb_process() -> &'static Mutex<Option<LldbProcess>> {
    LLDB_PROCESS.get_or_init(|| Mutex::new(None))
}

/// Set the lldb-dap process.
pub fn set_lldb_process(process: LldbProcess) {
    match get_lldb_process().lock() {
        Ok(mut guard) => *guard = Some(process),
        Err(poisoned) => {
            warn!("lldb process lock poisoned during set, recovering");
            *poisoned.into_inner() = Some(process);
        }
    }
}

/// Take and clear the lldb-dap process.
pub fn take_lldb_process() -> Option<LldbProcess> {
    match get_lldb_process().lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => {
            warn!("lldb process lock poisoned during take, recovering");
            poisoned.into_inner().take()
        }
    }
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

    #[test]
    fn test_set_initialized_roundtrip() {
        set_initialized(true);
        assert!(is_initialized());
        set_initialized(false);
        assert!(!is_initialized());
    }

    #[test]
    fn test_take_lldb_process_returns_none_when_empty() {
        // Reset any existing process
        let _ = take_lldb_process();
        assert!(take_lldb_process().is_none());
    }

    #[test]
    fn test_to_status_response_port_cast_safe() {
        let mut state = InternalState::default();
        // u16::MAX should safely convert to i32
        state.actual_control_port = u16::MAX;
        state.actual_debug_port = u16::MAX;

        let response = state.to_status_response();
        assert_eq!(response.control_port, i32::from(u16::MAX));
        assert_eq!(response.debug_port, i32::from(u16::MAX));
    }
}
