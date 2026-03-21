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

/// Explicit ownership filter for location lookups.
///
/// Replaces the ambiguous `Option<&str>` in `find_by_location`:
/// - `User(uid)` → match metrics owned by this user
/// - `System` → match system metrics (maps to `SYSTEM_USER_ID` sentinel in storage)
#[derive(Debug, Clone)]
pub enum OwnerFilter<'a> {
    /// Match metrics owned by this specific user.
    User(&'a str),
    /// Match system metrics (no explicit owner).
    System,
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
    /// Filter by user ID (None = return all users, Some(uid) = only that user's metrics)
    pub user_id: Option<String>,
    /// Optional agent_id filter for multi-tenant queries (future use).
    pub agent_id: Option<String>,
}

/// Repository for metric entities
#[async_trait]
pub trait MetricRepository: Send + Sync {
    /// Save a new metric
    ///
    /// If `upsert` is false (default), fails if metric with same (location, connection_id, user_id) exists.
    /// If `upsert` is true, updates existing metric with same (location, connection_id, user_id).
    ///
    /// Note: Uniqueness is based on (location, connection_id, user_id).
    /// Each user can have their own metric at the same location; `NULL` user_ids are
    /// each treated as distinct in the SQLite UNIQUE index.
    async fn save(&self, metric: &Metric) -> Result<MetricId> {
        self.save_with_options(metric, false).await
    }

    /// Save a metric with explicit upsert control
    ///
    /// # Arguments
    /// * `metric` - The metric to save
    /// * `upsert` - If true, update on (location, connection_id, user_id) conflict; if false, fail on conflict
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

    /// Find metric by location (file:line), connection ID, and ownership.
    ///
    /// Used to detect duplicate metrics at the same breakpoint location for a specific user.
    /// In multi-tenant mode, each user owns their own metric at a location.
    async fn find_by_location(
        &self,
        connection_id: &ConnectionId,
        file: &str,
        line: u32,
        owner: OwnerFilter<'_>,
    ) -> Result<Option<Metric>>;

    /// Find ALL metrics at a location across all users
    ///
    /// Used by logpoint merging: DAP has one logpoint per (file, line, connection_id),
    /// so we need to union expressions from all users' metrics at that location.
    async fn find_all_at_location(
        &self,
        connection_id: &ConnectionId,
        file: &str,
        line: u32,
    ) -> Result<Vec<Metric>>;

    /// Update existing metric
    ///
    /// Note: `user_id` and `agent_id` are immutable after creation —
    /// `update()` preserves the original values stored at save time.
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
    /// * `filter` - Filter criteria (connection_id, enabled, group, user_id)
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

    /// Get group summaries filtered by user ID.
    ///
    /// Uses SQL `GROUP BY … WHERE user_id = ?` for efficient tenant-scoped aggregation.
    /// Falls back to `get_group_summaries` when not implemented.
    async fn get_group_summaries_by_user(&self, user_id: &str) -> Result<Vec<GroupSummary>> {
        // Default implementation: load all and filter in memory (overridden by SQLite)
        let _ = user_id;
        self.get_group_summaries().await
    }

    /// Delete all metrics for a connection.
    ///
    /// Used when a connection is explicitly deleted (not just disconnected)
    /// to prevent orphaned metrics.
    ///
    /// # Returns
    /// The number of metrics deleted.
    async fn delete_by_connection_id(&self, connection_id: &ConnectionId) -> Result<u64>;

    /// Migrate all metrics from one connection to another.
    ///
    /// Used when a container restarts with a new hostname (new `connection_id`) but the same
    /// project identity. Metrics from the old connection carry over to the new one so debugging
    /// can continue without re-adding them.
    ///
    /// Metrics that would conflict with an existing metric at the same location on the target
    /// connection are skipped (keeping the target's version).
    ///
    /// # Returns
    /// The number of metrics migrated.
    async fn migrate_connection_id(&self, from: &ConnectionId, to: &ConnectionId) -> Result<u64>;
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

    /// Find events by metric ID with timestamp filter (events since given timestamp)
    ///
    /// # Arguments
    /// * `metric_id` - The metric to query events for
    /// * `since_micros` - Only return events with timestamp >= this value (microseconds since epoch)
    /// * `limit` - Maximum number of events to return
    async fn find_by_metric_id_since(
        &self,
        metric_id: MetricId,
        since_micros: i64,
        limit: i64,
    ) -> Result<Vec<MetricEvent>>;

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

