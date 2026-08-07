//! Agent Connection Manager
//!
//! Manages agent connections on the server side. Speaks only in domain types —
//! no proto types cross into this module. Proto conversion happens at the gRPC
//! boundary in `detrix-api`.
//!
//! # Key Design
//!
//! - `register_atomic`: write-lock for in-memory state, then SQLite batch outside lock.
//!   `connection_to_agent` is only populated after the transaction commits.
//! - `connection_requests`: separate DashMap tracking active request_ids per connection,
//!   avoiding write-guard contention on AgentInfo.
//! - `cancel_pending_for_connection`: resolves in-flight oneshots immediately so
//!   RemoteAdapters fail fast instead of timing out.

mod helpers;
mod types;

pub use types::{
    AgentBinaryInfo, AgentCapabilities, AgentConnectionManager, AgentConnectionManagerRef,
    AgentOutgoingTx, IncomingAgentMessage, OutgoingAgentMessage, RegisterResult, VariableInfo,
};

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use detrix_core::{connection::AGENT_NAME_PREFIX, Connection, ConnectionId, SourceLanguage};
use detrix_logging::{error, info, warn};
use detrix_ports::ConnectionRepositoryRef;

use self::helpers::{extract_request_id, semver_compare, SemverCmp};
use self::types::AgentInfo;

/// Build the persisted identity for an agent-managed binary.
///
/// `agent_id` identifies a live stream and may change when the agent loses its
/// state file. It must not participate in the connection identity: metrics are
/// keyed by the observed host and binary, so they survive agent replacement.
fn agent_connection_identity(
    binary_path: &str,
    language: SourceLanguage,
    hostname: &str,
) -> detrix_core::ConnectionIdentity {
    let stable_binary_path = binary_path.trim_start_matches('/');
    let name = format!("{AGENT_NAME_PREFIX}{stable_binary_path}");
    detrix_core::ConnectionIdentity::new(&name, language, "/", hostname)
}

fn incoming_connection_id(msg: &IncomingAgentMessage) -> Option<&ConnectionId> {
    match msg {
        IncomingAgentMessage::ConnectionUpdate { connection_id, .. }
        | IncomingAgentMessage::EventBatch { connection_id, .. }
        | IncomingAgentMessage::DropCount { connection_id, .. } => Some(connection_id),
        _ => None,
    }
}

fn incoming_request_id(msg: &IncomingAgentMessage) -> Option<&str> {
    match msg {
        IncomingAgentMessage::SetMetricAck { request_id, .. }
        | IncomingAgentMessage::RemoveMetricAck { request_id, .. }
        | IncomingAgentMessage::FileResponse { request_id, .. }
        | IncomingAgentMessage::InspectResponse { request_id, .. } => Some(request_id),
        _ => None,
    }
}

impl AgentConnectionManager {
    pub async fn set_adapter_lifecycle_manager(
        &self,
        mgr: Arc<crate::services::AdapterLifecycleManager>,
    ) {
        *self.adapter_lifecycle_manager.write().await = Some(mgr);
    }

    async fn adapter_lifecycle_manager(
        &self,
    ) -> Option<Arc<crate::services::AdapterLifecycleManager>> {
        self.adapter_lifecycle_manager.read().await.clone()
    }

    /// Get the connection repository reference (for testing and status queries).
    pub fn connection_repo(&self) -> &ConnectionRepositoryRef {
        &self.connection_repo
    }

