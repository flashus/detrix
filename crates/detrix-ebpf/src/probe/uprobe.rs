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
use crate::error::{Error, Result};
use crate::probe::types::{CaptureConfig, ProbeConfig};

use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;

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
    raw_event_tx: Option<mpsc::UnboundedSender<(String, Vec<u8>)>>,
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
        tx: mpsc::UnboundedSender<(String, Vec<u8>)>,
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
        tx: mpsc::UnboundedSender<(String, Vec<u8>)>,
        capture_config: CaptureConfig,
    ) -> Self {
        Self {
            _binary_path: binary_path.into(),
            active_probes: HashMap::new(),
            raw_event_tx: Some(tx),
            capture_config,
        }
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
    pub fn attach(&mut self, metric_name: &str, probe_point: &ProbePoint) -> Result<()> {
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
        let handles = self.load_and_attach_linux(metric_name, probe_point)?;

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

    /// Compile the BPF program, load it with aya, attach as uprobe, and spawn
    /// a ring buffer polling task that forwards events to `raw_event_tx`.
    ///
    /// Returns the aya handles that must be kept alive while the probe is active.
    #[cfg(target_os = "linux")]
    fn load_and_attach_linux(
        &self,
        metric_name: &str,
        probe_point: &ProbePoint,
    ) -> Result<AyaHandles> {
        use crate::probe::loader::compile_bpf;
        use crate::probe::program::generate_bpf_program;
        use aya::programs::uprobe::UProbeLink;
        use aya::programs::UProbe;

        // Step 1: Generate BPF C source from variable locations
        let bpf_program =
            generate_bpf_program(&probe_point.variables, false, &self.capture_config)?;

        // Debug: log generated BPF source
        detrix_logging::debug!(
            "[uprobe] Generated BPF for '{}':\n{}",
            metric_name,
            bpf_program.source
        );

        // Step 2: Compile C → ELF via clang
        let compiled = compile_bpf(&bpf_program)?;

        // Step 3: Load the ELF object with aya
        let mut ebpf = aya::Ebpf::load(&compiled.elf_bytes)
            .map_err(|e| Error::Ebpf(format!("aya load failed: {e}")))?;

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
                .map_err(|e| Error::Ebpf(format!("Not a uprobe program: {e}")))?;

            program
                .load()
                .map_err(|e| Error::Ebpf(format!("BPF verifier rejected: {e}")))?;

            // Attach at symbol_offset in the target binary.
            // fn_name=None + offset=symbol_offset → mid-function attachment.
            let link_id = program
                .attach(
                    None,                      // fn_name: None = use offset directly
                    probe_point.symbol_offset, // offset from .text base
                    binary_path_str,
                    None, // namespace (cgroups)
                )
                .map_err(|e| Error::Ebpf(format!("uprobe attach failed: {e}")))?;

            program
                .take_link(link_id)
                .map_err(|e| Error::Ebpf(format!("take_link failed: {e}")))?
            // &mut UProbe borrow released here
        };

        // Step 7: Extract ring buffer and spawn poller task.
        //
        // Each probe has its own DETRIX_EVENTS ring buffer map. We take
        // ownership of the map data here (before boxing ebpf) and move it
        // into a spawn_blocking task that polls for new events.
        //
        // NOTE: For lower latency, replace the sleep-poll loop with
        // `tokio::io::unix::AsyncFd` event-driven notification.
        let poller: tokio::task::JoinHandle<()> = if let Some(ref tx) = self.raw_event_tx {
            let map_data = ebpf
                .take_map("DETRIX_EVENTS")
                .ok_or_else(|| Error::Ebpf("DETRIX_EVENTS map not found".to_string()))?;

            let mut ring_buf = aya::maps::RingBuf::try_from(map_data)
                .map_err(|e| Error::Ebpf(format!("RingBuf init failed: {e}")))?;

            let tx = tx.clone();
            let name = metric_name.to_string();

            tokio::task::spawn_blocking(move || loop {
                let mut got_event = false;
                while let Some(item) = ring_buf.next() {
                    got_event = true;
                    if tx.send((name.clone(), item.to_vec())).is_err() {
                        return; // Receiver dropped — stop polling
                    }
                }
                // Use park_timeout for efficient sleeping that can be woken early.
                // This is more efficient than sleep() because it doesn't hold a CPU
                // timeslice and can be woken via thread.unpark() if needed.
                if !got_event {
                    std::thread::park_timeout(std::time::Duration::from_millis(10));
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
    use crate::dwarf::types::{
        ProbePoint, Register, ResolvedVariable, VariableLocation, VariableSize,
    };
    use std::path::PathBuf;

    fn test_probe_point() -> ProbePoint {
        ProbePoint {
            binary_path: PathBuf::from("/test/binary"),
            pc: 0x401100,
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
        let (tx, _rx) = mpsc::unbounded_channel();
        let mgr = UprobeManager::new_with_events("/test/binary", tx);
        assert!(mgr.raw_event_tx.is_some());
    }

    #[test]
    fn attach_and_detach() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();

        mgr.attach("order_amount", &point).unwrap();
        assert_eq!(mgr.active_count(), 1);
        assert!(mgr.has_probe("order_amount"));

        mgr.detach("order_amount").unwrap();
        assert_eq!(mgr.active_count(), 0);
        assert!(!mgr.has_probe("order_amount"));
    }

    #[test]
    fn attach_duplicate_fails() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();

        mgr.attach("metric1", &point).unwrap();
        let result = mgr.attach("metric1", &point);
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
    fn detach_all_clears_everything() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();

        mgr.attach("m1", &point).unwrap();
        mgr.attach("m2", &point.clone()).unwrap();
        assert_eq!(mgr.active_count(), 2);

        mgr.detach_all();
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn multiple_probes_tracked_independently() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();

        for i in 0..5 {
            mgr.attach(&format!("metric_{i}"), &point).unwrap();
        }
        assert_eq!(mgr.active_count(), 5);

        for i in 0..3 {
            mgr.detach(&format!("metric_{i}")).unwrap();
        }
        assert_eq!(mgr.active_count(), 2);
    }

    #[test]
    fn drop_detaches_all() {
        let mut mgr = UprobeManager::new("/test/binary");
        let point = test_probe_point();
        mgr.attach("m1", &point).unwrap();
        // Drop mgr — should not panic even with active probes
        drop(mgr);
    }

    #[test]
    fn attach_with_events_records_probe() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut mgr = UprobeManager::new_with_events("/test/binary", tx);
        let point = test_probe_point();

        mgr.attach("metric_with_events", &point).unwrap();
        assert!(mgr.has_probe("metric_with_events"));
    }
}
