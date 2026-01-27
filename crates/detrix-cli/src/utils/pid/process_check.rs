//! Platform-specific process detection
//!
//! Provides functions to check if a process is running and if it's a detrix daemon.

use std::process::Command;

/// Check if a process with the given PID is running AND is a detrix process
#[cfg(windows)]
#[allow(dead_code)] // Used indirectly via is_pid_detrix_daemon on Windows
pub(crate) fn is_process_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // Use tasklist to check if process exists
    // Try multiple times with small delay to handle race conditions
    for attempt in 0..3 {
        match std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                // tasklist returns "INFO: No tasks..." when process not found
                // Check both stdout and stderr as behavior varies by Windows version
                let combined = format!("{}{}", stdout, stderr);

                // If clearly says no tasks, process doesn't exist
                if combined.contains("INFO:") || combined.to_lowercase().contains("no tasks") {
                    return false;
                }

                // If output contains our PID AND the process is named "detrix", it's running
                // This prevents false positives from PID reuse by other processes
                let stdout_lower = stdout.to_lowercase();
                if stdout.contains(&pid.to_string()) && stdout_lower.contains("detrix") {
                    return true;
                }

                // PID exists but isn't detrix - treat as stale (PID was reused)
                if stdout.contains(&pid.to_string()) && !stdout_lower.contains("detrix") {
                    return false;
                }
            }
            Err(_) => {
                // tasklist failed, try again
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // If tasklist failed or gave ambiguous results, default to false
    // This allows recovery from stale PID files at the cost of potentially
    // starting a duplicate daemon (which will fail on port binding anyway)
    false
}

/// Check if a process with the given PID is running
///
/// This is a basic existence check - it doesn't verify the process is a detrix daemon.
/// For daemon-specific verification, use `is_pid_detrix_daemon`.
#[cfg(unix)]
pub(crate) fn is_process_running(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    if pid == 0 {
        return false;
    }
    // On Unix, send signal 0 (None) to check if process exists without actually signaling
    // This returns Ok if process exists (and we have permission), Err otherwise
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

/// Check if a PID is a running detrix daemon process
///
/// This prevents false positives when a stale PID file references a PID
/// that has been reused by an unrelated process.
///
/// Returns true if:
/// - Process with given PID exists
/// - Process command line contains "detrix" and "serve"
#[cfg_attr(not(windows), allow(dead_code))]
pub fn is_pid_detrix_daemon(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    #[cfg(target_os = "linux")]
    {
        // On Linux, read /proc/{pid}/cmdline
        let cmdline_path = format!("/proc/{}/cmdline", pid);
        if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
            // cmdline uses null bytes as separators
            let cmdline_lower = cmdline.to_lowercase();
            return cmdline_lower.contains("detrix") && cmdline_lower.contains("serve");
        }
        false
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, use ps command
        if let Ok(output) = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
        {
            if output.status.success() {
                let cmd = String::from_utf8_lossy(&output.stdout);
                let cmd_lower = cmd.to_lowercase();
                return cmd_lower.contains("detrix") && cmd_lower.contains("serve");
            }
        }
        false
    }

    #[cfg(windows)]
    {
        // On Windows, use tasklist
        if let Ok(output) = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stdout_lower = stdout.to_lowercase();
            // Check if process exists and is detrix
            return stdout.contains(&pid.to_string()) && stdout_lower.contains("detrix");
        }
        false
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        // For other Unix-like systems, try ps
        if let Ok(output) = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
        {
            if output.status.success() {
                let cmd = String::from_utf8_lossy(&output.stdout);
                let cmd_lower = cmd.to_lowercase();
                return cmd_lower.contains("detrix") && cmd_lower.contains("serve");
            }
        }
        false
    }
}
