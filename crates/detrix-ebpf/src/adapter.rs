//! eBPF adapter implementing DapAdapter for Go/Rust scalar capture on Linux
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

use crate::decode::{ScalarFieldSpec, ScalarKind};
use crate::dwarf::{DwarfInfo, ProbePoint, ResolvedVariable, VariableLocation, VariableSize};
#[cfg(target_os = "linux")]
use crate::error::Error;
use crate::error::Result;
use crate::probe::ringbuf::RawEnvelopeExpectation;
use crate::probe::types::{CaptureConfig, CapturedValue};
use crate::probe::{UprobeManager, RAW_EVENT_CHANNEL_CAPACITY};
use crate::profile::{GoProfile, LanguageProfile, ProfileId, RustProfile};
use crate::runtime::ProfiledCaptureRuntime;

use async_trait::async_trait;
use detrix_core::{ExpressionValue, Metric, MetricEvent, MetricId, TypedValue};
use detrix_ports::{DapAdapter, RemoveMetricResult, SetMetricResult};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
#[cfg(target_os = "linux")]
use tokio::task::JoinHandle;

/// Raw event channel receiver type — used for the placeholder channel.
type RawEventRx = mpsc::Receiver<(String, Vec<u8>)>;

/// eBPF-based adapter for Go logpoints on Linux.
///
/// Implements the same `DapAdapter` trait as DAP-based adapters,
/// making it a drop-in replacement from MetricService's perspective.
pub struct EbpfAdapter {
    /// Executable used for uprobe attachment and text-address mapping.
    binary_path: PathBuf,
    /// Optional external ELF carrying DWARF sections. Address space remains
    /// anchored to `binary_path` for uprobe attachment.
    dwarf_path: PathBuf,
    profile_id: ProfileId,
    /// Language profile strategy.  The typed `profile_id` remains only for
    /// compatibility with the existing uprobe attachment API; all profile
    /// metadata decisions flow through this object.
    profile: Arc<dyn LanguageProfile>,
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
    /// Correlator accounting dimensions, reported independently from
    /// transport drops.
    decode_drops: Arc<AtomicU64>,
    unavailable_fields: Arc<AtomicU64>,
    decoded_events: Arc<AtomicU64>,
    /// Whether the adapter is started. AtomicBool for lock-free check-and-set in start().
    started: AtomicBool,
    /// Serializes start/stop so a concurrent restart cannot install resources
    /// after shutdown has begun. The atomic above remains the cheap status
    /// query used by `is_connected()`.
    lifecycle: Mutex<()>,
    /// Placeholder raw event receiver — keeps the pre-start channel open so ring
    /// buffer pollers don't see a broken channel if set_metric() is called before start().
    /// Dropped (set to None) in start() when the real correlator channel is created.
    _placeholder_raw_rx: RwLock<Option<RawEventRx>>,
    /// Handle to the event correlator task (Linux only). Stored for graceful shutdown.
    #[cfg(target_os = "linux")]
    correlator_handle: RwLock<Option<JoinHandle<()>>>,
    /// Process memory reader for dereferencing Go string pointers (Linux only).
    #[cfg(target_os = "linux")]
    mem_reader: crate::mem_reader::ProcessMemoryReaderRef,
}

/// A metric with its resolved probe point, ready for event correlation.
///
/// Fields are read by `run_event_correlator` (Linux-only).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone)]
struct ActiveMetric {
    metric: Metric,
    probe_point: ProbePoint,
    /// Whether the BPF program captures the goroutine ID (from `runtime.g` via R14).
    ///
    /// Set from `capture_config.capture_goid` in `set_metric()`. When `true`,
    /// the generated BPF program includes `GOID_EXTRACT` code to read `runtime.g.goid`.
    /// The correlator passes this flag to `parse_ring_buffer_event` to parse the
    /// goid field from ring buffer events.
    capture_goid: bool,
    runtime: Option<Arc<Mutex<ProfiledCaptureRuntime>>>,
    raw_envelope: Option<RawEnvelopeExpectation>,
}

