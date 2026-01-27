//! Parent process detection for MCP bridge
//!
//! Detects the IDE/editor that spawned the MCP bridge process.

use detrix_logging::info;

/// Information about the parent process that spawned the MCP bridge
#[derive(Debug, Clone)]
pub struct ParentProcessInfo {
    /// Parent process ID (the IDE/editor like Windsurf, Claude Code, Cursor)
    pub pid: u32,
    /// Name of the parent process
    pub name: String,
    /// This bridge process PID
    pub bridge_pid: u32,
}

/// Detect the parent process of the MCP bridge
///
/// On Unix: Uses getppid() and reads /proc or uses ps to get the parent name
/// On Windows: Uses GetParentProcessId
pub fn detect_parent_process() -> Option<ParentProcessInfo> {
    let bridge_pid = std::process::id();

    #[cfg(unix)]
    {
        // Get parent PID
        let ppid = unsafe { nix::libc::getppid() } as u32;
        if ppid == 0 || ppid == 1 {
            // init process or orphaned
            return None;
        }

        // Get parent process name
        let parent_name = get_process_name(ppid)?;

        // Normalize the name (extract known IDE names)
        let normalized_name = normalize_parent_name(&parent_name);

        info!(
            "Detected parent process: {} (PID: {}) -> bridge (PID: {})",
            normalized_name, ppid, bridge_pid
        );

        Some(ParentProcessInfo {
            pid: ppid,
            name: normalized_name,
            bridge_pid,
        })
    }

    #[cfg(windows)]
    {
        use std::process::Command;

        // Use WMIC to get parent process info
        let output = Command::new("wmic")
            .args([
                "process",
                "where",
                &format!("ProcessId={}", bridge_pid),
                "get",
                "ParentProcessId",
                "/value",
            ])
            .output()
            .ok()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let ppid: u32 = stdout
            .lines()
            .find(|l| l.starts_with("ParentProcessId="))
            .and_then(|l| l.split('=').nth(1))
            .and_then(|s| s.trim().parse().ok())?;

        let parent_name = get_process_name_windows(ppid)?;
        let normalized_name = normalize_parent_name(&parent_name);

        info!(
            "Detected parent process: {} (PID: {}) -> bridge (PID: {})",
            normalized_name, ppid, bridge_pid
        );

        Some(ParentProcessInfo {
            pid: ppid,
            name: normalized_name,
            bridge_pid,
        })
    }

    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// Get process name from PID on Unix
#[cfg(unix)]
fn get_process_name(pid: u32) -> Option<String> {
    // Try /proc first (Linux)
    #[cfg(target_os = "linux")]
    {
        let comm_path = format!("/proc/{}/comm", pid);
        if let Ok(name) = std::fs::read_to_string(&comm_path) {
            return Some(name.trim().to_string());
        }
    }

    // Fallback: use ps (macOS and Linux)
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;

    if output.status.success() {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            // Extract just the binary name from full path
            return Some(name.rsplit('/').next().unwrap_or(&name).to_string());
        }
    }

    None
}

/// Get process name from PID on Windows
#[cfg(windows)]
fn get_process_name_windows(pid: u32) -> Option<String> {
    use std::process::Command;

    let output = Command::new("wmic")
        .args([
            "process",
            "where",
            &format!("ProcessId={}", pid),
            "get",
            "Name",
            "/value",
        ])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find(|l| l.starts_with("Name="))
        .and_then(|l| l.split('=').nth(1))
        .map(|s| s.trim().to_string())
}

/// Normalize parent process name to known IDE names
fn normalize_parent_name(name: &str) -> String {
    let lower = name.to_lowercase();

    // Map known process names to friendly display names
    if lower.contains("windsurf") {
        "windsurf".to_string()
    } else if lower.contains("claude") {
        "claude".to_string()
    } else if lower.contains("cursor") {
        "cursor".to_string()
    } else if lower.contains("code") || lower.contains("vscode") {
        "vscode".to_string()
    } else if lower.contains("zed") {
        "zed".to_string()
    } else if lower.contains("nvim") || lower.contains("neovim") {
        "neovim".to_string()
    } else if lower.contains("vim") {
        "vim".to_string()
    } else if lower.contains("emacs") {
        "emacs".to_string()
    } else if lower.contains("intellij") || lower.contains("idea") {
        "intellij".to_string()
    } else if lower.contains("pycharm") {
        "pycharm".to_string()
    } else if lower.contains("webstorm") {
        "webstorm".to_string()
    } else if lower.contains("goland") {
        "goland".to_string()
    } else if lower.contains("rustrover") {
        "rustrover".to_string()
    } else if lower.contains("sublime") {
        "sublime".to_string()
    } else if lower.contains("atom") {
        "atom".to_string()
    } else {
        // Return original name for unknown processes
        name.to_string()
    }
}
