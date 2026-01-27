//! lldb-dap / CodeLLDB discovery
//!
//! Handles finding or downloading the appropriate debug adapter:
//! - On Windows: Prefers CodeLLDB (better PDB support), auto-downloads if needed
//! - On macOS/Linux: Uses standard lldb-dap from LLVM

#[cfg(target_os = "windows")]
use super::codelldb_installer;
#[cfg(target_os = "windows")]
use detrix_logging::warn;
use detrix_logging::{debug, info};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Find the debug adapter executable (lldb-dap or CodeLLDB)
///
/// Discovery order:
/// 1. Explicit path via `--lldb-dap-path` argument
/// 2. On Windows: CodeLLDB from VSCode extensions or auto-download from GitHub
/// 3. Standard lldb-dap from PATH or common locations
/// 4. macOS: Try xcrun
pub fn find_lldb_dap(explicit_path: &Option<PathBuf>) -> anyhow::Result<PathBuf> {
    // 1. Use explicit path if provided
    if let Some(path) = explicit_path {
        if path.exists() {
            info!("Using explicitly specified debug adapter: {:?}", path);
            return Ok(path.clone());
        }
        anyhow::bail!("Specified lldb-dap not found: {:?}", path);
    }

    // 2. On Windows, prefer CodeLLDB for better PDB support
    #[cfg(target_os = "windows")]
    {
        info!("Windows detected - looking for CodeLLDB (better PDB support)...");
        match codelldb_installer::find_or_install_codelldb() {
            Ok(path) => {
                info!("Using CodeLLDB: {:?}", path);
                return Ok(path);
            }
            Err(e) => {
                warn!(
                    "Failed to find/install CodeLLDB: {}. Falling back to lldb-dap.",
                    e
                );
            }
        }
    }

    // 3. Try standard lldb-dap locations
    let path = find_standard_lldb_dap()?;
    info!("Using lldb-dap: {:?}", path);
    Ok(path)
}

/// Find standard lldb-dap (without CodeLLDB auto-download)
fn find_standard_lldb_dap() -> anyhow::Result<PathBuf> {
    // Try common locations
    #[cfg(not(target_os = "windows"))]
    let candidates = [
        "lldb-dap",    // In PATH
        "lldb-vscode", // Older name, also in PATH
        "/usr/bin/lldb-dap",
        "/usr/local/bin/lldb-dap",
        "/opt/homebrew/bin/lldb-dap",
        "/opt/homebrew/opt/llvm/bin/lldb-dap",
    ];

    #[cfg(target_os = "windows")]
    let candidates = [
        "lldb-dap.exe",    // In PATH
        "lldb-vscode.exe", // Older name, also in PATH
    ];

    for candidate in &candidates {
        let path = PathBuf::from(candidate);
        if path.exists() {
            debug!("Found debug adapter at: {:?}", path);
            return Ok(path);
        }

        // Also try which/where command
        #[cfg(not(target_os = "windows"))]
        let which_cmd = "which";
        #[cfg(target_os = "windows")]
        let which_cmd = "where";

        if let Ok(output) = Command::new(which_cmd).arg(candidate).output() {
            if output.status.success() {
                let found = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !found.is_empty() {
                    debug!("Found debug adapter via {}: {}", which_cmd, found);
                    return Ok(PathBuf::from(found));
                }
            }
        }
    }

    // Try xcrun on macOS
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("xcrun").args(["-f", "lldb-dap"]).output() {
            if output.status.success() {
                let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !found.is_empty() {
                    debug!("Found debug adapter via xcrun: {}", found);
                    return Ok(PathBuf::from(found));
                }
            }
        }
    }

    // Platform-specific error messages
    #[cfg(target_os = "windows")]
    anyhow::bail!(
        "Debug adapter not found. Options:\n\
         1. Detrix will auto-download CodeLLDB (check network/firewall)\n\
         2. Install CodeLLDB VSCode extension manually\n\
         3. Install LLVM and add to PATH\n\
         4. Specify path with --lldb-dap-path\n\
         5. Build with x86_64-pc-windows-gnu toolchain for DWARF debug info"
    );

    #[cfg(target_os = "macos")]
    anyhow::bail!(
        "lldb-dap not found. Install LLVM or specify path with --lldb-dap-path.\n\
         Recommended: brew install llvm"
    );

    #[cfg(target_os = "linux")]
    anyhow::bail!(
        "lldb-dap not found. Install LLVM or specify path with --lldb-dap-path.\n\
         On Ubuntu/Debian: sudo apt install lldb\n\
         On Fedora: sudo dnf install lldb"
    );

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    anyhow::bail!("lldb-dap not found. Install LLVM or specify path with --lldb-dap-path.")
}

/// Check if the given path is a CodeLLDB adapter (vs standard lldb-dap)
pub fn is_codelldb(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase().contains("codelldb"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_codelldb_true() {
        assert!(is_codelldb(&PathBuf::from("/path/to/codelldb")));
        assert!(is_codelldb(&PathBuf::from("/path/to/codelldb.exe")));
        assert!(is_codelldb(&PathBuf::from(
            "C:\\Users\\user\\.vscode\\extensions\\vadimcn.vscode-lldb\\adapter\\codelldb.exe"
        )));
    }

    #[test]
    fn test_is_codelldb_false() {
        assert!(!is_codelldb(&PathBuf::from("/path/to/lldb-dap")));
        assert!(!is_codelldb(&PathBuf::from("/usr/bin/lldb-dap")));
        assert!(!is_codelldb(&PathBuf::from("lldb-vscode.exe")));
    }
}
