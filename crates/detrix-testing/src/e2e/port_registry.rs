//! Cross-process test port registry.
//!
//! When `cargo test --all` runs multiple test binaries in parallel, each binary
//! needs unique port ranges to avoid collisions. This module provides a file-based
//! registry with `flock` coordination that guarantees non-overlapping ranges.
//!
//! # How it works
//!
//! 1. Each test binary calls `TestPortRegistry::get()` on first port allocation
//! 2. The registry acquires an exclusive file lock on `/tmp/detrix-test-port-registry.json`
//! 3. Reads existing allocations, cleans entries from dead PIDs
//! 4. Claims the next non-overlapping port range
//! 5. Releases the lock — subsequent allocations are in-process (atomic counter)
//!
//! # Usage
//!
//! ```rust,ignore
//! use detrix_testing::e2e::port_registry::TestPortRegistry;
//!
//! // Allocate a port (increment by 1)
//! let port = TestPortRegistry::get().allocate();
//!
//! // Allocate with 200-port spacing (for port_fallback tests)
//! let port = TestPortRegistry::get().allocate_spaced(200);
//! ```

use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::OnceLock;
use std::time::SystemTime;

use fs2::FileExt;

/// Base port for the allocatable range (above most system services).
const BASE_PORT: u16 = 15000;

/// Maximum port (stay below ephemeral port range which starts at 49152 on many systems).
const MAX_PORT: u16 = 55000;

/// Default range size per test binary (1000 ports).
/// Basic tests allocate 2 ports (http + grpc), DAP tests allocate up to 6 (+ debugger ports).
/// unified_e2e has ~168 tests; with lazy debugger ports, needs ~400-600 ports.
/// 1000 ports per binary × ~10 E2E binaries = 10,000 out of 40,000 available range.
const DEFAULT_RANGE_SIZE: u16 = 1000;

/// Path to the shared registry file.
const REGISTRY_PATH: &str = "/tmp/detrix-test-port-registry.json";

/// Ports known to be used by system services that should be skipped.
const SYSTEM_RESERVED_PORTS: &[u16] = &[5000, 7000];

/// A cross-process port registry that guarantees non-overlapping port ranges
/// per test binary process.
///
/// Initialized once per process via `OnceLock`. The first call acquires a file lock,
/// claims a range, and releases the lock. Subsequent allocations are lock-free
/// (atomic counter within the claimed range).
pub struct TestPortRegistry {
    range_start: u16,
    range_end: u16,
    next_port: AtomicU16,
}

/// Global singleton.
static REGISTRY: OnceLock<TestPortRegistry> = OnceLock::new();

impl TestPortRegistry {
    /// Get the global port registry, initializing it on first call.
    ///
    /// Claims a port range of `DEFAULT_RANGE_SIZE` ports from the shared
    /// registry file. Thread-safe via `OnceLock`.
    pub fn get() -> &'static TestPortRegistry {
        REGISTRY.get_or_init(|| claim_range(DEFAULT_RANGE_SIZE))
    }

    /// Allocate the next available port (increment by 1).
    ///
    /// Verifies port availability via `TcpListener::bind`. Skips system-reserved ports.
    /// Panics if the claimed range is exhausted.
    pub fn allocate(&self) -> u16 {
        self.allocate_spaced(1)
    }

    /// Allocate the next available port with spacing between allocations.
    ///
    /// Use spacing > 1 when the daemon uses `port_fallback` which scans forward from
    /// the configured port. For example, `allocate_spaced(200)` ensures 200 ports of
    /// headroom for each allocation.
    pub fn allocate_spaced(&self, spacing: u16) -> u16 {
        let (port, _listener) = self.allocate_spaced_listener(spacing);
        port
    }

    /// Allocate the next available port and return it along with a bound `TcpListener`.
    ///
    /// The listener keeps the port reserved until the caller hands it off to a server,
    /// eliminating the TOCTOU window between availability check and actual bind.
    pub fn allocate_listener(&self) -> (u16, std::net::TcpListener) {
        self.allocate_spaced_listener(1)
    }

    fn allocate_spaced_listener(&self, spacing: u16) -> (u16, std::net::TcpListener) {
        let spacing = spacing.max(1);
        loop {
            let port = self.next_port.fetch_add(spacing, Ordering::SeqCst);
            if port >= self.range_end {
                // A local machine may have unrelated services occupying every
                // port in the claimed slice. Ordinary single-port callers do
                // not require deterministic spacing, so use the kernel's
                // ephemeral allocator instead of failing before the test
                // process starts. Spaced callers still require a complete
                // bindable window, which is checked below before fallback.
                if let Some((ephemeral, listener)) = find_fallback_window(self.range_end, spacing) {
                    eprintln!(
                        "TestPortRegistry: claimed range exhausted; using fallback base {ephemeral} (spacing {spacing})"
                    );
                    return (ephemeral, listener);
                }
                panic!(
                    "TestPortRegistry: range exhausted ({}-{}), spacing={spacing}. \
                     Increase DEFAULT_RANGE_SIZE or reduce port spacing.",
                    self.range_start, self.range_end
                );
            }
            if SYSTEM_RESERVED_PORTS.contains(&port) {
                continue;
            }
            match std::net::TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => return (port, listener),
                Err(_) => continue, // port in use, try next
            }
        }
    }

    /// Get the claimed range (start, end).
    pub fn range(&self) -> (u16, u16) {
        (self.range_start, self.range_end)
    }
}

