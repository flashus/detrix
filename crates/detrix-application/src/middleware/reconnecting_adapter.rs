//! Reconnecting Adapter Middleware
//!
//! A decorator pattern implementation that wraps any `DapAdapter` to add
//! automatic reconnection with exponential backoff.
//!
//! # Design
//!
//! This middleware follows the Decorator/Wrapper pattern:
//! - Wraps any existing `DapAdapter` implementation
//! - Intercepts connection failures and retries with exponential backoff
//! - Optionally re-registers metrics after successful reconnection
//! - Fully transparent - consumers don't know they're using a wrapped adapter
//!
//! # Usage
//!
//! ```rust,ignore
//! use detrix_application::middleware::ReconnectingAdapter;
//! use detrix_config::ReconnectConfig;
//!
//! let inner_adapter = create_python_adapter(...)?;
//! let reconnecting = ReconnectingAdapter::new(inner_adapter, ReconnectConfig::default());
//! // Use reconnecting just like any DapAdapter
//! ```

use crate::{DapAdapter, DapAdapterRef, RemoveMetricResult, SetMetricResult};
use async_trait::async_trait;
use detrix_config::ReconnectConfig;
use detrix_core::{Metric, MetricEvent, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Notify, RwLock};
use tracing::{debug, error, info, warn};

/// RAII guard that ensures the reconnecting flag is reset even on panic
struct ReconnectGuard {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl ReconnectGuard {
    fn new(flag: Arc<AtomicBool>, notify: Arc<Notify>) -> Self {
        Self { flag, notify }
    }
}

impl Drop for ReconnectGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

/// Connection state for the reconnecting adapter
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not yet connected
    Disconnected,
    /// Actively trying to connect
    Connecting,
    /// Successfully connected
    Connected,
    /// Connection failed, in backoff period
    Backoff,
    /// Max retries exhausted, given up
    Failed,
}

/// Statistics about reconnection attempts
#[derive(Debug, Clone, Default)]
pub struct ReconnectStats {
    /// Total number of reconnection attempts
    pub total_attempts: u32,
    /// Successful reconnections
    pub successful_reconnects: u32,
    /// Failed reconnection attempts
    pub failed_attempts: u32,
    /// Metrics successfully restored after reconnection
    pub metrics_restored: u32,
    /// Current consecutive failure count
    pub consecutive_failures: u32,
}

/// Reconnecting adapter middleware
///
/// Wraps an inner `DapAdapter` and provides automatic reconnection
/// with configurable exponential backoff.
pub struct ReconnectingAdapter {
    /// The wrapped adapter
    inner: DapAdapterRef,
    /// Reconnection configuration
    config: ReconnectConfig,
    /// Current connection state
    state: RwLock<ConnectionState>,
    /// Current retry attempt number
    attempt: AtomicU32,
    /// Whether reconnection is in progress
    reconnecting: Arc<AtomicBool>,
    /// Notifier for reconnection completion (replaces busy-wait)
    reconnect_notify: Arc<Notify>,
    /// Metrics to restore after reconnection
    registered_metrics: RwLock<Vec<Metric>>,
    /// Reconnection statistics
    stats: RwLock<ReconnectStats>,
}

impl ReconnectingAdapter {
    /// Create a new reconnecting adapter wrapping the given adapter
    pub fn new(inner: DapAdapterRef, config: ReconnectConfig) -> Self {
        Self {
            inner,
            config,
            state: RwLock::new(ConnectionState::Disconnected),
            attempt: AtomicU32::new(0),
            reconnecting: Arc::new(AtomicBool::new(false)),
            reconnect_notify: Arc::new(Notify::new()),
            registered_metrics: RwLock::new(Vec::new()),
            stats: RwLock::new(ReconnectStats::default()),
        }
    }

    /// Get current connection state
    pub async fn connection_state(&self) -> ConnectionState {
        *self.state.read().await
    }

    /// Get reconnection statistics
    pub async fn stats(&self) -> ReconnectStats {
        self.stats.read().await.clone()
    }

