//! ConnectionRepository implementation for SQLite

use super::{now_micros, SqliteStorage};
use async_trait::async_trait;
use detrix_application::ConnectionRepository;
use detrix_core::error::{Error, Result};
use detrix_core::{Connection, ConnectionId, ConnectionStatus, SourceLanguage};
use sqlx::Row;
use tracing::debug;

/// Column list for SELECT queries on the connections table.
const CONNECTION_COLUMNS: &str =
    "id, name, workspace_root, hostname, host, port, language, status, auto_reconnect, safe_mode, created_at, last_connected_at, last_active";

#[async_trait]
impl ConnectionRepository for SqliteStorage {
    async fn save(&self, connection: &Connection) -> Result<ConnectionId> {
        sqlx::query(
            r#"
            INSERT INTO connections (id, name, workspace_root, hostname, host, port, language, status, auto_reconnect, safe_mode, created_at, last_connected_at, last_active)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                workspace_root = excluded.workspace_root,
                hostname = excluded.hostname,
                host = excluded.host,
                port = excluded.port,
                language = excluded.language,
                status = excluded.status,
                auto_reconnect = excluded.auto_reconnect,
                safe_mode = excluded.safe_mode,
                last_connected_at = excluded.last_connected_at,
                last_active = excluded.last_active
            "#,
        )
        .bind(&connection.id.0)
        .bind(&connection.name)
        .bind(&connection.workspace_root)
        .bind(&connection.hostname)
        .bind(&connection.host)
        .bind(connection.port as i64)
        .bind(connection.language.as_str())
        .bind(connection.status.to_string())
        .bind(connection.auto_reconnect)
        .bind(connection.safe_mode)
        .bind(connection.created_at)
        .bind(connection.last_connected_at)
        .bind(connection.last_active)
        .execute(self.pool())
        .await?;

        debug!(connection_id = %connection.id.0, "Connection saved");

        Ok(connection.id.clone())
    }

