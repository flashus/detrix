//! eBPF adapter implementing DapAdapter for Go on Linux
//!
//! Provides the same interface as DAP-based adapters but uses eBPF uprobes
//! instead of the Debug Adapter Protocol. This gives ~10-50x lower overhead
//! per logpoint hit compared to Delve/DAP.
//!
//! # Lifecycle
//!
//! ```text
//! new() → start() → set_metric() ←→ subscribe_events()
//!                  → remove_metric()
//!                  → stop()
//! ```
//!
//! # Event flow (Linux)
//!
//! ```text
//! BPF program fires (uprobe hit)
//!   └─ writes raw bytes to DETRIX_EVENTS ring buffer
//!       └─ ring buffer poller task (per probe, in UprobeManager)
//!           └─ sends (metric_name, raw_bytes) to raw_event_rx
//!               └─ correlator task (spawned in start())
//!                   └─ parse_ring_buffer_event() + probe_event_to_metric_event()
//!                       └─ event_tx → subscribe_events() caller
//! ```

use crate::dwarf::{DwarfInfo, ProbePoint, ResolvedVariable, VariableSize};
use crate::probe::types::{CaptureConfig, CapturedValue};
use crate::probe::UprobeManager;

use async_trait::async_trait;
use detrix_ports::{DapAdapter, RemoveMetricResult, SetMetricResult};
use detrix_core::{ExpressionValue, Metric, MetricEvent, MetricId, TypedValue};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
#[cfg(target_os = "linux")]
use tokio::task::JoinHandle;

/// eBPF-based adapter for Go logpoints on Linux.
///
/// Implements the same `DapAdapter` trait as DAP-based adapters,
/// making it a drop-in replacement from MetricService's perspective.
pub struct EbpfAdapter {
    /// Path to the target Go binary (ELF with DWARF).
    binary_path: PathBuf,
    /// Parsed DWARF info (populated on start).
    dwarf: RwLock<Option<DwarfInfo>>,
    /// Uprobe manager — owns the BPF programs and ring buffer pollers.
    uprobe_manager: RwLock<UprobeManager>,
    /// Active metrics keyed by name. Shared with the correlator task via Arc.
    active_metrics: Arc<RwLock<HashMap<String, ActiveMetric>>>,
    /// Capture limits — kept here for the event correlator's ring buffer parsing (Linux only).
    capture_config: CaptureConfig,
    /// Event sender — probe events are converted to MetricEvents and sent here.
    /// Cloned for the Linux correlator task in start().
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    event_tx: mpsc::Sender<MetricEvent>,
    /// Event receiver — handed out exactly once via subscribe_events().
    event_rx: RwLock<Option<mpsc::Receiver<MetricEvent>>>,
    /// Raw ring buffer events from UprobeManager polling tasks (Linux only).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    raw_event_rx: RwLock<Option<mpsc::UnboundedReceiver<(String, Vec<u8>)>>>,
    /// Whether the adapter is Started.
    started: RwLock<bool>,
    /// Handle to the event correlator task (Linux only). Stored for graceful shutdown.
    #[cfg(target_os = "linux")]
    correlator_handle: RwLock<Option<JoinHandle<()>>>,
    /// Process memory reader for dereferencing Go string pointers (Linux only).
    #[cfg(target_os = "linux")]
    mem_reader: Arc<dyn crate::mem_reader::ProcessMemoryReader>,
}

/// A metric with its resolved probe point, ready for event correlation.
///
/// Fields are read by `run_event_correlator` (Linux-only).
#[allow(dead_code)]
struct ActiveMetric {
    metric: Metric,
    probe_point: ProbePoint,
    /// Whether the BPF program captures the goroutine ID (from runtime.g via R14).
    capture_goid: bool,
}

impl EbpfAdapter {
    /// Create a new eBPF adapter for a Go binary.
    ///
    /// The binary must be compiled with `-gcflags=all=-N -l` for
    /// reliable DWARF variable locations.
    pub fn new(binary_path: impl AsRef<Path>) -> Self {
        Self::new_with_config(binary_path, CaptureConfig::default())
    }

