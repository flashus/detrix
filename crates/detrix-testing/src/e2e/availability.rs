//! Tool availability checks for E2E tests
//!
//! This module provides functions to check if external tools required for tests
//! are available on the system. Tests use these to skip gracefully when tools
//! are not installed.
//!
//! # Features
//!
//! - Availability check functions for debugpy, rust-analyzer, gopls, etc.
//! - Tracking of missing dependencies across test runs
//! - Big warning at end of test suite for missing tools
//!
//! # Usage
//!
//! ```ignore
//! use detrix_testing::e2e::availability::{require_tool, ToolDependency};
//!
//! #[tokio::test]
//! async fn test_python_debugging() {
//!     if !require_tool(ToolDependency::Debugpy).await {
//!         return; // Warning already printed, dependency tracked
//!     }
//!     // ... test code
//! }
//! ```

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use tokio::process::Command;

// ============================================================================
// Dependency Tracking
// ============================================================================

/// Global tracker for missing dependencies
static MISSING_DEPS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();

fn get_missing_deps() -> &'static Mutex<HashSet<&'static str>> {
    MISSING_DEPS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Record a missing dependency
pub fn record_missing(name: &'static str) {
    if let Ok(mut deps) = get_missing_deps().lock() {
        deps.insert(name);
    }
}

/// Get list of missing dependencies (for testing/reporting)
pub fn get_missing_dependencies() -> Vec<&'static str> {
    get_missing_deps()
        .lock()
        .map(|deps| deps.iter().copied().collect())
        .unwrap_or_default()
}

/// Print warning banner for missing dependencies
///
/// Call this at the end of test runs to show a summary of missing tools.
pub fn print_missing_deps_warning() {
    let missing = get_missing_dependencies();
    if missing.is_empty() {
        return;
    }

    eprintln!();
    eprintln!("\x1b[1;31m═══════════════════════════════════════════════════════════════\x1b[0m");
    eprintln!(
        "\x1b[1;31m  WARNING: {} TEST DEPENDENCIES MISSING\x1b[0m",
        missing.len()
    );
    eprintln!("\x1b[1;31m═══════════════════════════════════════════════════════════════\x1b[0m");
    eprintln!();
    for dep in &missing {
        let hint = get_install_hint(dep);
        eprintln!("  \x1b[33m• {}\x1b[0m", dep);
        eprintln!("    Install: {}", hint);
    }
    eprintln!();
    eprintln!("\x1b[33mSome tests were SKIPPED. Install missing tools for full coverage.\x1b[0m");
    eprintln!();
}

fn get_install_hint(name: &str) -> &'static str {
    match name {
        "debugpy" => "pip install debugpy",
        "rust-analyzer" => "rustup component add rust-analyzer",
        "gopls" => "go install golang.org/x/tools/gopls@latest",
        "delve" => "go install github.com/go-delve/delve/cmd/dlv@latest",
        "lldb-dap" => "brew install llvm (macOS) or apt install lldb (Linux)",
        "codelldb" => "Install CodeLLDB VSCode extension or run with Detrix auto-download",
        "pyright" => "npm install -g pyright",
        "pylsp" => "pip install python-lsp-server",
        "go" => "https://go.dev/doc/install",
        "rustc" => "https://rustup.rs",
        _ => "(see documentation)",
    }
}

// ============================================================================
// Tool Dependency Enum
// ============================================================================

/// Predefined tool dependencies with metadata
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolDependency {
    Debugpy,
    RustAnalyzer,
    Gopls,
    Delve,
    LldbDap,
    /// CodeLLDB (vadimcn's adapter) - has better PDB support on Windows
    CodeLldb,
    Pyright,
    Pylsp,
    Go,
    Rustc,
}

