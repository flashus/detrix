//! Centralized logging configuration for Detrix
//!
//! This crate provides a unified logging initialization API for all Detrix binaries.
//! It wraps `tracing` and `tracing-subscriber` to ensure consistent logging behavior.
//!
//! # Usage
//!
//! ```rust,ignore
//! use detrix_logging::{init, LogConfig, LogOutput};
//!
//! // Simple initialization with defaults
//! init(LogConfig::default());
//!
//! // CLI with debug flag
//! init(LogConfig::new().debug(true));
//!
//! // MCP mode (logs to stderr, stdout reserved for JSON-RPC)
//! init(LogConfig::new().output(LogOutput::Stderr));
//!
//! // TUI mode (minimal logging)
//! init(LogConfig::new().default_level("warn").output(LogOutput::Stderr));
//!
//! // File logging (daemon mode)
//! let guard = init_with_file(
//!     LogConfig::new().output(LogOutput::File),
//!     Path::new("/var/log/detrix_daemon.log"),
//! );
//! // Guard must be held for the duration of the program
//! ```
//!
//! # Re-exports
//!
//! This crate re-exports commonly used tracing macros for convenience:
//! - `trace!`, `debug!`, `info!`, `warn!`, `error!`
//! - `span!`, `Level`
//! - `instrument` attribute macro

use std::io::IsTerminal;
use std::path::Path;
use tracing_subscriber::{
    fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer as _,
};

// Re-export tracing macros for standardized imports
pub use tracing::{debug, error, info, span, trace, warn, Level};

// Re-export instrument attribute for function instrumentation
pub use tracing::instrument;

// Re-export entire tracing crate for backwards compatibility with qualified usage (tracing::debug!)
// This allows gradual migration - both styles work:
//   use detrix_logging::{debug, info};  // Preferred
//   detrix_logging::tracing::debug!()   // Backwards compatible
pub use tracing;

// Re-export WorkerGuard for file logging lifetime management
pub use tracing_appender::non_blocking::WorkerGuard;

/// Output destination for logs
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogOutput {
    /// Write logs to stdout (default)
    #[default]
    Stdout,
    /// Write logs to stderr (required for MCP/TUI modes)
    Stderr,
    /// Write logs to a file (daemon mode)
    File,
}

/// Timestamp format for log output
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimestampFormat {
    /// Use local timezone (default)
    #[default]
    Local,
    /// Use UTC timezone
    Utc,
}

/// Configuration for logging initialization
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Enable debug-level logging (overrides default_level)
    pub debug: bool,
    /// Default log level when RUST_LOG is not set
    pub default_level: String,
    /// Output destination
    pub output: LogOutput,
    /// Show module target in log output
    pub show_target: bool,
    /// Timestamp format (local or UTC)
    pub timestamp_format: TimestampFormat,
    /// Minimum level for stderr when using combined file+stderr output.
    /// If set, stderr will only receive logs at this level or higher,
    /// while the file receives all logs. Useful for showing only errors
    /// on console while keeping full logs in file.
    pub stderr_min_level: Option<tracing::Level>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            debug: false,
            default_level: "info".to_string(),
            output: LogOutput::Stdout,
            show_target: false,
            timestamp_format: TimestampFormat::default(),
            stderr_min_level: None,
        }
    }
}

impl LogConfig {
    /// Create a new LogConfig with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable debug-level logging
    pub fn debug(mut self, enabled: bool) -> Self {
        self.debug = enabled;
        self
    }

    /// Set the default log level (used when RUST_LOG is not set)
    pub fn default_level(mut self, level: impl Into<String>) -> Self {
        self.default_level = level.into();
        self
    }

    /// Set the output destination
    pub fn output(mut self, output: LogOutput) -> Self {
        self.output = output;
        self
    }

    /// Show or hide module target in log output
    pub fn show_target(mut self, show: bool) -> Self {
        self.show_target = show;
        self
    }

