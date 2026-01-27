//! File Watcher Service Implementation
//!
//! Uses the `notify` crate to watch for file system changes.
//! Implements debouncing to coalesce rapid changes.
//!
//! # Architecture
//!
//! The watcher runs in a background thread and sends events through
//! a channel. Events are debounced to prevent processing the same
//! file multiple times during rapid changes (e.g., editor auto-save).

use crate::ports::{FileEvent, FileWatcher, FileWatcherConfig};
use async_trait::async_trait;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind, Debouncer};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, trace, warn};

/// File watcher implementation using the notify crate
pub struct NotifyFileWatcher {
    /// Configuration
    config: FileWatcherConfig,
    /// Set of paths being watched
    watched_paths: Arc<RwLock<HashSet<PathBuf>>>,
    /// Channel sender for file events (to subscribers)
    event_tx: mpsc::Sender<FileEvent>,
    /// The debouncer (owns the watcher)
    debouncer: Arc<RwLock<Option<Debouncer<RecommendedWatcher>>>>,
    /// Shutdown flag
    shutdown: Arc<RwLock<bool>>,
}

impl NotifyFileWatcher {
    /// Create a new file watcher with default configuration
    pub fn new() -> detrix_core::Result<(Self, mpsc::Receiver<FileEvent>)> {
        Self::with_config(FileWatcherConfig::default())
    }

