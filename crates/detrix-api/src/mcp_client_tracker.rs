//! MCP Client Tracker
//!
//! Tracks active MCP bridge clients for daemon lifecycle management.
//! When the daemon was spawned by MCP (--mcp-spawned), it should auto-shutdown
//! when all MCP clients disconnect.
//!
//! ## How it works
//!
//! MCP over HTTP is stateless, so we track clients by:
//! 1. Registering on `initialize` request (returns client_id)
//! 2. Client sends X-Detrix-Client-Id header with each request
//! 3. Each request updates the last_seen timestamp
//! 4. Cleanup task removes clients that haven't been seen for `heartbeat_timeout`
//! 5. When all clients are gone (and daemon was mcp-spawned), signal shutdown

use chrono::{DateTime, Utc};
use detrix_config::constants::{
    DEFAULT_MCP_CLEANUP_INTERVAL_SECS, DEFAULT_MCP_HEARTBEAT_TIMEOUT_SECS,
    DEFAULT_SHUTDOWN_GRACE_PERIOD_SECS,
};
use detrix_config::McpBridgeConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{watch, RwLock};
use tracing::{debug, info, warn};

/// Unique identifier for an MCP client session
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct McpClientId(pub String);

impl McpClientId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl Default for McpClientId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for McpClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Information about the parent process that spawned the MCP client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentProcessInfo {
    /// Parent process ID
    pub pid: u32,
    /// Name of the parent process (e.g., "windsurf", "claude", "cursor")
    pub name: String,
    /// MCP bridge process ID (the direct client)
    pub bridge_pid: u32,
}

/// Information about a connected MCP client
#[derive(Debug, Clone)]
pub struct McpClientInfo {
    /// Client identifier
    pub id: McpClientId,
    /// When the client connected (monotonic, for duration calculations)
    pub connected_at: Instant,
    /// When the client connected (wall clock, for display)
    pub connected_at_utc: DateTime<Utc>,
    /// Last heartbeat received (monotonic)
    pub last_heartbeat: Instant,
    /// Last heartbeat received (wall clock, for display)
    pub last_heartbeat_utc: DateTime<Utc>,
    /// Parent process information
    pub parent_process: Option<ParentProcessInfo>,
}

/// Serializable summary of MCP client info for status display
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpClientSummary {
    /// Client identifier
    pub id: String,
    /// When the client connected (ISO 8601)
    pub connected_at: String,
    /// Duration connected in seconds
    pub connected_duration_secs: u64,
    /// Last activity timestamp (ISO 8601)
    pub last_activity: String,
    /// Seconds since last activity
    pub last_activity_ago_secs: u64,
    /// Parent process information
    pub parent_process: Option<ParentProcessInfo>,
}

/// Configuration for the MCP client tracker
#[derive(Debug, Clone)]
pub struct McpTrackerConfig {
    /// Heartbeat timeout - if no heartbeat received within this duration, client is considered dead
    pub heartbeat_timeout: Duration,
    /// Grace period before shutdown after all clients disconnect
    pub shutdown_grace_period: Duration,
    /// Cleanup interval for stale clients
    pub cleanup_interval: Duration,
}

impl Default for McpTrackerConfig {
    fn default() -> Self {
        Self {
            heartbeat_timeout: Duration::from_secs(DEFAULT_MCP_HEARTBEAT_TIMEOUT_SECS),
            shutdown_grace_period: Duration::from_secs(DEFAULT_SHUTDOWN_GRACE_PERIOD_SECS),
            cleanup_interval: Duration::from_secs(DEFAULT_MCP_CLEANUP_INTERVAL_SECS),
        }
    }
}

impl From<&McpBridgeConfig> for McpTrackerConfig {
    fn from(config: &McpBridgeConfig) -> Self {
        Self {
            heartbeat_timeout: Duration::from_secs(config.heartbeat_timeout_secs),
            shutdown_grace_period: Duration::from_secs(config.shutdown_grace_period_secs),
            cleanup_interval: Duration::from_secs(config.cleanup_interval_secs),
        }
    }
}

