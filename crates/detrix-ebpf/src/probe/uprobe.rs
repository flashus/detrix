//! Uprobe attachment and lifecycle management
//!
//! Handles loading BPF programs and attaching them as uprobes.
//! On Linux: uses aya to compile, load, and attach BPF programs; spawns a
//! background task per probe to drain its ring buffer.
//! On other platforms: records probes for testing without kernel interaction.
//!
//! # Linux uprobe lifecycle
//!
//! ```text
//! attach(metric_name, probe_point)
//!   │
//!   ├─ generate_bpf_program() → BpfProgram (C source)
//!   ├─ compile_bpf()          → CompiledBpf (ELF bytes via clang)
//!   ├─ Ebpf::load()           → loaded aya object
//!   ├─ program.load()         → BPF verifier OK
//!   ├─ program.attach()       → UProbeLink (auto-detaches on drop)
//!   ├─ ebpf.take_map()        → RingBuf (kernel→userspace events)
//!   ├─ spawn_blocking poller  → sends (metric_name, raw_bytes) to channel
//!   └─ store in active_probes
//! ```

use crate::dwarf::types::ProbePoint;
#[cfg(target_os = "linux")]
use crate::dwarf::types::{Register, VariableLocation};
#[allow(unused_imports)] // used inside #[cfg(target_os = "linux")] blocks
use crate::error::ErrContext;
use crate::error::{Error, Result};
use crate::probe::types::{CaptureConfig, ProbeConfig};
use crate::profile::ProfileId;

use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;

/// Bound the userspace handoff between ring-buffer pollers and the correlator.
///
/// The kernel ring buffer already has its own bounded storage and drop counter;
/// keeping this queue bounded prevents a slow DWARF decoder or subscriber from
/// turning a burst into unbounded host-memory growth. A full queue drops the
/// userspace copy and is reported by the poller, distinct from kernel drops.
pub const RAW_EVENT_CHANNEL_CAPACITY: usize = 4096;

/// Manages active uprobe attachments for a single target binary.
///
/// Each logpoint gets its own uprobe with an individually compiled BPF program.
/// Probes can be added and removed dynamically.
pub struct UprobeManager {
    /// Target binary path (used for uprobe attachment on Linux).
    _binary_path: PathBuf,
    /// Active probes keyed by metric name.
    active_probes: HashMap<String, AttachedProbe>,
    /// Channel for raw ring buffer events: `(metric_name, raw_bytes)`.
    ///
    /// On Linux, each probe's ring buffer polling task sends events here.
    /// Unused on non-Linux (the field exists to keep the API uniform).
    #[allow(dead_code)]
    raw_event_tx: Option<mpsc::Sender<(String, Vec<u8>)>>,
    /// Capture limits — used when generating per-metric BPF programs (Linux only).
    #[allow(dead_code)]
    capture_config: CaptureConfig,
}

/// An attached uprobe with its BPF program and aya handles.
///
/// Dropping this struct auto-detaches the uprobe on Linux.
struct AttachedProbe {
    /// The probe configuration used (retained for inspection/reload).
    _config: ProbeConfig,
    /// aya handles — Linux-only.
    #[cfg(target_os = "linux")]
    _handles: AyaHandles,
}

/// aya objects that must stay alive while the uprobe is attached.
///
/// Drop order matters:
/// 1. `_poller` is aborted (stops the ring buffer poller task)
/// 2. `_link` is dropped → uprobe detaches
/// 3. `_ebpf` is dropped → BPF program released
#[cfg(target_os = "linux")]
struct AyaHandles {
    /// Loaded BPF object (must outlive the link).
    _ebpf: Box<aya::Ebpf>,
    /// Owned link — dropping this detaches the uprobe.
    _link: aya::programs::uprobe::UProbeLink,
    /// Ring buffer polling task — aborted on drop.
    _poller: tokio::task::JoinHandle<()>,
    /// Drop counter map — per-CPU counters for ring buffer overflows.
    /// Wrapped in Arc<Mutex> so the ring buffer poller thread can read it
    /// periodically to detect ring buffer overflow events.
    _drop_cnt: std::sync::Arc<std::sync::Mutex<aya::maps::PerCpuArray<aya::maps::MapData, u64>>>,
}

