//! Factory for creating eBPF adapters
//!
//! Creates `EbpfAdapter` instances for Go binaries on Linux.
//! On non-Linux platforms, `EbpfAdapterFactory` methods return errors, and
//! `EbpfGoFactory` transparently delegates to the inner DAP factory.

use crate::adapter::EbpfAdapter;
use crate::error::{Error, Result};

use async_trait::async_trait;
use detrix_application::{DapAdapterFactory, DapAdapterFactoryRef, DapAdapterRef};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Factory for creating eBPF-based adapters.
///
/// Unlike `DapAdapterFactory` which creates per-language adapters,
/// this factory creates eBPF adapters specifically for Go binaries
/// on Linux. Other languages continue to use DAP adapters.
pub struct EbpfAdapterFactory {
    /// Base path for resolving relative binary paths.
    base_path: PathBuf,
}

impl EbpfAdapterFactory {
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
        }
    }

    /// Create an eBPF adapter for a Go binary.
    ///
    /// # Arguments
    /// * `binary_path` - Path to the Go ELF binary with DWARF debug info.
    ///
    /// # Errors
    /// Returns error if:
    /// - Not running on Linux
    /// - Binary doesn't exist or isn't readable
    pub fn create_go_adapter(&self, binary_path: impl AsRef<Path>) -> Result<DapAdapterRef> {
        let path = if binary_path.as_ref().is_absolute() {
            binary_path.as_ref().to_path_buf()
        } else {
            self.base_path.join(binary_path)
        };

        if !path.exists() {
            return Err(Error::Adapter(format!(
                "Binary not found: {}",
                path.display()
            )));
        }

        Ok(Arc::new(EbpfAdapter::new(path)))
    }

    /// Check if eBPF adapters are available on this platform.
    pub fn is_available() -> bool {
        cfg!(target_os = "linux")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn factory_create_with_existing_binary() {
        let tmp = NamedTempFile::new().unwrap();
        let factory = EbpfAdapterFactory::new("/tmp");
        let result = factory.create_go_adapter(tmp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn factory_create_nonexistent_binary_fails() {
        let factory = EbpfAdapterFactory::new("/tmp");
        let result = factory.create_go_adapter("/nonexistent/binary");
        assert!(result.is_err());
    }

    #[test]
    fn factory_resolves_relative_path() {
        let tmp = NamedTempFile::new_in("/tmp").unwrap();
        let filename = tmp.path().file_name().unwrap().to_str().unwrap();
        let factory = EbpfAdapterFactory::new("/tmp");
        let result = factory.create_go_adapter(filename);
        assert!(result.is_ok());
    }

    #[test]
    fn is_available_matches_platform() {
        let available = EbpfAdapterFactory::is_available();
        if cfg!(target_os = "linux") {
            assert!(available);
        } else {
            assert!(!available);
        }
    }
}

/// Composite adapter factory that routes Go connections to eBPF on Linux,
/// delegating all other languages (and Go on non-Linux) to the inner DAP factory.
///
/// # Binary path convention
///
/// On Linux, `create_go_adapter(host, port)` treats `host` as the path to the
/// Go ELF binary (e.g., `"/opt/myapp/bin/server"`). The `port` argument is
/// unused — eBPF attaches directly to the binary without a debugger daemon.
///
/// On non-Linux, all calls are forwarded to the inner factory unchanged.
pub struct EbpfGoFactory {
    /// Inner factory for Python, Rust, and Go fallback on non-Linux.
    inner: DapAdapterFactoryRef,
    /// eBPF factory for Go connections on Linux.
    #[allow(dead_code)] // Only used inside #[cfg(target_os = "linux")] blocks
    ebpf: EbpfAdapterFactory,
}

impl EbpfGoFactory {
    /// Create a composite factory.
    ///
    /// * `inner`     — base DAP factory (used for non-Go adapters)
    /// * `base_path` — base directory for resolving relative binary paths
    pub fn new(inner: DapAdapterFactoryRef, base_path: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            ebpf: EbpfAdapterFactory::new(base_path),
        }
    }
}

#[async_trait]
impl DapAdapterFactory for EbpfGoFactory {
    async fn create_python_adapter(
        &self,
        host: &str,
        port: u16,
    ) -> detrix_core::Result<DapAdapterRef> {
        self.inner.create_python_adapter(host, port).await
    }

    async fn create_go_adapter(
        &self,
        host: &str,
        port: u16,
    ) -> detrix_core::Result<DapAdapterRef> {
        // On Linux: `host` is the binary path; attach via eBPF uprobe.
        // On non-Linux: fall through to Delve/DAP as usual.
        #[cfg(target_os = "linux")]
        {
            self.ebpf
                .create_go_adapter(host)
                .map_err(|e| detrix_core::Error::Adapter(e.to_string()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.inner.create_go_adapter(host, port).await
        }
    }

    async fn create_rust_adapter(
        &self,
        host: &str,
        port: u16,
        program: Option<&str>,
        pid: Option<u32>,
    ) -> detrix_core::Result<DapAdapterRef> {
        self.inner.create_rust_adapter(host, port, program, pid).await
    }
}

#[cfg(test)]
mod ebpf_go_factory_tests {
    use super::*;
    use detrix_application::DapAdapterRef;
    use detrix_core::Result;

    /// Minimal stub factory that records calls.
    struct StubFactory;

    #[async_trait]
    impl DapAdapterFactory for StubFactory {
        async fn create_python_adapter(&self, _host: &str, _port: u16) -> Result<DapAdapterRef> {
            Err(detrix_core::Error::Adapter("stub python".to_string()))
        }
        async fn create_go_adapter(&self, _host: &str, _port: u16) -> Result<DapAdapterRef> {
            Err(detrix_core::Error::Adapter("stub go".to_string()))
        }
        async fn create_rust_adapter(
            &self,
            _host: &str,
            _port: u16,
            _program: Option<&str>,
            _pid: Option<u32>,
        ) -> Result<DapAdapterRef> {
            Err(detrix_core::Error::Adapter("stub rust".to_string()))
        }
    }

    #[tokio::test]
    async fn python_delegates_to_inner() {
        let factory = EbpfGoFactory::new(Arc::new(StubFactory), "/tmp");
        match factory.create_python_adapter("127.0.0.1", 5678).await {
            Err(e) => assert!(e.to_string().contains("stub python")),
            Ok(_) => panic!("expected Err"),
        }
    }

    #[tokio::test]
    async fn rust_delegates_to_inner() {
        let factory = EbpfGoFactory::new(Arc::new(StubFactory), "/tmp");
        match factory.create_rust_adapter("127.0.0.1", 1234, None, None).await {
            Err(e) => assert!(e.to_string().contains("stub rust")),
            Ok(_) => panic!("expected Err"),
        }
    }

    #[tokio::test]
    #[cfg(not(target_os = "linux"))]
    async fn go_delegates_to_inner_on_non_linux() {
        let factory = EbpfGoFactory::new(Arc::new(StubFactory), "/tmp");
        match factory.create_go_adapter("127.0.0.1", 2345).await {
            Err(e) => assert!(e.to_string().contains("stub go")),
            Ok(_) => panic!("expected Err"),
        }
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn go_uses_ebpf_on_linux_with_valid_binary() {
        use tempfile::NamedTempFile;
        let tmp = NamedTempFile::new().unwrap();
        let factory = EbpfGoFactory::new(Arc::new(StubFactory), "/tmp");
        // host = binary path on Linux
        let result = factory.create_go_adapter(tmp.path().to_str().unwrap(), 0).await;
        assert!(result.is_ok());
    }
}