/// Tracks active MCP bridge clients
pub struct McpClientTracker {
    /// Active clients
    clients: Arc<RwLock<HashMap<McpClientId, McpClientInfo>>>,
    /// Configuration
    config: McpTrackerConfig,
    /// Shutdown signal sender (signals when all clients disconnect)
    shutdown_tx: watch::Sender<bool>,
    /// Shutdown signal receiver (for daemon to listen)
    shutdown_rx: watch::Receiver<bool>,
    /// Whether this daemon was spawned by MCP (should auto-shutdown)
    mcp_spawned: bool,
    /// Number of active debugger connections (updated by AdapterLifecycleManager)
    /// If > 0, daemon should NOT auto-shutdown even if MCP clients disconnect
    active_connection_count: Arc<AtomicUsize>,
}

impl McpClientTracker {
    /// Create a new MCP client tracker
    pub fn new(mcp_spawned: bool) -> Self {
        Self::with_config(mcp_spawned, McpTrackerConfig::default())
    }

    /// Create with custom configuration
    pub fn with_config(mcp_spawned: bool, config: McpTrackerConfig) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            config,
            shutdown_tx,
            shutdown_rx,
            mcp_spawned,
            active_connection_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Register a new MCP client
    pub async fn register(&self, parent_process: Option<ParentProcessInfo>) -> McpClientId {
        let client_id = McpClientId::new();
        let now = Instant::now();
        let now_utc = Utc::now();

        let info = McpClientInfo {
            id: client_id.clone(),
            connected_at: now,
            connected_at_utc: now_utc,
            last_heartbeat: now,
            last_heartbeat_utc: now_utc,
            parent_process: parent_process.clone(),
        };

        let mut clients = self.clients.write().await;
        clients.insert(client_id.clone(), info);

        if let Some(ref pp) = parent_process {
            info!(
                client_id = %client_id.0,
                parent_name = %pp.name,
                parent_pid = pp.pid,
                bridge_pid = pp.bridge_pid,
                total_clients = clients.len(),
                "MCP client registered ({}->detrix)"
                , pp.name
            );
        } else {
            info!(
                client_id = %client_id.0,
                total_clients = clients.len(),
                "MCP client registered"
            );
        }

        client_id
    }

    /// Record a heartbeat from a client
    pub async fn heartbeat(&self, client_id: &McpClientId) -> bool {
        let mut clients = self.clients.write().await;

        if let Some(info) = clients.get_mut(client_id) {
            info.last_heartbeat = Instant::now();
            info.last_heartbeat_utc = Utc::now();
            debug!(client_id = %client_id.0, "MCP client heartbeat");
            true
        } else {
            warn!(client_id = %client_id.0, "Heartbeat from unknown client");
            false
        }
    }

    /// Touch a client to update last_seen (implicit heartbeat with each request)
    /// If client_id is not found, registers a new client (without parent info)
    pub async fn touch(&self, client_id: &McpClientId) {
        let mut clients = self.clients.write().await;

        if let Some(info) = clients.get_mut(client_id) {
            info.last_heartbeat = Instant::now();
            info.last_heartbeat_utc = Utc::now();
            debug!(client_id = %client_id.0, "MCP client activity");
        } else {
            // Auto-register unknown client (no parent info available)
            let now = Instant::now();
            let now_utc = Utc::now();
            let info = McpClientInfo {
                id: client_id.clone(),
                connected_at: now,
                connected_at_utc: now_utc,
                last_heartbeat: now,
                last_heartbeat_utc: now_utc,
                parent_process: None,
            };
            clients.insert(client_id.clone(), info);
            info!(
                client_id = %client_id.0,
                total_clients = clients.len(),
                "MCP client auto-registered on activity"
            );
        }
    }