/// Return the stable internal key used for probe attachment and event correlation.
///
/// Metric names are operator-facing labels and are intentionally not unique in
/// storage. Persisted IDs are the metric identity. The name fallback is only for
/// unsaved metrics used by tests or direct adapter callers.
fn metric_probe_key(metric: &Metric) -> String {
    match metric.id {
        Some(id) => format!("id:{}", id.0),
        None => format!("name:{}", metric.name),
    }
}

impl EbpfAdapter {
    /// Create a new eBPF adapter for a Go binary (compatibility default).
    ///
    /// The binary must be compiled with `-gcflags=all=-N -l` for
    /// reliable DWARF variable locations.
    ///
    /// # Errors
    /// Returns `Error::InvalidBinaryPath` if the path contains non-UTF-8 bytes.
    pub fn new(binary_path: impl AsRef<Path>) -> Result<Self> {
        Self::new_with_config(binary_path, CaptureConfig::default())
    }

    pub fn new_with_config(
        binary_path: impl AsRef<Path>,
        capture_config: CaptureConfig,
    ) -> Result<Self> {
        Self::new_with_profile(binary_path, capture_config, ProfileId::Go)
    }

    pub fn new_with_profile(
        binary_path: impl AsRef<Path>,
        capture_config: CaptureConfig,
        profile_id: ProfileId,
    ) -> Result<Self> {
        Self::new_with_profile_and_debug_path(
            binary_path,
            capture_config,
            profile_id,
            None::<&Path>,
        )
    }

    pub fn new_with_profile_and_debug_path(
        binary_path: impl AsRef<Path>,
        capture_config: CaptureConfig,
        profile_id: ProfileId,
        debug_path: Option<impl AsRef<Path>>,
    ) -> Result<Self> {
        let profile: Arc<dyn LanguageProfile> = match profile_id {
            ProfileId::Go => Arc::new(GoProfile),
            ProfileId::Rust => Arc::new(RustProfile),
        };
        Self::new_with_profile_object_and_debug_path(
            binary_path,
            capture_config,
            profile_id,
            profile,
            debug_path,
        )
    }

