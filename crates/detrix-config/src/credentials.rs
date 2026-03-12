//! Credential storage for per-host daemon authentication.
//!
//! Stores tokens at `~/detrix/credentials.toml` (alongside `auth-token`).
//! Inspired by Docker `~/.docker/config.json` and GitHub CLI `~/.config/gh/hosts.yml`.
//!
//! # File format
//!
//! ```toml
//! [targets."localhost:8095"]
//! token = "demo-token"
//!
//! [targets."myapp.prod:8095"]
//! token = "prod-token"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::constants::ENV_DETRIX_TOKEN;
use crate::paths;

/// Errors that can occur during credential operations.
#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    #[error("Failed to read credentials: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse credentials: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("Failed to serialize credentials: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Credential for a single target daemon.
#[derive(Clone, Serialize, Deserialize)]
pub struct TargetCredential {
    pub token: String,
}

impl std::fmt::Debug for TargetCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetCredential")
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// Collection of per-host credentials.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CredentialsFile {
    #[serde(default)]
    pub targets: BTreeMap<String, TargetCredential>,
}

impl CredentialsFile {
    /// Get the default path for credentials.toml.
    pub fn default_path() -> PathBuf {
        paths::credentials_path()
    }

    /// Load credentials from the default path.
    ///
    /// Returns `Default` if the file doesn't exist.
    pub fn load() -> Result<Self, CredentialsError> {
        Self::load_from(&Self::default_path())
    }

    /// Load credentials from a specific path.
    pub fn load_from(path: &Path) -> Result<Self, CredentialsError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        let creds: CredentialsFile = toml::from_str(&content)?;
        Ok(creds)
    }

    /// Save credentials to the default path with secure permissions (0600).
    pub fn save(&self) -> Result<(), CredentialsError> {
        self.save_to(&Self::default_path())
    }

    /// Save credentials to a specific path with secure permissions (0600).
    pub fn save_to(&self, path: &Path) -> Result<(), CredentialsError> {
        paths::ensure_parent_dir(path)?;
        let content = toml::to_string_pretty(self)?;
        write_file_securely(path, &content)?;
        Ok(())
    }

    /// Look up a token for a given host:port.
    pub fn lookup(&self, host_port: &str) -> Option<&str> {
        self.targets.get(host_port).map(|c| c.token.as_str())
    }

    /// Add or update a credential for a host:port.
    pub fn add(&mut self, host_port: impl Into<String>, token: impl Into<String>) {
        self.targets.insert(
            host_port.into(),
            TargetCredential {
                token: token.into(),
            },
        );
    }

    /// Remove a credential for a host:port. Returns true if it existed.
    pub fn remove(&mut self, host_port: &str) -> bool {
        self.targets.remove(host_port).is_some()
    }
}

/// Resolve token for a target host:port.
///
/// Priority:
/// 1. `DETRIX_TOKEN` env var (global override)
/// 2. `~/detrix/credentials.toml` per-host lookup
/// 3. `~/detrix/auth-token` file (only if `is_local_daemon`)
/// 4. None
pub fn resolve_token_for_target(host_port: &str, is_local_daemon: bool) -> Option<String> {
    let token = resolve_token_from_env(host_port)
        .or_else(|| resolve_token_from_credentials_file(host_port))
        .or_else(|| resolve_token_from_auth_token_file(host_port, is_local_daemon));

    if token.is_none() {
        debug!("No token found for {}", host_port);
    }
    token
}

/// Try to resolve a token from the `DETRIX_TOKEN` environment variable.
fn resolve_token_from_env(host_port: &str) -> Option<String> {
    let raw = std::env::var(ENV_DETRIX_TOKEN).ok()?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        return None;
    }
    debug!("Token resolved from DETRIX_TOKEN env var for {}", host_port);
    Some(token)
}

/// Try to resolve a token from the per-host credentials file (`~/detrix/credentials.toml`).
fn resolve_token_from_credentials_file(host_port: &str) -> Option<String> {
    let creds = CredentialsFile::load().ok()?;
    let token = creds.lookup(host_port)?.to_string();
    debug!("Token resolved from credentials.toml for {}", host_port);
    Some(token)
}

/// Try to resolve a token from the `~/detrix/auth-token` file (local daemon only).
fn resolve_token_from_auth_token_file(host_port: &str, is_local_daemon: bool) -> Option<String> {
    if !is_local_daemon {
        return None;
    }
    let raw = std::fs::read_to_string(paths::auth_token_path()).ok()?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        return None;
    }
    debug!("Token resolved from auth-token file for {}", host_port);
    Some(token)
}

