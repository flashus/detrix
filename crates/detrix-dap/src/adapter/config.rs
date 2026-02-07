//! Adapter Configuration
//!
//! Configuration types for spawning and connecting to debug adapters.

use detrix_config::AdapterConnectionConfig;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// Connection mode for the debug adapter
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ConnectionMode {
    /// Launch a new debug adapter process (spawn subprocess)
    #[default]
    Launch,
    /// Attach to an existing debug adapter (TCP connection)
    /// This spawns an adapter subprocess that connects to the debugpy server
    Attach {
        /// Host where debugpy server is listening (e.g., "127.0.0.1")
        host: String,
        /// Port where debugpy server is listening
        port: u16,
    },
    /// Attach to a remote headless debug server (e.g., `dlv exec --headless`)
    ///
    /// This mode is used when connecting to a pre-started debug server that
    /// already has a target loaded. Unlike regular Attach, this sends a DAP
    /// "attach" request with mode="remote" which tells the server we expect
    /// it to already be debugging something.
    ///
    /// See: <https://github.com/go-delve/delve/blob/master/Documentation/api/dap/README.md>
    AttachRemote {
        /// Host where debug server is listening
        host: String,
        /// Port where debug server is listening
        port: u16,
    },
    /// Launch a program via DAP (for lldb-dap server mode)
    ///
    /// This mode is used when connecting to a DAP server (like lldb-dap in
    /// `--connection listen://` mode) that doesn't have a target loaded yet.
    /// We send a DAP "launch" request with the program path to start debugging.
    ///
    /// See: <https://github.com/llvm/llvm-project/blob/main/lldb/tools/lldb-dap/README.md>
    LaunchProgram {
        /// Host where DAP server is listening
        host: String,
        /// Port where DAP server is listening
        port: u16,
        /// Path to the program to launch
        program: String,
        /// Optional arguments for the program
        args: Vec<String>,
        /// Stop on entry (pause at program start)
        stop_on_entry: bool,
        /// LLDB initialization commands (e.g., type formatters, settings)
        ///
        /// These are sent in the launch request as `initCommands` and executed
        /// BEFORE the target is created. Used for setting up:
        /// - Type formatters (type summary add)
        /// - LLDB settings (settings set ...)
        /// - Breakpoint aliases
        init_commands: Vec<String>,
        /// LLDB pre-run commands (e.g., symbol loading)
        ///
        /// These are sent in the launch request as `preRunCommands` and executed
        /// AFTER the target is created but BEFORE launch. Used for:
        /// - Loading PDB/dSYM symbols (target symbols add)
        /// - Target-specific configuration
        pre_run_commands: Vec<String>,
    },
    /// Attach to a running process by PID or program name (for lldb-dap)
    ///
    /// This mode is used when connecting to lldb-dap and attaching to an
    /// already running process. The process can be specified by:
    /// - PID: Direct process ID
    /// - Program path: lldb-dap finds the process by executable path
    /// - waitFor: Wait for the next instance to launch (macOS only)
    ///
    /// See: <https://github.com/llvm/llvm-project/blob/main/lldb/tools/lldb-dap/README.md>
    AttachPid {
        /// Host where DAP server is listening
        host: String,
        /// Port where DAP server is listening
        port: u16,
        /// Process ID to attach to (optional if program is specified)
        pid: Option<u32>,
        /// Path to the program (used to find process if pid is not specified)
        program: Option<String>,
        /// Wait for the next instance to launch (macOS only)
        wait_for: bool,
        /// LLDB initialization commands (e.g., type formatters, settings)
        ///
        /// These are sent in the attach request as `initCommands` and executed
        /// BEFORE attaching. Used for setting up:
        /// - Type formatters (type summary add)
        /// - LLDB settings (settings set ...)
        init_commands: Vec<String>,
    },
}

/// Configuration for spawning the debugpy adapter subprocess
#[derive(Debug, Clone)]
pub struct AdapterSubprocessConfig {
    /// Python executable path
    pub python_path: String,
    /// Path to debugpy adapter (e.g., "debugpy/adapter" or full path)
    pub adapter_path: Option<String>,
    /// Host for adapter to listen on
    pub adapter_host: Ipv4Addr,
    /// Connection timeout in milliseconds
    pub timeout_ms: u64,
}

impl Default for AdapterSubprocessConfig {
    fn default() -> Self {
        let adapter_config = AdapterConnectionConfig::default();
        Self {
            python_path: "python".to_string(),
            adapter_path: None,
            adapter_host: Ipv4Addr::new(127, 0, 0, 1),
            timeout_ms: adapter_config.connection_timeout_ms,
        }
    }
}

