//! Tests for AdapterLifecycleManager
//!
//! Following TDD approach - tests define expected behavior

use super::adapter_lifecycle_manager::{AdapterLifecycleManager, ManagedAdapterStatus};
use crate::ports::{
    ConnectionRepository, ConnectionRepositoryRef, DapAdapter, DapAdapterFactory,
    DapAdapterFactoryRef, DapAdapterRef, EventRepository, MetricRepository, MetricRepositoryRef,
    RemoveMetricResult, SetMetricResult,
};
use crate::services::EventCaptureService;
use async_trait::async_trait;
use detrix_config::constants::DEFAULT_EVENT_FLUSH_INTERVAL_MS;
use detrix_core::{
    Connection, ConnectionId, ConnectionStatus, Metric, MetricEvent, MetricId, Result,
    SourceLanguage, SystemEvent,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

/// Delay to wait for events to be flushed (20% longer than flush interval)
const FLUSH_WAIT_MS: u64 = (DEFAULT_EVENT_FLUSH_INTERVAL_MS as f64 * 1.2) as u64;

// =============================================================================
// Mock Implementations
// =============================================================================

/// Mock DAP adapter for testing
pub struct MockDapAdapter {
    /// Whether the adapter is "connected"
    connected: AtomicBool,
    /// Whether start() was called
    started: AtomicBool,
    /// Whether stop() was called
    stopped: AtomicBool,
    /// Event sender - used to simulate events from debugpy
    event_tx: Mutex<Option<mpsc::Sender<MetricEvent>>>,
    /// How many times subscribe_events was called
    subscribe_count: AtomicUsize,
}

impl MockDapAdapter {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            event_tx: Mutex::new(None),
            subscribe_count: AtomicUsize::new(0),
        }
    }

    /// Send a test event (simulates debugpy output)
    pub async fn send_event(&self, event: MetricEvent) {
        if let Some(tx) = self.event_tx.lock().await.as_ref() {
            let _ = tx.try_send(event);
        }
    }

    /// Check if start was called
    pub fn was_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Check if stop was called
    pub fn was_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

