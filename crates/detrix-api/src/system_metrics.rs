//! System metrics collection for process monitoring
//!
//! Provides CPU and memory usage metrics for the current process.

use std::sync::OnceLock;
use sysinfo::{Pid, System};

/// System metrics for the current process
#[derive(Debug, Clone, Default)]
pub struct ProcessMetrics {
    /// CPU usage as a percentage (0.0 to 100.0)
    pub cpu_usage_percent: f32,
    /// Memory usage in bytes
    pub memory_usage_bytes: u64,
}

/// Get current process metrics
///
/// Returns CPU and memory usage for the detrix process.
/// CPU usage is calculated since the last call (requires two calls to get meaningful values).
pub fn get_process_metrics() -> ProcessMetrics {
    static SYSTEM: OnceLock<std::sync::Mutex<System>> = OnceLock::new();

    let system_mutex = SYSTEM.get_or_init(|| std::sync::Mutex::new(System::new_all()));

    let mut system = match system_mutex.lock() {
        Ok(s) => s,
        Err(_) => return ProcessMetrics::default(),
    };

    let pid = Pid::from_u32(std::process::id());

    // Refresh only process information for efficiency
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

    match system.process(pid) {
        Some(process) => ProcessMetrics {
            cpu_usage_percent: process.cpu_usage(),
            memory_usage_bytes: process.memory(),
        },
        None => ProcessMetrics::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_process_metrics() {
        let metrics = get_process_metrics();
        // Memory should be non-zero for a running process
        assert!(metrics.memory_usage_bytes > 0);
        // CPU can be 0 on first call (no baseline)
    }

    #[test]
    fn test_multiple_calls() {
        // First call establishes baseline
        let _ = get_process_metrics();
        // Small delay to let CPU usage accumulate
        std::thread::sleep(std::time::Duration::from_millis(100));
        // Second call should have more meaningful values
        let metrics = get_process_metrics();
        assert!(metrics.memory_usage_bytes > 0);
    }
}
