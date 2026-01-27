//! Rust Adapter Integration (CodeLLDB)
//!
//! Provides Rust-specific functionality for working with CodeLLDB:
//! - Setting metrics as logpoints via DAP
//! - Parsing output events
//! - Managing Rust-specific configuration
//!
//! # Architecture
//!
//! This module uses `BaseAdapter<RustOutputParser>` for common DAP operations.
//! Rust-specific behavior is implemented in `RustOutputParser`:
//! - Output parsing with DETRICS: prefix
//! - LLDB expression syntax support
//!
//! CodeLLDB is a VS Code debugger extension powered by LLDB. It supports:
//! - Native debugging of Rust, C++, and other compiled languages
//! - Logpoints with expression interpolation: `"Value: {expression}"`
//! - Multiple expression syntaxes: native, Python, Simple
//!
//! # Usage
//!
//! Start CodeLLDB in DAP mode and attach to a running Rust process:
//! ```bash
//! # Build your Rust program with debug info
//! cargo build
//!
//! # Start the program (paused for debugger attach)
//! # CodeLLDB connects via DAP protocol
//! ```

use crate::{
    base::{
        create_metric_event_from_logpoint, parse_logpoint_core, BaseAdapter, OutputParser,
        ThreadExtractor, ThreadInfo, DETRICS_PREFIX,
    },
    error_detection::{parse_error_output_common, RUST_ERROR_PATTERNS},
    AdapterConfig, OutputEventBody,
};
use async_trait::async_trait;
use detrix_config::AdapterConnectionConfig;
use detrix_core::{Metric, MetricEvent};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::trace;

// ============================================================================
// RustOutputParser - Rust/CodeLLDB-specific output parsing
// ============================================================================

/// Rust-specific output parser for CodeLLDB.
///
/// Handles:
/// - DETRICS logpoint output parsing
/// - LLDB expression evaluation results
/// - Thread ID extraction from LLDB output
///
/// CodeLLDB logpoints use curly braces for expression interpolation:
/// `"DETRICS:metric_name={expression}"` becomes `"DETRICS:metric_name=value"`
pub struct RustOutputParser;

/// Rust/LLDB thread extractor.
///
/// Extracts thread ID from various LLDB output formats:
/// - "thread #1: DETRICS:metric_name=value"
/// - "* thread #2, stop reason = breakpoint\nDETRICS:metric_name=value"
/// - "[thread 3] DETRICS:metric_name=value"
#[derive(Debug, Default, Clone, Copy)]
pub struct RustThreadExtractor;

impl ThreadExtractor for RustThreadExtractor {
    fn extract_thread_info(&self, output: &str) -> ThreadInfo {
        // Pattern 1: "thread #N: ..." or "thread #N, ..."
        if let Some(rest) = output
            .strip_prefix("thread #")
            .or_else(|| output.strip_prefix("* thread #"))
        {
            // Find where the thread number ends
            let end_pos = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            if end_pos > 0 {
                if let Ok(thread_id) = rest[..end_pos].parse::<i64>() {
                    // Find DETRICS: in the remaining text
                    if let Some(detrix_pos) = rest.find(DETRICS_PREFIX) {
                        return ThreadInfo::with_thread(thread_id, &rest[detrix_pos..]);
                    }
                }
            }
        }

        // Pattern 2: "[thread N] DETRICS:..."
        if let Some(rest) = output.strip_prefix("[thread ") {
            if let Some(bracket_pos) = rest.find(']') {
                if let Ok(thread_id) = rest[..bracket_pos].trim().parse::<i64>() {
                    let remaining = rest[bracket_pos + 1..].trim_start();
                    if remaining.starts_with(DETRICS_PREFIX) {
                        return ThreadInfo::with_thread(thread_id, remaining);
                    }
                }
            }
        }

        // Pattern 3: Multi-line output where thread info is on first line
        // "* thread #1, name = 'main', stop reason = breakpoint\n  DETRICS:metric=value"
        if output.contains('\n') {
            let mut found_thread_id: Option<i64> = None;

            for line in output.lines() {
                let line = line.trim();

                // Check for thread info
                if line.starts_with("thread #") || line.starts_with("* thread #") {
                    let thread_part = line
                        .strip_prefix("* thread #")
                        .or_else(|| line.strip_prefix("thread #"))
                        .unwrap_or(line);
                    let end_pos = thread_part
                        .find(|c: char| !c.is_ascii_digit())
                        .unwrap_or(thread_part.len());
                    if end_pos > 0 {
                        if let Ok(thread_id) = thread_part[..end_pos].parse::<i64>() {
                            found_thread_id = Some(thread_id);
                        }
                    }
                }

                // Direct DETRICS line - use thread if found earlier
                if line.starts_with(DETRICS_PREFIX) {
                    if let Some(thread_id) = found_thread_id {
                        return ThreadInfo::with_thread(thread_id, line);
                    }
                    return ThreadInfo::without_thread(line);
                }
            }
        }

        // Fallback: no thread info, try to find DETRICS: directly
        if let Some(detrix_pos) = output.find(DETRICS_PREFIX) {
            return ThreadInfo::without_thread(&output[detrix_pos..]);
        }

        ThreadInfo::without_thread(output)
    }
}

