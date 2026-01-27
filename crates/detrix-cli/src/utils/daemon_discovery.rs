//! Centralized daemon discovery module
//!
//! Provides robust daemon detection that works across CLI commands and MCP bridge.
//! Uses multiple strategies to find the running daemon:
//!
//! 1. **PID file check**: Primary method - reads PID file and verifies process is running
//! 2. **Port probe fallback**: If PID file is stale/missing, probes default ports
//!
//! ## Usage
//!
//! ```no_run
//! use detrix_cli::utils::daemon_discovery::{DaemonDiscovery, DaemonInfo};
//!
//! let discovery = DaemonDiscovery::new();
//! match discovery.find_daemon() {
//!     Ok(Some(info)) => println!("Daemon found at {}:{}", info.host, info.grpc_port),
//!     Ok(None) => println!("Daemon is not running"),
//!     Err(e) => eprintln!("Error: {}", e),
//! }
//! ```

use crate::utils::pid::PidFile;
use anyhow::{Context, Result};
use detrix_config::constants::{
    DEFAULT_API_HOST, DEFAULT_GRPC_PORT, DEFAULT_PORT_PROBE_TIMEOUT_MS, DEFAULT_REST_PORT,
};
use detrix_config::pid::PidInfo;
use detrix_config::ServiceType;
use detrix_logging::debug;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Information about a discovered daemon
#[derive(Debug, Clone)]
pub struct DaemonInfo {
    /// Process ID of the daemon (if known)
    pub pid: Option<u32>,
    /// Host the daemon is listening on
    pub host: String,
    /// HTTP/REST port
    pub http_port: u16,
    /// gRPC port
    pub grpc_port: u16,
    /// How the daemon was discovered
    pub discovery_method: DiscoveryMethod,
}

impl DaemonInfo {
    /// Create from PidInfo
    pub fn from_pid_info(info: &PidInfo) -> Self {
        Self {
            pid: Some(info.pid),
            host: info.host.clone(),
            http_port: info
                .get_port(ServiceType::Http)
                .unwrap_or(DEFAULT_REST_PORT),
            grpc_port: info
                .get_port(ServiceType::Grpc)
                .unwrap_or(DEFAULT_GRPC_PORT),
            discovery_method: DiscoveryMethod::PidFile,
        }
    }

    /// Create from port probe
    pub fn from_port_probe(host: String, http_port: u16, grpc_port: u16) -> Self {
        Self {
            pid: None,
            host,
            http_port,
            grpc_port,
            discovery_method: DiscoveryMethod::PortProbe,
        }
    }

    /// Get the gRPC endpoint URL
    pub fn grpc_endpoint(&self) -> String {
        format!("http://{}:{}", self.host, self.grpc_port)
    }

    /// Get the HTTP endpoint URL
    pub fn http_endpoint(&self) -> String {
        format!("http://{}:{}", self.host, self.http_port)
    }
}

/// How the daemon was discovered
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryMethod {
    /// Found via PID file
    PidFile,
    /// Found via port probe (PID file was stale or missing)
    PortProbe,
}

/// Daemon discovery configuration
#[derive(Debug, Clone)]
pub struct DaemonDiscovery {
    /// Path to the PID file
    pid_file_path: PathBuf,
    /// Host to probe for daemon
    probe_host: String,
    /// HTTP port to probe
    probe_http_port: u16,
    /// gRPC port to probe
    probe_grpc_port: u16,
    /// Timeout for port probing
    probe_timeout: Duration,
    /// Whether to enable port probe fallback
    enable_port_probe: bool,
}

impl Default for DaemonDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

impl DaemonDiscovery {
    /// Create a new daemon discovery with default settings
    pub fn new() -> Self {
        Self {
            pid_file_path: detrix_config::paths::default_pid_path(),
            probe_host: DEFAULT_API_HOST.to_string(),
            probe_http_port: DEFAULT_REST_PORT,
            probe_grpc_port: DEFAULT_GRPC_PORT,
            probe_timeout: Duration::from_millis(DEFAULT_PORT_PROBE_TIMEOUT_MS),
            enable_port_probe: true,
        }
    }