    pub fn new_with_config(binary_path: impl AsRef<Path>, capture_config: CaptureConfig) -> Self {
        let path = binary_path.as_ref().to_path_buf();
        let (event_tx, event_rx) = mpsc::channel(1024);
        let (raw_tx, raw_rx) = mpsc::unbounded_channel();

        Self {
            binary_path: path.clone(),
            dwarf: RwLock::new(None),
            uprobe_manager: RwLock::new(UprobeManager::new_with_events(&path, raw_tx)),
            active_metrics: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            event_rx: RwLock::new(Some(event_rx)),
            raw_event_rx: RwLock::new(Some(raw_rx)),
            started: RwLock::new(false),
            capture_config,
            #[cfg(target_os = "linux")]
            mem_reader: Arc::new(crate::mem_reader::LinuxProcessMemoryReader::new(
                path.to_str().unwrap_or("/unknown"),
            )),
            #[cfg(target_os = "linux")]
            correlator_handle: RwLock::new(None),
        }
    }

    /// Convert a ring buffer probe event to a MetricEvent for a specific metric.
    pub fn probe_event_to_metric_event(
        values: &[CapturedValue],
        variables: &[ResolvedVariable],
        metric: &Metric,
        thread_id: Option<i64>,
    ) -> MetricEvent {
        let expression_values: Vec<ExpressionValue> = variables
            .iter()
            .zip(values.iter())
            .map(|(var, captured)| {
                let value_json = captured.to_json_value(var.size);
                let typed_value = match captured {
                    CapturedValue::Scalar(v) => match var.size {
                        VariableSize::Byte => Some(TypedValue::Boolean(*v != 0)),
                        _ => Some(TypedValue::Numeric(*v as f64)),
                    },
                    CapturedValue::String { data, len } => {
                        let actual_len = (*len).min(data.len());
                        std::str::from_utf8(&data[..actual_len])
                            .ok()
                            .map(|s| TypedValue::Text(s.to_string()))
                    }
                    CapturedValue::Float(f) => Some(TypedValue::Numeric(*f)),
                    CapturedValue::Slice { len, .. } => Some(TypedValue::Numeric(*len as f64)),
                    CapturedValue::Array { .. } => {
                        // Arrays have no single typed value — use JSON representation
                        None
                    }
                    CapturedValue::Bytes(_) => None, // raw bytes have no single typed representation
                    CapturedValue::Error(_) => None,
                    CapturedValue::Struct { .. } => None, // structs have no single typed value (use value_json)
                    CapturedValue::Map { .. } => None, // maps have no single typed value (use value_json)
                };
                ExpressionValue {
                    expression: var.name.clone(),
                    value_json,
                    typed_value,
                }
            })
            .collect();

        MetricEvent {
            id: None,
            metric_id: metric.id.unwrap_or(MetricId(0)),
            metric_name: metric.name.clone(),
            connection_id: metric.connection_id.clone(),
            timestamp: MetricEvent::now_micros(),
            thread_name: None,
            thread_id,
            values: expression_values,
            is_error: false,
            error_type: None,
            error_message: None,
            request_id: None,
            session_id: None,
            stack_trace: None,
            memory_snapshot: None,
        }
    }
}

#[async_trait]
impl DapAdapter for EbpfAdapter {
    async fn start(&self) -> detrix_core::Result<()> {
        let dwarf = DwarfInfo::parse(&self.binary_path)?;
        *self.dwarf.write().await = Some(dwarf);

        // On Linux: spawn the event correlator task that reads raw ring buffer
        // events from UprobeManager pollers, parses them, and forwards to event_tx.
        #[cfg(target_os = "linux")]
        if let Some(raw_rx) = self.raw_event_rx.write().await.take() {
            let active_metrics = Arc::clone(&self.active_metrics);
            let event_tx = self.event_tx.clone();
            let capture_config = self.capture_config.clone();
            let mem_reader = Arc::clone(&self.mem_reader);
            let handle = tokio::spawn(run_event_correlator(
                raw_rx,
                active_metrics,
                event_tx,
                capture_config,
                mem_reader,
            ));
            *self.correlator_handle.write().await = Some(handle);
        }

        *self.started.write().await = true;
        Ok(())
    }