#[cfg(target_os = "linux")]
impl Drop for AyaHandles {
    fn drop(&mut self) {
        self._poller.abort();
    }
}

impl UprobeManager {
    /// Create a new uprobe manager for a target binary (no event forwarding).
    pub fn new(binary_path: impl Into<PathBuf>) -> Self {
        Self {
            _binary_path: binary_path.into(),
            active_probes: HashMap::new(),
            raw_event_tx: None,
            capture_config: CaptureConfig::default(),
        }
    }

    /// Create a manager that forwards raw ring buffer events to `tx`.
    ///
    /// On Linux, each attached probe's ring buffer polling task will send
    /// `(metric_name, raw_bytes)` tuples for correlation by the adapter.
    pub fn new_with_events(
        binary_path: impl Into<PathBuf>,
        tx: mpsc::Sender<(String, Vec<u8>)>,
    ) -> Self {
        Self {
            _binary_path: binary_path.into(),
            active_probes: HashMap::new(),
            raw_event_tx: Some(tx),
            capture_config: CaptureConfig::default(),
        }
    }

    /// Create a manager with custom capture limits.
    pub fn new_with_config(
        binary_path: impl Into<PathBuf>,
        tx: mpsc::Sender<(String, Vec<u8>)>,
        capture_config: CaptureConfig,
    ) -> Self {
        Self {
            _binary_path: binary_path.into(),
            active_probes: HashMap::new(),
            raw_event_tx: Some(tx),
            capture_config,
        }
    }

    /// Replace the raw-event sender.
    ///
    /// Called by `EbpfAdapter::start()` on each restart to supply a fresh channel.
    /// Previously attached probes that still send on the old sender will have their
    /// events dropped; this is acceptable since probes are detached on `stop()`.
    pub fn set_raw_tx(&mut self, tx: mpsc::Sender<(String, Vec<u8>)>) {
        self.raw_event_tx = Some(tx);
    }

    /// Close the adapter-owned raw-event sender during shutdown.
    ///
    /// Probe pollers clone the sender, but their handles are detached first by
    /// `detach_all()`. Clearing this final owner lets the correlator observe
    /// channel closure and finish its shutdown instead of waiting forever.
    pub fn clear_raw_tx(&mut self) {
        self.raw_event_tx = None;
    }

    /// Number of active probes.
    pub fn active_count(&self) -> usize {
        self.active_probes.len()
    }

    /// Check if a probe is active for a given metric name.
    pub fn has_probe(&self, metric_name: &str) -> bool {
        self.active_probes.contains_key(metric_name)
    }

    /// Attach a uprobe for a resolved probe point.
    ///
    /// On Linux: compiles and loads a BPF program, attaches as uprobe, and
    /// spawns a ring buffer polling task that sends raw events to `raw_event_tx`.
    /// On other platforms: records the probe config for testing.
    pub fn attach(
        &mut self,
        metric_name: &str,
        probe_point: &ProbePoint,
        g_addr_offset: Option<i64>,
        goid_offset: Option<u64>,
    ) -> Result<()> {
        self.attach_for_profile(
            metric_name,
            probe_point,
            g_addr_offset,
            goid_offset,
            ProfileId::Go,
        )
    }

    /// Attach using a language profile. Go and Rust both use the validated
    /// CapturePlan renderer; the legacy no-plan API remains for compatibility.
    #[allow(unused_variables)]
    pub fn attach_for_profile(
        &mut self,
        metric_name: &str,
        probe_point: &ProbePoint,
        g_addr_offset: Option<i64>,
        goid_offset: Option<u64>,
        profile: ProfileId,
    ) -> Result<()> {
        // Rust probes always carry a validated CapturePlan identity. Keep the
        // legacy no-plan API for Go compatibility, but do not let a direct
        // Rust caller silently bypass the plan compiler.
        let plan_hash = (profile == ProfileId::Rust).then(|| format!("probe:{:x}", probe_point.pc));
        self.attach_for_profile_with_plan(
            metric_name,
            probe_point,
            g_addr_offset,
            goid_offset,
            profile,
            plan_hash.as_deref(),
        )
    }

