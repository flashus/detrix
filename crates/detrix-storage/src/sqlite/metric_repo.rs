//! MetricRepository implementation for SQLite

use super::{mode_config_to_json, mode_to_string, now_micros, string_to_mode, SqliteStorage};
use crate::json_helpers::{deserialize_optional, serialize_optional};
use async_trait::async_trait;
use detrix_application::MetricRepository;
use detrix_core::entities::{
    AnchorStatus, Metric, MetricId, SafetyLevel, SAFETY_STRICT, SAFETY_TRUSTED,
};
use detrix_core::error::Result;
use detrix_core::ConnectionId;
use sqlx::Row;
use tracing::{debug, trace};

#[async_trait]
impl MetricRepository for SqliteStorage {
    /// Save a new metric (fails on duplicate name+connection_id)
    async fn save(&self, metric: &Metric) -> Result<MetricId> {
        self.save_with_options(metric, false).await
    }

    /// Save a metric with explicit upsert control.
    ///
    /// # Arguments
    /// * `metric` - The metric to save
    /// * `upsert` - If true, update on conflict; if false, fail on conflict (default behavior)
    async fn save_with_options(&self, metric: &Metric, upsert: bool) -> Result<MetricId> {
        let now = now_micros();
        let mode_type = mode_to_string(&metric.mode);
        let mode_config = mode_config_to_json(&metric.mode);
        let safety_level = metric.safety_level.as_str();

        // Serialize stack trace slice to JSON
        let context = format!("metric_name={}", metric.name);
        let stack_trace_slice_json =
            serialize_optional(&metric.stack_trace_slice, "stack_trace_slice", &context);

        // Serialize snapshot scope to string
        let snapshot_scope_str = metric.snapshot_scope.as_ref().map(|scope| match scope {
            detrix_core::SnapshotScope::Local => "local",
            detrix_core::SnapshotScope::Global => "global",
            detrix_core::SnapshotScope::Both => "both",
        });

        // Serialize anchor data
        let (anchor_symbol, anchor_symbol_kind, anchor_symbol_range, anchor_offset) =
            if let Some(ref anchor) = metric.anchor {
                (
                    anchor.enclosing_symbol.clone(),
                    anchor.symbol_kind,
                    anchor
                        .symbol_range
                        .map(|(start, end)| format!("{}:{}", start, end)),
                    anchor.offset_in_symbol,
                )
            } else {
                (None, None, None, None)
            };

        let (anchor_fingerprint, anchor_context_before, anchor_source_line, anchor_context_after) =
            if let Some(ref anchor) = metric.anchor {
                (
                    anchor.context_fingerprint.clone(),
                    anchor.context_before.clone(),
                    anchor.source_line.clone(),
                    anchor.context_after.clone(),
                )
            } else {
                (None, None, None, None)
            };

        let (anchor_last_verified, anchor_original_location) =
            if let Some(ref anchor) = metric.anchor {
                (anchor.last_verified_at, anchor.original_location.clone())
            } else {
                (None, None)
            };

        let anchor_status_str = metric.anchor_status.to_string();

        // For upsert mode, we need RETURNING to get the ID (could be existing or new)
        // For non-upsert mode, we use execute() + last_insert_rowid() which is simpler
        let id = if upsert {
            let query = r#"
            INSERT INTO metrics (
                name, connection_id, group_name, location, expression, expression_hash, language,
                enabled, mode_type, mode_config, condition_expr, safety_level,
                created_at, updated_at, created_by,
                capture_stack_trace, stack_trace_ttl, stack_trace_slice,
                capture_memory_snapshot, snapshot_scope, snapshot_ttl,
                anchor_symbol, anchor_symbol_kind, anchor_symbol_range, anchor_offset,
                anchor_fingerprint, anchor_context_before, anchor_source_line, anchor_context_after,
                anchor_last_verified, anchor_original_location, anchor_status
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21,
                    ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32)
            ON CONFLICT(name, connection_id) DO UPDATE SET
                group_name = excluded.group_name,
                location = excluded.location,
                expression = excluded.expression,
                expression_hash = excluded.expression_hash,
                language = excluded.language,
                enabled = excluded.enabled,
                mode_type = excluded.mode_type,
                mode_config = excluded.mode_config,
                condition_expr = excluded.condition_expr,
                safety_level = excluded.safety_level,
                updated_at = excluded.updated_at,
                capture_stack_trace = excluded.capture_stack_trace,
                stack_trace_ttl = excluded.stack_trace_ttl,
                stack_trace_slice = excluded.stack_trace_slice,
                capture_memory_snapshot = excluded.capture_memory_snapshot,
                snapshot_scope = excluded.snapshot_scope,
                snapshot_ttl = excluded.snapshot_ttl,
                anchor_symbol = excluded.anchor_symbol,
                anchor_symbol_kind = excluded.anchor_symbol_kind,
                anchor_symbol_range = excluded.anchor_symbol_range,
                anchor_offset = excluded.anchor_offset,
                anchor_fingerprint = excluded.anchor_fingerprint,
                anchor_context_before = excluded.anchor_context_before,
                anchor_source_line = excluded.anchor_source_line,
                anchor_context_after = excluded.anchor_context_after,
                anchor_last_verified = excluded.anchor_last_verified,
                anchor_original_location = excluded.anchor_original_location,
                anchor_status = excluded.anchor_status
            RETURNING id
            "#;

            let row = sqlx::query(query)
                .bind(&metric.name)
                .bind(&metric.connection_id.0)
                .bind(&metric.group)
                .bind(metric.location.to_string_format())
                .bind(&metric.expression)
                .bind(metric.expression_hash())
                .bind(metric.language.as_str())
                .bind(metric.enabled)
                .bind(&mode_type)
                .bind(&mode_config)
                .bind(&metric.condition)
                .bind(safety_level)
                .bind(now)
                .bind(now)
                .bind("system") // created_by
                .bind(metric.capture_stack_trace)
                .bind(metric.stack_trace_ttl.map(|t| t as i64))
                .bind(&stack_trace_slice_json)
                .bind(metric.capture_memory_snapshot)
                .bind(snapshot_scope_str)
                .bind(metric.snapshot_ttl.map(|t| t as i64))
                // Anchor columns (?22-?32)
                .bind(&anchor_symbol)
                .bind(anchor_symbol_kind.map(|k| k as i64))
                .bind(&anchor_symbol_range)
                .bind(anchor_offset.map(|o| o as i64))
                .bind(&anchor_fingerprint)
                .bind(&anchor_context_before)
                .bind(&anchor_source_line)
                .bind(&anchor_context_after)
                .bind(anchor_last_verified)
                .bind(&anchor_original_location)
                .bind(&anchor_status_str)
                .fetch_one(self.pool())
                .await?;

            row.get::<i64, _>("id")
        } else {
            // Plain INSERT - use execute() + last_insert_rowid() for simplicity
            let query = r#"
            INSERT INTO metrics (
                name, connection_id, group_name, location, expression, expression_hash, language,
                enabled, mode_type, mode_config, condition_expr, safety_level,
                created_at, updated_at, created_by,
                capture_stack_trace, stack_trace_ttl, stack_trace_slice,
                capture_memory_snapshot, snapshot_scope, snapshot_ttl,
                anchor_symbol, anchor_symbol_kind, anchor_symbol_range, anchor_offset,
                anchor_fingerprint, anchor_context_before, anchor_source_line, anchor_context_after,
                anchor_last_verified, anchor_original_location, anchor_status
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21,
                    ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32)
            "#;

            sqlx::query(query)
                .bind(&metric.name)
                .bind(&metric.connection_id.0)
                .bind(&metric.group)
                .bind(metric.location.to_string_format())
                .bind(&metric.expression)
                .bind(metric.expression_hash())
                .bind(metric.language.as_str())
                .bind(metric.enabled)
                .bind(&mode_type)
                .bind(&mode_config)
                .bind(&metric.condition)
                .bind(safety_level)
                .bind(now)
                .bind(now)
                .bind("system") // created_by
                .bind(metric.capture_stack_trace)
                .bind(metric.stack_trace_ttl.map(|t| t as i64))
                .bind(&stack_trace_slice_json)
                .bind(metric.capture_memory_snapshot)
                .bind(snapshot_scope_str)
                .bind(metric.snapshot_ttl.map(|t| t as i64))
                // Anchor columns (?22-?32)
                .bind(&anchor_symbol)
                .bind(anchor_symbol_kind.map(|k| k as i64))
                .bind(&anchor_symbol_range)
                .bind(anchor_offset.map(|o| o as i64))
                .bind(&anchor_fingerprint)
                .bind(&anchor_context_before)
                .bind(&anchor_source_line)
                .bind(&anchor_context_after)
                .bind(anchor_last_verified)
                .bind(&anchor_original_location)
                .bind(&anchor_status_str)
                .execute(self.pool())
                .await?
                .last_insert_rowid()
        };

        debug!(metric_id = id, name = %metric.name, upsert = upsert, "Metric saved");

        Ok(MetricId(id as u64)) // SQLite returns i64, domain uses u64
    }

