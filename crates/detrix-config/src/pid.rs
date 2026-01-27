//! PID File Data Structures
//!
//! Contains the `PidInfo` struct for reading daemon PID file data.
//! The actual file locking and management is in `detrix-cli`.
//!
//! ## File Format (JSON)
//!
//! ```json
//! {"pid":12345,"ports":{"http":8080,"grpc":50051},"host":"127.0.0.1"}
//! ```

use crate::{constants::DEFAULT_API_HOST, PortRegistry, ServiceType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Default API host (127.0.0.1)
fn default_api_host() -> String {
    DEFAULT_API_HOST.to_string()
}

/// Information stored in the PID file (JSON format)
///
/// This struct represents the daemon state stored in the PID file.
/// It can be used to read daemon info without the file locking functionality
/// needed by the daemon itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PidInfo {
    /// Process ID
    pub pid: u32,
    /// All allocated ports by service type
    #[serde(default)]
    pub ports: HashMap<ServiceType, u16>,
    /// API host the daemon is listening on (default: 127.0.0.1)
    #[serde(default = "default_api_host")]
    pub host: String,
}

impl PidInfo {
    /// Create new PID info with just PID
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            ports: HashMap::new(),
            host: default_api_host(),
        }
    }

    /// Create new PID info with PID and HTTP port (backward compatible)
    pub fn with_port(pid: u32, port: u16) -> Self {
        let mut ports = HashMap::new();
        ports.insert(ServiceType::Http, port);
        Self {
            pid,
            ports,
            host: default_api_host(),
        }
    }

    /// Create PID info from a PortRegistry
    pub fn from_registry(pid: u32, registry: &PortRegistry) -> Self {
        Self {
            pid,
            ports: registry.all_allocated(),
            host: default_api_host(),
        }
    }

    /// Create PID info from a PortRegistry with custom host
    pub fn from_registry_with_host(pid: u32, registry: &PortRegistry, host: String) -> Self {
        Self {
            pid,
            ports: registry.all_allocated(),
            host,
        }
    }

    /// Get the HTTP port (backward compatible)
    pub fn port(&self) -> Option<u16> {
        self.ports.get(&ServiceType::Http).copied()
    }

    /// Get port for a specific service
    pub fn get_port(&self, service: ServiceType) -> Option<u16> {
        self.ports.get(&service).copied()
    }

    /// Set port for a specific service
    pub fn set_port(&mut self, service: ServiceType, port: u16) {
        self.ports.insert(service, port);
    }

    /// Get the API host
    pub fn get_host(&self) -> &str {
        &self.host
    }

    /// Read PID info from a file path
    ///
    /// This is a simple read operation without file locking.
    /// For daemon-safe operations with locking, use `PidFile` from `detrix-cli`.
    ///
    /// # Returns
    /// - `Ok(Some(PidInfo))` if file exists and is valid JSON
    /// - `Ok(None)` if file doesn't exist
    /// - `Err` if file exists but can't be parsed
    pub fn read_from_file(path: impl AsRef<Path>) -> Result<Option<Self>, PidReadError> {
        let path = path.as_ref();

        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(path).map_err(|e| PidReadError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        let info: PidInfo =
            serde_json::from_str(content.trim()).map_err(|e| PidReadError::Parse {
                path: path.to_path_buf(),
                source: e,
            })?;

        Ok(Some(info))
    }
}

/// Errors when reading PID file
#[derive(Debug, thiserror::Error)]
pub enum PidReadError {
    #[error("Failed to read PID file {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to parse PID file {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        source: serde_json::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pid_info_new() {
        let info = PidInfo::new(12345);
        assert_eq!(info.pid, 12345);
        assert!(info.ports.is_empty());
        assert_eq!(info.host, "127.0.0.1");
    }

    #[test]
    fn test_pid_info_with_port() {
        let info = PidInfo::with_port(12345, 8080);
        assert_eq!(info.pid, 12345);
        assert_eq!(info.port(), Some(8080));
        assert_eq!(info.get_port(ServiceType::Http), Some(8080));
    }

    #[test]
    fn test_pid_info_serialization() {
        let info = PidInfo::with_port(12345, 8080);
        let json = serde_json::to_string(&info).unwrap();
        let parsed: PidInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, parsed);
    }

    #[test]
    fn test_parse_json_with_host() {
        let json = r#"{"pid":12345,"ports":{"http":8080},"host":"192.168.1.1"}"#;
        let info: PidInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.pid, 12345);
        assert_eq!(info.port(), Some(8080));
        assert_eq!(info.get_host(), "192.168.1.1");
    }

    #[test]
    fn test_parse_json_default_host() {
        // JSON without host field should get default
        let json = r#"{"pid":12345,"ports":{"http":8080}}"#;
        let info: PidInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.get_host(), "127.0.0.1");
    }

    #[test]
    fn test_read_from_file_not_exists() {
        let result = PidInfo::read_from_file("/nonexistent/path.pid");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
