//! Registries are the extension seam for adding languages and backends.

use crate::compiler::{CaptureCompiler, GoBpfCompiler, RustBpfCompiler};
use crate::profile::{GoProfile, LanguageProfile, ProfileId, RustProfile};
use crate::runtime::{ProfiledCaptureRuntime, RuntimeError};
use crate::ScalarFieldSpec;
use std::collections::BTreeMap;
use std::sync::Arc;

pub trait CaptureBackendFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn supports(&self, profile: ProfileId) -> bool;
    fn compiler(&self) -> Box<dyn CaptureCompiler>;
    fn create_runtime(
        &self,
        profile: ProfileId,
        plan_hash: &str,
        fields: Vec<ScalarFieldSpec>,
        max_payload: usize,
    ) -> Result<ProfiledCaptureRuntime, RuntimeError>;
}

/// Capability-only registration for the current uprobe backend. Actual
/// adapter construction remains in `EbpfAdapterFactory` until the runtime is
/// fully split from the legacy Go decoder.
#[derive(Debug, Default, Clone, Copy)]
pub struct GoEbpfBackend;

impl CaptureBackendFactory for GoEbpfBackend {
    fn id(&self) -> &'static str {
        "ebpf"
    }
    fn supports(&self, profile: ProfileId) -> bool {
        profile == ProfileId::Go
    }
    fn compiler(&self) -> Box<dyn CaptureCompiler> {
        Box::new(GoBpfCompiler::default())
    }
    fn create_runtime(
        &self,
        profile: ProfileId,
        plan_hash: &str,
        fields: Vec<ScalarFieldSpec>,
        max_payload: usize,
    ) -> Result<ProfiledCaptureRuntime, RuntimeError> {
        if profile != ProfileId::Go {
            return Err(RuntimeError::MissingIdentity);
        }
        ProfiledCaptureRuntime::new("go", plan_hash, fields, max_payload)
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
    fn compiler(&self) -> Box<dyn CaptureCompiler> {
        Box::new(RustBpfCompiler::default())
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
        self.profiles.insert(profile.id().as_str().into(), profile);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_profiles_are_plugins() {
        let r = ProfileRegistry::with_defaults();
        assert!(r.get("go").is_some());
        assert!(r.get("rust").is_some());
    }
    #[test]
    fn backend_capabilities_are_profile_scoped() {
        let r = BackendRegistry::with_defaults();
        let b = r.get("ebpf").unwrap();
        assert!(b.supports(ProfileId::Go));
        assert!(!b.supports(ProfileId::Rust));
        assert!(!b
            .compiler()
            .compile(
                &crate::profile::GoProfile
                    .scalar_plan("n", 1, crate::dwarf::types::Register::Rax,)
                    .unwrap()
            )
            .unwrap()
            .artifact
            .is_empty());
        let rust = r.get("ebpf-rust").unwrap();
        assert!(rust.supports(ProfileId::Rust));
        assert!(!rust.supports(ProfileId::Go));
        assert!(matches!(
            rust.create_runtime(ProfileId::Rust, "sha256:rust", vec![], 64),
            Err(RuntimeError::EmptyPlan)
        ));
    }
}
