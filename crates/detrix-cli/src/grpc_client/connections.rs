//! gRPC client for connection operations

use super::helpers::connection_info_from_proto;
use super::types::ConnectionInfo;
use anyhow::{Context, Result};
use detrix_api::generated::detrix::v1::{
    connection_service_client::ConnectionServiceClient, CleanupConnectionsRequest,
    CloseConnectionRequest, CreateConnectionRequest, GetConnectionRequest,
    ListActiveConnectionsRequest, ListConnectionsRequest, RequestMetadata,
};
use detrix_api::grpc::AuthChannel;

/// gRPC client for connection operations
pub struct ConnectionsClient {
    client: ConnectionServiceClient<AuthChannel>,
}

impl ConnectionsClient {
    /// Create a new connections client from an existing channel
    pub fn new(channel: AuthChannel) -> Self {
        Self {
            client: ConnectionServiceClient::new(channel),
        }
    }

    /// Create a new connections client using pre-discovered endpoints
    pub async fn with_endpoints(endpoints: &detrix_api::grpc::DaemonEndpoints) -> Result<Self> {
        let channel = super::connect_with_endpoints(endpoints).await?;
        Ok(Self::new(channel))
    }

    /// Create a new connection
    ///
    /// For Rust with launch mode, provide `program` path and start lldb-dap first:
    /// `lldb-dap --connection listen://HOST:PORT`
    pub async fn create(
        &mut self,
        host: &str,
        port: u32,
        language: &str,
        connection_id: Option<&str>,
        program: Option<&str>,
    ) -> Result<ConnectionInfo> {
        let request = CreateConnectionRequest {
            host: host.to_string(),
            port,
            language: language.to_string(),
            connection_id: connection_id.map(|s| s.to_string()),
            metadata: Some(RequestMetadata::default()),
            program: program.map(|s| s.to_string()),
        };

        let response = self
            .client
            .create_connection(request)
            .await
            .context("Failed to create connection via gRPC")?
            .into_inner();

        let conn = response
            .connection
            .ok_or_else(|| anyhow::anyhow!("No connection in response"))?;

        Ok(connection_info_from_proto(conn))
    }

    /// List all connections (optionally only active ones)
    pub async fn list(&mut self, active_only: bool) -> Result<Vec<ConnectionInfo>> {
        let response = if active_only {
            let request = ListActiveConnectionsRequest {
                metadata: Some(RequestMetadata::default()),
            };
            self.client
                .list_active_connections(request)
                .await
                .context("Failed to list active connections via gRPC")?
                .into_inner()
        } else {
            let request = ListConnectionsRequest {
                metadata: Some(RequestMetadata::default()),
            };
            self.client
                .list_connections(request)
                .await
                .context("Failed to list connections via gRPC")?
                .into_inner()
        };

        Ok(response
            .connections
            .into_iter()
            .map(connection_info_from_proto)
            .collect())
    }

    /// Get a connection by ID
    pub async fn get(&mut self, connection_id: &str) -> Result<ConnectionInfo> {
        let request = GetConnectionRequest {
            connection_id: connection_id.to_string(),
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .get_connection(request)
            .await
            .context("Failed to get connection via gRPC")?
            .into_inner();

        let conn = response
            .connection
            .ok_or_else(|| anyhow::anyhow!("Connection not found"))?;

        Ok(connection_info_from_proto(conn))
    }

    /// Close a connection
    pub async fn close(&mut self, connection_id: &str) -> Result<()> {
        let request = CloseConnectionRequest {
            connection_id: connection_id.to_string(),
            metadata: Some(RequestMetadata::default()),
        };

        self.client
            .close_connection(request)
            .await
            .context("Failed to close connection via gRPC")?;

        Ok(())
    }

    /// Cleanup stale (disconnected/failed) connections
    pub async fn cleanup(&mut self) -> Result<u64> {
        let request = CleanupConnectionsRequest {
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .cleanup_connections(request)
            .await
            .context("Failed to cleanup connections via gRPC")?
            .into_inner();

        Ok(response.deleted)
    }
}
