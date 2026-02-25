//! PidFile core implementation
//!
//! Main PidFile struct and its lifecycle management.

#[cfg(not(windows))]
use super::io::read_pid_info_from_file;
use super::io::{write_pid_info, PidInfo};
#[cfg(windows)]
use super::process_check::is_pid_detrix_daemon;
#[cfg(not(windows))]
use super::process_check::is_process_running;
use anyhow::{bail, Context, Result};
use detrix_config::{PortRegistry, ServiceType};
use detrix_logging::debug;
#[cfg(unix)]
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Default API host (127.0.0.1)
fn default_api_host() -> String {
    detrix_config::constants::DEFAULT_API_HOST.to_string()
}

/// PID file manager for single-instance daemon enforcement
///
/// Holds an exclusive lock on the PID file while the daemon is running.
/// The lock is automatically released when the PidFile is dropped.
///
/// Stores PID, host, and all service ports in JSON format.
#[derive(Debug)]
pub struct PidFile {
    /// Path to the PID file
    path: PathBuf,
    /// File handle (kept open to maintain lock)
    file: File,
    /// All allocated ports by service type
    ports: HashMap<ServiceType, u16>,
    /// API host the daemon is listening on
    host: String,
}

impl PidFile {
    /// Acquire PID file lock
    ///
    /// Creates the PID file (and parent directories if needed), acquires an exclusive lock,
    /// and writes the current process ID. Port can be added later via `set_port`.
    ///
    /// # Errors
    ///
    /// Returns error if another instance holds the lock or for I/O errors.
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();

        // Create parent directory if it doesn't exist
        detrix_config::paths::ensure_parent_dir(&path)
            .with_context(|| "Failed to create PID file directory")?;

        // On Windows, don't rely on file locking - it's unreliable.
        // Instead, use process checking as the primary mechanism.
        #[cfg(windows)]
        {
            // Check if PID file exists and contains a running daemon
            if path.exists() {
                if let Ok(Some(info)) = PidInfo::read_from_file(&path) {
                    if info.pid > 0 && is_pid_detrix_daemon(info.pid) {
                        bail!(
                            "Daemon already running (PID: {}, PID file: {})",
                            info.pid,
                            path.display()
                        );
                    }
                    // Process not running - stale file
                    debug!(
                        pid = info.pid,
                        path = %path.display(),
                        "Detected stale PID file (process not running)"
                    );
                } else {
                    // File exists but is empty or unreadable - treat as stale
                    debug!(
                        path = %path.display(),
                        "PID file exists but is empty or unreadable (stale)"
                    );
                }

                // Try to remove stale PID file, but don't fail if we can't
                // We'll overwrite it instead
                match std::fs::remove_file(&path) {
                    Ok(_) => debug!(path = %path.display(), "Removed stale PID file"),
                    Err(e) => debug!(
                        path = %path.display(),
                        error = %e,
                        "Could not remove stale PID file (will overwrite)"
                    ),
                }
            }

            // Create/open PID file
            // IMPORTANT: Don't use truncate(true) - it empties the file BEFORE we write.
            // If the subsequent write fails, the file would remain blank.
            // Instead, we use write_pid_info which does: seek(0) + write + set_len()
            let mut opts = OpenOptions::new();
            opts.read(true).write(true).create(true);
            // FILE_SHARE_READ | FILE_SHARE_WRITE allows other processes to read
            opts.share_mode(3);

            let mut file = opts
                .open(&path)
                .with_context(|| format!("Failed to open PID file: {:?}", path))?;

            // Don't use file locking on Windows - it's unreliable with share modes.
            // Process checking (is_pid_detrix_daemon) is the primary mechanism.

            let host = default_api_host();
            write_pid_info(&mut file, &HashMap::new(), &host)?;

            return Ok(Self {
                path,
                file,
                ports: HashMap::new(),
                host,
            });
        }