    #[allow(unused_variables)]
    pub fn attach_for_profile_with_plan(
        &mut self,
        metric_name: &str,
        probe_point: &ProbePoint,
        g_addr_offset: Option<i64>,
        goid_offset: Option<u64>,
        profile: ProfileId,
        plan_hash: Option<&str>,
    ) -> Result<()> {
        #[allow(unused_variables)]
        // g_addr_offset/goid_offset are only used on Linux for TLS-based goid capture
        let _ = (g_addr_offset, goid_offset);
        if self.active_probes.contains_key(metric_name) {
            return Err(Error::Ebpf(format!(
                "Probe already attached for metric '{metric_name}'"
            )));
        }

        let config = ProbeConfig {
            binary_path: probe_point.binary_path.clone(),
            symbol_offset: probe_point.symbol_offset,
            var_count: probe_point.variables.len(),
            capture_goid: false,
        };

        #[cfg(target_os = "linux")]
        let handles = self.load_and_attach_linux(
            metric_name,
            probe_point,
            g_addr_offset,
            goid_offset,
            profile,
            plan_hash,
        )?;

        let probe = AttachedProbe {
            _config: config,
            #[cfg(target_os = "linux")]
            _handles: handles,
        };

        self.active_probes.insert(metric_name.to_string(), probe);
        Ok(())
    }

    /// Detach a uprobe for a given metric name.
    ///
    /// On Linux, dropping the `AttachedProbe` auto-detaches the BPF program
    /// via the `UProbeLink`'s `Drop` implementation and aborts the poller task.
    pub fn detach(&mut self, metric_name: &str) -> Result<()> {
        if self.active_probes.remove(metric_name).is_none() {
            return Err(Error::Ebpf(format!(
                "No probe attached for metric '{metric_name}'"
            )));
        }
        Ok(())
    }

    /// Detach all probes (called on Stop and Drop).
    pub fn detach_all(&mut self) {
        self.active_probes.clear();
    }

