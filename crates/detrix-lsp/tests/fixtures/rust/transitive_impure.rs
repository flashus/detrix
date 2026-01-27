//! Transitive impurity fixture for Rust LSP purity testing
//!
//! Tests that impurity is correctly propagated through call chains.

/// Entry point - looks pure but transitively impure
/// Calls: middle_layer which eventually calls impure functions
pub fn entry_point(value: i32) -> Result<i32, String> {
    let processed = middle_layer(value)?;
    Ok(processed * 2)
}

/// Middle layer - looks pure but calls impure function
/// Calls: read_file which does file I/O
pub fn middle_layer(value: i32) -> Result<i32, String> {
    let data = read_file("/tmp/config.txt")?;
    Ok(value + data.len() as i32)
}

/// Read file (impure - file I/O)
fn read_file(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| e.to_string())
}

/// Another transitive chain through mutable reference
/// Calls: modify_state which mutates parameter
pub fn transitive_via_mutation(counter: &mut i32) {
    modify_state(counter);
}

/// Modify state via mutable reference (impure - affects caller)
fn modify_state(value: &mut i32) {
    *value += 1;
}

/// Transitive through global state
/// Calls: increment_global
pub fn transitive_via_global() -> usize {
    increment_global()
}

use std::sync::atomic::{AtomicUsize, Ordering};
static GLOBAL_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Increment global counter (impure - global state)
fn increment_global() -> usize {
    GLOBAL_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Transitive through time (impure via system time)
pub fn transitive_via_time() -> String {
    format!("Time: {}", get_timestamp())
}

/// Get current timestamp (impure - system state)
fn get_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Multi-level transitive impurity
/// level1 -> level2 -> level3 -> file_io
pub fn level1(x: i32) -> Result<i32, String> {
    level2(x)
}

fn level2(x: i32) -> Result<i32, String> {
    level3(x)
}

fn level3(x: i32) -> Result<i32, String> {
    let data = file_io()?;
    Ok(x + data as i32)
}

fn file_io() -> Result<usize, String> {
    let content = std::fs::read_to_string("/tmp/data.txt").map_err(|e| e.to_string())?;
    Ok(content.len())
}

/// Pure wrapper around impure function (still impure)
pub fn pure_looking_wrapper(value: i32) -> i32 {
    // This looks pure but calls impure function
    let _ = transitive_via_global();
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transitive_via_global() {
        let before = transitive_via_global();
        let after = transitive_via_global();
        assert!(after > before);
    }
}
