//! DAP protocol constants
//!
//! Centralizes magic strings used throughout the DAP implementation.
//! This improves maintainability and prevents typos.

/// DAP event types received from debug adapters
pub mod events {
    /// Output event - logpoint/console output
    pub const OUTPUT: &str = "output";
    /// Stopped event - breakpoint hit, step, etc.
    pub const STOPPED: &str = "stopped";
    /// Initialized event - adapter ready
    pub const INITIALIZED: &str = "initialized";
    /// Terminated event - debug session ended
    pub const TERMINATED: &str = "terminated";
    /// Exited event - debuggee process exited
    pub const EXITED: &str = "exited";
}

/// DAP request command names
pub mod requests {
    /// Initialize the debug adapter
    pub const INITIALIZE: &str = "initialize";
    /// Launch a program for debugging
    pub const LAUNCH: &str = "launch";
    /// Attach to a running process
    pub const ATTACH: &str = "attach";
    /// Set breakpoints in a source file
    pub const SET_BREAKPOINTS: &str = "setBreakpoints";
    /// Get the current stack trace
    pub const STACK_TRACE: &str = "stackTrace";
    /// Continue execution
    pub const CONTINUE: &str = "continue";
    /// Disconnect from the debuggee
    pub const DISCONNECT: &str = "disconnect";
    /// Get variable scopes for a stack frame
    pub const SCOPES: &str = "scopes";
    /// Get variables in a scope
    pub const VARIABLES: &str = "variables";
    /// Evaluate an expression
    pub const EVALUATE: &str = "evaluate";
    /// Configuration done - signals adapter is ready
    pub const CONFIGURATION_DONE: &str = "configurationDone";
}

/// Stopped event reasons
pub mod stop_reasons {
    /// Stopped at a breakpoint
    pub const BREAKPOINT: &str = "breakpoint";
    /// Stopped after a step
    pub const STEP: &str = "step";
    /// Stopped due to pause request
    pub const PAUSE: &str = "pause";
    /// Stopped due to exception
    pub const EXCEPTION: &str = "exception";
    /// Stopped at entry point
    pub const ENTRY: &str = "entry";
}

/// Output event categories
pub mod output_categories {
    /// Standard output
    pub const STDOUT: &str = "stdout";
    /// Standard error
    pub const STDERR: &str = "stderr";
    /// Console output (debugger messages)
    pub const CONSOLE: &str = "console";
    /// Telemetry output
    pub const TELEMETRY: &str = "telemetry";
}

/// DAP protocol default values
///
/// These are fallback values used when optional fields are missing from DAP messages.
/// DAP adapters don't always provide all fields, so reasonable defaults are needed.
pub mod defaults {
    /// Default thread ID when not specified in DAP events.
    ///
    /// DAP protocol says thread_id is optional in some events (like "stopped"),
    /// but we need a thread ID for requests like stackTrace and continue.
    /// Thread 1 is typically the main thread in most applications.
    pub const THREAD_ID: i64 = 1;
}
