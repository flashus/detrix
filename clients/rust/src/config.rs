//! Configuration for the Detrix client.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use crate::Error;

/// Configuration for the Detrix client.
#[derive(Debug, Clone)]
pub struct Config {
    /// Connection name (default: "detrix-client-{pid}")
    pub name: Option<String>,

    /// Control plane bind host (default: "127.0.0.1")
    pub control_host: String,

    /// Control plane port (0 = auto-assign)
    pub control_port: u16,

    /// Debug adapter port (0 = auto-assign)
    pub debug_port: u16,

    /// Detrix daemon URL (default: "http://127.0.0.1:8090")
    pub daemon_url: String,

    /// Path to lldb-dap binary (default: searches PATH)
    pub lldb_dap_path: Option<PathBuf>,

    /// Detrix home directory (default: ~/detrix)
    pub detrix_home: Option<PathBuf>,

    /// Safe mode: only logpoints allowed, no breakpoint operations.
    /// Recommended for production environments.
    pub safe_mode: bool,

    /// Timeout for daemon health checks
    pub health_check_timeout: Duration,

    /// Timeout for connection registration
    pub register_timeout: Duration,

    /// Timeout for connection unregistration
    pub unregister_timeout: Duration,

    /// Timeout for lldb-dap to start
    pub lldb_start_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: None,
            control_host: "127.0.0.1".to_string(),
            control_port: 0,
            debug_port: 0,
            daemon_url: "http://127.0.0.1:8090".to_string(),
            lldb_dap_path: None,
            detrix_home: None,
            safe_mode: false,
            health_check_timeout: Duration::from_secs(2),
            // Aligned with Python/Go: 5s is sufficient for registration
            register_timeout: Duration::from_secs(5),
            unregister_timeout: Duration::from_secs(2),
            lldb_start_timeout: Duration::from_secs(10),
        }
    }
}

impl Config {
    /// Create a new Config with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply environment variable overrides.
    ///
    /// Environment variables take precedence over defaults but are overridden
    /// by explicitly set config values.
    pub fn with_env_overrides(mut self) -> Self {
        // Name
        if self.name.is_none() {
            if let Ok(name) = env::var("DETRIX_NAME") {
                if !name.is_empty() {
                    self.name = Some(name);
                }
            }
        }

        // Control host
        if self.control_host == "127.0.0.1" {
            if let Ok(host) = env::var("DETRIX_CONTROL_HOST") {
                if !host.is_empty() {
                    self.control_host = host;
                }
            }
        }

        // Control port
        if self.control_port == 0 {
            if let Ok(port_str) = env::var("DETRIX_CONTROL_PORT") {
                if let Ok(port) = port_str.parse() {
                    self.control_port = port;
                }
            }
        }

        // Debug port
        if self.debug_port == 0 {
            if let Ok(port_str) = env::var("DETRIX_DEBUG_PORT") {
                if let Ok(port) = port_str.parse() {
                    self.debug_port = port;
                }
            }
        }

        // Daemon URL
        if self.daemon_url == "http://127.0.0.1:8090" {
            if let Ok(url) = env::var("DETRIX_DAEMON_URL") {
                if !url.is_empty() {
                    self.daemon_url = url;
                }
            }
        }

        // lldb-dap path
        if self.lldb_dap_path.is_none() {
            if let Ok(path) = env::var("DETRIX_LLDB_DAP_PATH") {
                if !path.is_empty() {
                    self.lldb_dap_path = Some(PathBuf::from(path));
                }
            }
        }

        // Detrix home
        if self.detrix_home.is_none() {
            if let Ok(home) = env::var("DETRIX_HOME") {
                if !home.is_empty() {
                    self.detrix_home = Some(PathBuf::from(home));
                }
            }
        }

        // Timeout overrides (values in seconds, e.g. "2.0")
        if self.health_check_timeout == Duration::from_secs(2) {
            if let Some(d) = Self::parse_duration_env("DETRIX_HEALTH_CHECK_TIMEOUT") {
                self.health_check_timeout = d;
            }
        }
        if self.register_timeout == Duration::from_secs(5) {
            if let Some(d) = Self::parse_duration_env("DETRIX_REGISTER_TIMEOUT") {
                self.register_timeout = d;
            }
        }
        if self.unregister_timeout == Duration::from_secs(2) {
            if let Some(d) = Self::parse_duration_env("DETRIX_UNREGISTER_TIMEOUT") {
                self.unregister_timeout = d;
            }
        }

        self
    }

    /// Parse a duration from an environment variable (value in seconds, e.g. "2.0").
    fn parse_duration_env(key: &str) -> Option<Duration> {
        env::var(key)
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .map(Duration::from_secs_f64)
    }

    /// Generate the connection name.
    ///
    /// Returns the configured name or generates one as "detrix-client-{pid}".
    pub fn connection_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("detrix-client-{}", std::process::id()))
    }

    /// Get the detrix home directory.
    ///
    /// Returns the configured path or defaults to ~/detrix.
    pub fn detrix_home_path(&self) -> Option<PathBuf> {
        self.detrix_home
            .clone()
            .or_else(|| dirs::home_dir().map(|home| home.join("detrix")))
    }
}

/// TLS configuration for daemon communication.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Whether to verify TLS certificates (default: true).
    pub verify: bool,
    /// Path to CA bundle file for TLS verification.
    pub ca_bundle: Option<PathBuf>,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            verify: true,
            ca_bundle: None,
        }
    }
}

impl TlsConfig {
    /// Validate the TLS configuration (fail fast on invalid CA bundle).
    ///
    /// # Errors
    ///
    /// Returns `Error::ConfigError` if the CA bundle path is specified but doesn't exist.
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(ref path) = self.ca_bundle {
            if !path.is_file() {
                return Err(Error::ConfigError(format!(
                    "CA bundle not found: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.name.is_none());
        assert_eq!(config.control_host, "127.0.0.1");
        assert_eq!(config.control_port, 0);
        assert_eq!(config.debug_port, 0);
        assert_eq!(config.daemon_url, "http://127.0.0.1:8090");
        assert!(config.lldb_dap_path.is_none());
        assert!(!config.safe_mode);
    }

    #[test]
    fn test_register_timeout_default() {
        let config = Config::default();
        // Changed from 10s to 5s for consistency with Python/Go clients
        assert_eq!(config.register_timeout, Duration::from_secs(5));
    }

    #[test]
    fn test_connection_name_default() {
        let config = Config::default();
        let name = config.connection_name();
        assert!(name.starts_with("detrix-client-"));
    }

    #[test]
    fn test_connection_name_custom() {
        let config = Config {
            name: Some("my-service".to_string()),
            ..Config::default()
        };
        assert_eq!(config.connection_name(), "my-service");
    }

    #[test]
    fn test_detrix_home_path() {
        let config = Config::default();
        if let Some(path) = config.detrix_home_path() {
            assert!(path.ends_with("detrix"));
        }
    }

    #[test]
    fn test_tls_config_default() {
        let config = TlsConfig::default();
        assert!(config.verify);
        assert!(config.ca_bundle.is_none());
    }

    #[test]
    fn test_tls_config_validate_missing_ca_bundle() {
        let config = TlsConfig {
            verify: true,
            ca_bundle: Some(PathBuf::from("/nonexistent/ca.pem")),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_tls_config_validate_no_ca_bundle() {
        let config = TlsConfig::default();
        assert!(config.validate().is_ok());
    }
}