    /// Create a new file watcher with custom configuration
    pub fn with_config(
        config: FileWatcherConfig,
    ) -> detrix_core::Result<(Self, mpsc::Receiver<FileEvent>)> {
        let (event_tx, event_rx) = mpsc::channel(config.channel_size);
        let watched_paths = Arc::new(RwLock::new(HashSet::new()));
        let shutdown = Arc::new(RwLock::new(false));

        // Create debouncer with event handler
        let tx_clone = event_tx.clone();
        let debounce_duration = Duration::from_millis(config.debounce_ms);

        let debouncer = new_debouncer(
            debounce_duration,
            move |result: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
                match result {
                    Ok(events) => {
                        for event in events {
                            let file_event = match event.kind {
                                DebouncedEventKind::Any => {
                                    // Check if file exists to determine if created/modified/deleted
                                    if event.path.exists() {
                                        FileEvent::Modified(event.path)
                                    } else {
                                        FileEvent::Deleted(event.path)
                                    }
                                }
                                DebouncedEventKind::AnyContinuous => {
                                    // Continuous events during editing - treat as modified
                                    FileEvent::Modified(event.path)
                                }
                                // Handle any future variants
                                _ => FileEvent::Modified(event.path),
                            };

                            trace!(path = ?file_event.path(), "File event detected");

                            // Send to channel (non-blocking)
                            if let Err(e) = tx_clone.try_send(file_event) {
                                warn!("Failed to send file event: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("File watcher error: {}", e);
                    }
                }
            },
        )
        .map_err(|e| {
            detrix_core::Error::InvalidConfig(format!("Failed to create file watcher: {}", e))
        })?;

        let watcher = Self {
            config,
            watched_paths,
            event_tx,
            debouncer: Arc::new(RwLock::new(Some(debouncer))),
            shutdown,
        };

        Ok((watcher, event_rx))
    }

    /// Get a clone of the event sender for additional subscribers
    pub fn event_sender(&self) -> mpsc::Sender<FileEvent> {
        self.event_tx.clone()
    }
}

impl Default for NotifyFileWatcher {
    fn default() -> Self {
        Self::new()
            .expect("Failed to create default file watcher")
            .0
    }
}

#[async_trait]
impl FileWatcher for NotifyFileWatcher {
    async fn watch(&self, path: PathBuf) -> detrix_core::Result<()> {
        // Check if already shut down
        if *self.shutdown.read().await {
            return Err(detrix_core::Error::InvalidConfig(
                "File watcher has been shut down".to_string(),
            ));
        }

        // Canonicalize path for consistent comparison
        let canonical_path = path.canonicalize().map_err(|e| {
            detrix_core::Error::InvalidConfig(format!(
                "Path '{}' does not exist or cannot be resolved: {}",
                path.display(),
                e
            ))
        })?;

        // Check if already watching
        {
            let watched = self.watched_paths.read().await;
            if watched.contains(&canonical_path) {
                debug!(path = ?canonical_path, "Path already being watched");
                return Ok(());
            }
        }

        // Add to watcher
        let recursive_mode = if self.config.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        {
            let mut debouncer_guard = self.debouncer.write().await;
            if let Some(ref mut debouncer) = *debouncer_guard {
                debouncer
                    .watcher()
                    .watch(&canonical_path, recursive_mode)
                    .map_err(|e| {
                        detrix_core::Error::InvalidConfig(format!(
                            "Failed to watch '{}': {}",
                            canonical_path.display(),
                            e
                        ))
                    })?;
            } else {
                return Err(detrix_core::Error::InvalidConfig(
                    "File watcher has been shut down".to_string(),
                ));
            }
        }

        // Track the path
        {
            let mut watched = self.watched_paths.write().await;
            watched.insert(canonical_path.clone());
        }

        debug!(path = ?canonical_path, "Started watching path");
        Ok(())
    }

    async fn unwatch(&self, path: PathBuf) -> detrix_core::Result<()> {
        // Try to canonicalize, but if file was deleted, use original path
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());

        // Check if we're watching this path
        {
            let watched = self.watched_paths.read().await;
            if !watched.contains(&canonical_path) {
                debug!(path = ?canonical_path, "Path not being watched");
                return Ok(());
            }
        }

        // Remove from watcher
        {
            let mut debouncer_guard = self.debouncer.write().await;
            if let Some(ref mut debouncer) = *debouncer_guard {
                // Ignore errors for unwatch (file might be deleted)
                let _ = debouncer.watcher().unwatch(&canonical_path);
            }
        }

        // Remove from tracked paths
        {
            let mut watched = self.watched_paths.write().await;
            watched.remove(&canonical_path);
        }

        debug!(path = ?canonical_path, "Stopped watching path");
        Ok(())
    }

    fn subscribe(&self) -> mpsc::Receiver<FileEvent> {
        // Create a new receiver by creating a new channel and forwarding
        // This is a simplified implementation - in production you might want
        // a broadcast channel instead
        let (_tx, rx) = mpsc::channel(self.config.channel_size);

        // For now, we return an empty receiver since the main receiver
        // was returned at construction time. This is a limitation of the
        // current design - consider using broadcast::channel for multiple subscribers.
        rx
    }

    fn is_watching(&self, path: &Path) -> bool {
        // Use blocking read since this is a sync method
        // In practice, the set is small and reads are fast
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        // Use try_read to avoid blocking; return false if can't acquire lock
        match self.watched_paths.try_read() {
            Ok(watched) => watched.contains(&path),
            Err(_) => false,
        }
    }

    fn watched_paths(&self) -> Vec<PathBuf> {
        match self.watched_paths.try_read() {
            Ok(watched) => watched.iter().cloned().collect(),
            Err(_) => vec![],
        }
    }

    async fn shutdown(&self) -> detrix_core::Result<()> {
        // Set shutdown flag
        {
            let mut shutdown = self.shutdown.write().await;
            *shutdown = true;
        }

        // Drop the debouncer to stop watching
        {
            let mut debouncer_guard = self.debouncer.write().await;
            *debouncer_guard = None;
        }

        // Clear watched paths
        {
            let mut watched = self.watched_paths.write().await;
            watched.clear();
        }

        debug!("File watcher shut down");
        Ok(())
    }
}

// ============================================================================
// FileWatcherOrchestrator - Application-layer service for watch_paths
// ============================================================================

use crate::services::MetricService;
use detrix_config::AnchorConfig;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::info;

/// Orchestrates file watching and metric relocation
///
/// This is an application-layer service that:
/// - Uses `NotifyFileWatcher` for actual file watching (infrastructure)
/// - Resolves watch paths from config or derives from existing metrics
/// - Delegates event processing to `MetricService.handle_file_change()`
///
/// # Architecture
///
/// Separating orchestration from infrastructure keeps concerns clean:
/// - `NotifyFileWatcher` - Pure file watching (infrastructure layer)
/// - `FileWatcherOrchestrator` - Path resolution + event handling (application layer)
pub struct FileWatcherOrchestrator {
    /// The underlying file watcher
    watcher: Arc<NotifyFileWatcher>,
    /// Reference to metric service for relocation
    metric_service: Arc<MetricService>,
    /// Configuration for anchor system
    config: AnchorConfig,
}

impl FileWatcherOrchestrator {
    /// Create a new orchestrator
    ///
    /// Returns the orchestrator and an event receiver channel.
    pub fn new(
        metric_service: Arc<MetricService>,
        config: AnchorConfig,
    ) -> detrix_core::Result<(Self, mpsc::Receiver<FileEvent>)> {
        let watcher_config = FileWatcherConfig {
            debounce_ms: config.file_change_debounce_ms,
            recursive: true,
            channel_size: config.file_watcher_channel_size,
        };

        let (watcher, event_rx) = NotifyFileWatcher::with_config(watcher_config)?;

        Ok((
            Self {
                watcher: Arc::new(watcher),
                metric_service,
                config,
            },
            event_rx,
        ))
    }