    /// Update parent process info for an existing client
    pub async fn update_parent_process(
        &self,
        client_id: &McpClientId,
        parent_process: ParentProcessInfo,
    ) {
        let mut clients = self.clients.write().await;
        if let Some(info) = clients.get_mut(client_id) {
            info!(
                client_id = %client_id.0,
                parent_name = %parent_process.name,
                parent_pid = parent_process.pid,
                bridge_pid = parent_process.bridge_pid,
                "MCP client parent process updated ({}->detrix)",
                parent_process.name
            );
            info.parent_process = Some(parent_process);
        }
    }

    /// Unregister a client (client explicitly disconnecting)
    pub async fn unregister(&self, client_id: &McpClientId) {
        let mut clients = self.clients.write().await;

        if clients.remove(client_id).is_some() {
            info!(
                client_id = %client_id.0,
                remaining_clients = clients.len(),
                "MCP client unregistered"
            );

            // Check if we should signal shutdown
            if self.mcp_spawned && clients.is_empty() {
                drop(clients); // Release lock before signaling
                self.schedule_shutdown().await;
            }
        }
    }

    /// Get the number of active clients
    pub async fn active_client_count(&self) -> usize {
        self.clients.read().await.len()
    }

    /// Check if any clients are connected
    pub async fn has_clients(&self) -> bool {
        !self.clients.read().await.is_empty()
    }

    /// Get a summary of all connected clients for status display
    pub async fn get_clients(&self) -> Vec<McpClientSummary> {
        let clients = self.clients.read().await;
        let now = Instant::now();

        clients
            .values()
            .map(|info| {
                let connected_duration = now.duration_since(info.connected_at);
                let last_activity_ago = now.duration_since(info.last_heartbeat);

                McpClientSummary {
                    id: info.id.0.clone(),
                    connected_at: info.connected_at_utc.to_rfc3339(),
                    connected_duration_secs: connected_duration.as_secs(),
                    last_activity: info.last_heartbeat_utc.to_rfc3339(),
                    last_activity_ago_secs: last_activity_ago.as_secs(),
                    parent_process: info.parent_process.clone(),
                }
            })
            .collect()
    }

    /// Get shutdown receiver (daemon listens to this)
    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    /// Clean up stale clients (clients that haven't sent heartbeat)
    pub async fn cleanup_stale(&self) {
        let now = Instant::now();
        let timeout = self.config.heartbeat_timeout;

        let mut clients = self.clients.write().await;
        let before_count = clients.len();

        clients.retain(|id, info| {
            let elapsed = now.duration_since(info.last_heartbeat);
            if elapsed > timeout {
                warn!(
                    client_id = %id.0,
                    elapsed_seconds = elapsed.as_secs(),
                    "Removing stale MCP client"
                );
                false
            } else {
                true
            }
        });

        let removed = before_count - clients.len();
        if removed > 0 {
            info!(
                removed_count = removed,
                remaining_clients = clients.len(),
                "Cleaned up stale MCP clients"
            );

            // Check if we should signal shutdown
            if self.mcp_spawned && clients.is_empty() {
                drop(clients); // Release lock before signaling
                self.schedule_shutdown().await;
            }
        }
    }