    /// Atomically register a connected agent, upsert its connections, and
    /// enqueue RegisterAck + CreateConnection messages.
    ///
    /// Called from the gRPC handler before entering the read loop.
    ///
    /// # Lock-split design
    ///
    /// Steps 1-6 hold a write lock on the agents DashMap (in-memory only).
    /// Steps 7-9 run outside the lock (SQLite batch, routing table updates).
    /// If the SQLite transaction fails, in-memory state remains valid and
    /// the next reconnect retries the DB write.
    pub async fn register_atomic(
        &self,
        agent_id: String,
        hostname: String,
        agent_version: String,
        capabilities: AgentCapabilities,
        binaries: Vec<AgentBinaryInfo>,
        outgoing_tx: AgentOutgoingTx,
    ) -> Result<RegisterResult, detrix_core::Error> {
        // ── Write lock: in-memory only ──
        let min_version = self
            .agent_config
            .as_ref()
            .and_then(|c| c.min_compatible_agent_version.as_ref());

        if let Some(min_ver) = min_version {
            match semver_compare(&agent_version, min_ver) {
                SemverCmp::Incompatible { reason } => {
                    let _ = outgoing_tx.send(OutgoingAgentMessage::RegisterAck {
                        accepted: false,
                        rejection_reason: reason.clone(),
                        min_compatible_version: min_ver.clone(),
                    });
                    return Ok(RegisterResult::Rejected { reason });
                }
                SemverCmp::Compatible => {}
            }
        }

        // Pre-compute connection identities for batch save
        let mut identities = Vec::with_capacity(binaries.len());
        for binary in &binaries {
            let identity =
                agent_connection_identity(&binary.binary_path, binary.language, &hostname);
            identities.push((
                identity,
                binary.binary_path.clone(),
                true, // safe_mode
            ));
        }

        // Pre-compute connection IDs for post-batch routing
        let conn_ids: Vec<ConnectionId> = identities
            .iter()
            .map(|(identity, _, _)| ConnectionId(identity.to_uuid()))
            .collect();

        // ── Release write lock (drop guard) ──

        // ── No lock: SQLite batch ──
        let mut connections = Vec::with_capacity(conn_ids.len());
        for (identity, binary_path, safe) in &identities {
            let mut conn = detrix_core::Connection::new_with_identity(
                identity.clone(),
                binary_path.clone(),
                0,
            )
            .map_err(|e| {
                detrix_core::Error::Adapter(format!("Invalid connection identity: {e}"))
            })?;
            conn.safe_mode = *safe;
            conn.hostname = hostname.clone();
            connections.push(conn);
        }

        if let Err(e) = self.connection_repo.save_batch(&connections).await {
            error!(
                agent_id = %agent_id,
                error = %e,
                "Agent registration: SQLite batch save failed"
            );
            // Do not advertise success or install in-memory indexes when the
            // durable registration did not commit. Otherwise the stream can
            // receive RegisterAck{accepted:true} while status/events have no
            // durable connection and a reconnect can inherit stale channels.
            return Err(e);
        }

        for conn in &connections {
            self.cleanup_stale_connections(conn).await;
        }

        // Install all in-memory state only after the durable batch succeeds.
        // Use entry()'s Vacant arm so reconnects preserve existing event
        // channels while failed first registrations cannot leave orphans.
        for conn_id in &conn_ids {
            use dashmap::mapref::entry::Entry;
            if let Entry::Vacant(e) = self.event_channels.entry(conn_id.clone()) {
                let (event_tx, event_rx) = tokio::sync::mpsc::channel(1024);
                e.insert(Some(event_tx));
                self.event_receivers.insert(conn_id.clone(), Some(event_rx));
            }
            self.connection_to_agent
                .insert(conn_id.clone(), agent_id.clone());
            self.connection_requests
                .insert(conn_id.clone(), std::collections::HashSet::new());
        }

        self.agents.insert(
            agent_id.clone(),
            AgentInfo {
                agent_id: agent_id.clone(),
                hostname: hostname.clone(),
                capabilities,
                binaries: binaries.clone(),
                outgoing_tx: outgoing_tx.clone(),
                connected_at: std::time::Instant::now(),
            },
        );

        let _ = outgoing_tx.send(OutgoingAgentMessage::RegisterAck {
            accepted: true,
            rejection_reason: String::new(),
            min_compatible_version: min_version.cloned().unwrap_or_default(),
        });

        // Only publish CreateConnection after the database commit and
        // routing-table update. The agent can report Connected as soon as it
        // receives this frame.
        for (identity, binary_path, safe_mode) in &identities {
            let conn_id = ConnectionId(identity.to_uuid());
            let _ = outgoing_tx.send(OutgoingAgentMessage::CreateConnection {
                connection_id: conn_id.0.clone(),
                language: identity.language.as_str().to_string(),
                binary_path: binary_path.clone(),
                host: hostname.clone(),
                port: 0,
                safe_mode: *safe_mode,
            });
        }

        Ok(RegisterResult::Accepted)
    }

