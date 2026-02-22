//! ConnectionReferenceRepository implementation for SQLite

use super::{now_micros, SqliteStorage};
use async_trait::async_trait;
use detrix_core::connection_reference::{ClientIdentity, ConnectionReference, ReferenceKind};
use detrix_core::error::Result;
use detrix_core::ConnectionId;
use detrix_ports::ConnectionReferenceRepository;
use sqlx::Row;
use tracing::debug;

#[async_trait]
impl ConnectionReferenceRepository for SqliteStorage {
    async fn add_reference(&self, reference: &ConnectionReference) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO connection_references (connection_id, client_id, kind, created_at, last_active)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(connection_id, client_id) DO UPDATE SET
                last_active = excluded.last_active,
                kind = excluded.kind
            "#,
        )
        .bind(&reference.connection_id.0)
        .bind(reference.client_identity.as_str())
        .bind(reference.kind.as_str())
        .bind(reference.created_at)
        .bind(reference.last_active)
        .execute(self.pool())
        .await?;

        debug!(
            connection_id = %reference.connection_id.0,
            client = %reference.client_identity,
            "Connection reference added/updated"
        );
        Ok(())
    }

    async fn remove_reference_and_count(
        &self,
        connection_id: &ConnectionId,
        client_identity: &ClientIdentity,
    ) -> Result<(bool, u64)> {
        // Use a transaction for atomicity
        let mut tx = self.pool().begin().await?;

        let delete_result = sqlx::query(
            "DELETE FROM connection_references WHERE connection_id = ? AND client_id = ?",
        )
        .bind(&connection_id.0)
        .bind(client_identity.as_str())
        .execute(&mut *tx)
        .await?;

        let was_removed = delete_result.rows_affected() > 0;

        let count_row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM connection_references WHERE connection_id = ?")
                .bind(&connection_id.0)
                .fetch_one(&mut *tx)
                .await?;

        tx.commit().await?;

        let remaining = count_row.0 as u64;
        debug!(
            connection_id = %connection_id.0,
            client = %client_identity,
            was_removed,
            remaining,
            "Reference removed atomically"
        );
        Ok((was_removed, remaining))
    }

    async fn remove_all_by_client_and_count(
        &self,
        client_identity: &ClientIdentity,
    ) -> Result<Vec<(ConnectionId, u64)>> {
        let mut tx = self.pool().begin().await?;

        // Get affected connection IDs first
        let affected_rows = sqlx::query(
            "SELECT DISTINCT connection_id FROM connection_references WHERE client_id = ?",
        )
        .bind(client_identity.as_str())
        .fetch_all(&mut *tx)
        .await?;

        let affected_ids: Vec<String> = affected_rows
            .iter()
            .map(|r| r.get::<String, _>("connection_id"))
            .collect();

        if affected_ids.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        // Delete all references by this client
        sqlx::query("DELETE FROM connection_references WHERE client_id = ?")
            .bind(client_identity.as_str())
            .execute(&mut *tx)
            .await?;

        // Count remaining for each affected connection
        let mut results = Vec::with_capacity(affected_ids.len());
        for conn_id in &affected_ids {
            let count_row: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM connection_references WHERE connection_id = ?",
            )
            .bind(conn_id)
            .fetch_one(&mut *tx)
            .await?;
            results.push((ConnectionId::from(conn_id.as_str()), count_row.0 as u64));
        }

        tx.commit().await?;

        debug!(
            client = %client_identity,
            affected = results.len(),
            "All client references removed atomically"
        );
        Ok(results)
    }

    async fn remove_all_by_connection(&self, connection_id: &ConnectionId) -> Result<u64> {
        let result = sqlx::query("DELETE FROM connection_references WHERE connection_id = ?")
            .bind(&connection_id.0)
            .execute(self.pool())
            .await?;

        Ok(result.rows_affected())
    }

    async fn count_references(&self, connection_id: &ConnectionId) -> Result<u64> {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM connection_references WHERE connection_id = ?")
                .bind(&connection_id.0)
                .fetch_one(self.pool())
                .await?;

        Ok(row.0 as u64)
    }

    async fn find_by_connection(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<Vec<ConnectionReference>> {
        let rows = sqlx::query(
            "SELECT connection_id, client_id, kind, created_at, last_active FROM connection_references WHERE connection_id = ? ORDER BY created_at",
        )
        .bind(&connection_id.0)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_reference).collect()
    }

    async fn find_by_client(
        &self,
        client_identity: &ClientIdentity,
    ) -> Result<Vec<ConnectionReference>> {
        let rows = sqlx::query(
            "SELECT connection_id, client_id, kind, created_at, last_active FROM connection_references WHERE client_id = ? ORDER BY created_at",
        )
        .bind(client_identity.as_str())
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_reference).collect()
    }

    async fn has_reference(
        &self,
        connection_id: &ConnectionId,
        client_identity: &ClientIdentity,
    ) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT 1 FROM connection_references WHERE connection_id = ? AND client_id = ?",
        )
        .bind(&connection_id.0)
        .bind(client_identity.as_str())
        .fetch_optional(self.pool())
        .await?;

        Ok(row.is_some())
    }

    async fn cleanup_stale_references(&self, ttl_days: i64) -> Result<u64> {
        if ttl_days < 0 {
            return Ok(0); // indefinite
        }

        let cutoff = if ttl_days == 0 {
            // Remove all
            i64::MAX
        } else {
            let micros_per_day: i64 = 86_400 * 1_000_000;
            now_micros() - ttl_days * micros_per_day
        };

        let result = sqlx::query("DELETE FROM connection_references WHERE last_active < ?")
            .bind(cutoff)
            .execute(self.pool())
            .await?;

        Ok(result.rows_affected())
    }

    async fn touch_all_by_client(&self, client_identity: &ClientIdentity) -> Result<u64> {
        let now = now_micros();
        let result =
            sqlx::query("UPDATE connection_references SET last_active = ? WHERE client_id = ?")
                .bind(now)
                .bind(client_identity.as_str())
                .execute(self.pool())
                .await?;

        Ok(result.rows_affected())
    }
}