    /// Calculate delay for current attempt using exponential backoff with jitter
    fn calculate_delay(&self, attempt: u32) -> Duration {
        let base_delay = self.config.initial_delay_ms as f64;
        let multiplier = self.config.multiplier;
        let max_delay = self.config.max_delay_ms as f64;

        // Exponential backoff: initial_delay * multiplier^attempt
        let delay = base_delay * multiplier.powi(attempt as i32);
        let delay = delay.min(max_delay);

        // Add jitter to prevent thundering herd
        let jitter_range = delay * self.config.jitter;
        let jitter = if jitter_range > 0.0 {
            // Simple deterministic jitter based on attempt number
            // In production, use rand crate for true randomness
            ((attempt as f64 * 0.618033988749895) % 1.0) * jitter_range
        } else {
            0.0
        };

        Duration::from_millis((delay + jitter) as u64)
    }

    /// Attempt to reconnect with exponential backoff
    async fn attempt_reconnect(&self) -> Result<()> {
        if !self.config.enabled {
            return Err(detrix_core::Error::NotConnected(
                "Reconnection disabled".to_string(),
            ));
        }

        // Prevent concurrent reconnection attempts
        if self
            .reconnecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            debug!("Reconnection already in progress, waiting...");
            // Wait for the other reconnection attempt using Notify (no busy-wait)
            self.reconnect_notify.notified().await;
            // Check if it succeeded
            return if self.inner.is_connected() {
                Ok(())
            } else {
                Err(detrix_core::Error::NotConnected(
                    "Reconnection failed".to_string(),
                ))
            };
        }

        // RAII guard ensures reconnecting flag is reset and waiters notified even on panic
        let _guard = ReconnectGuard::new(
            Arc::clone(&self.reconnecting),
            Arc::clone(&self.reconnect_notify),
        );

        self.do_reconnect().await
    }

    /// Internal reconnection logic (called by attempt_reconnect)
    async fn do_reconnect(&self) -> Result<()> {
        *self.state.write().await = ConnectionState::Connecting;

        let max_attempts = if self.config.max_attempts == 0 {
            u32::MAX // Unlimited
        } else {
            self.config.max_attempts
        };

        let mut attempt = 0u32;

        while attempt < max_attempts {
            self.attempt.store(attempt, Ordering::SeqCst);
            {
                let mut stats = self.stats.write().await;
                stats.total_attempts += 1;
            }

            if attempt > 0 {
                let delay = self.calculate_delay(attempt - 1);
                info!(
                    attempt = attempt,
                    max_attempts = max_attempts,
                    delay_ms = delay.as_millis(),
                    "Reconnection attempt, waiting..."
                );
                *self.state.write().await = ConnectionState::Backoff;
                tokio::time::sleep(delay).await;
            }

            *self.state.write().await = ConnectionState::Connecting;

            match self.inner.start().await {
                Ok(()) => {
                    info!(attempt = attempt, "Reconnection successful");
                    *self.state.write().await = ConnectionState::Connected;
                    self.attempt.store(0, Ordering::SeqCst);

                    {
                        let mut stats = self.stats.write().await;
                        stats.successful_reconnects += 1;
                        stats.consecutive_failures = 0;
                    }

                    // Restore metrics if configured
                    if self.config.restore_metrics {
                        self.restore_metrics().await;
                    }

                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        attempt = attempt,
                        error = %e,
                        "Reconnection attempt failed"
                    );
                    {
                        let mut stats = self.stats.write().await;
                        stats.failed_attempts += 1;
                        stats.consecutive_failures += 1;
                    }
                }
            }

            attempt += 1;
        }

        error!(
            max_attempts = max_attempts,
            "Max reconnection attempts exhausted"
        );
        *self.state.write().await = ConnectionState::Failed;

        Err(detrix_core::Error::NotConnected(format!(
            "Max reconnection attempts ({}) exhausted",
            max_attempts
        )))
    }

    /// Restore registered metrics after reconnection
    async fn restore_metrics(&self) {
        let metrics = self.registered_metrics.read().await.clone();
        if metrics.is_empty() {
            debug!("No metrics to restore after reconnection");
            return;
        }

        info!(
            count = metrics.len(),
            "Restoring metrics after reconnection"
        );

        let mut restored = 0u32;
        for metric in &metrics {
            match self.inner.set_metric(metric).await {
                Ok(_) => {
                    restored += 1;
                    debug!(
                        metric_name = %metric.name,
                        "Metric restored after reconnection"
                    );
                }
                Err(e) => {
                    warn!(
                        metric_name = %metric.name,
                        error = %e,
                        "Failed to restore metric after reconnection"
                    );
                }
            }
        }

        {
            let mut stats = self.stats.write().await;
            stats.metrics_restored += restored;
        }

        info!(
            total = metrics.len(),
            restored = restored,
            "Metrics restoration complete"
        );
    }

    /// Track a metric for restoration after reconnection
    async fn track_metric(&self, metric: &Metric) {
        let mut metrics = self.registered_metrics.write().await;
        // Remove existing metric with same ID if present
        if let Some(id) = metric.id {
            metrics.retain(|m| m.id != Some(id));
        }
        metrics.push(metric.clone());
    }

    /// Untrack a metric (remove from restoration list)
    async fn untrack_metric(&self, metric: &Metric) {
        let mut metrics = self.registered_metrics.write().await;
        if let Some(id) = metric.id {
            metrics.retain(|m| m.id != Some(id));
        }
    }

    /// Check if an error is a connection-related error
    fn is_connection_error(error: &detrix_core::Error) -> bool {
        matches!(
            error,
            detrix_core::Error::NotConnected(_)
                | detrix_core::Error::Adapter(_)
                | detrix_core::Error::Io(_)
        )
    }
}