        // On Unix, use file locking as the primary mechanism
        #[cfg(not(windows))]
        {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .with_context(|| format!("Failed to open PID file: {:?}", path))?;

            // Try to acquire exclusive lock with retries for transient contention.
            //
            // PidFile::read_info() momentarily acquires an exclusive flock to probe
            // daemon liveness. If our acquire() races with that probe, try_lock_exclusive
            // fails even though no real daemon is running. Retrying with a short delay
            // handles this gracefully while still failing fast when a genuine daemon
            // holds the lock (verified via process liveness check).
            const LOCK_RETRIES: u32 = 5;
            const LOCK_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

            for attempt in 0..LOCK_RETRIES {
                match file.try_lock_exclusive() {
                    Ok(_) => {
                        let mut file = file;
                        let host = default_api_host();
                        write_pid_info(&mut file, &HashMap::new(), &host)?;
                        return Ok(Self {
                            path,
                            file,
                            ports: HashMap::new(),
                            host,
                        });
                    }
                    Err(_) => {
                        // Check if a real daemon process holds the lock
                        let info = read_pid_info_from_file(&file).unwrap_or(PidInfo::new(0));
                        if info.pid > 0 && is_process_running(info.pid) {
                            // Confirmed running daemon — fail immediately
                            bail!(
                                "Daemon already running (PID: {}, PID file: {})",
                                info.pid,
                                path.display()
                            );
                        }
                        // No running daemon — likely transient contention from read_info()
                        debug!(
                            attempt = attempt + 1,
                            max_retries = LOCK_RETRIES,
                            "PID file lock contention (transient), retrying"
                        );
                        std::thread::sleep(LOCK_RETRY_DELAY);
                    }
                }
            }

            // Final attempt after retries
            match file.try_lock_exclusive() {
                Ok(_) => {
                    let mut file = file;
                    let host = default_api_host();
                    write_pid_info(&mut file, &HashMap::new(), &host)?;
                    Ok(Self {
                        path,
                        file,
                        ports: HashMap::new(),
                        host,
                    })
                }
                Err(_) => {
                    let info = read_pid_info_from_file(&file).unwrap_or(PidInfo::new(0));
                    bail!(
                        "Daemon already running (PID: {}, PID file: {})",
                        info.pid,
                        path.display()
                    )
                }
            }
        }
    }

    /// Set the HTTP port in the PID file (backward compatible)
    ///
    /// Call this after the HTTP server has started and the port is known.
    /// For setting multiple ports, use `set_service_port` or `set_ports`.
    #[allow(dead_code)]
    pub fn set_port(&mut self, port: u16) -> Result<()> {
        self.set_service_port(ServiceType::Http, port)
    }

    /// Set a specific service port in the PID file
    ///
    /// Call this after a service has started and its port is known.
    #[allow(dead_code)]
    pub fn set_service_port(&mut self, service: ServiceType, port: u16) -> Result<()> {
        self.ports.insert(service, port);
        write_pid_info(&mut self.file, &self.ports, &self.host)
    }

    /// Set all ports from a PortRegistry
    ///
    /// Call this after all services have been allocated ports.
    #[allow(dead_code)]
    pub fn set_ports(&mut self, registry: &PortRegistry) -> Result<()> {
        self.ports = registry.all_allocated();
        write_pid_info(&mut self.file, &self.ports, &self.host)
    }

    /// Set all ports from a PortRegistry and the host
    ///
    /// Call this after all services have been allocated ports.
    pub fn set_ports_with_host(&mut self, registry: &PortRegistry, host: String) -> Result<()> {
        self.ports = registry.all_allocated();
        self.host = host;
        write_pid_info(&mut self.file, &self.ports, &self.host)
    }

    /// Get the HTTP port (backward compatible)
    #[allow(dead_code)]
    pub fn port(&self) -> Option<u16> {
        self.ports.get(&ServiceType::Http).copied()
    }

    /// Get port for a specific service
    #[allow(dead_code)]
    pub fn get_port(&self, service: ServiceType) -> Option<u16> {
        self.ports.get(&service).copied()
    }

    /// Get all allocated ports
    #[allow(dead_code)]
    pub fn all_ports(&self) -> &HashMap<ServiceType, u16> {
        &self.ports
    }

    /// Check if daemon is running
    ///
    /// # Returns
    ///
    /// - `Some(pid)` if daemon is running with given PID
    /// - `None` if PID file doesn't exist or daemon process is not alive
    pub fn is_running(path: impl AsRef<Path>) -> Result<Option<u32>> {
        Ok(Self::read_info(path)?.map(|info| info.pid))
    }

    /// Read daemon info (PID and port) from PID file
    ///
    /// Uses flock as the primary mechanism on Unix, with process liveness check as backup.
    /// On Windows, uses process checking only.
    ///
    /// # Returns
    ///
    /// - `Some(PidInfo)` if daemon is running (verified by flock and/or process check)
    /// - `None` if PID file doesn't exist, or the PID process is no longer alive
    pub fn read_info(path: impl AsRef<Path>) -> Result<Option<PidInfo>> {
        let path = path.as_ref();

        // If file doesn't exist, daemon is not running
        if !path.exists() {
            return Ok(None);
        }

        // First, try to read file contents using PidInfo::read_from_file()
        // This uses std::fs::read_to_string() which works reliably on all platforms
        // including Windows where the daemon has the file open with FILE_SHARE_READ
        let pid_info = match PidInfo::read_from_file(path) {
            Ok(info) => info,
            Err(e) => {
                debug!(path = %path.display(), "Failed to read PID file: {}", e);
                None
            }
        };

        // On Windows, don't rely on file locking - it's unreliable with share_mode files.
        // Instead, directly check if the PID from the file is a running detrix daemon.
        #[cfg(windows)]
        {
            if let Some(ref info) = pid_info {
                if info.pid > 0 && is_pid_detrix_daemon(info.pid) {
                    debug!(
                        pid = info.pid,
                        "Daemon process is running (verified via tasklist)"
                    );
                    return Ok(pid_info);
                }
            }
            // PID file exists but daemon is not running (stale file)
            debug!(path = %path.display(), "PID file exists but daemon is not running (stale)");
            return Ok(None);
        }

        // On Unix, use file locking as primary mechanism with process check as backup
        #[cfg(not(windows))]
        {
            let verify_daemon_running = |info: Option<PidInfo>| -> Option<PidInfo> {
                if let Some(ref pid_info) = info {
                    // Double-check the process is actually running
                    if pid_info.pid > 0 && is_process_running(pid_info.pid) {
                        return info;
                    }
                }
                // PID 0 or process not running - treat as stale
                None
            };

            let file_result = OpenOptions::new().read(true).write(true).open(path);

            match file_result {
                Ok(file) => {
                    // File opened successfully, try to acquire lock
                    match file.try_lock_exclusive() {
                        Ok(_) => {
                            // Lock acquired successfully — no detrix daemon holds the flock.
                            // The flock is the authoritative liveness mechanism on Unix:
                            // our daemon holds it for its entire lifetime (PidFile::acquire →
                            // PidFile::Drop). If we can acquire it, the daemon is not running,
                            // regardless of whether the stored PID happens to belong to another
                            // unrelated process (e.g. PID reuse by launchd/init = PID 1).
                            drop(file);
                            Ok(None)
                        }
                        Err(_) => {
                            // Lock failed - daemon holds lock, verify process is alive
                            let info = pid_info.or_else(|| read_pid_info_from_file(&file).ok());
                            Ok(verify_daemon_running(info))
                        }
                    }
                }
                Err(e) => {
                    // For other errors, propagate them
                    Err(e).with_context(|| format!("Failed to open PID file: {:?}", path))
                }
            }
        }
    }

    /// Get the path to the PID file
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        // Lock is automatically released when file is closed
        // Delete PID file on clean shutdown
        if let Err(e) = std::fs::remove_file(&self.path) {
            // On Windows, deletion often fails due to file sharing/locking issues.
            // In that case, try to truncate the file to 0 bytes so it's recognized
            // as stale/invalid on next startup.
            #[cfg(windows)]
            {
                // Try to clear the file contents as fallback
                if let Err(e2) = self.file.set_len(0) {
                    eprintln!(
                        "Warning: Failed to remove or truncate PID file: {} / {}",
                        e, e2
                    );
                } else {
                    // Successfully truncated - next startup will see empty file as stale
                    eprintln!(
                        "Warning: Could not remove PID file (truncated instead): {}",
                        e
                    );
                }
            }

            #[cfg(not(windows))]
            {
                // Log error but don't panic in Drop
                eprintln!("Warning: Failed to remove PID file: {}", e);
            }
        }
    }
}
