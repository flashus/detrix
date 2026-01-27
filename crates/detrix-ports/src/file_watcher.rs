//! File Watcher Port
//!
//! Defines the interface for file system change notifications.
//! Used by the anchor system to detect when source files change,
//! triggering metric relocation.
//!
//! # Architecture
//!
//! The FileWatcher is responsible for:
//! 1. **Watch**: Registering files/directories for change monitoring
//! 2. **Notify**: Emitting events when files are modified/renamed/deleted
//! 3. **Debounce**: Coalescing rapid changes to prevent event storms
//!
//! # Event Flow
//!
//! ```text
//! File Modified
//!     |
//! Debounce (500ms default)
//!     |
//! FileEvent::Modified(path)
//!     |
//! MetricService::handle_file_change
//!     |
//! Relocate affected metrics
//! ```

use async_trait::async_trait;
use detrix_config::constants::{
    DEFAULT_FILE_CHANGE_DEBOUNCE_MS, DEFAULT_FILE_WATCHER_CHANNEL_SIZE,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Events emitted by the file watcher
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEvent {
    /// File content was modified
    Modified(PathBuf),
    /// File was deleted
    Deleted(PathBuf),
    /// File was renamed (from, to)
    Renamed { from: PathBuf, to: PathBuf },
    /// File was created
    Created(PathBuf),
}

impl FileEvent {
    /// Get the primary path affected by this event
    pub fn path(&self) -> &PathBuf {
        match self {
            FileEvent::Modified(p) => p,
            FileEvent::Deleted(p) => p,
            FileEvent::Renamed { to, .. } => to,
            FileEvent::Created(p) => p,
        }
    }

    /// Check if this event affects a specific file
    pub fn affects(&self, path: &Path) -> bool {
        match self {
            FileEvent::Modified(p) => p == path,
            FileEvent::Deleted(p) => p == path,
            FileEvent::Renamed { from, to } => from == path || to == path,
            FileEvent::Created(p) => p == path,
        }
    }
}

/// Configuration for file watching
#[derive(Debug, Clone)]
pub struct FileWatcherConfig {
    /// Debounce duration in milliseconds (default: 500ms)
    pub debounce_ms: u64,
    /// Whether to watch recursively (default: true)
    pub recursive: bool,
    /// Channel buffer size for file events (default: 256)
    pub channel_size: usize,
}

impl Default for FileWatcherConfig {
    fn default() -> Self {
        Self {
            debounce_ms: DEFAULT_FILE_CHANGE_DEBOUNCE_MS,
            recursive: true,
            channel_size: DEFAULT_FILE_WATCHER_CHANNEL_SIZE,
        }
    }
}

/// Service for watching file system changes
///
/// Implementations should use platform-native file system notification
/// APIs (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows).
#[async_trait]
pub trait FileWatcher: Send + Sync {
    /// Start watching a file or directory
    ///
    /// # Arguments
    /// * `path` - File or directory to watch
    ///
    /// # Returns
    /// Ok(()) if watch was added, Err if path doesn't exist or watch failed
    async fn watch(&self, path: PathBuf) -> detrix_core::Result<()>;

    /// Stop watching a file or directory
    ///
    /// # Arguments
    /// * `path` - File or directory to stop watching
    ///
    /// # Returns
    /// Ok(()) if watch was removed (or didn't exist), Err on failure
    async fn unwatch(&self, path: PathBuf) -> detrix_core::Result<()>;

    /// Get channel receiver for file events
    ///
    /// Returns a receiver that will emit FileEvents when watched files change.
    /// The channel is bounded to prevent memory issues from event storms.
    fn subscribe(&self) -> mpsc::Receiver<FileEvent>;

    /// Check if a path is currently being watched
    fn is_watching(&self, path: &Path) -> bool;

    /// Get all currently watched paths
    fn watched_paths(&self) -> Vec<PathBuf>;

    /// Stop all watches and shutdown the watcher
    async fn shutdown(&self) -> detrix_core::Result<()>;
}

/// Thread-safe reference to a file watcher
pub type FileWatcherRef = Arc<dyn FileWatcher + Send + Sync>;

/// A null file watcher that does nothing (for testing or when disabled)
pub struct NullFileWatcher;

#[async_trait]
impl FileWatcher for NullFileWatcher {
    async fn watch(&self, _path: PathBuf) -> detrix_core::Result<()> {
        Ok(())
    }

    async fn unwatch(&self, _path: PathBuf) -> detrix_core::Result<()> {
        Ok(())
    }

    fn subscribe(&self) -> mpsc::Receiver<FileEvent> {
        let (_tx, rx) = mpsc::channel(1);
        rx
    }

    fn is_watching(&self, _path: &Path) -> bool {
        false
    }

    fn watched_paths(&self) -> Vec<PathBuf> {
        vec![]
    }

    async fn shutdown(&self) -> detrix_core::Result<()> {
        Ok(())
    }
}