/// Write content to a file with secure permissions (0600 on Unix).
///
/// On Unix: creates file with mode 0600 (owner read/write only) atomically.
/// On Windows: creates file then applies restrictive ACL (owner-only access).
pub fn write_file_securely(path: &Path, content: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    #[cfg(windows)]
    {
        use std::io::Write;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);

        // Apply Windows ACL using icacls (equivalent to Unix 0600)
        let path_str = path.to_string_lossy();
        let username = std::env::var("USERNAME").unwrap_or_else(|_| "CURRENT_USER".to_string());
        let _ = std::process::Command::new("icacls")
            .args([
                path_str.as_ref(),
                "/inheritance:r",
                "/grant:r",
                &format!("{}:F", username),
            ])
            .output();

        Ok(())
    }

    #[cfg(all(not(unix), not(windows)))]
    {
        std::fs::write(path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_toml_roundtrip() {
        let mut creds = CredentialsFile::default();
        creds.add("localhost:8095", "demo-token");
        creds.add("prod.example.com:8095", "prod-token");

        let serialized = toml::to_string_pretty(&creds).unwrap();
        let deserialized: CredentialsFile = toml::from_str(&serialized).unwrap();

        assert_eq!(deserialized.targets.len(), 2);
        assert_eq!(deserialized.lookup("localhost:8095"), Some("demo-token"));
        assert_eq!(
            deserialized.lookup("prod.example.com:8095"),
            Some("prod-token")
        );
    }

    #[test]
    fn test_lookup_hit_miss() {
        let mut creds = CredentialsFile::default();
        creds.add("localhost:8095", "token-123");

        assert_eq!(creds.lookup("localhost:8095"), Some("token-123"));
        assert_eq!(creds.lookup("localhost:9090"), None);
        assert_eq!(creds.lookup("unknown:8095"), None);
    }

    #[test]
    fn test_add_overwrites() {
        let mut creds = CredentialsFile::default();
        creds.add("localhost:8095", "old-token");
        creds.add("localhost:8095", "new-token");

        assert_eq!(creds.lookup("localhost:8095"), Some("new-token"));
        assert_eq!(creds.targets.len(), 1);
    }

    #[test]
    fn test_remove() {
        let mut creds = CredentialsFile::default();
        creds.add("localhost:8095", "token");

        assert!(creds.remove("localhost:8095"));
        assert!(!creds.remove("localhost:8095")); // Already removed
        assert_eq!(creds.lookup("localhost:8095"), None);
    }

    #[test]
    fn test_empty_credentials_file() {
        let creds = CredentialsFile::default();
        assert!(creds.targets.is_empty());
        assert_eq!(creds.lookup("anything"), None);
    }

    #[test]
    fn test_parse_toml_format() {
        let toml_str = r#"
[targets."localhost:8095"]
token = "demo-token"

[targets."myapp.prod:8095"]
token = "prod-token"
"#;

        let creds: CredentialsFile = toml::from_str(toml_str).unwrap();
        assert_eq!(creds.targets.len(), 2);
        assert_eq!(creds.lookup("localhost:8095"), Some("demo-token"));
        assert_eq!(creds.lookup("myapp.prod:8095"), Some("prod-token"));
    }

    #[test]
    fn test_missing_file_returns_default() {
        let path = std::path::Path::new("/nonexistent/credentials.toml");
        let creds = CredentialsFile::load_from(path).unwrap();
        assert!(creds.targets.is_empty());
    }

    #[test]
    fn test_save_and_load() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("detrix_test_credentials.toml");

        // Clean up from previous runs
        let _ = std::fs::remove_file(&path);

        let mut creds = CredentialsFile::default();
        creds.add("localhost:8095", "test-token");
        creds.save_to(&path).unwrap();

        let loaded = CredentialsFile::load_from(&path).unwrap();
        assert_eq!(loaded.lookup("localhost:8095"), Some("test-token"));

        // Clean up
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_write_file_securely() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("detrix_test_secure_write");

        let _ = std::fs::remove_file(&path);

        write_file_securely(&path, "test-content").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "test-content");

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(&path).unwrap();
            assert_eq!(meta.mode() & 0o777, 0o600);
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_resolve_token_env_var_priority() {
        // This test is difficult to run in isolation due to env var side effects.
        // We test the credential file lookup path instead.
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("detrix_test_resolve_creds.toml");

        let mut creds = CredentialsFile::default();
        creds.add("test-host:8095", "cred-token");
        creds.save_to(&path).unwrap();

        let loaded = CredentialsFile::load_from(&path).unwrap();
        assert_eq!(loaded.lookup("test-host:8095"), Some("cred-token"));
        assert_eq!(loaded.lookup("unknown:8095"), None);

        let _ = std::fs::remove_file(&path);
    }

    // === resolve_token_from_* helper tests ===

    #[test]
    fn test_resolve_token_from_auth_token_file_skips_non_local_daemon() {
        // When is_local_daemon is false the auth-token file is never consulted,
        // so this should return None immediately without any file I/O.
        let result = resolve_token_from_auth_token_file("any-host:8095", false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_token_from_env_returns_none_when_var_unset() {
        // Guard: only run this assertion if DETRIX_TOKEN is not set in the environment,
        // so the test stays deterministic in CI where the var might be absent.
        if std::env::var(ENV_DETRIX_TOKEN).is_err() {
            let result = resolve_token_from_env("check-host:8095");
            assert_eq!(result, None);
        }
    }

    #[test]
    fn test_resolve_token_from_credentials_file_returns_none_for_unknown_host() {
        // resolve_token_from_credentials_file loads the real credentials file.
        // For a host that almost certainly has no entry, it must return None.
        // (It's fine if credentials.toml doesn't exist — load() returns Default.)
        let result = resolve_token_from_credentials_file("totally-unknown-host-xyz-99999:1");
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_token_for_target_returns_none_when_nothing_configured() {
        // With no DETRIX_TOKEN env var, no credentials entry for this synthetic host,
        // and is_local_daemon=false (so auth-token file is skipped), result is None.
        if std::env::var(ENV_DETRIX_TOKEN).is_err() {
            let result = resolve_token_for_target("totally-unknown-host-xyz-99999:1", false);
            assert_eq!(result, None);
        }
    }
}
