//! Impure function chain fixture for Rust LSP purity testing
//!
//! Functions with side effects: file I/O, network, system calls, global state

use std::fs::File;
use std::io::{Read, Write};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Global order counter (mutable global state)
static ORDER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Process an order (impure - modifies global state)
/// Calls: save_to_database, write_log
pub fn process_order(order_id: &str, amount: f64) -> Result<String, String> {
    // Modify global state
    let count = ORDER_COUNT.fetch_add(1, Ordering::SeqCst);

    // Call other impure functions
    save_to_database(order_id, amount)?;
    write_log(&format!("Processed order {} (count: {})", order_id, count));

    Ok(format!("Order {} processed", order_id))
}

/// Save data to database (impure - file I/O simulation)
pub fn save_to_database(order_id: &str, amount: f64) -> Result<(), String> {
    // This would be a database write in real code
    let mut file = File::create("/tmp/order.txt").map_err(|e| e.to_string())?;
    write!(file, "{}:{}", order_id, amount).map_err(|e| e.to_string())?;
    Ok(())
}

/// Write log message (impure - I/O)
/// Using println which is "acceptable impurity"
pub fn write_log(message: &str) {
    println!("LOG: {}", message);
}

/// Execute a shell command (impure - system call)
pub fn execute_command(cmd: &str) -> Result<String, String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map_err(|e| e.to_string())?;

    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

/// Read configuration from file (impure - file I/O)
pub fn read_config(path: &str) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).map_err(|e| e.to_string())?;
    Ok(contents)
}

/// Write data to file (impure - file I/O)
pub fn write_data(path: &str, data: &str) -> Result<(), String> {
    let mut file = File::create(path).map_err(|e| e.to_string())?;
    file.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

/// Get current time (impure - system state)
pub fn get_current_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Create a thread (impure - concurrency)
pub fn spawn_worker(task_id: usize) -> std::thread::JoinHandle<usize> {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(10));
        task_id * 2
    })
}

/// Modify mutable reference parameter (impure - affects caller)
pub fn increment_counter(counter: &mut i32) {
    *counter += 1;
}

/// Unsafe memory operation (impure)
pub unsafe fn unsafe_memory_access(ptr: *mut i32, value: i32) {
    *ptr = value;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_log() {
        // This test just ensures the function compiles
        write_log("test message");
    }
}
