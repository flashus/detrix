//! Repository port traits for data persistence
//!
//! Per Clean Architecture, repository interfaces (output ports) belong in the
//! Application layer. Infrastructure adapters (detrix-storage) implement these traits.

use async_trait::async_trait;
use detrix_core::connection::{Connection, ConnectionId, ConnectionStatus};
use detrix_core::entities::{Metric, MetricEvent, MetricId};
use detrix_core::error::Result;
use detrix_core::system_event::{SystemEvent, SystemEventType};

/// Summary of a metric group (for GROUP BY queries)
#[derive(Debug, Clone)]
pub struct GroupSummary {
    /// Group name (None means "default" group for ungrouped metrics)
    pub name: Option<String>,
    /// Total metrics in the group
    pub metric_count: u64,
    /// Enabled metrics in the group
    pub enabled_count: u64,
}

/// Filter options for querying metrics
#[derive(Debug, Clone, Default)]
pub struct MetricFilter {
    /// Filter by connection ID
    pub connection_id: Option<ConnectionId>,
    /// Filter by enabled state
    pub enabled: Option<bool>,
    /// Filter by group name
    pub group: Option<String>,
}

/// Repository for metric entities
#[async_trait]
pub trait MetricRepository: Send + Sync {
    /// Save a new metric
    ///
    /// If `upsert` is false (default), fails if metric with same (location, connection_id) exists.
    /// If `upsert` is true, updates existing metric with same (location, connection_id).
    ///
    /// Note: Uniqueness is based on location (file:line) + connection_id, not name.
    /// This matches DAP's constraint of one logpoint per line.
    async fn save(&self, metric: &Metric) -> Result<MetricId> {
        self.save_with_options(metric, false).await
    }

    /// Save a metric with explicit upsert control
    ///
    /// # Arguments
    /// * `metric` - The metric to save
    /// * `upsert` - If true, update on (location, connection_id) conflict; if false, fail on conflict
    async fn save_with_options(&self, metric: &Metric, upsert: bool) -> Result<MetricId>;

    /// Find metric by ID
    async fn find_by_id(&self, id: MetricId) -> Result<Option<Metric>>;

    /// Find metric by name
    async fn find_by_name(&self, name: &str) -> Result<Option<Metric>>;

    /// Find all metrics
    async fn find_all(&self) -> Result<Vec<Metric>>;

    /// Find metrics with pagination
    ///
    /// Returns a tuple of (metrics, total_count) for efficient pagination.
    /// Results are ordered by created_at DESC (newest first).
    ///
    /// # Arguments
    /// * `limit` - Maximum number of metrics to return
    /// * `offset` - Number of metrics to skip
    async fn find_paginated(&self, limit: usize, offset: usize) -> Result<(Vec<Metric>, u64)>;

    /// Count total number of metrics
    async fn count_all(&self) -> Result<u64>;

    /// Find metrics by group
    async fn find_by_group(&self, group: &str) -> Result<Vec<Metric>>;

    /// Find metrics by connection ID
    async fn find_by_connection_id(&self, connection_id: &ConnectionId) -> Result<Vec<Metric>>;

    /// Find metric by location (file:line) and connection ID
    /// Used to detect duplicate metrics at the same breakpoint location
    async fn find_by_location(
        &self,
        connection_id: &ConnectionId,
        file: &str,
        line: u32,
    ) -> Result<Option<Metric>>;

    /// Update existing metric
    async fn update(&self, metric: &Metric) -> Result<()>;

    /// Delete metric
    async fn delete(&self, id: MetricId) -> Result<()>;

    /// Check if metric name exists
    async fn exists_by_name(&self, name: &str) -> Result<bool>;

    /// Find metrics with filtering at database level
    ///
    /// Applies filters at the SQL level for efficient pagination with filters.
    /// Returns a tuple of (metrics, total_count) matching the filter.
    ///
    /// # Arguments
    /// * `filter` - Filter criteria (connection_id, enabled, group)
    /// * `limit` - Maximum number of metrics to return
    /// * `offset` - Number of metrics to skip
    async fn find_filtered(
        &self,
        filter: &MetricFilter,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Metric>, u64)>;

    /// Get group summaries with counts
    ///
    /// Returns aggregated group statistics using a single GROUP BY query.
    /// Much more efficient than loading all metrics and counting in memory.
    async fn get_group_summaries(&self) -> Result<Vec<GroupSummary>>;

    /// Delete all metrics for a connection.
    ///
    /// Used when a connection is explicitly deleted (not just disconnected)
    /// to prevent orphaned metrics.
    ///
    /// # Returns
    /// The number of metrics deleted.
    async fn delete_by_connection_id(&self, connection_id: &ConnectionId) -> Result<u64>;
}

/// Repository for metric events
#[async_trait]
pub trait EventRepository: Send + Sync {
    /// Save a new event
    async fn save(&self, event: &MetricEvent) -> Result<i64>;