    async fn stop(&self) -> detrix_core::Result<()> {
        self.uprobe_manager.write().await.detach_all();
        self.active_metrics.write().await.clear();

        // Gracefully shut down the correlator task by dropping raw_event_rx
        // (which closes the channel) and awaiting the handle.
        #[cfg(target_os = "linux")]
        if let Some(handle) = self.correlator_handle.write().await.take() {
            // The correlator will exit when raw_rx is exhausted (already taken in start()).
            // We don't need to send a cancellation signal — the channel is already closed.
            if let Err(e) = handle.await {
                detrix_logging::warn!("[EbpfAdapter] Correlator task panicked: {e}");
            }
        }

        *self.started.write().await = false;
        Ok(())
    }

    async fn ensure_connected(&self) -> detrix_core::Result<()> {
        if *self.started.read().await {
            Ok(())
        } else {
            self.start().await
        }
    }

    fn is_connected(&self) -> bool {
        self.started.try_read().map(|guard| *guard).unwrap_or(false)
    }

    async fn set_metric(&self, metric: &Metric) -> detrix_core::Result<SetMetricResult> {
        let dwarf_guard = self.dwarf.read().await;
        let dwarf = dwarf_guard
            .as_ref()
            .ok_or_else(|| detrix_core::Error::Adapter("Adapter not started".to_string()))?;

        detrix_logging::debug!(
            "[EbpfAdapter] set_metric: name={} file={} line={} expressions={:?}",
            metric.name, metric.location.file, metric.location.line, metric.expressions
        );

        let probe_point = dwarf.resolve_probe_point(
            &metric.location.file,
            metric.location.line,
            &metric.expressions,
            self.capture_config.max_capture_depth,
        ).map_err(|e| {
            detrix_logging::error!("[EbpfAdapter] resolve_probe_point failed for '{}': {}", metric.name, e);
            detrix_core::Error::Adapter(format!("DWARF resolution failed: {e}"))
        })?;

        detrix_logging::debug!(
            "[EbpfAdapter] resolved probe_point: pc={:#x} function={} variables={} symbol_offset={:#x}",
            probe_point.pc, probe_point.function_name, probe_point.variables.len(), probe_point.symbol_offset
        );
        for var in &probe_point.variables {
            detrix_logging::debug!(
                "[EbpfAdapter]   variable: name={} type={} location={:?}",
                var.name, var.type_name, var.location
            );
        }

        self.uprobe_manager
            .write()
            .await
            .attach(&metric.name, &probe_point)
            .map_err(|e| {
                detrix_logging::error!("[EbpfAdapter] uprobe attach failed for '{}': {}", metric.name, e);
                detrix_core::Error::Adapter(e.to_string())
            })?;

        let actual_line = metric.location.line;

        self.active_metrics.write().await.insert(
            metric.name.clone(),
            ActiveMetric {
                metric: metric.clone(),
                probe_point,
                capture_goid: false,
            },
        );

        Ok(SetMetricResult {
            verified: true,
            line: actual_line,
            message: Some("eBPF uprobe attached".to_string()),
        })
    }

    async fn remove_metric(&self, metric: &Metric) -> detrix_core::Result<RemoveMetricResult> {
        self.uprobe_manager
            .write()
            .await
            .detach(&metric.name)
            .map_err(|e| detrix_core::Error::Adapter(e.to_string()))?;

        self.active_metrics.write().await.remove(&metric.name);

        Ok(RemoveMetricResult::success())
    }

    async fn subscribe_events(&self) -> detrix_core::Result<mpsc::Receiver<MetricEvent>> {
        self.event_rx
            .write()
            .await
            .take()
            .ok_or_else(|| detrix_core::Error::Adapter("Events already subscribed".to_string()))
    }
}

