//! Toggle and group operations for MetricService

use crate::ports::{GroupSummary, ToggleMetricResult};
use crate::scope::MetricScope;
use crate::{GroupOperationResult, Result};
use detrix_core::{ConnectionId, GroupInfo, MetricId, SystemEvent};
use std::collections::HashSet;

use super::MetricService;

impl MetricService {
    /// Toggle metric enabled status
    ///
    /// Checks scope permission, updates storage, then syncs the DAP logpoint.
    /// Returns rich result with both storage and DAP confirmation status.
    pub async fn toggle_metric(
        &self,
        metric_id: MetricId,
        enabled: bool,
        scope: &MetricScope,
    ) -> Result<ToggleMetricResult> {
        let mut metric =
            self.storage.find_by_id(metric_id).await?.ok_or_else(|| {
                detrix_core::Error::MetricNotFound(format!("Metric {}", metric_id))
            })?;

        // Scope check
        Self::check_mutate_access(scope, &metric)?;

        let was_enabled = metric.enabled;
        if was_enabled == enabled {
            return Ok(ToggleMetricResult::no_change()); // No change needed
        }

        metric.enabled = enabled;

        // Update in storage first
        self.storage.update(&metric).await?;

        // Build the merged logpoint for this location (same logic as sync_logpoint)
        // and apply it via the adapter — propagating errors for rollback.
        // We inline the merge instead of calling sync_logpoint because toggle is
        // user-initiated and must fail+rollback on adapter errors, whereas sync_logpoint
        // is fire-and-forget (used by delete where the metric is already gone from storage).
        let toggle_result = async {
            let adapter = self
                .get_adapter_for_connection(&metric.connection_id)
                .await?;

            let all_at_loc = self
                .storage
                .find_all_at_location(
                    &metric.connection_id,
                    &metric.location.file,
                    metric.location.line,
                )
                .await?;

            let enabled_at_loc: Vec<&detrix_core::Metric> =
                all_at_loc.iter().filter(|m| m.enabled).collect();

            if !enabled {
                // Disabling: remove this metric from adapter's active_metrics first.
                // Without this, the adapter still tracks the disabled metric and includes
                // its stale breakpoint in setBreakpoints calls.
                adapter.remove_metric(&metric).await?;
            }

            if enabled_at_loc.is_empty() {
                // remove_metric above already handled the DAP cleanup
            } else {
                // Merge expressions from all enabled metrics at this location
                let merged_exprs: Vec<String> = enabled_at_loc
                    .iter()
                    .flat_map(|m| m.expressions.iter().cloned())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let mut merged = enabled_at_loc[0].clone();
                merged.expressions = merged_exprs;
                adapter.set_metric(&merged).await?;
            }
            Ok::<(), crate::Error>(())
        }
        .await;

        if let Err(e) = toggle_result {
            // Rollback storage on adapter failure
            metric.enabled = was_enabled;
            if let Err(rollback_err) = self.storage.update(&metric).await {
                return Err(crate::Error::OperationWithRollbackFailure {
                    primary: e.to_string(),
                    rollback: rollback_err.to_string(),
                    context: format!("toggle metric '{}'", metric.name),
                });
            }
            return Err(e);
        }

        // Emit metric toggled event
        let event = SystemEvent::metric_toggled(
            metric_id.0,
            metric.name.clone(),
            enabled,
            metric.connection_id.clone(),
        );
        if let Err(e) = self.system_event_tx.send(event) {
            tracing::warn!("No subscribers for metric_toggled event: {e}");
        }

        Ok(ToggleMetricResult {
            storage_updated: true,
            dap_confirmed: true,
            actual_line: if enabled {
                Some(metric.location.line)
            } else {
                None
            },
            dap_message: None,
        })
    }