impl RustOutputParser {
    /// Parse error output from lldb-dap into MetricEvent.
    ///
    /// Uses shared error detection patterns from `error_detection.rs` to identify
    /// LLDB/Rust errors like:
    /// - "error: use of undeclared identifier 'x'"
    /// - "expression error: cannot evaluate expression"
    /// - "index out of bounds: the len is 5 but the index is 10"
    async fn parse_error_output(
        output: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent> {
        parse_error_output_common(output, active_metrics, RUST_ERROR_PATTERNS).await
    }
}

#[async_trait]
impl OutputParser for RustOutputParser {
    async fn parse_output(
        body: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent> {
        let output = body.output.trim();

        // Use shared parsing with Rust-specific thread extraction
        let extractor = RustThreadExtractor;
        if let Some(parse_result) = parse_logpoint_core(output, &extractor) {
            trace!(
                "Parsing CodeLLDB DETRICS logpoint: {} (thread_id={:?})",
                parse_result.metric_name,
                parse_result.thread_info.thread_id
            );
            return create_metric_event_from_logpoint(&parse_result, active_metrics).await;
        }

        // Try error detection as fallback
        Self::parse_error_output(body, active_metrics).await
    }

    fn language_name() -> &'static str {
        "rust"
    }

    /// CodeLLDB starts in running state by default when attaching,
    /// but may need continue depending on launch configuration.
    fn needs_continue_after_connect() -> bool {
        false
    }
}

// ============================================================================
// RustAdapter - Type alias for BaseAdapter<RustOutputParser>
// ============================================================================

/// Rust adapter for CodeLLDB.
///
/// This is a type alias for `BaseAdapter<RustOutputParser>`, providing
/// Rust-specific output parsing while reusing common DAP logic.
///
/// # CodeLLDB Setup
///
/// CodeLLDB is typically used through VS Code, but can be used standalone:
///
/// 1. Install CodeLLDB extension in VS Code
/// 2. The extension includes the LLDB debugger
/// 3. For standalone use, the `lldb-dap` (formerly `lldb-vscode`) binary can be used
///
/// # Expression Syntax
///
/// CodeLLDB supports multiple expression syntaxes:
/// - **Native**: Uses the language's native syntax (Rust)
/// - **Simple**: Simplified syntax for common operations
/// - **Python**: Full Python expressions with `$variable` substitution
pub type RustAdapter = BaseAdapter<RustOutputParser>;

// ============================================================================
// RustAdapter Configuration Methods
// ============================================================================

impl RustAdapter {
    /// Create default CodeLLDB configuration for attach mode.
    ///
    /// This connects to a DAP server (like `detrix lldb-serve` or lldb-dap)
    /// and sends attach request. The DAP server should have already launched
    /// or be ready to attach to a target.
    ///
    /// This mirrors the behavior of Python (debugpy) and Go (delve) adapters.
    ///
    /// Typical workflow:
    /// 1. User starts: `detrix lldb-serve ./binary --listen localhost:4711`
    /// 2. Detrix connects to port 4711 and sends attach (this config)
    /// 3. The wrapper handles the launch internally
    pub fn default_config_with_connection(
        connection_config: &AdapterConnectionConfig,
    ) -> AdapterConfig {
        // Use regular Attach mode - the target is already loaded by the wrapper
        // This is the same pattern as Python (debugpy) and Go (delve)
        AdapterConfig::new("rust", "lldb-dap").attach(
            &connection_config.default_host,
            connection_config.default_port,
        )
    }