    /// Find connection by ID (UUID)
    async fn find_by_id(&self, id: &ConnectionId) -> Result<Option<Connection>>;

    /// Find connection by identity components (for process restart detection)
    async fn find_by_identity(
        &self,
        name: &str,
        language: &str,
        workspace_root: &str,
        hostname: &str,
    ) -> Result<Option<Connection>>;

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

    /// Find stale connections for the same project, excluding a specific connection.
    ///
    /// When a new connection registers for (name, language, workspace_root) with a different
    /// hostname (e.g. Docker container restart with new container ID), old disconnected/failed
    /// connections with the same project identity are stale. This method returns their IDs so
    /// the caller can migrate metrics before cleaning up the stale connections.
    ///
    /// Only returns connections with status: Disconnected or Failed.
    /// Connected/Connecting connections are excluded to avoid race conditions.
    ///
    /// # Arguments
    /// * `name` - Connection name (app name)
    /// * `language` - Source language (e.g. "go", "python")
    /// * `workspace_root` - Workspace root path
    /// * `exclude_id` - The newly registered connection to preserve
    ///
    /// # Returns
    /// IDs of stale connections that should be migrated and cleaned up
    async fn find_stale_same_project(
        &self,
        name: &str,
        language: &str,
        workspace_root: &str,
        exclude_id: &ConnectionId,
    ) -> Result<Vec<ConnectionId>>;
}

/// Minimal read-only lookup for connections.
///
/// A narrow trait extracted from `ConnectionService` so that `RemoteAppService`
/// depends on an abstraction rather than a concrete service type.
#[async_trait]
pub trait ConnectionLookup: Send + Sync {
    /// Retrieve a connection by its UUID, or None if not found.
    async fn get_connection(&self, id: &ConnectionId) -> Result<Option<Connection>>;
}

/// Repository for connection reference counting (multi-user safety)
///
/// Manages references that clients hold on connections. A connection should
/// only be disconnected when its last reference is removed.
///
/// Key atomic methods:
/// - `remove_reference_and_count`: atomically removes a reference and returns remaining count
/// - `remove_all_by_client_and_count`: atomically removes all client refs and returns affected counts
#[async_trait]
pub trait ConnectionReferenceRepository: Send + Sync {
    /// Upsert reference (if same client+connection exists, touch it)
    async fn add_reference(&self, reference: &detrix_core::ConnectionReference) -> Result<()>;

    /// Atomically remove reference and return remaining count.
    /// Wraps DELETE + SELECT COUNT in a transaction.
    /// Returns `(was_removed, remaining_count)`.
    async fn remove_reference_and_count(
        &self,
        connection_id: &ConnectionId,
        client_identity: &detrix_core::ClientIdentity,
    ) -> Result<(bool, u64)>;

    /// Atomically remove ALL references by client, return vec of (connection_id, remaining_count).
    /// Connections not in the result have 0 remaining references.
    async fn remove_all_by_client_and_count(
        &self,
        client_identity: &detrix_core::ClientIdentity,
    ) -> Result<Vec<(ConnectionId, u64)>>;

    /// Remove ALL references for a connection (for admin/cascade)
    async fn remove_all_by_connection(&self, connection_id: &ConnectionId) -> Result<u64>;

    /// Count references for a connection
    async fn count_references(&self, connection_id: &ConnectionId) -> Result<u64>;

    /// List references for a connection
    async fn find_by_connection(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<Vec<detrix_core::ConnectionReference>>;

    /// List references held by a client
    async fn find_by_client(
        &self,
        client_identity: &detrix_core::ClientIdentity,
    ) -> Result<Vec<detrix_core::ConnectionReference>>;

    /// Check if client holds a reference
    async fn has_reference(
        &self,
        connection_id: &ConnectionId,
        client_identity: &detrix_core::ClientIdentity,
    ) -> Result<bool>;

    /// Remove references inactive > N calendar days
    async fn cleanup_stale_references(&self, ttl_days: i64) -> Result<u64>;

    /// Touch all references by client (update last_active)
    async fn touch_all_by_client(
        &self,
        client_identity: &detrix_core::ClientIdentity,
    ) -> Result<u64>;
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