    /// Enable all metrics in a group
    ///
    /// Enables as many metrics as possible, collecting failures.
    /// Returns `GroupOperationResult` with succeeded count and failed metrics.
    ///
    /// Uses batch operations when supported by the adapter for better performance.
    /// Groups metrics by connection_id to minimize adapter calls.
    ///
    /// Use `.ensure_complete()?` if you want to treat any failure as an error:
    /// ```ignore
    /// let result = service.enable_group("trading").await?;
    /// result.ensure_complete()?;  // Err if any metrics failed
    /// ```
    pub async fn enable_group(
        &self,
        group: &str,
        scope: &MetricScope,
    ) -> Result<GroupOperationResult> {
        let metrics = self.storage.find_by_group(group).await?;
        let mut succeeded = 0;
        let mut failed: Vec<(String, String)> = Vec::new();
        let mut skipped = 0u64;

        // Update storage for each metric that needs enabling, collect affected locations.
        //
        // NOTE: Individual storage.update() calls are intentional here (not N+1 anti-pattern).
        // This allows partial success - if one metric fails to update, others still get enabled.
        let mut locations: HashSet<(ConnectionId, String, u32)> = HashSet::new();
        for mut metric in metrics {
            // Scope check: skip metrics the caller can't mutate
            if !scope.can_mutate(&metric) {
                skipped += 1;
                continue;
            }
            if !metric.enabled {
                metric.enabled = true;

                // Update storage first
                if let Err(e) = self.storage.update(&metric).await {
                    failed.push((metric.name.clone(), format!("Storage error: {}", e)));
                    continue;
                }

                locations.insert((
                    metric.connection_id.clone(),
                    metric.location.file.clone(),
                    metric.location.line,
                ));
                succeeded += 1;
            }
        }

        // Sync merged logpoints for each affected location
        for (conn_id, file, line) in &locations {
            if let Err(e) = self.sync_logpoint(conn_id, file, *line, None).await {
                tracing::warn!(
                    file = %file,
                    line = line,
                    error = %e,
                    "sync_logpoint failed during enable_group"
                );
            }
        }

        if skipped > 0 {
            tracing::info!(
                group,
                succeeded,
                failed = failed.len(),
                skipped,
                "enable_group: metrics skipped due to scope restrictions"
            );
        }

        Ok(GroupOperationResult {
            succeeded,
            failed,
            skipped,
        })
    }

    /// Disable all metrics in a group
    ///
    /// Disables as many metrics as possible, collecting failures.
    /// Returns `GroupOperationResult` with succeeded count and failed metrics.
    ///
    /// Uses batch operations when supported by the adapter for better performance.
    /// Groups metrics by connection_id to minimize adapter calls.
    ///
    /// Use `.ensure_complete()?` if you want to treat any failure as an error:
    /// ```ignore
    /// let result = service.disable_group("trading").await?;
    /// result.ensure_complete()?;  // Err if any metrics failed
    /// ```
    pub async fn disable_group(
        &self,
        group: &str,
        scope: &MetricScope,
    ) -> Result<GroupOperationResult> {
        let metrics = self.storage.find_by_group(group).await?;
        let mut succeeded = 0;
        let mut failed: Vec<(String, String)> = Vec::new();
        let mut skipped = 0u64;

        // Update storage for each metric that needs disabling, collect affected locations.
        //
        // NOTE: Individual storage.update() calls are intentional here (not N+1 anti-pattern).
        // This allows partial success - if one metric fails to update, others still get disabled.
        let mut locations: HashSet<(ConnectionId, String, u32)> = HashSet::new();
        for mut metric in metrics {
            // Scope check: skip metrics the caller can't mutate
            if !scope.can_mutate(&metric) {
                skipped += 1;
                continue;
            }
            if metric.enabled {
                metric.enabled = false;

                // Update storage first
                if let Err(e) = self.storage.update(&metric).await {
                    failed.push((metric.name.clone(), format!("Storage error: {}", e)));
                    continue;
                }

                locations.insert((
                    metric.connection_id.clone(),
                    metric.location.file.clone(),
                    metric.location.line,
                ));
                succeeded += 1;
            }
        }

        // Sync merged logpoints for each affected location
        for (conn_id, file, line) in &locations {
            if let Err(e) = self.sync_logpoint(conn_id, file, *line, None).await {
                tracing::warn!(
                    file = %file,
                    line = line,
                    error = %e,
                    "sync_logpoint failed during disable_group"
                );
            }
        }

        if skipped > 0 {
            tracing::info!(
                group,
                succeeded,
                failed = failed.len(),
                skipped,
                "disable_group: metrics skipped due to scope restrictions"
            );
        }

        Ok(GroupOperationResult {
            succeeded,
            failed,
            skipped,
        })
    }

