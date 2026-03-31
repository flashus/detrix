//! Process memory reader for user-space dereferencing
//!
//! Used to read Go string content from heap memory after eBPF captures {ptr, len}.
//! eBPF's `bpf_probe_read_user` cannot access Go's heap, so we read from user-space
//! after the event is received.

use crate::error::Result;

/// Trait for reading memory from another process.
pub trait ProcessMemoryReader: Send + Sync {
    /// Read a string from the target process memory.
    ///
    /// # Arguments
    /// * `pid` - Process ID of the target process
    /// * `ptr` - Memory address of the string data
    /// * `len` - Length of the string in bytes
    ///
    /// # Returns
    /// The string content as UTF-8 text, or an error if the read fails.
    fn read_string(&self, pid: u32, ptr: u64, len: usize) -> Result<String>;
}

/// Linux implementation using process_vm_readv syscall.
///
/// Falls back to /proc/pid/mem if process_vm_readv is unavailable.
#[cfg(target_os = "linux")]
pub struct LinuxProcessMemoryReader {
    /// Cached PID for the target process (found by executable path)
    target_pid: std::sync::Mutex<Option<u32>>,
    /// Path to the target executable (used to find the PID)
    target_exe: std::sync::Arc<str>,
}

#[cfg(target_os = "linux")]
impl LinuxProcessMemoryReader {
    pub fn new(target_exe: &str) -> Self {
        Self {
            target_pid: std::sync::Mutex::new(None),
            target_exe: target_exe.into(),
        }
    }
    
    /// Find the target process PID by searching /proc for matching executable
    fn find_target_pid(&self) -> Option<u32> {
        // Check cache first
        {
            let cache = self.target_pid.lock().ok()?;
            if let Some(pid) = *cache {
                // Verify process still exists
                if std::path::Path::new(&format!("/proc/{}", pid)).exists() {
                    return Some(pid);
                }
            }
        }
        
        // Search /proc for matching executable
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if let Ok(pid) = name_str.parse::<u32>() {
                    // Check if /proc/[pid]/exe matches our target
                    let exe_path = format!("/proc/{}/exe", pid);
                    if let Ok(link_target) = std::fs::read_link(&exe_path) {
                        let link_str = link_target.to_string_lossy();
                        if link_str.as_ref() == self.target_exe.as_ref() {
                            // Found it! Cache and return
                            let mut cache = self.target_pid.lock().ok()?;
                            *cache = Some(pid);
                            detrix_logging::debug!(
                                "[mem_reader] Found target PID {} for {}",
                                pid, self.target_exe
                            );
                            return Some(pid);
                        }
                    }
                }
            }
        }
        
        detrix_logging::warn!(
            "[mem_reader] Could not find target process: {}",
            self.target_exe
        );
        None
    }
}

#[cfg(target_os = "linux")]
impl ProcessMemoryReader for LinuxProcessMemoryReader {
    fn read_string(&self, _kernel_pid: u32, ptr: u64, len: usize) -> Result<String> {
        use crate::error::Error;
        
        // Find the target process PID in our namespace by searching /proc
        let target_pid = self.find_target_pid().ok_or_else(|| {
            Error::Ebpf(format!("Could not find target process: {}", self.target_exe))
        })?;
        
        detrix_logging::debug!(
            "[mem_reader] Reading string from target PID {} (kernel reported {}): ptr={:#x} len={}",
            target_pid, _kernel_pid, ptr, len
        );
        
        // Limit read size to prevent excessive memory access
        let read_len = len.min(1024);
        
        // Try process_vm_readv first
        match process_vm_read(target_pid, ptr, read_len) {
            Ok(buf) => {
                let actual_len = buf.len().min(len);
                if actual_len > 0 && buf.iter().any(|&b| b != 0) {
                    return String::from_utf8(buf[..actual_len].to_vec())
                        .map_err(|e| Error::Ebpf(format!("Invalid UTF-8 in string: {e}")));
                }
            }
            Err(e) => {
                detrix_logging::debug!(
                    "[mem_reader] process_vm_readv failed: {}, trying /proc/mem fallback",
                    e
                );
            }
        }

        // Fallback: try /proc/target_pid/mem
        match read_proc_mem(target_pid, ptr, read_len) {
            Ok(buf) => {
                let actual_len = buf.len().min(len);
                if actual_len > 0 && buf.iter().any(|&b| b != 0) {
                    return String::from_utf8(buf[..actual_len].to_vec())
                        .map_err(|e| Error::Ebpf(format!("Invalid UTF-8 in string: {e}")));
                }
            }
            Err(e) => {
                detrix_logging::debug!(
                    "[mem_reader] /proc/{}/mem failed: {}",
                    target_pid, e
                );
            }
        }
        
        Err(Error::Ebpf(format!(
            "Failed to read string from target_pid={} ptr={:#x} len={}",
            target_pid, ptr, len
        )))
    }
}