    async fn find_by_id(&self, id: &ConnectionId) -> Result<Option<Connection>> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM connections WHERE id = ?",
            CONNECTION_COLUMNS
        ))
        .bind(&id.0)
        .fetch_optional(self.pool())
        .await?;

        match row {
            Some(row) => {
                let connection = row_to_connection(&row)?;
                Ok(Some(connection))
            }
            None => Ok(None),
        }
    }

    async fn find_by_identity(
        &self,
        name: &str,
        language: &str,
        workspace_root: &str,
        hostname: &str,
    ) -> Result<Option<Connection>> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM connections WHERE name = ? AND language = ? AND workspace_root = ? AND hostname = ?",
            CONNECTION_COLUMNS
        ))
        .bind(name)
        .bind(language)
        .bind(workspace_root)
        .bind(hostname)
        .fetch_optional(self.pool())
        .await?;

        match row {
            Some(row) => {
                let connection = row_to_connection(&row)?;
                Ok(Some(connection))
            }
            None => Ok(None),
        }
    }

    async fn find_by_address(&self, host: &str, port: u16) -> Result<Option<Connection>> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM connections WHERE host = ? AND port = ?",
            CONNECTION_COLUMNS
        ))
        .bind(host)
        .bind(port as i64)
        .fetch_optional(self.pool())
        .await?;

        match row {
            Some(row) => {
                let connection = row_to_connection(&row)?;
                Ok(Some(connection))
            }
            None => Ok(None),
        }
    }

    async fn list_all(&self) -> Result<Vec<Connection>> {
        let rows = sqlx::query(&format!(
            "SELECT {} FROM connections ORDER BY last_active DESC",
            CONNECTION_COLUMNS
        ))
        .fetch_all(self.pool())
        .await?;

        let mut connections = Vec::with_capacity(rows.len());
        for row in rows {
            connections.push(row_to_connection(&row)?);
        }

        Ok(connections)
    }

    async fn update(&self, connection: &Connection) -> Result<()> {
        let rows_affected = sqlx::query(
            r#"
            UPDATE connections
            SET name = ?, host = ?, port = ?, language = ?, status = ?, auto_reconnect = ?, last_connected_at = ?, last_active = ?
            WHERE id = ?
            "#,
        )
        .bind(&connection.name)
        .bind(&connection.host)
        .bind(connection.port as i64)
        .bind(connection.language.as_str())
        .bind(connection.status.to_string())
        .bind(connection.auto_reconnect)
        .bind(connection.last_connected_at)
        .bind(connection.last_active)
        .bind(&connection.id.0)
        .execute(self.pool())
        .await?
        .rows_affected();

        if rows_affected == 0 {
            return Err(Error::Database(format!(
                "Connection {} not found",
                connection.id.0
            )));
        }

        debug!(connection_id = %connection.id.0, "Connection updated");

        Ok(())
    }

    async fn update_status(&self, id: &ConnectionId, status: ConnectionStatus) -> Result<()> {
        // If status is Connected, also update last_connected_at
        let now = now_micros();
        let (rows_affected, query) = if matches!(status, ConnectionStatus::Connected) {
            (
                sqlx::query(
                    r#"
                    UPDATE connections
                    SET status = ?, last_connected_at = ?, last_active = ?
                    WHERE id = ?
                    "#,
                )
                .bind(status.to_string())
                .bind(now)
                .bind(now)
                .bind(&id.0)
                .execute(self.pool())
                .await?
                .rows_affected(),
                "with last_connected_at",
            )
        } else {
            (
                sqlx::query(
                    r#"
                    UPDATE connections
                    SET status = ?, last_active = ?
                    WHERE id = ?
                    "#,
                )
                .bind(status.to_string())
                .bind(now)
                .bind(&id.0)
                .execute(self.pool())
                .await?
                .rows_affected(),
                "without last_connected_at",
            )
        };

        if rows_affected == 0 {
            return Err(Error::Database(format!("Connection {} not found", id.0)));
        }

        debug!(connection_id = %id.0, status = %status, query = query, "Connection status updated");

        Ok(())
    }

    async fn touch(&self, id: &ConnectionId) -> Result<()> {
        let now = now_micros();

        let rows_affected = sqlx::query(
            r#"
            UPDATE connections
            SET last_active = ?
            WHERE id = ?
            "#,
        )
        .bind(now)
        .bind(&id.0)
        .execute(self.pool())
        .await?
        .rows_affected();

        if rows_affected == 0 {
            return Err(Error::Database(format!("Connection {} not found", id.0)));
        }

        Ok(())
    }

    async fn delete(&self, id: &ConnectionId) -> Result<()> {
        let rows_affected = sqlx::query(
            r#"
            DELETE FROM connections
            WHERE id = ?
            "#,
        )
        .bind(&id.0)
        .execute(self.pool())
        .await?
        .rows_affected();

        if rows_affected == 0 {
            return Err(Error::Database(format!("Connection {} not found", id.0)));
        }

        debug!(connection_id = %id.0, "Connection deleted");

        Ok(())
    }

    async fn exists(&self, id: &ConnectionId) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as(
            r#"
            SELECT 1
            FROM connections
            WHERE id = ?
            "#,
        )
        .bind(&id.0)
        .fetch_optional(self.pool())
        .await?;

        Ok(row.is_some())
    }

    async fn find_active(&self) -> Result<Vec<Connection>> {
        let rows = sqlx::query(&format!(
            "SELECT {} FROM connections WHERE LOWER(status) IN ('connected', 'connecting') ORDER BY last_active DESC",
            CONNECTION_COLUMNS
        ))
        .fetch_all(self.pool())
        .await?;

        let mut connections = Vec::with_capacity(rows.len());
        for row in rows {
            connections.push(row_to_connection(&row)?);
        }

        Ok(connections)
    }

    async fn find_for_reconnect(&self) -> Result<Vec<Connection>> {
        // Include 'connected' status because when daemon restarts after crash,
        // connections may still be marked as connected but have no running adapters.
        // On daemon restart, all connections with auto_reconnect=true should be considered.
        let rows = sqlx::query(&format!(
            "SELECT {} FROM connections WHERE auto_reconnect = 1 AND LOWER(status) IN ('connected', 'disconnected', 'reconnecting', 'failed') ORDER BY last_active DESC",
            CONNECTION_COLUMNS
        ))
        .fetch_all(self.pool())
        .await?;

        let mut connections = Vec::with_capacity(rows.len());
        for row in rows {
            connections.push(row_to_connection(&row)?);
        }

        Ok(connections)
    }

    async fn find_by_language(&self, language: &str) -> Result<Vec<Connection>> {
        let rows = sqlx::query(&format!(
            "SELECT {} FROM connections WHERE language = ? ORDER BY last_active DESC",
            CONNECTION_COLUMNS
        ))
        .bind(language)
        .fetch_all(self.pool())
        .await?;

        let mut connections = Vec::with_capacity(rows.len());
        for row in rows {
            connections.push(row_to_connection(&row)?);
        }

        Ok(connections)
    }

    async fn delete_disconnected(&self) -> Result<u64> {
        // Delete connections with status: Disconnected or Failed:*
        let result = sqlx::query(
            r#"
            DELETE FROM connections
            WHERE LOWER(status) = 'disconnected' OR LOWER(status) LIKE 'failed:%'
            "#,
        )
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected())
    }
}