    /// List all groups with metric counts (PERF-02 N+1 fix)
    ///
    /// Uses efficient SQL GROUP BY aggregation instead of loading all metrics.
    pub async fn list_groups(&self) -> Result<Vec<GroupInfo>> {
        // Use efficient GROUP BY query instead of loading all metrics
        let summaries = self.storage.get_group_summaries().await?;

        // Convert GroupSummary -> GroupInfo
        let mut groups: Vec<GroupInfo> = summaries
            .into_iter()
            .filter_map(|s| {
                // Skip ungrouped metrics (name is None)
                s.name.map(|name| GroupInfo {
                    name,
                    metric_count: s.metric_count as u32,
                    enabled_count: s.enabled_count as u32,
                })
            })
            .collect();

        // Sort by name
        groups.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(groups)
    }

    /// Get raw group summaries including ungrouped metrics
    ///
    /// Returns `GroupSummary` which includes metrics with no group (name = None).
    /// Useful for API responses that need to show "default" group.
    pub async fn list_group_summaries(&self) -> Result<Vec<GroupSummary>> {
        let summaries = self.storage.get_group_summaries().await?;
        Ok(summaries)
    }

    /// List group summaries respecting multi-tenant scope.
    ///
    /// Admin scope: uses efficient SQL GROUP BY from storage.
    /// User/Agent scope: fetches the user's metrics and computes summaries in-memory.
    pub async fn list_group_summaries_scoped(
        &self,
        scope: &MetricScope,
    ) -> Result<Vec<GroupSummary>> {
        if scope.user_id().is_none() {
            let summaries = self.storage.get_group_summaries().await?;
            return Ok(summaries);
        }

        let filter = crate::ports::MetricFilter {
            user_id: scope.user_id().map(|s| s.to_string()),
            ..Default::default()
        };
        let (metrics, _) = self.list_metrics_filtered(&filter, usize::MAX, 0).await?;

        let mut group_map: std::collections::HashMap<Option<String>, (u64, u64)> =
            std::collections::HashMap::new();
        for m in &metrics {
            let entry = group_map.entry(m.group.clone()).or_insert((0, 0));
            entry.0 += 1;
            if m.enabled {
                entry.1 += 1;
            }
        }

        Ok(group_map
            .into_iter()
            .map(|(name, (metric_count, enabled_count))| GroupSummary {
                name,
                metric_count,
                enabled_count,
            })
            .collect())
    }

    /// Disable all metrics created by a specific client.
    ///
    /// Used by switch_daemon(disable_metrics=true) for user-scoped cleanup.
    /// Only disables metrics where `user_id` matches the given client identity.
    ///
    /// # Returns
    /// Number of metrics that were disabled.
    pub async fn disable_metrics_by_owner(&self, client_identity: &str) -> Result<u64> {
        use detrix_ports::MetricFilter;

        let filter = MetricFilter {
            user_id: Some(client_identity.to_string()),
            enabled: Some(true),
            ..Default::default()
        };
        let (metrics, _) = self.list_metrics_filtered(&filter, usize::MAX, 0).await?;
        let mut disabled = 0u64;

        // Use Admin scope — this is an internal operation triggered by disconnect
        let scope = MetricScope::Admin;

        for metric in metrics {
            if let Some(metric_id) = metric.id {
                match self.toggle_metric(metric_id, false, &scope).await {
                    Ok(_) => disabled += 1,
                    Err(e) => tracing::warn!(
                        "Failed to disable metric {} for client {}: {e}",
                        metric_id,
                        client_identity
                    ),
                }
            }
        }

        if disabled > 0 {
            tracing::info!(
                disabled,
                client_identity,
                "Disabled metrics owned by client"
            );
        }

        Ok(disabled)
    }
}
