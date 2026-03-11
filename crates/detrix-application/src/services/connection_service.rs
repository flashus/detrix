//! ConnectionService - Use case for managing debugger connections (debugpy, delve, lldb-dap)
//!
//! Following Clean Architecture, this service:
//! - Contains business logic for connection management
//! - Depends on domain traits (ConnectionRepository)
//! - Delegates adapter lifecycle to AdapterLifecycleManager
//! - Is protocol-agnostic (no knowledge of gRPC, REST, MCP, etc.)

use crate::services::AdapterLifecycleManager;
use crate::{
    ConnectionReferenceRepositoryRef, ConnectionRepositoryRef, MetricRepositoryRef, VfsRef,
};
use detrix_core::{
    connection_reference::{ClientIdentity, ConnectionReference, ReferenceKind},
    Connection, ConnectionId, ConnectionIdentity, ConnectionStatus, Result, SystemEvent,
};
use detrix_logging::{debug, info, instrument};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Service for managing debugger connections
///
/// This service handles the business logic for:
/// - Creating new connections to debugger servers (debugpy, delve, lldb-dap)
/// - Disconnecting from debugger servers
/// - Listing and querying connections
/// - Reference counting for multi-user safety
///
/// Uses dependency injection via trait objects for testability.
/// Delegates adapter lifecycle management to AdapterLifecycleManager.
pub struct ConnectionService {
    /// Repository for persisting connections
    connection_repo: ConnectionRepositoryRef,

    /// Repository for persisting metrics (needed for cascade delete)
    metric_repo: MetricRepositoryRef,

    /// Repository for connection reference counting (multi-user safety)
    reference_repo: ConnectionReferenceRepositoryRef,

    /// Lifecycle manager for DAP adapters
    adapter_lifecycle_manager: Arc<AdapterLifecycleManager>,

    /// System event broadcast channel for connection events
    system_event_tx: broadcast::Sender<SystemEvent>,

    /// Virtual file system for caching remote files
    vfs: VfsRef,
}

impl ConnectionService {
    /// Create a new ConnectionService
    ///
    /// # Arguments
    /// * `connection_repo` - Repository for connection persistence
    /// * `metric_repo` - Repository for metric persistence (for cascade delete)
    /// * `reference_repo` - Repository for connection reference counting (multi-user safety)
    /// * `adapter_lifecycle_manager` - Manager for DAP adapter lifecycle
    /// * `system_event_tx` - Broadcast channel for system events
    /// * `vfs` - Virtual file system for caching remote files
    pub fn new(
        connection_repo: ConnectionRepositoryRef,
        metric_repo: MetricRepositoryRef,
        reference_repo: ConnectionReferenceRepositoryRef,
        adapter_lifecycle_manager: Arc<AdapterLifecycleManager>,
        system_event_tx: broadcast::Sender<SystemEvent>,
        vfs: VfsRef,
    ) -> Self {
        Self {
            connection_repo,
            metric_repo,
            reference_repo,
            adapter_lifecycle_manager,
            system_event_tx,
            vfs,
        }
    }

    /// Create a new connection to a debugger server
    ///
    /// This method:
    /// 1. Validates connection identity and parameters
    /// 2. Generates deterministic UUID from identity
    /// 3. Checks for existing connection with same identity (idempotency)
    /// 4. Saves connection to repository
    /// 5. Delegates adapter lifecycle to AdapterLifecycleManager
    /// 6. Updates connection status on success
    ///
    /// # Arguments
    /// * `host` - Host address (e.g., "127.0.0.1", "localhost")
    /// * `port` - Port number (must be >= 1024)
    /// * `identity` - Connection identity (name, language, workspace_root, hostname)
    /// * `program` - Optional program path for Rust direct lldb-dap launch mode
    /// * `pid` - Optional PID for AttachPid mode
    /// * `safe_mode` - Enable SafeMode: only allow logpoints, disable breakpoint-based operations
    ///
    /// # Returns
    /// ConnectionId (which is the deterministic UUID) of the created connection
    ///
    /// # Business Rules
    /// - Port must be >= 1024 (not in reserved range)
    /// - Host must not be empty
    /// - Identity must be valid (non-empty fields)
    /// - Connection UUID is deterministic based on identity
    /// - Idempotent: same identity + connected adapter = return existing
    #[instrument(skip(self), fields(host = %host, port = port, name = %identity.name, language = %identity.language, workspace_root = %identity.workspace_root, hostname = %identity.hostname, pid = ?pid, safe_mode = safe_mode))]
    pub async fn create_connection(
        &self,
        host: String,
        port: u16,
        identity: ConnectionIdentity,
        program: Option<String>,
        pid: Option<u32>,
        safe_mode: bool,
    ) -> Result<ConnectionId> {
        self.create_connection_with_metadata(
            host, port, identity, program, pid, safe_mode, None, None, None, None,
        )
        .await
    }