#[async_trait]
impl DapAdapter for ReconnectingAdapter {
    async fn start(&self) -> Result<()> {
        match self.inner.start().await {
            Ok(()) => {
                *self.state.write().await = ConnectionState::Connected;
                Ok(())
            }
            Err(e) if self.config.enabled => {
                warn!(error = %e, "Initial connection failed, attempting reconnection");
                self.attempt_reconnect().await
            }
            Err(e) => Err(e),
        }
    }

    async fn stop(&self) -> Result<()> {
        *self.state.write().await = ConnectionState::Disconnected;
        self.registered_metrics.write().await.clear();
        self.inner.stop().await
    }

    async fn ensure_connected(&self) -> Result<()> {
        if self.inner.is_connected() {
            return Ok(());
        }

        // Check current state
        let state = *self.state.read().await;
        match state {
            ConnectionState::Connected => Ok(()),
            ConnectionState::Failed if !self.config.enabled => {
                Err(detrix_core::Error::NotConnected(
                    "Connection failed and reconnection is disabled".to_string(),
                ))
            }
            _ => self.attempt_reconnect().await,
        }
    }

    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    async fn set_metric(&self, metric: &Metric) -> Result<SetMetricResult> {
        // Track the metric for potential restoration
        if self.config.restore_metrics {
            self.track_metric(metric).await;
        }

        // Try the operation, reconnecting if needed
        if !self.is_connected() {
            self.ensure_connected().await?;
        }

        match self.inner.set_metric(metric).await {
            Ok(result) => Ok(result),
            Err(e) if self.config.enabled && Self::is_connection_error(&e) => {
                warn!(error = %e, "Connection error during set_metric, reconnecting");
                self.attempt_reconnect().await?;
                self.inner.set_metric(metric).await
            }
            Err(e) => Err(e),
        }
    }

    async fn remove_metric(&self, metric: &Metric) -> Result<RemoveMetricResult> {
        // Untrack the metric
        self.untrack_metric(metric).await;

        if !self.is_connected() {
            // If not connected and we're removing, that's fine - return success
            return Ok(RemoveMetricResult::success());
        }

        match self.inner.remove_metric(metric).await {
            Ok(result) => Ok(result),
            Err(e) if self.config.enabled && Self::is_connection_error(&e) => {
                warn!(error = %e, "Connection error during remove_metric, reconnecting");
                self.attempt_reconnect().await?;
                self.inner.remove_metric(metric).await
            }
            Err(e) => Err(e),
        }
    }

    async fn subscribe_events(&self) -> Result<mpsc::Receiver<MetricEvent>> {
        if !self.is_connected() {
            self.ensure_connected().await?;
        }

        self.inner.subscribe_events().await
    }

    fn supports_batch_operations(&self) -> bool {
        self.inner.supports_batch_operations()
    }
}