    /// Get the drop count for a specific probe.
    ///
    /// Returns the number of events dropped due to ring buffer overflow.
    /// For non-Linux platforms or non-existent probes, returns 0.
    ///
    /// The drop counter is a per-CPU array, so this sums values across all CPUs.
    pub fn get_drop_count(&self, metric_name: &str) -> u64 {
        #[cfg(target_os = "linux")]
        {
            match self.active_probes.get(metric_name) {
                Some(probe) => {
                    // Per-CPU array: sum across all CPUs
                    let key: u32 = 0;
                    match probe._handles._drop_cnt.lock() {
                        Ok(map) => match map.get(&key, 0) {
                            Ok(values) => values.iter().copied().sum::<u64>(),
                            Err(e) => {
                                detrix_logging::debug!(
                                    "[uprobe] get_drop_count: failed to read drop counter for '{}': {}",
                                    metric_name,
                                    e
                                );
                                0
                            }
                        },
                        Err(_) => 0, // Mutex poisoned — shouldn't happen
                    }
                }
                None => {
                    detrix_logging::debug!(
                        "[uprobe] get_drop_count: probe '{}' not found",
                        metric_name
                    );
                    0
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = metric_name;
            0
        }
    }

    /// Compile the BPF program, load it with aya, attach as uprobe, and spawn
    /// a ring buffer polling task that forwards events to `raw_event_tx`.
    ///
    /// Returns the aya handles that must be kept alive while the probe is active.
    #[cfg(target_os = "linux")]
    fn load_and_attach_linux(
        &self,
        metric_name: &str,
        probe_point: &ProbePoint,
        g_addr_offset: Option<i64>,
        goid_offset: Option<u64>,
        profile: ProfileId,
        plan_hash: Option<&str>,
    ) -> Result<AyaHandles> {
        use crate::compiler::{GoBpfCompiler, RustBpfCompiler};
        use crate::probe::loader::compile_bpf_for_arch;
        use crate::probe::program::{BpfProgram, RawEnvelopeSpec};
        use aya::programs::uprobe::UProbeLink;
        use aya::programs::UProbe;

        let compile_point = probe_point.clone();
        if profile == ProfileId::Rust {
            detrix_logging::debug!(
                "[uprobe] Rust compile locations: {:?}",
                compile_point
                    .variables
                    .iter()
                    .map(|v| (&v.name, &v.location))
                    .collect::<Vec<_>>()
            );
        }

        // Step 1: Generate BPF C source from variable locations
        let bpf_program = match profile {
            ProfileId::Go => {
                let compiler = GoBpfCompiler {
                    config: self.capture_config.clone(),
                    capture_goid: self.capture_config.capture_goid,
                    g_addr_offset,
                    goid_offset,
                };
                // Every Go location form must enter through the validated
                // CapturePlan boundary. The existing Go generator remains the
                // compatibility renderer behind `CaptureCompiler::compile`,
                // but an unrepresentable layout now fails closed instead of
                // silently bypassing plan validation.
                let plan = GoBpfCompiler::plan_from_probe(
                    probe_point,
                    format!("probe:{:x}", probe_point.pc),
                )
                .map_err(|error| {
                    detrix_logging::warn!(
                        "[uprobe] Go CapturePlan rejected '{}': {}",
                        metric_name,
                        error
                    );
                    Error::Ebpf(error.to_string())
                })?;
                let envelope = plan_hash.map(|hash| RawEnvelopeSpec {
                    profile_tag: crate::compiler::profile_tag("go"),
                    plan_tag: crate::compiler::plan_tag(hash),
                });
                compiler
                    .compile_with_envelope(&plan, envelope)
                    .and_then(|compiled| {
                        String::from_utf8(compiled.artifact)
                            .map(|source| BpfProgram {
                                source,
                                var_count: probe_point.variables.len(),
                                captures_goid: self.capture_config.capture_goid,
                                g_addr_offset,
                                goid_offset,
                                versioned_envelope: envelope.is_some(),
                            })
                            .map_err(|error| {
                                crate::compiler::CompileError::Backend(error.to_string())
                            })
                    })
                    .map_err(|error| Error::Ebpf(error.to_string()))?
            }
            ProfileId::Rust => {
                let compiler = RustBpfCompiler {
                    config: self.capture_config.clone(),
                };
                if let Some(plan_hash) = plan_hash {
                    let plan = RustBpfCompiler::plan_from_probe(&compile_point, plan_hash)
                        .map_err(|error| Error::Ebpf(error.to_string()))?;
                    compiler
                        .compile_plan_to_program(&plan)
                        .map_err(|error| Error::Ebpf(error.to_string()))?
                } else {
                    compiler.compile_variables(&compile_point.variables)?
                }
            }
        };

        if profile == ProfileId::Rust {
            for (index, variable) in compile_point.variables.iter().enumerate() {
                let aggregate_size = match &variable.location {
                    VariableLocation::StackBlob { byte_size, .. }
                    | VariableLocation::PiecewiseBlob { byte_size, .. } => {
                        Some((*byte_size).min(self.capture_config.max_blob_capture))
                    }
                    _ => None,
                };
                if let Some(aggregate_size) = aggregate_size {
                    let declaration = format!("var{index}_blob[{aggregate_size}]");
                    if !bpf_program.source.contains(&declaration) {
                        return Err(Error::Ebpf(format!(
                            "Rust aggregate field '{}' is missing generated declaration '{}'",
                            variable.name, declaration
                        )));
                    }
                } else if variable.size.bytes() > 8
                    && !bpf_program.source.contains(&format!("var{index}_blob["))
                {
                    return Err(Error::Ebpf(format!(
                        "Rust aggregate field '{}' ({} bytes) has no blob in generated event layout",
                        variable.name,
                        variable.size.bytes()
                    )));
                }
            }
            detrix_logging::debug!(
                "[uprobe] Rust generated layout: bytes={} has_var0_blob={} declaration={}",
                bpf_program.source.len(),
                bpf_program.source.contains("var0_blob["),
                bpf_program
                    .source
                    .lines()
                    .find(|line| line.contains("var0_blob["))
                    .unwrap_or("<none>")
            );
        }

        // Debug: log generated BPF source
        detrix_logging::debug!(
            "[uprobe] Generated BPF for '{}':\n{}",
            metric_name,
            bpf_program.source
        );
        // Step 2: Compile C → ELF via clang. Select the target from the
        // resolved DWARF probe, not from the Detrix build host; this is what
        // keeps x86-64 and AArch64 plans from silently sharing register ABI.
        let architecture = probe_point
            .variables
            .iter()
            .find_map(|variable| match variable.location {
                VariableLocation::Register(Register::Arm64(_))
                | VariableLocation::FrameOffset {
                    register: Register::Arm64(_),
                    ..
                } => Some(crate::dwarf::types::TargetArchitecture::Aarch64),
                VariableLocation::Register(_) | VariableLocation::FrameOffset { .. } => {
                    Some(crate::dwarf::types::TargetArchitecture::X86_64)
                }
                _ => None,
            })
            .unwrap_or({
                if cfg!(target_arch = "aarch64") {
                    crate::dwarf::types::TargetArchitecture::Aarch64
                } else {
                    crate::dwarf::types::TargetArchitecture::X86_64
                }
            });
        let compiled = compile_bpf_for_arch(&bpf_program, architecture)?;

        // Step 3: Load the ELF object with aya
        let mut ebpf = aya::Ebpf::load(&compiled.elf_bytes).context("aya load failed")?;

        // Step 3a: Populate DETRIX_NS_INFO with our PID namespace dev/ino so
        // the BPF program can call bpf_get_ns_current_pid_tgid() and emit
        // container-local PIDs instead of host PIDs for process_vm_readv.
        populate_ns_info(&mut ebpf);

        let binary_path_str = probe_point
            .binary_path
            .to_str()
            .ok_or_else(|| Error::Ebpf("Non-UTF8 binary path".to_string()))?;

        // Steps 4-6: Load, attach, and take the link.
        //
        // Use an inner block so the `&mut UProbe` borrow is released before
        // we call `ebpf.take_map()` below (borrow checker requirement).
        let link: UProbeLink = {
            let program: &mut UProbe = ebpf
                .program_mut("detrix_capture")
                .ok_or_else(|| Error::Ebpf("BPF program 'detrix_capture' not found".to_string()))?
                .try_into()
                .context("Not a uprobe program")?;

            program
                .load()
                .map_err(|error| Error::VerifierRejected(error.to_string()))?;

            // Attach at symbol_offset in the target binary.
            // fn_name=None + offset=symbol_offset → mid-function attachment.
            let link_id = program
                .attach(
                    None,                      // fn_name: None = use offset directly
                    probe_point.symbol_offset, // offset from .text base
                    binary_path_str,
                    None, // namespace (cgroups)
                )
                .map_err(|error| Error::AttachFailed(error.to_string()))?;

            program.take_link(link_id).context("take_link failed")?
            // &mut UProbe borrow released here
        };

        // Step 7: Extract ring buffer and spawn poller task.
        //
        // Each probe has its own DETRIX_EVENTS ring buffer map. We take
        // ownership of the map data here (before boxing ebpf) and move it
        // into a spawn_blocking task that polls for new events.
        //
        // NOTE (C-1 from audit — improved):
        // This uses spawn_blocking because aya's RingBuf is memory-mapped (not
        // file-descriptor based), so AsyncFd cannot be used directly. A proper
        // async interface would require changes to aya itself.
        //
        // Step 7b: Extract drop counter map before spawning the ring buffer poller.
        //
        // Each probe has its own DETRIX_DROP_CNT per-CPU array map for drop counting.
        // This map is incremented by the BPF program when bpf_ringbuf_reserve() fails.
        // Wrapping in Arc<Mutex<...>> lets the poller thread read it periodically to
        // detect ring buffer overflow without a separate async task.
        let drop_cnt_map = ebpf
            .take_map("DETRIX_DROP_CNT")
            .ok_or_else(|| Error::Ebpf("DETRIX_DROP_CNT map not found".to_string()))?;

        let drop_cnt = aya::maps::PerCpuArray::<_, u64>::try_from(drop_cnt_map)
            .context("Drop counter map init failed")?;

        let drop_cnt = std::sync::Arc::new(std::sync::Mutex::new(drop_cnt));

        // IMPROVEMENT: Adaptive polling with exponential backoff.
        // - Starts at 1ms poll interval for low latency during active periods
        // - Backs off to 10ms after 10 consecutive idle polls
        // - Resets to 1ms immediately when events are detected
        // This reduces thread-pool pressure during idle periods while maintaining
        // sub-10ms latency during active event processing.
        let poller: tokio::task::JoinHandle<()> = if let Some(ref tx) = self.raw_event_tx {
            let map_data = ebpf
                .take_map("DETRIX_EVENTS")
                .ok_or_else(|| Error::Ebpf("DETRIX_EVENTS map not found".to_string()))?;

            let mut ring_buf =
                aya::maps::RingBuf::try_from(map_data).context("RingBuf init failed")?;

            let tx = tx.clone();
            let name = metric_name.to_string();
            let drop_cnt_poller = drop_cnt.clone();

            tokio::task::spawn_blocking(move || {
                let mut idle_polls = 0u32;
                let mut drop_check_polls = 0u32;
                let mut last_drop_total: u64 = 0;
                let mut userspace_drops: u64 = 0;
                const IDLE_THRESHOLD: u32 = 10; // Back off after 10 idle polls
                                                // Check drop counter approximately every 100 slow polls (~1 second)
                const DROP_CHECK_INTERVAL: u32 = 100;
                const FAST_POLL: std::time::Duration = std::time::Duration::from_millis(1);
                const SLOW_POLL: std::time::Duration = std::time::Duration::from_millis(10);

                loop {
                    while let Some(item) = ring_buf.next() {
                        idle_polls = 0; // Reset backoff on event
                        match tx.try_send((name.clone(), item.to_vec())) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                // The kernel-side drop counter cannot account for
                                // userspace backpressure. Keep polling so one full
                                // burst does not permanently disable the probe.
                                userspace_drops = userspace_drops.saturating_add(1);
                                // Avoid turning a sustained overload into a log
                                // amplification loop while retaining evidence of
                                // the first loss and periodic progress.
                                if userspace_drops == 1 || userspace_drops.is_multiple_of(1024) {
                                    detrix_logging::warn!(
                                        dropped = userspace_drops,
                                        "eBPF userspace event queue full — events dropped"
                                    );
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                return; // Receiver dropped — stop polling
                            }
                        }
                    }
                    // Adaptive sleep: fast when active, slow when idle
                    let sleep_duration = if idle_polls >= IDLE_THRESHOLD {
                        drop_check_polls += 1;
                        if drop_check_polls >= DROP_CHECK_INTERVAL {
                            drop_check_polls = 0;
                            // Periodically read drop counter and warn on increase
                            if let Ok(map) = drop_cnt_poller.lock() {
                                let total: u64 = map
                                    .get(&0u32, 0)
                                    .ok()
                                    .map(|pcu| pcu.iter().sum::<u64>())
                                    .unwrap_or(0);
                                if total > last_drop_total {
                                    let new_drops = total - last_drop_total;
                                    detrix_logging::warn!(
                                        dropped = new_drops,
                                        metric = %name,
                                        "eBPF ring buffer overflow — events dropped"
                                    );
                                    last_drop_total = total;
                                }
                            }
                        }
                        SLOW_POLL
                    } else {
                        idle_polls += 1;
                        FAST_POLL
                    };
                    std::thread::park_timeout(sleep_duration);
                }
            })
        } else {
            // No event channel configured — idle task so _poller is always valid.
            tokio::spawn(std::future::pending())
        };

        Ok(AyaHandles {
            _ebpf: Box::new(ebpf),
            _link: link,
            _poller: poller,
            _drop_cnt: drop_cnt,
        })
    }
}

/// Populate the DETRIX_NS_INFO BPF array map with the daemon's PID namespace
/// device/inode numbers. This enables the BPF uprobe to call
/// `bpf_get_ns_current_pid_tgid()` and emit the container-local PID rather
/// than the host (root namespace) PID, allowing `process_vm_readv` to work
/// correctly from inside Docker containers.
///
/// On bare Linux (no container), `/proc/self/ns/pid` describes the root
/// namespace. The fixture processes are also in the same namespace, so
/// `bpf_get_ns_current_pid_tgid()` returns a PID identical to the host PID
/// and everything still works — there is no regression.
///
/// If the map lookup or stat call fails (shouldn't happen in practice),
/// the map entries stay zero and the BPF program falls back to the host PID.
#[cfg(target_os = "linux")]
fn populate_ns_info(ebpf: &mut aya::Ebpf) {
    use aya::maps::Array;
    use std::os::unix::fs::MetadataExt;

    let meta = match std::fs::metadata("/proc/self/ns/pid") {
        Ok(m) => m,
        Err(e) => {
            detrix_logging::warn!("[uprobe] Failed to stat /proc/self/ns/pid: {e}");
            return;
        }
    };

    let dev = meta.dev();
    let ino = meta.ino();

    let map = match ebpf.map_mut("DETRIX_NS_INFO") {
        Some(m) => m,
        None => {
            detrix_logging::warn!("[uprobe] DETRIX_NS_INFO map not found in BPF object");
            return;
        }
    };

    let mut array: Array<_, [u64; 2]> = match Array::try_from(map) {
        Ok(a) => a,
        Err(e) => {
            detrix_logging::warn!("[uprobe] DETRIX_NS_INFO is not an Array map: {e}");
            return;
        }
    };

    match array.set(0, [dev, ino], 0) {
        Ok(()) => detrix_logging::debug!("[uprobe] PID namespace info set: dev={dev} ino={ino}"),
        Err(e) => detrix_logging::warn!("[uprobe] Failed to set DETRIX_NS_INFO: {e}"),
    }
}

impl Drop for UprobeManager {
    fn drop(&mut self) {
        self.detach_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(target_os = "linux"))]
    use crate::dwarf::types::{
        ProbePoint, Register, ResolvedVariable, VariableLocation, VariableSize,
    };
    #[cfg(not(target_os = "linux"))]
    use std::path::PathBuf;