    /// Construct an adapter with a registry-owned profile object.  Built-in
    /// profiles use the compatibility `ProfileId`; third-party backends can
    /// provide their own adapter through `CaptureBackendFactory::create_adapter`.
    pub fn new_with_profile_object_and_debug_path(
        binary_path: impl AsRef<Path>,
        capture_config: CaptureConfig,
        profile_id: ProfileId,
        profile: Arc<dyn LanguageProfile>,
        debug_path: Option<impl AsRef<Path>>,
    ) -> Result<Self> {
        let path = binary_path.as_ref().to_path_buf();
        let dwarf_path = debug_path
            .map(|path| path.as_ref().to_path_buf())
            .unwrap_or_else(|| path.clone());
        let (event_tx, event_rx) = mpsc::channel(1024);

        #[cfg(target_os = "linux")]
        let path_str = path.to_str().ok_or_else(|| {
            detrix_logging::error!(
                "[EbpfAdapter] Binary path contains non-UTF-8 bytes: {:?}",
                path.as_os_str().as_encoded_bytes()
            );
            Error::InvalidBinaryPath(format!(
                "Binary path contains non-UTF-8 bytes: {:?}",
                path.as_os_str().as_encoded_bytes()
            ))
        })?;

        // Create a placeholder raw-event channel so ring buffer pollers don't see a
        // broken sender if set_metric() is called before start(). start() replaces
        // this with a fresh channel and drops the placeholder receiver.
        let (placeholder_tx, placeholder_rx) = mpsc::channel(RAW_EVENT_CHANNEL_CAPACITY);
        Ok(Self {
            binary_path: path.clone(),
            dwarf_path,
            profile_id,
            profile,
            dwarf: RwLock::new(None),
            uprobe_manager: RwLock::new(UprobeManager::new_with_config(
                &path,
                placeholder_tx,
                capture_config.clone(),
            )),
            active_metrics: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            event_rx: RwLock::new(Some(event_rx)),
            decode_drops: Arc::new(AtomicU64::new(0)),
            unavailable_fields: Arc::new(AtomicU64::new(0)),
            decoded_events: Arc::new(AtomicU64::new(0)),
            started: AtomicBool::new(false),
            lifecycle: Mutex::new(()),
            _placeholder_raw_rx: RwLock::new(Some(placeholder_rx)),
            capture_config,
            #[cfg(target_os = "linux")]
            mem_reader: Arc::new(crate::mem_reader::LinuxProcessMemoryReader::new(path_str)),
            #[cfg(target_os = "linux")]
            correlator_handle: RwLock::new(None),
        })
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
        let _lifecycle = self.lifecycle.lock().await;
        // Atomic check-and-set: only one concurrent caller proceeds.
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let result: detrix_core::Result<()> = async {
            let dwarf =
                DwarfInfo::parse_with_debug_path(&self.binary_path, Some(&self.dwarf_path))?;
            *self.dwarf.write().await = Some(dwarf);

            // On Linux: create a fresh raw-event channel on each start() so the adapter
            // is restartable after stop(). set_raw_tx() replaces the sender in
            // UprobeManager; newly attached probes will use the new sender.
            #[cfg(target_os = "linux")]
            {
                let (raw_tx, raw_rx) = mpsc::channel(RAW_EVENT_CHANNEL_CAPACITY);
                self.uprobe_manager.write().await.set_raw_tx(raw_tx);
                // Drop the placeholder receiver now that the real channel is active.
                *self._placeholder_raw_rx.write().await = None;
                let active_metrics = Arc::clone(&self.active_metrics);
                let event_tx = self.event_tx.clone();
                let capture_config = self.capture_config.clone();
                let mem_reader = Arc::clone(&self.mem_reader);
                let decode_drops = Arc::clone(&self.decode_drops);
                let unavailable_fields = Arc::clone(&self.unavailable_fields);
                let decoded_events = Arc::clone(&self.decoded_events);
                let handle = tokio::spawn(run_event_correlator(
                    raw_rx,
                    active_metrics,
                    event_tx,
                    capture_config,
                    mem_reader,
                    decode_drops,
                    unavailable_fields,
                    decoded_events,
                ));
                *self.correlator_handle.write().await = Some(handle);
            }
            Ok(())
        }
        .await;

        if result.is_err() {
            // Reset so a future start() attempt can retry.
            self.started.store(false, Ordering::Release);
        }
        result
    }

    async fn stop(&self) -> detrix_core::Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        let mut uprobe_manager = self.uprobe_manager.write().await;
        uprobe_manager.detach_all();
        // Drop the manager's final raw sender after pollers are detached so
        // the correlator receiver can observe closure and terminate.
        #[cfg(target_os = "linux")]
        uprobe_manager.clear_raw_tx();
        drop(uprobe_manager);
        self.active_metrics.write().await.clear();

        // Gracefully shut down the correlator task by dropping raw_event_rx
        // (which closes the channel) and awaiting the handle.
        #[cfg(target_os = "linux")]
        if let Some(handle) = self.correlator_handle.write().await.take() {
            // The correlator will exit when raw_rx is exhausted (already taken in start()).
            // We don't need to send a cancellation signal — the channel is already closed.
            if let Err(e) = handle.await {
                // Correlator panic is a real error — don't silently return Ok(())
                detrix_logging::error!("[EbpfAdapter] Correlator task panicked: {e}");
                return Err(detrix_core::Error::Adapter(format!(
                    "Event correlator task panicked: {e}"
                )));
            }
        }

