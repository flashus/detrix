//! StreamingService gRPC implementation
//!
//! Following Clean Architecture: This is a CONTROLLER that does DTO mapping ONLY.
//! ALL business logic is in services::StreamingService.

use crate::error::ToStatusResult;
use crate::generated::detrix::v1::{streaming_service_server::StreamingService, *};
use crate::grpc::conversions::core_event_to_proto;
use crate::state::ApiState;
use detrix_application::{MetricFilter, MetricScope};
use futures::Stream;
use regex::Regex;
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;
use tonic::{Request, Response, Status};

type EventStream = Pin<Box<dyn Stream<Item = Result<MetricEvent, Status>> + Send>>;

#[derive(Debug, Clone)]
pub struct StreamingServiceImpl {
    state: Arc<ApiState>,
}

impl StreamingServiceImpl {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }

    /// Build a set of metric IDs the given scope is allowed to read.
    async fn allowed_metric_ids(&self, scope: &MetricScope) -> Result<HashSet<u64>, Status> {
        let filter = MetricFilter {
            user_id: scope.user_id().map(|s| s.to_string()),
            ..Default::default()
        };
        let (metrics, _) = self
            .state
            .context
            .metric_service
            .list_metrics_filtered(&filter, usize::MAX, 0)
            .await
            .to_status()?;
        Ok(metrics.iter().filter_map(|m| m.id.map(|id| id.0)).collect())
    }
}

#[tonic::async_trait]
impl StreamingService for StreamingServiceImpl {
    type StreamMetricsStream = EventStream;
    type StreamGroupStream = EventStream;
    type StreamAllStream = EventStream;