    /// Set the PID file path
    pub fn with_pid_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.pid_file_path = path.into();
        self
    }

    /// Set the probe host
    pub fn with_probe_host(mut self, host: impl Into<String>) -> Self {
        self.probe_host = host.into();
        self
    }

    /// Set the probe ports
    pub fn with_probe_ports(mut self, http_port: u16, grpc_port: u16) -> Self {
        self.probe_http_port = http_port;
        self.probe_grpc_port = grpc_port;
        self
    }

    /// Set the probe timeout
    #[allow(dead_code)]
    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// Enable or disable port probe fallback
    #[allow(dead_code)]
    pub fn with_port_probe(mut self, enabled: bool) -> Self {
        self.enable_port_probe = enabled;
        self
    }

    /// Configure from Config
    #[allow(dead_code)]
    pub fn from_config(config: &detrix_config::Config) -> Self {
        Self::new()
            .with_pid_file(&config.daemon.pid_file)
            .with_probe_host(&config.api.rest.host)
            .with_probe_ports(config.api.rest.port, config.api.grpc.port)
    }

    /// Find a running daemon
    ///
    /// Returns:
    /// - `Ok(Some(info))` if daemon is found
    /// - `Ok(None)` if daemon is not running
    /// - `Err(e)` if there was an error checking
    pub fn find_daemon(&self) -> Result<Option<DaemonInfo>> {
        // Strategy 1: Check PID file
        match self.find_via_pid_file()? {
            Some(info) => {
                debug!(
                    pid = info.pid,
                    host = %info.host,
                    grpc_port = info.grpc_port,
                    "Daemon found via PID file"
                );
                return Ok(Some(info));
            }
            None => {
                debug!(path = %self.pid_file_path.display(), "PID file check: daemon not found");
            }
        }

        // Strategy 2: Port probe fallback (if enabled)
        if self.enable_port_probe {
            match self.find_via_port_probe() {
                Some(info) => {
                    debug!(
                        host = %info.host,
                        grpc_port = info.grpc_port,
                        "Daemon found via port probe"
                    );
                    return Ok(Some(info));
                }
                None => {
                    debug!("Port probe: daemon not found on default ports");
                }
            }
        }

        Ok(None)
    }

    /// Check if daemon is running (simple boolean check)
    #[allow(dead_code)]
    pub fn is_running(&self) -> Result<bool> {
        Ok(self.find_daemon()?.is_some())
    }

    /// Get the PID file path
    #[allow(dead_code)]
    pub fn pid_file_path(&self) -> &Path {
        &self.pid_file_path
    }

    /// Find daemon via PID file
    fn find_via_pid_file(&self) -> Result<Option<DaemonInfo>> {
        match PidFile::read_info(&self.pid_file_path).context("Failed to read PID file")? {
            Some(pid_info) => Ok(Some(DaemonInfo::from_pid_info(&pid_info))),
            None => Ok(None),
        }
    }

    /// Find daemon via port probing
    ///
    /// Tries to connect to the configured gRPC and HTTP ports to check if daemon is running.
    /// Uses `probe_grpc_port` and `probe_http_port` which default to constants but can be
    /// configured via `with_probe_ports()`.
    /// This is useful when PID file is stale but daemon is actually running.
    fn find_via_port_probe(&self) -> Option<DaemonInfo> {
        // Try gRPC port first (primary protocol)
        let grpc_addr = format!("{}:{}", self.probe_host, self.probe_grpc_port);
        if self.is_port_open(&grpc_addr) {
            // Also check HTTP port to confirm it's Detrix
            let http_addr = format!("{}:{}", self.probe_host, self.probe_http_port);
            if self.is_port_open(&http_addr) {
                return Some(DaemonInfo::from_port_probe(
                    self.probe_host.clone(),
                    self.probe_http_port,
                    self.probe_grpc_port,
                ));
            }
            // Only gRPC port is open - might be Detrix
            return Some(DaemonInfo::from_port_probe(
                self.probe_host.clone(),
                self.probe_http_port, // Assume default
                self.probe_grpc_port,
            ));
        }

        // Try HTTP port alone
        let http_addr = format!("{}:{}", self.probe_host, self.probe_http_port);
        if self.is_port_open(&http_addr) {
            return Some(DaemonInfo::from_port_probe(
                self.probe_host.clone(),
                self.probe_http_port,
                self.probe_grpc_port, // Assume default
            ));
        }

        None
    }

    /// Check if a port is open by attempting a TCP connection
    fn is_port_open(&self, addr: &str) -> bool {
        let socket_addr = addr.parse().unwrap_or_else(|_| {
            // Safe fallback using SocketAddr::from which cannot fail
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_GRPC_PORT))
        });
        TcpStream::connect_timeout(&socket_addr, self.probe_timeout).is_ok()
    }
}

