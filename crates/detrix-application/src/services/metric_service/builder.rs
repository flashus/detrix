//! MetricServiceBuilder for constructing MetricService instances

use crate::ports::MetricRepositoryRef;
use crate::safety::ValidatorRegistry;
use crate::services::anchor_service::AnchorServiceConfig;
use crate::services::{AdapterLifecycleManager, DefaultAnchorService, FileInspectionService};
use crate::AnchorServiceRef;
use detrix_config::{AdapterConnectionConfig, AnchorConfig, LimitsConfig};
use detrix_core::SystemEvent;
use detrix_ports::VfsRef;
use std::sync::Arc;
use tokio::sync::broadcast;

use super::MetricService;

/// Create a default disk-only VFS (reads from std::fs, no caching).
///
/// Used when no VFS is explicitly configured (local daemon mode).
fn default_vfs() -> VfsRef {
    /// Minimal VFS that delegates to `std::fs`.
    struct DiskVfs;

    impl detrix_ports::VirtualFileSystem for DiskVfs {
        fn read_to_string(&self, path: &str) -> detrix_core::Result<String> {
            std::fs::read_to_string(path).map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    detrix_core::Error::FileNotFound(format!("File not found: {}", path))
                }
                _ => detrix_core::Error::Io(format!("Failed to read '{}': {}", path, e)),
            })
        }
        fn exists(&self, path: &str) -> detrix_core::Result<bool> {
            Ok(std::path::Path::new(path).exists())
        }
        fn store(&self, _: &str, _: &str, _: String) {}
        fn cached_hashes(&self, _: &str) -> Vec<(String, String)> {
            vec![]
        }
        fn validate_hashes(&self, _: &str, _: &[(String, String)]) -> Vec<String> {
            vec![]
        }
        fn mark_stale(&self, _: &str) {}
        fn clear_connection(&self, _: &str) {}
    }

    Arc::new(DiskVfs)
}

/// Builder for MetricService
///
/// # Example
/// ```ignore
/// let service = MetricService::builder(storage, adapter_manager, system_event_tx)
///     .validators(custom_validators)
///     .limits_config(limits)
///     .adapter_config(adapter_cfg)
///     .anchor_service(anchor_svc)
///     .vfs(vfs)
///     .build();
/// ```
pub struct MetricServiceBuilder {
    pub(super) storage: MetricRepositoryRef,
    pub(super) adapter_manager: Arc<AdapterLifecycleManager>,
    pub(super) system_event_tx: broadcast::Sender<SystemEvent>,
    pub(super) validators: Option<ValidatorRegistry>,
    pub(super) limits_config: LimitsConfig,
    pub(super) adapter_config: AdapterConnectionConfig,
    pub(super) anchor_service: Option<AnchorServiceRef>,
    pub(super) anchor_config: Option<AnchorServiceConfig>,
    pub(super) vfs: Option<VfsRef>,
}

impl MetricServiceBuilder {
    /// Create a new builder with required dependencies
    pub fn new(
        storage: MetricRepositoryRef,
        adapter_manager: Arc<AdapterLifecycleManager>,
        system_event_tx: broadcast::Sender<SystemEvent>,
    ) -> Self {
        Self {
            storage,
            adapter_manager,
            system_event_tx,
            validators: None,
            limits_config: LimitsConfig::default(),
            adapter_config: AdapterConnectionConfig::default(),
            anchor_service: None,
            anchor_config: None,
            vfs: None,
        }
    }

    /// Set custom validators
    ///
    /// If not set, defaults to ValidatorRegistry::default().
    pub fn validators(mut self, validators: ValidatorRegistry) -> Self {
        self.validators = Some(validators);
        self
    }

    /// Set limits configuration
    ///
    /// If not set, defaults to LimitsConfig::default().
    pub fn limits_config(mut self, config: LimitsConfig) -> Self {
        self.limits_config = config;
        self
    }

    /// Set adapter connection configuration
    ///
    /// If not set, defaults to AdapterConnectionConfig::default().
    pub fn adapter_config(mut self, config: AdapterConnectionConfig) -> Self {
        self.adapter_config = config;
        self
    }

    /// Set custom anchor service
    ///
    /// If not set, creates DefaultAnchorService using anchor_config (if set) or defaults.
    pub fn anchor_service(mut self, service: AnchorServiceRef) -> Self {
        self.anchor_service = Some(service);
        self
    }

    /// Set anchor configuration
    ///
    /// Used to create DefaultAnchorService if anchor_service is not explicitly set.
    /// This allows passing config values from TOML to the anchor service.
    pub fn anchor_config(mut self, config: &AnchorConfig) -> Self {
        self.anchor_config = Some(AnchorServiceConfig::from(config));
        self
    }

    /// Set the Virtual File System implementation.
    ///
    /// If not set, a default disk-only VFS is used (suitable for local daemon mode).
    pub fn vfs(mut self, vfs: VfsRef) -> Self {
        self.vfs = Some(vfs);
        self
    }

    /// Build the MetricService
    pub fn build(self) -> MetricService {
        // Create anchor service: use provided service, or create from config, or default
        let anchor_service = self.anchor_service.unwrap_or_else(|| {
            let config = self.anchor_config.unwrap_or_default();
            match &self.vfs {
                Some(vfs) => Arc::new(DefaultAnchorService::with_vfs(config, Arc::clone(vfs))),
                None => Arc::new(DefaultAnchorService::with_config(config)),
            }
        });

        // Use provided VFS or create a default disk-only VFS
        let vfs = self.vfs.unwrap_or_else(default_vfs);

        MetricService {
            storage: self.storage,
            adapter_manager: self.adapter_manager,
            validators: Arc::new(self.validators.unwrap_or_default()),
            limits_config: self.limits_config,
            adapter_config: self.adapter_config,
            file_inspection: FileInspectionService::new(vfs),
            system_event_tx: self.system_event_tx,
            anchor_service,
        }
    }
}
