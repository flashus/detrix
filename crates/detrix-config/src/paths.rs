//! Path utilities for Detrix configuration
//!
//! Provides home directory expansion and path resolution utilities.
//! Works cross-platform (Unix: ~/detrix, Windows: %USERPROFILE%\detrix).

use std::path::{Path, PathBuf};

use crate::constants::ENV_DETRIX_CONFIG;

/// Default detrix data directory name
pub const DETRIX_DIR_NAME: &str = "detrix";

/// Default log subdirectory name
pub const LOG_DIR_NAME: &str = "log";

/// Default database filename
pub const DEFAULT_DB_FILENAME: &str = "data.db";

/// Default PID filename
pub const DEFAULT_PID_FILENAME: &str = "daemon.pid";

/// Default MCP token filename
pub const DEFAULT_MCP_TOKEN_FILENAME: &str = "mcp-token";

/// Default daemon log filename (for tracing logs with daily rotation)
pub const DEFAULT_DAEMON_LOG_FILENAME: &str = "detrix_daemon.log";

/// Default daemon startup log filename (captures stdout/stderr of daemon process)
pub const DEFAULT_DAEMON_STARTUP_LOG_FILENAME: &str = "detrix_daemon_startup.log";

/// Default MCP log filename prefix (PID will be appended)
pub const DEFAULT_MCP_LOG_PREFIX: &str = "detrix_mcp";

/// Default lldb-serve log filename prefix (PID will be appended)
pub const DEFAULT_LLDB_SERVE_LOG_PREFIX: &str = "lldb_serve";

/// Default config filename
pub const DEFAULT_CONFIG_FILENAME: &str = "detrix.toml";

/// Get user's detrix home directory.
///
/// Returns `~/detrix` on Unix or `%USERPROFILE%\detrix` on Windows.
/// Falls back to current directory if home cannot be determined.
///
/// # Example
/// ```
/// use detrix_config::paths::detrix_home;
/// let home = detrix_home();
/// // On Unix: /home/user/detrix
/// // On Windows: C:\Users\user\detrix
/// ```
pub fn detrix_home() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DETRIX_DIR_NAME)
}

/// Get default database path.
///
/// Returns `~/detrix/data.db`.
pub fn default_database_path() -> PathBuf {
    detrix_home().join(DEFAULT_DB_FILENAME)
}

/// Get default PID file path.
///
/// Returns `~/detrix/daemon.pid`.
pub fn default_pid_path() -> PathBuf {
    detrix_home().join(DEFAULT_PID_FILENAME)
}

/// Get MCP auth token file path.
///
/// Returns `~/detrix/mcp-token`.
/// This file stores the auto-generated bearer token for local MCP usage.
/// The token is ephemeral and regenerated on each daemon restart.
pub fn mcp_token_path() -> PathBuf {
    detrix_home().join(DEFAULT_MCP_TOKEN_FILENAME)
}

/// Get default log directory.
///
/// Returns `~/detrix/log/`.
pub fn default_log_dir() -> PathBuf {
    detrix_home().join(LOG_DIR_NAME)
}

/// Get default daemon log file path (for tracing logs with daily rotation).
///
/// Returns `~/detrix/log/detrix_daemon.log`.
pub fn default_daemon_log_path() -> PathBuf {
    default_log_dir().join(DEFAULT_DAEMON_LOG_FILENAME)
}

/// Get default daemon startup log file path (captures stdout/stderr of daemon process).
///
/// Returns `~/detrix/log/detrix_daemon_startup.log`.
pub fn default_daemon_startup_log_path() -> PathBuf {
    default_log_dir().join(DEFAULT_DAEMON_STARTUP_LOG_FILENAME)
}

/// Get MCP log file path for a specific process.
///
/// Returns `~/detrix/log/detrix_mcp_{pid}.log`.
///
/// # Arguments
/// * `pid` - Process ID to include in the filename
pub fn mcp_log_path(pid: u32) -> PathBuf {
    default_log_dir().join(format!("{}_{}.log", DEFAULT_MCP_LOG_PREFIX, pid))
}

/// Get MCP log file path for the current process.
///
/// Returns `~/detrix/log/detrix_mcp_{current_pid}.log`.
pub fn default_mcp_log_path() -> PathBuf {
    mcp_log_path(std::process::id())
}

