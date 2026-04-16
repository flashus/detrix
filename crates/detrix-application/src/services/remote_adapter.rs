//! RemoteAdapter — proxies DapAdapter trait calls to a remote agent.
//!
//! Implements `DapAdapter` by sending commands to the agent via
//! `AgentConnectionManager` and awaiting responses. The existing
//! `AdapterLifecycleManager` is unaware that this adapter is remote.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

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
    drop_counts: Mutex<std::collections::HashMap<String, u64>>,
    /// Unix timestamp ms; updated on any liveness proof (Pong, SetMetricAck, RemoveMetricAck).
    last_confirmed_at: AtomicU64,
    /// Monotonic counter for generating unique request_ids.
    request_counter: AtomicUsize,
}

impl RemoteAdapter {
    fn next_request_id(&self) -> String {
        let n = self.request_counter.fetch_add(1, Ordering::Relaxed);
        format!("req-{n}")
    }

    pub fn new(connection_id: ConnectionId, agent_manager: AgentConnectionManagerRef) -> Self {
        // Subscribe to events — the channel was pre-created during register_atomic.
        let event_rx = agent_manager.subscribe_events(&connection_id);

        Self {
            connection_id,
            agent_manager,
            event_rx: Mutex::new(event_rx),
            circuit: CircuitBreaker::new(),
            drop_counts: Mutex::new(std::collections::HashMap::new()),
            last_confirmed_at: AtomicU64::new(0),
            request_counter: AtomicUsize::new(0),
        }
    }

    /// Record a liveness proof timestamp.
    fn confirm_alive(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.last_confirmed_at.store(now, Ordering::Relaxed);
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

    /// Update cached drop count from agent's DropCount message.
    pub fn update_drop_count(&self, metric_name: &str, count: u64) {
        if let Ok(mut map) = self.drop_counts.lock() {
            map.insert(metric_name.to_string(), count);
        }
    }
}

// ============================================================================
// DapAdapter Implementation
// ============================================================================

#[async_trait]
impl DapAdapter for RemoteAdapter {
    /// Start the adapter — wait for ConnectionUpdate(Connected) with 10s timeout.
    /// If already connected (registered before start_adapter was called), return immediately.
    async fn start(&self) -> Result<()> {
        // For agent connections, the adapter is "started" when the agent registers
        // and creates connections. We just need to verify the agent is responsive.
        // A quick Ping confirms liveness.
        match self
            .send_and_await_raw(
                OutgoingAgentMessage::Ping,
                std::time::Duration::from_secs(10),
            )
            .await
        {
            Ok(IncomingAgentMessage::Pong) => Ok(()),
            Ok(other) => Err(Error::Adapter(format!(
                "Unexpected response to Ping: {other:?}"
            ))),
            Err(e) => Err(Error::Adapter(format!(
                "Failed to start agent adapter: {e}"
            ))),
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

    /// Ensure connected — skip Ping if confirmed within 30s; else Ping with 5s timeout.
    async fn ensure_connected(&self) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let last = self.last_confirmed_at.load(Ordering::Relaxed);

        if now.saturating_sub(last) < 30_000 {
            // Recently confirmed alive
            return Ok(());
        }

        if self.circuit.is_open() {
            return Err(Error::Adapter("agent circuit open".to_string()));
        }

        // Send Ping, await Pong within 5s
        match self
            .circuit
            .call(|| {
                self.send_and_await_raw(
                    OutgoingAgentMessage::Ping,
                    std::time::Duration::from_secs(5),
                )
            })
            .await
        {
            Ok(IncomingAgentMessage::Pong) => Ok(()),
            Ok(other) => Err(Error::Adapter(format!(
                "Unexpected response to Ping: {other:?}"
            ))),
            Err(e) => Err(Error::Adapter(format!("ensure_connected failed: {e}"))),
        }
    }

    fn is_connected(&self) -> bool {
        !self.circuit.is_open() && self.agent_manager.is_agent_managed(&self.connection_id)
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

    /// Get cached drop count for a metric.
    fn get_drop_count(&self, metric_name: &str) -> Result<u64> {
        self.drop_counts
            .lock()
            .map_err(|e| Error::Adapter(format!("Drop count lock poisoned: {e}")))
            .map(|map| map.get(metric_name).copied().unwrap_or(0))
    }
}