    async fn stream_metrics(
        &self,
        request: Request<StreamRequest>,
    ) -> Result<Response<Self::StreamMetricsStream>, Status> {
        // Extract user BEFORE into_inner() consumes the request
        let user = crate::grpc::extract_user(&request)?;
        let client_id = crate::grpc::extract_client_id(&request)?;
        let scope = user.scope(client_id);

        let req = request.into_inner();
        let mut metric_ids: Vec<u64> = req.metric_ids;

        // Non-admin scope: filter to only allowed metric IDs
        if scope.user_id().is_some() {
            let allowed = self.allowed_metric_ids(&scope).await?;
            if metric_ids.is_empty() {
                metric_ids = allowed.into_iter().collect();
            } else {
                metric_ids.retain(|id| allowed.contains(id));
            }
        }

        // Subscribe to event broadcast
        let mut rx = self.state.subscribe_events();

        // Create a stream that filters events by metric_id
        let stream = async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        // Filter by metric_id if specified
                        if metric_ids.is_empty() || metric_ids.contains(&event.metric_id.0) {
                            // Convert to proto and yield
                            yield Ok(core_event_to_proto(&event));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Client was too slow, some events were dropped - continue streaming
                        tracing::warn!("gRPC stream_metrics: Client lagged, missed {} events", n);
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // Channel closed - stop streaming
                        tracing::debug!("gRPC stream_metrics: Event channel closed");
                        break;
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(stream) as Self::StreamMetricsStream))
    }

    async fn stream_group(
        &self,
        request: Request<GroupStreamRequest>,
    ) -> Result<Response<Self::StreamGroupStream>, Status> {
        // Extract user BEFORE into_inner() consumes the request
        let user = crate::grpc::extract_user(&request)?;
        let client_id = crate::grpc::extract_client_id(&request)?;
        let scope = user.scope(client_id);

        let req = request.into_inner();
        let group_name = req.group_name;

        // Get all metrics in this group — push user_id filter to DB level
        let filter = detrix_application::ports::MetricFilter {
            user_id: scope.user_id().map(String::from),
            group: Some(group_name.clone()),
            ..Default::default()
        };
        let (metrics, _) = self
            .state
            .context
            .metric_service
            .list_metrics_filtered(&filter, usize::MAX, 0)
            .await
            .to_status()?;

        let metric_ids: Vec<u64> = metrics.iter().filter_map(|m| m.id.map(|id| id.0)).collect();

        if metric_ids.is_empty() {
            return Err(Status::not_found(format!(
                "No metrics found in group '{}'",
                group_name
            )));
        }

        // Subscribe to event broadcast
        let mut rx = self.state.subscribe_events();

        // Create a stream that filters events by group's metric_ids
        let stream = async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if metric_ids.contains(&event.metric_id.0) {
                            yield Ok(core_event_to_proto(&event));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Client was too slow, some events were dropped - continue streaming
                        tracing::warn!("gRPC stream_group: Client lagged, missed {} events", n);
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // Channel closed - stop streaming
                        tracing::debug!("gRPC stream_group: Event channel closed");
                        break;
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(stream) as Self::StreamGroupStream))
    }

    async fn stream_all(
        &self,
        request: Request<StreamAllRequest>,
    ) -> Result<Response<Self::StreamAllStream>, Status> {
        // Extract user BEFORE into_inner() consumes the request
        let user = crate::grpc::extract_user(&request)?;
        let client_id = crate::grpc::extract_client_id(&request)?;
        let scope = user.scope(client_id);

        let req = request.into_inner();

        // Non-admin scope: build allowed metric ID set
        let allowed_ids: Option<std::collections::HashSet<u64>> = if scope.user_id().is_some() {
            Some(self.allowed_metric_ids(&scope).await?)
        } else {
            None
        };

        // Compile thread filter regex if provided
        let thread_filter = req
            .thread_filter
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(|pattern| {
                Regex::new(pattern).map_err(|e| {
                    Status::invalid_argument(format!("Invalid thread_filter regex: {}", e))
                })
            })
            .transpose()?;

        // Subscribe to event broadcast
        let mut rx = self.state.subscribe_events();

        // Create a stream that filters events by thread name and scope
        let stream = async_stream::stream! {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        // Apply scope filter for non-admin users
                        if let Some(ref allowed) = allowed_ids {
                            if !allowed.contains(&event.metric_id.0) {
                                continue;
                            }
                        }

                        // Apply thread filter if provided
                        let matches = match (&thread_filter, &event.thread_name) {
                            (Some(regex), Some(name)) => regex.is_match(name),
                            (Some(_), None) => false,  // Filter set but no thread name - skip
                            (None, _) => true,         // No filter - include all
                        };

                        if matches {
                            yield Ok(core_event_to_proto(&event));
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Client was too slow, some events were dropped - continue streaming
                        tracing::warn!("gRPC stream_all: Client lagged, missed {} events", n);
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // Channel closed - stop streaming
                        tracing::debug!("gRPC stream_all: Event channel closed");
                        break;
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(stream) as Self::StreamAllStream))
    }

    async fn query_metrics(
        &self,
        request: Request<QueryRequest>,
    ) -> Result<Response<QueryResponse>, Status> {
        // Extract user BEFORE into_inner() consumes the request
        let user = crate::grpc::extract_user(&request)?;
        let client_id = crate::grpc::extract_client_id(&request)?;
        let scope = user.scope(client_id);

        let req = request.into_inner();

        // Get query limits from ConfigService (hot-reload support)
        let config = self.state.config_service.get_config().await;
        let limit = req
            .limit
            .unwrap_or(config.api.default_query_limit as u32)
            .min(config.api.max_query_limit as u32) as i64;
        let offset = req.offset.unwrap_or(0) as usize;

        // Parse cursor if provided (cursor is timestamp:id format)
        let cursor_filter = req.cursor.as_ref().and_then(|c| parse_cursor(c));

        // Convert metric IDs
        let mut metric_ids: Vec<detrix_core::MetricId> = req
            .metric_ids
            .iter()
            .map(|&id| detrix_core::MetricId(id))
            .collect();

        // Non-admin scope: filter to only allowed metric IDs
        if scope.user_id().is_some() {
            let allowed = self.allowed_metric_ids(&scope).await?;
            if metric_ids.is_empty() {
                metric_ids = allowed.into_iter().map(detrix_core::MetricId).collect();
            } else {
                metric_ids.retain(|id| allowed.contains(&id.0));
            }
        }

        // Query events - fetch extra to determine has_more
        let fetch_limit = limit + 1;
        let mut all_events = if metric_ids.is_empty() {
            // No filter specified - query all recent events
            self.state
                .context
                .streaming_service
                .query_all_events(Some(fetch_limit))
                .await
                .to_status()?
        } else {
            // Filter by specific metric IDs
            self.state
                .context
                .streaming_service
                .query_metric_events_batch(&metric_ids, Some(fetch_limit))
                .await
                .to_status()?
        };

        // Sort by timestamp (descending by default)
        let order = req.order.as_deref().unwrap_or("desc");
        if order == "asc" {
            all_events.sort_by_key(|e| e.timestamp);
        } else {
            all_events.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        }

        // Apply cursor filter if provided
        if let Some((cursor_ts, cursor_id)) = cursor_filter {
            if order == "asc" {
                all_events.retain(|e| {
                    e.timestamp > cursor_ts
                        || (e.timestamp == cursor_ts && e.id.unwrap_or(0) > cursor_id)
                });
            } else {
                all_events.retain(|e| {
                    e.timestamp < cursor_ts
                        || (e.timestamp == cursor_ts && e.id.unwrap_or(0) < cursor_id)
                });
            }
        }

        // Apply offset (alternative to cursor-based pagination)
        if offset > 0 && offset < all_events.len() {
            all_events = all_events.into_iter().skip(offset).collect();
        }

        // Check if there are more results
        let has_more = all_events.len() > limit as usize;

        // Truncate to limit
        all_events.truncate(limit as usize);

        // Generate next cursor from last event
        let next_cursor = if has_more {
            all_events
                .last()
                .map(|e| format_cursor(e.timestamp, e.id.unwrap_or(0)))
        } else {
            None
        };

        // Convert to proto DTOs
        let proto_events: Vec<_> = all_events.iter().map(core_event_to_proto).collect();
        let event_count = proto_events.len() as u32;

        Ok(Response::new(QueryResponse {
            events: proto_events,
            total_count: event_count, // Count of returned events
            has_more,
            next_cursor,
            metadata: None,
        }))
    }

    async fn get_metric_value(
        &self,
        request: Request<GetValueRequest>,
    ) -> Result<Response<MetricValue>, Status> {
        let req = request.into_inner();
        let metric_id = detrix_core::MetricId(req.metric_id);

        // Call service to get latest value
        let event = self
            .state
            .context
            .streaming_service
            .get_latest_value(metric_id)
            .await
            .to_status()?
            .ok_or_else(|| {
                Status::not_found(format!("No events found for metric {}", metric_id))
            })?;

        // Get metric name from storage (event already has it, but verify from source)
        let metric_name = self
            .state
            .context
            .metric_service
            .get_metric(metric_id)
            .await
            .to_status()?
            .map(|m| m.name)
            .unwrap_or_else(|| event.metric_name.clone());

        Ok(Response::new(MetricValue {
            metric_id: metric_id.0,
            metric_name,
            value_json: event.value_json().to_string(),
            timestamp: event.timestamp,
            metadata: None,
        }))
    }
}

/// Parse cursor string (format: "timestamp:id")
fn parse_cursor(cursor: &str) -> Option<(i64, i64)> {
    let parts: Vec<&str> = cursor.split(':').collect();
    if parts.len() == 2 {
        let ts = parts[0].parse::<i64>().ok()?;
        let id = parts[1].parse::<i64>().ok()?;
        Some((ts, id))
    } else {
        None
    }
}

/// Format cursor string from timestamp and id
fn format_cursor(timestamp: i64, id: i64) -> String {
    format!("{}:{}", timestamp, id)
}
