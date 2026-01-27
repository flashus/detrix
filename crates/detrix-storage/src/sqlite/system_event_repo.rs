//! SystemEventRepository implementation for SQLite

use super::SqliteStorage;
use async_trait::async_trait;
use detrix_application::SystemEventRepository;
use detrix_config::constants::SQLITE_MAX_VARIABLES;
use detrix_core::error::{Error, Result};
use detrix_core::system_event::{SystemEvent, SystemEventType};
use detrix_core::ConnectionId;
use sqlx::Row;
use tracing::{debug, trace};

#[async_trait]
impl SystemEventRepository for SqliteStorage {
    async fn save(&self, event: &SystemEvent) -> Result<i64> {
        let event_type_str = event.event_type.as_str();
        let connection_id_str = event.connection_id.as_ref().map(|c| c.0.as_str());

        let id = sqlx::query(
            r#"
            INSERT INTO system_events (
                event_type, connection_id, metric_id, metric_name,
                timestamp, details_json, acknowledged,
                actor, resource_type, resource_id, old_value, new_value
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(event_type_str)
        .bind(connection_id_str)
        .bind(event.metric_id.map(|id| id as i64)) // SQLite only supports i64
        .bind(&event.metric_name)
        .bind(event.timestamp)
        .bind(&event.details_json)
        .bind(if event.acknowledged { 1 } else { 0 })
        // Audit columns
        .bind(&event.actor)
        .bind(&event.resource_type)
        .bind(&event.resource_id)
        .bind(&event.old_value)
        .bind(&event.new_value)
        .execute(self.pool())
        .await?
        .last_insert_rowid();

        trace!(id = id, event_type = event_type_str, "Saved system event");
        Ok(id)
    }

    async fn save_batch(&self, events: &[SystemEvent]) -> Result<Vec<i64>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        // SQLite variable limit (SQLITE_MAX_VARIABLE_NUMBER): 12 columns per event (7 + 5 audit)
        const COLUMNS_PER_EVENT: usize = 12;
        const CHUNK_SIZE: usize = SQLITE_MAX_VARIABLES / COLUMNS_PER_EVENT;

        let mut all_ids = Vec::with_capacity(events.len());
        let mut tx = self.pool().begin().await?;

        for chunk in events.chunks(CHUNK_SIZE) {
            let placeholders: Vec<String> = chunk
                .iter()
                .map(|_| "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)".to_string())
                .collect();

            let query = format!(
                r#"
                INSERT INTO system_events (
                    event_type, connection_id, metric_id, metric_name,
                    timestamp, details_json, acknowledged,
                    actor, resource_type, resource_id, old_value, new_value
                )
                VALUES {}
                "#,
                placeholders.join(", ")
            );

            let mut query_builder = sqlx::query(&query);

            for event in chunk {
                let event_type_str = event.event_type.as_str();
                let connection_id_str = event.connection_id.as_ref().map(|c| c.0.clone());

                query_builder = query_builder
                    .bind(event_type_str)
                    .bind(connection_id_str)
                    .bind(event.metric_id.map(|id| id as i64)) // SQLite only supports i64
                    .bind(&event.metric_name)
                    .bind(event.timestamp)
                    .bind(&event.details_json)
                    .bind(if event.acknowledged { 1 } else { 0 })
                    // Audit columns
                    .bind(&event.actor)
                    .bind(&event.resource_type)
                    .bind(&event.resource_id)
                    .bind(&event.old_value)
                    .bind(&event.new_value);
            }

            let result = query_builder.execute(&mut *tx).await?;
            let first_id = result.last_insert_rowid() - (chunk.len() as i64 - 1);

            for i in 0..chunk.len() {
                all_ids.push(first_id + i as i64);
            }
        }

        tx.commit().await?;
        debug!(count = all_ids.len(), "Batch saved system events");
        Ok(all_ids)
    }

    async fn find_since(&self, timestamp_micros: i64, limit: i64) -> Result<Vec<SystemEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, event_type, connection_id, metric_id, metric_name,
                   timestamp, details_json, acknowledged,
                   actor, resource_type, resource_id, old_value, new_value
            FROM system_events
            WHERE timestamp > ?
            ORDER BY timestamp ASC
            LIMIT ?
            "#,
        )
        .bind(timestamp_micros)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_system_event).collect()
    }

    async fn find_by_type(
        &self,
        event_type: SystemEventType,
        limit: i64,
    ) -> Result<Vec<SystemEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, event_type, connection_id, metric_id, metric_name,
                   timestamp, details_json, acknowledged,
                   actor, resource_type, resource_id, old_value, new_value
            FROM system_events
            WHERE event_type = ?
            ORDER BY timestamp DESC
            LIMIT ?
            "#,
        )
        .bind(event_type.as_str())
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_system_event).collect()
    }

    async fn find_by_connection(
        &self,
        connection_id: &ConnectionId,
        limit: i64,
    ) -> Result<Vec<SystemEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, event_type, connection_id, metric_id, metric_name,
                   timestamp, details_json, acknowledged,
                   actor, resource_type, resource_id, old_value, new_value
            FROM system_events
            WHERE connection_id = ?
            ORDER BY timestamp DESC
            LIMIT ?
            "#,
        )
        .bind(&connection_id.0)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_system_event).collect()
    }

    async fn find_unacknowledged(&self, limit: i64) -> Result<Vec<SystemEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, event_type, connection_id, metric_id, metric_name,
                   timestamp, details_json, acknowledged,
                   actor, resource_type, resource_id, old_value, new_value
            FROM system_events
            WHERE acknowledged = 0
            ORDER BY timestamp ASC
            LIMIT ?
            "#,
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_system_event).collect()
    }

    async fn acknowledge(&self, event_ids: &[i64]) -> Result<u64> {
        if event_ids.is_empty() {
            return Ok(0);
        }

        // Build IN clause with placeholders
        let placeholders: String = event_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!(
            "UPDATE system_events SET acknowledged = 1 WHERE id IN ({})",
            placeholders
        );

        let mut query_builder = sqlx::query(&query);
        for id in event_ids {
            query_builder = query_builder.bind(id);
        }

        let result = query_builder.execute(self.pool()).await?;
        let count = result.rows_affected();

        debug!(count = count, "Acknowledged system events");
        Ok(count)
    }

    async fn find_recent(&self, limit: i64) -> Result<Vec<SystemEvent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, event_type, connection_id, metric_id, metric_name,
                   timestamp, details_json, acknowledged,
                   actor, resource_type, resource_id, old_value, new_value
            FROM system_events
            ORDER BY timestamp DESC
            LIMIT ?
            "#,
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_system_event).collect()
    }

    async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM system_events WHERE timestamp < ?")
            .bind(timestamp_micros)
            .execute(self.pool())
            .await?;

        let count = result.rows_affected();
        if count > 0 {
            debug!(count = count, "Deleted old system events");
        }
        Ok(count)
    }

    async fn delete_keeping_recent(&self, max_events: usize) -> Result<u64> {
        // If max_events is 0, count-based retention is disabled
        if max_events == 0 {
            return Ok(0);
        }

        // Delete all events except the most recent max_events
        // Uses a subquery to find the cutoff ID
        let result = sqlx::query(
            r#"
            DELETE FROM system_events
            WHERE id NOT IN (
                SELECT id FROM system_events
                ORDER BY timestamp DESC
                LIMIT ?
            )
            "#,
        )
        .bind(max_events as i64)
        .execute(self.pool())
        .await?;

        let count = result.rows_affected();
        if count > 0 {
            debug!(
                count = count,
                max_events = max_events,
                "Deleted oldest system events (count-based retention)"
            );
        }
        Ok(count)
    }

    async fn count_unacknowledged(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM system_events WHERE acknowledged = 0")
            .fetch_one(self.pool())
            .await?;

        Ok(row.get::<i64, _>("count"))
    }

    async fn count_all(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM system_events")
            .fetch_one(self.pool())
            .await?;

        Ok(row.get::<i64, _>("count"))
    }
}

