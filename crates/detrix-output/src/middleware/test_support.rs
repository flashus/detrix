#![cfg(test)]

use async_trait::async_trait;
use detrix_core::{MetricEvent, Result};
use detrix_ports::{EventOutput, OutputStats};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub(crate) struct MockOutput {
    should_fail: bool,
    send_count: AtomicU64,
    batch_count: AtomicU64,
}

impl MockOutput {
    pub(crate) fn new() -> Self {
        Self::new_with_failure(false)
    }

    pub(crate) fn new_with_failure(should_fail: bool) -> Self {
        Self {
            should_fail,
            send_count: AtomicU64::new(0),
            batch_count: AtomicU64::new(0),
        }
    }

    pub(crate) fn send_count(&self) -> u64 {
        self.send_count.load(Ordering::Relaxed)
    }

    pub(crate) fn batch_count(&self) -> u64 {
        self.batch_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl EventOutput for MockOutput {
    async fn send(&self, _event: &MetricEvent) -> Result<()> {
        self.send_count.fetch_add(1, Ordering::Relaxed);
        if self.should_fail {
            Err(detrix_core::Error::Output("Mock failure".to_string()))
        } else {
            Ok(())
        }
    }

    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        self.batch_count
            .fetch_add(events.len() as u64, Ordering::Relaxed);
        if self.should_fail {
            Err(detrix_core::Error::Output("Mock batch failure".to_string()))
        } else {
            Ok(())
        }
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn stats(&self) -> OutputStats {
        OutputStats {
            events_sent: self.send_count.load(Ordering::Relaxed),
            events_failed: 0,
            bytes_sent: 0,
            reconnections: 0,
            connected: true,
        }
    }

    fn name(&self) -> &str {
        "mock"
    }
}