    /// Create a new connection with optional cloud metadata
    ///
    /// Extended version of `create_connection` that also accepts:
    /// * `control_plane_url` - App control plane URL for transparent file fetching
    /// * `build_commit` - Git commit SHA at build time
    /// * `build_tag` - Build version tag
    /// * `created_by` - Client identity of the creator (from X-Detrix-Client-Id header)
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(self), fields(host = %host, port = port, name = %identity.name, language = %identity.language, workspace_root = %identity.workspace_root, hostname = %identity.hostname, pid = ?pid, safe_mode = safe_mode))]
    pub async fn create_connection_with_metadata(
        &self,
        host: String,
        port: u16,
        identity: ConnectionIdentity,
        program: Option<String>,
        pid: Option<u32>,
        safe_mode: bool,
        control_plane_url: Option<String>,
        build_commit: Option<String>,
        build_tag: Option<String>,
        created_by: Option<String>,
    ) -> Result<ConnectionId> {
        // 1. Create Connection entity from identity (validates identity, host, port, language + generates UUID)
        let mut connection = Connection::new_with_identity(identity, host, port)?;
        connection.safe_mode = safe_mode;
        connection.control_plane_url = control_plane_url;
        connection.build_commit = build_commit;
        connection.build_tag = build_tag;
        connection.created_by = created_by.clone();
        let connection_id = connection.id.clone();

        // 2. Check if connection with same UUID already exists and is connected
        //    UUID is deterministic from identity, so find_by_id is equivalent to find_by_identity
        //    This handles idempotency and the case where restore_connections is running
        if let Ok(Some(existing)) = self.connection_repo.find_by_id(&connection_id).await {
            let is_connected = existing.status == ConnectionStatus::Connected
                && self
                    .adapter_lifecycle_manager
                    .has_adapter(&existing.id)
                    .await;
            // Connecting = background start_adapter task is still running; return early to
            // avoid spawning a second concurrent task for the same connection.
            let is_starting = existing.status == ConnectionStatus::Connecting;

            if is_connected || is_starting {
                // Even if already connected/starting, ensure the caller holds a reference
                if let Some(ref client_id) = created_by {
                    let reference = ConnectionReference::new(
                        existing.id.clone(),
                        ClientIdentity::bridge(client_id),
                        ReferenceKind::Client,
                    );
                    if let Err(e) = self.reference_repo.add_reference(&reference).await {
                        tracing::warn!(
                            "Failed to add connection reference for {}: {e}",
                            existing.id.0
                        );
                    }
                }
                info!(
                    "Connection already exists (status={}, UUID={}), returning existing",
                    existing.status, connection_id.0
                );
                return Ok(existing.id);
            }
            // Connection exists but not connected/starting - will be restarted below
            info!(
                "Connection exists but not connected (UUID={}), will restart adapter",
                connection_id.0
            );
        }

        // 3. Save connection to repository (initially Disconnected, ON CONFLICT upserts)
        self.connection_repo.save(&connection).await?;

        // 3b. Migrate metrics from stale same-project connections to the new connection,
        //     then clean them up. This handles the container restart case where the hostname
        //     changes — existing metrics carry over to the new connection seamlessly.
        match self
            .connection_repo
            .find_stale_same_project(
                connection.name.as_deref().unwrap_or_default(),
                connection.language.as_str(),
                &connection.workspace_root,
                &connection_id,
            )
            .await
        {
            Ok(stale_ids) if !stale_ids.is_empty() => {
                for stale_id in &stale_ids {
                    // Migrate metrics first so they carry over to the new connection
                    match self
                        .metric_repo
                        .migrate_connection_id(stale_id, &connection_id)
                        .await
                    {
                        Ok(migrated) if migrated > 0 => {
                            info!(
                                from = %stale_id.0,
                                to = %connection_id.0,
                                migrated,
                                "Migrated metrics from stale connection (container restart)"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(
                                stale_id = %stale_id.0,
                                error = %e,
                                "Metric migration from stale connection failed (non-fatal)"
                            );
                        }
                    }
                    // Clean up the stale connection; delete_by_connection_id inside
                    // delete_connection finds 0 metrics since they were just migrated above.
                    if let Err(e) = self.delete_connection(stale_id).await {
                        tracing::warn!(
                            stale_id = %stale_id.0,
                            error = %e,
                            "Stale connection cleanup failed (non-fatal)"
                        );
                    }
                }
            }
            Ok(_) => {} // no stale connections
            Err(e) => {
                tracing::warn!(
                    connection_id = %connection_id.0,
                    error = %e,
                    "Stale same-project lookup failed (non-fatal)"
                );
            }
        }

        // 4. Start the DAP adapter.
        //
        //    AttachPid mode (pid.is_some()) — used by the Rust client with lldb-dap —
        //    triggers a ptrace(PTRACE_ATTACH) call that freezes ALL threads of the target
        //    process for 60–180 s on macOS while lldb-dap enumerates 400+ system dylibs.
        //    If we awaited start_adapter synchronously here, the target's HTTP server would
        //    be frozen and unable to send the HTTP 200 response for the POST /api/v1/connections
        //    request, causing the client to time out.  We therefore set the status to
        //    "Connecting" and spawn the adapter startup in a background task so that the
        //    HTTP response is returned before ptrace freezes the process.
        //
        //    All other modes (Attach/AttachRemote/LaunchProgram) connect in under 5 s and
        //    do not freeze the caller, so they run synchronously and return "Connected".
        if pid.is_some() {
            // --- Background path (AttachPid / lldb-dap only) ---
            self.connection_repo
                .update_status(&connection_id, ConnectionStatus::Connecting)
                .await?;

            // Clone Arc handles for the background task (all are cheap Arc clones)
            let adapter_lifecycle_manager_bg = self.adapter_lifecycle_manager.clone();
            let connection_repo_bg = self.connection_repo.clone();
            let reference_repo_bg = self.reference_repo.clone();
            let system_event_tx_bg = self.system_event_tx.clone();
            let connection_id_bg = connection_id.clone();
            let host_bg = connection.host.clone();
            let port_bg = connection.port;
            let language_bg = connection.language; // Copy
            let safe_mode_bg = connection.safe_mode; // Copy

            tokio::spawn(async move {
                info!(
                    connection_id = %connection_id_bg.0,
                    host = %host_bg,
                    port = port_bg,
                    pid = ?pid,
                    "Background adapter task started"
                );

                // 5. Start adapter (may block 60–420 s for ptrace dylib enumeration).
                //
                // Inner timeouts per stage:
                //   connect 30 s + initialize 30 s + attach 420 s (macOS) / 120 s (Linux)
                //   + configurationDone 30 s = 510 s (macOS) / 210 s (Linux) max.
                // The outer 540 s timeout is a safety net in case any inner timeout
                // fails to fire, guaranteeing the task terminates before the
                // 600 s wait_for_connection_connected polling window expires.
                let start_result = tokio::time::timeout(
                    std::time::Duration::from_secs(540),
                    adapter_lifecycle_manager_bg.start_adapter(
                        connection_id_bg.clone(),
                        &host_bg,
                        port_bg,
                        language_bg,
                        program,
                        pid,
                        safe_mode_bg,
                    ),
                )
                .await
                .unwrap_or_else(|_| {
                    tracing::error!(
                        connection_id = %connection_id_bg.0,
                        "Background adapter task outer timeout (540 s) exceeded — \
                         inner DAP timeouts did not fire; forcing Failed status"
                    );
                    Err(detrix_core::Error::Adapter(
                        "adapter start timed out after 540 s (outer safety limit)".to_string(),
                    ))
                });

                match start_result {
                    Ok(_) => {
                        // 6. Update connection status to Connected
                        if let Err(e) = connection_repo_bg
                            .update_status(&connection_id_bg, ConnectionStatus::Connected)
                            .await
                        {
                            tracing::warn!(
                                "Background adapter: failed to update {} to Connected: {e}",
                                connection_id_bg.0
                            );
                        }

                        // 7. Add client reference if created_by is provided
                        if let Some(ref client_id) = created_by {
                            let reference = ConnectionReference::new(
                                connection_id_bg.clone(),
                                ClientIdentity::bridge(client_id),
                                ReferenceKind::Client,
                            );
                            if let Err(e) = reference_repo_bg.add_reference(&reference).await {
                                tracing::warn!(
                                    connection_id = %connection_id_bg.0,
                                    client_id = %client_id,
                                    error = %e,
                                    "Background adapter: failed to add client reference (non-fatal)"
                                );
                            }
                        }

                        // 8. Emit connection created event
                        let event = SystemEvent::connection_created(
                            connection_id_bg.clone(),
                            &host_bg,
                            port_bg,
                            language_bg.as_str(),
                        );
                        if let Err(e) = system_event_tx_bg.send(event) {
                            tracing::warn!("No subscribers for connection_created event: {e}");
                        }

                        info!(
                            "Adapter started for connection {} (background task)",
                            connection_id_bg.0
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            connection_id = %connection_id_bg.0,
                            error = %e,
                            "Background adapter start failed"
                        );
                        if let Err(update_err) = connection_repo_bg
                            .update_status(
                                &connection_id_bg,
                                ConnectionStatus::Failed(e.to_string()),
                            )
                            .await
                        {
                            tracing::warn!(
                                "Background adapter: also failed to update status to Failed: {update_err}"
                            );
                        }
                    }
                }
            });
        } else {
            // --- Synchronous path (all non-AttachPid modes) ---
            self.adapter_lifecycle_manager
                .start_adapter(
                    connection_id.clone(),
                    &connection.host,
                    connection.port,
                    connection.language,
                    program,
                    pid,
                    connection.safe_mode,
                )
                .await?;

            // 5. Update connection status to Connected
            self.connection_repo
                .update_status(&connection_id, ConnectionStatus::Connected)
                .await?;

            // 6. Add client reference if created_by is provided
            if let Some(ref client_id) = created_by {
                let reference = ConnectionReference::new(
                    connection_id.clone(),
                    ClientIdentity::bridge(client_id),
                    ReferenceKind::Client,
                );
                if let Err(e) = self.reference_repo.add_reference(&reference).await {
                    tracing::warn!(
                        connection_id = %connection_id.0,
                        client_id = %client_id,
                        error = %e,
                        "Failed to add client reference (non-fatal)"
                    );
                }
            }

            // 7. Emit connection created event
            let event = SystemEvent::connection_created(
                connection_id.clone(),
                &connection.host,
                connection.port,
                connection.language.as_str(),
            );
            if let Err(e) = self.system_event_tx.send(event) {
                tracing::warn!("No subscribers for connection_created event: {e}");
            }
        }

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

        // 2. Mark VFS entries as stale (don't clear - connection may reconnect)
        self.vfs.mark_stale(&id.0);
        debug!("VFS entries marked stale for connection {}", id.0);

        // 3. Update status to Disconnected
        self.connection_repo
            .update_status(id, ConnectionStatus::Disconnected)
            .await?;

        // 4. Emit connection closed event
        let event = SystemEvent::connection_closed(id.clone());
        if let Err(e) = self.system_event_tx.send(event) {
            tracing::warn!("No subscribers for connection_closed event: {e}");
        }

        Ok(())
    }

    /// Delete a connection and all associated metrics and references.
    ///
    /// This method:
    /// 1. Stops the adapter via AdapterLifecycleManager
    /// 2. Removes all connection references (explicit, for mock compatibility alongside CASCADE)
    /// 3. Deletes all metrics associated with the connection
    /// 4. Deletes the connection from repository
    ///
    /// Use this for explicit user-requested deletion, not for disconnect/reconnection scenarios.
    ///
    /// # Arguments
    /// * `id` - Connection ID to delete
    ///
    /// # Business Rules
    /// - Stops the adapter and cleans up resources
    /// - Removes all references (multi-user safety cleanup)
    /// - Cascade deletes all associated metrics
    /// - Removes the connection from storage
    #[instrument(skip(self), fields(connection_id = %id.0))]
    pub async fn delete_connection(&self, id: &ConnectionId) -> Result<()> {
        // 1. Stop adapter via lifecycle manager (ignore error if not running)
        if let Err(e) = self.adapter_lifecycle_manager.stop_adapter(id).await {
            tracing::warn!(
                "Failed to stop adapter for connection {} (may not be running): {e}",
                id.0
            );
        }

        // 2. Clear VFS cache for this connection (permanent deletion)
        self.vfs.clear_connection(&id.0);
        debug!("VFS cache cleared for connection {}", id.0);

        // 3. Remove all connection references (explicit cleanup for mock compatibility)
        if let Err(e) = self.reference_repo.remove_all_by_connection(id).await {
            tracing::warn!("Failed to remove references for connection {}: {e}", id.0);
        }

        // 4. Delete all associated metrics (cascade delete)
        let deleted_metrics = self.metric_repo.delete_by_connection_id(id).await?;
        if deleted_metrics > 0 {
            info!(
                "Deleted {} metrics for connection {}",
                deleted_metrics, id.0
            );
        }

        // 5. Delete connection from repository
        self.connection_repo.delete(id).await?;

        // 6. Emit connection closed event
        let event = SystemEvent::connection_closed(id.clone());
        if let Err(e) = self.system_event_tx.send(event) {
            tracing::warn!("No subscribers for connection_closed event on delete: {e}");
        }

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

    /// Set the control plane URL on a connection (if not already set).
    ///
    /// Used by `wake` to propagate the app URL for VFS file fetching.
    /// No-op if the connection already has a control_plane_url or doesn't exist.
    pub async fn set_control_plane_url(&self, id: &ConnectionId, url: String) -> Result<()> {
        if let Some(mut conn) = self.connection_repo.find_by_id(id).await? {
            if conn.control_plane_url.is_none() {
                conn.control_plane_url = Some(url);
                self.connection_repo.update(&conn).await?;
            }
        }
        Ok(())
    }

    /// Remove stale connections based on TTL, respecting reference counts.
    ///
    /// This cleans up connections that have been inactive for more than `ttl_days` calendar days.
    /// If ttl_days < 0, no cleanup is performed (indefinite TTL).
    ///
    /// First cleans up stale references, then only deletes connections with zero remaining refs.
    ///
    /// # Arguments
    /// * `ttl_days` - Number of calendar days of inactivity before cleanup. Use -1 for indefinite.
    ///
    /// # Returns
    /// The number of connections that were deleted
    #[instrument(skip(self))]
    pub async fn cleanup_stale_connections(&self, ttl_days: i64) -> Result<u64> {
        if ttl_days < 0 {
            return Ok(0); // -1 = indefinite, skip cleanup
        }

        // First: cleanup stale references
        let stale_refs = self
            .reference_repo
            .cleanup_stale_references(ttl_days)
            .await?;
        if stale_refs > 0 {
            tracing::info!(
                stale_refs,
                ttl_days,
                "Cleaned up stale connection references"
            );
        }

        let now_micros = chrono::Utc::now().timestamp_micros();
        let all_connections = self.connection_repo.list_all().await?;
        let mut removed_count = 0;

        for conn in all_connections {
            if conn.inactive_for_days(ttl_days, now_micros) {
                // Only delete if no live references remain
                let ref_count = self
                    .reference_repo
                    .count_references(&conn.id)
                    .await
                    .unwrap_or(0);
                if ref_count > 0 {
                    tracing::debug!(
                        connection_id = %conn.id,
                        ref_count,
                        "Skipping stale connection: still has live references"
                    );
                    continue;
                }

                tracing::debug!(
                    connection_id = %conn.id,
                    last_active_days_ago = (now_micros - conn.last_active) / (86_400 * 1_000_000),
                    "Removing stale connection (zero references)"
                );

                // Disconnect (kills debuggers) then remove
                if let Err(e) = self.disconnect(&conn.id).await {
                    tracing::warn!("Failed to disconnect stale connection {}: {e}", conn.id.0);
                }
                self.connection_repo.delete(&conn.id).await?;
                removed_count += 1;
            }
        }

        if removed_count > 0 {
            tracing::info!(
                removed = removed_count,
                ttl_days,
                "Cleaned up stale connections"
            );
        }

        Ok(removed_count)
    }

    /// Capture the connections that existed at daemon launch time.
    ///
    /// Call this **before** starting the HTTP server.  The resulting snapshot is then
    /// passed to `restore_connections_on_startup`, which runs in a background task.
    /// Taking the snapshot early guarantees that connections created by in-flight client
    /// requests (after the server starts) are never included in the restore list.
    pub async fn list_connections_for_startup_restore(&self) -> Vec<Connection> {
        use detrix_logging::warn;
        match self.connection_repo.list_all().await {
            Ok(conns) => conns,
            Err(e) => {
                warn!(
                    "Failed to query connections for startup restore snapshot: {}",
                    e
                );
                vec![]
            }
        }
    }

    /// Restore connections on daemon startup.
    ///
    /// **IMPORTANT – call `list_connections_for_startup_restore` first** and pass the
    /// resulting snapshot here.  The snapshot must be captured *before* the HTTP server
    /// starts accepting client requests so that connections created by the current session
    /// are never included in the restore list.  This eliminates the startup-restore race:
    /// if the background task only ever touches connections that existed at daemon launch
    /// time, it cannot interfere with adapters started by incoming client requests.
    ///
    /// This method:
    /// 1. Processes only the provided snapshot (Disconnected / Reconnecting / Failed)
    /// 2. Quick-checks if each debugger port is open (avoids long retry loops)
    /// 3. If port open → tries to reconnect
    /// 4. If port closed or reconnect fails → delete the connection from database
    ///
    /// # Returns
    /// A tuple of (reconnected_count, deleted_count)
    #[instrument(skip(self, startup_connections), fields(snapshot_size = startup_connections.len()))]
    pub async fn restore_connections_on_startup(
        &self,
        startup_connections: Vec<Connection>,
    ) -> (usize, usize) {
        use detrix_logging::{debug, info, warn};

        let connections = startup_connections;

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

            // Skip connections that are already active — they were created during the current
            // session (race with this background task) and must not be interrupted.
            // This is the primary guard against the startup-restore race condition:
            // under heavy parallel load the background task may run after tests have already
            // connected, and stopping a running adapter would cause "No adapter found" errors.
            if matches!(
                conn.status,
                ConnectionStatus::Connected | ConnectionStatus::Connecting
            ) && self.adapter_lifecycle_manager.has_adapter(&conn_id).await
            {
                debug!(
                    "Skipping {} at {} — already active (status: {})",
                    conn_id.0, addr, conn.status
                );
                continue;
            }

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
                    if let Err(e) = self.system_event_tx.send(event) {
                        tracing::warn!("No subscribers for connection_restored event: {e}");
                    }

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
                        if let Err(e) = self.system_event_tx.send(event) {
                            tracing::warn!("No subscribers for connection_closed event: {e}");
                        }
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

    /// Disconnect connections owned by a specific client (user-scoped).
    ///
    /// Releases all of the caller's references, then disconnects any connections
    /// that have zero remaining references.
    ///
    /// # Arguments
    /// * `client_identity` - The client releasing their connections
    ///
    /// # Returns
    /// `(references_released, connections_disconnected)`
    #[instrument(skip(self))]
    pub async fn disconnect_all_connections(
        &self,
        client_identity: &ClientIdentity,
    ) -> Result<(u64, u64)> {
        self.release_all_client_references(client_identity).await
    }

    /// Release a single connection reference for a client.
    ///
    /// If remaining references reach zero, the connection is disconnected.
    ///
    /// # Returns
    /// `(was_released, was_disconnected)` — true if the reference existed and was removed,
    /// and true if the connection was disconnected due to zero remaining references.
    #[instrument(skip(self), fields(connection_id = %connection_id.0, client = %client_identity))]
    pub async fn release_reference(
        &self,
        connection_id: &ConnectionId,
        client_identity: &ClientIdentity,
    ) -> Result<(bool, bool)> {
        let (was_removed, remaining) = self
            .reference_repo
            .remove_reference_and_count(connection_id, client_identity)
            .await?;

        let was_disconnected = if was_removed && remaining == 0 {
            info!(
                connection_id = %connection_id.0,
                "Last reference removed, disconnecting connection"
            );
            self.disconnect(connection_id).await.is_ok()
        } else {
            false
        };

        Ok((was_removed, was_disconnected))
    }

    /// Release ALL references held by a client, disconnecting unreferenced connections.
    ///
    /// # Returns
    /// `(references_released, connections_disconnected)`
    #[instrument(skip(self), fields(client = %client_identity))]
    pub async fn release_all_client_references(
        &self,
        client_identity: &ClientIdentity,
    ) -> Result<(u64, u64)> {
        let results = self
            .reference_repo
            .remove_all_by_client_and_count(client_identity)
            .await?;

        let released = results.len() as u64;
        let mut disconnected = 0u64;

        for (conn_id, remaining) in &results {
            if *remaining == 0 && self.disconnect(conn_id).await.is_ok() {
                disconnected += 1;
            }
        }

        info!(released, disconnected, "Released client references");

        Ok((released, disconnected))
    }

    /// Explicitly attach a client to a connection (create a Client reference).
    ///
    /// Used when a client starts using a connection that was created by someone else.
    #[instrument(skip(self), fields(connection_id = %connection_id.0, client = %client_identity))]
    pub async fn attach_to_connection(
        &self,
        connection_id: &ConnectionId,
        client_identity: &ClientIdentity,
    ) -> Result<()> {
        // Verify connection exists
        self.connection_repo
            .find_by_id(connection_id)
            .await?
            .ok_or_else(|| {
                detrix_core::error::Error::NotConnected(format!(
                    "Connection {} not found",
                    connection_id.0
                ))
            })?;

        let reference = ConnectionReference::new(
            connection_id.clone(),
            client_identity.clone(),
            ReferenceKind::Client,
        );
        self.reference_repo.add_reference(&reference).await?;

        debug!("Client attached to connection");
        Ok(())
    }

    /// Add a daemon reference to a connection (for system/persistent references).
    #[instrument(skip(self), fields(connection_id = %connection_id.0))]
    pub async fn add_daemon_reference(&self, connection_id: &ConnectionId) -> Result<()> {
        let reference = ConnectionReference::new(
            connection_id.clone(),
            ClientIdentity::daemon(),
            ReferenceKind::Daemon,
        );
        self.reference_repo.add_reference(&reference).await
    }

    /// Get the number of active references for a connection.
    pub async fn get_reference_count(&self, connection_id: &ConnectionId) -> Result<u64> {
        self.reference_repo.count_references(connection_id).await
    }

    /// List all references for a connection.
    pub async fn list_references(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<Vec<ConnectionReference>> {
        self.reference_repo.find_by_connection(connection_id).await
    }

    /// Admin force-disconnect all connections, ignoring reference counts.
    ///
    /// This is the old behavior of `disconnect_all_connections` — disconnects everything
    /// and clears all references. Only callable from admin endpoints.
    #[instrument(skip(self))]
    pub async fn admin_disconnect_all(&self) -> Result<usize> {
        let all_connections = self.connection_repo.list_all().await?;
        let mut disconnected_count = 0;

        for conn in &all_connections {
            // Remove all references for this connection
            if let Err(e) = self.reference_repo.remove_all_by_connection(&conn.id).await {
                tracing::warn!(
                    "Failed to remove references for connection {} during admin disconnect: {e}",
                    conn.id.0
                );
            }
            if self.disconnect(&conn.id).await.is_ok() {
                disconnected_count += 1;
            }
        }

        Ok(disconnected_count)
    }
}

#[async_trait::async_trait]
impl crate::ports::ConnectionLookup for ConnectionService {
    async fn get_connection(&self, id: &ConnectionId) -> Result<Option<Connection>> {
        self.connection_repo.find_by_id(id).await
    }
}

#[cfg(test)]
mod tests {
    // Tests are in tests/connection_service_tests.rs
    // This ensures they use the public API like external users would
}
