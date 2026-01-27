//! gRPC client for streaming operations

use super::types::EventInfo;
use anyhow::{Context, Result};
use detrix_api::generated::detrix::v1::{
    streaming_service_client::StreamingServiceClient, QueryRequest, RequestMetadata,
};
use detrix_api::grpc::AuthChannel;

/// gRPC client for streaming operations
pub struct StreamingClient {
    client: StreamingServiceClient<AuthChannel>,
}

impl StreamingClient {
    /// Create a new streaming client from an existing channel
    pub fn new(channel: AuthChannel) -> Self {
        Self {
            client: StreamingServiceClient::new(channel),
        }
    }

    /// Create a new streaming client using pre-discovered endpoints
    pub async fn with_endpoints(endpoints: &detrix_api::grpc::DaemonEndpoints) -> Result<Self> {
        let channel = super::connect_with_endpoints(endpoints).await?;
        Ok(Self::new(channel))
    }

    /// Query historical metric events
    pub async fn query_metrics(
        &mut self,
        metric_ids: Vec<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<EventInfo>> {
        let request = QueryRequest {
            metric_ids,
            time_range: None,
            limit,
            offset: None,
            cursor: None,
            order: None,
            metadata: Some(RequestMetadata::default()),
        };

        let response = self
            .client
            .query_metrics(request)
            .await
            .context("Failed to query metrics via gRPC")?
            .into_inner();

        Ok(response
            .events
            .into_iter()
            .map(|e| EventInfo {
                metric_id: e.metric_id,
                metric_name: e.metric_name,
                timestamp: e.timestamp,
                value_json: match e.result {
                    Some(detrix_api::generated::detrix::v1::metric_event::Result::ValueJson(v)) => {
                        Some(v)
                    }
                    _ => None,
                },
                thread_name: e.thread_name,
                thread_id: e.thread_id,
            })
            .collect())
    }
}
