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
use crate::scope::MetricScope;
use crate::services::{AdapterLifecycleManager, FileInspectionService};
use crate::Result;
use detrix_config::{AdapterConnectionConfig, LimitsConfig};
use detrix_core::{ConnectionId, Metric, SystemEvent};
use std::collections::BTreeSet;
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

    /// Sync the DAP logpoint at a given location after a metric change.
    ///
    /// In multi-tenant mode, multiple users can have metrics at the same location.
    /// DAP only supports one logpoint per (file, line, connection_id), so we merge
    /// all enabled metrics' expressions into a single logpoint.
    ///
    /// - If enabled metrics remain: set logpoint with unioned expressions (single adapter call)
    /// - If no enabled metrics: remove the logpoint using `fallback_metric` as reference
    ///
    /// `fallback_metric` should be the metric that was just deleted/disabled — it is used
    /// as the location reference for `adapter.remove_metric()` when no storage metric can
    /// serve that role (e.g. after `storage.delete()`).
    pub(super) async fn sync_logpoint(
        &self,
        connection_id: &ConnectionId,
        file: &str,
        line: u32,
        fallback_metric: Option<&Metric>,
    ) -> Result<()> {
        let all_metrics = self
            .storage
            .find_all_at_location(connection_id, file, line)
            .await?;

        let enabled: Vec<&Metric> = all_metrics.iter().filter(|m| m.enabled).collect();

        // Union + deduplicate expressions (BTreeSet preserves sorted order)
        let merged_exprs: Vec<String> = enabled
            .iter()
            .flat_map(|m| m.expressions.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let adapter = match self.get_adapter_for_connection(connection_id).await {
            Ok(a) => a,
            Err(_) => {
                // No adapter connected — logpoint will be synced when adapter reconnects
                return Ok(());
            }
        };

        if merged_exprs.is_empty() {
            // No enabled metrics remain — remove the DAP logpoint.
            // Prefer fallback_metric (the just-deleted/disabled metric) since it may no
            // longer be in storage; fall back to any remaining storage metric as reference.
            let reference = fallback_metric.or_else(|| all_metrics.first());
            if let Some(m) = reference {
                if let Err(e) = adapter.remove_metric(m).await {
                    tracing::warn!(
                        file = %file,
                        line = line,
                        error = %e,
                        "Failed to remove logpoint during sync"
                    );
                }
            }
        } else {
            // Other users' metrics still enabled at this location — update the merged logpoint.
            // This is a single adapter call: no remove+re-add gap, logpoint captures continuously.
            let template = enabled[0];
            let mut merged = template.clone();
            merged.expressions = merged_exprs;

            if let Err(e) = adapter.set_metric(&merged).await {
                tracing::warn!(
                    file = %file,
                    line = line,
                    error = %e,
                    "Failed to set merged logpoint during sync"
                );
            }
        }

        Ok(())
    }

    /// Check that the given scope allows mutation of the given metric.
    ///
    /// Returns `Err(AccessDenied)` if the scope does not permit the operation.
    pub(super) fn check_mutate_access(scope: &MetricScope, metric: &Metric) -> Result<()> {
        if !scope.can_mutate(metric) {
            return Err(crate::Error::AccessDenied(format!(
                "Cannot mutate metric '{}' (id={:?})",
                metric.name,
                metric.id.map(|id| id.0)
            )));
        }
        Ok(())
    }
}