impl Default for MockDapAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DapAdapter for MockDapAdapter {
    async fn start(&self) -> Result<()> {
        self.started.store(true, Ordering::SeqCst);
        self.connected.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.stopped.store(true, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn ensure_connected(&self) -> Result<()> {
        if self.connected.load(Ordering::SeqCst) {
            Ok(())
        } else {
            self.start().await
        }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    async fn set_metric(&self, _metric: &Metric) -> Result<SetMetricResult> {
        Ok(SetMetricResult {
            verified: true,
            line: 1,
            message: None,
        })
    }

    async fn remove_metric(&self, _metric: &Metric) -> Result<RemoveMetricResult> {
        Ok(RemoveMetricResult::success())
    }

    async fn subscribe_events(&self) -> Result<mpsc::Receiver<MetricEvent>> {
        self.subscribe_count.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel(1000);
        *self.event_tx.lock().await = Some(tx);
        Ok(rx)
    }
}

/// Mock adapter factory for testing
pub struct MockDapAdapterFactory {
    /// Pre-configured adapters to return
    adapters: RwLock<HashMap<String, Arc<MockDapAdapter>>>,
    /// Default adapter to return if no specific one is configured
    default_adapter: Option<Arc<MockDapAdapter>>,
}

impl MockDapAdapterFactory {
    pub fn new() -> Self {
        Self {
            adapters: RwLock::new(HashMap::new()),
            default_adapter: None,
        }
    }

    pub fn with_default_adapter(adapter: Arc<MockDapAdapter>) -> Self {
        Self {
            adapters: RwLock::new(HashMap::new()),
            default_adapter: Some(adapter),
        }
    }
}

impl Default for MockDapAdapterFactory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DapAdapterFactory for MockDapAdapterFactory {
    async fn create_python_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef> {
        let key = format!("{}:{}", host, port);
        let adapters = self.adapters.read().await;

        if let Some(adapter) = adapters.get(&key) {
            Ok(Arc::clone(adapter) as DapAdapterRef)
        } else if let Some(ref default) = self.default_adapter {
            Ok(Arc::clone(default) as DapAdapterRef)
        } else {
            // Return a new mock adapter
            Ok(Arc::new(MockDapAdapter::new()) as DapAdapterRef)
        }
    }

    async fn create_go_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef> {
        // Reuse the same logic as Python adapter for mocking
        self.create_python_adapter(host, port).await
    }

    async fn create_rust_adapter(
        &self,
        host: &str,
        port: u16,
        _program: Option<&str>,
        _pid: Option<u32>,
    ) -> Result<DapAdapterRef> {
        // Reuse the same logic as Python adapter for mocking
        self.create_python_adapter(host, port).await
    }
}

/// Mock event repository for testing
pub struct MockEventRepository {
    events: RwLock<Vec<MetricEvent>>,
}

impl MockEventRepository {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }

    pub async fn get_events(&self) -> Vec<MetricEvent> {
        self.events.read().await.clone()
    }
}

impl Default for MockEventRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventRepository for MockEventRepository {
    async fn save(&self, event: &MetricEvent) -> Result<i64> {
        let mut events = self.events.write().await;
        events.push(event.clone());
        Ok(events.len() as i64)
    }

    async fn save_batch(&self, events: &[MetricEvent]) -> Result<Vec<i64>> {
        let mut stored = self.events.write().await;
        let start_id = stored.len() as i64 + 1;
        let ids: Vec<i64> = (0..events.len()).map(|i| start_id + i as i64).collect();
        stored.extend(events.iter().cloned());
        Ok(ids)
    }

    async fn find_by_metric_id(&self, metric_id: MetricId, limit: i64) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().await;
        Ok(events
            .iter()
            .filter(|e| e.metric_id == metric_id)
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn find_by_metric_id_since(
        &self,
        metric_id: MetricId,
        since_micros: i64,
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().await;
        Ok(events
            .iter()
            .filter(|e| e.metric_id == metric_id && e.timestamp >= since_micros)
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn find_by_metric_ids(
        &self,
        metric_ids: &[MetricId],
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().await;
        Ok(events
            .iter()
            .filter(|e| metric_ids.contains(&e.metric_id))
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn find_by_metric_id_and_time_range(
        &self,
        metric_id: MetricId,
        start_micros: i64,
        end_micros: i64,
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().await;
        Ok(events
            .iter()
            .filter(|e| {
                e.metric_id == metric_id && e.timestamp >= start_micros && e.timestamp <= end_micros
            })
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn count_by_metric_id(&self, metric_id: MetricId) -> Result<i64> {
        let events = self.events.read().await;
        Ok(events.iter().filter(|e| e.metric_id == metric_id).count() as i64)
    }

    async fn count_all(&self) -> Result<i64> {
        Ok(self.events.read().await.len() as i64)
    }

    async fn find_recent(&self, limit: i64) -> Result<Vec<MetricEvent>> {
        let events = self.events.read().await;
        Ok(events.iter().rev().take(limit as usize).cloned().collect())
    }

    async fn delete_older_than(&self, timestamp_micros: i64) -> Result<u64> {
        let mut events = self.events.write().await;
        let before = events.len();
        events.retain(|e| e.timestamp >= timestamp_micros);
        Ok((before - events.len()) as u64)
    }

    async fn cleanup_metric_events(&self, metric_id: MetricId, keep_count: usize) -> Result<u64> {
        let mut events = self.events.write().await;
        let metric_events: Vec<_> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.metric_id == metric_id)
            .map(|(i, _)| i)
            .collect();

        if metric_events.len() <= keep_count {
            return Ok(0);
        }

        let to_remove = metric_events.len() - keep_count;
        let indices_to_remove: Vec<_> = metric_events.into_iter().take(to_remove).collect();

        // Remove in reverse order to avoid index shifting issues
        for i in indices_to_remove.into_iter().rev() {
            events.remove(i);
        }

        Ok(to_remove as u64)
    }
}

/// Mock metric repository for testing (always returns empty - no pre-existing metrics)
pub struct MockMetricRepository;

impl MockMetricRepository {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockMetricRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MetricRepository for MockMetricRepository {
    async fn save_with_options(&self, _metric: &Metric, _upsert: bool) -> Result<MetricId> {
        Ok(MetricId(1))
    }

    async fn find_by_id(&self, _id: MetricId) -> Result<Option<Metric>> {
        Ok(None)
    }

    async fn find_by_name(&self, _name: &str) -> Result<Option<Metric>> {
        Ok(None)
    }

    async fn find_all(&self) -> Result<Vec<Metric>> {
        Ok(Vec::new())
    }

    async fn find_paginated(&self, _limit: usize, _offset: usize) -> Result<(Vec<Metric>, u64)> {
        Ok((Vec::new(), 0))
    }

    async fn count_all(&self) -> Result<u64> {
        Ok(0)
    }

    async fn find_by_group(&self, _group: &str) -> Result<Vec<Metric>> {
        Ok(Vec::new())
    }

    async fn find_by_connection_id(&self, _connection_id: &ConnectionId) -> Result<Vec<Metric>> {
        Ok(Vec::new())
    }

    async fn update(&self, _metric: &Metric) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, _id: MetricId) -> Result<()> {
        Ok(())
    }

    async fn exists_by_name(&self, _name: &str) -> Result<bool> {
        Ok(false)
    }

    async fn find_by_location(
        &self,
        _connection_id: &ConnectionId,
        _file: &str,
        _line: u32,
    ) -> Result<Option<Metric>> {
        Ok(None)
    }

    async fn find_filtered(
        &self,
        _filter: &crate::ports::MetricFilter,
        _limit: usize,
        _offset: usize,
    ) -> Result<(Vec<Metric>, u64)> {
        Ok((Vec::new(), 0))
    }

    async fn get_group_summaries(&self) -> Result<Vec<crate::ports::GroupSummary>> {
        Ok(Vec::new())
    }

    async fn delete_by_connection_id(&self, _connection_id: &ConnectionId) -> Result<u64> {
        Ok(0)
    }
}

/// Mock connection repository for testing - minimal implementation
pub struct MockConnectionRepository {
    connections: Mutex<HashMap<ConnectionId, Connection>>,
}

impl MockConnectionRepository {
    pub fn new() -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for MockConnectionRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ConnectionRepository for MockConnectionRepository {
    async fn save(&self, connection: &Connection) -> Result<ConnectionId> {
        let mut conns = self.connections.lock().await;
        conns.insert(connection.id.clone(), connection.clone());
        Ok(connection.id.clone())
    }

    async fn find_by_id(&self, id: &ConnectionId) -> Result<Option<Connection>> {
        let conns = self.connections.lock().await;
        Ok(conns.get(id).cloned())
    }

    async fn find_by_address(&self, _host: &str, _port: u16) -> Result<Option<Connection>> {
        Ok(None)
    }

    async fn list_all(&self) -> Result<Vec<Connection>> {
        let conns = self.connections.lock().await;
        Ok(conns.values().cloned().collect())
    }

    async fn update(&self, connection: &Connection) -> Result<()> {
        let mut conns = self.connections.lock().await;
        conns.insert(connection.id.clone(), connection.clone());
        Ok(())
    }

    async fn update_status(&self, id: &ConnectionId, status: ConnectionStatus) -> Result<()> {
        let mut conns = self.connections.lock().await;
        if let Some(conn) = conns.get_mut(id) {
            conn.status = status;
        }
        Ok(())
    }

    async fn touch(&self, _id: &ConnectionId) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, id: &ConnectionId) -> Result<()> {
        let mut conns = self.connections.lock().await;
        conns.remove(id);
        Ok(())
    }

    async fn find_active(&self) -> Result<Vec<Connection>> {
        let conns = self.connections.lock().await;
        Ok(conns
            .values()
            .filter(|c| matches!(c.status, ConnectionStatus::Connected))
            .cloned()
            .collect())
    }

    async fn find_for_reconnect(&self) -> Result<Vec<Connection>> {
        Ok(Vec::new())
    }

    async fn exists(&self, id: &ConnectionId) -> Result<bool> {
        Ok(self.connections.lock().await.contains_key(id))
    }

    async fn find_by_language(&self, _language: &str) -> Result<Vec<Connection>> {
        Ok(Vec::new())
    }

    async fn delete_disconnected(&self) -> Result<u64> {
        let mut conns = self.connections.lock().await;
        let initial_count = conns.len();
        conns.retain(|_, c| {
            !matches!(
                c.status,
                ConnectionStatus::Disconnected | ConnectionStatus::Failed(_)
            )
        });
        Ok((initial_count - conns.len()) as u64)
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

fn create_test_event(metric_id: u64, value: &str) -> MetricEvent {
    MetricEvent {
        id: None,
        metric_id: MetricId(metric_id),
        metric_name: format!("test_metric_{}", metric_id),
        connection_id: ConnectionId::new("test-conn"),
        timestamp: chrono::Utc::now().timestamp_micros(),
        thread_name: None,
        thread_id: None,
        value_json: format!("{{\"value\": \"{}\"}}", value),
        value_numeric: None,
        value_string: Some(value.to_string()),
        value_boolean: None,
        is_error: false,
        error_type: None,
        error_message: None,
        request_id: None,
        session_id: None,
        stack_trace: None,
        memory_snapshot: None,
    }
}

async fn create_test_manager() -> (
    AdapterLifecycleManager,
    Arc<MockEventRepository>,
    Arc<MockDapAdapterFactory>,
) {
    let event_repo = Arc::new(MockEventRepository::new());
    let event_capture = Arc::new(EventCaptureService::new(
        Arc::clone(&event_repo) as Arc<dyn EventRepository + Send + Sync>
    ));
    let (broadcast_tx, _) = broadcast::channel::<MetricEvent>(100);
    let (system_event_tx, _) = broadcast::channel::<SystemEvent>(100);
    let adapter_factory = Arc::new(MockDapAdapterFactory::new());
    let metric_repo = Arc::new(MockMetricRepository::new());
    let connection_repo = Arc::new(MockConnectionRepository::new());

    let manager = AdapterLifecycleManager::new(
        event_capture,
        broadcast_tx,
        system_event_tx,
        Arc::clone(&adapter_factory) as DapAdapterFactoryRef,
        metric_repo as MetricRepositoryRef,
        connection_repo as ConnectionRepositoryRef,
    );

    (manager, event_repo, adapter_factory)
}

async fn create_test_manager_with_adapter(
    adapter: Arc<MockDapAdapter>,
) -> (
    AdapterLifecycleManager,
    Arc<MockEventRepository>,
    Arc<MockDapAdapter>,
) {
    let event_repo = Arc::new(MockEventRepository::new());
    let event_capture = Arc::new(EventCaptureService::new(
        Arc::clone(&event_repo) as Arc<dyn EventRepository + Send + Sync>
    ));
    let (broadcast_tx, _) = broadcast::channel::<MetricEvent>(100);
    let (system_event_tx, _) = broadcast::channel::<SystemEvent>(100);
    let adapter_factory = Arc::new(MockDapAdapterFactory::with_default_adapter(Arc::clone(
        &adapter,
    )));
    let metric_repo = Arc::new(MockMetricRepository::new());
    let connection_repo = Arc::new(MockConnectionRepository::new());

    let manager = AdapterLifecycleManager::new(
        event_capture,
        broadcast_tx,
        system_event_tx,
        adapter_factory as DapAdapterFactoryRef,
        metric_repo as MetricRepositoryRef,
        connection_repo as ConnectionRepositoryRef,
    );

    (manager, event_repo, adapter)
}

// =============================================================================
// Unit Tests
// =============================================================================

#[tokio::test]
async fn test_start_adapter_creates_and_starts() {
    let adapter = Arc::new(MockDapAdapter::new());
    let (manager, _event_repo, adapter_ref) = create_test_manager_with_adapter(adapter).await;

    let conn_id = ConnectionId::new("test-conn");

    // Start adapter
    let result = manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await;
    assert!(result.is_ok(), "start_adapter should succeed");

    // Verify adapter was started
    assert!(
        adapter_ref.was_started(),
        "Adapter should have been started"
    );

    // Verify adapter is registered
    assert!(
        manager.has_adapter(&conn_id).await,
        "Adapter should be registered"
    );
    assert_eq!(manager.adapter_count().await, 1, "Should have 1 adapter");
}

#[tokio::test]
async fn test_stop_adapter_cleans_up() {
    let adapter = Arc::new(MockDapAdapter::new());
    let (manager, _event_repo, adapter_ref) = create_test_manager_with_adapter(adapter).await;

    let conn_id = ConnectionId::new("test-conn");

    // Start adapter
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Stop adapter
    let result = manager.stop_adapter(&conn_id).await;
    assert!(result.is_ok(), "stop_adapter should succeed");

    // Verify adapter was stopped
    assert!(
        adapter_ref.was_stopped(),
        "Adapter should have been stopped"
    );

    // Verify adapter is unregistered
    assert!(
        !manager.has_adapter(&conn_id).await,
        "Adapter should not be registered"
    );
    assert_eq!(manager.adapter_count().await, 0, "Should have 0 adapters");
}

#[tokio::test]
async fn test_get_adapter_returns_correct_adapter() {
    let adapter = Arc::new(MockDapAdapter::new());
    let (manager, _event_repo, _) = create_test_manager_with_adapter(adapter).await;

    let conn_id = ConnectionId::new("test-conn");

    // Start adapter
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Get adapter
    let retrieved = manager.get_adapter(&conn_id).await;
    assert!(retrieved.is_some(), "Should find the adapter");

    // Get non-existent adapter
    let missing_id = ConnectionId::new("missing");
    let missing = manager.get_adapter(&missing_id).await;
    assert!(missing.is_none(), "Should not find missing adapter");
}

#[tokio::test]
async fn test_list_adapters_returns_all() {
    let (manager, _event_repo, _factory) = create_test_manager().await;

    let conn_id1 = ConnectionId::new("conn-1");
    let conn_id2 = ConnectionId::new("conn-2");

    // Start two adapters
    manager
        .start_adapter(
            conn_id1.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();
    manager
        .start_adapter(
            conn_id2.clone(),
            "127.0.0.1",
            5679,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // List adapters
    let adapters = manager.list_adapters();
    assert_eq!(adapters.len(), 2, "Should have 2 adapters");

    let ids: Vec<String> = adapters.iter().map(|a| a.connection_id.0.clone()).collect();
    assert!(ids.contains(&"conn-1".to_string()));
    assert!(ids.contains(&"conn-2".to_string()));
}

#[tokio::test]
async fn test_stop_all_stops_all_adapters() {
    let (manager, _event_repo, _factory) = create_test_manager().await;

    // Start multiple adapters
    for i in 0..3 {
        let conn_id = ConnectionId::new(format!("conn-{}", i));
        manager
            .start_adapter(
                conn_id,
                "127.0.0.1",
                5678 + i as u16,
                SourceLanguage::Python,
                None,  // program
                None,  // pid
                false, // safe_mode
            )
            .await
            .unwrap();
    }

    assert_eq!(manager.adapter_count().await, 3, "Should have 3 adapters");

    // Stop all
    let result = manager.stop_all().await;
    assert!(result.is_ok(), "stop_all should succeed");
    assert_eq!(
        manager.adapter_count().await,
        0,
        "Should have 0 adapters after stop_all"
    );
}

#[tokio::test]
async fn test_event_routing_to_capture_service() {
    let adapter = Arc::new(MockDapAdapter::new());
    let (manager, event_repo, adapter_ref) = create_test_manager_with_adapter(adapter).await;

    let conn_id = ConnectionId::new("test-conn");

    // Start adapter
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Give the event listener task time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Send a test event
    let event = create_test_event(1, "test_value");
    adapter_ref.send_event(event.clone()).await;

    // Give time for event to be processed (wait longer than flush interval)
    tokio::time::sleep(tokio::time::Duration::from_millis(FLUSH_WAIT_MS)).await;

    // Verify event was captured
    let captured = event_repo.get_events().await;
    assert_eq!(captured.len(), 1, "Should have captured 1 event");
    assert_eq!(captured[0].metric_name, "test_metric_1");
}

#[tokio::test]
async fn test_event_broadcast_to_subscribers() {
    let adapter = Arc::new(MockDapAdapter::new());
    let event_repo = Arc::new(MockEventRepository::new());
    let event_capture = Arc::new(EventCaptureService::new(
        Arc::clone(&event_repo) as Arc<dyn EventRepository + Send + Sync>
    ));
    let (broadcast_tx, mut broadcast_rx) = broadcast::channel::<MetricEvent>(100);
    let adapter_factory = Arc::new(MockDapAdapterFactory::with_default_adapter(Arc::clone(
        &adapter,
    )));
    let metric_repo = Arc::new(MockMetricRepository::new());
    let connection_repo = Arc::new(MockConnectionRepository::new());

    let (system_event_tx, _) = broadcast::channel::<SystemEvent>(100);
    let manager = AdapterLifecycleManager::new(
        event_capture,
        broadcast_tx,
        system_event_tx,
        adapter_factory as DapAdapterFactoryRef,
        metric_repo as MetricRepositoryRef,
        connection_repo as ConnectionRepositoryRef,
    );

    let conn_id = ConnectionId::new("test-conn");

    // Start adapter
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Give the event listener task time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Send a test event
    let event = create_test_event(1, "broadcast_test");
    adapter.send_event(event.clone()).await;

    // Receive from broadcast
    let received =
        tokio::time::timeout(tokio::time::Duration::from_millis(500), broadcast_rx.recv()).await;

    assert!(received.is_ok(), "Should receive broadcast within timeout");
    let received_event = received.unwrap().unwrap();
    assert_eq!(received_event.metric_name, "test_metric_1");
}

#[tokio::test]
async fn test_multiple_events_are_captured() {
    let adapter = Arc::new(MockDapAdapter::new());
    let (manager, event_repo, adapter_ref) = create_test_manager_with_adapter(adapter).await;

    let conn_id = ConnectionId::new("test-conn");

    // Start adapter
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Give the event listener task time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Send multiple events
    for i in 0..5 {
        let event = create_test_event(1, &format!("value_{}", i));
        adapter_ref.send_event(event).await;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Give time for events to be processed (wait longer than flush interval)
    tokio::time::sleep(tokio::time::Duration::from_millis(FLUSH_WAIT_MS)).await;

    // Verify all events were captured
    let captured = event_repo.get_events().await;
    assert_eq!(captured.len(), 5, "Should have captured 5 events");
}

/// Test that starting an adapter with existing ID replaces the old one
///
/// Verifies:
/// - Old adapter is stopped (cleanup)
/// - New adapter is started and running
/// - Count stays at 1 (no duplicates)
#[tokio::test]
async fn test_starting_existing_adapter_replaces_it() {
    let adapter1 = Arc::new(MockDapAdapter::new());
    let adapter1_clone = Arc::clone(&adapter1);
    let (manager, _event_repo, _) = create_test_manager_with_adapter(adapter1).await;

    let conn_id = ConnectionId::new("test-conn");

    // Start first adapter
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();
    assert_eq!(manager.adapter_count().await, 1);
    assert!(
        adapter1_clone.is_connected(),
        "First adapter should be connected"
    );

    // Start another adapter with same ID (should replace)
    // Note: This will create a new mock adapter via factory
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Should still have only 1 adapter
    assert_eq!(
        manager.adapter_count().await,
        1,
        "Should still have 1 adapter after replacement"
    );

    // Verify old adapter was stopped (cleanup verification)
    assert!(
        adapter1_clone.was_stopped(),
        "Old adapter should have been stopped during replacement"
    );

    // Verify we can still get the adapter (new one)
    let current_adapter = manager.get_adapter(&conn_id).await;
    assert!(current_adapter.is_some(), "Should have a current adapter");
}

#[tokio::test]
async fn test_adapter_status_is_running_after_start() {
    let adapter = Arc::new(MockDapAdapter::new());
    let (manager, _event_repo, _) = create_test_manager_with_adapter(adapter).await;

    let conn_id = ConnectionId::new("test-conn");

    // Start adapter
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode
        )
        .await
        .unwrap();

    // Check status
    let adapters = manager.list_adapters();
    assert_eq!(adapters.len(), 1);
    assert_eq!(adapters[0].status, ManagedAdapterStatus::Running);
    assert!(adapters[0].is_connected);
}

// =============================================================================
// Property-Based Tests for Batching Behavior
// =============================================================================

// =============================================================================
// SafeMode API Tests
// =============================================================================

/// Create a test manager with access to the connection repository for SafeMode tests
async fn create_test_manager_with_connection_repo() -> (
    AdapterLifecycleManager,
    Arc<MockConnectionRepository>,
    Arc<MockDapAdapterFactory>,
) {
    let event_repo = Arc::new(MockEventRepository::new());
    let event_capture = Arc::new(EventCaptureService::new(
        Arc::clone(&event_repo) as Arc<dyn EventRepository + Send + Sync>
    ));
    let (broadcast_tx, _) = broadcast::channel::<MetricEvent>(100);
    let (system_event_tx, _) = broadcast::channel::<SystemEvent>(100);
    let adapter_factory = Arc::new(MockDapAdapterFactory::new());
    let metric_repo = Arc::new(MockMetricRepository::new());
    let connection_repo = Arc::new(MockConnectionRepository::new());

    let manager = AdapterLifecycleManager::new(
        event_capture,
        broadcast_tx,
        system_event_tx,
        Arc::clone(&adapter_factory) as DapAdapterFactoryRef,
        metric_repo as MetricRepositoryRef,
        Arc::clone(&connection_repo) as ConnectionRepositoryRef,
    );

    (manager, connection_repo, adapter_factory)
}

#[tokio::test]
async fn test_is_safe_mode_returns_some_true_for_safe_mode_connection() {
    let (manager, _, _) = create_test_manager_with_connection_repo().await;
    let conn_id = ConnectionId::new("test-safe");

    // Start adapter with safe_mode = true
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None, // program
            None, // pid
            true, // safe_mode = true
        )
        .await
        .unwrap();

    assert_eq!(manager.is_safe_mode(&conn_id), Some(true));
}

#[tokio::test]
async fn test_is_safe_mode_returns_some_false_for_normal_connection() {
    let (manager, _, _) = create_test_manager_with_connection_repo().await;
    let conn_id = ConnectionId::new("test-normal");

    // Start adapter with safe_mode = false
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // safe_mode = false
        )
        .await
        .unwrap();

    assert_eq!(manager.is_safe_mode(&conn_id), Some(false));
}

#[tokio::test]
async fn test_is_safe_mode_returns_none_for_nonexistent_connection() {
    let (manager, _, _) = create_test_manager_with_connection_repo().await;
    let conn_id = ConnectionId::new("nonexistent");

    assert_eq!(manager.is_safe_mode(&conn_id), None);
}

#[tokio::test]
async fn test_update_safe_mode_updates_both_memory_and_db() {
    let (manager, connection_repo, _) = create_test_manager_with_connection_repo().await;
    let conn_id = ConnectionId::new("test-update");

    // Create connection in DB with safe_mode = false
    let mut connection = Connection::new(
        conn_id.clone(),
        "127.0.0.1".to_string(),
        5678,
        SourceLanguage::Python,
    )
    .unwrap();
    connection.safe_mode = false;
    connection_repo.save(&connection).await.unwrap();

    // Start adapter with safe_mode = false
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None, // program
            None, // pid
            false,
        )
        .await
        .unwrap();

    // Update to safe_mode = true
    let result = manager.update_safe_mode(&conn_id, true).await;
    assert!(result.is_ok());
    assert!(result.unwrap()); // true = updated

    // Verify in-memory updated
    assert_eq!(manager.is_safe_mode(&conn_id), Some(true));

    // Verify DB updated
    let db_conn = connection_repo.find_by_id(&conn_id).await.unwrap().unwrap();
    assert!(db_conn.safe_mode);
}

#[tokio::test]
async fn test_update_safe_mode_returns_false_when_adapter_not_found() {
    let (manager, _, _) = create_test_manager_with_connection_repo().await;
    let conn_id = ConnectionId::new("nonexistent");

    let result = manager.update_safe_mode(&conn_id, true).await;
    assert!(result.is_ok());
    assert!(!result.unwrap()); // false = adapter not found
}

#[tokio::test]
async fn test_update_safe_mode_succeeds_when_not_in_db() {
    // Adapter registered but connection not in DB (unusual state)
    // Should still update in-memory and succeed
    let (manager, _, _) = create_test_manager_with_connection_repo().await; // Empty DB
    let conn_id = ConnectionId::new("test-no-db");

    // Start adapter (creates adapter in memory, but doesn't persist to our mock DB)
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None, // program
            None, // pid
            false,
        )
        .await
        .unwrap();

    let result = manager.update_safe_mode(&conn_id, true).await;
    assert!(result.is_ok());
    assert!(result.unwrap()); // true = updated (in-memory at least)

    // In-memory should be updated
    assert_eq!(manager.is_safe_mode(&conn_id), Some(true));
}

#[tokio::test]
async fn test_update_safe_mode_db_first_ensures_consistency() {
    // This test verifies that DB is updated BEFORE in-memory.
    // If in-memory were updated first and DB update failed, we'd have inconsistent state.
    // With DB-first, if DB fails, in-memory remains unchanged.

    let (manager, connection_repo, _) = create_test_manager_with_connection_repo().await;
    let conn_id = ConnectionId::new("test-db-first");

    // Create connection in DB
    let mut connection = Connection::new(
        conn_id.clone(),
        "127.0.0.1".to_string(),
        5678,
        SourceLanguage::Python,
    )
    .unwrap();
    connection.safe_mode = false;
    connection_repo.save(&connection).await.unwrap();

    // Start adapter
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None,  // program
            None,  // pid
            false, // Initial state: safe_mode = false
        )
        .await
        .unwrap();

    // Verify initial state
    assert_eq!(manager.is_safe_mode(&conn_id), Some(false));
    let db_conn = connection_repo.find_by_id(&conn_id).await.unwrap().unwrap();
    assert!(!db_conn.safe_mode);

    // Update safe_mode to true
    let result = manager.update_safe_mode(&conn_id, true).await;
    assert!(result.is_ok());
    assert!(result.unwrap()); // true = updated

    // Verify both are now true
    assert_eq!(manager.is_safe_mode(&conn_id), Some(true));
    let db_conn = connection_repo.find_by_id(&conn_id).await.unwrap().unwrap();
    assert!(db_conn.safe_mode);
}

#[tokio::test]
async fn test_safe_mode_preserved_in_list_adapters() {
    let (manager, _, _) = create_test_manager_with_connection_repo().await;

    // Start adapter with safe_mode = true
    let conn_id = ConnectionId::new("test-list-safe");
    manager
        .start_adapter(
            conn_id.clone(),
            "127.0.0.1",
            5678,
            SourceLanguage::Python,
            None, // program
            None, // pid
            true, // safe_mode = true
        )
        .await
        .unwrap();

    // Check list_adapters returns safe_mode correctly
    let adapters = manager.list_adapters();
    assert_eq!(adapters.len(), 1);
    assert!(adapters[0].safe_mode);
}

mod proptest_batching {
    use super::*;
    use detrix_config::{DaemonConfig, EventBatchingConfig};
    use proptest::prelude::*;
    use std::collections::HashSet;

    /// Strategy for generating valid metric events
    fn arb_metric_event() -> impl Strategy<Value = MetricEvent> {
        (
            1..1000u64,      // metric_id
            "[a-z]{1,20}",   // metric_name suffix
            1..1_000_000i64, // timestamp offset
            prop_oneof![
                Just("null".to_string()),
                Just("42".to_string()),
                Just("\"hello\"".to_string()),
                Just("{\"key\": \"value\"}".to_string()),
                "[a-zA-Z0-9]{1,50}".prop_map(|s| format!("\"{}\"", s)),
            ],
        )
            .prop_map(|(metric_id, name_suffix, ts_offset, value)| MetricEvent {
                id: None,
                metric_id: MetricId(metric_id),
                metric_name: format!("metric_{}", name_suffix),
                connection_id: ConnectionId::new("proptest-conn"),
                timestamp: 1_700_000_000_000_000 + ts_offset,
                thread_name: None,
                thread_id: None,
                value_json: value.clone(),
                value_numeric: None,
                value_string: Some(value),
                value_boolean: None,
                is_error: false,
                error_type: None,
                error_message: None,
                request_id: None,
                session_id: None,
                stack_trace: None,
                memory_snapshot: None,
            })
    }

    /// Strategy for generating event bursts (1-100 events)
    fn arb_event_burst() -> impl Strategy<Value = Vec<MetricEvent>> {
        proptest::collection::vec(arb_metric_event(), 1..100)
    }

    /// Strategy for generating batch sizes (realistic range: 1-50)
    fn arb_batch_size() -> impl Strategy<Value = usize> {
        1..50usize
    }

    /// Helper to create a manager with specific batching config for testing
    async fn create_batching_manager(
        batch_size: usize,
    ) -> (AdapterLifecycleManager, Arc<MockEventRepository>) {
        let event_repo = Arc::new(MockEventRepository::new());
        let event_capture = Arc::new(EventCaptureService::new(
            Arc::clone(&event_repo) as Arc<dyn EventRepository + Send + Sync>
        ));
        let (broadcast_tx, _) = broadcast::channel::<MetricEvent>(1000);
        let adapter_factory = Arc::new(MockDapAdapterFactory::new());
        let metric_repo = Arc::new(MockMetricRepository::new());
        let connection_repo = Arc::new(MockConnectionRepository::new());

        // Create batching config with disabled timer (we'll flush explicitly)
        let batching_config = EventBatchingConfig {
            batch_size,
            flush_interval_ms: 0, // Disable timer-based flush for deterministic tests
            ..Default::default()
        };

        let (system_event_tx, _) = broadcast::channel::<SystemEvent>(100);
        let manager = AdapterLifecycleManager::with_config(
            event_capture,
            broadcast_tx,
            system_event_tx,
            adapter_factory as DapAdapterFactoryRef,
            metric_repo as MetricRepositoryRef,
            connection_repo as ConnectionRepositoryRef,
            batching_config,
            Default::default(),
            DaemonConfig::default().drain_timeout_ms,
            None, // No GELF output in tests
        );

        (manager, event_repo)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Property: All events added to buffer are eventually persisted after flush
        #[test]
        fn proptest_all_events_persisted_after_flush(
            events in arb_event_burst(),
            batch_size in arb_batch_size()
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (manager, event_repo) = create_batching_manager(batch_size).await;

                // Add all events to buffer directly (simulating buffered mode)
                {
                    let mut buffer = manager.event_buffer.lock().await;
                    buffer.extend(events.iter().cloned());
                }

                // Flush the buffer
                manager.flush_events().await;

                // Verify all events were persisted
                let persisted = event_repo.get_events().await;
                prop_assert_eq!(
                    persisted.len(),
                    events.len(),
                    "All {} events should be persisted, got {}",
                    events.len(),
                    persisted.len()
                );

                // Verify buffer is empty after flush
                let buffer_size = manager.event_buffer.lock().await.len();
                prop_assert_eq!(buffer_size, 0, "Buffer should be empty after flush");

                Ok(())
            })?;
        }

        /// Property: Events are persisted in the same order they were added
        #[test]
        fn proptest_event_order_preserved(
            events in proptest::collection::vec(arb_metric_event(), 2..50)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (manager, event_repo) = create_batching_manager(100).await;

                // Assign unique timestamps to track order
                let events_with_order: Vec<MetricEvent> = events
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut e)| {
                        e.timestamp = i as i64;
                        e
                    })
                    .collect();

                // Add events to buffer
                {
                    let mut buffer = manager.event_buffer.lock().await;
                    buffer.extend(events_with_order.iter().cloned());
                }

                // Flush
                manager.flush_events().await;

                // Verify order is preserved
                let persisted = event_repo.get_events().await;
                for (i, event) in persisted.iter().enumerate() {
                    prop_assert_eq!(
                        event.timestamp,
                        i as i64,
                        "Event at position {} should have timestamp {}, got {}",
                        i,
                        i,
                        event.timestamp
                    );
                }

                Ok(())
            })?;
        }

        /// Property: Flushing an empty buffer is a no-op (no errors, no events saved)
        #[test]
        fn proptest_empty_flush_is_noop(batch_size in arb_batch_size()) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (manager, event_repo) = create_batching_manager(batch_size).await;

                // Flush empty buffer multiple times
                for _ in 0..5 {
                    manager.flush_events().await;
                }

                // Verify no events were saved
                let persisted = event_repo.get_events().await;
                prop_assert_eq!(persisted.len(), 0, "No events should be persisted from empty buffer");

                Ok(())
            })?;
        }

        /// Property: Multiple flushes don't duplicate events
        #[test]
        fn proptest_no_duplicate_events_on_multiple_flush(
            events in arb_event_burst()
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (manager, event_repo) = create_batching_manager(100).await;

                // Add events
                {
                    let mut buffer = manager.event_buffer.lock().await;
                    buffer.extend(events.iter().cloned());
                }

                // Flush multiple times
                manager.flush_events().await;
                manager.flush_events().await;
                manager.flush_events().await;

                // Verify exactly events.len() events were saved (no duplicates)
                let persisted = event_repo.get_events().await;
                prop_assert_eq!(
                    persisted.len(),
                    events.len(),
                    "Should have exactly {} events, no duplicates",
                    events.len()
                );

                Ok(())
            })?;
        }

        /// Property: Buffer accumulates events correctly before flush
        #[test]
        fn proptest_buffer_accumulates_correctly(
            batch1 in proptest::collection::vec(arb_metric_event(), 1..20),
            batch2 in proptest::collection::vec(arb_metric_event(), 1..20)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (manager, event_repo) = create_batching_manager(100).await;

                // Add first batch
                {
                    let mut buffer = manager.event_buffer.lock().await;
                    buffer.extend(batch1.iter().cloned());
                }

                // Verify buffer has first batch
                {
                    let buffer = manager.event_buffer.lock().await;
                    prop_assert_eq!(buffer.len(), batch1.len(), "Buffer should have first batch");
                }

                // Add second batch
                {
                    let mut buffer = manager.event_buffer.lock().await;
                    buffer.extend(batch2.iter().cloned());
                }

                // Verify buffer has both batches
                let expected_total = batch1.len() + batch2.len();
                {
                    let buffer = manager.event_buffer.lock().await;
                    prop_assert_eq!(
                        buffer.len(),
                        expected_total,
                        "Buffer should have both batches"
                    );
                }

                // Flush and verify all events persisted
                manager.flush_events().await;
                let persisted = event_repo.get_events().await;
                prop_assert_eq!(persisted.len(), expected_total, "All events should be persisted");

                Ok(())
            })?;
        }

        /// Property: Unique events remain unique after flush (no data corruption)
        #[test]
        fn proptest_no_data_corruption(
            events in proptest::collection::vec(arb_metric_event(), 5..30)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (manager, event_repo) = create_batching_manager(100).await;

                // Make events have unique identifiers (timestamp + metric_id combo)
                let events_with_id: Vec<MetricEvent> = events
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut e)| {
                        e.timestamp = i as i64 * 1000;
                        e.metric_id = MetricId((i + 1) as u64);
                        e
                    })
                    .collect();

                let original_ids: HashSet<(u64, i64)> = events_with_id
                    .iter()
                    .map(|e| (e.metric_id.0, e.timestamp))
                    .collect();

                // Add and flush
                {
                    let mut buffer = manager.event_buffer.lock().await;
                    buffer.extend(events_with_id.iter().cloned());
                }
                manager.flush_events().await;

                // Verify all unique IDs are preserved
                let persisted = event_repo.get_events().await;
                let persisted_ids: HashSet<(u64, i64)> = persisted
                    .iter()
                    .map(|e| (e.metric_id.0, e.timestamp))
                    .collect();

                prop_assert_eq!(
                    original_ids,
                    persisted_ids,
                    "All unique event identifiers should be preserved"
                );

                Ok(())
            })?;
        }

        /// Property: Large event bursts are handled without panic or data loss
        #[test]
        fn proptest_large_burst_handled(
            events in proptest::collection::vec(arb_metric_event(), 50..200),
            batch_size in 5..20usize
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (manager, event_repo) = create_batching_manager(batch_size).await;

                // Add all events at once (simulating burst)
                {
                    let mut buffer = manager.event_buffer.lock().await;
                    buffer.extend(events.iter().cloned());
                }

                // Multiple flushes to handle all events
                for _ in 0..10 {
                    manager.flush_events().await;
                }

                // Verify all events persisted
                let persisted = event_repo.get_events().await;
                prop_assert_eq!(
                    persisted.len(),
                    events.len(),
                    "All {} events should be persisted even in large burst",
                    events.len()
                );

                Ok(())
            })?;
        }

        /// Property: Event values are preserved exactly (no truncation/modification)
        #[test]
        fn proptest_event_values_preserved(
            events in proptest::collection::vec(arb_metric_event(), 1..30)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (manager, event_repo) = create_batching_manager(100).await;

                // Store original values for comparison
                let original_values: Vec<String> = events
                    .iter()
                    .map(|e| e.value_json.clone())
                    .collect();

                // Add and flush
                {
                    let mut buffer = manager.event_buffer.lock().await;
                    buffer.extend(events.iter().cloned());
                }
                manager.flush_events().await;

                // Verify values are preserved exactly
                let persisted = event_repo.get_events().await;
                for (i, (original, persisted)) in original_values.iter().zip(persisted.iter()).enumerate() {
                    prop_assert_eq!(
                        original,
                        &persisted.value_json,
                        "Event {} value should be preserved exactly",
                        i
                    );
                }

                Ok(())
            })?;
        }
    }
}