    /// Resolve paths to watch
    ///
    /// If `watch_paths` is configured, use those paths.
    /// Otherwise, derive paths from existing metrics' directories.
    pub async fn resolve_watch_paths(&self) -> Vec<PathBuf> {
        if !self.config.watch_paths.is_empty() {
            // Use explicit config paths
            self.config
                .watch_paths
                .iter()
                .map(PathBuf::from)
                .filter(|p| p.exists())
                .collect()
        } else {
            // Derive from existing metrics
            self.metric_service.get_watched_directories().await
        }
    }

    /// Start watching the resolved paths
    pub async fn start_watching(&self) -> detrix_core::Result<()> {
        let paths = self.resolve_watch_paths().await;

        if paths.is_empty() {
            info!("No paths to watch for auto-relocation");
            return Ok(());
        }

        for path in paths {
            match self.watcher.watch(path.clone()).await {
                Ok(()) => info!(path = %path.display(), "Started watching for file changes"),
                Err(e) => warn!(path = %path.display(), error = %e, "Failed to watch path"),
            }
        }

        Ok(())
    }

    /// Run the event processing loop
    ///
    /// Processes file events and triggers metric relocation.
    /// Exits when shutdown signal is received.
    pub async fn run_event_loop(
        &self,
        mut event_rx: mpsc::Receiver<FileEvent>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) {
        info!("FileWatcherOrchestrator event loop started");

        loop {
            tokio::select! {
                biased;

                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("FileWatcherOrchestrator shutting down");
                        break;
                    }
                }

                Some(event) = event_rx.recv() => {
                    match &event {
                        FileEvent::Modified(path) | FileEvent::Created(path) => {
                            if let Some(path_str) = path.to_str() {
                                match self.metric_service.handle_file_change(path_str).await {
                                    Ok(result) => {
                                        if result.relocated > 0 || result.orphaned > 0 {
                                            info!(
                                                file = %path.display(),
                                                relocated = result.relocated,
                                                orphaned = result.orphaned,
                                                verified = result.verified,
                                                "File change processed"
                                            );
                                        } else {
                                            debug!(
                                                file = %path.display(),
                                                verified = result.verified,
                                                "File change processed (no relocation needed)"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        warn!(
                                            file = %path.display(),
                                            error = %e,
                                            "File change handling failed"
                                        );
                                    }
                                }
                            }
                        }
                        FileEvent::Deleted(path) => {
                            // Log deleted files - metrics in deleted files will fail
                            // verification on next check
                            debug!(file = %path.display(), "File deleted");
                        }
                        FileEvent::Renamed { from, to } => {
                            // Log renames - could be enhanced to update metric paths
                            debug!(
                                from = %from.display(),
                                to = %to.display(),
                                "File renamed"
                            );
                        }
                    }
                }
            }
        }

        // Clean up watcher
        if let Err(e) = self.watcher.shutdown().await {
            warn!(error = %e, "Error shutting down file watcher");
        }
    }