/// Read memory from another process using process_vm_readv syscall.
///
/// This is the preferred method as it:
/// - Doesn't require ptrace attachment
/// - Doesn't stop the target process
/// - Is faster than /proc/pid/mem
#[cfg(target_os = "linux")]
fn process_vm_read(pid: u32, addr: u64, len: usize) -> Result<Vec<u8>> {
    use crate::error::Error;
    use libc::{iovec, process_vm_readv};
    use std::ffi::c_void;

    let mut buf = vec![0u8; len];

    // Local iovec: where to write the data
    let local_iov = iovec {
        iov_base: buf.as_mut_ptr() as *mut c_void,
        iov_len: len,
    };

    // Remote iovec: where to read from in target process
    let remote_iov = iovec {
        iov_base: addr as *mut c_void,
        iov_len: len,
    };

    unsafe {
        let result = process_vm_readv(
            pid as libc::pid_t,
            &local_iov as *const iovec,
            1,
            &remote_iov as *const iovec,
            1,
            0,
        );

        if result == -1 {
            Err(Error::Ebpf(format!(
                "process_vm_readv failed: {}",
                std::io::Error::last_os_error()
            )))
        } else if result == 0 {
            Err(Error::Ebpf(
                "process_vm_readv returned 0 (address may be invalid)".to_string()
            ))
        } else {
            // Truncate buffer to actual bytes read
            buf.truncate(result as usize);
            Ok(buf)
        }
    }
}

/// Read memory from /proc/pid/mem as a fallback.
///
/// This is slower and may require ptrace permissions, but works in more environments.
#[cfg(target_os = "linux")]
fn read_proc_mem(pid: u32, addr: u64, len: usize) -> Result<Vec<u8>> {
    use crate::error::Error;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mem_path = format!("/proc/{}/mem", pid);
    let mut mem_file = File::open(&mem_path)
        .map_err(|e| Error::Ebpf(format!("Failed to open {}: {}", mem_path, e)))?;

    // Seek to the address
    mem_file
        .seek(SeekFrom::Start(addr))
        .map_err(|e| Error::Ebpf(format!("Failed to seek to {:#x}: {}", addr, e)))?;

    // Read the data
    let mut buf = vec![0u8; len];
    let bytes_read = mem_file
        .read(&mut buf)
        .map_err(|e| Error::Ebpf(format!("Failed to read from {}: {}", mem_path, e)))?;

    buf.truncate(bytes_read);
    Ok(buf)
}

/// Stub implementation for non-Linux platforms (for testing/compilation).
#[cfg(not(target_os = "linux"))]
pub struct StubProcessMemoryReader;

#[cfg(not(target_os = "linux"))]
impl ProcessMemoryReader for StubProcessMemoryReader {
    fn read_string(&self, _pid: u32, _ptr: u64, len: usize) -> Result<String> {
        // Return a placeholder string for testing on non-Linux
        Ok(format!("<stub-string-len-{len}>"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_reader_construction() {
        let _reader = LinuxProcessMemoryReader::new("/test/binary");
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn stub_reader_returns_placeholder() {
        let reader = StubProcessMemoryReader;
        let result = reader.read_string(1234, 0x1000, 10);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("stub-string"));
    }
}
