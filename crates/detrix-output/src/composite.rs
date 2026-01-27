//! Composite Output
//!
//! A composite output that fans out events to multiple output destinations.
//! Useful for sending the same events to multiple sinks (e.g., Graylog and file logging).

use async_trait::async_trait;
use detrix_core::{MetricEvent, Result};
use detrix_ports::{EventOutput, OutputStats};

/// Composite output that sends events to multiple destinations
///
/// When an event is sent, it is forwarded to all configured outputs.
/// If any output fails, the error is logged but other outputs still receive the event.
/// This provides fan-out functionality with graceful degradation.
///
/// # Example
///
/// ```ignore
/// use detrix_output::CompositeOutput;
///
/// let composite = CompositeOutput::new(vec![
///     Box::new(gelf_output),
///     Box::new(file_output),
/// ]);
/// composite.send(&event).await?;
/// ```
pub struct CompositeOutput {
    outputs: Vec<Box<dyn EventOutput>>,
}

impl CompositeOutput {
    /// Create a new composite output from a list of outputs
    pub fn new(outputs: Vec<Box<dyn EventOutput>>) -> Self {
        Self { outputs }
    }

    /// Get the number of configured outputs
    pub fn output_count(&self) -> usize {
        self.outputs.len()
    }
}

#[async_trait]
impl EventOutput for CompositeOutput {
    async fn send(&self, event: &MetricEvent) -> Result<()> {
        for output in &self.outputs {
            // Log errors but continue to other outputs
            if let Err(e) = output.send(event).await {
                tracing::warn!("Failed to send to output '{}': {}", output.name(), e);
            }
        }
        Ok(())
    }

    async fn send_batch(&self, events: &[MetricEvent]) -> Result<()> {
        for output in &self.outputs {
            if let Err(e) = output.send_batch(events).await {
                tracing::warn!("Failed to send batch to output '{}': {}", output.name(), e);
            }
        }
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        for output in &self.outputs {
            if let Err(e) = output.flush().await {
                tracing::warn!("Failed to flush output '{}': {}", output.name(), e);
            }
        }
        Ok(())
    }

    fn stats(&self) -> OutputStats {
        let mut combined = OutputStats {
            connected: true, // Assume connected if any output is connected
            ..Default::default()
        };

        for output in &self.outputs {
            let s = output.stats();
            combined.events_sent += s.events_sent;
            combined.events_failed += s.events_failed;
            combined.bytes_sent += s.bytes_sent;
            combined.reconnections += s.reconnections;
            combined.connected &= s.connected;
        }

        combined
    }

    fn name(&self) -> &str {
        "composite"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_ports::NullOutput;

    #[tokio::test]
    async fn test_composite_empty() {
        let composite = CompositeOutput::new(vec![]);
        assert_eq!(composite.output_count(), 0);
        assert!(composite.stats().connected);
    }

    #[tokio::test]
    async fn test_composite_with_null_outputs() {
        let composite = CompositeOutput::new(vec![Box::new(NullOutput), Box::new(NullOutput)]);
        assert_eq!(composite.output_count(), 2);

        let stats = composite.stats();
        assert!(stats.connected);
        assert_eq!(stats.events_sent, 0);
    }
}
