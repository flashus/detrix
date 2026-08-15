//! Factory for creating eBPF adapters
//!
//! Creates `EbpfAdapter` instances for Go binaries on Linux.
//! On non-Linux platforms, `EbpfAdapterFactory` methods return errors, and
//! `EbpfGoFactory` transparently delegates to the inner DAP factory.

use crate::adapter::EbpfAdapter;
use crate::debug_image::{
    DebugImageError, DebugImageMetadata, DebugImageProvider, EmbeddedDebugImageProvider,
    ExternalDebugImageProvider,
};
use crate::error::{Error, Result};
use crate::probe::types::CaptureConfig;
use crate::profile::{LanguageProfile, ProfileId};
use crate::registry::{BackendRegistry, CaptureBackendFactory, ProfileRegistry};

use async_trait::async_trait;
use detrix_ports::{DapAdapterFactory, DapAdapterFactoryRef, DapAdapterRef};
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
    /// Capture limits for generated BPF programs and ring buffer parsing.
    capture_config: CaptureConfig,
    profiles: ProfileRegistry,
    backends: BackendRegistry,
}

impl EbpfAdapterFactory {
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
            capture_config: CaptureConfig::default(),
            profiles: ProfileRegistry::with_defaults(),
            backends: BackendRegistry::with_defaults(),
        }
    }

    /// Create a factory with custom capture limits derived from config.
    pub fn new_with_config(base_path: impl Into<PathBuf>, capture_config: CaptureConfig) -> Self {
        Self {
            base_path: base_path.into(),
            capture_config,
            profiles: ProfileRegistry::with_defaults(),
            backends: BackendRegistry::with_defaults(),
        }
    }

    /// Register an additional language profile before the factory is shared
    /// with the agent. The profile key is later resolved through the same
    /// registry used by built-in Go/Rust adapters.
    pub fn register_profile(&mut self, profile: Arc<dyn LanguageProfile>) {
        self.profiles.register(profile);
    }

    /// Register an additional capture backend. Backends remain responsible
    /// for attachment/runtime mechanics; profiles remain responsible for
    /// language/type lowering.
    pub fn register_backend(&mut self, backend: Arc<dyn CaptureBackendFactory>) {
        self.backends.register(backend);
    }

    pub fn has_registered_profile(&self, profile: &str) -> bool {
        self.profiles.get(profile).is_some()
    }

    pub fn has_registered_backend_for_profile(&self, profile: &str) -> bool {
        self.backends.for_profile_name(profile).is_some()
    }

    /// String-keyed construction entry point for control-plane callers.
    /// Built-in profiles are mapped to their compatibility identities; an
    /// unknown registered profile is rejected until its runtime adapter can
    /// carry the profile object end-to-end.
    pub fn create_registered_adapter(
        &self,
        profile: &str,
        binary_path: impl AsRef<Path>,
    ) -> Result<DapAdapterRef> {
        self.create_registered_adapter_with_debug_path(profile, binary_path, None::<&Path>)
    }

    /// String-keyed construction with an optional external DWARF image. This
    /// is the manager-facing path for built-in and third-party profiles alike.
    pub fn create_registered_adapter_with_debug_path(
        &self,
        profile: &str,
        binary_path: impl AsRef<Path>,
        debug_path: Option<impl AsRef<Path>>,
    ) -> Result<DapAdapterRef> {
        let profile = profile.trim().to_ascii_lowercase();
        let profile_impl = self
            .profiles
            .get(&profile)
            .ok_or_else(|| Error::Adapter(format!("No registered eBPF profile for {profile}")))?;
        let backend = self
            .backends
            .for_profile_name(&profile)
            .ok_or_else(|| Error::Adapter(format!("No registered eBPF backend for {profile}")))?;
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
        let debug_path = debug_path.map(|candidate| {
            let candidate = candidate.as_ref();
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                self.base_path.join(candidate)
            }
        });
        if let Some(debug_path) = &debug_path {
            if !debug_path.exists() {
                return Err(Error::Adapter(format!(
                    "Debug image not found: {}",
                    debug_path.display()
                )));
            }
        }
        backend.create_adapter_with_profile_and_debug_path(
            &profile,
            profile_impl,
            &path,
            &self.base_path,
            &self.capture_config,
            debug_path.as_deref(),
        )
    }

    /// Create an eBPF adapter for a Go or Rust ELF binary.
    ///
    /// # Arguments
    /// * `binary_path` - Path to the Go ELF binary with DWARF debug info.
    ///
    /// # Errors
    /// Returns error if:
    /// - Not running on Linux
    /// - Binary doesn't exist or isn't readable
    pub fn create_go_adapter(&self, binary_path: impl AsRef<Path>) -> Result<DapAdapterRef> {
        self.create_profile_adapter(ProfileId::Go, binary_path)
    }

    pub fn create_rust_adapter(&self, binary_path: impl AsRef<Path>) -> Result<DapAdapterRef> {
        self.create_profile_adapter(ProfileId::Rust, binary_path)
    }

    fn create_profile_adapter(
        &self,
        profile: ProfileId,
        binary_path: impl AsRef<Path>,
    ) -> Result<DapAdapterRef> {
        self.create_profile_adapter_with_debug_path(profile, binary_path, None::<&Path>)
    }

    pub fn create_adapter_with_debug_path(
        &self,
        profile: ProfileId,
        binary_path: impl AsRef<Path>,
        debug_path: Option<impl AsRef<Path>>,
    ) -> Result<DapAdapterRef> {
        self.create_profile_adapter_with_debug_path(profile, binary_path, debug_path)
    }

    /// Resolve the debug image before constructing an adapter.  This is the
    /// transactional preflight used by the agent backend selector: a binary
    /// with symbols but no usable variable DWARF is not observation-ready.
    pub fn preflight_debug_image(
        &self,
        _profile: ProfileId,
        binary_path: impl AsRef<Path>,
        debug_path: Option<impl AsRef<Path>>,
    ) -> Result<DebugImageMetadata> {
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
        let provider =
            ExternalDebugImageProvider::new(path.parent().into_iter().map(Path::to_path_buf));
        let configured = debug_path.map(|p| {
            let candidate = p.as_ref();
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                self.base_path.join(candidate)
            }
        });
        let selected = configured.clone().unwrap_or_else(|| path.clone());
        if !selected.exists() {
            return Err(Error::Adapter(format!(
                "Debug image not found: {}",
                selected.display()
            )));
        }
        let metadata = if configured.is_some() && selected != path {
            let selected_metadata = EmbeddedDebugImageProvider
                .load(&selected)
                .map_err(|error: DebugImageError| Error::Adapter(error.to_string()))?;
            DebugImageMetadata {
                path: path.clone(),
                source: if selected_metadata.has_variable_dwarf {
                    crate::debug_image::DebugImageSource::External
                } else {
                    selected_metadata.source
                },
                debug_path: selected,
                ..selected_metadata
            }
        } else {
            provider
                .load(&path)
                .map_err(|error: DebugImageError| Error::Adapter(error.to_string()))?
        };
        if !metadata.has_variable_dwarf {
            return Err(Error::Adapter(format!(
                "Debug image has no usable variable DWARF: {}",
                metadata.debug_path.display()
            )));
        }
        Ok(metadata)
    }

    fn create_profile_adapter_with_debug_path(
        &self,
        profile: ProfileId,
        binary_path: impl AsRef<Path>,
        debug_path: Option<impl AsRef<Path>>,
    ) -> Result<DapAdapterRef> {
        if self.profiles.get(profile.as_str()).is_none() {
            return Err(Error::Adapter(format!(
                "No registered eBPF profile for {}",
                profile.as_str()
            )));
        }
        let backend = self
            .backends
            .for_profile(profile)
            .ok_or_else(|| Error::Adapter("No registered eBPF backend".into()))?;
        if !backend.supports(profile) {
            return Err(Error::Adapter(format!(
                "Registered eBPF backend does not support {}",
                profile.as_str()
            )));
        }
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

        let debug_path = debug_path.map(|debug| {
            let path = debug.as_ref();
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.base_path.join(path)
            }
        });
        if let Some(debug_path) = &debug_path {
            if !debug_path.exists() {
                return Err(Error::Adapter(format!(
                    "Debug image not found: {}",
                    debug_path.display()
                )));
            }
        }
        let profile_impl = self.profiles.get(profile.as_str()).ok_or_else(|| {
            Error::Adapter(format!(
                "No registered eBPF profile for {}",
                profile.as_str()
            ))
        })?;
        let adapter = EbpfAdapter::new_with_profile_object_and_debug_path(
            path,
            self.capture_config.clone(),
            profile,
            profile_impl,
            debug_path,
        )
        .map_err(|e: crate::error::Error| Error::Adapter(e.to_string()))?;
        Ok(Arc::new(adapter) as DapAdapterRef)
    }

    /// Profile-dispatched construction seam.  Rust is registered in the
    /// profile registry; Rust is scalar-only and must never reuse Go runtime
    /// layout handling for composites.
    pub fn create_adapter(
        &self,
        profile: ProfileId,
        binary_path: impl AsRef<Path>,
    ) -> Result<DapAdapterRef> {
        self.create_profile_adapter(profile, binary_path)
    }

    /// Check if eBPF adapters are available on this platform.
    pub fn is_available() -> bool {
        cfg!(target_os = "linux")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::CaptureCompiler;
    use crate::profile::{LanguageProfile, ProfileCapabilities, TypeDescriptor};
    use crate::registry::CaptureBackendFactory;
    use crate::runtime::RuntimeError;
    use crate::ScalarFieldSpec;
    use detrix_ports::DapAdapterRef;
    use std::path::Path;
    use tempfile::NamedTempFile;

    #[derive(Debug)]
    struct DynamicProfile;

    impl LanguageProfile for DynamicProfile {
        fn id(&self) -> &'static str {
            "test-language"
        }
        fn architectures(&self) -> &'static [crate::dwarf::types::TargetArchitecture] {
            &[]
        }
        fn capabilities(&self) -> ProfileCapabilities {
            ProfileCapabilities {
                scalar: true,
                pointer: false,
                inline_struct: false,
                fixed_array: false,
                string: false,
                borrowed_str: false,
                vector: false,
                borrowed_slice: false,
                slice: false,
                enumeration: false,
                niche_enumeration: false,
                trait_object: false,
                async_state: false,
            }
        }
        fn classify_type(&self, _name: &str, byte_size: usize) -> TypeDescriptor {
            TypeDescriptor::Scalar { size: byte_size }
        }
    }

    #[derive(Debug)]
    struct DynamicBackend;

    impl CaptureBackendFactory for DynamicBackend {
        fn id(&self) -> &'static str {
            "dynamic-test"
        }
        fn supports(&self, _profile: crate::profile::ProfileId) -> bool {
            false
        }
        fn supports_profile(&self, profile: &str) -> bool {
            profile.eq_ignore_ascii_case("test-language")
        }
        fn compiler(
            &self,
            _profile: crate::profile::ProfileId,
        ) -> std::result::Result<Box<dyn CaptureCompiler>, RuntimeError> {
            Err(RuntimeError::MissingIdentity)
        }
        fn create_runtime(
            &self,
            _profile: crate::profile::ProfileId,
            _plan_hash: &str,
            _fields: Vec<ScalarFieldSpec>,
            _max_payload: usize,
        ) -> std::result::Result<crate::runtime::ProfiledCaptureRuntime, RuntimeError> {
            Err(RuntimeError::MissingIdentity)
        }
        fn create_adapter(
            &self,
            profile: &str,
            _binary_path: &Path,
            _base_path: &Path,
            _capture_config: &CaptureConfig,
        ) -> crate::error::Result<DapAdapterRef> {
            Err(crate::error::Error::Adapter(format!(
                "dynamic constructor invoked for {profile}"
            )))
        }
    }

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
    fn rust_profile_uses_profiled_adapter_without_reusing_go_factory() {
        let tmp = NamedTempFile::new().unwrap();
        let factory = EbpfAdapterFactory::new("/tmp");
        let result = factory.create_adapter(ProfileId::Rust, tmp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn string_profile_entry_point_preserves_builtin_adapters() {
        let tmp = NamedTempFile::new().unwrap();
        let factory = EbpfAdapterFactory::new("/tmp");
        assert!(factory.has_registered_profile("GO"));
        assert!(factory.create_registered_adapter("go", tmp.path()).is_ok());
        assert!(factory
            .create_registered_adapter("rust", tmp.path())
            .is_ok());
        assert!(factory
            .create_registered_adapter("not-registered", tmp.path())
            .is_err());
    }

    #[test]
    fn string_profile_entry_point_dispatches_dynamic_backend_constructor() {
        let tmp = NamedTempFile::new().unwrap();
        let mut factory = EbpfAdapterFactory::new("/tmp");
        factory.register_profile(Arc::new(DynamicProfile));
        factory.register_backend(Arc::new(DynamicBackend));
        let error = match factory.create_registered_adapter("TEST-LANGUAGE", tmp.path()) {
            Ok(_) => panic!("dynamic constructor should return its diagnostic"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("dynamic constructor invoked for test-language"));
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
    /// Create a composite factory with default capture limits.
    ///
    /// * `inner`     — base DAP factory (used for non-Go adapters)
    /// * `base_path` — base directory for resolving relative binary paths
    pub fn new(inner: DapAdapterFactoryRef, base_path: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            ebpf: EbpfAdapterFactory::new(base_path),
        }
    }

    /// Create a composite factory with capture limits from config.
    pub fn new_with_config(
        inner: DapAdapterFactoryRef,
        base_path: impl Into<PathBuf>,
        capture_config: CaptureConfig,
    ) -> Self {
        Self {
            inner,
            ebpf: EbpfAdapterFactory::new_with_config(base_path, capture_config),
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

    async fn create_go_adapter(&self, host: &str, port: u16) -> detrix_core::Result<DapAdapterRef> {
        // On Linux: if host is a binary path (starts with '/'), use eBPF uprobe.
        // Otherwise (IP address for Delve/DAP), fall through to inner factory.
        // On non-Linux: always delegate to inner factory.
        #[cfg(target_os = "linux")]
        {
            if host.starts_with('/') {
                let _ = port;
                self.ebpf
                    .create_go_adapter(host)
                    .map_err(|e| detrix_core::Error::Adapter(e.to_string()))
            } else {
                self.inner.create_go_adapter(host, port).await
            }
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
        #[cfg(target_os = "linux")]
        if host.starts_with('/') && program.is_none() && pid.is_none() {
            let _ = port;
            return self
                .ebpf
                .create_rust_adapter(host)
                .map_err(|e| detrix_core::Error::Adapter(e.to_string()));
        }
        self.inner
            .create_rust_adapter(host, port, program, pid)
            .await
    }
}

#[cfg(test)]
mod ebpf_go_factory_tests {
    use super::*;
    use detrix_core::Result;
    use detrix_ports::DapAdapterRef;

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
        match factory
            .create_rust_adapter("127.0.0.1", 1234, None, None)
            .await
        {
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
        let result = factory
            .create_go_adapter(tmp.path().to_str().unwrap(), 0)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn go_falls_back_to_dap_for_non_path_host_on_linux() {
        let factory = EbpfGoFactory::new(Arc::new(StubFactory), "/tmp");
        // host = "127.0.0.1" (not a path) should fall back to inner DAP factory
        let result = factory.create_go_adapter("127.0.0.1", 0).await;
        let err = match result {
            Ok(_) => panic!("expected Err"),
            Err(err) => err,
        };
        // StubFactory returns "stub go" — proves it fell through to inner factory
        assert!(err.to_string().contains("stub go"));
    }
}