impl ToolDependency {
    /// Get the display name for this tool
    pub fn name(&self) -> &'static str {
        match self {
            Self::Debugpy => "debugpy",
            Self::RustAnalyzer => "rust-analyzer",
            Self::Gopls => "gopls",
            Self::Delve => "delve",
            Self::LldbDap => "lldb-dap",
            Self::CodeLldb => "codelldb",
            Self::Pyright => "pyright",
            Self::Pylsp => "pylsp",
            Self::Go => "go",
            Self::Rustc => "rustc",
        }
    }

    /// Check if this tool is available
    pub async fn is_available(&self) -> bool {
        match self {
            Self::Debugpy => is_debugpy_available().await,
            Self::RustAnalyzer => is_rust_analyzer_available().await,
            Self::Gopls => is_gopls_available().await,
            Self::Delve => is_delve_available().await,
            Self::LldbDap => is_lldb_available().await,
            Self::CodeLldb => is_codelldb_available().await,
            Self::Pyright => is_pyright_available().await,
            Self::Pylsp => is_pylsp_available().await,
            Self::Go => is_go_available().await,
            Self::Rustc => is_rustc_available().await,
        }
    }
}

/// Require a tool for test execution
///
/// Returns true if the tool is available, false if not.
/// When unavailable:
/// - Prints a warning message
/// - Records the dependency as missing for end-of-suite summary
///
/// # Usage
///
/// ```ignore
/// if !require_tool(ToolDependency::Debugpy).await {
///     return; // Test skipped
/// }
/// ```
pub async fn require_tool(tool: ToolDependency) -> bool {
    if tool.is_available().await {
        return true;
    }

    let name = tool.name();
    record_missing(name);

    eprintln!("\n\x1b[33m⚠ SKIPPING TEST: {} not available\x1b[0m", name);
    eprintln!("  Install with: {}\n", get_install_hint(name));

    false
}

// ============================================================================
// Individual Availability Checks
// ============================================================================