    /// Save multiple events in batch
    async fn save_batch(&self, events: &[MetricEvent]) -> Result<Vec<i64>>;

    /// Find events by metric ID
    async fn find_by_metric_id(&self, metric_id: MetricId, limit: i64) -> Result<Vec<MetricEvent>>;

    /// Find events by multiple metric IDs (batch query - single DB call)
    async fn find_by_metric_ids(
        &self,
        metric_ids: &[MetricId],
        limit: i64,
    ) -> Result<Vec<MetricEvent>>;

    /// Find events by metric ID with time range
    async fn find_by_metric_id_and_time_range(
        &self,
        metric_id: MetricId,
        start_micros: i64,
        end_micros: i64,
        limit: i64,
    ) -> Result<Vec<MetricEvent>>;

    /// Count events for metric
    async fn count_by_metric_id(&self, metric_id: MetricId) -> Result<i64>;

    /// Count all events across all metrics
    async fn count_all(&self) -> Result<i64>;

    /// Find recent events (most recent first)
    async fn find_recent(&self, limit: i64) -> Result<Vec<MetricEvent>>;

    /// Delete old events (cleanup)
    async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64>;

    /// Delete events for metric beyond limit (ring buffer)
    async fn cleanup_metric_events(&self, metric_id: MetricId, keep_count: usize) -> Result<u64>;
}

/// Repository for connection entities
#[async_trait]
pub trait ConnectionRepository: Send + Sync {
    /// Save a new connection
    async fn save(&self, connection: &Connection) -> Result<ConnectionId>;

    /// Find connection by ID
    async fn find_by_id(&self, id: &ConnectionId) -> Result<Option<Connection>>;

    /// Find connection by address (host:port)
    async fn find_by_address(&self, host: &str, port: u16) -> Result<Option<Connection>>;

    /// Find all connections
    async fn list_all(&self) -> Result<Vec<Connection>>;

    /// Update connection
    async fn update(&self, connection: &Connection) -> Result<()>;

    /// Update connection status
    async fn update_status(&self, id: &ConnectionId, status: ConnectionStatus) -> Result<()>;

    /// Update last active timestamp
    async fn touch(&self, id: &ConnectionId) -> Result<()>;

    /// Delete connection
    async fn delete(&self, id: &ConnectionId) -> Result<()>;

    /// Check if connection exists
    async fn exists(&self, id: &ConnectionId) -> Result<bool>;

    /// Find active connections (Connected or Connecting status)
    async fn find_active(&self) -> Result<Vec<Connection>>;

    /// Find connections that should attempt auto-reconnect
    /// Returns connections with auto_reconnect=true and status in (Disconnected, Reconnecting, Failed)
    async fn find_for_reconnect(&self) -> Result<Vec<Connection>>;

    /// Find connections by language
    async fn find_by_language(&self, language: &str) -> Result<Vec<Connection>>;

    /// Delete all disconnected connections (cleanup stale entries)
    ///
    /// Removes connections with status: Disconnected, Failed
    /// Returns the number of deleted connections
    async fn delete_disconnected(&self) -> Result<u64>;
}

/// Repository for system events (crashes, connections, metric CRUD)
///
/// Used for:
/// - MCP client catch-up queries (reconnecting agents)
/// - Audit trail for critical events
/// - Debugging and observability
#[async_trait]
pub trait SystemEventRepository: Send + Sync {
    /// Save a new system event
    async fn save(&self, event: &SystemEvent) -> Result<i64>;

    /// Save multiple system events in batch
    async fn save_batch(&self, events: &[SystemEvent]) -> Result<Vec<i64>>;

    /// Find events since a timestamp (for catch-up queries)
    async fn find_since(&self, timestamp_micros: i64, limit: i64) -> Result<Vec<SystemEvent>>;

    /// Find events by type
    async fn find_by_type(
        &self,
        event_type: SystemEventType,
        limit: i64,
    ) -> Result<Vec<SystemEvent>>;

    /// Find events by connection ID
    async fn find_by_connection(
        &self,
        connection_id: &ConnectionId,
        limit: i64,
    ) -> Result<Vec<SystemEvent>>;

    /// Find unacknowledged events (for MCP clients reconnecting)
    async fn find_unacknowledged(&self, limit: i64) -> Result<Vec<SystemEvent>>;

    /// Mark events as acknowledged
    async fn acknowledge(&self, event_ids: &[i64]) -> Result<u64>;

    /// Find recent events (most recent first)
    async fn find_recent(&self, limit: i64) -> Result<Vec<SystemEvent>>;

    /// Delete old events (cleanup)
    async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64>;

    /// Delete oldest events, keeping only the most recent `max_events`
    ///
    /// Returns the number of events deleted. If max_events is 0, no deletion occurs.
    async fn delete_keeping_recent(&self, max_events: usize) -> Result<u64>;

    /// Count unacknowledged events
    async fn count_unacknowledged(&self) -> Result<i64>;

    /// Count all events
    async fn count_all(&self) -> Result<i64>;
}