        self.started.store(false, Ordering::Release);
        Ok(())
    }

    async fn ensure_connected(&self) -> detrix_core::Result<()> {
        if self.started.load(Ordering::Acquire) {
            Ok(())
        } else {
            self.start().await
        }
    }

    fn is_connected(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    async fn set_metric(&self, metric: &Metric) -> detrix_core::Result<SetMetricResult> {
        let dwarf_guard = self.dwarf.read().await;
        let dwarf = dwarf_guard
            .as_ref()
            .ok_or_else(|| detrix_core::Error::Adapter("Adapter not started".to_string()))?;

        detrix_logging::debug!(
            "[EbpfAdapter] set_metric: name={} file={} line={} expressions={:?}",
            metric.name,
            metric.location.file,
            metric.location.line,
            metric.expressions
        );

        let (probe_point, resolution) = dwarf
            .resolve_probe_point_with_diagnostics(
                &metric.location.file,
                metric.location.line,
                &metric.expressions,
                self.capture_config.max_capture_depth,
            )
            .map_err(|e| {
                detrix_logging::error!(
                    "[EbpfAdapter] resolve_probe_point failed for '{}': {}",
                    metric.name,
                    e
                );
                detrix_core::Error::Adapter(format!("DWARF resolution failed: {e}"))
            })?;

        detrix_logging::debug!(
            "[EbpfAdapter] PC selection: selected={:#x} candidates={} rejected={:?}",
            resolution.selected_pc,
            resolution.candidates.len(),
            resolution.rejections
        );

        detrix_logging::debug!(
            "[EbpfAdapter] resolved probe_point: pc={:#x} function={} variables={} symbol_offset={:#x}",
            probe_point.pc, probe_point.function_name, probe_point.variables.len(), probe_point.symbol_offset
        );
        for var in &probe_point.variables {
            detrix_logging::debug!(
                "[EbpfAdapter]   variable: name={} type={} location={:?}",
                var.name,
                var.type_name,
                var.location
            );
        }

        // Compute TLS-based goid offset when goid capture is enabled.
        // This offset tells the BPF program where to find the G pointer
        // in the thread's TLS block (via fs_base on x86_64).
        let runtime_metadata = self
            .profile
            .runtime_metadata(&dwarf, self.capture_config.capture_goid);
        let g_addr_offset = runtime_metadata.g_addr_offset;
        let goid_offset = runtime_metadata.goid_offset;
        detrix_logging::debug!(
            "[EbpfAdapter] g_addr_offset={:?} goid_offset={:?} for '{}'",
            g_addr_offset,
            goid_offset,
            metric.name
        );

        let probe_key = metric_probe_key(metric);

        // The plan identity is shared by all eBPF profiles.  Rust adopted
        // DRX1 first; Go now uses the same envelope when the connection has
        // negotiated it, while the parser still accepts legacy records for
        // older agents.
        let runtime_plan_hash = Some(format!("probe:{:x}", probe_point.pc));
        let raw_envelope = runtime_plan_hash
            .as_deref()
            .map(|plan_hash| RawEnvelopeExpectation {
                profile_tag: crate::compiler::profile_tag(self.profile.id()),
                plan_tag: crate::compiler::plan_tag(plan_hash),
                field_count: probe_point.variables.len(),
            });
        let runtime = if self.profile_id == ProfileId::Rust {
            match rust_scalar_fields(&probe_point.variables) {
                Ok(fields) => {
                    let mut runtime = ProfiledCaptureRuntime::new(
                        "rust",
                        runtime_plan_hash.clone().expect("Rust runtime plan hash"),
                        fields,
                        self.capture_config.max_blob_capture.max(64),
                    )
                    .map_err(|e| detrix_core::Error::Adapter(e.to_string()))?;
                    runtime
                        .prepare()
                        .and_then(|_| runtime.attach())
                        .and_then(|_| runtime.activate())
                        .map_err(|e| detrix_core::Error::Adapter(e.to_string()))?;
                    Some(Arc::new(Mutex::new(runtime)))
                }
                Err(error)
                    if probe_point.variables.iter().all(|variable| {
                        matches!(
                            &variable.location,
                            VariableLocation::StackBlob { .. }
                                | VariableLocation::GoString { .. }
                                | VariableLocation::StringHeader { .. }
                                | VariableLocation::GoSlice { .. }
                                | VariableLocation::SliceHeader { .. }
                        )
                    }) =>
                {
                    detrix_logging::debug!(
                        "[EbpfAdapter] using bounded inline Rust composite capture without scalar runtime: {error}"
                    );
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };

        let attach_point = if self.profile_id == ProfileId::Rust {
            probe_point.clone()
        } else {
            probe_point.clone()
        };
        self.uprobe_manager
            .write()
            .await
            .attach_for_profile_with_plan(
                &probe_key,
                &attach_point,
                g_addr_offset,
                goid_offset,
                self.profile_id,
                runtime_plan_hash.as_deref(),
            )
            .map_err(|e| {
                detrix_logging::error!(
                    "[EbpfAdapter] uprobe attach failed for '{}': {}",
                    metric.name,
                    e
                );
                detrix_core::Error::Adapter(e.to_string())
            })?;

        let actual_line = metric.location.line;

        self.active_metrics.write().await.insert(
            probe_key,
            ActiveMetric {
                metric: metric.clone(),
                probe_point,
                capture_goid: self.capture_config.capture_goid,
                runtime,
                raw_envelope,
            },
        );

        Ok(SetMetricResult {
            verified: true,
            line: actual_line,
            message: Some("eBPF uprobe attached".to_string()),
        })
    }

    async fn remove_metric(&self, metric: &Metric) -> detrix_core::Result<RemoveMetricResult> {
        let probe_key = metric_probe_key(metric);
        self.uprobe_manager
            .write()
            .await
            .detach(&probe_key)
            .map_err(|e| detrix_core::Error::Adapter(e.to_string()))?;

        self.active_metrics.write().await.remove(&probe_key);

        Ok(RemoveMetricResult::success())
    }

    async fn subscribe_events(&self) -> detrix_core::Result<mpsc::Receiver<MetricEvent>> {
        self.event_rx
            .write()
            .await
            .take()
            .ok_or_else(|| detrix_core::Error::Adapter("Events already subscribed".to_string()))
    }

    fn get_drop_count(&self, metric_name: &str) -> detrix_core::Result<u64> {
        let probe_keys: Vec<String> = self
            .active_metrics
            .try_read()
            .map(|metrics| {
                metrics
                    .iter()
                    .filter(|(_, active)| active.metric.name == metric_name)
                    .map(|(key, _)| key.clone())
                    .collect()
            })
            .unwrap_or_default();
        let count = self
            .uprobe_manager
            .try_read()
            .map(|guard| probe_keys.iter().map(|key| guard.get_drop_count(key)).sum())
            .unwrap_or(0);
        Ok(count)
    }

    fn get_total_drop_count(&self) -> detrix_core::Result<u64> {
        let probe_keys: Vec<String> = self
            .active_metrics
            .try_read()
            .map(|metrics| metrics.keys().cloned().collect())
            .unwrap_or_default();
        let count = self
            .uprobe_manager
            .try_read()
            .map(|guard| probe_keys.iter().map(|key| guard.get_drop_count(key)).sum())
            .unwrap_or(0);
        Ok(count)
    }

    fn get_decode_drop_count(&self) -> detrix_core::Result<u64> {
        Ok(self.decode_drops.load(Ordering::Relaxed))
    }

    fn get_unavailable_field_count(&self) -> detrix_core::Result<u64> {
        Ok(self.unavailable_fields.load(Ordering::Relaxed))
    }

    fn get_decoded_event_count(&self) -> detrix_core::Result<u64> {
        Ok(self.decoded_events.load(Ordering::Relaxed))
    }
}

impl Drop for EbpfAdapter {
    fn drop(&mut self) {
        // Abort the correlator task so it doesn't outlive the adapter.
        #[cfg(target_os = "linux")]
        if let Ok(mut handle) = self.correlator_handle.try_write() {
            if let Some(h) = handle.take() {
                h.abort();
            }
        }
        // Detach eBPF probes from the kernel so they don't outlive the adapter.
        if let Ok(mut mgr) = self.uprobe_manager.try_write() {
            mgr.detach_all();
        } else {
            detrix_logging::warn!(
                "[EbpfAdapter] drop: uprobe_manager lock contended — probes may remain attached"
            );
        }
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
    mut raw_rx: mpsc::Receiver<(String, Vec<u8>)>,
    active_metrics: Arc<RwLock<HashMap<String, ActiveMetric>>>,
    event_tx: mpsc::Sender<MetricEvent>,
    capture_config: CaptureConfig,
    mem_reader: crate::mem_reader::ProcessMemoryReaderRef,
    decode_drops: Arc<AtomicU64>,
    unavailable_fields: Arc<AtomicU64>,
    decoded_events: Arc<AtomicU64>,
) {
    use crate::probe::ringbuf::parse_ring_buffer_event_with_envelope;

    while let Some((probe_key, raw_bytes)) = raw_rx.recv().await {
        // Clone the immutable metric context before doing any async runtime or
        // transport work. Holding the RwLock read guard across `.await` would
        // starve set/remove/stop writers whenever the event channel is slow.
        let active = {
            let guard = active_metrics.read().await;
            guard.get(&probe_key).cloned()
        };
        let Some(active) = active else {
            continue;
        };

        // Read PID from the event (first 4 bytes) - used by parse_ring_buffer_event internally
        // No need to pass it separately - the function reads it from the data

        match parse_ring_buffer_event_with_envelope(
            &raw_bytes,
            &active.probe_point.variables,
            active.capture_goid,
            &capture_config,
            mem_reader.as_ref(),
            active.raw_envelope,
        ) {
            Ok(probe_event) => {
                unavailable_fields.fetch_add(
                    probe_event
                        .values
                        .iter()
                        .filter(|value| matches!(value, CapturedValue::Error(_)))
                        .count() as u64,
                    Ordering::Relaxed,
                );
                decoded_events.fetch_add(1, Ordering::Relaxed);
                if let Some(runtime) = &active.runtime {
                    let (payload, partial) =
                        scalar_payload(&probe_event.values, &active.probe_point.variables);
                    let mut runtime = runtime.lock().await;
                    match runtime.encode_payload(&payload, partial) {
                        Ok(record) => {
                            if let Err(error) = runtime.ingest(&record) {
                                decode_drops.fetch_add(1, Ordering::Relaxed);
                                detrix_logging::warn!(
                                    "Rust profile runtime rejected event for '{}': {error}",
                                    active.metric.name
                                );
                            }
                        }
                        Err(error) => {
                            decode_drops.fetch_add(1, Ordering::Relaxed);
                            detrix_logging::warn!(
                                "Rust profile runtime could not encode event for '{}': {error}",
                                active.metric.name
                            );
                        }
                    }
                }
                // Prefer goroutine ID over OS thread ID for Go correlation.
                let thread_id = probe_event
                    .goid
                    .map(|g| g as i64)
                    .or(Some(probe_event.tid as i64));

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
                decode_drops.fetch_add(1, Ordering::Relaxed);
                detrix_logging::warn!(
                    "Failed to parse ring buffer event for '{}' ({probe_key}): {e}",
                    active.metric.name
                );
            }
        }
    }
}

fn rust_scalar_fields(variables: &[ResolvedVariable]) -> detrix_core::Result<Vec<ScalarFieldSpec>> {
    let mut offset = 0usize;
    let mut fields = Vec::with_capacity(variables.len());
    for variable in variables {
        let size = variable.size.bytes();
        if size == 0 || size > 8 || variable.nested_type.is_some() {
            return Err(detrix_core::Error::Adapter(format!(
                "Rust eBPF supports scalar fields up to 8 bytes; '{}' is unsupported",
                variable.name
            )));
        }
        let kind = if variable.type_name.contains("f32") {
            ScalarKind::Float32
        } else if variable.type_name.contains("f64") {
            ScalarKind::Float64
        } else if variable.type_name == "bool" {
            ScalarKind::Bool
        } else if variable.type_name.starts_with('&') || variable.type_name.starts_with('*') {
            ScalarKind::Address
        } else if variable.type_name.starts_with('i') {
            ScalarKind::Signed
        } else {
            ScalarKind::Unsigned
        };
        fields.push(ScalarFieldSpec {
            name: variable.name.clone(),
            offset,
            size,
            kind,
        });
        offset = offset.saturating_add(size);
    }
    Ok(fields)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn scalar_payload(values: &[CapturedValue], variables: &[ResolvedVariable]) -> (Vec<u8>, bool) {
    let mut payload = Vec::new();
    let mut partial = values.len() != variables.len();
    for (index, variable) in variables.iter().enumerate() {
        let size = variable.size.bytes();
        let value = values.get(index);
        let raw = match value {
            Some(CapturedValue::Error(_)) | None => {
                partial = true;
                vec![0; size]
            }
            Some(CapturedValue::Scalar(value)) => value.to_le_bytes().to_vec(),
            Some(CapturedValue::Float(value)) => value.to_bits().to_le_bytes().to_vec(),
            Some(_) => {
                partial = true;
                vec![0; size]
            }
        };
        payload.extend_from_slice(&raw[..size.min(raw.len())]);
    }
    (payload, partial)
}

#[cfg(test)]
mod scalar_payload_tests {
    use super::*;
    use crate::dwarf::types::Register;

    #[test]
    fn scalar_payload_preserves_field_count_when_value_is_missing() {
        let variables = vec![
            ResolvedVariable {
                name: "a".into(),
                type_name: "u64".into(),
                size: VariableSize::QWord,
                location: VariableLocation::Register(Register::Rax),
                nested_type: None,
            },
            ResolvedVariable {
                name: "b".into(),
                type_name: "u64".into(),
                size: VariableSize::QWord,
                location: VariableLocation::Register(Register::Rbx),
                nested_type: None,
            },
        ];
        let (payload, partial) = scalar_payload(&[CapturedValue::Scalar(7)], &variables);
        assert_eq!(payload.len(), 16);
        assert_eq!(&payload[..8], &7u64.to_le_bytes());
        assert_eq!(&payload[8..], &[0; 8]);
        assert!(partial);
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

    fn test_metric_with_id(name: &str, id: u64) -> Metric {
        let mut metric = test_metric();
        metric.name = name.to_string();
        metric.id = Some(MetricId(id));
        metric
    }

    #[test]
    fn probe_key_uses_metric_id_not_display_name() {
        let first = test_metric_with_id("duplicate-name", 41);
        let second = test_metric_with_id("duplicate-name", 42);

        let first_key = metric_probe_key(&first);
        let second_key = metric_probe_key(&second);

        assert_ne!(first_key, second_key);
        assert!(first_key.contains("41"));
        assert!(second_key.contains("42"));
    }

    #[test]
    fn probe_key_has_deterministic_fallback_for_unsaved_metric() {
        let metric = test_metric();
        assert_eq!(metric_probe_key(&metric), "name:test-metric");
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
    fn probe_event_to_metric_event_float() {
        let metric = test_metric();
        let variables = vec![ResolvedVariable {
            name: "amount".to_string(),
            location: VariableLocation::Register(Register::Rax),
            size: VariableSize::QWord,
            type_name: "float64".to_string(),
            nested_type: None,
        }];
        let values = vec![CapturedValue::Float(1234.5)];

        let event = EbpfAdapter::probe_event_to_metric_event(&values, &variables, &metric, None);

        assert_eq!(event.values[0].expression, "amount");
        assert_eq!(event.values[0].value_json, "1234.5");
        assert_eq!(
            event.values[0].typed_value,
            Some(TypedValue::Numeric(1234.5))
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
        let adapter = EbpfAdapter::new("/tmp/test-binary").unwrap();
        assert!(!adapter.is_connected());
    }

    #[tokio::test]
    async fn subscribe_events_only_once() {
        let adapter = EbpfAdapter::new("/tmp/test-binary").unwrap();

        let rx = adapter.subscribe_events().await;
        assert!(rx.is_ok());

        let rx2 = adapter.subscribe_events().await;
        assert!(rx2.is_err());
    }

    #[tokio::test]
    async fn stop_clears_metrics() {
        let adapter = EbpfAdapter::new("/tmp/test-binary").unwrap();
        // Directly insert into active_metrics to simulate a set_metric()
        {
            let metric = test_metric();
            let mut guard = adapter.active_metrics.write().await;
            guard.insert(
                "test-metric".to_string(),
                ActiveMetric {
                    metric: metric.clone(),
                    probe_point: crate::dwarf::ProbePoint {
                        binary_path: std::path::PathBuf::from("/tmp/test-binary"),
                        pc: 0x1000,
                        symbol_offset: 0x100,
                        function_name: "main.test".to_string(),
                        variables: vec![],
                    },
                    capture_goid: false,
                    runtime: None,
                    raw_envelope: None,
                },
            );
        }

        assert_eq!(adapter.active_metrics.read().await.len(), 1);
        let _ = adapter.stop().await;
        assert_eq!(adapter.active_metrics.read().await.len(), 0);
    }
}
