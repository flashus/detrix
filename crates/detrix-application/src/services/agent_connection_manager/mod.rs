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

use std::time::Duration;

use detrix_core::ConnectionId;
use detrix_logging::{error, info, warn};

use self::helpers::{extract_request_id, semver_compare, short_id, SemverCmp};
use self::types::AgentInfo;

impl AgentConnectionManager {
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
            let binary_basename = binary
                .binary_path
                .rsplit('/')
                .next()
                .unwrap_or(&binary.binary_path);
            let name = format!("agent/{}/{}", short_id(&agent_id), binary_basename);
            let identity = detrix_core::ConnectionIdentity::new(
                &name,
                detrix_core::SourceLanguage::Go,
                "/",
                &hostname,
            );
            identities.push((
                identity,
                binary.binary_path.clone(),
                0u16,
                true, // safe_mode
            ));
        }

        // Create event channels BEFORE enqueuing CreateConnection
        for (identity, _host, _port, _safe) in &identities {
            let conn_id = ConnectionId(identity.to_uuid());
            let (event_tx, event_rx) = tokio::sync::mpsc::channel(1024);
            self.event_channels.insert(conn_id.clone(), Some(event_tx));
            self.event_receivers.insert(conn_id, Some(event_rx));
        }

        let agent_info = AgentInfo {
            agent_id: agent_id.clone(),
            hostname: hostname.clone(),
            capabilities,
            binaries: binaries.clone(),
            outgoing_tx: outgoing_tx.clone(),
            connected_at: std::time::Instant::now(),
        };

        self.agents.insert(agent_id.clone(), agent_info);

        // Enqueue RegisterAck
        let _ = outgoing_tx.send(OutgoingAgentMessage::RegisterAck {
            accepted: true,
            rejection_reason: String::new(),
            min_compatible_version: min_version.cloned().unwrap_or_default(),
        });

        // Enqueue CreateConnection for each binary
        for (identity, host, port, safe_mode) in &identities {
            let conn_id = ConnectionId(identity.to_uuid());
            let _ = outgoing_tx.send(OutgoingAgentMessage::CreateConnection {
                connection_id: conn_id.0.clone(),
                language: "go".to_string(),
                binary_path: identity.name.clone(),
                host: host.clone(),
                port: *port as u32,
                safe_mode: *safe_mode,
            });
        }

        // Pre-compute connection IDs for post-batch routing
        let conn_ids: Vec<ConnectionId> = identities
            .iter()
            .map(|(identity, _, _, _)| ConnectionId(identity.to_uuid()))
            .collect();

        // ── Release write lock (drop guard) ──

        // ── No lock: SQLite batch ──
        match self
            .connection_service
            .create_connections_batch(identities)
            .await
        {
            Ok(_) => {
                // Populate routing table only after successful commit
                for conn_id in conn_ids {
                    self.connection_to_agent
                        .insert(conn_id.clone(), agent_id.clone());
                    self.connection_requests
                        .insert(conn_id, std::collections::HashSet::new());
                }
            }
            Err(e) => {
                error!(
                    agent_id = %agent_id,
                    error = %e,
                    "Agent registration: SQLite batch save failed (in-memory state valid, will retry on reconnect)"
                );
            }
        }

        Ok(RegisterResult::Accepted)
    }

    /// Dispatch a decoded IncomingAgentMessage.
    /// Called from the gRPC read loop after registration.
    pub async fn dispatch(&self, agent_id: &str, msg: IncomingAgentMessage) {
        match msg {
            IncomingAgentMessage::ConnectionUpdate {
                connection_id,
                status,
                error: _,
            } => {
                if let Err(e) = self
                    .connection_service
                    .connection_repo()
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
                if let Some(entry) = self.event_channels.get(&connection_id) {
                    if let Some(tx) = entry.value() {
                        for event in events {
                            if tx.send(event).await.is_err() {
                                break;
                            }
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
            }
            IncomingAgentMessage::RegisterUpdate { binaries } => {
                self.handle_register_update(agent_id, binaries).await;
            }
            IncomingAgentMessage::Pong => {
                self.resolve_pending("_ping_", IncomingAgentMessage::Pong);
            }
            IncomingAgentMessage::Error { code, message } => {
                warn!(agent_id = %agent_id, code = %code, message = %message, "Agent error");
            }
        }
    }

    /// Handle mid-session re-registration (scanner detected changes).
    async fn handle_register_update(&self, agent_id: &str, binaries: Vec<AgentBinaryInfo>) {
        let Some(mut entry) = self.agents.get_mut(agent_id) else {
            warn!(agent_id = %agent_id, "RegisterUpdate for unknown agent");
            return;
        };

        let current_paths: std::collections::HashMap<&str, &AgentBinaryInfo> = entry
            .binaries
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
            let binary_basename = removed_path.rsplit('/').next().unwrap_or(removed_path);
            let name = format!("agent/{}/{}", short_id(agent_id), binary_basename);
            let identity = detrix_core::ConnectionIdentity::new(
                &name,
                detrix_core::SourceLanguage::Go,
                "/",
                &entry.hostname,
            );
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
                .connection_service
                .connection_repo()
                .update_status(&conn_id, detrix_core::ConnectionStatus::Disconnected)
                .await
            {
                warn!(connection_id = %conn_id.0, error = %e, "Failed to disconnect removed connection");
            }
            if let Some(ref tx) = self.system_event_tx {
                let _ = tx.send(detrix_core::SystemEvent::connection_closed(conn_id.clone()));
            }

            // e. Remove from routing table
            self.connection_to_agent.remove(&conn_id);
        }

        // Handle added: register new connections
        for binary in &added {
            let binary_basename = binary
                .binary_path
                .rsplit('/')
                .next()
                .unwrap_or(&binary.binary_path);
            let name = format!("agent/{}/{}", short_id(agent_id), binary_basename);
            let identity = detrix_core::ConnectionIdentity::new(
                &name,
                detrix_core::SourceLanguage::Go,
                "/",
                &entry.hostname,
            );
            let conn_id = ConnectionId(identity.to_uuid());

            // Create event channel
            let (event_tx, event_rx) = tokio::sync::mpsc::channel(1024);
            self.event_channels.insert(conn_id.clone(), Some(event_tx));
            self.event_receivers.insert(conn_id.clone(), Some(event_rx));
            self.connection_requests
                .insert(conn_id.clone(), std::collections::HashSet::new());

            // Enqueue CreateConnection
            let _ = entry.outgoing_tx.send(OutgoingAgentMessage::CreateConnection {
                connection_id: conn_id.0.clone(),
                language: "go".to_string(),
                binary_path: binary.binary_path.clone(),
                host: binary.binary_path.clone(),
                port: 0,
                safe_mode: true,
            });

            // Save to DB
            let identities = vec![(
                identity,
                binary.binary_path.clone(),
                0u16,
                true,
            )];
            if let Err(e) = self
                .connection_service
                .create_connections_batch(identities)
                .await
            {
                error!(connection_id = %conn_id.0, error = %e, "Failed to save added connection");
                continue;
            }

            self.connection_to_agent
                .insert(conn_id, agent_id.to_string());
        }

        // Update agent's binary list
        entry.binaries = binaries;
    }

    /// Returns true if this connection_id was created by an agent.
    /// Used by AdapterLifecycleManager.start_adapter().
    pub fn is_agent_managed(&self, id: &ConnectionId) -> bool {
        self.connection_to_agent.contains_key(id)
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
                    .map_err(|_| {
                        detrix_core::Error::Adapter("Agent stream closed".to_string())
                    })?;
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

        // Track request for cancel_pending_for_connection
        if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
            set.insert(request_id.clone());
        }
        self.pending_requests.insert(request_id.clone(), tx);

        // Send to agent
        self.send_to_agent(connection_id, msg).await?;

        // Await response
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => {
                // Cleanup tracking
                if let Some(mut set) = self.connection_requests.get_mut(connection_id) {
                    set.remove(&request_id);
                }
                self.pending_requests.remove(&request_id);
                T::try_from(response)
            }
            Ok(Err(_)) => {
                // Channel dropped (agent disconnected)
                self.pending_requests.remove(&request_id);
                Err(detrix_core::Error::Adapter(
                    "Agent disconnected while waiting for response".to_string(),
                ))
            }
            Err(_) => {
                // Timeout
                self.pending_requests.remove(&request_id);
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
        let Some(_agent_info) = self.agents.remove(agent_id).map(|(_, v)| v) else {
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
                .connection_service
                .connection_repo()
                .update_status(&conn_id, detrix_core::ConnectionStatus::Disconnected)
                .await
            {
                warn!(connection_id = %conn_id.0, error = %e, "Failed to disconnect on agent unregister");
            }
            if let Some(ref tx) = self.system_event_tx {
                let _ = tx.send(detrix_core::SystemEvent::connection_closed(conn_id.clone()));
            }

            // e. Remove from routing table
            self.connection_to_agent.remove(&conn_id);
        }
    }

    /// Resolve a pending request with the given response.
    fn resolve_pending(&self, request_id: &str, response: IncomingAgentMessage) {
        // Find the connection_id for this request to clean up tracking
        let mut found_conn: Option<ConnectionId> = None;
        for entry in self.connection_requests.iter() {
            if entry.value().contains(request_id) {
                found_conn = Some(entry.key().clone());
                break;
            }
        }

        if let Some((_, tx)) = self.pending_requests.remove(request_id) {
            let _ = tx.send(response);
        }

        // Clean up tracking
        if let Some(conn_id) = found_conn {
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
    use super::types::OutgoingAgentMessage;

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
    fn test_short_id() {
        assert_eq!(short_id("abc1234567890"), "abc12345");
        assert_eq!(short_id("ab"), "ab");
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
        };
        assert_eq!(extract_request_id(&set_metric), Some("test-1"));

        let ping = OutgoingAgentMessage::Ping;
        assert_eq!(extract_request_id(&ping), None);
    }
}