/// Convert database row to ConnectionReference
fn row_to_reference(row: &sqlx::sqlite::SqliteRow) -> Result<ConnectionReference> {
    let connection_id: String = row.try_get("connection_id")?;
    let client_id: String = row.try_get("client_id")?;
    let kind: String = row.try_get("kind")?;
    let created_at: i64 = row.try_get("created_at")?;
    let last_active: i64 = row.try_get("last_active")?;

    Ok(ConnectionReference {
        connection_id: ConnectionId::from(connection_id),
        client_identity: ClientIdentity::from(client_id.as_str()),
        kind: ReferenceKind::from(kind.as_str()),
        created_at,
        last_active,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteStorage;

    async fn create_test_storage() -> SqliteStorage {
        SqliteStorage::in_memory().await.unwrap()
    }

    /// Helper to create a test connection in the DB
    async fn insert_test_connection(storage: &SqliteStorage, id: &str) {
        use detrix_core::{Connection, SourceLanguage};
        let conn = Connection::new(
            ConnectionId::from(id),
            "127.0.0.1".to_string(),
            5678,
            SourceLanguage::Python,
        )
        .unwrap();
        detrix_application::ConnectionRepository::save(storage, &conn)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_add_and_find_reference() {
        let storage = create_test_storage().await;
        insert_test_connection(&storage, "conn-1").await;

        let reference = ConnectionReference::new(
            ConnectionId::from("conn-1"),
            ClientIdentity::bridge("client-A"),
            ReferenceKind::Client,
        );

        ConnectionReferenceRepository::add_reference(&storage, &reference)
            .await
            .unwrap();

        let refs = ConnectionReferenceRepository::find_by_connection(
            &storage,
            &ConnectionId::from("conn-1"),
        )
        .await
        .unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].client_identity.as_str(), "client-A");
    }

    #[tokio::test]
    async fn test_remove_reference_and_count_atomic() {
        let storage = create_test_storage().await;
        insert_test_connection(&storage, "conn-1").await;

        // Add two references
        let ref_a = ConnectionReference::new(
            ConnectionId::from("conn-1"),
            ClientIdentity::bridge("client-A"),
            ReferenceKind::Client,
        );
        let ref_b = ConnectionReference::new(
            ConnectionId::from("conn-1"),
            ClientIdentity::bridge("client-B"),
            ReferenceKind::Client,
        );
        ConnectionReferenceRepository::add_reference(&storage, &ref_a)
            .await
            .unwrap();
        ConnectionReferenceRepository::add_reference(&storage, &ref_b)
            .await
            .unwrap();

        // Remove A → 1 remaining
        let (removed, remaining) = ConnectionReferenceRepository::remove_reference_and_count(
            &storage,
            &ConnectionId::from("conn-1"),
            &ClientIdentity::bridge("client-A"),
        )
        .await
        .unwrap();
        assert!(removed);
        assert_eq!(remaining, 1);

        // Remove B → 0 remaining
        let (removed, remaining) = ConnectionReferenceRepository::remove_reference_and_count(
            &storage,
            &ConnectionId::from("conn-1"),
            &ClientIdentity::bridge("client-B"),
        )
        .await
        .unwrap();
        assert!(removed);
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn test_remove_all_by_client_and_count() {
        let storage = create_test_storage().await;
        insert_test_connection(&storage, "conn-1").await;
        insert_test_connection(&storage, "conn-2").await;

        // Client A holds refs on both connections, Client B on conn-1 only
        for (conn, client) in [
            ("conn-1", "client-A"),
            ("conn-2", "client-A"),
            ("conn-1", "client-B"),
        ] {
            let r = ConnectionReference::new(
                ConnectionId::from(conn),
                ClientIdentity::bridge(client),
                ReferenceKind::Client,
            );
            ConnectionReferenceRepository::add_reference(&storage, &r)
                .await
                .unwrap();
        }

        // Remove all of client-A's refs
        let results = ConnectionReferenceRepository::remove_all_by_client_and_count(
            &storage,
            &ClientIdentity::bridge("client-A"),
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 2);
        // conn-1 should have 1 remaining (client-B), conn-2 should have 0
        for (conn_id, remaining) in &results {
            if conn_id.0 == "conn-1" {
                assert_eq!(*remaining, 1);
            } else if conn_id.0 == "conn-2" {
                assert_eq!(*remaining, 0);
            }
        }
    }

    #[tokio::test]
    async fn test_cascade_delete_on_connection_removal() {
        let storage = create_test_storage().await;
        insert_test_connection(&storage, "conn-1").await;

        let reference = ConnectionReference::new(
            ConnectionId::from("conn-1"),
            ClientIdentity::bridge("client-A"),
            ReferenceKind::Client,
        );
        ConnectionReferenceRepository::add_reference(&storage, &reference)
            .await
            .unwrap();

        // Delete the connection — CASCADE should remove references
        detrix_application::ConnectionRepository::delete(&storage, &ConnectionId::from("conn-1"))
            .await
            .unwrap();

        let count = ConnectionReferenceRepository::count_references(
            &storage,
            &ConnectionId::from("conn-1"),
        )
        .await
        .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_upsert_reference_updates_last_active() {
        let storage = create_test_storage().await;
        insert_test_connection(&storage, "conn-1").await;

        let reference = ConnectionReference::new(
            ConnectionId::from("conn-1"),
            ClientIdentity::bridge("client-A"),
            ReferenceKind::Client,
        );
        ConnectionReferenceRepository::add_reference(&storage, &reference)
            .await
            .unwrap();

        // Wait a bit and add again (upsert)
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let mut updated_ref = reference.clone();
        updated_ref.touch();
        ConnectionReferenceRepository::add_reference(&storage, &updated_ref)
            .await
            .unwrap();

        // Should still be 1 reference (upserted, not duplicated)
        let count = ConnectionReferenceRepository::count_references(
            &storage,
            &ConnectionId::from("conn-1"),
        )
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_has_reference() {
        let storage = create_test_storage().await;
        insert_test_connection(&storage, "conn-1").await;

        let client = ClientIdentity::bridge("client-A");
        let conn_id = ConnectionId::from("conn-1");

        assert!(
            !ConnectionReferenceRepository::has_reference(&storage, &conn_id, &client)
                .await
                .unwrap()
        );

        let reference =
            ConnectionReference::new(conn_id.clone(), client.clone(), ReferenceKind::Client);
        ConnectionReferenceRepository::add_reference(&storage, &reference)
            .await
            .unwrap();

        assert!(
            ConnectionReferenceRepository::has_reference(&storage, &conn_id, &client)
                .await
                .unwrap()
        );
    }
}
