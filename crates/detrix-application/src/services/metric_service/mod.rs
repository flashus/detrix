//! Metric management use cases (protocol-agnostic)
//!
//! This module contains the MetricService for managing metrics, including:
//! - CRUD operations (add, remove, update, get, list)
//! - Toggle and group operations (enable/disable metrics and groups)
//! - Expression validation and safety checks
//! - File change handling and metric relocation

mod builder;
mod crud;
mod relocation;
mod toggle;
mod validation;

pub use builder::MetricServiceBuilder;
pub use relocation::FileChangeResult;

use crate::ports::{AnchorServiceRef, MetricRepositoryRef};
use crate::safety::ValidatorRegistry;
use crate::services::{AdapterLifecycleManager, FileInspectionService};
use crate::Result;
use detrix_config::{AdapterConnectionConfig, LimitsConfig};
use detrix_core::{ConnectionId, SystemEvent};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Metric management service (protocol-agnostic)
///
/// This service handles metric CRUD operations and coordinates with
/// the AdapterLifecycleManager to set/remove logpoints on the appropriate
/// connection-specific adapters.
#[derive(Clone)]
pub struct MetricService {
    pub(super) storage: MetricRepositoryRef,
    /// Lifecycle manager for getting connection-specific adapters
    pub(super) adapter_manager: Arc<AdapterLifecycleManager>,
    /// Safety validator registry for expression validation
    pub(super) validators: Arc<ValidatorRegistry>,
    /// Limits configuration
    pub(super) limits_config: LimitsConfig,
    /// Adapter configuration for batch operations
    pub(super) adapter_config: AdapterConnectionConfig,
    /// File inspection service for scope validation
    pub(super) file_inspection: FileInspectionService,
    /// System event broadcast channel for metric CRUD events
    pub(super) system_event_tx: broadcast::Sender<SystemEvent>,
    /// Anchor service for location tracking across code changes
    pub(super) anchor_service: AnchorServiceRef,
}

impl std::fmt::Debug for MetricService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricService")
            .field("storage", &"<MetricRepository>")
            .field("adapter_manager", &"<AdapterLifecycleManager>")
            .finish()
    }
}

impl MetricService {
    /// Create a new builder for MetricService
    ///
    /// # Example
    /// ```ignore
    /// let service = MetricService::builder(storage, adapter_manager, system_event_tx)
    ///     .validators(validators)
    ///     .limits_config(limits)
    ///     .build();
    /// ```
    pub fn builder(
        storage: MetricRepositoryRef,
        adapter_manager: Arc<AdapterLifecycleManager>,
        system_event_tx: broadcast::Sender<SystemEvent>,
    ) -> MetricServiceBuilder {
        MetricServiceBuilder::new(storage, adapter_manager, system_event_tx)
    }

    /// Get batch threshold from config
    pub(super) fn batch_threshold(&self) -> usize {
        self.adapter_config.batch_threshold
    }

    /// Get batch concurrency from config
    pub(super) fn batch_concurrency(&self) -> usize {
        self.adapter_config.batch_concurrency
    }

    /// Get the adapter for a specific connection
    ///
    /// Returns the adapter if it exists and is connected, otherwise returns an error.
    pub(super) async fn get_adapter_for_connection(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<crate::ports::DapAdapterRef> {
        self.adapter_manager
            .get_adapter(connection_id)
            .await
            .ok_or_else(|| {
                crate::Error::ConnectionNotFound(format!(
                    "No adapter found for connection '{}'",
                    connection_id.0
                ))
            })
    }
}
