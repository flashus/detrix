//! CRUD operations for MetricService

use crate::error::{OperationOutcome, OperationWarning};
use crate::ports::MetricFilter;
use crate::scope::MetricScope;
use crate::{Error, Result};
use detrix_core::entities::AnchorStatus;
use detrix_core::{ConnectionId, Metric, MetricId};

use super::MetricService;

impl MetricService {
    /// Import a metric to storage only (without setting logpoint).
    ///
    /// Use this when loading metrics from config before the adapter is connected.
    /// Call `sync_metrics_for_connection()` after adapter connects to set logpoints.
    ///
    /// Validates metric fields (name, expressions, count/length limits, AST safety)
    /// before saving. Skips `validate_safe_mode()` (no active connection at import time)
    /// and `validate_expression_scope()` (LSP/file inspection may not be available).
    pub async fn import_metric(&self, metric: Metric) -> Result<MetricId> {
        self.validate_metric_fields(&metric)?;
        let metric_id = self.storage.save(&metric).await?;
        Ok(metric_id)
    }

    /// Sync all enabled metrics for a specific connection to its adapter.
    ///
    /// Call this after the adapter connects to set logpoints for all enabled metrics
    /// that belong to this connection.
    /// Returns the count of successfully synced metrics.
    ///
    /// Uses batch operations when supported by the adapter for better performance.
    /// Falls back to sequential calls for adapters that don't support batching.
    pub async fn sync_metrics_for_connection(&self, connection_id: &ConnectionId) -> Result<usize> {
        let adapter = self.get_adapter_for_connection(connection_id).await?;
        let metrics = self.storage.find_by_connection_id(connection_id).await?;

        // Filter only enabled metrics
        let enabled_metrics: Vec<_> = metrics.into_iter().filter(|m| m.enabled).collect();

        if enabled_metrics.is_empty() {
            return Ok(0);
        }

        // Use batch operation for efficiency
        let batch_result = adapter
            .set_metrics_batch(
                &enabled_metrics,
                self.batch_threshold(),
                self.batch_concurrency(),
            )
            .await;

        // Log any failures
        for failed in &batch_result.failed {
            tracing::error!(
                "Failed to sync metric '{}' (id={:?}) to adapter: {}",
                failed.name,
                failed.id,
                failed.error
            );
        }

        Ok(batch_result.success_count())
    }