    /// Schedule shutdown after grace period
    ///
    /// Shutdown is ONLY triggered if:
    /// 1. Daemon was spawned by MCP (--mcp-spawned)
    /// 2. No MCP clients connected
    /// 3. No active debugger connections
    ///
    /// This allows the daemon to survive MCP bridge restarts if there's active debugging work.
    async fn schedule_shutdown(&self) {
        if !self.mcp_spawned {
            return;
        }

        // Check if there are active debugger connections
        // If so, don't shutdown - the user is actively debugging
        let connection_count = self.connection_count();
        if connection_count > 0 {
            info!(
                connection_count = connection_count,
                "MCP clients disconnected but {} active debugger connection(s) exist - NOT scheduling shutdown",
                connection_count
            );
            return;
        }

        let grace_period = self.config.shutdown_grace_period;
        let clients = Arc::clone(&self.clients);
        let active_connections = Arc::clone(&self.active_connection_count);
        let shutdown_tx = self.shutdown_tx.clone();

        info!(
            grace_period_seconds = grace_period.as_secs(),
            "All MCP clients disconnected and no active connections, scheduling shutdown"
        );

        tokio::spawn(async move {
            tokio::time::sleep(grace_period).await;

            let no_clients = clients.read().await.is_empty();
            let no_connections = active_connections.load(Ordering::SeqCst) == 0;

            // Double-check conditions still hold after grace period
            if no_clients && no_connections {
                info!("Grace period elapsed, signaling daemon shutdown");
                let _ = shutdown_tx.send(true);
            } else if !no_clients {
                info!("New MCP client connected during grace period, canceling shutdown");
            } else {
                info!("New debugger connection created during grace period, canceling shutdown");
            }
        });
    }

    /// Start the background cleanup task
    pub fn start_cleanup_task(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let tracker = Arc::clone(self);
        let interval = tracker.config.cleanup_interval;

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);

