//! McpUsageRepository implementation for SQLite

use super::SqliteStorage;
use async_trait::async_trait;
use detrix_application::services::McpUsageEvent;
use detrix_application::{ErrorCountRow, McpUsageRepository, ToolCountRow, UsageStats};
use detrix_config::constants::SQLITE_MAX_VARIABLES;
use detrix_core::error::Result;
use sqlx::Row;
use tracing::{debug, trace};

#[async_trait]
impl McpUsageRepository for SqliteStorage {
    async fn save_event(&self, event: &McpUsageEvent) -> Result<i64> {
        let status = if event.success { "success" } else { "error" };
        let error_code = event.error_code.as_ref().map(|c| c.as_str());
        let prior_tools_json = serde_json::to_string(&event.prior_tools).unwrap_or_default();

        let id = sqlx::query(
            r#"
            INSERT INTO mcp_usage_events (
                timestamp, tool_name, status, error_code,
                latency_ms, session_id, prior_tools
            )
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(event.timestamp_micros)
        .bind(&event.tool_name)
        .bind(status)
        .bind(error_code)
        .bind(event.latency_ms as i64)
        .bind(&event.session_id)
        .bind(&prior_tools_json)
        .execute(self.pool())
        .await?
        .last_insert_rowid();

        trace!(id = id, tool = %event.tool_name, status = status, "Saved MCP usage event");
        Ok(id)
    }

    async fn save_batch(&self, events: &[McpUsageEvent]) -> Result<Vec<i64>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        // SQLite variable limit (SQLITE_MAX_VARIABLE_NUMBER): 7 columns per event
        const COLUMNS_PER_EVENT: usize = 7;
        const CHUNK_SIZE: usize = SQLITE_MAX_VARIABLES / COLUMNS_PER_EVENT;

        let mut all_ids = Vec::with_capacity(events.len());
        let mut tx = self.pool().begin().await?;

        for chunk in events.chunks(CHUNK_SIZE) {
            let placeholders: Vec<String> = chunk
                .iter()
                .map(|_| "(?, ?, ?, ?, ?, ?, ?)".to_string())
                .collect();

            let query = format!(
                r#"
                INSERT INTO mcp_usage_events (
                    timestamp, tool_name, status, error_code,
                    latency_ms, session_id, prior_tools
                )
                VALUES {}
                "#,
                placeholders.join(", ")
            );

            let mut query_builder = sqlx::query(&query);

            for event in chunk {
                let status = if event.success { "success" } else { "error" };
                let error_code = event.error_code.as_ref().map(|c| c.as_str().to_string());
                let prior_tools_json =
                    serde_json::to_string(&event.prior_tools).unwrap_or_default();

                query_builder = query_builder
                    .bind(event.timestamp_micros)
                    .bind(&event.tool_name)
                    .bind(status)
                    .bind(error_code)
                    .bind(event.latency_ms as i64)
                    .bind(&event.session_id)
                    .bind(prior_tools_json);
            }

            let result = query_builder.execute(&mut *tx).await?;
            let first_id = result.last_insert_rowid() - (chunk.len() as i64 - 1);

            for i in 0..chunk.len() {
                all_ids.push(first_id + i as i64);
            }
        }

        tx.commit().await?;
        debug!(count = all_ids.len(), "Batch saved MCP usage events");
        Ok(all_ids)
    }

