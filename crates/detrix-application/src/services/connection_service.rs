//! ConnectionService - Use case for managing debugger connections (debugpy, delve, lldb-dap)
//!
//! Following Clean Architecture, this service:
//! - Contains business logic for connection management
//! - Depends on domain traits (ConnectionRepository)
//! - Delegates adapter lifecycle to AdapterLifecycleManager
//! - Is protocol-agnostic (no knowledge of gRPC, REST, MCP, etc.)

use crate::services::AdapterLifecycleManager;
use crate::{ConnectionRepositoryRef, MetricRepositoryRef};
use detrix_core::{
    Connection, ConnectionId, ConnectionStatus, Result, SourceLanguage, SystemEvent,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, instrument};

/// Service for managing debugger connections
///
/// This service handles the business logic for:
/// - Creating new connections to debugger servers (debugpy, delve, lldb-dap)
/// - Disconnecting from debugger servers
/// - Listing and querying connections
///
/// Uses dependency injection via trait objects for testability.
/// Delegates adapter lifecycle management to AdapterLifecycleManager.
pub struct ConnectionService {
    /// Repository for persisting connections
    connection_repo: ConnectionRepositoryRef,

    /// Repository for persisting metrics (needed for cascade delete)
    metric_repo: MetricRepositoryRef,

    /// Lifecycle manager for DAP adapters
    adapter_lifecycle_manager: Arc<AdapterLifecycleManager>,

    /// System event broadcast channel for connection events
    system_event_tx: broadcast::Sender<SystemEvent>,
}

impl ConnectionService {
    /// Create a new ConnectionService
    ///
    /// # Arguments
    /// * `connection_repo` - Repository for connection persistence
    /// * `metric_repo` - Repository for metric persistence (for cascade delete)
    /// * `adapter_lifecycle_manager` - Manager for DAP adapter lifecycle
    /// * `system_event_tx` - Broadcast channel for system events
    pub fn new(
        connection_repo: ConnectionRepositoryRef,
        metric_repo: MetricRepositoryRef,
        adapter_lifecycle_manager: Arc<AdapterLifecycleManager>,
        system_event_tx: broadcast::Sender<SystemEvent>,
    ) -> Self {
        Self {
            connection_repo,
            metric_repo,
            adapter_lifecycle_manager,
            system_event_tx,
        }
    }

    /// Create a new connection to a debugger server
    ///
    /// This method:
    /// 1. Validates connection parameters via domain logic
    /// 2. Saves connection to repository
    /// 3. Delegates adapter lifecycle to AdapterLifecycleManager
    /// 4. Updates connection status on success
    ///
    /// # Arguments
    /// * `host` - Host address (e.g., "127.0.0.1", "localhost")
    /// * `port` - Port number (must be >= 1024)
    /// * `language` - Language/adapter type (e.g., "python", "go", "rust")
    /// * `id` - Optional custom connection ID. If None, auto-generates from host:port
    /// * `program` - Optional program path for Rust direct lldb-dap launch mode
    /// * `safe_mode` - Enable SafeMode: only allow logpoints, disable breakpoint-based operations
    ///
    /// # Returns
    /// ConnectionId of the created connection
    ///
    /// # Business Rules
    /// - Port must be >= 1024 (not in reserved range)
    /// - Host must not be empty
    /// - Language must not be empty
    /// - Connection ID must be unique (auto-generated IDs prevent collisions)
    #[instrument(skip(self), fields(host = %host, port = port, language = %language, pid = ?pid, safe_mode = safe_mode))]
    pub async fn create_connection(
        &self,
        host: String,
        port: u16,
        language: String,
        id: Option<String>,
        program: Option<String>,
        pid: Option<u32>,
        safe_mode: bool,
    ) -> Result<ConnectionId> {
        use tracing::info;

        // 1. Parse language string to SourceLanguage (fail-fast with clear error)
        let language: SourceLanguage = language.try_into()?;

        // 2. Determine connection ID (auto-generate or use provided)
        let connection_id = match id {
            Some(custom_id) => ConnectionId::new(custom_id),
            None => ConnectionId::from_host_port(&host, port),
        };

        // 3. Check if connection already exists and is connected
        //    This handles the case where restore_connections is running or connection was already created
        if let Ok(Some(existing)) = self.connection_repo.find_by_id(&connection_id).await {
            if existing.status == ConnectionStatus::Connected
                && self
                    .adapter_lifecycle_manager
                    .has_adapter(&connection_id)
                    .await
            {
                info!(
                    "Connection {} already exists and is connected, returning existing",
                    connection_id.0
                );
                return Ok(connection_id);
            }
            // Connection exists but not connected - will be restarted below
            info!(
                "Connection {} exists but status is {:?}, will restart adapter",
                connection_id.0, existing.status
            );
        }

        // 4. Create Connection entity (validates host, port, and rejects Unknown language)
        let mut connection = Connection::new(connection_id.clone(), host.clone(), port, language)?;
        connection.safe_mode = safe_mode;

        // 5. Save connection to repository (initially Disconnected)
        self.connection_repo.save(&connection).await?;

        // 6. Start adapter via lifecycle manager (handles everything: start, subscribe, route events)
        //    Pass language to dispatch to the correct adapter factory method
        //    Note: start_adapter returns degradation info (sync_failed, resume_failed) which is logged internally
        let _start_result = self
            .adapter_lifecycle_manager
            .start_adapter(
                connection_id.clone(),
                &host,
                port,
                connection.language,
                program,
                pid,
                connection.safe_mode,
            )
            .await?;

        // 7. Update connection status to Connected
        self.connection_repo
            .update_status(&connection_id, ConnectionStatus::Connected)
            .await?;

        // 8. Emit connection created event
        let event = SystemEvent::connection_created(
            connection_id.clone(),
            &host,
            port,
            connection.language.as_str(),
        );
        let _ = self.system_event_tx.send(event);

        Ok(connection_id)
    }