/// Get lldb-serve log file path for a specific process.
///
/// Returns `~/detrix/log/lldb_serve_{pid}.log`.
///
/// # Arguments
/// * `pid` - Process ID to include in the filename
pub fn lldb_serve_log_path(pid: u32) -> PathBuf {
    default_log_dir().join(format!("{}_{}.log", DEFAULT_LLDB_SERVE_LOG_PREFIX, pid))
}

/// Get lldb-serve log file path for the current process.
///
/// Returns `~/detrix/log/lldb_serve_{current_pid}.log`.
pub fn default_lldb_serve_log_path() -> PathBuf {
    lldb_serve_log_path(std::process::id())
}

/// Get default configuration file path.
///
/// Returns `~/detrix/detrix.toml`.
pub fn default_config_path() -> PathBuf {
    detrix_home().join(DEFAULT_CONFIG_FILENAME)
}

/// Discover configuration file path from multiple sources.
///
/// Resolution order:
/// 1. `--config <path>` or `-c <path>` CLI argument
/// 2. `DETRIX_CONFIG` environment variable (if set and non-empty)
/// 3. Default config path (`~/detrix/detrix.toml`)
///
/// Returns a tuple of (path, source) where source describes where the path came from.
///
/// # Example
/// ```
/// use detrix_config::paths::discover_config_path;
///
/// let (path, source) = discover_config_path();
/// println!("Using config from {}: {}", source, path.display());
/// ```
pub fn discover_config_path() -> (PathBuf, &'static str) {
    // Check CLI arguments for --config or -c
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        if (args[i] == "--config" || args[i] == "-c") && i + 1 < args.len() {
            let path = expand_tilde(Path::new(&args[i + 1]));
            return (path, "--config CLI arg");
        }
        // Also handle --config=path format
        if let Some(path_str) = args[i].strip_prefix("--config=") {
            let path = expand_tilde(Path::new(path_str));
            return (path, "--config CLI arg");
        }
        if let Some(path_str) = args[i].strip_prefix("-c=") {
            let path = expand_tilde(Path::new(path_str));
            return (path, "-c CLI arg");
        }
    }

    // Check DETRIX_CONFIG environment variable
    if let Ok(config_path) = std::env::var(ENV_DETRIX_CONFIG) {
        if !config_path.is_empty() {
            let path = expand_tilde(Path::new(&config_path));
            return (path, "DETRIX_CONFIG env var");
        }
    }

    // Fall back to default config path
    (default_config_path(), "default location")
}

/// Expand tilde (~) in path to user's home directory.
///
/// - `~/foo` becomes `/home/user/foo` on Unix
/// - `~/foo` becomes `C:\Users\user\foo` on Windows
/// - Paths without tilde are returned unchanged
///
/// # Example
/// ```
/// use detrix_config::paths::expand_tilde;
/// use std::path::Path;
///
/// let expanded = expand_tilde(Path::new("~/detrix/data.db"));
/// // Returns /home/user/detrix/data.db on Unix
/// ```
pub fn expand_tilde(path: &Path) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("~") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(stripped)
    } else {
        path.to_path_buf()
    }
}