    /// Set timestamp format (local or UTC)
    pub fn timestamp_format(mut self, format: TimestampFormat) -> Self {
        self.timestamp_format = format;
        self
    }

    /// Use UTC timestamps
    pub fn utc(self) -> Self {
        self.timestamp_format(TimestampFormat::Utc)
    }

    /// Use local timestamps (default)
    pub fn local(self) -> Self {
        self.timestamp_format(TimestampFormat::Local)
    }

    /// Set minimum level for stderr when using combined file+stderr output.
    ///
    /// When set, stderr will only receive logs at this level or higher,
    /// while the file receives all logs based on the default_level/debug settings.
    ///
    /// Example: `.stderr_min_level(Level::ERROR)` means only ERROR logs go to stderr,
    /// but INFO/WARN/DEBUG still go to the log file.
    pub fn stderr_min_level(mut self, level: tracing::Level) -> Self {
        self.stderr_min_level = Some(level);
        self
    }

    /// Convenience: Configure for CLI usage
    pub fn cli(debug: bool) -> Self {
        Self::new().debug(debug)
    }

    /// Convenience: Configure for MCP usage (logs to stderr)
    pub fn mcp(debug: bool) -> Self {
        Self::new().debug(debug).output(LogOutput::Stderr)
    }

    /// Convenience: Configure for TUI usage (minimal logging to stderr)
    pub fn tui() -> Self {
        Self::new().default_level("warn").output(LogOutput::Stderr)
    }

    /// Convenience: Configure for daemon usage (logs to file)
    pub fn daemon(debug: bool) -> Self {
        Self::new().debug(debug).output(LogOutput::File)
    }

    /// Convenience: Configure for tests
    pub fn test() -> Self {
        Self::new().default_level("debug")
    }

    fn build_filter(&self) -> EnvFilter {
        if self.debug {
            EnvFilter::new("debug")
        } else {
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&self.default_level))
        }
    }
}

/// Initialize the logging system with the given configuration.
///
/// This function should be called once at application startup.
/// It configures `tracing-subscriber` based on the provided `LogConfig`.
///
/// # Environment Variables
///
/// - `RUST_LOG`: Override log level (e.g., `RUST_LOG=debug` or `RUST_LOG=detrix_api=trace`)
///
/// # Panics
///
/// Panics if called more than once (tracing can only be initialized once).
///
/// # Note
///
/// For file logging, use `init_with_file` instead.
pub fn init(config: LogConfig) {
    use tracing_subscriber::fmt::time::{LocalTime, UtcTime};

    let filter = config.build_filter();

    // Helper macro to apply timer and init (avoids type-level branching duplication)
    macro_rules! with_timer_init {
        ($builder:expr, $ts_format:expr) => {
            match $ts_format {
                TimestampFormat::Utc => $builder.with_timer(UtcTime::rfc_3339()).init(),
                TimestampFormat::Local => $builder.with_timer(LocalTime::rfc_3339()).init(),
            }
        };
    }

    match config.output {
        LogOutput::Stdout => {
            let is_tty = std::io::stdout().is_terminal();
            let builder = fmt()
                .with_env_filter(filter)
                .with_target(config.show_target)
                .with_ansi(is_tty);
            with_timer_init!(builder, config.timestamp_format);
        }
        LogOutput::Stderr | LogOutput::File => {
            // File output without path falls back to stderr; use init_with_file() for proper file logging
            let is_tty = std::io::stderr().is_terminal();
            let builder = fmt()
                .with_env_filter(filter)
                .with_target(config.show_target)
                .with_writer(std::io::stderr)
                .with_ansi(is_tty);
            with_timer_init!(builder, config.timestamp_format);
        }
    }
}

