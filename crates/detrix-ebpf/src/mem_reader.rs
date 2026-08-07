//! Process memory reader for user-space dereferencing
//!
//! Used to read Go string content from heap memory after eBPF captures {ptr, len}.
//! eBPF's `bpf_probe_read_user` cannot access Go's heap, so we read from user-space
//! after the event is received.
//!
//! # Struct Field Reading
//!
//! For structs, the pattern is:
//! 1. BPF captures struct base address from stack
//! 2. User-space reads DWARF field offsets
//! 3. User-space reads each field via process_vm_readv(base_addr + field_offset)

#[allow(unused_imports)] // used inside #[cfg(target_os = "linux")] blocks
use crate::error::{ErrContext, Error, Result};
use std::sync::Arc;

/// Type alias for process memory reader — consistent with FooRef convention.
pub type ProcessMemoryReaderRef = Arc<dyn ProcessMemoryReader>;

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

    /// Read raw bytes from the target process memory.
    ///
    /// # Arguments
    /// * `pid` - Process ID of the target process  
    /// * `ptr` - Memory address to read from
    /// * `len` - Number of bytes to read
    ///
    /// # Returns
    /// The raw bytes, or an error if the read fails.
    fn read_bytes(&self, pid: u32, ptr: u64, len: usize) -> Result<Vec<u8>>;

    /// Read a u64 value from the target process memory.
    ///
    /// # Arguments
    /// * `pid` - Process ID of the target process
    /// * `ptr` - Memory address to read from (must be 8-byte aligned)
    ///
    /// # Returns
    /// The u64 value, or an error if the read fails.
    fn read_u64(&self, pid: u32, ptr: u64) -> Result<u64>;
}

/// Linux implementation using process_vm_readv syscall.
///
/// The `pid` parameter passed to each method is the **namespace-local PID** emitted
/// by the BPF program via `bpf_get_ns_current_pid_tgid()`. This is the PID as seen
/// inside the daemon's PID namespace (i.e. the container-local PID when running in
/// Docker), which is what `process_vm_readv` and `/proc/<pid>/mem` expect.
///
/// Falls back to /proc/pid/mem if process_vm_readv is unavailable.
#[cfg(target_os = "linux")]
pub struct LinuxProcessMemoryReader;

#[cfg(target_os = "linux")]
impl LinuxProcessMemoryReader {
    pub fn new(_target_exe: &str) -> Self {
        Self
    }

    /// Check if a process is still alive. Returns false if the process has exited.
    fn pid_exists(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }
}

#[cfg(target_os = "linux")]
impl ProcessMemoryReader for LinuxProcessMemoryReader {
    fn read_string(&self, pid: u32, ptr: u64, len: usize) -> Result<String> {
        use crate::error::Error;

        detrix_logging::debug!(
            "[mem_reader] Reading string from pid={}: ptr={:#x} len={}",
            pid,
            ptr,
            len
        );

        if !Self::pid_exists(pid) {
            return Err(Error::Ebpf(format!(
                "Process {pid} has exited — cannot read string at {:#x}",
                ptr
            )));
        }

        // Limit read size to prevent excessive memory access
        let read_len = len.min(1024);

        // Try process_vm_readv first
        match process_vm_read(pid, ptr, read_len) {
            Ok(buf) => {
                let actual_len = buf.len().min(len);
                if actual_len > 0 && buf.iter().any(|&b| b != 0) {
                    return Ok(String::from_utf8(buf[..actual_len].to_vec())
                        .context("Invalid UTF-8 in string")?);
                }
            }
            Err(e) => {
                detrix_logging::debug!(
                    "[mem_reader] process_vm_readv failed for pid={}: {}, trying /proc/mem fallback",
                    pid, e
                );
            }
        }

        // Fallback: try /proc/pid/mem
        match read_proc_mem(pid, ptr, read_len) {
            Ok(buf) => {
                let actual_len = buf.len().min(len);
                if actual_len > 0 && buf.iter().any(|&b| b != 0) {
                    return Ok(String::from_utf8(buf[..actual_len].to_vec())
                        .context("Invalid UTF-8 in string")?);
                }
            }
            Err(e) => {
                detrix_logging::debug!("[mem_reader] /proc/{}/mem failed: {}", pid, e);
            }
        }

        Err(Error::Ebpf(format!(
            "Failed to read string from pid={} ptr={:#x} len={}",
            pid, ptr, len
        )))
    }

    fn read_bytes(&self, pid: u32, ptr: u64, len: usize) -> Result<Vec<u8>> {
        if !Self::pid_exists(pid) {
            return Err(crate::error::Error::Ebpf(format!(
                "Process {pid} has exited — cannot read bytes at {:#x}",
                ptr
            )));
        }
        // Try process_vm_readv first
        if let Ok(buf) = process_vm_read(pid, ptr, len) {
            return Ok(buf);
        }
        // Fallback to /proc/pid/mem
        read_proc_mem(pid, ptr, len)
    }

    fn read_u64(&self, pid: u32, ptr: u64) -> Result<u64> {
        let bytes = self.read_bytes(pid, ptr, 8)?;
        if bytes.len() < 8 {
            return Err(crate::error::Error::Ebpf(format!(
                "read_u64: only got {} bytes",
                bytes.len()
            )));
        }
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
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
                "process_vm_readv returned 0 (address may be invalid)".to_string(),
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

    fn read_bytes(&self, _pid: u32, _ptr: u64, len: usize) -> Result<Vec<u8>> {
        Ok(vec![0u8; len])
    }

    fn read_u64(&self, _pid: u32, _ptr: u64) -> Result<u64> {
        Ok(0)
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
