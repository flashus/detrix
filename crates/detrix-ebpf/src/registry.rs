//! Registries are the extension seam for adding languages and backends.

use crate::adapter::EbpfAdapter;
use crate::compiler::{CaptureCompiler, GoBpfCompiler, RustBpfCompiler};
use crate::probe::types::CaptureConfig;
use crate::profile::{GoProfile, LanguageProfile, ProfileId, RustProfile};
use crate::runtime::{ProfiledCaptureRuntime, RuntimeError};
use crate::ScalarFieldSpec;
use detrix_ports::DapAdapterRef;
use std::collections::BTreeMap;
use std::path::Path;
use std::result::Result;
use std::sync::Arc;

pub trait CaptureBackendFactory: Send + Sync {
    fn id(&self) -> &'static str;
    /// Compatibility lookup for built-in profiles. External profiles should
    /// implement `supports_profile` instead and do not need a new enum value.
    fn supports(&self, _profile: ProfileId) -> bool {
        false
    }
    /// String-keyed capability seam for registry-provided profiles. Built-in
    /// backends retain the typed API, while an external backend can override
    /// this method without extending `ProfileId`.
    fn supports_profile(&self, profile: &str) -> bool {
        builtin_profile_id(profile).is_some_and(|id| self.supports(id))
    }
    fn compiler(&self, _profile: ProfileId) -> Result<Box<dyn CaptureCompiler>, RuntimeError> {
        Err(RuntimeError::MissingIdentity)
    }
    fn compiler_for_profile(
        &self,
        profile: &str,
    ) -> Result<Box<dyn CaptureCompiler>, RuntimeError> {
        builtin_profile_id(profile)
            .ok_or(RuntimeError::MissingIdentity)
            .and_then(|id| self.compiler(id))
    }
    fn create_runtime(
        &self,
        _profile: ProfileId,
        _plan_hash: &str,
        _fields: Vec<ScalarFieldSpec>,
        _max_payload: usize,
    ) -> Result<ProfiledCaptureRuntime, RuntimeError> {
        Err(RuntimeError::MissingIdentity)
    }

    /// Construct a runtime adapter for a registry profile key.
    ///
    /// Built-in Go/Rust construction remains available through
    /// `EbpfAdapterFactory`'s compatibility methods. External backends should
    /// override this hook to own their adapter, profile metadata, compiler,
    /// and lifecycle without adding a manager language branch. The default is
    /// deliberately fail-closed so registering a profile alone cannot claim
    /// runtime support.
    fn create_adapter(
        &self,
        _profile: &str,
        _binary_path: &Path,
        _base_path: &Path,
        _capture_config: &CaptureConfig,
    ) -> crate::error::Result<DapAdapterRef> {
        Err(crate::error::Error::Adapter(
            "backend has no dynamic adapter constructor".into(),
        ))
    }

    /// Construct an adapter with the registry-owned profile object.  This is
    /// the primary string-keyed runtime seam; the legacy `create_adapter`
    /// hook remains for backends that own a completely different adapter.
    fn create_adapter_with_profile(
        &self,
        profile_name: &str,
        profile: Arc<dyn LanguageProfile>,
        binary_path: &Path,
        base_path: &Path,
        capture_config: &CaptureConfig,
    ) -> crate::error::Result<DapAdapterRef> {
        let _ = (
            profile_name,
            profile,
            binary_path,
            base_path,
            capture_config,
        );
        self.create_adapter(profile_name, binary_path, base_path, capture_config)
    }

    /// Variant carrying an optional external DWARF image.  The default keeps
    /// third-party backends source-compatible while built-in uprobe backends
    /// can preserve their debug-image selection transactionally.
    fn create_adapter_with_profile_and_debug_path(
        &self,
        profile_name: &str,
        profile: Arc<dyn LanguageProfile>,
        binary_path: &Path,
        base_path: &Path,
        capture_config: &CaptureConfig,
        debug_path: Option<&Path>,
    ) -> crate::error::Result<DapAdapterRef> {
        let _ = debug_path;
        self.create_adapter_with_profile(
            profile_name,
            profile,
            binary_path,
            base_path,
            capture_config,
        )
    }
}

/// Shared uprobe backend. Language-specific lowering is selected by profile;
/// attachment, lifecycle, and transport remain backend-owned.
#[derive(Debug, Default, Clone, Copy)]
pub struct GoEbpfBackend;