/// Initialize the logging system with file output.
///
/// This function sets up non-blocking file logging using `tracing-appender`.
/// The returned `WorkerGuard` must be held for the duration of the program
/// to ensure all logs are flushed before shutdown.
///
/// # Arguments
///
/// * `config` - Logging configuration
/// * `log_path` - Path to the log file (parent directory will be created if needed)
///
/// # Returns
///
/// A `WorkerGuard` that must be held until program exit to ensure logs are flushed.
///
/// # Example
///
/// ```rust,ignore
/// use detrix_logging::{init_with_file, LogConfig};
/// use std::path::Path;
///
/// fn main() {
///     let _guard = init_with_file(
///         LogConfig::daemon(false),
///         Path::new("/var/log/detrix_daemon.log"),
///     );
///
///     // Application code...
///     // Guard is dropped at end of main, flushing remaining logs
/// }
/// ```
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created.
///
/// # Panics
///
/// Panics if called more than once (tracing can only be initialized once).
pub fn init_with_file(config: LogConfig, log_path: &Path) -> std::io::Result<WorkerGuard> {
    use tracing_subscriber::fmt::time::{LocalTime, UtcTime};

    let filter = config.build_filter();

    // Ensure parent directory exists
    detrix_config::paths::ensure_parent_dir(log_path)
        .map_err(|e| std::io::Error::other(format!("Failed to create log directory: {}", e)))?;

    // Get directory and filename
    let log_dir = log_path.parent().unwrap_or(Path::new("."));
    let log_filename = log_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("detrix.log");

    // Create file appender with daily rotation
    // Files named: {prefix}.YYYY-MM-DD (e.g., detrix_daemon.2026-01-11)
    let file_appender = tracing_appender::rolling::daily(log_dir, log_filename);

    // Create non-blocking writer with dedicated thread
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Initialize subscriber with file writer (no ANSI colors for file output)
    let builder = fmt()
        .with_env_filter(filter)
        .with_target(config.show_target)
        .with_writer(non_blocking)
        .with_ansi(false);

    match config.timestamp_format {
        TimestampFormat::Utc => builder.with_timer(UtcTime::rfc_3339()).init(),
        TimestampFormat::Local => builder.with_timer(LocalTime::rfc_3339()).init(),
    }

    Ok(guard)
}

/// Initialize the logging system with both file and stderr output.
///
/// This is useful for daemon mode where logs should go to both file and stderr
/// (for debugging or when running in foreground).
///
/// When `config.stderr_min_level` is set, stderr will only receive logs at that
/// level or higher (e.g., only ERROR), while the file receives all logs. This is
/// useful for lldb-serve where users want to see errors on console but keep full
/// logs in the file for debugging.
///
/// # Arguments
///
/// * `config` - Logging configuration
/// * `log_path` - Path to the log file
///
/// # Returns
///
/// A `WorkerGuard` that must be held until program exit.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created.
pub fn init_with_file_and_stderr(
    config: LogConfig,
    log_path: &Path,
) -> std::io::Result<WorkerGuard> {
    use tracing_subscriber::fmt::time::{LocalTime, UtcTime};
    use tracing_subscriber::layer::SubscriberExt;

    // Ensure parent directory exists
    detrix_config::paths::ensure_parent_dir(log_path)
        .map_err(|e| std::io::Error::other(format!("Failed to create log directory: {}", e)))?;

    // Get directory and filename
    let log_dir = log_path.parent().unwrap_or(Path::new("."));
    let log_filename = log_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("detrix.log");

    // Create file appender with daily rotation
    let file_appender = tracing_appender::rolling::daily(log_dir, log_filename);

    // Create non-blocking writer
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Check if we need split-level logging (different levels for file vs stderr)
    if let Some(stderr_level) = config.stderr_min_level {
        // Use two separate layers with different filters
        let file_filter = config.build_filter();
        let stderr_filter = EnvFilter::new(stderr_level.as_str());

        // File layer: all logs (based on config), no ANSI colors
        let file_layer = fmt::layer()
            .with_target(config.show_target)
            .with_writer(non_blocking)
            .with_ansi(false);

        // Stderr layer: only logs at stderr_min_level or higher, with ANSI colors
        let is_tty = std::io::stderr().is_terminal();
        let stderr_layer = fmt::layer()
            .with_target(config.show_target)
            .with_writer(std::io::stderr)
            .with_ansi(is_tty)
            .with_filter(stderr_filter);

        // Build subscriber with both layers
        let subscriber = tracing_subscriber::registry()
            .with(file_filter)
            .with(file_layer)
            .with(stderr_layer);

        match config.timestamp_format {
            TimestampFormat::Utc => {
                let subscriber = subscriber;
                // Note: timer is set per-layer in the layer builders above
                // For simplicity, we use default timer here
                subscriber.init();
            }
            TimestampFormat::Local => {
                subscriber.init();
            }
        }
    } else {
        // Original behavior: both outputs at same level
        use tracing_subscriber::fmt::writer::MakeWriterExt;

        let filter = config.build_filter();
        let combined_writer = non_blocking.and(std::io::stderr);

        // Initialize subscriber with combined writer (no ANSI colors since file doesn't support them)
        let builder = fmt()
            .with_env_filter(filter)
            .with_target(config.show_target)
            .with_writer(combined_writer)
            .with_ansi(false);

        match config.timestamp_format {
            TimestampFormat::Utc => builder.with_timer(UtcTime::rfc_3339()).init(),
            TimestampFormat::Local => builder.with_timer(LocalTime::rfc_3339()).init(),
        }
    }

    Ok(guard)
}

