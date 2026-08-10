//! RemoteAdapter — proxies DapAdapter trait calls to a remote agent.
//!
//! Implements `DapAdapter` by sending commands to the agent via
//! `AgentConnectionManager` and awaiting responses. The existing
//! `AdapterLifecycleManager` is unaware that this adapter is remote.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;
use detrix_core::{ConnectionId, Error, Metric, MetricEvent, Result};
use detrix_ports::DapAdapter;

use super::agent_connection_manager::{
    AgentConnectionManagerRef, IncomingAgentMessage, OutgoingAgentMessage,
};
use super::circuit_breaker::CircuitBreaker;

// ============================================================================
// RemoteAdapter
// ============================================================================

pub struct RemoteAdapter {
    connection_id: ConnectionId,
    agent_manager: AgentConnectionManagerRef,
    event_rx: Mutex<Option<tokio::sync::mpsc::Receiver<MetricEvent>>>,
    circuit: CircuitBreaker,
    /// Monotonic liveness timestamp; updated on any liveness proof (Pong, SetMetricAck, etc.).
    /// Stored as `Mutex<Option<Instant>>` to use the monotonic clock and avoid SystemTime
    /// jump-backward / jump-forward hazards from NTP or manual clock changes.
    last_confirmed_at: Mutex<Option<Instant>>,
    /// Monotonic counter for generating unique request_ids.
    request_counter: AtomicUsize,
}

fn scoped_request_id(connection_id: &ConnectionId, sequence: usize) -> String {
    format!("req-{}-{sequence}", connection_id.0)
}

impl RemoteAdapter {
    fn next_request_id(&self) -> String {
        let n = self.request_counter.fetch_add(1, Ordering::Relaxed);
        scoped_request_id(&self.connection_id, n)
    }

    pub fn new(connection_id: ConnectionId, agent_manager: AgentConnectionManagerRef) -> Self {
        // Subscribe to events — the channel was pre-created during register_atomic.
        let event_rx = agent_manager.subscribe_events(&connection_id);

        Self {
            connection_id,
            agent_manager,
            event_rx: Mutex::new(event_rx),
            circuit: CircuitBreaker::new(),
            last_confirmed_at: Mutex::new(None),
            request_counter: AtomicUsize::new(0),
        }
    }

    /// Record a liveness proof timestamp.
    ///
    /// Updates both the manager's shared `liveness_timestamps` (queried by
    /// `ensure_connected()`) and the local `last_confirmed_at` field.
    fn confirm_alive(&self) {
        self.agent_manager.record_liveness(&self.connection_id);
        if let Ok(mut guard) = self.last_confirmed_at.lock() {
            *guard = Some(Instant::now());
        }
    }

    /// Send a command and await the response, updating liveness on success.
    async fn send_and_await<T>(
        &self,
        msg: OutgoingAgentMessage,
        timeout: std::time::Duration,
    ) -> Result<T>
    where
        T: TryFrom<IncomingAgentMessage>,
    {
        let result = self.send_and_await_raw(msg, timeout).await?;
        T::try_from(result)
            .map_err(|_| Error::Adapter("Unexpected response type from agent".to_string()))
    }

    /// Send a command and await the raw IncomingAgentMessage (no type conversion).
    async fn send_and_await_raw(
        &self,
        msg: OutgoingAgentMessage,
        timeout: std::time::Duration,
    ) -> Result<IncomingAgentMessage> {
        let result = self
            .agent_manager
            .send_and_await_raw(&self.connection_id, msg, timeout)
            .await?;
        self.confirm_alive();
        Ok(result)
    }
}

// ============================================================================
// DapAdapter Implementation
// ============================================================================

#[async_trait]
impl DapAdapter for RemoteAdapter {
    /// Start the adapter.
    /// For agent-managed connections the server already received ConnectionUpdate(Connected),
    /// so startup only needs to confirm the routing entry still exists.
    async fn start(&self) -> Result<()> {
        if self.agent_manager.is_agent_managed(&self.connection_id) {
            self.confirm_alive();
            Ok(())
        } else {
            Err(Error::Adapter(
                "Agent connection is no longer routed".to_string(),
            ))
        }
    }

    /// Stop the adapter — send CloseConnection; fire-and-forget.
    async fn stop(&self) -> Result<()> {
        let _ = self
            .agent_manager
            .send_to_agent(
                &self.connection_id,
                OutgoingAgentMessage::CloseConnection {
                    connection_id: self.connection_id.0.clone(),
                },
            )
            .await;
        Ok(())
    }