impl CaptureBackendFactory for GoEbpfBackend {
    fn id(&self) -> &'static str {
        "ebpf"
    }
    fn supports(&self, profile: ProfileId) -> bool {
        profile == ProfileId::Go
    }
    fn compiler(&self, profile: ProfileId) -> Result<Box<dyn CaptureCompiler>, RuntimeError> {
        match profile {
            ProfileId::Go => Ok(Box::new(GoBpfCompiler::default())),
            ProfileId::Rust => Err(RuntimeError::MissingIdentity),
        }
    }
    fn create_runtime(
        &self,
        profile: ProfileId,
        plan_hash: &str,
        fields: Vec<ScalarFieldSpec>,
        max_payload: usize,
    ) -> Result<ProfiledCaptureRuntime, RuntimeError> {
        if !self.supports(profile) {
            return Err(RuntimeError::MissingIdentity);
        }
        ProfiledCaptureRuntime::new("go", plan_hash, fields, max_payload)
    }

    fn create_adapter_with_profile(
        &self,
        profile_name: &str,
        profile: Arc<dyn LanguageProfile>,
        binary_path: &Path,
        _base_path: &Path,
        capture_config: &CaptureConfig,
    ) -> crate::error::Result<DapAdapterRef> {
        if !profile_name.eq_ignore_ascii_case("go") {
            return Err(crate::error::Error::Adapter(format!(
                "ebpf backend does not support profile {profile_name}"
            )));
        }
        let adapter = EbpfAdapter::new_with_profile_object_and_debug_path(
            binary_path,
            capture_config.clone(),
            ProfileId::Go,
            profile,
            None::<&Path>,
        )?;
        Ok(Arc::new(adapter) as DapAdapterRef)
    }

    fn create_adapter_with_profile_and_debug_path(
        &self,
        profile_name: &str,
        profile: Arc<dyn LanguageProfile>,
        binary_path: &Path,
        _base_path: &Path,
        capture_config: &CaptureConfig,
        debug_path: Option<&Path>,
    ) -> crate::error::Result<DapAdapterRef> {
        if !profile_name.eq_ignore_ascii_case("go") {
            return Err(crate::error::Error::Adapter(format!(
                "ebpf backend does not support profile {profile_name}"
            )));
        }
        let adapter = EbpfAdapter::new_with_profile_object_and_debug_path(
            binary_path,
            capture_config.clone(),
            ProfileId::Go,
            profile,
            debug_path,
        )?;
        Ok(Arc::new(adapter) as DapAdapterRef)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RustEbpfBackend;

impl CaptureBackendFactory for RustEbpfBackend {
    fn id(&self) -> &'static str {
        "ebpf-rust"
    }
    fn supports(&self, profile: ProfileId) -> bool {
        profile == ProfileId::Rust
    }
    fn compiler(&self, profile: ProfileId) -> Result<Box<dyn CaptureCompiler>, RuntimeError> {
        if profile != ProfileId::Rust {
            return Err(RuntimeError::MissingIdentity);
        }
        Ok(Box::new(RustBpfCompiler::default()))
    }
    fn create_runtime(
        &self,
        profile: ProfileId,
        plan_hash: &str,
        fields: Vec<ScalarFieldSpec>,
        max_payload: usize,
    ) -> Result<ProfiledCaptureRuntime, RuntimeError> {
        if profile != ProfileId::Rust {
            return Err(RuntimeError::MissingIdentity);
        }
        ProfiledCaptureRuntime::new("rust", plan_hash, fields, max_payload)
    }

    fn create_adapter_with_profile(
        &self,
        profile_name: &str,
        profile: Arc<dyn LanguageProfile>,
        binary_path: &Path,
        _base_path: &Path,
        capture_config: &CaptureConfig,
    ) -> crate::error::Result<DapAdapterRef> {
        if !profile_name.eq_ignore_ascii_case("rust") {
            return Err(crate::error::Error::Adapter(format!(
                "ebpf-rust backend does not support profile {profile_name}"
            )));
        }
        let adapter = EbpfAdapter::new_with_profile_object_and_debug_path(
            binary_path,
            capture_config.clone(),
            ProfileId::Rust,
            profile,
            None::<&Path>,
        )?;
        Ok(Arc::new(adapter) as DapAdapterRef)
    }

    fn create_adapter_with_profile_and_debug_path(
        &self,
        profile_name: &str,
        profile: Arc<dyn LanguageProfile>,
        binary_path: &Path,
        _base_path: &Path,
        capture_config: &CaptureConfig,
        debug_path: Option<&Path>,
    ) -> crate::error::Result<DapAdapterRef> {
        if !profile_name.eq_ignore_ascii_case("rust") {
            return Err(crate::error::Error::Adapter(format!(
                "ebpf-rust backend does not support profile {profile_name}"
            )));
        }
        let adapter = EbpfAdapter::new_with_profile_object_and_debug_path(
            binary_path,
            capture_config.clone(),
            ProfileId::Rust,
            profile,
            debug_path,
        )?;
        Ok(Arc::new(adapter) as DapAdapterRef)
    }
}

#[derive(Default)]
pub struct ProfileRegistry {
    profiles: BTreeMap<String, Arc<dyn LanguageProfile>>,
}