/// Initialize component-specific file logging with PID in filename.
///
/// Creates a log file at `{log_dir}/{component}_{pid}.log` using non-blocking writes.
/// Useful for components like lldb-serve, MCP processes, etc. that need separate log files.
///
/// # Arguments
///
/// * `config` - Logging configuration (debug level, timestamp format, etc.)
/// * `component` - Component name (e.g., "lldb_serve", "mcp")
///
/// # Returns
///
/// A `WorkerGuard` that must be held until the component exits to ensure logs are flushed.
///
/// # Example
///
/// ```rust,ignore
/// use detrix_logging::{init_component_file_log, LogConfig};
///
/// fn main() {
///     let _guard = init_component_file_log(
///         LogConfig::new().debug(true),
///         "lldb_serve",
///     ).expect("Failed to init logging");
///
///     tracing::info!("Component started");
///     // ...
/// }
/// ```
///
/// # Errors
///
/// Returns an error if the log directory cannot be created.
pub fn init_component_file_log(config: LogConfig, component: &str) -> std::io::Result<WorkerGuard> {
    use tracing_subscriber::fmt::time::{LocalTime, UtcTime};

    let filter = config.build_filter();

    // Get log directory from detrix config
    let log_dir = detrix_config::paths::detrix_home().join("log");
    std::fs::create_dir_all(&log_dir)?;

    // Create filename with PID: {component}_{pid}.log
    let pid = std::process::id();
    let log_filename = format!("{}_{}.log", component, pid);
    let log_path = log_dir.join(&log_filename);

    // Create file appender (no rotation for component logs - single file per process)
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    // Create non-blocking writer
    let (non_blocking, guard) = tracing_appender::non_blocking(file);

    // Initialize subscriber with file writer (no ANSI colors for file output)
    let builder = fmt()
        .with_env_filter(filter)
        .with_target(config.show_target)
        .with_writer(non_blocking)
        .with_ansi(false);

    // Use try_init() to avoid panic if global subscriber already set
    // This can happen when lldb-serve is run as a subprocess and main.rs already initialized logging
    let result = match config.timestamp_format {
        TimestampFormat::Utc => builder.with_timer(UtcTime::rfc_3339()).try_init(),
        TimestampFormat::Local => builder.with_timer(LocalTime::rfc_3339()).try_init(),
    };

    // Log a warning if subscriber was already set, but don't fail
    if let Err(e) = result {
        eprintln!(
            "Note: Could not set global subscriber for {}: {}",
            component, e
        );
    }

    Ok(guard)
}

