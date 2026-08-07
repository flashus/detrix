//! Agent mode configuration
//!
//! Contains settings for both the agent binary and the server-side
//! agent connection management.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Agent mode configuration.
///
/// Required under `[agent]` in detrix.toml when running `detrix agent start`.
/// The server reads `agent_tokens` and `min_compatible_agent_version` from its own detrix.toml.
/// All other fields are agent-only and are ignored by the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    // ── Agent-side ───────────────────────────────────────────────────────
    /// gRPC URL of the centralised detrix server.
    #[serde(default)]
    pub server_grpc_url: String,

    /// File containing the bearer token to present to the server.
    #[serde(default)]
    pub token_file: Option<PathBuf>,

    /// File where the agent persists its generated ID across restarts.
    #[serde(default = "default_agent_id_file")]
    pub agent_id_file: PathBuf,

    /// Initial reconnect interval in seconds (exponential backoff base).
    #[serde(default = "default_reconnect_interval")]
    pub reconnect_interval_secs: u64,

    /// Maximum reconnect interval in seconds.
    #[serde(default = "default_reconnect_max_interval")]
    pub reconnect_max_interval_secs: u64,

    /// Heartbeat send interval in seconds.
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_secs: u64,

    /// Bind host for the Prometheus /metrics and /health endpoints.
    /// Defaults to "127.0.0.1" (loopback only). Set to "0.0.0.0" when
    /// a Prometheus scraper runs in a separate container or host.
    #[serde(default = "default_metrics_host")]
    pub metrics_host: String,

    /// HTTP port for the Prometheus /metrics and /health endpoints.
    #[serde(default = "default_metrics_port")]
    pub metrics_port: u16,

    /// Verify the server's TLS certificate against the system CA bundle.
    /// Set to false only for development with self-signed certificates.
    #[serde(default = "default_verify_tls")]
    pub verify_tls: bool,

    /// Optional path to a PEM CA certificate for self-signed server certificates.
    #[serde(default)]
    pub ca_cert_file: Option<PathBuf>,

    /// /proc binary scanner configuration.
    #[serde(default)]
    pub scanner: ScannerConfig,

    // ── Server-side (in the server's detrix.toml) ────────────────────────
    /// SHA-256 hashes of valid agent bearer tokens.
    /// Generate: echo -n "my-secret-token" | sha256sum
    /// TOML key is `agent_tokens` (legacy); `agent_token_hashes` is also accepted.
    #[serde(default, rename = "agent_tokens", alias = "agent_token_hashes")]
    pub agent_token_hashes: Vec<String>,

    /// Minimum agent semver this server accepts (e.g. "1.3.0").
    /// Agents with a lower minor version receive RegisterAck { accepted: false }.
    #[serde(default)]
    pub min_compatible_agent_version: Option<String>,

    /// Allow agent connections without authentication.
    /// DANGEROUS: only set true in isolated development environments.
    /// When false (the default) and agent_tokens is empty, all agent connections are rejected.
    #[serde(default)]
    pub dev_mode: bool,
}

/// /proc binary scanner configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerConfig {
    /// How often to scan /proc for new/changed binaries (seconds).
    #[serde(default = "default_scan_interval")]
    pub scan_interval_secs: u64,

    /// Glob patterns — only binaries matching at least one are reported.
    /// Empty = match all.
    #[serde(default)]
    pub include_patterns: Vec<String>,

    /// Glob patterns — matching binaries are excluded.
    #[serde(default)]
    pub exclude_patterns: Vec<String>,

    /// Only report binaries that contain .debug_info (DWARF). Recommended true —
    /// binaries without DWARF cannot be observed and would create unusable connections.
    #[serde(default = "default_require_dwarf")]
    pub require_dwarf: bool,

    /// Restrict server-requested file reads to these directory prefixes.
    /// The agent canonicalises the requested path and rejects anything outside
    /// these directories, preventing a compromised server from reading arbitrary files.
    ///
    /// Empty = allow any path only when `agent.dev_mode = true`; agent start rejects
    /// this configuration otherwise. Configure explicit prefixes in production.
    #[serde(default)]
    pub allowed_read_prefixes: Vec<PathBuf>,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        ScannerConfig {
            scan_interval_secs: default_scan_interval(),
            include_patterns: Vec::new(),
            exclude_patterns: Vec::new(),
            require_dwarf: default_require_dwarf(),
            allowed_read_prefixes: Vec::new(),
        }
    }
}