    async fn get_stats(
        &self,
        since_micros: Option<i64>,
        until_micros: Option<i64>,
    ) -> Result<UsageStats> {
        let (query, binds) = match (since_micros, until_micros) {
            (Some(since), Some(until)) => (
                r#"
                SELECT
                    COUNT(*) as total_calls,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as total_success,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as total_errors,
                    AVG(latency_ms) as avg_latency
                FROM mcp_usage_events
                WHERE timestamp >= ? AND timestamp < ?
                "#,
                vec![since, until],
            ),
            (Some(since), None) => (
                r#"
                SELECT
                    COUNT(*) as total_calls,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as total_success,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as total_errors,
                    AVG(latency_ms) as avg_latency
                FROM mcp_usage_events
                WHERE timestamp >= ?
                "#,
                vec![since],
            ),
            (None, Some(until)) => (
                r#"
                SELECT
                    COUNT(*) as total_calls,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as total_success,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as total_errors,
                    AVG(latency_ms) as avg_latency
                FROM mcp_usage_events
                WHERE timestamp < ?
                "#,
                vec![until],
            ),
            (None, None) => (
                r#"
                SELECT
                    COUNT(*) as total_calls,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as total_success,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as total_errors,
                    AVG(latency_ms) as avg_latency
                FROM mcp_usage_events
                "#,
                vec![],
            ),
        };

        let mut query_builder = sqlx::query(query);
        for bind in binds {
            query_builder = query_builder.bind(bind);
        }

        let row = query_builder.fetch_one(self.pool()).await?;

        Ok(UsageStats {
            total_calls: row.get::<i64, _>("total_calls") as u64,
            total_success: row.get::<Option<i64>, _>("total_success").unwrap_or(0) as u64,
            total_errors: row.get::<Option<i64>, _>("total_errors").unwrap_or(0) as u64,
            avg_latency_ms: row.get::<Option<f64>, _>("avg_latency").unwrap_or(0.0),
        })
    }

    async fn get_error_breakdown(&self, since_micros: Option<i64>) -> Result<Vec<ErrorCountRow>> {
        let rows = if let Some(since) = since_micros {
            sqlx::query(
                r#"
                SELECT error_code, COUNT(*) as count
                FROM mcp_usage_events
                WHERE status = 'error' AND error_code IS NOT NULL AND timestamp >= ?
                GROUP BY error_code
                ORDER BY count DESC
                "#,
            )
            .bind(since)
            .fetch_all(self.pool())
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT error_code, COUNT(*) as count
                FROM mcp_usage_events
                WHERE status = 'error' AND error_code IS NOT NULL
                GROUP BY error_code
                ORDER BY count DESC
                "#,
            )
            .fetch_all(self.pool())
            .await?
        };

        Ok(rows
            .iter()
            .map(|row| ErrorCountRow {
                error_code: row.get("error_code"),
                count: row.get::<i64, _>("count") as u64,
            })
            .collect())
    }

    async fn get_tool_counts(&self, since_micros: Option<i64>) -> Result<Vec<ToolCountRow>> {
        let rows = if let Some(since) = since_micros {
            sqlx::query(
                r#"
                SELECT
                    tool_name,
                    COUNT(*) as count,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as success_count,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as error_count
                FROM mcp_usage_events
                WHERE timestamp >= ?
                GROUP BY tool_name
                ORDER BY count DESC
                "#,
            )
            .bind(since)
            .fetch_all(self.pool())
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT
                    tool_name,
                    COUNT(*) as count,
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as success_count,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as error_count
                FROM mcp_usage_events
                GROUP BY tool_name
                ORDER BY count DESC
                "#,
            )
            .fetch_all(self.pool())
            .await?
        };

        Ok(rows
            .iter()
            .map(|row| ToolCountRow {
                tool_name: row.get("tool_name"),
                count: row.get::<i64, _>("count") as u64,
                success_count: row.get::<i64, _>("success_count") as u64,
                error_count: row.get::<i64, _>("error_count") as u64,
            })
            .collect())
    }

    async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM mcp_usage_events WHERE timestamp < ?")
            .bind(timestamp_micros)
            .execute(self.pool())
            .await?;

        let count = result.rows_affected();
        if count > 0 {
            debug!(count = count, "Deleted old MCP usage events");
        }
        Ok(count)
    }

    async fn count_all(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM mcp_usage_events")
            .fetch_one(self.pool())
            .await?;

        Ok(row.get::<i64, _>("count"))
    }
}
