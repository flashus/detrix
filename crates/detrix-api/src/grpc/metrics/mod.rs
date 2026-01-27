//! MetricsService gRPC implementation
//!
//! Following Clean Architecture: This is a CONTROLLER that does DTO mapping ONLY.
//! ALL business logic is in services::MetricService.

mod config;
mod control;
mod crud;
mod list_filter;
mod system;
mod tools;

use crate::generated::detrix::v1::{metrics_service_server::MetricsService, *};
use crate::state::ApiState;
use std::sync::Arc;
use tonic::{Request, Response, Status};

#[derive(Debug, Clone)]
pub struct MetricsServiceImpl {
    state: Arc<ApiState>,
}

impl MetricsServiceImpl {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl MetricsService for MetricsServiceImpl {
    async fn wake(&self, request: Request<WakeRequest>) -> Result<Response<WakeResponse>, Status> {
        system::handle_wake(&self.state, request).await
    }

    async fn sleep(
        &self,
        request: Request<SleepRequest>,
    ) -> Result<Response<SleepResponse>, Status> {
        system::handle_sleep(&self.state, request).await
    }

    async fn get_status(
        &self,
        request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        system::handle_get_status(&self.state, request).await
    }

    async fn add_metric(
        &self,
        request: Request<AddMetricRequest>,
    ) -> Result<Response<MetricResponse>, Status> {
        crud::handle_add_metric(&self.state, request).await
    }

    async fn remove_metric(
        &self,
        request: Request<RemoveMetricRequest>,
    ) -> Result<Response<RemoveMetricResponse>, Status> {
        crud::handle_remove_metric(&self.state, request).await
    }

    async fn update_metric(
        &self,
        request: Request<UpdateMetricRequest>,
    ) -> Result<Response<MetricResponse>, Status> {
        crud::handle_update_metric(&self.state, request).await
    }

    async fn get_metric(
        &self,
        request: Request<GetMetricRequest>,
    ) -> Result<Response<MetricResponse>, Status> {
        crud::handle_get_metric(&self.state, request).await
    }

    async fn list_metrics(
        &self,
        request: Request<ListMetricsRequest>,
    ) -> Result<Response<ListMetricsResponse>, Status> {
        list_filter::handle_list_metrics(&self.state, request).await
    }

    async fn toggle_metric(
        &self,
        request: Request<ToggleMetricRequest>,
    ) -> Result<Response<ToggleMetricResponse>, Status> {
        control::handle_toggle_metric(&self.state, request).await
    }

    async fn enable_group(
        &self,
        request: Request<GroupRequest>,
    ) -> Result<Response<GroupResponse>, Status> {
        control::handle_enable_group(&self.state, request).await
    }

    async fn disable_group(
        &self,
        request: Request<GroupRequest>,
    ) -> Result<Response<GroupResponse>, Status> {
        control::handle_disable_group(&self.state, request).await
    }

    async fn list_groups(
        &self,
        request: Request<ListGroupsRequest>,
    ) -> Result<Response<ListGroupsResponse>, Status> {
        list_filter::handle_list_groups(&self.state, request).await
    }

    async fn reload_config(
        &self,
        request: Request<ReloadConfigRequest>,
    ) -> Result<Response<ReloadConfigResponse>, Status> {
        config::handle_reload_config(&self.state, request).await
    }

    async fn validate_config(
        &self,
        request: Request<ValidateConfigRequest>,
    ) -> Result<Response<ValidationResponse>, Status> {
        config::handle_validate_config(&self.state, request).await
    }

    async fn get_config(
        &self,
        request: Request<GetConfigRequest>,
    ) -> Result<Response<ConfigResponse>, Status> {
        config::handle_get_config(&self.state, request).await
    }

    async fn update_config(
        &self,
        request: Request<UpdateConfigRequest>,
    ) -> Result<Response<ConfigResponse>, Status> {
        config::handle_update_config(&self.state, request).await
    }

    async fn validate_expression(
        &self,
        request: Request<ValidateExpressionRequest>,
    ) -> Result<Response<ValidateExpressionResponse>, Status> {
        tools::handle_validate_expression(&self.state, request).await
    }

    async fn inspect_file(
        &self,
        request: Request<InspectFileRequest>,
    ) -> Result<Response<InspectFileResponse>, Status> {
        tools::handle_inspect_file(&self.state, request).await
    }

    async fn get_mcp_usage(
        &self,
        request: Request<GetMcpUsageRequest>,
    ) -> Result<Response<GetMcpUsageResponse>, Status> {
        tools::handle_get_mcp_usage(&self.state, request).await
    }
}
