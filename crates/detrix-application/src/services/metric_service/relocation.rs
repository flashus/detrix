//! File change handling and metric relocation for MetricService

use crate::Result;
use detrix_core::entities::{AnchorStatus, RelocationResult};
use detrix_core::SystemEvent;
use std::path::PathBuf;

use super::MetricService;

/// Result of processing a file change
#[derive(Debug, Default, Clone)]
pub struct FileChangeResult {
    /// Metrics that remained valid at their current location
    pub verified: usize,
    /// Metrics that were successfully relocated
    pub relocated: usize,
    /// Metrics that could not be relocated (orphaned)
    pub orphaned: usize,
    /// Metrics skipped (no anchor data)
    pub skipped: usize,
    /// Metrics where relocation failed due to errors
    pub failed: usize,
}

impl FileChangeResult {
    /// Total number of metrics processed
    pub fn total(&self) -> usize {
        self.verified + self.relocated + self.orphaned + self.skipped + self.failed
    }

    /// Check if all metrics were successfully handled
    pub fn all_ok(&self) -> bool {
        self.orphaned == 0 && self.failed == 0
    }
}

impl MetricService {
    /// Handle file change event for metric relocation
    ///
    /// When a file changes, this method:
    /// 1. Finds all metrics in that file
    /// 2. Verifies anchors or attempts relocation
    /// 3. Updates storage with new locations
    /// 4. Re-sets logpoints in DAP
    /// 5. Emits warning events for relocated/orphaned metrics
    ///
    /// Returns a summary of the relocation results.
    pub async fn handle_file_change(&self, file_path: &str) -> Result<FileChangeResult> {
        let mut result = FileChangeResult::default();

        // Find all metrics in the changed file
        let all_metrics = self.storage.find_all().await?;
        let file_metrics: Vec<_> = all_metrics
            .into_iter()
            .filter(|m| m.location.file == file_path)
            .collect();

        if file_metrics.is_empty() {
            tracing::debug!(file = %file_path, "No metrics in changed file");
            return Ok(result);
        }

        tracing::info!(
            file = %file_path,
            metric_count = file_metrics.len(),
            "Processing file change for metrics"
        );

        for metric in file_metrics {
            let metric_id = match metric.id {
                Some(id) => id,
                None => continue, // Skip metrics without IDs
            };

            // Check if metric has anchor data
            let anchor = match &metric.anchor {
                Some(a) => a,
                None => {
                    // No anchor - just verify the line still exists
                    result.skipped += 1;
                    continue;
                }
            };

            // First, verify anchor at current location
            let is_valid = self
                .anchor_service
                .verify_anchor(&metric.location, anchor, metric.language)
                .await
                .unwrap_or(false);

            if is_valid {
                // Anchor still valid - update verification timestamp
                let mut updated_metric = metric.clone();
                if let Some(ref mut a) = updated_metric.anchor {
                    a.last_verified_at = Some(chrono::Utc::now().timestamp_micros());
                }
                updated_metric.anchor_status = AnchorStatus::Verified;

                if let Err(e) = self.storage.update(&updated_metric).await {
                    tracing::warn!(
                        metric = %metric.name,
                        error = %e,
                        "Failed to update anchor verification timestamp"
                    );
                }
                result.verified += 1;
                continue;
            }

            // Anchor invalid - attempt relocation
            tracing::debug!(
                metric = %metric.name,
                file = %file_path,
                old_line = metric.location.line,
                "Anchor invalid, attempting relocation"
            );

            let relocation = self
                .anchor_service
                .relocate(file_path, anchor, metric.language)
                .await;

            match relocation {
                Ok(RelocationResult::RelocatedInSymbol {
                    old_line,
                    new_line,
                    symbol_name,
                }) => {
                    // Update metric location
                    let mut updated_metric = metric.clone();
                    updated_metric.location.line = new_line;
                    updated_metric.anchor_status = AnchorStatus::Relocated;

                    // Re-capture anchor at new location
                    if let Ok(new_anchor) = self
                        .anchor_service
                        .capture_anchor(&updated_metric.location, updated_metric.language)
                        .await
                    {
                        updated_metric.anchor = Some(new_anchor);
                    }

                    // Update storage
                    if let Err(e) = self.storage.update(&updated_metric).await {
                        tracing::error!(
                            metric = %metric.name,
                            error = %e,
                            "Failed to update relocated metric in storage"
                        );
                        result.failed += 1;
                        continue;
                    }

                    // Re-set logpoint in DAP if enabled
                    if updated_metric.enabled {
                        if let Ok(adapter) = self
                            .get_adapter_for_connection(&updated_metric.connection_id)
                            .await
                        {
                            // Remove old, set new
                            let _ = adapter.remove_metric(&metric).await;
                            if let Err(e) = adapter.set_metric(&updated_metric).await {
                                tracing::warn!(
                                    metric = %metric.name,
                                    error = %e,
                                    "Failed to re-set logpoint after relocation"
                                );
                            }
                        }
                    }

                    // Emit warning event
                    let event = SystemEvent::metric_relocated(
                        metric_id.0,
                        metric.name.clone(),
                        old_line,
                        new_line,
                        Some(symbol_name),
                        metric.connection_id.clone(),
                    );
                    let _ = self.system_event_tx.send(event);

                    tracing::info!(
                        metric = %metric.name,
                        old_line = old_line,
                        new_line = new_line,
                        "Metric relocated via symbol"
                    );
                    result.relocated += 1;
                }
                Ok(RelocationResult::RelocatedByContext {
                    old_line,
                    new_line,
                    confidence,
                }) => {
                    // Similar to symbol relocation
                    let mut updated_metric = metric.clone();
                    updated_metric.location.line = new_line;
                    updated_metric.anchor_status = AnchorStatus::Relocated;

                    if let Ok(new_anchor) = self
                        .anchor_service
                        .capture_anchor(&updated_metric.location, updated_metric.language)
                        .await
                    {
                        updated_metric.anchor = Some(new_anchor);
                    }

                    if let Err(e) = self.storage.update(&updated_metric).await {
                        tracing::error!(
                            metric = %metric.name,
                            error = %e,
                            "Failed to update relocated metric in storage"
                        );
                        result.failed += 1;
                        continue;
                    }

                    if updated_metric.enabled {
                        if let Ok(adapter) = self
                            .get_adapter_for_connection(&updated_metric.connection_id)
                            .await
                        {
                            let _ = adapter.remove_metric(&metric).await;
                            if let Err(e) = adapter.set_metric(&updated_metric).await {
                                tracing::warn!(
                                    metric = %metric.name,
                                    error = %e,
                                    "Failed to re-set logpoint after relocation"
                                );
                            }
                        }
                    }

                    let event = SystemEvent::metric_relocated(
                        metric_id.0,
                        metric.name.clone(),
                        old_line,
                        new_line,
                        None, // No symbol name for context-based relocation
                        metric.connection_id.clone(),
                    );
                    let _ = self.system_event_tx.send(event);

                    tracing::info!(
                        metric = %metric.name,
                        old_line = old_line,
                        new_line = new_line,
                        confidence = confidence,
                        "Metric relocated via context fingerprint"
                    );
                    result.relocated += 1;
                }
                Ok(RelocationResult::ExactMatch) => {
                    // Should not happen since we already verified anchor was invalid
                    result.verified += 1;
                }
                Ok(RelocationResult::Orphaned {
                    last_known_location,
                    reason,
                }) => {
                    // Mark metric as orphaned
                    let mut updated_metric = metric.clone();
                    updated_metric.anchor_status = AnchorStatus::Orphaned;
                    updated_metric.enabled = false; // Disable orphaned metrics

                    if let Err(e) = self.storage.update(&updated_metric).await {
                        tracing::error!(
                            metric = %metric.name,
                            error = %e,
                            "Failed to mark metric as orphaned"
                        );
                    }

                    // Remove from DAP
                    if metric.enabled {
                        if let Ok(adapter) =
                            self.get_adapter_for_connection(&metric.connection_id).await
                        {
                            let _ = adapter.remove_metric(&metric).await;
                        }
                    }

                    // Emit warning event
                    let event = SystemEvent::metric_orphaned(
                        metric_id.0,
                        metric.name.clone(),
                        last_known_location.clone(),
                        reason.clone(),
                        metric.connection_id.clone(),
                    );
                    let _ = self.system_event_tx.send(event);

                    tracing::warn!(
                        metric = %metric.name,
                        last_location = %last_known_location,
                        reason = %reason,
                        "Metric orphaned - manual intervention needed"
                    );
                    result.orphaned += 1;
                }
                Err(e) => {
                    tracing::error!(
                        metric = %metric.name,
                        error = %e,
                        "Relocation failed"
                    );
                    result.failed += 1;
                }
            }
        }

        tracing::info!(
            file = %file_path,
            verified = result.verified,
            relocated = result.relocated,
            orphaned = result.orphaned,
            skipped = result.skipped,
            failed = result.failed,
            "File change processing complete"
        );

        Ok(result)
    }

    /// Get unique directories containing metrics
    ///
    /// Used by `FileWatcherOrchestrator` to derive watch paths when
    /// `watch_paths` config is empty. Returns parent directories of
    /// all metric file paths.
    pub async fn get_watched_directories(&self) -> Vec<PathBuf> {
        use std::collections::HashSet;

        let metrics = match self.storage.find_all().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to get metrics for watch paths");
                return Vec::new();
            }
        };

        // Collect unique parent directories
        let directories: HashSet<PathBuf> = metrics
            .iter()
            .filter_map(|m| {
                let path = PathBuf::from(&m.location.file);
                path.parent().map(|p| p.to_path_buf())
            })
            .filter(|p| p.exists())
            .collect();

        directories.into_iter().collect()
    }
}
