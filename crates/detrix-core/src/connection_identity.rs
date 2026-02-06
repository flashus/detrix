//! Connection identity value object for stable UUID generation
//!
//! This module provides a deterministic UUID generation system that ensures:
//! - Same workspace + language + name → same UUID (stable across restarts)
//! - Different workspace/language → different UUID (isolation)
//! - IDE-like behavior (process restart preserves metrics)

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Connection identity components for stable UUID generation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionIdentity {
    /// User-provided connection name (e.g., "trade-bot")
    pub name: String,
    /// Programming language ("python", "go", "rust")
    pub language: String,
    /// Absolute path to workspace directory
    pub workspace_root: String,
    /// Machine hostname for multi-host isolation
    pub hostname: String,
}

impl ConnectionIdentity {
    /// Create a new connection identity
    pub fn new(
        name: impl Into<String>,
        language: impl Into<String>,
        workspace_root: impl Into<String>,
        hostname: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            language: language.into(),
            workspace_root: workspace_root.into(),
            hostname: hostname.into(),
        }
    }

    /// Generate deterministic UUID from identity components
    ///
    /// Formula: SHA256(name|language|workspace_root|hostname)[0..16]
    /// Returns 32-character hex string (128-bit UUID)
    ///
    /// # Examples
    ///
    /// ```
    /// use detrix_core::ConnectionIdentity;
    ///
    /// let id1 = ConnectionIdentity::new("app", "python", "/workspace", "host1");
    /// let id2 = ConnectionIdentity::new("app", "python", "/workspace", "host1");
    /// assert_eq!(id1.to_uuid(), id2.to_uuid()); // Deterministic
    ///
    /// let id3 = ConnectionIdentity::new("app", "python", "/other", "host1");
    /// assert_ne!(id1.to_uuid(), id3.to_uuid()); // Different workspace
    /// ```
    pub fn to_uuid(&self) -> String {
        let input = format!(
            "{}|{}|{}|{}",
            self.name, self.language, self.workspace_root, self.hostname
        );
        let hash = Sha256::digest(input.as_bytes());
        hex::encode(&hash[0..16]) // 128-bit UUID
    }

    /// Validate identity components
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("Connection name cannot be empty".to_string());
        }
        if self.language.is_empty() {
            return Err("Language cannot be empty".to_string());
        }
        if self.workspace_root.is_empty() {
            return Err("Workspace root cannot be empty".to_string());
        }
        if self.hostname.is_empty() {
            return Err("Hostname cannot be empty".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uuid_deterministic() {
        let id1 = ConnectionIdentity::new("app", "go", "/workspace", "host1");
        let id2 = ConnectionIdentity::new("app", "go", "/workspace", "host1");
        assert_eq!(id1.to_uuid(), id2.to_uuid());
    }

    #[test]
    fn test_different_workspace_different_uuid() {
        let id1 = ConnectionIdentity::new("app", "python", "/dir1", "host").to_uuid();
        let id2 = ConnectionIdentity::new("app", "python", "/dir2", "host").to_uuid();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_different_language_different_uuid() {
        let id1 = ConnectionIdentity::new("app", "go", "/workspace", "host").to_uuid();
        let id2 = ConnectionIdentity::new("app", "rust", "/workspace", "host").to_uuid();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_different_name_different_uuid() {
        let id1 = ConnectionIdentity::new("app1", "python", "/workspace", "host").to_uuid();
        let id2 = ConnectionIdentity::new("app2", "python", "/workspace", "host").to_uuid();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_different_hostname_different_uuid() {
        let id1 = ConnectionIdentity::new("app", "python", "/workspace", "host1").to_uuid();
        let id2 = ConnectionIdentity::new("app", "python", "/workspace", "host2").to_uuid();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_uuid_is_32_chars() {
        let identity = ConnectionIdentity::new("app", "python", "/workspace", "host");
        let uuid = identity.to_uuid();
        assert_eq!(uuid.len(), 32); // 16 bytes * 2 hex chars
    }

    #[test]
    fn test_uuid_is_hex() {
        let identity = ConnectionIdentity::new("app", "python", "/workspace", "host");
        let uuid = identity.to_uuid();
        assert!(uuid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_validate_empty_name() {
        let identity = ConnectionIdentity::new("", "python", "/workspace", "host");
        assert!(identity.validate().is_err());
    }

    #[test]
    fn test_validate_empty_language() {
        let identity = ConnectionIdentity::new("app", "", "/workspace", "host");
        assert!(identity.validate().is_err());
    }

    #[test]
    fn test_validate_empty_workspace() {
        let identity = ConnectionIdentity::new("app", "python", "", "host");
        assert!(identity.validate().is_err());
    }

    #[test]
    fn test_validate_empty_hostname() {
        let identity = ConnectionIdentity::new("app", "python", "/workspace", "");
        assert!(identity.validate().is_err());
    }

    #[test]
    fn test_validate_success() {
        let identity = ConnectionIdentity::new("app", "python", "/workspace", "host");
        assert!(identity.validate().is_ok());
    }
}