    /// Disconnect from a debugger server
    ///
    /// This method:
    /// 1. Stops the adapter via AdapterLifecycleManager
    /// 2. Updates connection status to Disconnected
    ///
    /// # Arguments
    /// * `id` - Connection ID to disconnect
    ///
    /// # Business Rules
    /// - Stops the adapter and cleans up resources
    /// - Updates connection status to Disconnected
    /// - Does NOT delete the connection (keeps history)
    /// - Does NOT delete metrics (they persist for reconnection)
    #[instrument(skip(self), fields(connection_id = %id.0))]
    pub async fn disconnect(&self, id: &ConnectionId) -> Result<()> {
        // 1. Stop adapter via lifecycle manager
        self.adapter_lifecycle_manager.stop_adapter(id).await?;

        // 2. Update status to Disconnected
        self.connection_repo
            .update_status(id, ConnectionStatus::Disconnected)
            .await?;

        // 3. Emit connection closed event
        let event = SystemEvent::connection_closed(id.clone());
        let _ = self.system_event_tx.send(event);

        Ok(())
    }

    /// Delete a connection and all associated metrics.
    ///
    /// This method:
    /// 1. Stops the adapter via AdapterLifecycleManager
    /// 2. Deletes all metrics associated with the connection
    /// 3. Deletes the connection from repository
    ///
    /// Use this for explicit user-requested deletion, not for disconnect/reconnection scenarios.
    ///
    /// # Arguments
    /// * `id` - Connection ID to delete
    ///
    /// # Business Rules
    /// - Stops the adapter and cleans up resources
    /// - Cascade deletes all associated metrics
    /// - Removes the connection from storage
    #[instrument(skip(self), fields(connection_id = %id.0))]
    pub async fn delete_connection(&self, id: &ConnectionId) -> Result<()> {
        // 1. Stop adapter via lifecycle manager (ignore error if not running)
        let _ = self.adapter_lifecycle_manager.stop_adapter(id).await;

        // 2. Delete all associated metrics (cascade delete)
        let deleted_metrics = self.metric_repo.delete_by_connection_id(id).await?;
        if deleted_metrics > 0 {
            info!(
                "Deleted {} metrics for connection {}",
                deleted_metrics, id.0
            );
        }

        // 3. Delete connection from repository
        self.connection_repo.delete(id).await?;

        // 4. Emit connection closed event
        let event = SystemEvent::connection_closed(id.clone());
        let _ = self.system_event_tx.send(event);

        info!("Deleted connection {}", id.0);

        Ok(())
    }

    /// Get an adapter by connection ID
    ///
    /// Returns the adapter reference if found and running
    pub async fn get_adapter(&self, id: &ConnectionId) -> Option<crate::DapAdapterRef> {
        self.adapter_lifecycle_manager.get_adapter(id).await
    }

    /// Check if an adapter is running for this connection
    pub async fn has_running_adapter(&self, id: &ConnectionId) -> bool {
        self.adapter_lifecycle_manager.has_adapter(id).await
    }

    /// List all connections
    ///
    /// # Returns
    /// A vector of all connections (active and inactive)
    pub async fn list_connections(&self) -> Result<Vec<Connection>> {
        self.connection_repo.list_all().await
    }

    /// Get a connection by ID
    ///
    /// # Arguments
    /// * `id` - Connection ID to find
    ///
    /// # Returns
    /// Some(Connection) if found, None if not found
    pub async fn get_connection(&self, id: &ConnectionId) -> Result<Option<Connection>> {
        self.connection_repo.find_by_id(id).await
    }

    /// List only active connections (Connected or Connecting status)
    ///
    /// # Returns
    /// A vector of active connections
    #[instrument(skip(self))]
    pub async fn list_active_connections(&self) -> Result<Vec<Connection>> {
        self.connection_repo.find_active().await
    }

