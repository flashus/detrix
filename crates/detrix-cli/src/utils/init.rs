//! Centralized composition root initialization
//!
//! This module contains shared infrastructure initialization logic used by:
//! - `serve` command (full daemon with HTTP/gRPC)
//! - `mcp` command direct mode
//!
//! Follows Clean Architecture - this is where all the infrastructure wiring happens.

use anyhow::{Context, Result};
use detrix_application::ports::{DlqRepositoryRef, EventOutputRef, RemoteAppControlRef};
use detrix_application::{
    middleware::ReconnectingAdapterFactory, AgentConnectionManager, AgentConnectionManagerRef,
    AppContext, ConnectionRepositoryRef, DapAdapterFactoryRef, EventRepositoryRef, FileSourceChain,
    FileSourceRef, MetricRepositoryRef,
};
use detrix_config::{
    AdapterConnectionConfig, AgentConfig, AnchorConfig, ApiConfig, Config, DaemonConfig,
    DlqBackend, LimitsConfig, SafetyConfig, StorageConfig, VfsConfig,
};
use detrix_dap::DapAdapterFactoryImpl;
use detrix_logging::{debug, info};
use detrix_storage::{DlqStorage, SqliteConfig, SqliteStorage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Initialized infrastructure components
///
/// Contains all infrastructure dependencies ready for AppContext creation.
pub struct InfrastructureComponents {
    /// SQLite storage (implements multiple repository traits)
    pub storage: Arc<SqliteStorage>,
    /// Separate DLQ storage (for true isolation from main storage failures)
    pub dlq_storage: Option<Arc<DlqStorage>>,
    /// Adapter factory for creating DAP adapters
    pub adapter_factory: DapAdapterFactoryRef,
    /// Base path for resolving relative file paths (for future use)
    #[allow(dead_code)]
    pub base_path: PathBuf,
}

/// Result of creating an AppContext - includes both context and storage reference
pub struct AppContextWithStorage {
    /// The application context
    pub app_context: AppContext,
    /// Storage reference (for API state creation)
    pub storage: Arc<SqliteStorage>,
    /// Bridge file source reference (for setting bridge URL from MCP headers)
    pub bridge_file_source: Arc<detrix_api::file_sources::BridgeSource>,
}

impl InfrastructureComponents {
    /// Create AppContext from these components, returning both context and storage
    ///
    /// # Arguments
    /// * `api_config` - API configuration
    /// * `safety_config` - Safety validation configuration
    /// * `storage_config` - Storage configuration
    /// * `daemon_config` - Daemon configuration (includes drain timeout)
    /// * `adapter_config` - Adapter connection configuration (timeouts, batching)
    /// * `anchor_config` - Anchor configuration for metric location tracking
    /// * `limits_config` - Limits configuration (max metrics, expression length)
    /// * `output` - Optional event output (GELF, etc.)
    #[allow(clippy::too_many_arguments)]
    pub fn into_app_context(
        self,
        api_config: &ApiConfig,
        safety_config: &SafetyConfig,
        storage_config: &StorageConfig,
        daemon_config: &DaemonConfig,
        adapter_config: &AdapterConnectionConfig,
        anchor_config: &AnchorConfig,
        limits_config: &LimitsConfig,
        vfs_config: &VfsConfig,
        output: Option<EventOutputRef>,
        agent_config: Option<AgentConfig>,
    ) -> AppContextWithStorage {
        // Convert DlqStorage to DlqRepositoryRef if available
        let dlq_repo: Option<DlqRepositoryRef> = self.dlq_storage.map(|s| s as DlqRepositoryRef);

        // Create HttpRemoteAppControl for wake/sleep proxy to remote apps
        let remote_control: Option<RemoteAppControlRef> =
            match detrix_api::remote_app_control::HttpRemoteAppControl::new(
                daemon_config.remote_app_timeout_ms,
            ) {
                Ok(ctrl) => Some(Arc::new(ctrl) as RemoteAppControlRef),
                Err(e) => {
                    detrix_logging::warn!("Failed to create remote app control: {}", e);
                    None
                }
            };

        // Create VFS: CachedFileSystem with disk fallback for production use
        let vfs: detrix_application::VfsRef = Arc::new(detrix_storage::CachedFileSystem::new(
            std::time::Duration::from_secs(vfs_config.hot_reload_ttl_seconds),
        ));

        // Build pluggable file source chain from VFS config
        let timeout = std::time::Duration::from_secs(vfs_config.fetch_timeout_seconds);
        let max_size = vfs_config.max_file_size_bytes;
        // Keep a concrete reference to BridgeSource for runtime URL updates from MCP headers
        let bridge_source = Arc::new(detrix_api::file_sources::BridgeSource::new(
            timeout, max_size,
        ));

        // Create AgentConnectionManager whenever agent support is available on the server.
        // Authentication remains optional: an empty token list means dev mode / allow-all.
        let agent_manager: Option<AgentConnectionManagerRef> = agent_config.as_ref().map(|cfg| {
            Arc::new(AgentConnectionManager::new(
                Arc::clone(&self.storage) as ConnectionRepositoryRef,
                Arc::clone(&self.storage) as detrix_application::MetricRepositoryRef,
                Some(cfg.clone()),
                None, // system_event_tx — not available yet; events still flow via dispatch
            ))
        });

        // Build available sources, adding AgentFileSource if agent mode is active
        let mut available_sources: Vec<FileSourceRef> = vec![
            Arc::new(detrix_api::file_sources::ControlPlaneSource::new(
                timeout,
                max_size,
                std::env::var("DETRIX_TOKEN").ok(),
            )),
            Arc::clone(&bridge_source) as FileSourceRef,
        ];
        if let Some(ref mgr) = agent_manager {
            available_sources.push(
                Arc::new(detrix_application::AgentFileSource::new(mgr.clone())) as FileSourceRef,
            );
            info!("AgentFileSource added to file source chain (highest priority)");
        }
        available_sources.push(Arc::new(detrix_api::file_sources::DiskSource) as FileSourceRef);

        let file_source_chain = Arc::new(FileSourceChain::new(
            Arc::clone(&vfs),
            available_sources,
            &vfs_config.source_priority,
        ));

        // Instantiate LSP purity analyzers for languages with lsp.enabled = true
        let mut purity_analyzers = HashMap::new();
        if safety_config.python.lsp.enabled {
            match detrix_lsp::PythonPurityAnalyzer::new(&safety_config.python.lsp) {
                Ok(a) => {
                    purity_analyzers.insert(
                        detrix_core::SourceLanguage::Python,
                        Arc::new(a) as detrix_application::PurityAnalyzerRef,
                    );
                }
                Err(e) => detrix_logging::warn!("Python LSP purity analyzer init failed: {e}"),
            }
        }
        if safety_config.go.lsp.enabled {
            match detrix_lsp::GoPurityAnalyzer::new(&safety_config.go.lsp) {
                Ok(a) => {
                    purity_analyzers.insert(
                        detrix_core::SourceLanguage::Go,
                        Arc::new(a) as detrix_application::PurityAnalyzerRef,
                    );
                }
                Err(e) => detrix_logging::warn!("Go LSP purity analyzer init failed: {e}"),
            }
        }
        if safety_config.rust.lsp.enabled {
            match detrix_lsp::RustPurityAnalyzer::new(&safety_config.rust.lsp) {
                Ok(a) => {
                    purity_analyzers.insert(
                        detrix_core::SourceLanguage::Rust,
                        Arc::new(a) as detrix_application::PurityAnalyzerRef,
                    );
                }
                Err(e) => detrix_logging::warn!("Rust LSP purity analyzer init failed: {e}"),
            }
        }

        let agent_manager_for_context = agent_manager.clone();
        let mut app_context = AppContext::new(
            Arc::clone(&self.storage) as MetricRepositoryRef,
            Arc::clone(&self.storage) as EventRepositoryRef,
            Arc::clone(&self.storage) as ConnectionRepositoryRef,
            self.adapter_factory,
            api_config,
            safety_config,
            storage_config,
            daemon_config,
            adapter_config,
            anchor_config,
            limits_config,
            output,
            dlq_repo,
            remote_control,
            std::env::var("DETRIX_TOKEN").ok(),
            vfs,
            file_source_chain,
            Arc::clone(&self.storage) as detrix_application::ConnectionReferenceRepositoryRef,
            purity_analyzers,
            agent_manager,
        );
        if let Some(mgr) = agent_manager_for_context {
            mgr.set_adapter_lifecycle_manager(app_context.adapter_lifecycle_manager.clone());
            app_context = app_context.with_agent_connection_manager(mgr);
        }
        AppContextWithStorage {
            app_context,
            storage: self.storage,
            bridge_file_source: bridge_source,
        }
    }

    /// Create AppContext with default configs
    #[allow(dead_code)]
    pub fn into_app_context_default(self) -> AppContextWithStorage {
        self.into_app_context(
            &ApiConfig::default(),
            &SafetyConfig::default(),
            &StorageConfig::default(),
            &DaemonConfig::default(),
            &AdapterConnectionConfig::default(),
            &AnchorConfig::default(),
            &LimitsConfig::default(),
            &VfsConfig::default(),
            None,
            None, // No agent config
        )
    }
}

/// Options for infrastructure initialization
pub struct InitOptions {
    /// Enable reconnection middleware for adapters
    pub enable_reconnection: bool,
    /// Reconnection config (only used if enable_reconnection is true)
    pub reconnect_config: Option<detrix_config::ReconnectConfig>,
}

impl Default for InitOptions {
    fn default() -> Self {
        Self {
            enable_reconnection: true,
            reconnect_config: None,
        }
    }
}

impl InitOptions {
    /// Create options for simple/testing use (no reconnection middleware)
    #[allow(dead_code)]
    pub fn simple() -> Self {
        Self {
            enable_reconnection: false,
            reconnect_config: None,
        }
    }

    /// Create options from config
    pub fn from_config(config: &Config) -> Self {
        Self {
            enable_reconnection: true,
            reconnect_config: Some(config.adapter.reconnect.clone()),
        }
    }
}

/// Initialize infrastructure components from config
///
/// This is the centralized entry point for all composition root initialization.
///
/// # Arguments
/// * `config` - Full Detrix configuration
/// * `config_dir` - Directory containing config file (for resolving relative paths)
/// * `options` - Initialization options
///
/// # Returns
/// Infrastructure components ready for AppContext creation
pub async fn init_infrastructure(
    config: &Config,
    config_dir: &Path,
    options: InitOptions,
) -> Result<InfrastructureComponents> {
    // Initialize SQLite storage
    let storage = init_storage(&config.storage, config_dir).await?;

    // Initialize separate DLQ storage (for true isolation from main storage)
    let dlq_storage = init_dlq_storage(&config.storage, config_dir).await?;

    // Determine base path for metric file resolution
    // Use config.project.base_path if set, otherwise use config_dir
    let base_path = detrix_config::resolve_path(config_dir, &config.project.base_path);
    info!(
        "Base path for adapters: {} (from config.project.base_path='{}', config_dir='{}')",
        base_path.display(),
        config.project.base_path,
        config_dir.display()
    );

    // Create adapter factory
    let adapter_factory = init_adapter_factory(&base_path, &options, config)?;

    Ok(InfrastructureComponents {
        storage,
        dlq_storage,
        adapter_factory,
        base_path,
    })
}

/// Initialize infrastructure with minimal config (for testing/binaries)
///
/// # Arguments
/// * `db_path` - Path to SQLite database
/// * `base_path` - Base path for file resolution
#[allow(dead_code)]
pub async fn init_infrastructure_minimal(
    db_path: PathBuf,
    base_path: PathBuf,
) -> Result<InfrastructureComponents> {
    let sqlite_config = SqliteConfig {
        path: db_path,
        pool_size: 30,
        busy_timeout_ms: 5000,
    };

    let storage = Arc::new(
        SqliteStorage::new(&sqlite_config)
            .await
            .context("Failed to initialize storage")?,
    );

    let adapter_factory: DapAdapterFactoryRef =
        Arc::new(DapAdapterFactoryImpl::new(base_path.clone()));

    Ok(InfrastructureComponents {
        storage,
        dlq_storage: None, // Minimal init doesn't use DLQ
        adapter_factory,
        base_path,
    })
}

/// Initialize SQLite storage from config
async fn init_storage(
    storage_config: &StorageConfig,
    config_dir: &Path,
) -> Result<Arc<SqliteStorage>> {
    let storage_path = detrix_config::resolve_path(config_dir, &storage_config.path);

    // Ensure parent directory exists (creates ~/detrix/ if needed)
    detrix_config::paths::ensure_parent_dir(&storage_path)
        .context("Failed to create storage directory")?;

    let sqlite_config = SqliteConfig {
        path: storage_path.clone(),
        pool_size: storage_config.pool_size,
        busy_timeout_ms: storage_config.busy_timeout_ms,
    };

    let storage = Arc::new(
        SqliteStorage::new(&sqlite_config)
            .await
            .context("Failed to initialize storage")?,
    );

    info!("Storage initialized at: {}", storage_path.display());
    Ok(storage)
}

/// Initialize separate DLQ storage from config
///
/// DLQ uses a separate database from main storage to provide true isolation.
/// If the main database has issues, DLQ can still capture failed events.
///
/// Returns None if DLQ is disabled in config.
async fn init_dlq_storage(
    storage_config: &StorageConfig,
    config_dir: &Path,
) -> Result<Option<Arc<DlqStorage>>> {
    // Check if DLQ is enabled
    if !storage_config.event_batching.dlq.enabled {
        info!("DLQ disabled in config");
        return Ok(None);
    }

    let dlq_config = &storage_config.dlq_storage;

    // Create parent directory for SQLite file backend
    if dlq_config.backend == DlqBackend::SqliteFile {
        let dlq_path = detrix_config::resolve_path(config_dir, &dlq_config.path);
        detrix_config::paths::ensure_parent_dir(&dlq_path)
            .context("Failed to create DLQ storage directory")?;
    }

    let dlq_storage = DlqStorage::from_config(dlq_config, config_dir)
        .await
        .context("Failed to initialize DLQ storage")?;

    info!(
        backend = ?dlq_config.backend,
        path = %dlq_config.path.display(),
        "DLQ storage initialized (separate database)"
    );

    Ok(Some(Arc::new(dlq_storage)))
}

/// Create adapter factory with optional reconnection middleware.
///
/// On Linux, Go connections are routed to eBPF uprobes via `EbpfGoFactory`.
/// The `host` field of a Go connection must be the path to the ELF binary.
/// All other languages continue to use DAP adapters regardless of platform.
fn init_adapter_factory(
    base_path: &Path,
    options: &InitOptions,
    config: &Config,
) -> Result<DapAdapterFactoryRef> {
    let dap_factory = Arc::new(DapAdapterFactoryImpl::new(base_path.to_path_buf()));

    // On Linux, wrap with EbpfGoFactory so Go connections use eBPF uprobes.
    // Pass capture config from [ebpf] section so max_capture_depth etc. are respected.
    let factory: DapAdapterFactoryRef = {
        #[cfg(target_os = "linux")]
        let dap_factory: DapAdapterFactoryRef =
            Arc::new(detrix_ebpf::EbpfGoFactory::new_with_config(
                dap_factory,
                base_path.to_path_buf(),
                detrix_ebpf::CaptureConfig::from(&config.ebpf),
            ));

        #[cfg(not(target_os = "linux"))]
        let dap_factory: DapAdapterFactoryRef = dap_factory;

        if options.enable_reconnection {
            let reconnect_config = options
                .reconnect_config
                .clone()
                .unwrap_or_else(|| config.adapter.reconnect.clone());

            Arc::new(ReconnectingAdapterFactory::new(
                dap_factory,
                reconnect_config,
            ))
        } else {
            dap_factory
        }
    };

    debug!(
        "Adapter factory initialized (reconnection: {}, ebpf: {})",
        options.enable_reconnection,
        cfg!(target_os = "linux"),
    );
    Ok(factory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_init_infrastructure_minimal() {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let base_path = temp_dir.path().to_path_buf();

        let components = init_infrastructure_minimal(db_path, base_path.clone())
            .await
            .unwrap();

        assert_eq!(components.base_path, base_path);
    }
}