/// Ensure parent directory of a path exists.
///
/// Creates the parent directory and all intermediate directories if they don't exist.
/// Does nothing if the path has no parent or parent already exists.
///
/// # Errors
/// Returns an error if directory creation fails (e.g., permission denied).
///
/// # Example
/// ```ignore
/// use detrix_config::paths::ensure_parent_dir;
/// use std::path::Path;
///
/// // Creates ~/detrix/ directory if it doesn't exist
/// ensure_parent_dir(Path::new("/home/user/detrix/data.db"))?;
/// ```
pub fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Resolve a path relative to a base directory.
///
/// - Absolute paths are returned unchanged
/// - Paths starting with `~` are expanded to home directory
/// - Relative paths are joined with the base directory
///
/// # Example
/// ```
/// use detrix_config::paths::resolve_path;
/// use std::path::Path;
///
/// let base = Path::new("/etc/detrix");
///
/// // Absolute path unchanged
/// assert_eq!(resolve_path(base, Path::new("/var/data.db")), Path::new("/var/data.db"));
///
/// // Relative path joined with base
/// assert_eq!(resolve_path(base, Path::new("data.db")), Path::new("/etc/detrix/data.db"));
/// ```
pub fn resolve_path(base_dir: &Path, path: &Path) -> PathBuf {
    // First expand tilde if present
    let expanded = expand_tilde(path);

    if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detrix_home_not_empty() {
        let home = detrix_home();
        assert!(!home.as_os_str().is_empty());
        assert!(home.ends_with(DETRIX_DIR_NAME));
    }

    #[test]
    fn test_default_database_path() {
        let db_path = default_database_path();
        assert!(db_path.ends_with(DEFAULT_DB_FILENAME));
        assert!(db_path.to_string_lossy().contains(DETRIX_DIR_NAME));
    }

    #[test]
    fn test_default_pid_path() {
        let pid_path = default_pid_path();
        assert!(pid_path.ends_with(DEFAULT_PID_FILENAME));
        assert!(pid_path.to_string_lossy().contains(DETRIX_DIR_NAME));
    }

    #[test]
    fn test_mcp_token_path() {
        let token_path = mcp_token_path();
        assert!(token_path.ends_with(DEFAULT_MCP_TOKEN_FILENAME));
        assert!(token_path.to_string_lossy().contains(DETRIX_DIR_NAME));
    }

    #[test]
    fn test_default_log_dir() {
        let log_dir = default_log_dir();
        assert!(log_dir.ends_with(LOG_DIR_NAME));
        assert!(log_dir.to_string_lossy().contains(DETRIX_DIR_NAME));
    }

    #[test]
    fn test_default_daemon_log_path() {
        let log_path = default_daemon_log_path();
        assert!(log_path.ends_with(DEFAULT_DAEMON_LOG_FILENAME));
        assert!(log_path.to_string_lossy().contains(LOG_DIR_NAME));
    }

    #[test]
    fn test_mcp_log_path() {
        let log_path = mcp_log_path(12345);
        assert!(log_path.to_string_lossy().contains("detrix_mcp_12345.log"));
        assert!(log_path.to_string_lossy().contains(LOG_DIR_NAME));
    }

    #[test]
    fn test_default_mcp_log_path() {
        let log_path = default_mcp_log_path();
        let pid = std::process::id();
        assert!(log_path
            .to_string_lossy()
            .contains(&format!("detrix_mcp_{}.log", pid)));
    }

    #[test]
    fn test_expand_tilde() {
        // Path without tilde should be unchanged
        let no_tilde = Path::new("/absolute/path");
        assert_eq!(expand_tilde(no_tilde), no_tilde);

        // Relative path without tilde should be unchanged
        let relative = Path::new("relative/path");
        assert_eq!(expand_tilde(relative), relative);

        // Path with tilde should expand
        let with_tilde = Path::new("~/foo/bar");
        let expanded = expand_tilde(with_tilde);
        assert!(expanded.is_absolute() || expanded.starts_with(".")); // fallback case
        assert!(expanded.ends_with("foo/bar") || expanded.ends_with("foo\\bar"));
    }

    #[test]
    fn test_resolve_path_absolute() {
        let base = Path::new("/base/dir");
        let absolute = Path::new("/absolute/path");
        assert_eq!(resolve_path(base, absolute), absolute);
    }

    #[test]
    fn test_resolve_path_relative() {
        let base = Path::new("/base/dir");
        let relative = Path::new("relative/path");
        assert_eq!(
            resolve_path(base, relative),
            PathBuf::from("/base/dir/relative/path")
        );
    }

    #[test]
    fn test_ensure_parent_dir_creates_directory() {
        let temp_dir = std::env::temp_dir();
        let test_dir = temp_dir.join("detrix_test_ensure_parent");
        let nested_path = test_dir.join("a/b/c/file.txt");

        // Clean up if exists from previous run
        let _ = std::fs::remove_dir_all(&test_dir);

        // Should create parent directories
        ensure_parent_dir(&nested_path).unwrap();
        assert!(nested_path.parent().unwrap().exists());

        // Cleanup
        let _ = std::fs::remove_dir_all(&test_dir);
    }
}
