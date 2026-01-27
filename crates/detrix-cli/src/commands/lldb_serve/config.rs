//! Configuration types for lldb-serve command

use clap::Args;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

/// Arguments for the lldb-serve command
#[derive(Args, Debug)]
pub struct LldbServeArgs {
    /// Path to the Rust binary to debug
    #[arg(required = true)]
    pub program: PathBuf,

    /// Arguments to pass to the program
    #[arg(trailing_var_arg = true)]
    pub program_args: Vec<String>,

    /// Listen address for DAP connections (default: 127.0.0.1:4711)
    #[arg(long, default_value = "127.0.0.1:4711")]
    pub listen: String,

    /// Stop the program at entry point (like debugpy --wait-for-client)
    #[arg(long)]
    pub stop_on_entry: bool,

    /// Path to lldb-dap executable (auto-detected if not specified)
    #[arg(long)]
    pub lldb_dap_path: Option<PathBuf>,

    /// Working directory for the debugged program
    #[arg(long)]
    pub cwd: Option<PathBuf>,

    /// Keep running after client disconnects (allow reconnection)
    /// Without this flag, server exits after first client disconnects.
    #[arg(long)]
    pub persist: bool,
}

/// State for tracking session progress
pub struct ServerState {
    /// Whether launch request has been sent to lldb-dap
    pub launch_sent: AtomicBool,
    /// Whether a real DAP session was established (received initialize request)
    pub session_established: AtomicBool,
}

/// Result of handling a client connection
pub enum ClientResult {
    /// Real DAP session was established and completed normally
    SessionCompleted,
    /// Client disconnected before establishing a real session (e.g., port scan)
    NoSession,
    /// Error during session
    Error(anyhow::Error),
}