    /// Dispatch a decoded IncomingAgentMessage from the current gRPC session.
    ///
    /// The session channel is part of the authorization context. Stable agent
    /// IDs survive reconnects, so an old stream must not continue dispatching
    /// after a replacement stream has registered under the same ID.
    pub async fn dispatch(
        &self,
        agent_id: &str,
        session_tx: &AgentOutgoingTx,
        msg: IncomingAgentMessage,
    ) {
        let current = self
            .agents
            .get(agent_id)
            .is_some_and(|info| info.outgoing_tx.same_channel(session_tx));
        if !current {
            warn!(agent_id = %agent_id, "Ignoring message from stale agent stream");
            return;
        }

        // A bearer token authenticates an agent, but does not by itself prove
        // that the agent owns an arbitrary connection or request ID. Enforce
        // ownership at this boundary before any state, event, or pending
        // request can be mutated.
        if let Some(connection_id) = incoming_connection_id(&msg) {
            if !self.owns_connection(agent_id, connection_id) {
                warn!(
                    agent_id = %agent_id,
                    connection_id = %connection_id.0,
                    "Ignoring agent message for a connection owned by another agent"
                );
                return;
            }
        }
        if let Some(request_id) = incoming_request_id(&msg) {
            let owns_request = self
                .request_to_connection
                .get(request_id)
                .map(|connection_id| self.owns_connection(agent_id, connection_id.value()))
                .unwrap_or(false);
            if !owns_request {
                warn!(
                    agent_id = %agent_id,
                    request_id = %request_id,
                    "Ignoring agent response for an unknown or foreign request"
                );
                return;
            }
        }

        match msg {
            IncomingAgentMessage::ConnectionUpdate {
                connection_id,
                status,
                error: _,
            } => {
                let was_connected = matches!(status, detrix_core::ConnectionStatus::Connected);
                let was_disconnected = matches!(
                    status,
                    detrix_core::ConnectionStatus::Disconnected
                        | detrix_core::ConnectionStatus::Failed(_)
                );

                if let Some(adapter_mgr) = self.adapter_lifecycle_manager().await {
                    // Atomic claim: `starting_adapters.insert` returns true only for the first
                    // concurrent caller, preventing duplicate start_adapter() races.
                    if was_connected
                        && !adapter_mgr.has_adapter(&connection_id).await
                        && self.starting_adapters.insert(connection_id.clone())
                    {
                        match self.connection_repo.find_by_id(&connection_id).await {
                            Ok(Some(conn)) => {
                                let result = adapter_mgr
                                    .start_adapter(
                                        connection_id.clone(),
                                        &conn.host,
                                        conn.port,
                                        conn.language,
                                        None,
                                        None,
                                        conn.safe_mode,
                                    )
                                    .await;
                                self.starting_adapters.remove(&connection_id);
                                if let Err(e) = result {
                                    warn!(
                                        connection_id = %connection_id.0,
                                        error = %e,
                                        "Failed to start RemoteAdapter after agent reported connected"
                                    );
                                    if let Err(update_err) = self
                                        .connection_repo
                                        .update_status(
                                            &connection_id,
                                            detrix_core::ConnectionStatus::Failed(e.to_string()),
                                        )
                                        .await
                                    {
                                        warn!(
                                            connection_id = %connection_id.0,
                                            error = %update_err,
                                            "Failed to mark connection as failed after RemoteAdapter start error"
                                        );
                                    }
                                    return;
                                }
                            }
                            Ok(None) => {
                                self.starting_adapters.remove(&connection_id);
                                warn!(
                                    connection_id = %connection_id.0,
                                    "Agent reported connected for unknown connection"
                                );
                            }
                            Err(e) => {
                                self.starting_adapters.remove(&connection_id);
                                warn!(
                                    connection_id = %connection_id.0,
                                    error = %e,
                                    "Failed to load connection after agent reported connected"
                                );
                                return;
                            }
                        }
                    } else if was_disconnected && adapter_mgr.has_adapter(&connection_id).await {
                        if let Err(e) = adapter_mgr.stop_adapter(&connection_id).await {
                            warn!(
                                connection_id = %connection_id.0,
                                error = %e,
                                "Failed to stop RemoteAdapter after agent disconnect"
                            );
                        }
                    }
                }

                if let Err(e) = self
                    .connection_repo
                    .update_status(&connection_id, status.clone())
                    .await
                {
                    warn!(
                        connection_id = %connection_id.0,
                        error = %e,
                        "Failed to update connection status from agent"
                    );
                }

                if let Some(ref tx) = self.system_event_tx {
                    let event = match status {
                        detrix_core::ConnectionStatus::Disconnected => {
                            detrix_core::SystemEvent::connection_closed(connection_id)
                        }
                        detrix_core::ConnectionStatus::Failed(ref msg) => {
                            // No connection_failed event; use connection_closed
                            let _ = msg;
                            detrix_core::SystemEvent::connection_closed(connection_id)
                        }
                        _ => return,
                    };
                    let _ = tx.send(event);
                }
            }
            IncomingAgentMessage::EventBatch {
                connection_id,
                events,
            } => {
                // Clone the sender before awaiting. Holding a DashMap guard
                // while a bounded channel is full can block unrelated state
                // operations on the same shard indefinitely.
                let tx = self
                    .event_channels
                    .get(&connection_id)
                    .and_then(|entry| entry.value().as_ref().cloned());
                if let Some(tx) = tx {
                    for event in events {
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                }
            }
            IncomingAgentMessage::SetMetricAck {
                ref request_id,
                verified,
                actual_line,
                ref error,
            } => {
                self.resolve_pending(
                    request_id,
                    IncomingAgentMessage::SetMetricAck {
                        request_id: request_id.clone(),
                        verified,
                        actual_line,
                        error: error.clone(),
                    },
                );
            }
            IncomingAgentMessage::RemoveMetricAck {
                ref request_id,
                confirmed,
                ref error,
            } => {
                self.resolve_pending(
                    request_id,
                    IncomingAgentMessage::RemoveMetricAck {
                        request_id: request_id.clone(),
                        confirmed,
                        error: error.clone(),
                    },
                );
            }
            IncomingAgentMessage::FileResponse {
                ref request_id,
                ref content,
                ref error,
            } => {
                self.resolve_pending(
                    request_id,
                    IncomingAgentMessage::FileResponse {
                        request_id: request_id.clone(),
                        content: content.clone(),
                        error: error.clone(),
                    },
                );
            }
            IncomingAgentMessage::InspectResponse {
                ref request_id,
                ref variables,
                ref error,
            } => {
                self.resolve_pending(
                    request_id,
                    IncomingAgentMessage::InspectResponse {
                        request_id: request_id.clone(),
                        variables: variables.clone(),
                        error: error.clone(),
                    },
                );
            }
            IncomingAgentMessage::DropCount {
                connection_id,
                total_events_dropped,
            } => {
                info!(
                    connection_id = %connection_id.0,
                    dropped = total_events_dropped,
                    "Agent dropped events"
                );
            }
            IncomingAgentMessage::Heartbeat {
                cpu,
                memory_bytes,
                active_probes,
                uptime_secs,
                events_forwarded,
                events_dropped,
            } => {
                tracing::debug!(
                    agent_id = %agent_id,
                    cpu = cpu,
                    memory = memory_bytes,
                    probes = active_probes,
                    uptime = uptime_secs,
                    forwarded = events_forwarded,
                    dropped = events_dropped,
                    "Agent heartbeat"
                );
                // Update liveness for every connection this agent manages so that
                // RemoteAdapter::ensure_connected() does not mark idle (but live)
                // connections as stale.
                for entry in self.connection_to_agent.iter() {
                    if entry.value() == agent_id {
                        self.record_liveness(entry.key());
                    }
                }
            }
            IncomingAgentMessage::RegisterUpdate { binaries } => {
                self.handle_register_update(agent_id, binaries).await;
            }
            IncomingAgentMessage::Pong => {
                // Drain all pending pings for this agent — any Pong satisfies all waiters.
                if let Some((_, waiters)) = self.pending_pings.remove(agent_id) {
                    for (_, tx) in waiters {
                        let _ = tx.send(());
                    }
                }
            }
            IncomingAgentMessage::Error { code, message } => {
                warn!(agent_id = %agent_id, code = %code, message = %message, "Agent error");
            }
        }
    }

    fn owns_connection(&self, agent_id: &str, connection_id: &ConnectionId) -> bool {
        self.connection_to_agent
            .get(connection_id)
            .is_some_and(|owner| owner.value() == agent_id)
    }

    /// Handle mid-session re-registration (scanner detected changes).
    async fn handle_register_update(&self, agent_id: &str, binaries: Vec<AgentBinaryInfo>) {
        // Clone fields needed across await points, then drop the shard lock before any await.
        let (current_binaries, hostname, outgoing_tx) = {
            let Some(entry) = self.agents.get(agent_id) else {
                warn!(agent_id = %agent_id, "RegisterUpdate for unknown agent");
                return;
            };
            (
                entry.binaries.clone(),
                entry.hostname.clone(),
                entry.outgoing_tx.clone(),
            )
        }; // DashMap shard lock released here — no guard held across awaits below

        let current_paths: std::collections::HashMap<&str, &AgentBinaryInfo> = current_binaries
            .iter()
            .map(|b| (b.binary_path.as_str(), b))
            .collect();
        let new_paths: std::collections::HashMap<&str, &AgentBinaryInfo> = binaries
            .iter()
            .map(|b| (b.binary_path.as_str(), b))
            .collect();

        // Find added binaries
        let added: Vec<&AgentBinaryInfo> = binaries
            .iter()
            .filter(|b| !current_paths.contains_key(b.binary_path.as_str()))
            .collect();

        // Find removed binaries
        let removed_paths: Vec<String> = current_paths
            .keys()
            .filter(|p| !new_paths.contains_key(*p))
            .map(|s| s.to_string())
            .collect();

        // Handle removed: cancel pending, cleanup, disconnect
        for removed_path in &removed_paths {
            let removed_binary = current_paths[removed_path.as_str()];
            let identity =
                agent_connection_identity(removed_path, removed_binary.language, &hostname);
            let conn_id = ConnectionId(identity.to_uuid());

            // a. Cancel in-flight requests
            self.cancel_pending_for_connection(&conn_id);

            // b. Remove event channel and receiver
            self.event_channels.remove(&conn_id);
            self.event_receivers.remove(&conn_id);

            // c. Remove connection requests
            self.connection_requests.remove(&conn_id);

            // d. Mark disconnected + emit event
            if let Err(e) = self
                .connection_repo
                .update_status(&conn_id, detrix_core::ConnectionStatus::Disconnected)
                .await
            {
                warn!(connection_id = %conn_id.0, error = %e, "Failed to disconnect removed connection");
            }
            if let Some(ref tx) = self.system_event_tx {
                let _ = tx.send(detrix_core::SystemEvent::connection_closed(conn_id.clone()));
            }

            // e. Remove from routing table and liveness tracking
            self.connection_to_agent.remove(&conn_id);
            self.liveness_timestamps.remove(&conn_id);
        }

        // Handle added: register new connections
        for binary in &added {
            let identity =
                agent_connection_identity(&binary.binary_path, binary.language, &hostname);
            let conn_id = ConnectionId(identity.to_uuid());
            let language = identity.language.as_str().to_string();

            // Save to DB
            let mut conn =
                detrix_core::Connection::new_with_identity(identity, binary.binary_path.clone(), 0)
                    .map_err(|e| {
                        error!(error = %e, "Failed to create connection identity");
                        detrix_core::Error::Adapter(format!("Invalid connection identity: {e}"))
                    });
            if let Ok(ref mut c) = conn {
                c.safe_mode = true;
                c.hostname = hostname.clone();
                if let Err(e) = self
                    .connection_repo
                    .save_batch(std::slice::from_ref(c))
                    .await
                {
                    error!(connection_id = %conn_id.0, error = %e, "Failed to save added connection");
                    continue;
                }
                self.cleanup_stale_connections(c).await;
            } else {
                continue;
            }

            // Install routing and event state only after persistence succeeds;
            // a failed scanner update must not leave an unreachable channel.
            let (event_tx, event_rx) = tokio::sync::mpsc::channel(1024);
            self.event_channels.insert(conn_id.clone(), Some(event_tx));
            self.event_receivers.insert(conn_id.clone(), Some(event_rx));
            self.connection_requests
                .insert(conn_id.clone(), std::collections::HashSet::new());
            self.connection_to_agent
                .insert(conn_id.clone(), agent_id.to_string());

            // Publish only after persistence and ownership registration. The
            // agent may report Connected immediately after receiving this.
            let _ = outgoing_tx.send(OutgoingAgentMessage::CreateConnection {
                connection_id: conn_id.0,
                language,
                binary_path: binary.binary_path.clone(),
                host: hostname.clone(),
                port: 0,
                safe_mode: true,
            });
        }

        // Re-acquire briefly to update binaries — all awaits are complete.
        if let Some(mut e) = self.agents.get_mut(agent_id) {
            e.binaries = binaries;
        }
    }

    /// Returns true if this connection_id was created by an agent.
    /// Used by AdapterLifecycleManager.start_adapter().
    pub fn is_agent_managed(&self, id: &ConnectionId) -> bool {
        self.connection_to_agent.contains_key(id)
    }

    async fn cleanup_stale_connections(&self, connection: &Connection) {
        let stale_ids = match self
            .connection_repo
            .find_stale_same_project(
                connection.name.as_deref().unwrap_or_default(),
                connection.language.as_str(),
                &connection.workspace_root,
                &connection.id,
            )
            .await
        {
            Ok(ids) => ids,
            Err(e) => {
                warn!(
                    connection_id = %connection.id.0,
                    error = %e,
                    "Failed to look up stale agent connections"
                );
                return;
            }
        };

        for stale_id in stale_ids {
            match self
                .metric_repo
                .migrate_connection_id(&stale_id, &connection.id)
                .await
            {
                Ok(migrated) if migrated > 0 => {
                    info!(
                        from = %stale_id.0,
                        to = %connection.id.0,
                        migrated,
                        "Migrated metrics from stale agent connection"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(
                        stale_id = %stale_id.0,
                        error = %e,
                        "Failed to migrate metrics from stale agent connection"
                    );
                }
            }

            if let Err(e) = self.connection_repo.delete(&stale_id).await {
                warn!(
                    stale_id = %stale_id.0,
                    error = %e,
                    "Failed to delete stale agent connection"
                );
                continue;
            }

            self.cancel_pending_for_connection(&stale_id);
            self.event_channels.remove(&stale_id);
            self.event_receivers.remove(&stale_id);
            self.connection_requests.remove(&stale_id);
            self.connection_to_agent.remove(&stale_id);
            self.liveness_timestamps.remove(&stale_id);

            if let Some(ref tx) = self.system_event_tx {
                let _ = tx.send(detrix_core::SystemEvent::connection_closed(stale_id));
            }
        }
    }

    /// Route a OutgoingAgentMessage to the agent owning connection_id.
    pub async fn send_to_agent(
        &self,
        connection_id: &ConnectionId,
        msg: OutgoingAgentMessage,
    ) -> Result<(), detrix_core::Error> {
        if let Some(agent_id) = self.connection_to_agent.get(connection_id) {
            if let Some(agent_info) = self.agents.get(agent_id.value()) {
                agent_info
                    .outgoing_tx
                    .send(msg)
                    .map_err(|_| detrix_core::Error::Adapter("Agent stream closed".to_string()))?;
                return Ok(());
            }
        }
        Err(detrix_core::Error::NotConnected(
            "Agent not found for connection".to_string(),
        ))
    }

    /// Send a command and await the response via a oneshot channel.
    pub async fn send_and_await<T>(
        &self,
        connection_id: &ConnectionId,
        msg: OutgoingAgentMessage,
        timeout: Duration,
    ) -> Result<T, detrix_core::Error>
    where
        T: TryFrom<IncomingAgentMessage, Error = detrix_core::Error>,
    {
        let request_id = extract_request_id(&msg)
            .ok_or_else(|| {
                detrix_core::Error::Adapter("OutgoingAgentMessage has no request_id".to_string())
            })?
            .to_string();

        let (tx, rx) = tokio::sync::oneshot::channel();

        // Track request for cancel_pending_for_connection (#4 fix: cleanup on ALL exit paths).
        if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
            set.insert(request_id.clone());
        }
        self.request_to_connection
            .insert(request_id.clone(), connection_id.clone());
        self.pending_requests.insert(request_id.clone(), tx);

        if let Err(e) = self.send_to_agent(connection_id, msg).await {
            // Send failed — clean up all tracking maps to prevent leaks.
            self.pending_requests.remove(&request_id);
            self.request_to_connection.remove(&request_id);
            if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
                set.remove(&request_id);
            }
            return Err(e);
        }

        // Await response
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => {
                // Cleanup tracking
                if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
                    set.remove(&request_id);
                }
                self.request_to_connection.remove(&request_id);
                self.pending_requests.remove(&request_id);
                T::try_from(response)
            }
            Ok(Err(_)) => {
                // Channel dropped (agent disconnected) — clean up ALL tracking maps (#4).
                self.pending_requests.remove(&request_id);
                self.request_to_connection.remove(&request_id);
                if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
                    set.remove(&request_id);
                }
                Err(detrix_core::Error::Adapter(
                    "Agent disconnected while waiting for response".to_string(),
                ))
            }
            Err(_) => {
                // Timeout
                self.pending_requests.remove(&request_id);
                self.request_to_connection.remove(&request_id);
                if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
                    set.remove(&request_id);
                }
                Err(detrix_core::Error::Adapter(
                    "Request timed out waiting for agent response".to_string(),
                ))
            }
        }
    }

    /// Send a command and await the raw IncomingAgentMessage response.
    /// Unlike `send_and_await`, this doesn't require a TryFrom conversion.
    pub async fn send_and_await_raw(
        &self,
        connection_id: &ConnectionId,
        msg: OutgoingAgentMessage,
        timeout: Duration,
    ) -> Result<IncomingAgentMessage, detrix_core::Error> {
        let request_id = extract_request_id(&msg)
            .ok_or_else(|| {
                detrix_core::Error::Adapter("OutgoingAgentMessage has no request_id".to_string())
            })?
            .to_string();

        let (tx, rx) = tokio::sync::oneshot::channel();

        if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
            set.insert(request_id.clone());
        }
        self.request_to_connection
            .insert(request_id.clone(), connection_id.clone());
        self.pending_requests.insert(request_id.clone(), tx);

        if let Err(e) = self.send_to_agent(connection_id, msg).await {
            // Send failed before the agent received the message — clean up all tracking maps.
            self.pending_requests.remove(&request_id);
            self.request_to_connection.remove(&request_id);
            if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
                set.remove(&request_id);
            }
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => {
                if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
                    set.remove(&request_id);
                }
                self.request_to_connection.remove(&request_id);
                self.pending_requests.remove(&request_id);
                Ok(response)
            }
            Ok(Err(_)) => {
                // Channel dropped (agent disconnected) — clean up ALL tracking maps (#4).
                self.pending_requests.remove(&request_id);
                self.request_to_connection.remove(&request_id);
                if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
                    set.remove(&request_id);
                }
                Err(detrix_core::Error::Adapter(
                    "Agent disconnected while waiting for response".to_string(),
                ))
            }
            Err(_) => {
                self.pending_requests.remove(&request_id);
                self.request_to_connection.remove(&request_id);
                if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
                    set.remove(&request_id);
                }
                Err(detrix_core::Error::Adapter(
                    "Request timed out waiting for agent response".to_string(),
                ))
            }
        }
    }

    /// Get or create an event receiver for connection_id.
    /// The channel is created during register_atomic before CreateConnection
    /// is sent — so this is always available by the time start_adapter()
    /// constructs a RemoteAdapter.
    pub fn subscribe_events(
        &self,
        connection_id: &ConnectionId,
    ) -> Option<tokio::sync::mpsc::Receiver<detrix_core::MetricEvent>> {
        if let Some((_, rx)) = self.event_receivers.remove(connection_id) {
            rx
        } else {
            None
        }
    }

    /// Re-create the event channel for a connection, replacing any stale entries.
    ///
    /// Must be called before constructing a new `RemoteAdapter` for a connection
    /// that may have had a previous adapter (i.e. on adapter restart). This ensures
    /// `subscribe_events()` always finds a fresh receiver, even though the previous
    /// call consumed the old one via `.remove()`.
    pub fn refresh_event_channel(&self, connection_id: &ConnectionId) {
        let (tx, rx) = tokio::sync::mpsc::channel::<detrix_core::MetricEvent>(1024);
        self.event_channels.insert(connection_id.clone(), Some(tx));
        self.event_receivers.insert(connection_id.clone(), Some(rx));
    }

    /// Record a liveness proof for a connection (heartbeat or successful round-trip).
    ///
    /// Called from the heartbeat handler (for all connections of an agent) and from
    /// `RemoteAdapter::confirm_alive()` (after successful request/response).
    pub fn record_liveness(&self, connection_id: &ConnectionId) {
        self.liveness_timestamps
            .insert(connection_id.clone(), std::time::Instant::now());
    }

    /// Return how long ago the last liveness proof was recorded for a connection.
    ///
    /// Returns `None` if no liveness proof has been recorded yet (new connection).
    /// `RemoteAdapter::ensure_connected()` uses this to detect stale connections.
    pub fn liveness_age(&self, connection_id: &ConnectionId) -> Option<std::time::Duration> {
        self.liveness_timestamps
            .get(connection_id)
            .map(|t| t.elapsed())
    }

    /// Resolve all in-flight pending_requests for a connection with an immediate error.
    /// Uses connection_requests DashMap — no AgentInfo lookup, no cross-await guard held.
    fn cancel_pending_for_connection(&self, connection_id: &ConnectionId) {
        if let Some(mut entry) = self.connection_requests.get_mut(connection_id) {
            let request_ids: Vec<String> = entry.drain().collect();
            for request_id in request_ids {
                if let Some((_, tx)) = self.pending_requests.remove(&request_id) {
                    let _ = tx.send(IncomingAgentMessage::Error {
                        code: "CONNECTION_REMOVED".to_string(),
                        message: "connection removed by scanner update".to_string(),
                    });
                }
            }
        }
    }

    /// Called on gRPC stream close. Marks all owned connections Disconnected.
    pub async fn unregister_agent(&self, agent_id: &str) {
        self.unregister_agent_matching(agent_id, None).await;
    }

    /// Unregister only the stream that currently owns `agent_id`.
    ///
    /// Agent IDs are intentionally stable across reconnects. Without matching
    /// the stream channel, an old read task can finish after a reconnect and
    /// tear down the replacement agent's connections.
    pub async fn unregister_agent_if_current(&self, agent_id: &str, session_tx: &AgentOutgoingTx) {
        self.unregister_agent_matching(agent_id, Some(session_tx))
            .await;
    }

    async fn unregister_agent_matching(
        &self,
        agent_id: &str,
        session_tx: Option<&AgentOutgoingTx>,
    ) {
        let removed = match session_tx {
            Some(session_tx) => self.agents.remove_if(agent_id, |_, info| {
                info.outgoing_tx.same_channel(session_tx)
            }),
            None => self.agents.remove(agent_id),
        };
        let Some((_agent_id, _agent_info)) = removed else {
            return;
        };

        info!(agent_id = %agent_id, "Agent unregistered");

        // Find all connections owned by this agent
        let conn_ids: Vec<ConnectionId> = self
            .connection_to_agent
            .iter()
            .filter(|e| e.value() == agent_id)
            .map(|e| e.key().clone())
            .collect();

        for conn_id in conn_ids {
            // a. Cancel in-flight requests
            self.cancel_pending_for_connection(&conn_id);

            // b. Remove event channel and receiver
            self.event_channels.remove(&conn_id);
            self.event_receivers.remove(&conn_id);

            // c. Remove connection requests
            self.connection_requests.remove(&conn_id);

            // d. Mark disconnected + emit event
            if let Err(e) = self
                .connection_repo
                .update_status(&conn_id, detrix_core::ConnectionStatus::Disconnected)
                .await
            {
                warn!(connection_id = %conn_id.0, error = %e, "Failed to disconnect on agent unregister");
            }
            if let Some(ref tx) = self.system_event_tx {
                let _ = tx.send(detrix_core::SystemEvent::connection_closed(conn_id.clone()));
            }

            // e. Remove from routing table and liveness tracking
            self.connection_to_agent.remove(&conn_id);
            self.liveness_timestamps.remove(&conn_id);
        }

        // f. Drop any pending ping waiters — they'll see channel-closed error.
        self.pending_pings.remove(agent_id);
    }

    /// Send a Ping to the named agent and await a Pong response.
    ///
    /// Multiple concurrent callers are supported — each pushes a sender into
    /// `pending_pings[agent_id]`; any single Pong from that agent resolves ALL
    /// of them. This replaces the former hardcoded `"_ping_"` key that caused
    /// concurrent pings to clobber each other (#11 fix).
    pub async fn ping_agent(
        &self,
        agent_id: &str,
        timeout: Duration,
    ) -> Result<(), detrix_core::Error> {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        // Unique ID lets us remove exactly our waiter without relying on Vec::pop()
        // which is unsafe under concurrent callers.
        let ping_id = self.ping_counter.fetch_add(1, Ordering::Relaxed);

        // Register waiter before sending Ping to avoid a race where Pong arrives
        // before we insert.
        self.pending_pings
            .entry(agent_id.to_string())
            .or_default()
            .insert(ping_id, tx);

        let sent = self
            .agents
            .get(agent_id)
            .map(|a| a.outgoing_tx.send(OutgoingAgentMessage::Ping).is_ok())
            .unwrap_or(false);

        if !sent {
            // Remove exactly our waiter by ID — safe under concurrent callers.
            if let Some(waiters) = self.pending_pings.get(agent_id) {
                waiters.remove(&ping_id);
            }
            return Err(detrix_core::Error::Adapter(
                "agent not found or channel closed".to_string(),
            ));
        }

        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| {
                if let Some(waiters) = self.pending_pings.get(agent_id) {
                    waiters.remove(&ping_id);
                }
                detrix_core::Error::Adapter("ping timed out".to_string())
            })
            .and_then(|r| {
                r.map_err(|_| {
                    if let Some(waiters) = self.pending_pings.get(agent_id) {
                        waiters.remove(&ping_id);
                    }
                    detrix_core::Error::Adapter("ping channel dropped".to_string())
                })
            })
    }

    /// Resolve a pending request with the given response.
    ///
    /// Uses the `request_to_connection` reverse index for O(1) connection lookup (#10),
    /// eliminating the former O(N) scan over all `connection_requests` entries.
    fn resolve_pending(&self, request_id: &str, response: IncomingAgentMessage) {
        // O(1) reverse lookup — was O(N) scan before adding request_to_connection.
        let conn_id = self
            .request_to_connection
            .remove(request_id)
            .map(|(_, c)| c);

        if let Some((_, tx)) = self.pending_requests.remove(request_id) {
            let _ = tx.send(response);
        }

        if let Some(conn_id) = conn_id {
            if let Some(mut set) = self.connection_requests.get_mut(&conn_id) {
                set.remove(request_id);
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::helpers::*;
    use super::types::{AgentInfo, OutgoingAgentMessage};
    use super::*;
    use detrix_testing::{MockConnectionRepository, MockMetricRepository};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn test_manager() -> AgentConnectionManager {
        AgentConnectionManager::new(
            Arc::new(MockConnectionRepository::new()),
            Arc::new(MockMetricRepository::new()),
            None,
            None,
        )
    }

    #[test]
    fn test_semver_compare_compatible() {
        assert!(matches!(
            semver_compare("1.3.0", "1.3.0"),
            SemverCmp::Compatible
        ));
        assert!(matches!(
            semver_compare("1.4.0", "1.3.0"),
            SemverCmp::Compatible
        ));
        assert!(matches!(
            semver_compare("1.3.5", "1.3.0"),
            SemverCmp::Compatible
        ));
    }

    #[test]
    fn test_semver_compare_incompatible_major() {
        let result = semver_compare("2.0.0", "1.3.0");
        assert!(matches!(result, SemverCmp::Incompatible { .. }));
        if let SemverCmp::Incompatible { reason } = result {
            assert!(reason.contains("Major version mismatch"));
        }
    }

    #[test]
    fn test_semver_compare_incompatible_minor() {
        let result = semver_compare("1.2.0", "1.3.0");
        assert!(matches!(result, SemverCmp::Incompatible { .. }));
        if let SemverCmp::Incompatible { reason } = result {
            assert!(reason.contains("Minor version too low"));
        }
    }

    #[test]
    fn test_semver_compare_invalid_version() {
        let result = semver_compare("bad", "1.3.0");
        assert!(matches!(result, SemverCmp::Incompatible { .. }));
    }

    #[test]
    fn test_extract_request_id() {
        let set_metric = OutgoingAgentMessage::SetMetric {
            request_id: "test-1".to_string(),
            connection_id: "conn".to_string(),
            metric_name: "metric".to_string(),
            file: "file.go".to_string(),
            line: 10,
            expressions: vec![],
            enabled: true,
            metric_id: 123,
        };
        assert_eq!(extract_request_id(&set_metric), Some("test-1"));

        let ping = OutgoingAgentMessage::Ping;
        assert_eq!(extract_request_id(&ping), None);
    }

    #[test]
    fn connection_ownership_is_scoped_to_the_registered_agent() {
        let manager = test_manager();
        let connection_id = ConnectionId("connection-1".to_string());
        manager
            .connection_to_agent
            .insert(connection_id.clone(), "agent-a".to_string());

        assert!(manager.owns_connection("agent-a", &connection_id));
        assert!(!manager.owns_connection("agent-b", &connection_id));
        assert!(!manager.owns_connection("agent-a", &ConnectionId("missing".to_string())));
    }

    #[tokio::test]
    async fn stale_agent_stream_cannot_unregister_replacement_stream() {
        let manager = test_manager();
        let (old_tx, _old_rx) = mpsc::unbounded_channel();
        let (new_tx, _new_rx) = mpsc::unbounded_channel();
        manager.agents.insert(
            "agent-a".to_string(),
            AgentInfo {
                agent_id: "agent-a".to_string(),
                hostname: "host".to_string(),
                capabilities: AgentCapabilities::default(),
                binaries: Vec::new(),
                outgoing_tx: new_tx.clone(),
                connected_at: std::time::Instant::now(),
            },
        );

        manager
            .unregister_agent_if_current("agent-a", &old_tx)
            .await;
        assert!(manager.agents.contains_key("agent-a"));

        manager
            .unregister_agent_if_current("agent-a", &new_tx)
            .await;
        assert!(!manager.agents.contains_key("agent-a"));
    }

    #[tokio::test]
    async fn stale_agent_stream_cannot_replace_current_registration_state() {
        let manager = test_manager();
        let (old_tx, _old_rx) = mpsc::unbounded_channel();
        let (new_tx, _new_rx) = mpsc::unbounded_channel();
        let binary = AgentBinaryInfo {
            binary_path: "/proc/10/exe".to_string(),
            pid: 10,
            inode: 10,
            build_info: String::new(),
            has_dwarf: true,
            exported_functions: Vec::new(),
            language: detrix_core::SourceLanguage::Go,
        };
        manager.agents.insert(
            "agent-a".to_string(),
            AgentInfo {
                agent_id: "agent-a".to_string(),
                hostname: "host".to_string(),
                capabilities: AgentCapabilities::default(),
                binaries: vec![binary],
                outgoing_tx: new_tx.clone(),
                connected_at: std::time::Instant::now(),
            },
        );

        manager
            .dispatch(
                "agent-a",
                &old_tx,
                IncomingAgentMessage::RegisterUpdate { binaries: vec![] },
            )
            .await;
        assert_eq!(manager.agents.get("agent-a").unwrap().binaries.len(), 1);

        manager
            .dispatch(
                "agent-a",
                &new_tx,
                IncomingAgentMessage::RegisterUpdate { binaries: vec![] },
            )
            .await;
        assert!(manager.agents.get("agent-a").unwrap().binaries.is_empty());
    }
}
