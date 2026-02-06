//! Mock repository implementations for testing
//!
//! - [`MockMetricRepository`] - In-memory metric storage
//! - [`MockEventRepository`] - In-memory event storage
//! - [`MockSystemEventRepository`] - In-memory system event storage
//! - [`MockConnectionRepository`] - In-memory connection storage
//! - [`MockDlqRepository`] - In-memory dead-letter queue storage

use async_trait::async_trait;
use detrix_core::system_event::{SystemEvent, SystemEventType};
use detrix_core::{
    Connection, ConnectionId, ConnectionStatus, Error, Metric, MetricEvent, MetricId, Result,
};
use detrix_ports::{
    ConnectionRepository, DlqEntry, DlqEntryStatus, DlqRepository, EventRepository,
    MetricRepository, SystemEventRepository,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;

// ============================================================================
// Mock Metric Repository
// ============================================================================

/// A mock metric repository for testing
///
/// Stores metrics in memory using a HashMap. Thread-safe via RwLock.
#[derive(Debug, Default)]
pub struct MockMetricRepository {
    metrics: RwLock<HashMap<MetricId, Metric>>,
    next_id: AtomicUsize,
}

impl MockMetricRepository {
    /// Create a new empty mock repository
    pub fn new() -> Self {
        Self {
            metrics: RwLock::new(HashMap::new()),
            next_id: AtomicUsize::new(1), // Start at 1 to match real DB behavior
        }
    }

    /// Get the number of metrics stored
    pub fn count(&self) -> usize {
        self.metrics.read().unwrap().len()
    }

    /// Clear all metrics
    pub fn clear(&self) {
        self.metrics.write().unwrap().clear();
    }
}

#[async_trait]
impl MetricRepository for MockMetricRepository {
    async fn save_with_options(&self, metric: &Metric, _upsert: bool) -> Result<MetricId> {
        let id = metric
            .id
            .unwrap_or_else(|| MetricId(self.next_id.fetch_add(1, Ordering::SeqCst) as u64));
        let mut metric = metric.clone();
        metric.id = Some(id);
        self.metrics.write().unwrap().insert(id, metric);
        Ok(id)
    }

    async fn find_by_id(&self, id: MetricId) -> Result<Option<Metric>> {
        Ok(self.metrics.read().unwrap().get(&id).cloned())
    }

    async fn find_by_name(&self, name: &str) -> Result<Option<Metric>> {
        Ok(self
            .metrics
            .read()
            .unwrap()
            .values()
            .find(|m| m.name == name)
            .cloned())
    }

    async fn find_all(&self) -> Result<Vec<Metric>> {
        Ok(self.metrics.read().unwrap().values().cloned().collect())
    }

    async fn find_paginated(&self, limit: usize, offset: usize) -> Result<(Vec<Metric>, u64)> {
        let metrics = self.metrics.read().unwrap();
        let total = metrics.len() as u64;
        let paginated: Vec<Metric> = metrics.values().skip(offset).take(limit).cloned().collect();
        Ok((paginated, total))
    }

    async fn count_all(&self) -> Result<u64> {
        Ok(self.metrics.read().unwrap().len() as u64)
    }

    async fn find_by_group(&self, group: &str) -> Result<Vec<Metric>> {
        Ok(self
            .metrics
            .read()
            .unwrap()
            .values()
            .filter(|m| m.group.as_deref() == Some(group))
            .cloned()
            .collect())
    }

    async fn update(&self, metric: &Metric) -> Result<()> {
        if let Some(id) = metric.id {
            self.metrics.write().unwrap().insert(id, metric.clone());
        }
        Ok(())
    }

    async fn delete(&self, id: MetricId) -> Result<()> {
        self.metrics.write().unwrap().remove(&id);
        Ok(())
    }

    async fn exists_by_name(&self, name: &str) -> Result<bool> {
        Ok(self
            .metrics
            .read()
            .unwrap()
            .values()
            .any(|m| m.name == name))
    }

    async fn find_by_connection_id(&self, connection_id: &ConnectionId) -> Result<Vec<Metric>> {
        Ok(self
            .metrics
            .read()
            .unwrap()
            .values()
            .filter(|m| m.connection_id == *connection_id)
            .cloned()
            .collect())
    }

    async fn find_by_location(
        &self,
        connection_id: &ConnectionId,
        file: &str,
        line: u32,
    ) -> Result<Option<Metric>> {
        Ok(self
            .metrics
            .read()
            .unwrap()
            .values()
            .find(|m| {
                m.connection_id == *connection_id
                    && m.location.file == file
                    && m.location.line == line
            })
            .cloned())
    }

    async fn find_filtered(
        &self,
        filter: &detrix_ports::MetricFilter,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Metric>, u64)> {
        let metrics = self.metrics.read().unwrap();

        // Apply filters
        let filtered: Vec<Metric> = metrics
            .values()
            .filter(|m| {
                if let Some(ref conn_id) = filter.connection_id {
                    if m.connection_id != *conn_id {
                        return false;
                    }
                }
                if let Some(enabled) = filter.enabled {
                    if m.enabled != enabled {
                        return false;
                    }
                }
                if let Some(ref group) = filter.group {
                    let metric_group = m.group.as_deref().unwrap_or("default");
                    if metric_group != group {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        let total = filtered.len() as u64;
        let paginated: Vec<Metric> = filtered.into_iter().skip(offset).take(limit).collect();
        Ok((paginated, total))
    }

    async fn get_group_summaries(&self) -> Result<Vec<detrix_ports::GroupSummary>> {
        let metrics = self.metrics.read().unwrap();

        // Group by group_name
        let mut groups: HashMap<Option<String>, (u64, u64)> = HashMap::new();
        for metric in metrics.values() {
            let entry = groups.entry(metric.group.clone()).or_insert((0, 0));
            entry.0 += 1; // total count
            if metric.enabled {
                entry.1 += 1; // enabled count
            }
        }

        let summaries: Vec<detrix_ports::GroupSummary> = groups
            .into_iter()
            .map(|(name, (total, enabled))| detrix_ports::GroupSummary {
                name,
                metric_count: total,
                enabled_count: enabled,
            })
            .collect();

        Ok(summaries)
    }

    async fn delete_by_connection_id(&self, connection_id: &ConnectionId) -> Result<u64> {
        let mut metrics = self.metrics.write().unwrap();
        let before = metrics.len();
        metrics.retain(|_, m| m.connection_id != *connection_id);
        Ok((before - metrics.len()) as u64)
    }
}

// ============================================================================
// Mock Event Repository
// ============================================================================

/// A mock event repository for testing
///
/// Stores events in memory. Thread-safe via RwLock.
#[derive(Debug, Default)]
pub struct MockEventRepository {
    events: RwLock<Vec<MetricEvent>>,
    next_id: AtomicUsize,
}

impl MockEventRepository {
    /// Create a new empty mock repository
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
            next_id: AtomicUsize::new(1),
        }
    }

    /// Get the number of events stored
    pub fn count(&self) -> usize {
        self.events.read().unwrap().len()
    }

    /// Clear all events
    pub fn clear(&self) {
        self.events.write().unwrap().clear();
    }

    /// Get all stored events
    pub fn all(&self) -> Vec<MetricEvent> {
        self.events.read().unwrap().clone()
    }
}

#[async_trait]
impl EventRepository for MockEventRepository {
    async fn save(&self, event: &MetricEvent) -> Result<i64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as i64;
        let mut event = event.clone();
        event.id = Some(id);
        self.events.write().unwrap().push(event);
        Ok(id)
    }

    async fn save_batch(&self, events: &[MetricEvent]) -> Result<Vec<i64>> {
        let mut ids = Vec::with_capacity(events.len());
        for event in events {
            let id = self.save(event).await?;
            ids.push(id);
        }
        Ok(ids)
    }

    async fn find_by_metric_id(&self, metric_id: MetricId, limit: i64) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().unwrap();
        let mut filtered: Vec<_> = events
            .iter()
            .filter(|e| e.metric_id == metric_id)
            .cloned()
            .collect();
        // Sort by timestamp descending to match real DB behavior
        filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(filtered.into_iter().take(limit as usize).collect())
    }

    async fn find_by_metric_id_since(
        &self,
        metric_id: MetricId,
        since_micros: i64,
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().unwrap();
        let mut filtered: Vec<_> = events
            .iter()
            .filter(|e| e.metric_id == metric_id && e.timestamp >= since_micros)
            .cloned()
            .collect();
        // Sort by timestamp descending to match real DB behavior
        filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(filtered.into_iter().take(limit as usize).collect())
    }

    async fn find_by_metric_ids(
        &self,
        metric_ids: &[MetricId],
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().unwrap();
        let mut filtered: Vec<_> = events
            .iter()
            .filter(|e| metric_ids.contains(&e.metric_id))
            .cloned()
            .collect();
        // Sort by timestamp descending to match real DB behavior
        filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(filtered.into_iter().take(limit as usize).collect())
    }

    async fn find_by_metric_id_and_time_range(
        &self,
        metric_id: MetricId,
        start_micros: i64,
        end_micros: i64,
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().unwrap();
        let mut filtered: Vec<_> = events
            .iter()
            .filter(|e| {
                e.metric_id == metric_id && e.timestamp >= start_micros && e.timestamp <= end_micros
            })
            .cloned()
            .collect();
        // Sort by timestamp descending to match real DB behavior
        filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(filtered.into_iter().take(limit as usize).collect())
    }

    async fn count_by_metric_id(&self, metric_id: MetricId) -> Result<i64> {
        let events = self.events.read().unwrap();
        Ok(events.iter().filter(|e| e.metric_id == metric_id).count() as i64)
    }

    async fn count_all(&self) -> Result<i64> {
        Ok(self.events.read().unwrap().len() as i64)
    }

    async fn find_recent(&self, limit: i64) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().unwrap();
        let mut sorted: Vec<_> = events.iter().cloned().collect();
        sorted.sort_by(|a, b| b.timestamp.cmp(&a.timestamp)); // desc by timestamp
        Ok(sorted.into_iter().take(limit as usize).collect())
    }

    async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64> {
        let mut events = self.events.write().unwrap();
        let before = events.len();
        events.retain(|e| e.timestamp >= timestamp_micros);
        Ok((before - events.len()) as u64)
    }

    async fn cleanup_metric_events(&self, metric_id: MetricId, keep_count: usize) -> Result<u64> {
        let mut events = self.events.write().unwrap();
        let before = events.len();

        // Get indices of events for this metric, sorted by timestamp desc
        let mut metric_events: Vec<_> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.metric_id == metric_id)
            .map(|(i, e)| (i, e.timestamp))
            .collect();
        metric_events.sort_by(|a, b| b.1.cmp(&a.1)); // desc by timestamp

        // Mark indices to remove (beyond keep_count)
        let to_remove: std::collections::HashSet<_> = metric_events
            .into_iter()
            .skip(keep_count)
            .map(|(i, _)| i)
            .collect();

        // Remove marked events
        let mut i = 0;
        events.retain(|_| {
            let keep = !to_remove.contains(&i);
            i += 1;
            keep
        });

        Ok((before - events.len()) as u64)
    }
}