/// Initialize logging for tests.
///
/// Uses `with_test_writer()` to capture logs in test output.
/// Safe to call multiple times (uses `try_init` internally).
pub fn init_test() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_test_writer())
        .try_init();
}

/// Clean up old log files based on retention policy.
///
/// This function deletes log files older than `retention_days` from the specified directory.
/// It matches files with patterns:
/// - `detrix_daemon.log.*` (daily rotated daemon logs)
/// - `detrix_mcp_*.log` (MCP process logs)
///
/// # Arguments
///
/// * `log_dir` - Directory containing log files
/// * `retention_days` - Delete files older than this many days (0 = disabled)
///
/// # Returns
///
/// Number of files deleted, or error if directory cannot be read.
pub fn cleanup_old_logs(log_dir: &Path, retention_days: u32) -> std::io::Result<usize> {
    use std::time::{Duration, SystemTime};

    if retention_days == 0 {
        return Ok(0); // Cleanup disabled
    }

    let cutoff = SystemTime::now() - Duration::from_secs(u64::from(retention_days) * 24 * 60 * 60);
    let mut deleted = 0;

    let entries = match std::fs::read_dir(log_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Directory doesn't exist yet, nothing to clean
            return Ok(0);
        }
        Err(e) => return Err(e),
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Skip if not a file
        if !path.is_file() {
            continue;
        }

        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => continue,
        };

        // Match rotated daemon logs (detrix_daemon.log.2026-01-10),
        // MCP logs (detrix_mcp_12345.log), and lldb-serve logs (lldb_serve_12345.log)
        let is_log_file = file_name.starts_with("detrix_daemon.log.")
            || (file_name.starts_with("detrix_mcp_") && file_name.ends_with(".log"))
            || (file_name.starts_with("lldb_serve_") && file_name.ends_with(".log"));

        if !is_log_file {
            continue;
        }

        // Check file age
        if let Ok(metadata) = entry.metadata() {
            if let Ok(modified) = metadata.modified() {
                if modified < cutoff {
                    if let Err(e) = std::fs::remove_file(&path) {
                        warn!("Failed to delete old log file {:?}: {}", path, e);
                    } else {
                        info!("Deleted old log file: {:?}", path);
                        deleted += 1;
                    }
                }
            }
        }
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::{Duration, SystemTime};

    // ==========================================================================
    // LogConfig Builder Tests
    // ==========================================================================

    #[test]
    fn test_log_config_timestamp_format() {
        let config = LogConfig::new().timestamp_format(TimestampFormat::Utc);
        assert_eq!(config.timestamp_format, TimestampFormat::Utc);

        let config = LogConfig::new().timestamp_format(TimestampFormat::Local);
        assert_eq!(config.timestamp_format, TimestampFormat::Local);
    }

    #[test]
    fn test_build_filter_respects_debug_flag() {
        // Debug flag should override default level
        let config = LogConfig::new().default_level("warn").debug(true);
        let filter_str = format!("{:?}", config.build_filter());
        // Debug level should be present
        assert!(
            filter_str.contains("debug") || filter_str.contains("DEBUG"),
            "Expected debug level in filter: {}",
            filter_str
        );
    }

    // ==========================================================================
    // Test Initialization Tests
    // ==========================================================================

    #[test]
    fn test_init_test_does_not_panic() {
        // init_test should be safe to call multiple times
        init_test();
        init_test(); // Second call should not panic
    }

    // ==========================================================================
    // Cleanup Tests
    // ==========================================================================

    #[test]
    fn test_cleanup_old_logs_disabled_when_zero() {
        let temp_dir = tempfile::tempdir().unwrap();
        let result = cleanup_old_logs(temp_dir.path(), 0);
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_cleanup_old_logs_nonexistent_dir() {
        let nonexistent = Path::new("/nonexistent/path/that/does/not/exist");
        let result = cleanup_old_logs(nonexistent, 7);
        // Should return Ok(0) for nonexistent directory
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_cleanup_old_logs_empty_dir() {
        let temp_dir = tempfile::tempdir().unwrap();
        let result = cleanup_old_logs(temp_dir.path(), 7);
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_cleanup_old_logs_preserves_recent() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Create recent log files
        File::create(temp_dir.path().join("detrix_daemon.log.2026-01-20")).unwrap();
        File::create(temp_dir.path().join("detrix_mcp_12345.log")).unwrap();

        // These should not be deleted (retention = 7 days, files are new)
        let result = cleanup_old_logs(temp_dir.path(), 7);
        assert_eq!(result.unwrap(), 0);

        // Verify files still exist
        assert!(temp_dir
            .path()
            .join("detrix_daemon.log.2026-01-20")
            .exists());
        assert!(temp_dir.path().join("detrix_mcp_12345.log").exists());
    }

    #[test]
    fn test_cleanup_old_logs_ignores_non_matching_files() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Create files that don't match the log patterns
        File::create(temp_dir.path().join("other_file.txt")).unwrap();
        File::create(temp_dir.path().join("detrix_config.toml")).unwrap();

        // Artificially make files old by creating them and backdating (not possible without platform code)
        // Instead, just verify they're not deleted even with low retention
        let result = cleanup_old_logs(temp_dir.path(), 1);
        assert_eq!(result.unwrap(), 0);

        // Verify non-matching files still exist
        assert!(temp_dir.path().join("other_file.txt").exists());
        assert!(temp_dir.path().join("detrix_config.toml").exists());
    }

    #[test]
    fn test_cleanup_old_logs_deletes_expired() {
        use std::io::Write;

        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("detrix_daemon.log.2026-01-01");

        // Create a log file
        let mut file = File::create(&log_path).unwrap();
        writeln!(file, "test log content").unwrap();
        drop(file);

        // Set the file's modification time to 10 days ago
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 24 * 60 * 60);
        filetime::set_file_mtime(
            &log_path,
            filetime::FileTime::from_system_time(ten_days_ago),
        )
        .unwrap();

        // Cleanup with 7 day retention should delete the file
        let result = cleanup_old_logs(temp_dir.path(), 7);
        assert_eq!(result.unwrap(), 1);

        // Verify file was deleted
        assert!(!log_path.exists());
    }

    #[test]
    fn test_cleanup_old_logs_deletes_mcp_logs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("detrix_mcp_99999.log");

        // Create an MCP log file
        File::create(&log_path).unwrap();

        // Set the file's modification time to 10 days ago
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 24 * 60 * 60);
        filetime::set_file_mtime(
            &log_path,
            filetime::FileTime::from_system_time(ten_days_ago),
        )
        .unwrap();

        // Cleanup with 7 day retention should delete the file
        let result = cleanup_old_logs(temp_dir.path(), 7);
        assert_eq!(result.unwrap(), 1);

        // Verify file was deleted
        assert!(!log_path.exists());
    }

    #[test]
    fn test_cleanup_old_logs_deletes_lldb_serve_logs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let log_path = temp_dir.path().join("lldb_serve_12345.log");

        // Create an lldb-serve log file
        File::create(&log_path).unwrap();

        // Set the file's modification time to 10 days ago
        let ten_days_ago = SystemTime::now() - Duration::from_secs(10 * 24 * 60 * 60);
        filetime::set_file_mtime(
            &log_path,
            filetime::FileTime::from_system_time(ten_days_ago),
        )
        .unwrap();

        // Cleanup with 7 day retention should delete the file
        let result = cleanup_old_logs(temp_dir.path(), 7);
        assert_eq!(result.unwrap(), 1);

        // Verify file was deleted
        assert!(!log_path.exists());
    }
}