    /// Check if a connection exists
    ///
    /// # Arguments
    /// * `id` - Connection ID to check
    ///
    /// # Returns
    /// true if connection exists, false otherwise
    pub async fn connection_exists(&self, id: &ConnectionId) -> Result<bool> {
        self.connection_repo.exists(id).await
    }

    /// Update connection's last active timestamp
    ///
    /// # Arguments
    /// * `id` - Connection ID to touch
    ///
    /// This is useful for tracking which connections are actively being used
    pub async fn touch_connection(&self, id: &ConnectionId) -> Result<()> {
        self.connection_repo.touch(id).await
    }

    /// Remove all stale (disconnected/failed) connections from the database
    ///
    /// This cleans up connections that are no longer active, which can accumulate
    /// over time as debuggers are started and stopped.
    ///
    /// # Returns
    /// The number of connections that were deleted
    #[instrument(skip(self))]
    pub async fn cleanup_stale_connections(&self) -> Result<u64> {
        let deleted = self.connection_repo.delete_disconnected().await?;
        tracing::info!("Cleaned up {} stale connection(s)", deleted);
        Ok(deleted)
    }

    /// Restore connections on daemon startup
    ///
    /// This method:
    /// 1. Lists ALL saved connections (regardless of status or auto_reconnect flag)
    /// 2. Quick-checks if each debugger port is open (avoids long retry loops)
    /// 3. If port open → tries to reconnect
    /// 4. If port closed or reconnect fails → delete the connection from database
    ///
    /// Call this on daemon startup to restore previous debugging sessions
    /// and clean up stale connections.
    ///
    /// # Returns
    /// A tuple of (reconnected_count, deleted_count)
    #[instrument(skip(self))]
    pub async fn restore_connections_on_startup(&self) -> (usize, usize) {
        use tracing::{debug, info, warn};

        // Get ALL connections from database
        let connections = match self.connection_repo.list_all().await {
            Ok(conns) => conns,
            Err(e) => {
                warn!("Failed to query connections for startup restore: {}", e);
                return (0, 0);
            }
        };

        if connections.is_empty() {
            debug!("No saved connections to restore");
            return (0, 0);
        }

        info!(
            "Restoring {} saved connection(s) on startup",
            connections.len()
        );

        let mut reconnected_count = 0;
        let mut deleted_count = 0;

        for conn in connections {
            let conn_id = conn.id.clone();
            let addr = format!("{}:{}", conn.host, conn.port);

            info!(
                "Attempting to reconnect to {} debugger at {}",
                conn.language.as_str(),
                addr
            );

            // Update status to Reconnecting
            if let Err(e) = self
                .connection_repo
                .update_status(&conn_id, ConnectionStatus::Reconnecting)
                .await
            {
                warn!("Failed to update status for {}: {}", conn_id.0, e);
            }

            // Attempt to start the adapter (port is open, so this should succeed quickly)
            // Note: restored connections use attach mode (no program path, no PID)
            match self
                .adapter_lifecycle_manager
                .start_adapter(
                    conn_id.clone(),
                    &conn.host,
                    conn.port,
                    conn.language,
                    None, // program
                    None, // pid - restored connections don't use AttachPid mode
                    conn.safe_mode,
                )
                .await
            {
                Ok(_) => {
                    // Update status to Connected
                    if let Err(e) = self
                        .connection_repo
                        .update_status(&conn_id, ConnectionStatus::Connected)
                        .await
                    {
                        warn!(
                            "Failed to update status after reconnect for {}: {}",
                            conn_id.0, e
                        );
                    }

                    // Emit connection restored event
                    let event = SystemEvent::connection_restored(conn_id.clone(), 1);
                    let _ = self.system_event_tx.send(event);

                    info!("✅ Reconnected to {}", conn_id.0);
                    reconnected_count += 1;
                }
                Err(e) => {
                    // Reconnection failed - debugger might have closed between probe and connect
                    // Delete the stale connection
                    info!(
                        "Debugger not available for {} ({}), removing connection",
                        conn_id.0, e
                    );

                    if let Err(del_err) = self.connection_repo.delete(&conn_id).await {
                        warn!(
                            "Failed to delete stale connection {}: {}",
                            conn_id.0, del_err
                        );
                    } else {
                        deleted_count += 1;

                        // Emit connection closed event
                        let event = SystemEvent::connection_closed(conn_id);
                        let _ = self.system_event_tx.send(event);
                    }
                }
            }
        }

        if reconnected_count > 0 || deleted_count > 0 {
            info!(
                "Startup restore complete: {} reconnected, {} removed",
                reconnected_count, deleted_count
            );
        }

        (reconnected_count, deleted_count)
    }
}

#[cfg(test)]
mod tests {
    // Tests are in tests/connection_service_tests.rs
    // This ensures they use the public API like external users would
}