    async fn find_by_id(&self, id: MetricId) -> Result<Option<Metric>> {
        let row = sqlx::query("SELECT * FROM metrics WHERE id = ?")
            .bind(id.0 as i64) // SQLite only supports i64
            .fetch_optional(self.pool())
            .await?;

        match row {
            Some(row) => {
                let metric = row_to_metric(&row)?;
                Ok(Some(metric))
            }
            None => Ok(None),
        }
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Metric>> {
        let row = sqlx::query("SELECT * FROM metrics WHERE name = ?")
            .bind(name)
            .fetch_optional(self.pool())
            .await?;

        match row {
            Some(row) => {
                let metric = row_to_metric(&row)?;
                Ok(Some(metric))
            }
            None => Ok(None),
        }
    }

    async fn find_all(&self) -> Result<Vec<Metric>> {
        let rows = sqlx::query("SELECT * FROM metrics ORDER BY created_at DESC")
            .fetch_all(self.pool())
            .await?;

        let metrics: Result<Vec<Metric>> = rows.iter().map(row_to_metric).collect();
        metrics
    }

    async fn find_paginated(&self, limit: usize, offset: usize) -> Result<(Vec<Metric>, u64)> {
        // Execute both queries concurrently for better performance
        let (rows, total) = tokio::try_join!(
            sqlx::query("SELECT * FROM metrics ORDER BY created_at DESC LIMIT ? OFFSET ?")
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(self.pool()),
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM metrics").fetch_one(self.pool())
        )?;

        let metrics: Result<Vec<Metric>> = rows.iter().map(row_to_metric).collect();
        let total = total as u64;

        debug!(
            limit = limit,
            offset = offset,
            returned = metrics.as_ref().map(|m| m.len()).unwrap_or(0),
            total = total,
            "Paginated metrics query"
        );

        Ok((metrics?, total))
    }

    async fn count_all(&self) -> Result<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metrics")
            .fetch_one(self.pool())
            .await?;
        Ok(count as u64)
    }

