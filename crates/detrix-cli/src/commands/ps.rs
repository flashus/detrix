//! Process listing command
//!
//! Shows all running detrix processes with their roles:
//! - daemon (serve): Long-running server
//! - mcp: MCP server/client for LLM integration
//! - stream: Event streaming client
//!
//! Also shows debugpy processes that detrix is managing.

use crate::utils::pid::PidFile;
use anyhow::Result;
use detrix_config::ServiceType;
use std::process::Command;

/// Process role detected from command line arguments
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessRole {
    Daemon,
    Mcp,
    McpBridge,
    Stream,
    List,
    Ps,
    Unknown,
}

impl std::fmt::Display for ProcessRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessRole::Daemon => write!(f, "daemon"),
            ProcessRole::Mcp => write!(f, "mcp"),
            ProcessRole::McpBridge => write!(f, "mcp-bridge"),
            ProcessRole::Stream => write!(f, "stream"),
            ProcessRole::List => write!(f, "list"),
            ProcessRole::Ps => write!(f, "ps"),
            ProcessRole::Unknown => write!(f, "unknown"),
        }
    }
}

/// Detected process information
#[derive(Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub role: ProcessRole,
    pub cmdline: String,
    pub cpu: String,
    pub mem: String,
    /// HTTP port (only for daemon role, read from PID file)
    pub port: Option<u16>,
    /// gRPC port (only for daemon role, read from PID file)
    pub grpc_port: Option<u16>,
}

/// Run the ps command to list all detrix processes
pub async fn run() -> Result<()> {
    let current_pid = std::process::id();
    let processes = find_detrix_processes()?;

    // Filter out ps/status commands (transient) and current process
    let running_processes: Vec<_> = processes
        .into_iter()
        .filter(|p| p.pid != current_pid && p.role != ProcessRole::Ps)
        .collect();

    if running_processes.is_empty() {
        println!("No detrix processes running.");
        return Ok(());
    }

    // Print header
    println!(
        "{:<8} {:<12} {:>14} {:>6} {:>6}   COMMAND",
        "PID", "ROLE", "PORTS", "CPU%", "MEM%"
    );
    println!("{}", "-".repeat(100));

    for proc in &running_processes {
        // Format port display (show HTTP and gRPC ports)
        let port_display = match (proc.port, proc.grpc_port) {
            (Some(http), Some(grpc)) => format!("{}:{}", http, grpc),
            (Some(http), None) => http.to_string(),
            (None, Some(grpc)) => format!("-:{}", grpc),
            (None, None) => "-".to_string(),
        };

        // Truncate command if too long
        let cmd_display = if proc.cmdline.len() > 40 {
            format!("{}...", &proc.cmdline[..37])
        } else {
            proc.cmdline.clone()
        };

        println!(
            "{:<8} {:<12} {:>14} {:>6} {:>6}   {}",
            proc.pid, proc.role, port_display, proc.cpu, proc.mem, cmd_display
        );
    }

    println!();
    println!("Total: {} detrix process(es)", running_processes.len());

    Ok(())
}

/// Find all running detrix processes using ps command
fn find_detrix_processes() -> Result<Vec<ProcessInfo>> {
    // Use ps to find detrix processes
    // On macOS/Linux: ps aux | grep detrix
    let output = Command::new("ps").args(["aux"]).output()?;

    if !output.status.success() {
        anyhow::bail!("Failed to run ps command");
    }

    // Try to read port from PID file for daemon processes
    let pid_file_path = detrix_config::paths::default_pid_path();
    let daemon_info = PidFile::read_info(&pid_file_path).ok().flatten();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();

    for line in stdout.lines() {
        // Skip header line
        if line.starts_with("USER") {
            continue;
        }

        // Must be the actual detrix binary with a subcommand, not just a path containing "detrix"
        // Pattern: binary path ending with "detrix" followed by space and subcommand/flag
        // Matches: "/usr/bin/detrix serve", "/home/user/detrix mcp", "detrix --config"
        // Avoids: "/_src/detrix/some_script.py", "/detrix/config.toml"
        let is_detrix_binary = line.contains("/detrix serve")
            || line.contains("/detrix mcp")
            || line.contains("/detrix stream")
            || line.contains("/detrix list")
            || line.contains("/detrix daemon")
            || line.contains("/detrix ps")
            || line.contains("/detrix --");

        if !is_detrix_binary {
            continue;
        }

        // Skip grep and ps itself
        if line.contains("grep") || line.contains("ps aux") {
            continue;
        }

        // Parse ps aux output
        // USER PID %CPU %MEM VSZ RSS TT STAT STARTED TIME COMMAND
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 11 {
            continue;
        }

        // parts[0] is USER (not used currently)
        let pid: u32 = match parts[1].parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let cpu = parts[2].to_string();
        let mem = parts[3].to_string();

        // Command is from index 10 onwards
        let cmdline = parts[10..].join(" ");

        // Determine role from command line
        let role = detect_role(&cmdline);

        // Get ports from PID file if this is the daemon and PIDs match
        let (port, grpc_port) = if role == ProcessRole::Daemon {
            let info = daemon_info.as_ref().filter(|info| info.pid == pid);
            (
                info.and_then(|i| i.port()),
                info.and_then(|i| i.get_port(ServiceType::Grpc)),
            )
        } else {
            (None, None)
        };

        processes.push(ProcessInfo {
            pid,
            role,
            cmdline,
            cpu,
            mem,
            port,
            grpc_port,
        });
    }

    Ok(processes)
}

/// Detect the process role from command line arguments
fn detect_role(cmdline: &str) -> ProcessRole {
    // Check for subcommands in order of specificity
    // "serve" and "daemon" commands both indicate daemon role
    if cmdline.contains(" serve") || cmdline.contains("/serve") || cmdline.contains(" daemon ") {
        ProcessRole::Daemon
    } else if cmdline.contains(" mcp") && cmdline.contains("--use-daemon") {
        ProcessRole::McpBridge
    } else if cmdline.contains(" mcp") {
        ProcessRole::Mcp
    } else if cmdline.contains(" stream") {
        ProcessRole::Stream
    } else if cmdline.contains(" list") {
        ProcessRole::List
    } else if cmdline.contains(" ps") || cmdline.contains(" status") {
        ProcessRole::Ps
    } else {
        ProcessRole::Unknown
    }
}

/// Check if a detrix daemon is running
#[allow(dead_code)]
pub fn is_daemon_running() -> bool {
    if let Ok(processes) = find_detrix_processes() {
        processes
            .iter()
            .any(|p| matches!(p.role, ProcessRole::Daemon))
    } else {
        false
    }
}

/// Health response from daemon's /health endpoint
#[derive(Debug, serde::Deserialize)]
struct HealthResponse {
    service: Option<String>,
}

/// Check if daemon is running and responding on the given host and port
///
/// Verifies that the service on the port is actually a detrix daemon
/// by checking the `service` field in the health response.
pub async fn is_daemon_healthy(host: &str, port: u16) -> bool {
    // Try to hit the health endpoint
    let url = format!("http://{}:{}/health", host, port);

    match reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(response) => {
            if !response.status().is_success() {
                return false;
            }
            // Verify it's actually detrix, not some other service
            match response.json::<HealthResponse>().await {
                Ok(health) => health.service.as_deref() == Some("detrix"),
                Err(_) => false, // Not detrix - can't parse or wrong format
            }
        }
        Err(_) => false,
    }
}
