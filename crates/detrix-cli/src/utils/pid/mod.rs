//! PID File Management for Single-Instance Daemon
//!
//! Implements cross-platform PID file locking to ensure only one daemon instance runs at a time.
//! Stores PID and all service ports in JSON format.
//!
//! ## Algorithm
//!
//! 1. Check if PID file exists
//! 2. If exists:
//!    a. Try to acquire exclusive lock (flock/LockFileEx)
//!    b. If lock succeeds → stale file (previous crash), overwrite
//!    c. If lock fails → daemon running, exit with error
//! 3. If not exists:
//!    a. Create PID file
//!    b. Acquire exclusive lock
//!    c. Write current PID (ports added later via `set_port` or `set_ports`)
//! 4. On shutdown:
//!    a. Release lock (automatic on process exit)
//!    b. Delete PID file
//!
//! ## File Format (JSON)
//!
//! ```json
//! {"pid":12345,"ports":{"http":8080,"grpc":50051},"host":"127.0.0.1"}
//! ```
//!
//! ## Example
//!
//! ```no_run
//! use detrix_cli::utils::pid::PidFile;
//! use detrix_config::ServiceType;
//! use std::path::PathBuf;
//!
//! # fn main() -> Result<(), anyhow::Error> {
//! let mut pid_file = PidFile::acquire(PathBuf::from(".detrix/daemon.pid"))?;
//! // After servers start and ports are known:
//! pid_file.set_port(8080)?;  // Sets HTTP port (backward compatible)
//! // Or use set_service_port for specific services:
//! pid_file.set_service_port(ServiceType::Grpc, 50051)?;
//! // PID file automatically released and deleted when pid_file is dropped
//! # Ok(())
//! # }
//! ```

mod core;
mod io;
mod process_check;

#[cfg(test)]
mod tests;

// Re-exports
pub use core::PidFile;
pub use io::PidInfo;
