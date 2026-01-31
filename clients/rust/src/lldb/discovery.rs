//! Discovery of the lldb-dap binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// Find the lldb-dap binary.
///
/// Search order:
/// 1. Explicit path if provided
/// 2. PATH lookup for "lldb-dap"
/// 3. macOS: /opt/homebrew/opt/llvm/bin/lldb-dap (Homebrew LLVM)
/// 4. macOS: xcrun -f lldb-dap (Xcode Command Line Tools)
/// 5. Linux: /usr/bin/lldb-dap
pub fn find_lldb_dap(explicit_path: Option<&Path>) -> Result<PathBuf> {
    // 1. Check explicit path
    if let Some(path) = explicit_path {
        if path.exists() && is_executable(path) {
            return Ok(path.to_path_buf());
        }
        return Err(Error::LldbNotFound(format!(
            "lldb-dap not found at {:?}",
            path
        )));
    }

    // 2. PATH lookup
    if let Ok(output) = Command::new("which").arg("lldb-dap").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let path = PathBuf::from(&path_str);
            if path.exists() && is_executable(&path) {
                return Ok(path);
            }
        }
    }

    // Platform-specific paths
    #[cfg(target_os = "macos")]
    {
        // 3. Homebrew LLVM
        let homebrew_path = PathBuf::from("/opt/homebrew/opt/llvm/bin/lldb-dap");
        if homebrew_path.exists() && is_executable(&homebrew_path) {
            return Ok(homebrew_path);
        }

        // Also check Intel Mac Homebrew path
        let homebrew_intel = PathBuf::from("/usr/local/opt/llvm/bin/lldb-dap");
        if homebrew_intel.exists() && is_executable(&homebrew_intel) {
            return Ok(homebrew_intel);
        }

        // 4. Xcode Command Line Tools via xcrun
        if let Ok(output) = Command::new("xcrun").args(["-f", "lldb-dap"]).output() {
            if output.status.success() {
                let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let path = PathBuf::from(&path_str);
                if path.exists() && is_executable(&path) {
                    return Ok(path);
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        // 5. Common Linux paths
        let linux_paths = [
            "/usr/bin/lldb-dap",
            "/usr/local/bin/lldb-dap",
            "/usr/lib/llvm-18/bin/lldb-dap",
            "/usr/lib/llvm-17/bin/lldb-dap",
            "/usr/lib/llvm-16/bin/lldb-dap",
            "/usr/lib/llvm-15/bin/lldb-dap",
        ];

        for path_str in linux_paths {
            let path = PathBuf::from(path_str);
            if path.exists() && is_executable(&path) {
                return Ok(path);
            }
        }
    }

    Err(Error::LldbNotFound(
        "lldb-dap not found in PATH or common locations. \
         Install LLVM or set DETRIX_LLDB_DAP_PATH."
            .to_string(),
    ))
}

/// Check if a path is executable.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_lldb_dap_explicit_nonexistent() {
        let result = find_lldb_dap(Some(Path::new("/nonexistent/lldb-dap")));
        assert!(result.is_err());
        if let Err(Error::LldbNotFound(msg)) = result {
            assert!(msg.contains("not found"));
        }
    }

    #[test]
    fn test_find_lldb_dap_discovery() {
        // This test may pass or fail depending on whether lldb-dap is installed
        // Just verify it doesn't panic
        let _ = find_lldb_dap(None);
    }
}