/// Find a bindable contiguous window after the process-local claim is full.
///
/// Binding port 0 is not sufficient for spaced callers: the kernel may choose
/// a random ephemeral port whose following ports are occupied.  Scan a
/// deterministic range instead and hold every successful bind until the whole
/// window has been validated, eliminating the old false exhaustion failure.
fn find_fallback_window(start: u16, spacing: u16) -> Option<(u16, std::net::TcpListener)> {
    let spacing = spacing.max(1);
    let first = start.max(BASE_PORT);
    let last = MAX_PORT.saturating_sub(spacing.saturating_sub(1));
    if first > last {
        return None;
    }

    for candidate in first..=last {
        if SYSTEM_RESERVED_PORTS.contains(&candidate) {
            continue;
        }
        let listener = match std::net::TcpListener::bind(("127.0.0.1", candidate)) {
            Ok(listener) => listener,
            Err(_) => continue,
        };
        let mut window = Vec::with_capacity(spacing.saturating_sub(1) as usize);
        let mut valid = true;
        for offset in 1..spacing {
            match std::net::TcpListener::bind(("127.0.0.1", candidate.saturating_add(offset))) {
                Ok(port) => window.push(port),
                Err(_) => {
                    valid = false;
                    break;
                }
            }
        }
        if valid {
            // Keep the first listener; the additional probes only reserve the
            // window during validation and are intentionally released before
            // the caller starts its server.
            drop(window);
            return Some((candidate, listener));
        }
    }
    None
}

/// Get the current test binary name (for diagnostics).
fn current_binary_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".to_string())
}

// ---- Registry file format ----

#[derive(Serialize, Deserialize, Default)]
struct Registry {
    allocations: Vec<RegistryEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct RegistryEntry {
    pid: u32,
    binary: String,
    range_start: u16,
    range_end: u16,
    ts: u64,
    /// Process start time, in seconds since the Unix epoch.
    ///
    /// PIDs can be reused after a test process exits.  The start time makes
    /// the registry identity `(pid, start_time)` instead of trusting the PID
    /// alone.  A zero value denotes a legacy entry and is intentionally
    /// discarded during the next cleanup pass.
    #[serde(default)]
    start_time: u64,
}

/// Claim a port range from the shared registry file.
///
/// 1. Open/create the registry file
/// 2. Acquire exclusive file lock (blocks until other processes release)
/// 3. Read existing allocations
/// 4. Clean entries from dead PIDs
/// 5. Find the first non-overlapping range
/// 6. Write our allocation
/// 7. Release lock
fn claim_range(range_size: u16) -> TestPortRegistry {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(REGISTRY_PATH)
        .unwrap_or_else(|e| panic!("Failed to open port registry at {REGISTRY_PATH}: {e}"));

    // Acquire exclusive lock (blocks until available)
    file.lock_exclusive()
        .unwrap_or_else(|e| panic!("Failed to lock port registry: {e}"));

    // Read existing registry
    let mut contents = String::new();
    // Need to re-read after lock (another process may have written)
    let mut file_ref = &file;
    let _ = file_ref.read_to_string(&mut contents);

    let mut registry: Registry = if contents.trim().is_empty() {
        Registry::default()
    } else {
        serde_json::from_str(&contents).unwrap_or_else(|e| {
            eprintln!("Warning: corrupt port registry, resetting (error: {e})");
            Registry::default()
        })
    };

    // Clean entries from dead or PID-reused processes.  `kill(pid, 0)` alone
    // is insufficient here because a new process may inherit the old PID.
    let before = registry.allocations.len();
    registry.allocations.retain(process_identity_is_current);
    let cleaned = before - registry.allocations.len();
    if cleaned > 0 {
        eprintln!("TestPortRegistry: cleaned {cleaned} stale entries from dead PIDs");
    }

    // Find next available range start (after all live allocations)
    let mut start = BASE_PORT;
    for alloc in &registry.allocations {
        if alloc.range_end > start {
            start = alloc.range_end;
        }
    }

    // Validate range fits
    let end = start + range_size;
    if end > MAX_PORT {
        // All ranges exhausted — try resetting (all entries might be stale from a
        // previous test run that didn't clean up). Force a full cleanup.
        eprintln!(
            "TestPortRegistry: range would exceed MAX_PORT ({end} > {MAX_PORT}), \
             resetting registry"
        );
        registry.allocations.clear();
        start = BASE_PORT;
    }
    let end = start + range_size;

    // Add our allocation
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let start_time = process_start_time(std::process::id()).unwrap_or(0);

    registry.allocations.push(RegistryEntry {
        pid: std::process::id(),
        binary: current_binary_name(),
        range_start: start,
        range_end: end,
        ts,
        start_time,
    });

    // Write back (truncate first)
    let mut file_write = &file;
    let _ = file_write.seek(SeekFrom::Start(0));
    let _ = file.set_len(0);
    let _ = serde_json::to_writer_pretty(&mut file_write, &registry);

    // Release lock
    let _ = file.unlock();

    eprintln!(
        "TestPortRegistry: claimed ports {start}-{end} for {} (PID {})",
        current_binary_name(),
        std::process::id()
    );

    TestPortRegistry {
        range_start: start,
        range_end: end,
        next_port: AtomicU16::new(start),
    }
}

/// Check if a process is still alive.
fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        kill(Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(not(unix))]
    {
        // On non-Unix, assume alive (conservative — won't clean up stale entries)
        true
    }
}