/// Convert database row to Connection entity
fn row_to_connection(row: &sqlx::sqlite::SqliteRow) -> Result<Connection> {
    let id: String = row.try_get("id")?;
    let name: Option<String> = row.try_get("name")?;
    let workspace_root: String = row
        .try_get("workspace_root")
        .unwrap_or_else(|_| "/unknown".to_string());
    let hostname: String = row
        .try_get("hostname")
        .unwrap_or_else(|_| "unknown".to_string());
    let host: String = row.try_get("host")?;
    let port: i64 = row.try_get("port")?;
    let language: String = row.try_get("language")?;
    let status_str: String = row.try_get("status")?;
    let auto_reconnect: i64 = row.try_get("auto_reconnect")?;
    let safe_mode: i64 = row.try_get("safe_mode").unwrap_or(0);
    let created_at: i64 = row.try_get("created_at")?;
    let last_connected_at: Option<i64> = row.try_get("last_connected_at")?;
    let last_active: i64 = row.try_get("last_active")?;

    // Parse status string to enum (case-insensitive for compatibility)
    let status = match status_str.to_lowercase().as_str() {
        "disconnected" => ConnectionStatus::Disconnected,
        "connecting" => ConnectionStatus::Connecting,
        "connected" => ConnectionStatus::Connected,
        "reconnecting" => ConnectionStatus::Reconnecting,
        _ if status_str.to_lowercase().starts_with("failed:") => {
            // Extract error message from "failed: error message"
            let error_msg = status_str
                .split_once(':')
                .map(|(_, msg)| msg.trim().to_string())
                .unwrap_or_default();
            ConnectionStatus::Failed(error_msg)
        }
        _ => {
            return Err(Error::Database(format!(
                "Invalid connection status: {}",
                status_str
            )))
        }
    };

    // Parse language from database - should always succeed for valid stored data
    use detrix_core::ParseLanguageResultExt;
    let language: SourceLanguage = language
        .try_into()
        .with_language_context(format!("in database for connection {}", id))?;

    Ok(Connection {
        id: ConnectionId(id),
        name,
        workspace_root,
        hostname,
        host,
        port: port as u16,
        language,
        status,
        auto_reconnect: auto_reconnect != 0,
        safe_mode: safe_mode != 0,
        created_at,
        last_connected_at,
        last_active,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStorage;
    use detrix_core::SourceLanguage;

    async fn create_test_storage() -> SqliteStorage {
        SqliteStorage::in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn test_connection_save_and_find() {
        let storage = create_test_storage().await;

        let connection = Connection::new(
            ConnectionId::from("test_conn_1"),
            "127.0.0.1".to_string(),
            5678,
            SourceLanguage::Python,
        )
        .unwrap();

        // Save connection
        let saved_id = ConnectionRepository::save(&storage, &connection)
            .await
            .unwrap();
        assert_eq!(saved_id.0, "test_conn_1");

        // Find by ID
        let found = ConnectionRepository::find_by_id(&storage, &saved_id)
            .await
            .unwrap();
        assert!(found.is_some());
        let found_conn = found.unwrap();
        assert_eq!(found_conn.id.0, "test_conn_1");
        assert_eq!(found_conn.host, "127.0.0.1");
        assert_eq!(found_conn.port, 5678);
        assert!(matches!(found_conn.status, ConnectionStatus::Disconnected));
    }

    #[tokio::test]
    async fn test_connection_update_status() {
        let storage = create_test_storage().await;

        let connection = Connection::new(
            ConnectionId::from("test_conn_2"),
            "localhost".to_string(),
            6789,
            SourceLanguage::Python,
        )
        .unwrap();

        ConnectionRepository::save(&storage, &connection)
            .await
            .unwrap();

        // Update status to Connected
        ConnectionRepository::update_status(&storage, &connection.id, ConnectionStatus::Connected)
            .await
            .unwrap();

        let found = ConnectionRepository::find_by_id(&storage, &connection.id)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(found.status, ConnectionStatus::Connected));

        // Update status to Failed
        ConnectionRepository::update_status(
            &storage,
            &connection.id,
            ConnectionStatus::Failed("Connection timeout".to_string()),
        )
        .await
        .unwrap();

        let found = ConnectionRepository::find_by_id(&storage, &connection.id)
            .await
            .unwrap()
            .unwrap();
        if let ConnectionStatus::Failed(msg) = found.status {
            assert_eq!(msg, "Connection timeout");
        } else {
            panic!("Expected Failed status");
        }
    }

    #[tokio::test]
    async fn test_connection_touch() {
        let storage = create_test_storage().await;

        let connection = Connection::new(
            ConnectionId::from("test_conn_3"),
            "127.0.0.1".to_string(),
            7890,
            SourceLanguage::Python,
        )
        .unwrap();

        ConnectionRepository::save(&storage, &connection)
            .await
            .unwrap();

        let original_last_active = ConnectionRepository::find_by_id(&storage, &connection.id)
            .await
            .unwrap()
            .unwrap()
            .last_active;

        // Wait a bit and touch
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        ConnectionRepository::touch(&storage, &connection.id)
            .await
            .unwrap();

        let updated_last_active = ConnectionRepository::find_by_id(&storage, &connection.id)
            .await
            .unwrap()
            .unwrap()
            .last_active;

        assert!(updated_last_active > original_last_active);
    }

    #[tokio::test]
    async fn test_connection_list_all() {
        let storage = create_test_storage().await;

        // Save 3 connections
        for i in 1..=3 {
            let conn = Connection::new(
                ConnectionId::from(format!("conn_{}", i)),
                "127.0.0.1".to_string(),
                5000 + i,
                SourceLanguage::Python,
            )
            .unwrap();
            ConnectionRepository::save(&storage, &conn).await.unwrap();
        }

        let connections = ConnectionRepository::list_all(&storage).await.unwrap();
        assert_eq!(connections.len(), 3);
    }

    #[tokio::test]
    async fn test_connection_find_active() {
        let storage = create_test_storage().await;

        // Create 3 connections with different statuses
        let conn1 = Connection::new(
            ConnectionId::from("active_1"),
            "127.0.0.1".to_string(),
            5001,
            SourceLanguage::Python,
        )
        .unwrap();
        ConnectionRepository::save(&storage, &conn1).await.unwrap();
        ConnectionRepository::update_status(&storage, &conn1.id, ConnectionStatus::Connected)
            .await
            .unwrap();

        let conn2 = Connection::new(
            ConnectionId::from("active_2"),
            "127.0.0.1".to_string(),
            5002,
            SourceLanguage::Python,
        )
        .unwrap();
        ConnectionRepository::save(&storage, &conn2).await.unwrap();
        ConnectionRepository::update_status(&storage, &conn2.id, ConnectionStatus::Connecting)
            .await
            .unwrap();

        let conn3 = Connection::new(
            ConnectionId::from("inactive"),
            "127.0.0.1".to_string(),
            5003,
            SourceLanguage::Python,
        )
        .unwrap();
        ConnectionRepository::save(&storage, &conn3).await.unwrap();
        // Leave as Disconnected

        let active = ConnectionRepository::find_active(&storage).await.unwrap();
        assert_eq!(active.len(), 2); // Only Connected and Connecting
    }

    #[tokio::test]
    async fn test_connection_exists() {
        let storage = create_test_storage().await;

        let connection = Connection::new(
            ConnectionId::from("exist_test"),
            "127.0.0.1".to_string(),
            8901,
            SourceLanguage::Python,
        )
        .unwrap();

        // Should not exist before saving
        let exists_before = ConnectionRepository::exists(&storage, &connection.id)
            .await
            .unwrap();
        assert!(!exists_before);

        // Save and check again
        ConnectionRepository::save(&storage, &connection)
            .await
            .unwrap();

        let exists_after = ConnectionRepository::exists(&storage, &connection.id)
            .await
            .unwrap();
        assert!(exists_after);
    }

    #[tokio::test]
    async fn test_connection_delete() {
        let storage = create_test_storage().await;

        let connection = Connection::new(
            ConnectionId::from("delete_test"),
            "127.0.0.1".to_string(),
            9012,
            SourceLanguage::Python,
        )
        .unwrap();

        ConnectionRepository::save(&storage, &connection)
            .await
            .unwrap();

        // Verify it exists
        let exists_before = ConnectionRepository::exists(&storage, &connection.id)
            .await
            .unwrap();
        assert!(exists_before);

        // Delete
        ConnectionRepository::delete(&storage, &connection.id)
            .await
            .unwrap();

        // Verify it's gone
        let exists_after = ConnectionRepository::exists(&storage, &connection.id)
            .await
            .unwrap();
        assert!(!exists_after);
    }

    #[tokio::test]
    async fn test_connection_upsert() {
        let storage = create_test_storage().await;

        let mut connection = Connection::new(
            ConnectionId::from("upsert_test"),
            "127.0.0.1".to_string(),
            9123,
            SourceLanguage::Python,
        )
        .unwrap();

        // Initial save
        ConnectionRepository::save(&storage, &connection)
            .await
            .unwrap();

        // Update fields and save again (upsert)
        connection.set_status(ConnectionStatus::Connected);
        connection.host = "localhost".to_string();

        ConnectionRepository::save(&storage, &connection)
            .await
            .unwrap();

        // Verify updated values
        let found = ConnectionRepository::find_by_id(&storage, &connection.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.host, "localhost");
        assert!(matches!(found.status, ConnectionStatus::Connected));
    }

    #[tokio::test]
    async fn test_connection_find_by_id_with_identity() {
        use detrix_core::ConnectionIdentity;

        let storage = create_test_storage().await;

        let identity =
            ConnectionIdentity::new("test-app", SourceLanguage::Python, "/workspace", "host1");
        let connection =
            Connection::new_with_identity(identity.clone(), "127.0.0.1".to_string(), 5678).unwrap();

        ConnectionRepository::save(&storage, &connection)
            .await
            .unwrap();

        // Find by ID (which is the UUID)
        let found = ConnectionRepository::find_by_id(&storage, &connection.id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(found.id, connection.id);
        assert_eq!(found.name, Some("test-app".to_string()));
        assert_eq!(found.workspace_root, "/workspace");
        assert_eq!(found.hostname, "host1");
    }

    #[tokio::test]
    async fn test_connection_find_by_identity() {
        use detrix_core::ConnectionIdentity;

        let storage = create_test_storage().await;

        let identity = ConnectionIdentity::new(
            "trade-bot",
            SourceLanguage::Go,
            "/workspace/project",
            "server1",
        );
        let connection =
            Connection::new_with_identity(identity.clone(), "127.0.0.1".to_string(), 5679).unwrap();

        ConnectionRepository::save(&storage, &connection)
            .await
            .unwrap();

        // Find by identity components
        let found = ConnectionRepository::find_by_identity(
            &storage,
            "trade-bot",
            "go",
            "/workspace/project",
            "server1",
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(found.id, connection.id);
        assert_eq!(found.workspace_root, "/workspace/project");
    }

    #[tokio::test]
    async fn test_connection_different_workspace_different_uuid() {
        use detrix_core::ConnectionIdentity;

        let storage = create_test_storage().await;

        let identity1 =
            ConnectionIdentity::new("app", SourceLanguage::Python, "/workspace1", "host");
        let conn1 =
            Connection::new_with_identity(identity1, "127.0.0.1".to_string(), 5680).unwrap();

        let identity2 =
            ConnectionIdentity::new("app", SourceLanguage::Python, "/workspace2", "host");
        let conn2 =
            Connection::new_with_identity(identity2, "127.0.0.1".to_string(), 5681).unwrap();

        ConnectionRepository::save(&storage, &conn1).await.unwrap();
        ConnectionRepository::save(&storage, &conn2).await.unwrap();

        // Different workspace → different UUID
        assert_ne!(conn1.id.0, conn2.id.0);
        assert_ne!(conn1.id, conn2.id);

        // Both should be findable by their respective IDs
        let found1 = ConnectionRepository::find_by_id(&storage, &conn1.id)
            .await
            .unwrap()
            .unwrap();
        let found2 = ConnectionRepository::find_by_id(&storage, &conn2.id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(found1.workspace_root, "/workspace1");
        assert_eq!(found2.workspace_root, "/workspace2");
    }
}