/// Convenience function to find daemon with default settings
#[allow(dead_code)]
pub fn find_daemon() -> Result<Option<DaemonInfo>> {
    DaemonDiscovery::new().find_daemon()
}

/// Convenience function to find daemon with custom PID file path
#[allow(dead_code)]
pub fn find_daemon_with_pid_file(pid_file_path: &Path) -> Result<Option<DaemonInfo>> {
    DaemonDiscovery::new()
        .with_pid_file(pid_file_path)
        .find_daemon()
}

/// Convenience function to check if daemon is running
#[allow(dead_code)]
pub fn is_daemon_running() -> Result<bool> {
    DaemonDiscovery::new().is_running()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    #[test]
    fn test_daemon_info_from_pid_info() {
        let mut pid_info = PidInfo::new(12345);
        pid_info.ports.insert(ServiceType::Http, 9080);
        pid_info.ports.insert(ServiceType::Grpc, 59051);
        pid_info.host = "192.168.1.1".to_string();

        let daemon_info = DaemonInfo::from_pid_info(&pid_info);

        assert_eq!(daemon_info.pid, Some(12345));
        assert_eq!(daemon_info.host, "192.168.1.1");
        assert_eq!(daemon_info.http_port, 9080);
        assert_eq!(daemon_info.grpc_port, 59051);
        assert_eq!(daemon_info.discovery_method, DiscoveryMethod::PidFile);
    }

    #[test]
    fn test_daemon_info_endpoints() {
        let info = DaemonInfo::from_port_probe("localhost".to_string(), 8080, 50051);

        assert_eq!(info.grpc_endpoint(), "http://localhost:50051");
        assert_eq!(info.http_endpoint(), "http://localhost:8080");
    }

    #[test]
    fn test_discovery_builder() {
        let discovery = DaemonDiscovery::new()
            .with_pid_file("/custom/path.pid")
            .with_probe_host("192.168.1.100")
            .with_probe_ports(9080, 59051)
            .with_probe_timeout(Duration::from_secs(1))
            .with_port_probe(false);

        assert_eq!(discovery.pid_file_path, PathBuf::from("/custom/path.pid"));
        assert_eq!(discovery.probe_host, "192.168.1.100");
        assert_eq!(discovery.probe_http_port, 9080);
        assert_eq!(discovery.probe_grpc_port, 59051);
        assert_eq!(discovery.probe_timeout, Duration::from_secs(1));
        assert!(!discovery.enable_port_probe);
    }

    #[test]
    fn test_find_daemon_no_daemon_running() {
        let temp_dir = std::env::temp_dir();
        let pid_path = temp_dir.join(format!("detrix_discovery_test_{}.pid", process::id()));

        // Clean up
        let _ = std::fs::remove_file(&pid_path);

        // With port probe disabled, should return None
        let discovery = DaemonDiscovery::new()
            .with_pid_file(&pid_path)
            .with_port_probe(false);

        let result = discovery.find_daemon().expect("find_daemon failed");
        assert!(result.is_none());

        // Clean up
        let _ = std::fs::remove_file(&pid_path);
    }

    #[test]
    fn test_is_port_open_closed_port() {
        let discovery = DaemonDiscovery::new().with_probe_timeout(Duration::from_millis(100));

        // Use a port that's unlikely to be open
        let is_open = discovery.is_port_open("127.0.0.1:59999");
        assert!(!is_open, "Port 59999 should not be open");
    }
}