// ============================================================================
// Mock System Event Repository
// ============================================================================

/// In-memory mock implementation of SystemEventRepository for testing
#[derive(Debug, Default)]
pub struct MockSystemEventRepository {
    events: RwLock<Vec<SystemEvent>>,
    next_id: AtomicUsize,
}

impl MockSystemEventRepository {
    /// Create a new empty mock repository
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
            next_id: AtomicUsize::new(1),
        }
    }

    /// Get count of stored events
    pub fn count(&self) -> usize {
        self.events.read().unwrap().len()
    }
}

#[async_trait]
impl SystemEventRepository for MockSystemEventRepository {
    async fn save(&self, event: &SystemEvent) -> Result<i64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as i64;
        let mut event = event.clone();
        event.id = Some(id);
        self.events.write().unwrap().push(event);
        Ok(id)
    }

    async fn save_batch(&self, events: &[SystemEvent]) -> Result<Vec<i64>> {
        let mut ids = Vec::with_capacity(events.len());
        for event in events {
            ids.push(self.save(event).await?);
        }
        Ok(ids)
    }

    async fn find_since(&self, timestamp_micros: i64, limit: i64) -> Result<Vec<SystemEvent>> {
        let events = self.events.read().unwrap();
        Ok(events
            .iter()
            .filter(|e| e.timestamp >= timestamp_micros)
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn find_by_type(
        &self,
        event_type: SystemEventType,
        limit: i64,
    ) -> Result<Vec<SystemEvent>> {
        let events = self.events.read().unwrap();
        Ok(events
            .iter()
            .filter(|e| e.event_type == event_type)
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn find_by_connection(
        &self,
        connection_id: &ConnectionId,
        limit: i64,
    ) -> Result<Vec<SystemEvent>> {
        let events = self.events.read().unwrap();
        Ok(events
            .iter()
            .filter(|e| e.connection_id.as_ref() == Some(connection_id))
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn find_unacknowledged(&self, limit: i64) -> Result<Vec<SystemEvent>> {
        let events = self.events.read().unwrap();
        Ok(events
            .iter()
            .filter(|e| !e.acknowledged)
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn acknowledge(&self, event_ids: &[i64]) -> Result<u64> {
        let mut events = self.events.write().unwrap();
        let mut count = 0u64;
        for event in events.iter_mut() {
            if let Some(id) = event.id {
                if event_ids.contains(&id) && !event.acknowledged {
                    event.acknowledged = true;
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    async fn find_recent(&self, limit: i64) -> Result<Vec<SystemEvent>> {
        let events = self.events.read().unwrap();
        let mut sorted: Vec<_> = events.iter().cloned().collect();
        sorted.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        Ok(sorted.into_iter().take(limit as usize).collect())
    }

    async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64> {
        let mut events = self.events.write().unwrap();
        let before = events.len();
        events.retain(|e| e.timestamp >= timestamp_micros);
        Ok((before - events.len()) as u64)
    }

    async fn delete_keeping_recent(&self, max_events: usize) -> Result<u64> {
        if max_events == 0 {
            return Ok(0);
        }

        let mut events = self.events.write().unwrap();
        if events.len() <= max_events {
            return Ok(0);
        }

        // Sort by timestamp descending, keep only most recent
        events.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        let to_delete = events.len() - max_events;
        events.truncate(max_events);
        Ok(to_delete as u64)
    }

    async fn count_unacknowledged(&self) -> Result<i64> {
        let events = self.events.read().unwrap();
        Ok(events.iter().filter(|e| !e.acknowledged).count() as i64)
    }

    async fn count_all(&self) -> Result<i64> {
        Ok(self.events.read().unwrap().len() as i64)
    }
}

// ============================================================================
// Mock Connection Repository
// ============================================================================

/// In-memory mock implementation of ConnectionRepository for testing
#[derive(Debug, Clone, Default)]
pub struct MockConnectionRepository {
    connections: Arc<Mutex<HashMap<ConnectionId, Connection>>>,
}

impl MockConnectionRepository {
    /// Create a new empty mock repository
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a connection by ID (test helper)
    pub async fn get_connection(&self, id: &ConnectionId) -> Option<Connection> {
        self.connections.lock().await.get(id).cloned()
    }

    /// Get count of stored connections
    pub async fn connection_count(&self) -> usize {
        self.connections.lock().await.len()
    }
}

#[async_trait]
impl ConnectionRepository for MockConnectionRepository {
    async fn save(&self, connection: &Connection) -> Result<ConnectionId> {
        let mut connections = self.connections.lock().await;
        connections.insert(connection.id.clone(), connection.clone());
        Ok(connection.id.clone())
    }

    async fn find_by_id(&self, id: &ConnectionId) -> Result<Option<Connection>> {
        let connections = self.connections.lock().await;
        Ok(connections.get(id).cloned())
    }

    async fn find_by_address(&self, host: &str, port: u16) -> Result<Option<Connection>> {
        let connections = self.connections.lock().await;
        Ok(connections
            .values()
            .find(|c| c.host == host && c.port == port)
            .cloned())
    }

    async fn find_by_identity(
        &self,
        name: &str,
        language: &str,
        workspace_root: &str,
        hostname: &str,
    ) -> Result<Option<Connection>> {
        let connections = self.connections.lock().await;
        Ok(connections
            .values()
            .find(|c| {
                c.name.as_deref() == Some(name)
                    && c.language.as_str() == language
                    && c.workspace_root == workspace_root
                    && c.hostname == hostname
            })
            .cloned())
    }

    async fn list_all(&self) -> Result<Vec<Connection>> {
        let connections = self.connections.lock().await;
        Ok(connections.values().cloned().collect())
    }

    async fn update(&self, connection: &Connection) -> Result<()> {
        let mut connections = self.connections.lock().await;
        connections.insert(connection.id.clone(), connection.clone());
        Ok(())
    }

    async fn update_status(&self, id: &ConnectionId, status: ConnectionStatus) -> Result<()> {
        let mut connections = self.connections.lock().await;
        if let Some(conn) = connections.get_mut(id) {
            conn.status = status;
            Ok(())
        } else {
            Err(Error::Database(format!("Connection {} not found", id.0)))
        }
    }

    async fn touch(&self, id: &ConnectionId) -> Result<()> {
        let mut connections = self.connections.lock().await;
        if let Some(conn) = connections.get_mut(id) {
            conn.touch();
            Ok(())
        } else {
            Err(Error::Database(format!("Connection {} not found", id.0)))
        }
    }

    async fn delete(&self, id: &ConnectionId) -> Result<()> {
        let mut connections = self.connections.lock().await;
        connections.remove(id);
        Ok(())
    }

    async fn exists(&self, id: &ConnectionId) -> Result<bool> {
        let connections = self.connections.lock().await;
        Ok(connections.contains_key(id))
    }

    async fn find_active(&self) -> Result<Vec<Connection>> {
        let connections = self.connections.lock().await;
        Ok(connections
            .values()
            .filter(|c| c.is_active())
            .cloned()
            .collect())
    }

    async fn find_for_reconnect(&self) -> Result<Vec<Connection>> {
        let connections = self.connections.lock().await;
        Ok(connections
            .values()
            .filter(|c| c.should_reconnect())
            .cloned()
            .collect())
    }

    async fn find_by_language(&self, language: &str) -> Result<Vec<Connection>> {
        let connections = self.connections.lock().await;
        Ok(connections
            .values()
            .filter(|c| c.language.as_str() == language)
            .cloned()
            .collect())
    }

    async fn delete_disconnected(&self) -> Result<u64> {
        let mut connections = self.connections.lock().await;
        let initial_count = connections.len();
        connections.retain(|_, c| {
            !matches!(
                c.status,
                ConnectionStatus::Disconnected | ConnectionStatus::Failed(_)
            )
        });
        Ok((initial_count - connections.len()) as u64)
    }
}

// ============================================================================
// Mock DLQ Repository
// ============================================================================

/// In-memory mock implementation of DlqRepository for testing
#[derive(Debug)]
pub struct MockDlqRepository {
    entries: Mutex<Vec<DlqEntry>>,
    next_id: AtomicU64,
    /// If true, all save operations will fail (for testing error handling)
    should_fail: AtomicBool,
}

impl Default for MockDlqRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl MockDlqRepository {
    /// Create a new empty mock repository
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            should_fail: AtomicBool::new(false),
        }
    }

    /// Create a mock repository that fails all save operations
    pub fn failing() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            should_fail: AtomicBool::new(true),
        }
    }

    /// Add an entry directly (test helper)
    pub async fn add_entry(&self, event_json: String, error_message: String) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as i64;
        let entry = DlqEntry {
            id,
            event_json,
            error_message,
            retry_count: 0,
            status: DlqEntryStatus::Pending,
            created_at: chrono::Utc::now().timestamp_micros(),
            last_retry_at: None,
        };
        self.entries.lock().await.push(entry);
        id
    }

    /// Get count of stored entries
    pub async fn count(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// Get all entries (test helper)
    pub async fn get_entries(&self) -> Vec<(String, String)> {
        self.entries
            .lock()
            .await
            .iter()
            .map(|e| (e.event_json.clone(), e.error_message.clone()))
            .collect()
    }

    /// Push a complete DlqEntry directly (test helper for specific test scenarios)
    pub async fn push_entry(&self, entry: DlqEntry) {
        self.entries.lock().await.push(entry);
    }

    /// Get an entry by ID (test helper)
    pub async fn get_entry(&self, id: i64) -> Option<DlqEntry> {
        self.entries
            .lock()
            .await
            .iter()
            .find(|e| e.id == id)
            .cloned()
    }

    /// Get all DlqEntry objects (test helper for inspection)
    pub async fn all_entries(&self) -> Vec<DlqEntry> {
        self.entries.lock().await.clone()
    }
}

#[async_trait]
impl DlqRepository for MockDlqRepository {
    async fn save(&self, event_json: String, error_message: String) -> Result<i64> {
        if self.should_fail.load(Ordering::SeqCst) {
            return Err(detrix_core::Error::Database("DLQ save failed".to_string()));
        }
        Ok(self.add_entry(event_json, error_message).await)
    }

    async fn save_batch(&self, events: Vec<(String, String)>) -> Result<Vec<i64>> {
        if self.should_fail.load(Ordering::SeqCst) {
            return Err(detrix_core::Error::Database(
                "DLQ batch save failed".to_string(),
            ));
        }
        let mut ids = Vec::new();
        for (json, err) in events {
            ids.push(self.add_entry(json, err).await);
        }
        Ok(ids)
    }

    async fn find_pending(&self, limit: usize) -> Result<Vec<DlqEntry>> {
        let entries = self.entries.lock().await;
        Ok(entries
            .iter()
            .filter(|e| e.status == DlqEntryStatus::Pending)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn mark_retrying(&self, id: i64) -> Result<()> {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.status = DlqEntryStatus::Retrying;
        }
        Ok(())
    }

    async fn mark_retrying_batch(&self, ids: &[i64]) -> Result<()> {
        let mut entries = self.entries.lock().await;
        for entry in entries.iter_mut() {
            if ids.contains(&entry.id) {
                entry.status = DlqEntryStatus::Retrying;
            }
        }
        Ok(())
    }

    async fn increment_retry(&self, id: i64) -> Result<()> {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.retry_count += 1;
            entry.status = DlqEntryStatus::Pending;
        }
        Ok(())
    }

    async fn increment_retry_batch(&self, ids: &[i64]) -> Result<()> {
        let mut entries = self.entries.lock().await;
        for entry in entries.iter_mut() {
            if ids.contains(&entry.id) {
                entry.retry_count += 1;
                entry.status = DlqEntryStatus::Pending;
            }
        }
        Ok(())
    }

    async fn mark_failed(&self, id: i64) -> Result<()> {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            entry.status = DlqEntryStatus::Failed;
        }
        Ok(())
    }

    async fn mark_failed_batch(&self, ids: &[i64]) -> Result<()> {
        let mut entries = self.entries.lock().await;
        for entry in entries.iter_mut() {
            if ids.contains(&entry.id) {
                entry.status = DlqEntryStatus::Failed;
            }
        }
        Ok(())
    }

    async fn delete(&self, id: i64) -> Result<()> {
        let mut entries = self.entries.lock().await;
        entries.retain(|e| e.id != id);
        Ok(())
    }

    async fn delete_batch(&self, ids: &[i64]) -> Result<()> {
        let mut entries = self.entries.lock().await;
        entries.retain(|e| !ids.contains(&e.id));
        Ok(())
    }

    async fn count_by_status(&self, status: DlqEntryStatus) -> Result<u64> {
        let entries = self.entries.lock().await;
        Ok(entries.iter().filter(|e| e.status == status).count() as u64)
    }

    async fn count_all(&self) -> Result<u64> {
        Ok(self.entries.lock().await.len() as u64)
    }

    async fn delete_old_failed(&self, _older_than_micros: i64) -> Result<u64> {
        let mut entries = self.entries.lock().await;
        let before = entries.len();
        entries.retain(|e| e.status != DlqEntryStatus::Failed);
        Ok((before - entries.len()) as u64)
    }
}