fn default_agent_id_file() -> PathBuf {
    PathBuf::from("/var/lib/detrix/agent-id")
}
fn default_reconnect_interval() -> u64 {
    5
}
fn default_reconnect_max_interval() -> u64 {
    60
}
fn default_heartbeat_interval() -> u64 {
    30
}
fn default_metrics_host() -> String {
    "127.0.0.1".to_string()
}
fn default_metrics_port() -> u16 {
    9091
}
fn default_verify_tls() -> bool {
    true
}
fn default_scan_interval() -> u64 {
    30
}
fn default_require_dwarf() -> bool {
    true
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            server_grpc_url: String::new(),
            token_file: None,
            agent_id_file: default_agent_id_file(),
            reconnect_interval_secs: default_reconnect_interval(),
            reconnect_max_interval_secs: default_reconnect_max_interval(),
            heartbeat_interval_secs: default_heartbeat_interval(),
            metrics_host: default_metrics_host(),
            metrics_port: default_metrics_port(),
            verify_tls: default_verify_tls(),
            ca_cert_file: None,
            scanner: ScannerConfig::default(),
            agent_token_hashes: Vec::new(),
            min_compatible_agent_version: None,
            dev_mode: false,
        }
    }
}

impl AgentConfig {
    /// Validate agent-mode required fields. Called by `detrix agent start` before connecting.
    pub fn validate_for_agent(&self) -> Result<(), String> {
        if self.server_grpc_url.is_empty() {
            return Err("agent.server_grpc_url must be set".to_string());
        }
        let url = &self.server_grpc_url;
        if !url.starts_with("http://")
            && !url.starts_with("https://")
            && !url.starts_with("grpc://")
            && !url.starts_with("grpcs://")
        {
            return Err(format!(
                "agent.server_grpc_url has unrecognized scheme (expected http://, https://, grpc://, or grpcs://): {url}"
            ));
        }
        if let Some(ref path) = self.token_file {
            std::fs::metadata(path).map_err(|e| {
                format!(
                    "agent.token_file '{}' not found or not readable: {e}",
                    path.display()
                )
            })?;
        }
        if self.scanner.allowed_read_prefixes.is_empty() && !self.dev_mode {
            return Err(
                "agent.scanner.allowed_read_prefixes must contain at least one directory "
                    .to_string()
                    + "for non-development agent deployments; set agent.dev_mode=true only "
                    + "in an isolated test environment",
            );
        }
        Ok(())
    }