impl ProfileRegistry {
    pub fn with_defaults() -> Self {
        let mut registry = Self::default();
        registry.register(Arc::new(GoProfile));
        registry.register(Arc::new(RustProfile));
        registry
    }
    pub fn register(&mut self, profile: Arc<dyn LanguageProfile>) {
        self.profiles
            .insert(profile.id().to_ascii_lowercase(), profile);
    }
    pub fn get(&self, id: &str) -> Option<Arc<dyn LanguageProfile>> {
        self.profiles.get(&id.to_lowercase()).cloned()
    }
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.profiles.keys().map(String::as_str)
    }
}

#[derive(Default)]
pub struct BackendRegistry {
    backends: BTreeMap<String, Arc<dyn CaptureBackendFactory>>,
}

impl BackendRegistry {
    pub fn with_defaults() -> Self {
        let mut registry = Self::default();
        registry.register(Arc::new(GoEbpfBackend));
        registry.register(Arc::new(RustEbpfBackend));
        registry
    }
    pub fn register(&mut self, backend: Arc<dyn CaptureBackendFactory>) {
        self.backends.insert(backend.id().into(), backend);
    }
    pub fn get(&self, id: &str) -> Option<Arc<dyn CaptureBackendFactory>> {
        self.backends.get(&id.to_lowercase()).cloned()
    }

    /// Resolve the backend owned by a language profile.  Keeping this mapping
    /// in the registry makes backend selection an extension seam: adding a
    /// profile does not require changing adapter construction code.
    pub fn for_profile(&self, profile: ProfileId) -> Option<Arc<dyn CaptureBackendFactory>> {
        // Backend ownership is capability-driven. A newly registered profile
        // only needs a backend that advertises support; construction does not
        // require another language match here.
        self.backends
            .values()
            .find(|b| b.supports(profile))
            .cloned()
    }

    /// Resolve a backend for a registry profile key. This is intentionally
    /// string-based so adding a profile can be a registration change rather
    /// than a cross-cutting enum change.
    pub fn for_profile_name(&self, profile: &str) -> Option<Arc<dyn CaptureBackendFactory>> {
        self.backends
            .values()
            .find(|backend| backend.supports_profile(profile))
            .cloned()
    }
}

fn builtin_profile_id(profile: &str) -> Option<ProfileId> {
    match profile.trim().to_ascii_lowercase().as_str() {
        "go" => Some(ProfileId::Go),
        "rust" => Some(ProfileId::Rust),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwarf::types::TargetArchitecture;

    #[derive(Debug)]
    struct TestProfile;

    impl LanguageProfile for TestProfile {
        fn id(&self) -> &'static str {
            "test-language"
        }

        fn architectures(&self) -> &'static [TargetArchitecture] {
            &[]
        }

        fn capabilities(&self) -> crate::profile::ProfileCapabilities {
            crate::profile::ProfileCapabilities {
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

        fn classify_type(&self, _name: &str, byte_size: usize) -> crate::profile::TypeDescriptor {
            crate::profile::TypeDescriptor::Scalar { size: byte_size }
        }
    }

    #[test]
    fn default_profiles_are_plugins() {
        let r = ProfileRegistry::with_defaults();
        assert!(r.get("go").is_some());
        assert!(r.get("rust").is_some());
    }

    #[test]
    fn registry_accepts_profile_without_builtin_enum_change() {
        let mut r = ProfileRegistry::with_defaults();
        r.register(Arc::new(TestProfile));
        assert!(r.get("TEST-LANGUAGE").is_some());
        assert!(r.ids().any(|id| id == "test-language"));
    }

    #[test]
    fn backend_registry_exposes_string_profile_lookup() {
        let r = BackendRegistry::with_defaults();
        assert_eq!(r.for_profile_name("go").map(|b| b.id()), Some("ebpf"));
        assert_eq!(
            r.for_profile_name("rust").map(|b| b.id()),
            Some("ebpf-rust")
        );
        assert!(r.for_profile_name("unregistered").is_none());
    }
    #[test]
    fn backend_capabilities_are_profile_scoped() {
        let r = BackendRegistry::with_defaults();
        let b = r.get("ebpf").unwrap();
        assert!(b.supports(ProfileId::Go));
        assert!(!b.supports(ProfileId::Rust));
        assert!(!b
            .compiler(ProfileId::Go)
            .unwrap()
            .compile(
                &crate::profile::GoProfile
                    .scalar_plan("n", 1, crate::dwarf::types::Register::Rax,)
                    .unwrap()
            )
            .unwrap()
            .artifact
            .is_empty());
        let rust = r.for_profile(ProfileId::Rust).unwrap();
        assert!(rust
            .compiler(ProfileId::Rust)
            .unwrap()
            .compile(
                &crate::profile::RustProfile
                    .scalar_plan("n", 1, crate::dwarf::types::Register::Rax)
                    .unwrap()
            )
            .is_ok());
        assert!(matches!(
            rust.create_runtime(ProfileId::Rust, "sha256:rust", vec![], 64),
            Err(RuntimeError::EmptyPlan)
        ));
    }
}