/// Helper to convert a database row to a SystemEvent
fn row_to_system_event(row: &sqlx::sqlite::SqliteRow) -> Result<SystemEvent> {
    let id: i64 = row.get("id");
    let event_type_str: String = row.get("event_type");
    let connection_id_str: Option<String> = row.get("connection_id");
    let metric_id: Option<i64> = row.get("metric_id");
    let metric_name: Option<String> = row.get("metric_name");
    let timestamp: i64 = row.get("timestamp");
    let details_json: Option<String> = row.get("details_json");
    let acknowledged: i64 = row.get("acknowledged");

    // Audit columns (nullable, may not exist in old databases before migration)
    let actor: Option<String> = row.try_get("actor").unwrap_or(None);
    let resource_type: Option<String> = row.try_get("resource_type").unwrap_or(None);
    let resource_id: Option<String> = row.try_get("resource_id").unwrap_or(None);
    let old_value: Option<String> = row.try_get("old_value").unwrap_or(None);
    let new_value: Option<String> = row.try_get("new_value").unwrap_or(None);

    let event_type = event_type_str
        .parse::<SystemEventType>()
        .map_err(|e| Error::Database(format!("Invalid event_type '{}': {}", event_type_str, e)))?;

    Ok(SystemEvent {
        id: Some(id),
        event_type,
        connection_id: connection_id_str.map(ConnectionId::new),
        metric_id: metric_id.map(|id| id as u64), // SQLite returns i64, domain uses u64
        metric_name,
        timestamp,
        details_json,
        acknowledged: acknowledged != 0,
        // Audit fields
        actor,
        resource_type,
        resource_id,
        old_value,
        new_value,
    })
}