    async fn find_by_group(&self, group: &str) -> Result<Vec<Metric>> {
        let rows =
            sqlx::query("SELECT * FROM metrics WHERE group_name = ? ORDER BY created_at DESC")
                .bind(group)
                .fetch_all(self.pool())
                .await?;

        let metrics: Result<Vec<Metric>> = rows.iter().map(row_to_metric).collect();
        metrics
    }

    async fn update(&self, metric: &Metric) -> Result<()> {
        use detrix_core::error::Error;

        let id = metric
            .id
            .ok_or_else(|| Error::InvalidConfig("Metric must have ID to update".to_string()))?;

        let now = now_micros();
        let mode_type = mode_to_string(&metric.mode);
        let mode_config = mode_config_to_json(&metric.mode);
        let safety_level = metric.safety_level.as_str();

        // Serialize anchor data
        let (anchor_symbol, anchor_symbol_kind, anchor_symbol_range, anchor_offset) =
            if let Some(ref anchor) = metric.anchor {
                (
                    anchor.enclosing_symbol.clone(),
                    anchor.symbol_kind,
                    anchor
                        .symbol_range
                        .map(|(start, end)| format!("{}:{}", start, end)),
                    anchor.offset_in_symbol,
                )
            } else {
                (None, None, None, None)
            };

        let (anchor_fingerprint, anchor_context_before, anchor_source_line, anchor_context_after) =
            if let Some(ref anchor) = metric.anchor {
                (
                    anchor.context_fingerprint.clone(),
                    anchor.context_before.clone(),
                    anchor.source_line.clone(),
                    anchor.context_after.clone(),
                )
            } else {
                (None, None, None, None)
            };

        let (anchor_last_verified, anchor_original_location) =
            if let Some(ref anchor) = metric.anchor {
                (anchor.last_verified_at, anchor.original_location.clone())
            } else {
                (None, None)
            };

        let anchor_status_str = metric.anchor_status.to_string();

        let result = sqlx::query(
            r#"
            UPDATE metrics SET
                name = ?,
                group_name = ?,
                location = ?,
                expression = ?,
                expression_hash = ?,
                language = ?,
                enabled = ?,
                mode_type = ?,
                mode_config = ?,
                condition_expr = ?,
                safety_level = ?,
                updated_at = ?,
                anchor_symbol = ?,
                anchor_symbol_kind = ?,
                anchor_symbol_range = ?,
                anchor_offset = ?,
                anchor_fingerprint = ?,
                anchor_context_before = ?,
                anchor_source_line = ?,
                anchor_context_after = ?,
                anchor_last_verified = ?,
                anchor_original_location = ?,
                anchor_status = ?
            WHERE id = ?
            "#,
        )
        .bind(&metric.name)
        .bind(&metric.group)
        .bind(metric.location.to_string_format())
        .bind(&metric.expression)
        .bind(metric.expression_hash())
        .bind(metric.language.as_str())
        .bind(metric.enabled)
        .bind(mode_type)
        .bind(mode_config)
        .bind(&metric.condition)
        .bind(safety_level)
        .bind(now)
        .bind(&anchor_symbol)
        .bind(anchor_symbol_kind.map(|k| k as i64))
        .bind(&anchor_symbol_range)
        .bind(anchor_offset.map(|o| o as i64))
        .bind(&anchor_fingerprint)
        .bind(&anchor_context_before)
        .bind(&anchor_source_line)
        .bind(&anchor_context_after)
        .bind(anchor_last_verified)
        .bind(&anchor_original_location)
        .bind(&anchor_status_str)
        .bind(id.0 as i64) // SQLite only supports i64
        .execute(self.pool())
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::MetricNotFound(id.0.to_string()));
        }

        debug!(metric_id = id.0, "Metric updated");
        Ok(())
    }

    async fn delete(&self, id: MetricId) -> Result<()> {
        use detrix_core::error::Error;

        let result = sqlx::query("DELETE FROM metrics WHERE id = ?")
            .bind(id.0 as i64) // SQLite only supports i64
            .execute(self.pool())
            .await?;

        if result.rows_affected() == 0 {
            return Err(Error::MetricNotFound(id.0.to_string()));
        }

        debug!(metric_id = id.0, "Metric deleted");
        Ok(())
    }

    async fn exists_by_name(&self, name: &str) -> Result<bool> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metrics WHERE name = ?")
            .bind(name)
            .fetch_one(self.pool())
            .await?;

        Ok(count > 0)
    }

    async fn find_by_connection_id(&self, connection_id: &ConnectionId) -> Result<Vec<Metric>> {
        let rows =
            sqlx::query("SELECT * FROM metrics WHERE connection_id = ? ORDER BY created_at DESC")
                .bind(&connection_id.0)
                .fetch_all(self.pool())
                .await?;

        let metrics: Result<Vec<Metric>> = rows.iter().map(row_to_metric).collect();
        metrics
    }

    async fn find_by_location(
        &self,
        connection_id: &ConnectionId,
        file: &str,
        line: u32,
    ) -> Result<Option<Metric>> {
        // Location is stored as "@file#line" format
        let location_pattern = format!("@{}#{}", file, line);

        let row =
            sqlx::query("SELECT * FROM metrics WHERE connection_id = ? AND location = ? LIMIT 1")
                .bind(&connection_id.0)
                .bind(&location_pattern)
                .fetch_optional(self.pool())
                .await?;

        match row {
            Some(ref r) => Ok(Some(row_to_metric(r)?)),
            None => Ok(None),
        }
    }

    async fn find_filtered(
        &self,
        filter: &detrix_application::ports::MetricFilter,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Metric>, u64)> {
        // Build dynamic WHERE clause
        let mut conditions = Vec::new();
        let mut bind_values: Vec<String> = Vec::new();

        if let Some(ref conn_id) = filter.connection_id {
            conditions.push("connection_id = ?");
            bind_values.push(conn_id.0.clone());
        }

        if let Some(enabled) = filter.enabled {
            conditions.push(if enabled {
                "enabled = 1"
            } else {
                "enabled = 0"
            });
        }

        if let Some(ref group) = filter.group {
            if group == "default" {
                conditions.push("(group_name IS NULL OR group_name = 'default')");
            } else {
                conditions.push("group_name = ?");
                bind_values.push(group.clone());
            }
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        // Build count and data queries
        let count_query = format!("SELECT COUNT(*) FROM metrics{}", where_clause);
        let data_query = format!(
            "SELECT * FROM metrics{} ORDER BY created_at DESC LIMIT ? OFFSET ?",
            where_clause
        );

        // Execute count query
        let mut count_query_builder = sqlx::query_scalar::<_, i64>(&count_query);
        for value in &bind_values {
            count_query_builder = count_query_builder.bind(value);
        }
        let total = count_query_builder.fetch_one(self.pool()).await? as u64;

        // Execute data query
        let mut data_query_builder = sqlx::query(&data_query);
        for value in &bind_values {
            data_query_builder = data_query_builder.bind(value);
        }
        data_query_builder = data_query_builder.bind(limit as i64).bind(offset as i64);

        let rows = data_query_builder.fetch_all(self.pool()).await?;
        let metrics: Result<Vec<Metric>> = rows.iter().map(row_to_metric).collect();

        debug!(
            limit = limit,
            offset = offset,
            filter = ?filter,
            returned = metrics.as_ref().map(|m| m.len()).unwrap_or(0),
            total = total,
            "Filtered metrics query"
        );

        Ok((metrics?, total))
    }

    async fn get_group_summaries(&self) -> Result<Vec<detrix_application::ports::GroupSummary>> {
        // Use COALESCE to handle NULL group_name as 'default'
        // Group by the actual group_name (including NULL) but report it consistently
        let query = r#"
            SELECT
                group_name,
                COUNT(*) as metric_count,
                SUM(CASE WHEN enabled = 1 THEN 1 ELSE 0 END) as enabled_count
            FROM metrics
            GROUP BY group_name
            ORDER BY COALESCE(group_name, 'default')
        "#;

        let rows = sqlx::query(query).fetch_all(self.pool()).await?;

        let summaries: Vec<detrix_application::ports::GroupSummary> = rows
            .iter()
            .map(|row| {
                let name: Option<String> = row.try_get("group_name").unwrap_or(None);
                let metric_count: i64 = row.try_get("metric_count").unwrap_or(0);
                let enabled_count: i64 = row.try_get("enabled_count").unwrap_or(0);

                detrix_application::ports::GroupSummary {
                    name,
                    metric_count: metric_count as u64,
                    enabled_count: enabled_count as u64,
                }
            })
            .collect();

        debug!(group_count = summaries.len(), "Group summaries query");

        Ok(summaries)
    }
}