    /// Unsync all metrics for a specific connection from its adapter.
    ///
    /// Call this when disconnecting to remove all logpoints from the adapter.
    /// Returns the count of successfully unsynced metrics.
    ///
    /// Uses batch operations when supported by the adapter for better performance.
    pub async fn unsync_metrics_for_connection(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<usize> {
        let adapter = self.get_adapter_for_connection(connection_id).await?;
        let metrics = self.storage.find_by_connection_id(connection_id).await?;

        if metrics.is_empty() {
            return Ok(0);
        }

        // Use batch operation for efficiency
        let batch_result = adapter
            .remove_metrics_batch(&metrics, self.batch_threshold(), self.batch_concurrency())
            .await;

        // Log any failures
        for failed in &batch_result.failed {
            tracing::error!(
                "Failed to unsync metric '{}' (id={:?}) from adapter: {}",
                failed.name,
                failed.id,
                failed.error
            );
        }

        Ok(batch_result.succeeded)
    }

    /// Add a new metric
    ///
    /// If the metric is enabled, it will also be set in the DAP adapter for its connection.
    /// If adapter fails, the metric is rolled back from storage.
    ///
    /// # Arguments
    /// * `metric` - The metric to add
    /// * `replace` - If true, replace any existing metric at the same location.
    ///   If false (default), return error when metric already exists at this location.
    ///   DAP only supports one logpoint per line.
    ///
    /// Returns `OperationOutcome<MetricId>` containing the metric ID and any warnings.
    // This function orchestrates multiple ordered phases (validate, store, set in adapter, restore on
    // failure, emit event) that naturally result in high complexity and line count.
    #[allow(clippy::cognitive_complexity)]
    #[allow(clippy::too_many_lines)]
    pub async fn add_metric(
        &self,
        metric: Metric,
        replace: bool,
        file_content: Option<String>,
    ) -> Result<OperationOutcome<MetricId>> {
        // If file content is provided, store it in VFS so downstream
        // validation (scope check, anchor capture) can read the file.
        if let Some(content) = file_content {
            self.file_inspection.vfs().store(
                &metric.connection_id.0,
                &metric.location.file,
                content,
            );
        }

        // Validate SafeMode constraints (stack trace/memory snapshot in SafeMode)
        self.validate_safe_mode(&metric)?;

        // Validate metric (may produce warnings)
        let mut warnings = self.validate_metric(&metric)?;

        // Check for existing metric at the same location (file:line)
        // DAP only supports one logpoint per line
        if let Some(existing) = self
            .storage
            .find_by_location(
                &metric.connection_id,
                &metric.location.file,
                metric.location.line,
                metric.user_id.as_deref(),
            )
            .await?
        {
            if replace {
                // Replace mode: remove existing metric first
                // Existing metrics from storage always have IDs
                let existing_id = existing.id.ok_or_else(|| {
                    Error::Storage("Stored metric missing ID - database integrity issue".into())
                })?;
                let existing_name = existing.name.clone();

                // Remove from adapter if it was enabled
                if existing.enabled {
                    if let Ok(adapter) =
                        self.get_adapter_for_connection(&metric.connection_id).await
                    {
                        if let Err(e) = adapter.remove_metric(&existing).await {
                            tracing::warn!(
                                metric = %existing.name,
                                error = %e,
                                "Failed to remove logpoint from adapter during metric replacement"
                            );
                        }
                    }
                }

                // Delete from storage
                self.storage.delete(existing_id).await?;

                // Add warning about replacement
                warnings.push(crate::OperationWarning::MetricReplaced {
                    replaced_name: existing_name,
                    replaced_id: existing_id.0,
                    location: format!("{}:{}", metric.location.file, metric.location.line),
                });
            } else {
                // No replace flag: merge any new expressions into the existing metric.
                // This handles the case where a second add_metric call targets the same
                // line (e.g. via find_logpoint) with an additional expression. The DAP
                // logpoint is updated in-place so both expressions are captured together.
                // If no new expressions, this is fully idempotent (daemon-restart safe).
                let existing_id = existing.id.ok_or_else(|| {
                    Error::Storage(format!(
                        "Existing metric '{}' at {}:{} missing ID - database integrity issue",
                        existing.name, metric.location.file, metric.location.line
                    ))
                })?;

                let new_exprs: Vec<String> = metric
                    .expressions
                    .iter()
                    .filter(|e| !existing.expressions.contains(*e))
                    .cloned()
                    .collect();

                if !new_exprs.is_empty() {
                    let mut merged = existing.clone();
                    merged.expressions.extend(new_exprs.clone());

                    // Persist the merged expressions
                    self.storage.update(&merged).await?;

                    // Re-register the logpoint with the adapter so it captures all expressions
                    if merged.enabled {
                        if let Ok(adapter) =
                            self.get_adapter_for_connection(&metric.connection_id).await
                        {
                            if let Err(e) = adapter.remove_metric(&existing).await {
                                tracing::warn!(
                                    metric = %existing.name,
                                    error = %e,
                                    "Failed to remove old logpoint from adapter during expression merge"
                                );
                            }
                            if let Err(e) = adapter.set_metric(&merged).await {
                                tracing::warn!(
                                    metric = %merged.name,
                                    error = %e,
                                    "Failed to re-set logpoint in adapter after expression merge"
                                );
                            }
                        }
                    }

                    tracing::info!(
                        "Merged {} new expression(s) {:?} into metric '{}' at {}:{} for connection {}",
                        new_exprs.len(),
                        new_exprs,
                        existing.name,
                        metric.location.file,
                        metric.location.line,
                        metric.connection_id.0,
                    );
                    warnings.push(OperationWarning::ExpressionsMerged {
                        metric_name: existing.name.clone(),
                        metric_id: existing_id.0,
                        added_expressions: new_exprs,
                        location: format!("{}:{}", metric.location.file, metric.location.line),
                    });
                } else {
                    tracing::info!(
                        "Metric already exists at {}:{} for connection {}, returning existing ID {} (name='{}')",
                        metric.location.file,
                        metric.location.line,
                        metric.connection_id.0,
                        existing_id.0,
                        existing.name
                    );
                }
                return Ok(OperationOutcome::with_warnings(existing_id, warnings));
            }
        }

        // Capture anchor data for location tracking
        let mut metric_with_anchor = metric.clone();
        match self
            .anchor_service
            .capture_anchor(&metric.location, metric.language)
            .await
        {
            Ok(anchor) => {
                tracing::debug!(
                    "add_metric: Captured anchor for metric '{}' at {}:{}",
                    metric.name,
                    metric.location.file,
                    metric.location.line
                );
                metric_with_anchor.anchor = Some(anchor);
                metric_with_anchor.anchor_status = AnchorStatus::Verified;
            }
            Err(e) => {
                // Anchor capture failed - add warning but continue (metric is still valid)
                tracing::warn!(
                    "add_metric: Failed to capture anchor for metric '{}': {}",
                    metric.name,
                    e
                );
                warnings.push(crate::OperationWarning::AnchorCaptureFailed {
                    metric_name: metric.name.clone(),
                    location: format!("{}:{}", metric.location.file, metric.location.line),
                    error: e.to_string(),
                });
                metric_with_anchor.anchor_status = AnchorStatus::Unanchored;
            }
        }

        // Save to storage (with anchor data)
        let metric_id = self.storage.save(&metric_with_anchor).await?;
        tracing::debug!(
            "add_metric: Saved metric to storage: id={}, name={}, file={}:{}",
            metric_id.0,
            metric_with_anchor.name,
            metric_with_anchor.location.file,
            metric_with_anchor.location.line
        );

        // Set via DAP adapter if enabled (uses connection-specific adapter)
        // Important: Update metric with ID so adapter stores it correctly
        if metric_with_anchor.enabled {
            let mut metric_with_id = metric_with_anchor.clone();
            metric_with_id.id = Some(metric_id);

            // Get the adapter for this metric's connection
            let adapter = self
                .get_adapter_for_connection(&metric_with_anchor.connection_id)
                .await?;

            match adapter.set_metric(&metric_with_id).await {
                Ok(result) => {
                    // Check if breakpoint was verified
                    if !result.verified {
                        // Breakpoint not verified - add warning with DAP message if available
                        warnings.push(crate::OperationWarning::BreakpointNotVerified {
                            location: metric_with_anchor.location.file.clone(),
                            line: result.line,
                            message: result.message,
                        });
                    }
                }
                Err(e) => {
                    // Rollback: delete from storage if adapter fails
                    if let Err(rollback_err) = self.storage.delete(metric_id).await {
                        // Return compound error with both primary and rollback failure
                        return Err(crate::Error::OperationWithRollbackFailure {
                            primary: e.to_string(),
                            rollback: rollback_err.to_string(),
                            context: format!("metric_id={}", metric_id),
                        });
                    }
                    return Err(e.into());
                }
            }
        }

        // Emit metric added event
        let event = detrix_core::SystemEvent::metric_added(
            metric_id.0,
            metric_with_anchor.name.clone(),
            metric_with_anchor.connection_id.clone(),
        );
        if let Err(e) = self.system_event_tx.send(event) {
            tracing::warn!("No subscribers for metric_added event: {e}");
        }

        Ok(OperationOutcome::with_warnings(metric_id, warnings))
    }

    /// Remove a metric by ID
    ///
    /// Checks scope permission before deletion.
    /// After removal, syncs the DAP logpoint at the location (other users may still have metrics there).
    pub async fn remove_metric(&self, metric_id: MetricId, scope: &MetricScope) -> Result<()> {
        // Get metric before deleting (needed for adapter call + scope check)
        let metric =
            self.storage.find_by_id(metric_id).await?.ok_or_else(|| {
                detrix_core::Error::MetricNotFound(format!("Metric {}", metric_id))
            })?;

        // Scope check: caller must be allowed to mutate this metric
        Self::check_mutate_access(scope, &metric)?;

        let connection_id = metric.connection_id.clone();
        let file = metric.location.file.clone();
        let line = metric.location.line;

        // Delete from storage first so find_all_at_location returns only remaining users.
        self.storage.delete(metric_id).await?;

        // Sync the DAP logpoint. Pass the just-deleted metric as fallback so that
        // sync_logpoint can call adapter.remove_metric() when no remaining metrics exist
        // (the deleted metric is no longer in storage but is a valid location reference).
        //
        // Multi-tenant (other user B has metric at same location):
        //   sync_logpoint finds B → adapter.set_metric(merged_B) — single call, no gap
        //
        // Single-tenant (deleted metric was the only one):
        //   sync_logpoint finds nothing → adapter.remove_metric(fallback) — single call
        self.sync_logpoint(&connection_id, &file, line, Some(&metric))
            .await?;

        // Emit metric removed event
        let event = detrix_core::SystemEvent::metric_removed(
            metric_id.0,
            metric.name.clone(),
            connection_id,
        );
        if let Err(e) = self.system_event_tx.send(event) {
            tracing::warn!("No subscribers for metric_removed event: {e}");
        }

        Ok(())
    }

    /// Update an existing metric
    ///
    /// Checks scope permission before update.
    /// Returns `OperationOutcome<()>` containing any warnings encountered.
    /// Warnings are generated when non-critical operations fail (e.g., fetching
    /// old metric for comparison).
    pub async fn update_metric(
        &self,
        metric: &Metric,
        scope: &MetricScope,
    ) -> Result<OperationOutcome<()>> {
        let mut warnings = Vec::new();

        // Scope check: verify caller can mutate this metric
        Self::check_mutate_access(scope, metric)?;

        // Validate SafeMode constraints (stack trace/memory snapshot in SafeMode)
        self.validate_safe_mode(metric)?;

        // Validate changes
        self.validate_metric(metric)?;

        // Get old metric to check if enabled status changed
        let old_metric = if let Some(id) = metric.id {
            match self.storage.find_by_id(id).await {
                Ok(m) => m,
                Err(e) => {
                    warnings.push(OperationWarning::FetchFailed {
                        context: format!("old metric (id={}) for comparison", id),
                        error: e.to_string(),
                    });
                    None
                }
            }
        } else {
            None
        };

        // Update in storage
        self.storage.update(metric).await?;

        // Get connection-specific adapter (if available)
        let adapter = match self.get_adapter_for_connection(&metric.connection_id).await {
            Ok(adapter) => Some(adapter),
            Err(e) => {
                warnings.push(OperationWarning::AdapterSyncFailed {
                    metric_name: metric.name.clone(),
                    error: format!("Failed to get adapter for connection: {}", e),
                });
                None
            }
        };

        // Update in DAP adapter based on enabled status
        if let Some(adapter) = adapter {
            if let Some(old) = old_metric {
                match (old.enabled, metric.enabled) {
                    (false, true) => {
                        // Metric was disabled, now enabled - set it
                        adapter.set_metric(metric).await?;
                    }
                    (true, false) => {
                        // Metric was enabled, now disabled - remove it
                        adapter.remove_metric(metric).await?;
                    }
                    (true, true) => {
                        // Metric was and still is enabled - update it
                        adapter.remove_metric(&old).await?;
                        adapter.set_metric(metric).await?;
                    }
                    (false, false) => {
                        // Metric was and still is disabled - no adapter action needed
                    }
                }
            } else if metric.enabled {
                // New metric being updated (shouldn't happen, but handle gracefully)
                adapter.set_metric(metric).await?;
            }
        }
        // If no adapter found, metric will be synced when connection is established

        Ok(OperationOutcome::with_warnings((), warnings))
    }

    /// Get a metric by ID
    pub async fn get_metric(&self, metric_id: MetricId) -> Result<Option<Metric>> {
        let metric = self.storage.find_by_id(metric_id).await?;
        Ok(metric)
    }

    /// Get a metric by name
    pub async fn get_metric_by_name(&self, name: &str) -> Result<Option<Metric>> {
        let metric = self.storage.find_by_name(name).await?;
        Ok(metric)
    }

    /// List all metrics
    pub async fn list_metrics(&self) -> Result<Vec<Metric>> {
        let metrics = self.storage.find_all().await?;
        Ok(metrics)
    }

    /// List metrics with pagination
    ///
    /// Returns a tuple of (metrics, total_count) for efficient pagination.
    /// Results are ordered by created_at DESC (newest first).
    ///
    /// # Arguments
    /// * `limit` - Maximum number of metrics to return
    /// * `offset` - Number of metrics to skip
    pub async fn list_metrics_paginated(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Metric>, u64)> {
        let (metrics, total) = self.storage.find_paginated(limit, offset).await?;
        Ok((metrics, total))
    }

    /// List metrics with filtering at database level (PERF-02 N+1 fix)
    ///
    /// Returns a tuple of (metrics, total_count) for efficient pagination.
    /// Filters are applied at the database level for better performance.
    ///
    /// # Arguments
    /// * `filter` - Filter criteria (connection_id, enabled, group)
    /// * `limit` - Maximum number of metrics to return
    /// * `offset` - Number of metrics to skip
    pub async fn list_metrics_filtered(
        &self,
        filter: &MetricFilter,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Metric>, u64)> {
        let (metrics, total) = self.storage.find_filtered(filter, limit, offset).await?;
        Ok((metrics, total))
    }

    /// List metrics by group
    pub async fn list_metrics_by_group(&self, group: &str) -> Result<Vec<Metric>> {
        let metrics = self.storage.find_by_group(group).await?;
        Ok(metrics)
    }
}