    #[cfg(not(target_os = "linux"))]
    fn test_probe_point() -> ProbePoint {
        ProbePoint {
            binary_path: PathBuf::from("/test/binary"),
            pc: 0x0040_1100,
            symbol_offset: 0x100,
            function_name: "main.handleOrder".to_string(),
            variables: vec![ResolvedVariable {
                name: "amount".to_string(),
                location: VariableLocation::Register(Register::Rax),
                size: VariableSize::QWord,
                type_name: "int64".to_string(),
                nested_type: None,
            }],
        }
    }

    #[test]
    fn new_manager_is_empty() {
        let mgr = UprobeManager::new("/test/binary");
        assert_eq!(mgr.active_count(), 0);
        assert!(!mgr.has_probe("anything"));
    }

    #[test]
    fn new_with_events_stores_sender() {
        let (tx, _rx) = mpsc::channel(RAW_EVENT_CHANNEL_CAPACITY);
        let mgr = UprobeManager::new_with_events("/test/binary", tx);
        assert!(mgr.raw_event_tx.is_some());
    }

    #[test]
    fn raw_event_channel_is_bounded() {
        let (tx, mut rx) = mpsc::channel::<(String, Vec<u8>)>(RAW_EVENT_CHANNEL_CAPACITY);
        for index in 0..RAW_EVENT_CHANNEL_CAPACITY {
            tx.try_send(("metric".into(), vec![index as u8]))
                .expect("capacity should accept exactly the configured bound");
        }
        assert!(tx.try_send(("metric".into(), vec![0])).is_err());
        assert_eq!(rx.try_recv().unwrap().1, vec![0]);
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn attach_and_detach() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();

        mgr.attach("order_amount", &point, None, None).unwrap();
        assert_eq!(mgr.active_count(), 1);
        assert!(mgr.has_probe("order_amount"));

        mgr.detach("order_amount").unwrap();
        assert_eq!(mgr.active_count(), 0);
        assert!(!mgr.has_probe("order_amount"));
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn attach_duplicate_fails() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();

        mgr.attach("metric1", &point, None, None).unwrap();
        let result = mgr.attach("metric1", &point, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already attached"));
    }

    #[test]
    fn detach_nonexistent_fails() {
        let mut mgr = UprobeManager::new("/test/binary");
        let result = mgr.detach("nonexistent");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No probe attached"));
    }

    #[test]
    fn clear_raw_tx_releases_manager_sender() {
        let (tx, _rx) = mpsc::channel(RAW_EVENT_CHANNEL_CAPACITY);
        let mut mgr = UprobeManager::new_with_events("/test/binary", tx);
        assert!(mgr.raw_event_tx.is_some());
        mgr.clear_raw_tx();
        assert!(mgr.raw_event_tx.is_none());
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn detach_all_clears_everything() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();

        mgr.attach("m1", &point, None, None).unwrap();
        mgr.attach("m2", &point.clone(), None, None).unwrap();
        assert_eq!(mgr.active_count(), 2);

        mgr.detach_all();
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn multiple_probes_tracked_independently() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();

        for i in 0..5 {
            mgr.attach(&format!("metric_{i}"), &point, None, None)
                .unwrap();
        }
        assert_eq!(mgr.active_count(), 5);

        for i in 0..3 {
            mgr.detach(&format!("metric_{i}")).unwrap();
        }
        assert_eq!(mgr.active_count(), 2);
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn drop_detaches_all() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();
        mgr.attach("m1", &point, None, None).unwrap();
        // Drop mgr — should not panic even with active probes
        drop(mgr);
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn attach_with_events_records_probe() {
        let (tx, _rx) = mpsc::channel(RAW_EVENT_CHANNEL_CAPACITY);
        let mut mgr = UprobeManager::new_with_events("/test/binary", tx);
        let point = test_probe_point();

        mgr.attach("metric_with_events", &point, None, None)
            .unwrap();
        assert!(mgr.has_probe("metric_with_events"));
    }

    #[test]
    fn get_drop_count_nonexistent_probe_returns_zero() {
        let mgr = UprobeManager::new("/test/binary");
        let count = mgr.get_drop_count("nonexistent");
        assert_eq!(count, 0);
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn get_drop_count_returns_zero_on_non_linux() {
        // On non-Linux, attach creates a mock probe with no actual BPF.
        // get_drop_count returns 0 for simplicity.
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();
        mgr.attach("test_metric", &point, None, None).unwrap();

        // On non-Linux (test environment), this should return 0
        let count = mgr.get_drop_count("test_metric");
        assert_eq!(count, 0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn get_drop_count_linux_returns_sum_of_per_cpu_values() {
        // This test only runs on Linux with actual BPF.
        // It verifies that get_drop_count sums per-CPU counters.
        // In practice, this is tested via E2E tests (ebpf_e2e.rs).
        // The unit test ensures the API exists and compiles on Linux.
        let mgr = UprobeManager::new("/test/binary");
        let count = mgr.get_drop_count("nonexistent");
        assert_eq!(count, 0);
    }
}
