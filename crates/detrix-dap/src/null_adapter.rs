//! NullAdapter - A no-op DAP adapter for when no debugger is connected
//!
//! This adapter is used when the daemon starts before any debugger connections
//! are established. It always returns "not connected" and fails gracefully.

use async_trait::async_trait;
use detrix_application::{DapAdapter, RemoveMetricResult, SetMetricResult};
use detrix_core::{Error, Metric, MetricEvent, Result};
use tokio::sync::mpsc;

/// A no-op DAP adapter that indicates no debugger is connected
///
/// Used by the daemon when starting up without waiting for debugger connections.
/// All metric operations return errors indicating no connection is available.
#[derive(Debug, Default)]
pub struct NullAdapter;

impl NullAdapter {
    /// Create a new NullAdapter
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DapAdapter for NullAdapter {
    async fn start(&self) -> Result<()> {
        // NullAdapter is always "started" but never connected
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        Ok(())
    }

    async fn ensure_connected(&self) -> Result<()> {
        // NullAdapter is never connected
        Err(Error::NotConnected(
            "No debugger connection. Use 'connect' command first.".to_string(),
        ))
    }

    fn is_connected(&self) -> bool {
        // NullAdapter is never connected
        false
    }

    async fn set_metric(&self, _metric: &Metric) -> Result<SetMetricResult> {
        // Cannot set metrics without a debugger connection
        Err(Error::NotConnected(
            "No debugger connection. Use 'connect' command first.".to_string(),
        ))
    }

    async fn remove_metric(&self, _metric: &Metric) -> Result<RemoveMetricResult> {
        // Cannot remove metrics without a debugger connection
        Err(Error::NotConnected(
            "No debugger connection. Use 'connect' command first.".to_string(),
        ))
    }

    async fn subscribe_events(&self) -> Result<mpsc::Receiver<MetricEvent>> {
        // Return a bounded channel that never receives events (capacity=1 is enough for null adapter)
        let (_tx, rx) = mpsc::channel(1);
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_core::{ConnectionId, Location, MetricMode, SafetyLevel, SourceLanguage};

    fn sample_metric() -> Metric {
        Metric {
            id: None,
            name: "test".to_string(),
            connection_id: ConnectionId::from("test"),
            group: None,
            location: Location {
                file: "test.py".to_string(),
                line: 10,
            },
            expressions: vec!["x".to_string()],
            language: SourceLanguage::Python,
            mode: MetricMode::Stream,
            enabled: true,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_null_adapter_is_not_connected() {
        let adapter = NullAdapter::new();
        assert!(!adapter.is_connected());
    }

    #[tokio::test]
    async fn test_null_adapter_start_stop_succeed() {
        let adapter = NullAdapter::new();
        assert!(adapter.start().await.is_ok());
        assert!(adapter.stop().await.is_ok());
    }

    #[tokio::test]
    async fn test_null_adapter_set_metric_fails() {
        let adapter = NullAdapter::new();
        let metric = sample_metric();

        let result = adapter.set_metric(&metric).await;
        assert!(result.is_err());

        if let Err(Error::NotConnected(msg)) = result {
            assert!(msg.contains("No debugger connection"));
        } else {
            panic!("Expected NotConnected error");
        }
    }

    #[tokio::test]
    async fn test_null_adapter_ensure_connected_fails() {
        let adapter = NullAdapter::new();

        let result = adapter.ensure_connected().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_null_adapter_subscribe_returns_empty_channel() {
        let adapter = NullAdapter::new();

        let result = adapter.subscribe_events().await;
        assert!(result.is_ok());
        // Channel should be empty and sender dropped
    }
}
