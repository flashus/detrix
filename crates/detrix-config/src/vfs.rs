//! VFS (Virtual File System) configuration
//!
//! Controls how the server resolves source files in cloud/Docker deployments
//! where files may not be on the server's local disk.
//!
//! # Configuration
//!
//! ```toml
//! [vfs]
//! source_priority = ["control_plane", "disk"]  # File source chain priority
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

use crate::constants::{DEFAULT_VFS_FETCH_TIMEOUT_SECONDS, DEFAULT_VFS_MAX_FILE_SIZE_BYTES};

/// VFS configuration for transparent file sourcing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsConfig {
    /// File source priority (first success wins, after VFS cache check).
    /// Options: "control_plane", "bridge", "disk"
    #[serde(default = "default_source_priority")]
    pub source_priority: Vec<String>,

    /// HTTP fetch timeout in seconds (for control_plane and bridge sources)
    #[serde(default = "default_fetch_timeout_seconds")]
    pub fetch_timeout_seconds: u64,

    /// Maximum file size in bytes that can be fetched/cached
    #[serde(default = "default_max_file_size_bytes")]
    pub max_file_size_bytes: usize,
}

fn default_source_priority() -> Vec<String> {
    vec!["control_plane".into(), "disk".into()]
}

fn default_fetch_timeout_seconds() -> u64 {
    DEFAULT_VFS_FETCH_TIMEOUT_SECONDS
}

fn default_max_file_size_bytes() -> usize {
    DEFAULT_VFS_MAX_FILE_SIZE_BYTES
}

impl Default for VfsConfig {
    fn default() -> Self {
        Self {
            source_priority: default_source_priority(),
            fetch_timeout_seconds: default_fetch_timeout_seconds(),
            max_file_size_bytes: default_max_file_size_bytes(),
        }
    }
}