/// Return the process start time used to disambiguate reused PIDs.
fn process_start_time(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    let system = sysinfo::System::new_all();
    system
        .process(sysinfo::Pid::from_u32(pid))
        .map(|process| process.start_time())
        .filter(|start_time| *start_time != 0)
}

/// Check the complete registry identity, not just whether the PID exists.
fn process_identity_is_current(entry: &RegistryEntry) -> bool {
    is_process_alive(entry.pid)
        && entry.start_time != 0
        && process_start_time(entry.pid) == Some(entry.start_time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_allocate() {
        if std::net::TcpListener::bind(("127.0.0.1", 40000)).is_err() {
            eprintln!("skipping port allocation test: loopback bind unavailable");
            return;
        }
        // Can't use the global singleton in tests (it persists), so test the
        // allocation logic directly.
        let registry = TestPortRegistry {
            range_start: 40000,
            range_end: 40100,
            next_port: AtomicU16::new(40000),
        };

        let p1 = registry.allocate();
        let p2 = registry.allocate();
        assert_ne!(p1, p2);
        assert!((40000..40100).contains(&p1));
        assert!((40000..40100).contains(&p2));
    }

    #[test]
    fn test_registry_allocate_spaced() {
        if std::net::TcpListener::bind(("127.0.0.1", 40000)).is_err() {
            eprintln!("skipping spaced port allocation test: loopback bind unavailable");
            return;
        }
        let registry = TestPortRegistry {
            range_start: 40000,
            range_end: 41000,
            next_port: AtomicU16::new(40000),
        };

        let p1 = registry.allocate_spaced(200);
        let p2 = registry.allocate_spaced(200);
        assert!(
            p2 >= p1 + 200,
            "Spacing should be at least 200: p1={p1}, p2={p2}"
        );
    }

    #[test]
    fn exhausted_spaced_range_uses_bindable_fallback_window() {
        if std::net::TcpListener::bind(("127.0.0.1", 45000)).is_err() {
            eprintln!("skipping fallback-window test: loopback bind unavailable");
            return;
        }
        let registry = TestPortRegistry {
            range_start: 45000,
            range_end: 45001,
            next_port: AtomicU16::new(45001),
        };

        let (port, listener) = registry.allocate_spaced_listener(16);
        assert!(port >= 45001);
        assert_eq!(listener.local_addr().unwrap().port(), port);
    }

    #[test]
    fn test_is_process_alive() {
        // Our own PID should be alive
        assert!(is_process_alive(std::process::id()));
        // PID 0 should not be alive
        assert!(!is_process_alive(0));
    }

    #[test]
    fn test_process_identity_rejects_legacy_and_reused_pid_entries() {
        let pid = std::process::id();
        let start_time = process_start_time(pid).expect("current process has a start time");
        let base = RegistryEntry {
            pid,
            binary: current_binary_name(),
            range_start: 15000,
            range_end: 16000,
            ts: 0,
            start_time,
        };

        assert!(process_identity_is_current(&base));

        let mut reused_pid = base.clone();
        reused_pid.start_time = start_time.saturating_sub(1).max(1);
        assert!(!process_identity_is_current(&reused_pid));

        let mut legacy = base;
        legacy.start_time = 0;
        assert!(!process_identity_is_current(&legacy));
    }
}