    /// Validate server-side agent configuration constraints.
    /// Called during config load alongside other section validators.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.reconnect_interval_secs > self.reconnect_max_interval_secs {
            errors.push(format!(
                "agent.reconnect_interval_secs ({}) must be ≤ reconnect_max_interval_secs ({})",
                self.reconnect_interval_secs, self.reconnect_max_interval_secs
            ));
        }
        errors
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_config_default_is_valid_for_server() {
        let config = AgentConfig::default();
        assert!(config.agent_token_hashes.is_empty());
        assert!(config.min_compatible_agent_version.is_none());
        assert!(config.server_grpc_url.is_empty());
    }

    #[test]
    fn test_validate_for_agent_fails_on_empty_url() {
        let config = AgentConfig::default();
        let result = config.validate_for_agent();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "agent.server_grpc_url must be set");
    }

    #[test]
    fn test_validate_for_agent_fails_on_missing_token_file() {
        let config = AgentConfig {
            server_grpc_url: "http://localhost:50061".to_string(),
            token_file: Some(PathBuf::from("/nonexistent/token")),
            ..Default::default()
        };
        let result = config.validate_for_agent();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found or not readable"));
    }

    #[test]
    fn test_validate_for_agent_passes_with_valid_config() {
        let token_path = std::env::temp_dir().join("detrix-test-token");
        std::fs::write(&token_path, "test-token").unwrap();

        let config = AgentConfig {
            server_grpc_url: "http://localhost:50061".to_string(),
            token_file: Some(token_path.clone()),
            scanner: ScannerConfig {
                allowed_read_prefixes: vec![PathBuf::from("/tmp")],
                ..Default::default()
            },
            ..Default::default()
        };
        let result = config.validate_for_agent();
        assert!(result.is_ok());

        std::fs::remove_file(&token_path).ok();
    }

    #[test]
    fn test_validate_for_agent_passes_without_token_file() {
        let config = AgentConfig {
            server_grpc_url: "http://localhost:50061".to_string(),
            token_file: None,
            scanner: ScannerConfig {
                allowed_read_prefixes: vec![PathBuf::from("/tmp")],
                ..Default::default()
            },
            ..Default::default()
        };
        let result = config.validate_for_agent();
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_for_agent_accepts_grpcs_scheme() {
        let config = AgentConfig {
            server_grpc_url: "grpcs://server:50061".to_string(),
            scanner: ScannerConfig {
                allowed_read_prefixes: vec![PathBuf::from("/tmp")],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(config.validate_for_agent().is_ok());
    }

    #[test]
    fn test_validate_for_agent_accepts_grpc_scheme() {
        let config = AgentConfig {
            server_grpc_url: "grpc://server:50061".to_string(),
            scanner: ScannerConfig {
                allowed_read_prefixes: vec![PathBuf::from("/tmp")],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(config.validate_for_agent().is_ok());
    }

    #[test]
    fn test_validate_for_agent_rejects_unknown_scheme() {
        let config = AgentConfig {
            server_grpc_url: "ftp://server:50061".to_string(),
            ..Default::default()
        };
        let result = config.validate_for_agent();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unrecognized scheme"));
    }

    #[test]
    fn test_validate_for_agent_rejects_bare_hostname() {
        let config = AgentConfig {
            server_grpc_url: "server:50061".to_string(),
            ..Default::default()
        };
        let result = config.validate_for_agent();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unrecognized scheme"));
    }

    #[test]
    fn test_validate_for_agent_rejects_unrestricted_reads() {
        let config = AgentConfig {
            server_grpc_url: "https://detrix-server:50061".to_string(),
            ..Default::default()
        };
        let result = config.validate_for_agent();
        assert!(result
            .unwrap_err()
            .contains("allowed_read_prefixes must contain"));
    }

    #[test]
    fn test_validate_for_agent_allows_unrestricted_reads_only_in_dev_mode() {
        let config = AgentConfig {
            server_grpc_url: "http://detrix-server:50061".to_string(),
            dev_mode: true,
            ..Default::default()
        };
        assert!(config.validate_for_agent().is_ok());
    }

    #[test]
    fn test_scanner_config_defaults() {
        let scanner = ScannerConfig::default();
        assert_eq!(scanner.scan_interval_secs, 30);
        assert!(scanner.include_patterns.is_empty());
        assert!(scanner.exclude_patterns.is_empty());
        assert!(scanner.require_dwarf);
    }

    #[test]
    fn test_agent_config_serialization_roundtrip() {
        let config = AgentConfig {
            server_grpc_url: "https://detrix-server:50061".to_string(),
            token_file: Some(PathBuf::from("/etc/detrix/agent-token")),
            verify_tls: true,
            scanner: ScannerConfig {
                scan_interval_secs: 30,
                include_patterns: vec!["/app/*".to_string()],
                exclude_patterns: vec![],
                require_dwarf: true,
                allowed_read_prefixes: vec![],
            },
            agent_token_hashes: vec!["abc123".to_string()],
            min_compatible_agent_version: Some("1.3.0".to_string()),
            ..Default::default()
        };

        let toml_str = toml::to_string(&config).unwrap();
        let parsed: AgentConfig = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.server_grpc_url, config.server_grpc_url);
        assert_eq!(parsed.token_file, config.token_file);
        assert_eq!(parsed.verify_tls, config.verify_tls);
        assert_eq!(
            parsed.scanner.include_patterns,
            config.scanner.include_patterns
        );
        assert_eq!(parsed.agent_token_hashes, config.agent_token_hashes);
        assert_eq!(
            parsed.min_compatible_agent_version,
            config.min_compatible_agent_version
        );
    }
}