/// Correlates raw ring buffer bytes with active metric context and emits MetricEvents.
///
/// Runs as a background tokio task (Linux-only, spawned in `start()`).
/// Reads `(metric_name, raw_bytes)` from the channel, looks up variable info
/// from `active_metrics`, parses the ring buffer event, and forwards the
/// resulting `MetricEvent` to `event_tx`.
#[cfg(target_os = "linux")]
async fn run_event_correlator(
    mut raw_rx: mpsc::UnboundedReceiver<(String, Vec<u8>)>,
    active_metrics: Arc<RwLock<HashMap<String, ActiveMetric>>>,
    event_tx: mpsc::Sender<MetricEvent>,
    capture_config: CaptureConfig,
    mem_reader: Arc<dyn crate::mem_reader::ProcessMemoryReader>,
) {
    use crate::probe::ringbuf::parse_ring_buffer_event;

    while let Some((metric_name, raw_bytes)) = raw_rx.recv().await {
        let guard = active_metrics.read().await;
        let Some(active) = guard.get(&metric_name) else {
            continue;
        };

        // Read PID from the event (first 4 bytes) - used by parse_ring_buffer_event internally
        // No need to pass it separately - the function reads it from the data

        match parse_ring_buffer_event(
            &raw_bytes,
            &active.probe_point.variables,
            active.capture_goid,
            &capture_config,
            mem_reader.as_ref(),
        ) {
            Ok(probe_event) => {
                // Prefer goroutine ID over OS thread ID for Go correlation.
                let thread_id = probe_event
                    .goid
                    .map(|g| g as i64)
                    .or_else(|| Some(probe_event.tid as i64));

                let metric_event = EbpfAdapter::probe_event_to_metric_event(
                    &probe_event.values,
                    &active.probe_point.variables,
                    &active.metric,
                    thread_id,
                );

                if event_tx.send(metric_event).await.is_err() {
                    break; // Subscriber dropped the receiver — shut down
                }
            }
            Err(e) => {
                detrix_logging::warn!("Failed to parse ring buffer event for '{metric_name}': {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwarf::types::{Register, VariableLocation};

    fn test_metric() -> Metric {
        Metric::new(
            "test-metric".to_string(),
            detrix_core::ConnectionId::new("test-conn"),
            detrix_core::Location {
                file: "main.go".to_string(),
                line: 42,
            },
            vec!["amount".to_string()],
            detrix_core::SourceLanguage::Go,
        )
        .expect("valid test metric")
    }

    #[test]
    fn probe_event_to_metric_event_scalar() {
        let metric = test_metric();
        let variables = vec![ResolvedVariable {
            name: "amount".to_string(),
            location: VariableLocation::Register(Register::Rax),
            size: VariableSize::QWord,
            type_name: "int64".to_string(),
            nested_type: None,
        }];
        let values = vec![CapturedValue::Scalar(100)];

        let event =
            EbpfAdapter::probe_event_to_metric_event(&values, &variables, &metric, Some(5678));

        assert_eq!(event.metric_name, "test-metric");
        assert_eq!(event.thread_id, Some(5678));
        assert_eq!(event.values.len(), 1);
        assert_eq!(event.values[0].expression, "amount");
        assert_eq!(event.values[0].value_json, "100");
        assert!(!event.is_error);
    }

    #[test]
    fn probe_event_to_metric_event_string() {
        let metric = test_metric();
        let variables = vec![ResolvedVariable {
            name: "name".to_string(),
            location: VariableLocation::GoString {
                ptr: Box::new(VariableLocation::Register(Register::Rax)),
                len: Box::new(VariableLocation::Register(Register::Rbx)),
            },
            size: VariableSize::QWord,
            type_name: "string".to_string(),
            nested_type: None,
        }];
        let values = vec![CapturedValue::String {
            data: b"hello".to_vec(),
            len: 5,
        }];

        let event = EbpfAdapter::probe_event_to_metric_event(&values, &variables, &metric, None);

        assert_eq!(event.values[0].expression, "name");
        assert_eq!(event.values[0].value_json, "\"hello\"");
        assert_eq!(
            event.values[0].typed_value,
            Some(TypedValue::Text("hello".to_string()))
        );
    }

    #[test]
    fn probe_event_to_metric_event_slice() {
        let metric = test_metric();
        let variables = vec![ResolvedVariable {
            name: "items".to_string(),
            location: VariableLocation::GoSlice {
                ptr: Box::new(VariableLocation::Register(Register::Rax)),
                len: Box::new(VariableLocation::Register(Register::Rbx)),
                cap: Box::new(VariableLocation::Register(Register::Rcx)),
            },
            size: VariableSize::QWord,
            type_name: "[]int64".to_string(),
            nested_type: None,
        }];
        let values = vec![CapturedValue::Slice { len: 5, cap: 10 }];

        let event = EbpfAdapter::probe_event_to_metric_event(&values, &variables, &metric, None);

        assert_eq!(event.values[0].expression, "items");
        assert_eq!(event.values[0].value_json, r#"{"len":5,"cap":10}"#);
        assert_eq!(event.values[0].typed_value, Some(TypedValue::Numeric(5.0)));
    }

    #[test]
    fn probe_event_to_metric_event_bytes() {
        let metric = test_metric();
        let variables = vec![ResolvedVariable {
            name: "req".to_string(),
            location: VariableLocation::StackBlob {
                offset: -32,
                byte_size: 4,
            },
            size: VariableSize::QWord,
            type_name: "TradeRequest".to_string(),
            nested_type: None,
        }];
        let values = vec![CapturedValue::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF])];

        let event = EbpfAdapter::probe_event_to_metric_event(&values, &variables, &metric, None);

        assert_eq!(event.values[0].expression, "req");
        assert_eq!(event.values[0].value_json, "\"0xdeadbeef\"");
        assert_eq!(event.values[0].typed_value, None);
    }

    #[test]
    fn probe_event_to_metric_event_error_value() {
        let metric = test_metric();
        let variables = vec![ResolvedVariable {
            name: "x".to_string(),
            location: VariableLocation::Register(Register::Rax),
            size: VariableSize::QWord,
            type_name: "int64".to_string(),
            nested_type: None,
        }];
        let values = vec![CapturedValue::Error("optimized out".to_string())];

        let event = EbpfAdapter::probe_event_to_metric_event(&values, &variables, &metric, None);

        assert!(event.values[0].value_json.contains("error"));
        assert_eq!(event.values[0].typed_value, None);
    }

    #[test]
    fn adapter_new_not_started() {
        let adapter = EbpfAdapter::new("/tmp/test-binary");
        assert!(!adapter.is_connected());
    }

    #[tokio::test]
    async fn subscribe_events_only_once() {
        let adapter = EbpfAdapter::new("/tmp/test-binary");

        let rx = adapter.subscribe_events().await;
        assert!(rx.is_ok());

        let rx2 = adapter.subscribe_events().await;
        assert!(rx2.is_err());
    }

    #[tokio::test]
    async fn new_has_raw_event_channel() {
        let adapter = EbpfAdapter::new("/tmp/test-binary");
        // raw_event_rx should be Some before start() is called
        let guard = adapter.raw_event_rx.read().await;
        assert!(guard.is_some());
    }

    #[tokio::test]
    async fn stop_clears_metrics() {
        let adapter = EbpfAdapter::new("/tmp/test-binary");
        // Directly insert into active_metrics to simulate a set_metric()
        {
            let metric = test_metric();
            let mut guard = adapter.active_metrics.write().await;
            guard.insert(
                "test-metric".to_string(),
                ActiveMetric {
                    metric: metric.clone(),
                    probe_point: crate::dwarf::ProbePoint {
                        binary_path: std::path::PathBuf::from("/tmp/test"),
                        pc: 0x1000,
                        symbol_offset: 0x100,
                        function_name: "main.test".to_string(),
                        variables: vec![],
                    },
                    capture_goid: false,
                },
            );
        }

        assert_eq!(adapter.active_metrics.read().await.len(), 1);
        let _ = adapter.stop().await;
        assert_eq!(adapter.active_metrics.read().await.len(), 0);
    }
}