/// Convert database row to Metric entity
pub(crate) fn row_to_metric(row: &sqlx::sqlite::SqliteRow) -> Result<Metric> {
    let id: i64 = row.try_get("id")?;
    let name: String = row.try_get("name")?;
    let connection_id: String = row.try_get("connection_id")?;
    let group_name: Option<String> = row.try_get("group_name")?;
    let location: String = row.try_get("location")?;
    let expression: String = row.try_get("expression")?;
    let language: String = row.try_get("language")?;
    let enabled: bool = row.try_get("enabled")?;
    let mode_type: String = row.try_get("mode_type")?;
    let mode_config: Option<String> = row.try_get("mode_config")?;
    let condition_expr: Option<String> = row.try_get("condition_expr")?;
    let safety_level_str: String = row.try_get("safety_level")?;
    let created_at: i64 = row.try_get("created_at")?;

    // Read new fields with logging for unexpected failures
    let capture_stack_trace: bool = row.try_get("capture_stack_trace").unwrap_or_else(|e| {
        // This is expected if column doesn't exist in older DB versions
        trace!(metric_name = %name, error = %e, "capture_stack_trace column not found, defaulting to false");
        false
    });
    let stack_trace_ttl: Option<i64> = row.try_get("stack_trace_ttl").unwrap_or_else(|e| {
        trace!(metric_name = %name, error = %e, "stack_trace_ttl column not found");
        None
    });
    let stack_trace_slice_json: Option<String> =
        row.try_get("stack_trace_slice").unwrap_or_else(|e| {
            trace!(metric_name = %name, error = %e, "stack_trace_slice column not found");
            None
        });
    let capture_memory_snapshot: bool =
        row.try_get("capture_memory_snapshot").unwrap_or_else(|e| {
            trace!(metric_name = %name, error = %e, "capture_memory_snapshot column not found, defaulting to false");
            false
        });
    let snapshot_scope_str: Option<String> = row.try_get("snapshot_scope").unwrap_or_else(|e| {
        trace!(metric_name = %name, error = %e, "snapshot_scope column not found");
        None
    });
    let snapshot_ttl: Option<i64> = row.try_get("snapshot_ttl").unwrap_or_else(|e| {
        trace!(metric_name = %name, error = %e, "snapshot_ttl column not found");
        None
    });

    // Read anchor fields (may not exist in older databases before migration 004)
    let anchor_symbol: Option<String> = row.try_get("anchor_symbol").unwrap_or_else(|e| {
        trace!(metric_name = %name, error = %e, "anchor_symbol column not found");
        None
    });
    let anchor_symbol_kind: Option<i64> = row.try_get("anchor_symbol_kind").unwrap_or_else(|e| {
        trace!(metric_name = %name, error = %e, "anchor_symbol_kind column not found");
        None
    });
    let anchor_symbol_range: Option<String> =
        row.try_get("anchor_symbol_range").unwrap_or_else(|e| {
            trace!(metric_name = %name, error = %e, "anchor_symbol_range column not found");
            None
        });
    let anchor_offset: Option<i64> = row.try_get("anchor_offset").unwrap_or_else(|e| {
        trace!(metric_name = %name, error = %e, "anchor_offset column not found");
        None
    });
    let anchor_fingerprint: Option<String> =
        row.try_get("anchor_fingerprint").unwrap_or_else(|e| {
            trace!(metric_name = %name, error = %e, "anchor_fingerprint column not found");
            None
        });
    let anchor_context_before: Option<String> =
        row.try_get("anchor_context_before").unwrap_or_else(|e| {
            trace!(metric_name = %name, error = %e, "anchor_context_before column not found");
            None
        });
    let anchor_source_line: Option<String> =
        row.try_get("anchor_source_line").unwrap_or_else(|e| {
            trace!(metric_name = %name, error = %e, "anchor_source_line column not found");
            None
        });
    let anchor_context_after: Option<String> =
        row.try_get("anchor_context_after").unwrap_or_else(|e| {
            trace!(metric_name = %name, error = %e, "anchor_context_after column not found");
            None
        });
    let anchor_last_verified: Option<i64> =
        row.try_get("anchor_last_verified").unwrap_or_else(|e| {
            trace!(metric_name = %name, error = %e, "anchor_last_verified column not found");
            None
        });
    let anchor_original_location: Option<String> =
        row.try_get("anchor_original_location").unwrap_or_else(|e| {
            trace!(metric_name = %name, error = %e, "anchor_original_location column not found");
            None
        });
    let anchor_status_str: Option<String> = row.try_get("anchor_status").unwrap_or_else(|e| {
        trace!(metric_name = %name, error = %e, "anchor_status column not found");
        None
    });

    let location = detrix_core::entities::Location::parse(&location)?;
    let mode = string_to_mode(&mode_type, mode_config.as_deref())?;
    let safety_level = match safety_level_str.as_str() {
        SAFETY_STRICT => SafetyLevel::Strict,
        SAFETY_TRUSTED => SafetyLevel::Trusted,
        _ => SafetyLevel::Strict, // Default to strict for unknown values
    };

    // Deserialize stack trace slice from JSON
    let context = format!("metric_name={}", name);
    let stack_trace_slice = deserialize_optional(
        stack_trace_slice_json.as_deref(),
        "stack_trace_slice",
        &context,
    );

    // Deserialize snapshot scope from string
    let snapshot_scope = snapshot_scope_str.and_then(|s| match s.as_str() {
        "local" => Some(detrix_core::SnapshotScope::Local),
        "global" => Some(detrix_core::SnapshotScope::Global),
        "both" => Some(detrix_core::SnapshotScope::Both),
        _ => None,
    });

    // Parse anchor status from string
    let anchor_status = anchor_status_str
        .and_then(|s| match s.as_str() {
            "unanchored" => Some(AnchorStatus::Unanchored),
            "verified" => Some(AnchorStatus::Verified),
            "relocated" => Some(AnchorStatus::Relocated),
            "orphaned" => Some(AnchorStatus::Orphaned),
            _ => None,
        })
        .unwrap_or_default();

    // Parse symbol range from "start:end" format
    let symbol_range = anchor_symbol_range.and_then(|s| {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 2 {
            let start = parts[0].parse::<u32>().ok()?;
            let end = parts[1].parse::<u32>().ok()?;
            Some((start, end))
        } else {
            None
        }
    });

    // Reconstruct MetricAnchor if any anchor data exists
    let anchor = if anchor_symbol.is_some()
        || anchor_fingerprint.is_some()
        || anchor_context_before.is_some()
        || anchor_source_line.is_some()
    {
        Some(detrix_core::entities::MetricAnchor {
            enclosing_symbol: anchor_symbol,
            symbol_kind: anchor_symbol_kind.map(|k| k as u32),
            symbol_range,
            offset_in_symbol: anchor_offset.map(|o| o as u32),
            context_fingerprint: anchor_fingerprint,
            context_before: anchor_context_before,
            source_line: anchor_source_line,
            context_after: anchor_context_after,
            last_verified_at: anchor_last_verified,
            original_location: anchor_original_location,
        })
    } else {
        None
    };

    // Parse language from database - should always succeed for valid stored data
    use detrix_core::ParseLanguageResultExt;
    let language: detrix_core::SourceLanguage = language
        .try_into()
        .with_language_context(format!("in database for metric {}", id))?;

    Ok(Metric {
        id: Some(MetricId(id as u64)), // SQLite returns i64, domain uses u64
        name,
        connection_id: detrix_core::ConnectionId::from(connection_id),
        group: group_name,
        location,
        expression,
        language,
        enabled,
        mode,
        condition: condition_expr,
        safety_level,
        created_at: Some(created_at),
        // Introspection fields
        capture_stack_trace,
        stack_trace_ttl: stack_trace_ttl.map(|t| t as u64),
        stack_trace_slice,
        capture_memory_snapshot,
        snapshot_scope,
        snapshot_ttl: snapshot_ttl.map(|t| t as u64),
        // Anchor tracking fields
        anchor,
        anchor_status,
    })
}