/// Configuration for spawning a debug adapter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterConfig {
    /// Adapter ID (e.g., "python", "go", "rust")
    pub adapter_id: String,

    /// Command to execute (e.g., "python", "dlv")
    pub command: String,

    /// Arguments for the command
    pub args: Vec<String>,

    /// Working directory (optional)
    pub cwd: Option<String>,

    /// Environment variables to set
    pub env: Vec<(String, String)>,

    /// Port to listen on (if not in args, will be substituted)
    pub port: Option<u16>,

    /// Connection mode (Launch or Attach)
    #[serde(default)]
    pub connection_mode: ConnectionMode,
}

impl AdapterConfig {
    /// Create a new adapter config
    pub fn new(adapter_id: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            port: None,
            connection_mode: ConnectionMode::default(),
        }
    }

    /// Add an argument
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments
    pub fn args(mut self, args: Vec<String>) -> Self {
        self.args.extend(args);
        self
    }

    /// Set working directory
    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Add environment variable
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Set port (will substitute {port} in args)
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set connection mode to Attach
    pub fn attach(mut self, host: impl Into<String>, port: u16) -> Self {
        self.connection_mode = ConnectionMode::Attach {
            host: host.into(),
            port,
        };
        self
    }

    /// Set connection mode to AttachRemote (for headless debuggers like `dlv exec --headless`)
    ///
    /// This mode connects via TCP and sends a DAP "attach" request with mode="remote",
    /// which tells the server we expect it to already be debugging a target.
    pub fn attach_remote(mut self, host: impl Into<String>, port: u16) -> Self {
        self.connection_mode = ConnectionMode::AttachRemote {
            host: host.into(),
            port,
        };
        self
    }

    /// Set connection mode to LaunchProgram (for lldb-dap server mode)
    ///
    /// This mode connects via TCP and sends a DAP "launch" request with the program path.
    /// Used when connecting to lldb-dap started with `--connection listen://host:port`.
    pub fn launch_program(
        mut self,
        host: impl Into<String>,
        port: u16,
        program: impl Into<String>,
    ) -> Self {
        self.connection_mode = ConnectionMode::LaunchProgram {
            host: host.into(),
            port,
            program: program.into(),
            args: Vec::new(),
            stop_on_entry: false,
            init_commands: Vec::new(),
            pre_run_commands: Vec::new(),
        };
        self
    }

    /// Set connection mode to LaunchProgram with program arguments
    pub fn launch_program_with_args(
        mut self,
        host: impl Into<String>,
        port: u16,
        program: impl Into<String>,
        args: Vec<String>,
        stop_on_entry: bool,
    ) -> Self {
        self.connection_mode = ConnectionMode::LaunchProgram {
            host: host.into(),
            port,
            program: program.into(),
            args,
            stop_on_entry,
            init_commands: Vec::new(),
            pre_run_commands: Vec::new(),
        };
        self
    }

    /// Set connection mode to LaunchProgram with initialization commands
    ///
    /// This mode connects via TCP and sends a DAP "launch" request with the program path
    /// and LLDB initialization commands. The init_commands are executed BEFORE the
    /// target is created (good for settings and type formatters).
    ///
    /// Used for:
    /// - Direct lldb-dap connection (without the lldb-serve proxy)
    /// - Setting up Rust type formatters for readable String/Vec/etc. display
    ///
    /// # Example
    /// ```ignore
    /// let config = AdapterConfig::new("rust", "lldb-dap")
    ///     .launch_program_with_init_commands(
    ///         "127.0.0.1", 4711, "/path/to/binary",
    ///         vec![], false,
    ///         RUST_LLDB_TYPE_FORMATTERS.iter().map(|s| s.to_string()).collect(),
    ///     );
    /// ```
    pub fn launch_program_with_init_commands(
        mut self,
        host: impl Into<String>,
        port: u16,
        program: impl Into<String>,
        args: Vec<String>,
        stop_on_entry: bool,
        init_commands: Vec<String>,
    ) -> Self {
        self.connection_mode = ConnectionMode::LaunchProgram {
            host: host.into(),
            port,
            program: program.into(),
            args,
            stop_on_entry,
            init_commands,
            pre_run_commands: Vec::new(),
        };
        self
    }

    /// Set connection mode to LaunchProgram with both init and pre-run commands
    ///
    /// This mode connects via TCP and sends a DAP "launch" request with:
    /// - `initCommands`: Executed BEFORE target creation (settings, type formatters)
    /// - `preRunCommands`: Executed AFTER target creation but before launch (symbol loading)
    ///
    /// Used for Windows PDB symbol loading where `target symbols add` must run
    /// after the target is created.
    ///
    /// # Example
    /// ```ignore
    /// let config = AdapterConfig::new("rust", "lldb-dap")
    ///     .launch_program_with_commands(
    ///         "127.0.0.1", 4711, "/path/to/binary",
    ///         vec![], false,
    ///         vec!["settings set target.debug-file-search-paths ...".into()],
    ///         vec!["target symbols add /path/to/file.pdb".into()],
    ///     );
    /// ```
    pub fn launch_program_with_commands(
        mut self,
        host: impl Into<String>,
        port: u16,
        program: impl Into<String>,
        args: Vec<String>,
        stop_on_entry: bool,
        init_commands: Vec<String>,
        pre_run_commands: Vec<String>,
    ) -> Self {
        self.connection_mode = ConnectionMode::LaunchProgram {
            host: host.into(),
            port,
            program: program.into(),
            args,
            stop_on_entry,
            init_commands,
            pre_run_commands,
        };
        self
    }

    /// Set connection mode to AttachPid (for lldb-dap attach by PID)
    ///
    /// This mode connects via TCP and sends a DAP "attach" request with the PID.
    /// Used when connecting to lldb-dap and attaching to a running process.
    pub fn attach_pid(mut self, host: impl Into<String>, port: u16, pid: u32) -> Self {
        self.connection_mode = ConnectionMode::AttachPid {
            host: host.into(),
            port,
            pid: Some(pid),
            program: None,
            wait_for: false,
            init_commands: Vec::new(),
        };
        self
    }

    /// Set connection mode to AttachPid with init commands (for type formatters)
    ///
    /// This mode connects via TCP and sends a DAP "attach" request with the PID
    /// and includes LLDB init commands for type formatters.
    pub fn attach_pid_with_commands(
        mut self,
        host: impl Into<String>,
        port: u16,
        pid: u32,
        init_commands: Vec<String>,
    ) -> Self {
        self.connection_mode = ConnectionMode::AttachPid {
            host: host.into(),
            port,
            pid: Some(pid),
            program: None,
            wait_for: false,
            init_commands,
        };
        self
    }

    /// Set connection mode to AttachPid with program name (lldb-dap finds the process)
    ///
    /// This mode connects via TCP and sends a DAP "attach" request with the program path.
    /// lldb-dap will find the process by its executable name.
    pub fn attach_program(
        mut self,
        host: impl Into<String>,
        port: u16,
        program: impl Into<String>,
    ) -> Self {
        self.connection_mode = ConnectionMode::AttachPid {
            host: host.into(),
            port,
            pid: None,
            program: Some(program.into()),
            wait_for: false,
            init_commands: Vec::new(),
        };
        self
    }

    /// Set connection mode to AttachPid with waitFor (macOS only)
    ///
    /// This mode connects via TCP and sends a DAP "attach" request with waitFor=true.
    /// lldb-dap will wait for the next instance of the program to launch.
    pub fn attach_wait_for(
        mut self,
        host: impl Into<String>,
        port: u16,
        program: impl Into<String>,
    ) -> Self {
        self.connection_mode = ConnectionMode::AttachPid {
            host: host.into(),
            port,
            pid: None,
            program: Some(program.into()),
            wait_for: true,
            init_commands: Vec::new(),
        };
        self
    }

    /// Set connection mode to Attach with custom subprocess config
    pub fn attach_with_subprocess(
        mut self,
        host: impl Into<String>,
        port: u16,
        subprocess_config: AdapterSubprocessConfig,
    ) -> Self {
        self.connection_mode = ConnectionMode::Attach {
            host: host.into(),
            port,
        };
        // Store subprocess config in command/args for now
        self.command = subprocess_config.python_path;
        if let Some(adapter_path) = subprocess_config.adapter_path {
            self.args = vec![adapter_path];
        }
        self
    }

    /// Substitute {port} in args with actual port
    pub fn substitute_port(&self) -> Vec<String> {
        if let Some(port) = self.port {
            self.args
                .iter()
                .map(|arg| arg.replace("{port}", &port.to_string()))
                .collect()
        } else {
            self.args.clone()
        }
    }
}
