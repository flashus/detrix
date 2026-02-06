//! ConnectionService gRPC implementation
//!
//! Following Clean Architecture: This is a CONTROLLER that does DTO mapping ONLY.
//! ALL business logic is in services::ConnectionService.

use crate::constants::status;
use crate::generated::detrix::v1::{connection_service_server::ConnectionService, *};
use crate::grpc::conversions::core_to_proto_connection_info;
use crate::state::ApiState;
use detrix_core::ConnectionId;
use std::sync::Arc;
use tonic::{Request, Response, Status};

#[derive(Debug, Clone)]
pub struct ConnectionServiceImpl {
    state: Arc<ApiState>,
}

impl ConnectionServiceImpl {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }

    /// Get connection service reference
    fn get_connection_service(&self) -> &Arc<detrix_application::ConnectionService> {
        &self.state.context.connection_service
    }
}

/// Extension trait for converting core Results to gRPC Status Results
trait CoreToStatus<T> {
    fn to_status(self) -> std::result::Result<T, Status>;
}

impl<T> CoreToStatus<T> for std::result::Result<T, detrix_core::Error> {
    fn to_status(self) -> std::result::Result<T, Status> {
        self.map_err(|err| Status::internal(err.to_string()))
    }
}

#[tonic::async_trait]
impl ConnectionService for ConnectionServiceImpl {
    async fn create_connection(
        &self,
        request: Request<CreateConnectionRequest>,
    ) -> Result<Response<CreateConnectionResponse>, Status> {
        let req = request.into_inner();
        let connection_service = self.get_connection_service();

        // Build connection identity from request
        let identity = detrix_core::ConnectionIdentity::new(
            req.name,
            req.language.clone(),
            req.workspace_root,
            req.hostname,
        );

        // Call service (ALL business logic happens here)
        // ConnectionService.create_connection now handles adapter lifecycle internally
        let connection_id = connection_service
            .create_connection(
                req.host.clone(),
                req.port as u16,
                identity,
                req.program,   // Optional program path for Rust direct lldb-dap
                req.pid,       // Optional PID for Rust client AttachPid mode
                req.safe_mode, // SafeMode: only allow logpoints
            )
            .await
            .to_status()?;

        // Fetch the created connection for full response
        let connection = connection_service
            .get_connection(&connection_id)
            .await
            .to_status()?
            .ok_or_else(|| Status::internal("Connection not found after creation"))?;

        Ok(Response::new(CreateConnectionResponse {
            connection_id: connection.id.0.clone(),
            status: status::CREATED.to_string(),
            connection: Some(core_to_proto_connection_info(&connection)),
            metadata: None,
        }))
    }

    async fn close_connection(
        &self,
        request: Request<CloseConnectionRequest>,
    ) -> Result<Response<CloseConnectionResponse>, Status> {
        let req = request.into_inner();
        let connection_service = self.get_connection_service();

        let connection_id = ConnectionId::new(&req.connection_id);

        // Call service
        connection_service
            .disconnect(&connection_id)
            .await
            .to_status()?;

        Ok(Response::new(CloseConnectionResponse {
            success: true,
            metadata: None,
        }))
    }

    async fn get_connection(
        &self,
        request: Request<GetConnectionRequest>,
    ) -> Result<Response<ConnectionResponse>, Status> {
        let req = request.into_inner();
        let connection_service = self.get_connection_service();

        let connection_id = ConnectionId::new(&req.connection_id);

        let connection = connection_service
            .get_connection(&connection_id)
            .await
            .to_status()?
            .ok_or_else(|| {
                Status::not_found(format!("Connection '{}' not found", req.connection_id))
            })?;

        Ok(Response::new(ConnectionResponse {
            connection: Some(core_to_proto_connection_info(&connection)),
            metadata: None,
        }))
    }

    async fn list_connections(
        &self,
        _request: Request<ListConnectionsRequest>,
    ) -> Result<Response<ListConnectionsResponse>, Status> {
        let connection_service = self.get_connection_service();

        let connections = connection_service.list_connections().await.to_status()?;

        let connection_infos: Vec<_> = connections
            .iter()
            .map(core_to_proto_connection_info)
            .collect();

        Ok(Response::new(ListConnectionsResponse {
            connections: connection_infos,
            metadata: None,
        }))
    }

    async fn list_active_connections(
        &self,
        _request: Request<ListActiveConnectionsRequest>,
    ) -> Result<Response<ListConnectionsResponse>, Status> {
        let connection_service = self.get_connection_service();

        let connections = connection_service
            .list_active_connections()
            .await
            .to_status()?;

        let connection_infos: Vec<_> = connections
            .iter()
            .map(core_to_proto_connection_info)
            .collect();

        Ok(Response::new(ListConnectionsResponse {
            connections: connection_infos,
            metadata: None,
        }))
    }

    async fn cleanup_connections(
        &self,
        _request: Request<CleanupConnectionsRequest>,
    ) -> Result<Response<CleanupConnectionsResponse>, Status> {
        let connection_service = self.get_connection_service();

        let deleted = connection_service
            .cleanup_stale_connections()
            .await
            .to_status()?;

        Ok(Response::new(CleanupConnectionsResponse {
            deleted,
            metadata: None,
        }))
    }
}