    /// Create default CodeLLDB configuration for attach mode with explicit port.
    ///
    /// This connects to a DAP server (like `detrix lldb-serve` or lldb-dap)
    /// and sends attach request. The DAP server should have already launched
    /// or be ready to attach to a target.
    ///
    /// Typical workflow:
    /// 1. User starts: `detrix lldb-serve ./binary --listen localhost:4711`
    /// 2. Detrix connects to port 4711 and sends attach (this config)
    /// 3. The wrapper handles the launch internally
    pub fn default_config(port: u16) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        // Use regular Attach mode - the target is already loaded by the wrapper
        // This is the same pattern as Python (debugpy) and Go (delve)
        AdapterConfig::new("rust", "lldb-dap").attach(&config.default_host, port)
    }

    /// Create CodeLLDB configuration to attach by PID.
    ///
    /// This connects to lldb-dap server and sends an attach request with the PID.
    /// The process must already be running.
    ///
    /// lldb-dap must be started in server mode: `lldb-dap --connection listen://host:port`
    ///
    /// # Arguments
    /// * `pid` - Process ID to attach to
    /// * `port` - Port where lldb-dap is listening
    pub fn config_with_pid(pid: u32, port: u16) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        AdapterConfig::new("rust", "lldb-dap").attach_pid(&config.default_host, port, pid)
    }

    /// Create CodeLLDB configuration to attach by program path (find by name).
    ///
    /// This connects to lldb-dap server and sends an attach request with the program path.
    /// lldb-dap will find the process by its executable name.
    ///
    /// lldb-dap must be started in server mode: `lldb-dap --connection listen://host:port`
    ///
    /// # Arguments
    /// * `program` - Path to the executable (lldb-dap finds the running process)
    /// * `port` - Port where lldb-dap is listening
    pub fn config_with_program(program: &str, port: u16) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        AdapterConfig::new("rust", "lldb-dap").attach_program(&config.default_host, port, program)
    }

    /// Create CodeLLDB configuration to wait for program launch (macOS only).
    ///
    /// This connects to lldb-dap server and sends an attach request with waitFor=true.
    /// lldb-dap will wait for the next instance of the program to launch.
    ///
    /// lldb-dap must be started in server mode: `lldb-dap --connection listen://host:port`
    ///
    /// # Arguments
    /// * `program` - Path to the executable to wait for
    /// * `port` - Port where lldb-dap is listening
    pub fn config_wait_for_program(program: &str, port: u16) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        AdapterConfig::new("rust", "lldb-dap").attach_wait_for(&config.default_host, port, program)
    }

    /// Create CodeLLDB configuration for launch mode (lldb-dap starts the program).
    ///
    /// This connects to lldb-dap server and sends a launch request with the program path.
    /// lldb-dap will load and start the target.
    ///
    /// lldb-dap must be started in server mode: `lldb-dap --connection listen://host:port`
    ///
    /// # Arguments
    /// * `program` - Path to the executable to debug
    /// * `port` - Port where lldb-dap is listening
    pub fn config_launch_program(program: &str, port: u16) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        AdapterConfig::new("rust", "lldb-dap").launch_program(&config.default_host, port, program)
    }

    /// Create CodeLLDB configuration for launch mode with program arguments.
    ///
    /// # Arguments
    /// * `program` - Path to the executable to debug
    /// * `args` - Arguments to pass to the program
    /// * `port` - Port where lldb-dap is listening
    /// * `stop_on_entry` - Whether to pause at program start
    pub fn config_launch_with_args(
        program: &str,
        args: Vec<String>,
        port: u16,
        stop_on_entry: bool,
    ) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        AdapterConfig::new("rust", "lldb-dap").launch_program_with_args(
            &config.default_host,
            port,
            program,
            args,
            stop_on_entry,
        )
    }

    // =========================================================================
    // Direct lldb-dap Connection (New Preferred Way)
    // =========================================================================
    //
    // These methods enable direct TCP connection to lldb-dap's native server mode,
    // making Rust debugging work identically to Python (debugpy) and Go (delve).
    //
    // Architecture:
    //   Detrix → lldb-dap (TCP)
    //
    // vs. Proxy mode (lldb-serve):
    //   Detrix → lldb-serve → lldb-dap (stdio)
    //
    // Direct connection benefits:
    // - Same architecture as Python/Go adapters
    // - Simpler (no proxy code, fewer threads)
    // - Type formatters via initCommands in launch request
    //
    // Usage:
    //   1. Start lldb-dap: `lldb-dap --connection listen://localhost:4711`
    //   2. Use config_direct_launch() or config_direct_launch_with_args()

    /// Create configuration for direct lldb-dap connection (new preferred way).
    ///
    /// This connects directly to lldb-dap in server mode and sends a launch request
    /// with Rust type formatters included as initCommands. No proxy needed.
    ///
    /// # Architecture
    /// ```text
    /// Detrix → lldb-dap (TCP on port)
    /// ```
    ///
    /// # Usage
    /// ```bash
    /// # 1. Start lldb-dap in server mode
    /// lldb-dap --connection listen://localhost:4711
    ///
    /// # 2. Connect with Detrix using this config
    /// ```
    ///
    /// # Arguments
    /// * `program` - Path to the executable to debug (must be built with debug info)
    /// * `port` - Port where lldb-dap is listening
    ///
    /// # Type Formatters
    /// Automatically includes LLDB type summary commands for readable Rust types:
    /// - String shows actual content instead of internal struct
    /// - Vec shows length and capacity
    /// - Option/Result show variant
    /// - Box/Rc/Arc dereference the pointer
    ///
    /// On Windows, also adds commands to help LLDB find PDB debug symbols.
    pub fn config_direct_launch(program: &str, port: u16) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        AdapterConfig::new("rust", "lldb-dap").launch_program_with_commands(
            &config.default_host,
            port,
            program,
            vec![],
            false,
            Self::build_init_commands(program),
            Self::build_pre_run_commands(program),
        )
    }

    /// Build LLDB init commands for Rust debugging.
    ///
    /// Includes:
    /// - Type formatters for readable Rust types
    /// - Windows-specific symbol search path settings
    ///
    /// NOTE: initCommands run BEFORE target creation, so only settings work here.
    /// Use build_pre_run_commands() for commands that require a target.
    #[allow(unused_variables, unused_mut)]
    fn build_init_commands(program: &str) -> Vec<String> {
        use detrix_config::RUST_LLDB_TYPE_FORMATTERS;

        let mut commands: Vec<String> = RUST_LLDB_TYPE_FORMATTERS
            .iter()
            .map(|s| s.to_string())
            .collect();

        // On Windows, add settings to help LLDB find PDB symbols
        // NOTE: Only SETTINGS can go in initCommands (before target creation)
        #[cfg(target_os = "windows")]
        {
            // Get the directory containing the program
            if let Some(parent) = std::path::Path::new(program).parent() {
                let parent_str = parent.to_string_lossy();

                // Check if PDB exists for debugging
                let pdb_path = program
                    .strip_suffix(".exe")
                    .or_else(|| program.strip_suffix(".EXE"))
                    .map(|p| format!("{}.pdb", p))
                    .unwrap_or_else(|| format!("{}.pdb", program));

                let pdb_exists = std::path::Path::new(&pdb_path).exists();
                tracing::info!(
                    "[Windows PDB] Program: {}, Search path: {}, PDB path: {}, PDB exists: {}",
                    program,
                    parent_str,
                    pdb_path,
                    pdb_exists
                );

                // Add the program's directory to symbol search paths
                commands.insert(
                    0,
                    format!(
                        r#"settings set target.debug-file-search-paths "{}""#,
                        parent_str
                    ),
                );
            }
        }

        commands
    }

    /// Build LLDB preRunCommands for Rust debugging.
    ///
    /// These commands run AFTER target creation but BEFORE launch/attach.
    /// Use this for commands that require a target to exist (like loading symbols).
    ///
    /// NOTE: We no longer explicitly load PDB files here because LLDB's PDB support
    /// is limited and often fails with "does not match any existing module".
    /// Instead, we rely on `settings set target.debug-file-search-paths` in initCommands
    /// to let LLDB auto-discover symbols.
    #[allow(unused_variables)]
    fn build_pre_run_commands(_program: &str) -> Vec<String> {
        // Currently empty - LLDB auto-discovers symbols via debug-file-search-paths
        Vec::new()
    }

    /// Create configuration for direct lldb-dap connection with program arguments.
    ///
    /// Same as `config_direct_launch()` but allows specifying program arguments
    /// and stop-on-entry behavior.
    ///
    /// # Arguments
    /// * `program` - Path to the executable to debug
    /// * `args` - Arguments to pass to the program
    /// * `port` - Port where lldb-dap is listening
    /// * `stop_on_entry` - Whether to pause at program start (useful for setting
    ///   early breakpoints)
    ///
    /// # Example
    /// ```ignore
    /// let config = RustAdapter::config_direct_launch_with_args(
    ///     "/path/to/binary",
    ///     vec!["--verbose".to_string(), "--port=8080".to_string()],
    ///     4711,
    ///     false,
    /// );
    /// ```
    ///
    /// On Windows, also adds commands to help LLDB find PDB debug symbols.
    pub fn config_direct_launch_with_args(
        program: &str,
        args: Vec<String>,
        port: u16,
        stop_on_entry: bool,
    ) -> AdapterConfig {
        let config = AdapterConnectionConfig::default();
        AdapterConfig::new("rust", "lldb-dap").launch_program_with_commands(
            &config.default_host,
            port,
            program,
            args,
            stop_on_entry,
            Self::build_init_commands(program),
            Self::build_pre_run_commands(program),
        )
    }

    /// Build a logpoint message for Rust/LLDB expressions.
    ///
    /// CodeLLDB logpoints use curly braces for expression interpolation.
    /// The expression is evaluated in the context of the current frame.
    ///
    /// # Example
    /// ```text
    /// // For a metric with expression "user.name"
    /// // This generates: "DETRICS:metric_name={user.name}"
    /// ```
    pub fn build_logpoint_message(metric: &Metric) -> String {
        // CodeLLDB uses the same format as debugpy/Delve
        RustOutputParser::build_logpoint_message(metric)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::OutputCategory;
    use detrix_core::entities::MetricId;
    use detrix_core::SourceLanguage;
    use detrix_testing::fixtures::sample_metric_for_language;
    use std::path::PathBuf;

    fn sample_metric() -> Metric {
        let mut metric = sample_metric_for_language("test_metric", SourceLanguage::Rust);
        metric.id = Some(MetricId(1));
        metric
    }

    #[test]
    fn test_default_config() {
        let config = RustAdapter::default_config(4711);
        assert_eq!(config.adapter_id, "rust");
        assert_eq!(config.command, "lldb-dap");
        match config.connection_mode {
            crate::ConnectionMode::Attach { port, .. } => {
                assert_eq!(port, 4711);
            }
            _ => panic!("Expected Attach mode"),
        }
    }

    #[test]
    fn test_config_with_pid() {
        let config = RustAdapter::config_with_pid(12345, 4711);
        assert_eq!(config.adapter_id, "rust");
        assert_eq!(config.command, "lldb-dap");
        match config.connection_mode {
            crate::ConnectionMode::AttachPid { port, pid, .. } => {
                assert_eq!(port, 4711);
                assert_eq!(pid, Some(12345));
            }
            _ => panic!("Expected AttachPid mode"),
        }
    }

    #[test]
    fn test_config_with_program() {
        let config = RustAdapter::config_with_program("/path/to/binary", 4711);
        assert_eq!(config.adapter_id, "rust");
        assert_eq!(config.command, "lldb-dap");
        match config.connection_mode {
            crate::ConnectionMode::AttachPid {
                port,
                program,
                wait_for,
                ..
            } => {
                assert_eq!(port, 4711);
                assert_eq!(program, Some("/path/to/binary".to_string()));
                assert!(!wait_for); // attach_program doesn't use waitFor
            }
            _ => panic!("Expected AttachPid mode"),
        }
    }

    #[test]
    fn test_config_wait_for_program() {
        let config = RustAdapter::config_wait_for_program("/path/to/binary", 4711);
        assert_eq!(config.adapter_id, "rust");
        match config.connection_mode {
            crate::ConnectionMode::AttachPid {
                port,
                program,
                wait_for,
                ..
            } => {
                assert_eq!(port, 4711);
                assert_eq!(program, Some("/path/to/binary".to_string()));
                assert!(wait_for);
            }
            _ => panic!("Expected AttachPid mode"),
        }
    }

    #[test]
    fn test_config_launch_program() {
        let config = RustAdapter::config_launch_program("/path/to/binary", 4711);
        assert_eq!(config.adapter_id, "rust");
        match config.connection_mode {
            crate::ConnectionMode::LaunchProgram { port, program, .. } => {
                assert_eq!(port, 4711);
                assert_eq!(program, "/path/to/binary");
            }
            _ => panic!("Expected LaunchProgram mode"),
        }
    }

    #[test]
    fn test_config_launch_with_args() {
        let config = RustAdapter::config_launch_with_args(
            "/path/to/binary",
            vec!["--arg1".to_string(), "--arg2".to_string()],
            4711,
            true,
        );
        assert_eq!(config.adapter_id, "rust");
        match config.connection_mode {
            crate::ConnectionMode::LaunchProgram {
                port,
                program,
                args,
                stop_on_entry,
                ..
            } => {
                assert_eq!(port, 4711);
                assert_eq!(program, "/path/to/binary");
                assert_eq!(args, vec!["--arg1", "--arg2"]);
                assert!(stop_on_entry);
            }
            _ => panic!("Expected LaunchProgram mode"),
        }
    }

    // =========================================================================
    // Direct lldb-dap connection tests
    // =========================================================================

    #[test]
    fn test_config_direct_launch() {
        let config = RustAdapter::config_direct_launch("/path/to/binary", 4711);
        assert_eq!(config.adapter_id, "rust");
        assert_eq!(config.command, "lldb-dap");
        match config.connection_mode {
            crate::ConnectionMode::LaunchProgram {
                port,
                program,
                args,
                stop_on_entry,
                init_commands,
                ..
            } => {
                assert_eq!(port, 4711);
                assert_eq!(program, "/path/to/binary");
                assert!(args.is_empty());
                assert!(!stop_on_entry);
                // Should include type formatters
                assert!(!init_commands.is_empty());
                // Check for String formatter
                assert!(init_commands
                    .iter()
                    .any(|cmd| cmd.contains("alloc::") && cmd.contains("String")));
                // Check for Vec formatter
                assert!(init_commands
                    .iter()
                    .any(|cmd| cmd.contains("Vec") && cmd.contains("len=")));
            }
            _ => panic!("Expected LaunchProgram mode"),
        }
    }

    #[test]
    fn test_config_direct_launch_with_args() {
        let config = RustAdapter::config_direct_launch_with_args(
            "/path/to/binary",
            vec!["--verbose".to_string(), "--port=8080".to_string()],
            4711,
            true,
        );
        assert_eq!(config.adapter_id, "rust");
        assert_eq!(config.command, "lldb-dap");
        match config.connection_mode {
            crate::ConnectionMode::LaunchProgram {
                port,
                program,
                args,
                stop_on_entry,
                init_commands,
                ..
            } => {
                assert_eq!(port, 4711);
                assert_eq!(program, "/path/to/binary");
                assert_eq!(args, vec!["--verbose", "--port=8080"]);
                assert!(stop_on_entry);
                // Should include type formatters
                assert!(!init_commands.is_empty());
            }
            _ => panic!("Expected LaunchProgram mode"),
        }
    }

    #[test]
    fn test_direct_launch_includes_all_formatters() {
        use detrix_config::RUST_LLDB_TYPE_FORMATTERS;

        let config = RustAdapter::config_direct_launch("/path/to/binary", 4711);
        match config.connection_mode {
            crate::ConnectionMode::LaunchProgram { init_commands, .. } => {
                // Should include exactly as many formatters as the constant
                assert_eq!(init_commands.len(), RUST_LLDB_TYPE_FORMATTERS.len());
                // Each formatter should be present
                for formatter in RUST_LLDB_TYPE_FORMATTERS {
                    assert!(
                        init_commands.contains(&formatter.to_string()),
                        "Missing formatter: {}",
                        formatter
                    );
                }
            }
            _ => panic!("Expected LaunchProgram mode"),
        }
    }

    #[test]
    fn test_build_logpoint_message() {
        let metric = sample_metric();
        let message = RustAdapter::build_logpoint_message(&metric);
        assert_eq!(message, "DETRICS:test_metric={user.name}");
    }

    #[test]
    fn test_rust_adapter_creation() {
        let config = RustAdapter::default_config(4711);
        let adapter = RustAdapter::new(config, PathBuf::from("/tmp"));

        assert!(!adapter.is_ready());
    }

    #[test]
    fn test_location_key() {
        let key = RustAdapter::location_key("src/main.rs", 42);
        assert_eq!(key, "src/main.rs#42");
    }

    #[test]
    fn test_needs_continue() {
        // CodeLLDB doesn't need explicit continue
        assert!(!RustOutputParser::needs_continue_after_connect());
    }

    #[tokio::test]
    async fn test_parse_output_valid_string() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "DETRICS:test_metric=hello_world".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.metric_name, "test_metric");
        assert_eq!(event.value_json, "hello_world");
        assert_eq!(event.value_string, Some("hello_world".to_string()));
    }

    #[tokio::test]
    async fn test_parse_output_valid_numeric() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "DETRICS:test_metric=42".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.metric_name, "test_metric");
        assert_eq!(event.value_json, "42");
        assert_eq!(event.value_numeric, Some(42.0));
    }

    #[tokio::test]
    async fn test_parse_output_valid_float() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "DETRICS:test_metric=1.23".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.value_numeric, Some(1.23));
    }

    #[tokio::test]
    async fn test_parse_output_json_value() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: r#"DETRICS:test_metric={"name":"John","age":30}"#.to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert!(event.value_json.contains("John"));
        assert!(event.value_json.contains("30"));
    }

    #[tokio::test]
    async fn test_parse_output_non_detrix() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));

        let body = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "some other output".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        assert!(RustOutputParser::parse_output(&body, &active_metrics)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn test_parse_output_unknown_metric() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));

        let body = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "DETRICS:unknown_metric=42".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        // Should return None because metric isn't tracked
        assert!(RustOutputParser::parse_output(&body, &active_metrics)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn test_parse_output_boolean() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "DETRICS:test_metric=true".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.value_boolean, Some(true));
    }

    // =========================================================================
    // Thread ID extraction tests
    // =========================================================================

    use crate::base::ThreadExtractor;

    #[test]
    fn test_extract_thread_info_direct_detrics() {
        let extractor = RustThreadExtractor;
        let info = extractor.extract_thread_info("DETRICS:test_metric=42");
        assert_eq!(info.thread_id, None);
        assert_eq!(info.cleaned_text, "DETRICS:test_metric=42");
    }

    #[test]
    fn test_extract_thread_info_thread_prefix_colon() {
        let extractor = RustThreadExtractor;
        let info = extractor.extract_thread_info("thread #1: DETRICS:test_metric=42");
        assert_eq!(info.thread_id, Some(1));
        assert_eq!(info.cleaned_text, "DETRICS:test_metric=42");
    }

    #[test]
    fn test_extract_thread_info_thread_prefix_star() {
        let extractor = RustThreadExtractor;
        let info = extractor
            .extract_thread_info("* thread #5, stop reason = breakpoint DETRICS:test_metric=hello");
        assert_eq!(info.thread_id, Some(5));
        assert_eq!(info.cleaned_text, "DETRICS:test_metric=hello");
    }

    #[test]
    fn test_extract_thread_info_bracket_format() {
        let extractor = RustThreadExtractor;
        let info = extractor.extract_thread_info("[thread 3] DETRICS:test_metric=value");
        assert_eq!(info.thread_id, Some(3));
        assert_eq!(info.cleaned_text, "DETRICS:test_metric=value");
    }

    #[test]
    fn test_extract_thread_info_multiline() {
        let output = "* thread #2, name = 'worker', stop reason = breakpoint\n  frame #0: 0x00001234\n  DETRICS:test_metric=42";
        let extractor = RustThreadExtractor;
        let info = extractor.extract_thread_info(output);
        assert_eq!(info.thread_id, Some(2));
        assert_eq!(info.cleaned_text, "DETRICS:test_metric=42");
    }

    #[test]
    fn test_extract_thread_info_no_match() {
        let extractor = RustThreadExtractor;
        let info = extractor.extract_thread_info("some random output");
        assert_eq!(info.thread_id, None);
        assert_eq!(info.cleaned_text, "some random output");
    }

    #[test]
    fn test_extract_thread_info_large_thread_id() {
        let extractor = RustThreadExtractor;
        let info = extractor.extract_thread_info("thread #12345: DETRICS:metric=value");
        assert_eq!(info.thread_id, Some(12345));
        assert_eq!(info.cleaned_text, "DETRICS:metric=value");
    }

    #[tokio::test]
    async fn test_parse_output_with_thread_id() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "thread #1: DETRICS:test_metric=42".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.metric_name, "test_metric");
        assert_eq!(event.thread_id, Some(1));
        assert_eq!(event.thread_name, Some("thread-1".to_string()));
        assert_eq!(event.value_numeric, Some(42.0));
    }

    #[tokio::test]
    async fn test_parse_output_without_thread_id() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Console),
            output: "DETRICS:test_metric=42".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.thread_id, None);
        assert_eq!(event.thread_name, None);
    }

    // =========================================================================
    // Error detection tests
    // =========================================================================
    //
    // Note: Comprehensive error detection tests are in error_detection.rs.
    // These tests verify the Rust adapter integration with error detection.

    use crate::error_detection::detect_error;

    #[test]
    fn test_detect_error_lldb_error() {
        let result = detect_error(
            "error: use of undeclared identifier 'foo'",
            RUST_ERROR_PATTERNS,
        );
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "LldbError");
    }

    #[test]
    fn test_detect_error_expression_error() {
        let result = detect_error(
            "expression error: cannot evaluate this expression",
            RUST_ERROR_PATTERNS,
        );
        assert!(result.is_some());
        let (error_type, message) = result.unwrap();
        assert_eq!(error_type, "ExpressionError");
        assert!(message.contains("cannot evaluate"));
    }

    #[test]
    fn test_detect_error_no_error() {
        let result = detect_error("DETRICS:test_metric=42", RUST_ERROR_PATTERNS);
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_error_normal_output() {
        let result = detect_error("some normal console output", RUST_ERROR_PATTERNS);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_parse_error_output_lldb_error() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Stderr),
            output: "error: use of undeclared identifier 'undefined_var'".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert!(event.is_error);
        assert_eq!(event.error_type, Some("LldbError".to_string()));
        assert!(event
            .error_message
            .as_ref()
            .unwrap()
            .contains("undeclared identifier"));
    }

    #[tokio::test]
    async fn test_parse_error_output_index_error() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Stderr),
            output: "index out of bounds: the len is 5 but the index is 10".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_some());
        let event = result.unwrap();
        assert!(event.is_error);
        assert_eq!(event.error_type, Some("IndexError".to_string()));
    }

    #[tokio::test]
    async fn test_parse_error_output_not_error() {
        let active_metrics = Arc::new(RwLock::new(HashMap::new()));
        let metric = sample_metric();
        active_metrics
            .write()
            .await
            .insert("src/main.rs#42".to_string(), metric);

        let body = OutputEventBody {
            category: Some(OutputCategory::Stdout),
            output: "Hello, world!".to_string(),
            variables_reference: None,
            source: None,
            line: None,
            column: None,
        };

        let result = RustOutputParser::parse_output(&body, &active_metrics).await;
        assert!(result.is_none());
    }
}
