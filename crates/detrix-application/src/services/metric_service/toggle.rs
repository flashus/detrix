//! Toggle and group operations for MetricService

use crate::ports::{GroupSummary, ToggleMetricResult};
use crate::{GroupOperationResult, Result};
use detrix_core::{GroupInfo, MetricId, SystemEvent};
use std::collections::HashMap;

use super::MetricService;

impl MetricService {
    /// Toggle metric enabled status
    ///
    /// Updates storage first, then adapter. If adapter fails, storage is rolled back.
    /// Returns rich result with both storage and DAP confirmation status.
    pub async fn toggle_metric(
        &self,
        metric_id: MetricId,
        enabled: bool,
    ) -> Result<ToggleMetricResult> {
        let mut metric =
            self.storage.find_by_id(metric_id).await?.ok_or_else(|| {
                detrix_core::Error::MetricNotFound(format!("Metric {}", metric_id))
            })?;

        let was_enabled = metric.enabled;
        if was_enabled == enabled {
            return Ok(ToggleMetricResult::no_change()); // No change needed
        }

        metric.enabled = enabled;

        // Update in storage first
        self.storage.update(&metric).await?;

        // Get connection-specific adapter
        let adapter = self
            .get_adapter_for_connection(&metric.connection_id)
            .await?;

        // Update in DAP adapter and capture the result
        let toggle_result = if enabled {
            match adapter.set_metric(&metric).await {
                Ok(set_result) => Ok(ToggleMetricResult::enabled(&set_result)),
                Err(e) => Err(e),
            }
        } else {
            match adapter.remove_metric(&metric).await {
                Ok(remove_result) => Ok(ToggleMetricResult::disabled(&remove_result)),
                Err(e) => Err(e),
            }
        };

        // Rollback storage if adapter fails
        if let Err(e) = toggle_result {
            metric.enabled = was_enabled;
            if let Err(rollback_err) = self.storage.update(&metric).await {
                // Return compound error with both primary and rollback failure
                return Err(crate::Error::OperationWithRollbackFailure {
                    primary: e.to_string(),
                    rollback: rollback_err.to_string(),
                    context: format!("toggle metric '{}'", metric.name),
                });
            }
            return Err(e.into());
        }

        // Emit metric toggled event
        let event = SystemEvent::metric_toggled(
            metric_id.0,
            metric.name.clone(),
            enabled,
            metric.connection_id.clone(),
        );
        let _ = self.system_event_tx.send(event);

        Ok(toggle_result?)
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
    pub async fn enable_group(&self, group: &str) -> Result<GroupOperationResult> {
        let metrics = self.storage.find_by_group(group).await?;
        let mut succeeded = 0;
        let mut failed: Vec<(String, String)> = Vec::new();

        // Filter metrics that need enabling and group by connection
        //
        // NOTE: Individual storage.update() calls are intentional here (not N+1 anti-pattern).
        // This allows partial success - if one metric fails to update, others still get enabled.
        // A batch update would be all-or-nothing, which is worse for user experience in group
        // operations where partial success is preferred over complete failure.
        let mut by_connection: HashMap<detrix_core::ConnectionId, Vec<detrix_core::Metric>> =
            HashMap::new();
        for mut metric in metrics {
            if !metric.enabled {
                metric.enabled = true;

                // Update storage first
                if let Err(e) = self.storage.update(&metric).await {
                    failed.push((metric.name.clone(), format!("Storage error: {}", e)));
                    continue;
                }

                by_connection
                    .entry(metric.connection_id.clone())
                    .or_default()
                    .push(metric);
            }
        }

        // Process each connection's metrics using batch operations
        for (connection_id, metrics_to_enable) in by_connection {
            let adapter = match self.get_adapter_for_connection(&connection_id).await {
                Ok(a) => a,
                Err(e) => {
                    // Rollback storage for all metrics in this connection
                    for mut metric in metrics_to_enable {
                        metric.enabled = false;
                        let error_msg =
                            if let Err(rollback_err) = self.storage.update(&metric).await {
                                format!(
                                    "No adapter: {}. Additionally, rollback failed: {}",
                                    e, rollback_err
                                )
                            } else {
                                format!("No adapter: {}", e)
                            };
                        failed.push((metric.name.clone(), error_msg));
                    }
                    continue;
                }
            };

            // Build O(1) lookup map for metrics by name (for rollback)
            let metrics_by_name: HashMap<String, detrix_core::Metric> = metrics_to_enable
                .iter()
                .map(|m| (m.name.clone(), m.clone()))
                .collect();

            // Use batch set operation
            let batch_result = adapter
                .set_metrics_batch(
                    &metrics_to_enable,
                    self.batch_threshold(),
                    self.batch_concurrency(),
                )
                .await;
            succeeded += batch_result.success_count();

            // Handle failures with rollback (O(1) lookup per failure)
            for failed_metric in batch_result.failed {
                if let Some(metric) = metrics_by_name.get(&failed_metric.name) {
                    let mut metric = metric.clone();
                    metric.enabled = false;
                    if let Err(rollback_err) = self.storage.update(&metric).await {
                        failed.push((
                            metric.name.clone(),
                            format!(
                                "Adapter error: {}. Additionally, rollback failed: {}",
                                failed_metric.error, rollback_err
                            ),
                        ));
                    } else {
                        failed.push((
                            metric.name.clone(),
                            format!("Adapter error: {}", failed_metric.error),
                        ));
                    }
                }
            }
        }

        Ok(GroupOperationResult { succeeded, failed })
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
    pub async fn disable_group(&self, group: &str) -> Result<GroupOperationResult> {
        let metrics = self.storage.find_by_group(group).await?;
        let mut succeeded = 0;
        let mut failed: Vec<(String, String)> = Vec::new();

        // Filter metrics that need disabling and group by connection
        //
        // NOTE: Individual storage.update() calls are intentional here (not N+1 anti-pattern).
        // This allows partial success - if one metric fails to update, others still get disabled.
        // See enable_group() for full explanation.
        let mut by_connection: HashMap<detrix_core::ConnectionId, Vec<detrix_core::Metric>> =
            HashMap::new();
        for mut metric in metrics {
            if metric.enabled {
                metric.enabled = false;

                // Update storage first
                if let Err(e) = self.storage.update(&metric).await {
                    failed.push((metric.name.clone(), format!("Storage error: {}", e)));
                    continue;
                }

                by_connection
                    .entry(metric.connection_id.clone())
                    .or_default()
                    .push(metric);
            }
        }

        // Process each connection's metrics using batch operations
        for (connection_id, metrics_to_disable) in by_connection {
            let adapter = match self.get_adapter_for_connection(&connection_id).await {
                Ok(a) => a,
                Err(e) => {
                    // Rollback storage for all metrics in this connection
                    for mut metric in metrics_to_disable {
                        metric.enabled = true;
                        let error_msg =
                            if let Err(rollback_err) = self.storage.update(&metric).await {
                                format!(
                                    "No adapter: {}. Additionally, rollback failed: {}",
                                    e, rollback_err
                                )
                            } else {
                                format!("No adapter: {}", e)
                            };
                        failed.push((metric.name.clone(), error_msg));
                    }
                    continue;
                }
            };

            // Build O(1) lookup map for metrics by name (for rollback)
            let metrics_by_name: HashMap<String, detrix_core::Metric> = metrics_to_disable
                .iter()
                .map(|m| (m.name.clone(), m.clone()))
                .collect();

            // Use batch remove operation
            let batch_result = adapter
                .remove_metrics_batch(
                    &metrics_to_disable,
                    self.batch_threshold(),
                    self.batch_concurrency(),
                )
                .await;
            succeeded += batch_result.succeeded;

            // Handle failures with rollback (O(1) lookup per failure)
            for failed_metric in batch_result.failed {
                if let Some(metric) = metrics_by_name.get(&failed_metric.name) {
                    let mut metric = metric.clone();
                    metric.enabled = true;
                    if let Err(rollback_err) = self.storage.update(&metric).await {
                        failed.push((
                            metric.name.clone(),
                            format!(
                                "Adapter error: {}. Additionally, rollback failed: {}",
                                failed_metric.error, rollback_err
                            ),
                        ));
                    } else {
                        failed.push((
                            metric.name.clone(),
                            format!("Adapter error: {}", failed_metric.error),
                        ));
                    }
                }
            }
        }

        Ok(GroupOperationResult { succeeded, failed })
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
}