            loop {
                ticker.tick().await;
                tracker.cleanup_stale().await;
            }
        })
    }

    /// Whether this tracker is for an MCP-spawned daemon
    pub fn is_mcp_spawned(&self) -> bool {
        self.mcp_spawned
    }

    /// Update the active debugger connection count
    /// Called by AdapterLifecycleManager when connections change
    pub fn update_connection_count(&self, count: usize) {
        let old_count = self.active_connection_count.swap(count, Ordering::SeqCst);
        if old_count != count {
            debug!(
                old_count = old_count,
                new_count = count,
                "Active debugger connection count updated"
            );
        }
    }

    /// Get the current active debugger connection count
    pub fn connection_count(&self) -> usize {
        self.active_connection_count.load(Ordering::SeqCst)
    }

    /// Check if there are active debugger connections
    pub fn has_active_connections(&self) -> bool {
        self.connection_count() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_unregister() {
        let tracker = McpClientTracker::new(false);

        // Register a client
        let client_id = tracker.register(None).await;
        assert_eq!(tracker.active_client_count().await, 1);

        // Unregister
        tracker.unregister(&client_id).await;
        assert_eq!(tracker.active_client_count().await, 0);
    }

    #[tokio::test]
    async fn test_heartbeat() {
        let tracker = McpClientTracker::new(false);

        let client_id = tracker.register(None).await;

        // Valid heartbeat
        assert!(tracker.heartbeat(&client_id).await);

        // Invalid heartbeat (unknown client)
        let unknown = McpClientId("unknown".to_string());
        assert!(!tracker.heartbeat(&unknown).await);
    }

    #[tokio::test]
    async fn test_stale_cleanup() {
        let config = McpTrackerConfig {
            heartbeat_timeout: Duration::from_millis(100),
            shutdown_grace_period: Duration::from_millis(50),
            cleanup_interval: Duration::from_millis(50),
        };

        let tracker = McpClientTracker::with_config(false, config);

        // Register a client
        let _client_id = tracker.register(None).await;
        assert_eq!(tracker.active_client_count().await, 1);

        // Wait for timeout
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Cleanup should remove stale client
        tracker.cleanup_stale().await;
        assert_eq!(tracker.active_client_count().await, 0);
    }

    #[tokio::test]
    async fn test_mcp_spawned_shutdown_signal() {
        let config = McpTrackerConfig {
            heartbeat_timeout: Duration::from_secs(30),
            shutdown_grace_period: Duration::from_millis(50),
            cleanup_interval: Duration::from_secs(5),
        };

        let tracker = McpClientTracker::with_config(true, config);
        let shutdown_rx = tracker.shutdown_receiver();

        // Register and unregister a client
        let client_id = tracker.register(None).await;
        tracker.unregister(&client_id).await;

        // Wait for grace period
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should receive shutdown signal
        assert!(*shutdown_rx.borrow());
    }

    #[tokio::test]
    async fn test_non_mcp_spawned_no_shutdown() {
        let config = McpTrackerConfig {
            heartbeat_timeout: Duration::from_secs(30),
            shutdown_grace_period: Duration::from_millis(50),
            cleanup_interval: Duration::from_secs(5),
        };

        let tracker = McpClientTracker::with_config(false, config); // NOT mcp-spawned
        let shutdown_rx = tracker.shutdown_receiver();

        // Register and unregister a client
        let client_id = tracker.register(None).await;
        tracker.unregister(&client_id).await;

        // Wait for grace period
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should NOT receive shutdown signal (not mcp-spawned)
        assert!(!*shutdown_rx.borrow());
    }

    #[tokio::test]
    async fn test_touch_auto_registers_unknown_client() {
        let tracker = McpClientTracker::new(false);

        // No clients initially
        assert_eq!(tracker.active_client_count().await, 0);

        // Touch an unknown client ID
        let client_id = McpClientId::from_string("auto-registered-client");
        tracker.touch(&client_id).await;

        // Should have auto-registered
        assert_eq!(tracker.active_client_count().await, 1);

        // Subsequent touch should work normally
        tracker.touch(&client_id).await;
        assert_eq!(tracker.active_client_count().await, 1);
    }

    #[tokio::test]
    async fn test_multiple_clients() {
        let tracker = McpClientTracker::new(false);

        // Register multiple clients
        let client1 = tracker.register(None).await;
        let client2 = tracker.register(None).await;
        let client3 = tracker.register(None).await;

        assert_eq!(tracker.active_client_count().await, 3);

        // Unregister one
        tracker.unregister(&client1).await;
        assert_eq!(tracker.active_client_count().await, 2);

        // Unregister remaining
        tracker.unregister(&client2).await;
        tracker.unregister(&client3).await;
        assert_eq!(tracker.active_client_count().await, 0);
    }

    #[tokio::test]
    async fn test_shutdown_canceled_when_new_client_connects() {
        let config = McpTrackerConfig {
            heartbeat_timeout: Duration::from_secs(30),
            shutdown_grace_period: Duration::from_millis(100),
            cleanup_interval: Duration::from_secs(5),
        };

        let tracker = McpClientTracker::with_config(true, config);
        let shutdown_rx = tracker.shutdown_receiver();

        // Register and unregister first client (schedules shutdown)
        let client1 = tracker.register(None).await;
        tracker.unregister(&client1).await;

        // Wait less than grace period, then connect new client
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _client2 = tracker.register(None).await;

        // Wait for grace period to complete
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Shutdown should have been canceled
        assert!(!*shutdown_rx.borrow());
    }

    #[tokio::test]
    async fn test_stale_cleanup_triggers_shutdown_for_mcp_spawned() {
        let config = McpTrackerConfig {
            heartbeat_timeout: Duration::from_millis(50),
            shutdown_grace_period: Duration::from_millis(50),
            cleanup_interval: Duration::from_millis(10),
        };

        let tracker = McpClientTracker::with_config(true, config);
        let shutdown_rx = tracker.shutdown_receiver();

        // Register a client
        let _client_id = tracker.register(None).await;
        assert_eq!(tracker.active_client_count().await, 1);

        // Wait for heartbeat timeout (client becomes stale)
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Cleanup should remove stale client and trigger shutdown
        tracker.cleanup_stale().await;
        assert_eq!(tracker.active_client_count().await, 0);

        // Wait for grace period
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should receive shutdown signal
        assert!(*shutdown_rx.borrow());
    }

    #[tokio::test]
    async fn test_has_clients() {
        let tracker = McpClientTracker::new(false);

        assert!(!tracker.has_clients().await);

        let client_id = tracker.register(None).await;
        assert!(tracker.has_clients().await);

        tracker.unregister(&client_id).await;
        assert!(!tracker.has_clients().await);
    }

    #[tokio::test]
    async fn test_is_mcp_spawned() {
        let tracker_spawned = McpClientTracker::new(true);
        assert!(tracker_spawned.is_mcp_spawned());

        let tracker_not_spawned = McpClientTracker::new(false);
        assert!(!tracker_not_spawned.is_mcp_spawned());
    }

    #[tokio::test]
    async fn test_double_unregister_is_safe() {
        let tracker = McpClientTracker::new(false);

        let client_id = tracker.register(None).await;
        assert_eq!(tracker.active_client_count().await, 1);

        // First unregister
        tracker.unregister(&client_id).await;
        assert_eq!(tracker.active_client_count().await, 0);

        // Second unregister should be safe (no-op)
        tracker.unregister(&client_id).await;
        assert_eq!(tracker.active_client_count().await, 0);
    }

    #[tokio::test]
    async fn test_connection_count_tracking() {
        let tracker = McpClientTracker::new(false);

        // Initial count is 0
        assert_eq!(tracker.connection_count(), 0);
        assert!(!tracker.has_active_connections());

        // Update connection count
        tracker.update_connection_count(2);
        assert_eq!(tracker.connection_count(), 2);
        assert!(tracker.has_active_connections());

        // Update to 0
        tracker.update_connection_count(0);
        assert_eq!(tracker.connection_count(), 0);
        assert!(!tracker.has_active_connections());
    }

    #[tokio::test]
    async fn test_no_shutdown_when_connections_exist() {
        let config = McpTrackerConfig {
            heartbeat_timeout: Duration::from_secs(30),
            shutdown_grace_period: Duration::from_millis(50),
            cleanup_interval: Duration::from_secs(5),
        };

        let tracker = McpClientTracker::with_config(true, config);
        let shutdown_rx = tracker.shutdown_receiver();

        // Simulate active debugger connection
        tracker.update_connection_count(1);

        // Register and unregister MCP client
        let client_id = tracker.register(None).await;
        tracker.unregister(&client_id).await;

        // Wait for grace period (plus some buffer)
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should NOT receive shutdown signal because there's an active connection
        assert!(
            !*shutdown_rx.borrow(),
            "Should NOT shutdown when active connections exist"
        );
    }

    #[tokio::test]
    async fn test_shutdown_when_no_connections() {
        let config = McpTrackerConfig {
            heartbeat_timeout: Duration::from_secs(30),
            shutdown_grace_period: Duration::from_millis(50),
            cleanup_interval: Duration::from_secs(5),
        };

        let tracker = McpClientTracker::with_config(true, config);
        let shutdown_rx = tracker.shutdown_receiver();

        // No active connections
        tracker.update_connection_count(0);

        // Register and unregister MCP client
        let client_id = tracker.register(None).await;
        tracker.unregister(&client_id).await;

        // Wait for grace period
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should receive shutdown signal
        assert!(
            *shutdown_rx.borrow(),
            "Should shutdown when no connections and no clients"
        );
    }

    #[tokio::test]
    async fn test_shutdown_canceled_when_connection_created_during_grace() {
        let config = McpTrackerConfig {
            heartbeat_timeout: Duration::from_secs(30),
            shutdown_grace_period: Duration::from_millis(100),
            cleanup_interval: Duration::from_secs(5),
        };

        let tracker = McpClientTracker::with_config(true, config);
        let shutdown_rx = tracker.shutdown_receiver();

        // No connections initially
        tracker.update_connection_count(0);

        // Register and unregister MCP client (schedules shutdown)
        let client_id = tracker.register(None).await;
        tracker.unregister(&client_id).await;

        // Wait 30ms, then simulate connection being created
        tokio::time::sleep(Duration::from_millis(30)).await;
        tracker.update_connection_count(1);

        // Wait for grace period to complete
        tokio::time::sleep(Duration::from_millis(120)).await;

        // Shutdown should have been canceled
        assert!(
            !*shutdown_rx.borrow(),
            "Shutdown should be canceled when connection created during grace period"
        );
    }
}
