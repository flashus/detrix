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
    /// Claims a port range of `DEFAULT_RANGE_SIZE` (500) ports from the shared
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
                panic!(
                    "TestPortRegistry: range exhausted ({}-{}). \
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

#[derive(Serialize, Deserialize)]
struct RegistryEntry {
    pid: u32,
    binary: String,
    range_start: u16,
    range_end: u16,
    ts: u64,
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

    // Clean entries from dead PIDs
    let before = registry.allocations.len();
    registry
        .allocations
        .retain(|entry| is_process_alive(entry.pid));
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

    registry.allocations.push(RegistryEntry {
        pid: std::process::id(),
        binary: current_binary_name(),
        range_start: start,
        range_end: end,
        ts,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_allocate() {
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
        assert!(p1 >= 40000 && p1 < 40100);
        assert!(p2 >= 40000 && p2 < 40100);
    }

    #[test]
    fn test_registry_allocate_spaced() {
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
    fn test_is_process_alive() {
        // Our own PID should be alive
        assert!(is_process_alive(std::process::id()));
        // PID 0 should not be alive
        assert!(!is_process_alive(0));
    }
}