    /// Ensure connected — verify the connection is still routed AND recently active.
    ///
    /// A stale connection (no liveness proof for > 60 s, i.e. 2× the default
    /// 30 s heartbeat interval) is treated as unhealthy even if still in the
    /// routing table, because the agent may be dead but not yet unregistered.
    ///
    /// Liveness is tracked in the shared `AgentConnectionManager::liveness_timestamps`
    /// map — updated by heartbeats and by `confirm_alive()` after successful round-trips.
    /// This method intentionally does NOT call `confirm_alive()` so that it cannot
    /// reset the stale timer on its own.
    async fn ensure_connected(&self) -> Result<()> {
        if self.circuit.is_open() {
            return Err(Error::Adapter("agent circuit open".to_string()));
        }

        if !self.agent_manager.is_agent_managed(&self.connection_id) {
            return Err(Error::Adapter("agent connection is not routed".to_string()));
        }

        // Reject connections whose last liveness proof is older than 60 s.
        // `None` means no proof recorded yet (new connection) — skip the check.
        const STALE_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(60);
        let is_stale = self
            .agent_manager
            .liveness_age(&self.connection_id)
            .map(|age| age > STALE_THRESHOLD)
            .unwrap_or(false);
        if is_stale {
            return Err(Error::Adapter(
                "agent connection is stale (no liveness proof)".to_string(),
            ));
        }

        Ok(())
    }

    fn is_connected(&self) -> bool {
        // Use peek_open (no side effects) — is_connected is a status query only.
        // The Open → HalfOpen transition is intentionally deferred to ensure_connected.
        !self.circuit.peek_open() && self.agent_manager.is_agent_managed(&self.connection_id)
    }

    /// Set a metric — routed through circuit breaker with 30s timeout.
    async fn set_metric(&self, metric: &Metric) -> Result<detrix_ports::SetMetricResult> {
        self.circuit
            .call(|| {
                self.send_and_await::<IncomingAgentMessage>(
                    OutgoingAgentMessage::SetMetric {
                        request_id: self.next_request_id(),
                        connection_id: self.connection_id.0.clone(),
                        metric_name: metric.name.clone(),
                        file: metric.location.file.clone(),
                        line: metric.location.line,
                        expressions: metric.expressions.clone(),
                        enabled: metric.enabled,
                        metric_id: metric.id.unwrap_or(detrix_core::MetricId(0)).0,
                    },
                    std::time::Duration::from_secs(30),
                )
            })
            .await
            .map(|resp| match resp {
                IncomingAgentMessage::SetMetricAck {
                    verified,
                    actual_line,
                    error,
                    ..
                } => {
                    if let Some(err) = error {
                        detrix_ports::SetMetricResult {
                            verified: false,
                            line: metric.location.line,
                            message: Some(err),
                        }
                    } else {
                        detrix_ports::SetMetricResult {
                            verified,
                            line: actual_line.unwrap_or(metric.location.line),
                            message: None,
                        }
                    }
                }
                _ => detrix_ports::SetMetricResult {
                    verified: false,
                    line: metric.location.line,
                    message: Some("Unexpected response".to_string()),
                },
            })
    }

    /// Remove a metric — routed through circuit breaker with 30s timeout.
    async fn remove_metric(&self, metric: &Metric) -> Result<detrix_ports::RemoveMetricResult> {
        self.circuit
            .call(|| {
                self.send_and_await::<IncomingAgentMessage>(
                    OutgoingAgentMessage::RemoveMetric {
                        request_id: self.next_request_id(),
                        connection_id: self.connection_id.0.clone(),
                        metric_name: metric.name.clone(),
                    },
                    std::time::Duration::from_secs(30),
                )
            })
            .await
            .map(|resp| match resp {
                IncomingAgentMessage::RemoveMetricAck {
                    confirmed, error, ..
                } => detrix_ports::RemoveMetricResult::new(confirmed, error),
                _ => detrix_ports::RemoveMetricResult::new(
                    false,
                    Some("Unexpected response".to_string()),
                ),
            })
    }

    /// Subscribe to metric events from the agent.
    async fn subscribe_events(&self) -> Result<tokio::sync::mpsc::Receiver<MetricEvent>> {
        let mut guard = self
            .event_rx
            .lock()
            .map_err(|e| Error::Adapter(format!("Event receiver lock poisoned: {e}")))?;
        guard
            .take()
            .ok_or_else(|| Error::Adapter("Event receiver already consumed".to_string()))
    }

    /// Continue execution — no-op for eBPF (uprobes never pause).
    async fn continue_execution(&self) -> Result<bool> {
        Ok(true)
    }

    fn get_drop_count(&self, _metric_name: &str) -> Result<u64> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_scoped_to_connection() {
        let a = scoped_request_id(&ConnectionId("connection-a".to_string()), 0);
        let b = scoped_request_id(&ConnectionId("connection-b".to_string()), 0);
        assert_ne!(a, b);
        assert!(a.contains("connection-a"));
        assert!(b.contains("connection-b"));
    }
}
