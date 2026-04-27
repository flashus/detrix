//! VFS (Virtual File System) configuration
//!
//! Controls how the server resolves source files in cloud/Docker deployments
//! where files may not be on the server's local disk.
//!
//! # Configuration
//!
//! ```toml
//! [vfs]
//! source_priority = ["agent", "control_plane", "disk"]  # File source chain priority
//! fetch_timeout_seconds = 10                    # HTTP fetch timeout
//! max_file_size_bytes = 10485760                # 10MB max file size
//! ```
//!
//! # Source Priority
//!
//! The server tries each source in order until one succeeds:
//! - `"control_plane"` — fetch from app's control plane URL (`/detrix/files/read`)
//! - `"bridge"` — fetch from MCP bridge's file server (supports git-pinned serving)
//! - `"disk"` — read from local filesystem

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::constants::{
    DEFAULT_VFS_FETCH_TIMEOUT_SECONDS, DEFAULT_VFS_HOT_RELOAD_TTL_SECONDS,
    DEFAULT_VFS_MAX_FILE_SIZE_BYTES,
};

/// File source kind for the VFS source chain.
///
/// Defines the available file source providers. Used in `source_priority`
/// to configure the fetch order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Fetch from app's control plane URL (`/detrix/files/read`)
    ControlPlane,
    /// Fetch from MCP bridge's file server (supports git-pinned serving)
    Bridge,
    /// Fetch from agent (remote file access via gRPC stream)
    Agent,
    /// Read from local filesystem
    Disk,
}

impl SourceKind {
    /// Get the string representation (matches `FileSource::name()` values).
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::ControlPlane => "control_plane",
            SourceKind::Bridge => "bridge",
            SourceKind::Agent => "agent",
            SourceKind::Disk => "disk",
        }
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// VFS configuration for transparent file sourcing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsConfig {
    /// File source priority (first success wins, after VFS cache check).
    #[serde(default = "default_source_priority")]
    pub source_priority: Vec<SourceKind>,

    /// HTTP fetch timeout in seconds (for control_plane and bridge sources)
    #[serde(default = "default_fetch_timeout_seconds")]
    pub fetch_timeout_seconds: u64,

    /// Maximum file size in bytes that can be fetched/cached
    #[serde(default = "default_max_file_size_bytes")]
    pub max_file_size_bytes: usize,

    /// Hot-reload TTL in seconds — cached files are re-read from source after this duration
    #[serde(default = "default_hot_reload_ttl_seconds")]
    pub hot_reload_ttl_seconds: u64,
}

fn default_source_priority() -> Vec<SourceKind> {
    vec![
        SourceKind::Agent,
        SourceKind::ControlPlane,
        SourceKind::Disk,
    ]
}

fn default_fetch_timeout_seconds() -> u64 {
    DEFAULT_VFS_FETCH_TIMEOUT_SECONDS
}

fn default_max_file_size_bytes() -> usize {
    DEFAULT_VFS_MAX_FILE_SIZE_BYTES
}

fn default_hot_reload_ttl_seconds() -> u64 {
    DEFAULT_VFS_HOT_RELOAD_TTL_SECONDS
}

impl Default for VfsConfig {
    fn default() -> Self {
        Self {
            source_priority: default_source_priority(),
            fetch_timeout_seconds: default_fetch_timeout_seconds(),
            max_file_size_bytes: default_max_file_size_bytes(),
            hot_reload_ttl_seconds: default_hot_reload_ttl_seconds(),
        }
    }
}