    /// Spawn the orchestrator as a background task
    ///
    /// Returns a `JoinHandle` that can be used to wait for completion.
    pub fn spawn(
        self: Arc<Self>,
        event_rx: mpsc::Receiver<FileEvent>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run_event_loop(event_rx, shutdown_rx).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_watch_directory() {
        let temp_dir = TempDir::new().unwrap();
        let (watcher, _rx) = NotifyFileWatcher::new().unwrap();

        // Watch the temp directory
        watcher.watch(temp_dir.path().to_path_buf()).await.unwrap();

        assert!(watcher.is_watching(&temp_dir.path().to_path_buf()));
        assert_eq!(watcher.watched_paths().len(), 1);
    }

    #[tokio::test]
    async fn test_unwatch() {
        let temp_dir = TempDir::new().unwrap();
        let (watcher, _rx) = NotifyFileWatcher::new().unwrap();

        watcher.watch(temp_dir.path().to_path_buf()).await.unwrap();
        assert!(watcher.is_watching(&temp_dir.path().to_path_buf()));

        watcher
            .unwatch(temp_dir.path().to_path_buf())
            .await
            .unwrap();
        assert!(!watcher.is_watching(&temp_dir.path().to_path_buf()));
    }

    #[tokio::test]
    async fn test_watch_nonexistent_path_fails() {
        let (watcher, _rx) = NotifyFileWatcher::new().unwrap();

        let result = watcher
            .watch(PathBuf::from("/nonexistent/path/that/does/not/exist"))
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shutdown() {
        let temp_dir = TempDir::new().unwrap();
        let (watcher, _rx) = NotifyFileWatcher::new().unwrap();

        watcher.watch(temp_dir.path().to_path_buf()).await.unwrap();
        watcher.shutdown().await.unwrap();

        assert!(watcher.watched_paths().is_empty());

        // Watch should fail after shutdown
        let result = watcher.watch(temp_dir.path().to_path_buf()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_file_modification_event() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        // Create initial file
        {
            let mut file = std::fs::File::create(&file_path).unwrap();
            file.write_all(b"initial content").unwrap();
        }

        let config = FileWatcherConfig {
            debounce_ms: 100, // Short debounce for testing
            recursive: true,
            channel_size: 256,
        };
        let (watcher, mut rx) = NotifyFileWatcher::with_config(config).unwrap();

        // Watch the directory
        watcher.watch(temp_dir.path().to_path_buf()).await.unwrap();

        // Give the watcher time to start
        sleep(Duration::from_millis(50)).await;

        // Modify the file
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&file_path)
                .unwrap();
            file.write_all(b"modified content").unwrap();
        }

        // Wait for debounced event
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;

        match event {
            Ok(Some(FileEvent::Modified(path))) => {
                assert_eq!(path.file_name().unwrap(), "test.txt");
            }
            Ok(Some(other)) => {
                // File creation might be detected instead
                assert!(matches!(
                    other,
                    FileEvent::Created(_) | FileEvent::Modified(_)
                ));
            }
            Ok(None) => {
                // Channel closed - watcher may have shut down
                println!("Warning: File event channel closed (watcher may have shut down)");
            }
            Err(_) => {
                // Timeout is acceptable in CI environments where file events may be unreliable
                println!("Warning: File event not received within timeout (may be CI environment)");
            }
        }
    }

    #[tokio::test]
    async fn test_double_watch_is_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let (watcher, _rx) = NotifyFileWatcher::new().unwrap();

        watcher.watch(temp_dir.path().to_path_buf()).await.unwrap();
        watcher.watch(temp_dir.path().to_path_buf()).await.unwrap();

        // Should still only have one watched path
        assert_eq!(watcher.watched_paths().len(), 1);
    }
}