/// Check if debugpy (Python debugger) is available
pub async fn is_debugpy_available() -> bool {
    let result = Command::new("python3")
        .args(["-c", "import debugpy; print(debugpy.__version__)"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    matches!(result, Ok(status) if status.success())
}

/// Check if rust-analyzer (Rust LSP) is available
pub async fn is_rust_analyzer_available() -> bool {
    let result = Command::new("rust-analyzer")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    matches!(result, Ok(status) if status.success())
}

/// Check if delve (Go debugger) is available
pub async fn is_delve_available() -> bool {
    let result = Command::new("dlv")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    matches!(result, Ok(status) if status.success())
}

/// Check if lldb-dap (LLDB DAP adapter for Rust/C/C++) is available
pub async fn is_lldb_available() -> bool {
    // Try the homebrew LLVM path first (macOS)
    let lldb_paths = [
        "/opt/homebrew/opt/llvm/bin/lldb-dap",
        "lldb-dap",
        "lldb-vscode", // older name
    ];

    for path in lldb_paths {
        let result = Command::new(path)
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        if matches!(result, Ok(status) if status.success()) {
            return true;
        }
    }
    false
}

/// Check if pyright (Python LSP) is available
pub async fn is_pyright_available() -> bool {
    let result = Command::new("pyright-langserver")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    matches!(result, Ok(status) if status.success())
}

/// Check if pylsp (Python LSP - alternative) is available
pub async fn is_pylsp_available() -> bool {
    let result = Command::new("pylsp")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    matches!(result, Ok(status) if status.success())
}

/// Check if any Python LSP is available (pyright or pylsp)
pub async fn is_python_lsp_available() -> bool {
    is_pyright_available().await || is_pylsp_available().await
}

/// Check if gopls (Go LSP) is available
pub async fn is_gopls_available() -> bool {
    let result = Command::new("gopls")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    matches!(result, Ok(status) if status.success())
}

/// Check if Go compiler is available (needed for building Go test programs)
pub async fn is_go_available() -> bool {
    let result = Command::new("go")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    matches!(result, Ok(status) if status.success())
}

/// Check if Rust compiler is available (needed for building Rust test programs)
pub async fn is_rustc_available() -> bool {
    let result = Command::new("rustc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    matches!(result, Ok(status) if status.success())
}

/// Check if CodeLLDB (vadimcn's adapter) is available
///
/// CodeLLDB has better PDB support on Windows compared to standard lldb-dap.
/// Checks VSCode extensions directories and Detrix's auto-download location.
pub async fn is_codelldb_available() -> bool {
    // Check VSCode extensions directories
    #[cfg(target_os = "windows")]
    let home_var = "USERPROFILE";
    #[cfg(not(target_os = "windows"))]
    let home_var = "HOME";

    if let Ok(home) = std::env::var(home_var) {
        let extension_dirs = [
            format!("{}/.vscode/extensions", home),
            format!("{}/.vscode-insiders/extensions", home),
            #[cfg(target_os = "linux")]
            format!("{}/.vscode-oss/extensions", home),
        ];

        for ext_dir in &extension_dirs {
            let ext_path = std::path::PathBuf::from(ext_dir);
            if !ext_path.exists() {
                continue;
            }

            if let Ok(entries) = std::fs::read_dir(&ext_path) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with("vadimcn.vscode-lldb") {
                        #[cfg(target_os = "windows")]
                        let binary_name = "codelldb.exe";
                        #[cfg(not(target_os = "windows"))]
                        let binary_name = "codelldb";

                        let adapter_path = entry.path().join("adapter").join(binary_name);
                        if adapter_path.exists() {
                            return true;
                        }
                    }
                }
            }
        }
    }

    // Check Detrix's auto-download location
    #[cfg(target_os = "windows")]
    let base = std::env::var("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("USERPROFILE")
                .map(|h| std::path::PathBuf::from(h).join("AppData").join("Local"))
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
        });

    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME")
        .map(|h| {
            std::path::PathBuf::from(h)
                .join("Library")
                .join("Application Support")
        })
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    #[cfg(target_os = "linux")]
    let base = std::env::var("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| {
            std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".local").join("share"))
        })
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    let adapters_dir = base.join("detrix").join("debug_adapters").join("CodeLLDB");
    if adapters_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&adapters_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    #[cfg(target_os = "windows")]
                    let binary_name = "codelldb.exe";
                    #[cfg(not(target_os = "windows"))]
                    let binary_name = "codelldb";

                    let binary_path = path.join("extension").join("adapter").join(binary_name);
                    if binary_path.exists() {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Macro to skip test if tool is not available (with tracking)
///
/// Prefer using `require_tool(ToolDependency::X)` for new code.
/// This macro is kept for backwards compatibility.
///
/// # Usage
///
/// ```ignore
/// skip_if_unavailable!(is_debugpy_available, "debugpy");
/// ```
#[macro_export]
macro_rules! skip_if_unavailable {
    ($check_fn:expr, $tool_name:expr) => {
        if !$check_fn.await {
            $crate::e2e::availability::record_missing($tool_name);
            eprintln!(
                "\n\x1b[33m⚠ SKIPPING TEST: {} not available\x1b[0m\n",
                $tool_name
            );
            return;
        }
    };
}

/// Macro to require a tool dependency (preferred over skip_if_unavailable)
///
/// # Usage
///
/// ```ignore
/// require_tool!(ToolDependency::Debugpy);
/// ```
#[macro_export]
macro_rules! require_tool_macro {
    ($tool:expr) => {
        if !$crate::e2e::availability::require_tool($tool).await {
            return;
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_availability_checks_dont_panic() {
        // These should not panic, just return true/false
        let _ = is_debugpy_available().await;
        let _ = is_rust_analyzer_available().await;
        let _ = is_delve_available().await;
        let _ = is_lldb_available().await;
        let _ = is_codelldb_available().await;
        let _ = is_pyright_available().await;
        let _ = is_pylsp_available().await;
        let _ = is_gopls_available().await;
        let _ = is_go_available().await;
        let _ = is_rustc_available().await;
    }

    #[tokio::test]
    async fn test_tool_dependency_enum() {
        // Verify all tools can be checked without panic
        for tool in [
            ToolDependency::Debugpy,
            ToolDependency::RustAnalyzer,
            ToolDependency::Gopls,
            ToolDependency::Delve,
            ToolDependency::LldbDap,
            ToolDependency::CodeLldb,
            ToolDependency::Pyright,
            ToolDependency::Pylsp,
            ToolDependency::Go,
            ToolDependency::Rustc,
        ] {
            let _ = tool.is_available().await;
            assert!(!tool.name().is_empty());
        }
    }

    #[test]
    fn test_missing_deps_tracking() {
        record_missing("test-tool");
        let missing = get_missing_dependencies();
        assert!(missing.contains(&"test-tool"));
    }
}
