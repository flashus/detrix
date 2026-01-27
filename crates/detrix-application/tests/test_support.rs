use detrix_application::{
    ConnectionRepository, DapAdapter, DapAdapterFactory, DapAdapterRef, EventRepository,
    MetricRepository, RemoveMetricResult, SetMetricResult,
};
use detrix_core::{
    Connection, ConnectionId, ConnectionStatus, Error, Metric, MetricEvent, MetricId, Result,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

// Note: Using std::sync::Mutex is acceptable in this async context because:
// 1. Lock duration is very short (just reading/writing a bool)
// 2. This is test-only code - not production
// 3. No await points while holding the lock
use std::sync::Mutex as StdMutex;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Default)]
pub struct MockConnectionRepository {
    connections: Arc<Mutex<HashMap<ConnectionId, Connection>>>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct StatefulMockDapAdapter {
    #[allow(dead_code)]
    host: String,
    #[allow(dead_code)]
    port: u16,
    connected: AtomicBool,
    started: AtomicBool,
    stopped: AtomicBool,
    event_tx: Mutex<Option<mpsc::Sender<MetricEvent>>>,
    subscribe_count: AtomicUsize,
}

impl StatefulMockDapAdapter {
    #[allow(dead_code)]
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            connected: AtomicBool::new(false),
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            event_tx: Mutex::new(None),
            subscribe_count: AtomicUsize::new(0),
        }
    }

    #[allow(dead_code)]
    pub async fn send_event(&self, event: MetricEvent) {
        if let Some(tx) = self.event_tx.lock().await.as_ref() {
            let _ = tx.try_send(event);
        }
    }

    #[allow(dead_code)]
    pub fn was_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn was_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Simulate a debugger crash by closing the event channel
    ///
    /// This drops the sender, causing the receiver to close and
    /// triggering the cleanup mechanism.
    #[allow(dead_code)]
    pub async fn simulate_crash(&self) {
        // Clear the event sender to simulate channel closure
        *self.event_tx.lock().await = None;
        self.connected.store(false, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl DapAdapter for StatefulMockDapAdapter {
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
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    async fn set_metric(&self, _metric: &Metric) -> Result<SetMetricResult> {
        Ok(SetMetricResult {
            verified: true,
            line: 10,
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

#[allow(dead_code)]
#[derive(Debug)]
pub struct StatefulMockAdapterFactory {
    adapters: RwLock<HashMap<String, Arc<StatefulMockDapAdapter>>>,
}

impl StatefulMockAdapterFactory {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            adapters: RwLock::new(HashMap::new()),
        }
    }

    #[allow(dead_code)]
    pub async fn get_adapter(&self, host: &str, port: u16) -> Option<Arc<StatefulMockDapAdapter>> {
        let key = format!("{}:{}", host, port);
        self.adapters.read().await.get(&key).cloned()
    }
}

#[async_trait::async_trait]
impl DapAdapterFactory for StatefulMockAdapterFactory {
    async fn create_python_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef> {
        let key = format!("{}:{}", host, port);
        let adapter = Arc::new(StatefulMockDapAdapter::new(host, port));

        self.adapters
            .write()
            .await
            .insert(key, Arc::clone(&adapter));

        Ok(adapter as DapAdapterRef)
    }

    async fn create_go_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef> {
        self.create_python_adapter(host, port).await
    }

    async fn create_rust_adapter(
        &self,
        host: &str,
        port: u16,
        _program: Option<&str>,
    ) -> Result<DapAdapterRef> {
        self.create_python_adapter(host, port).await
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct SimpleMockDapAdapter {
    #[allow(dead_code)]
    host: String,
    #[allow(dead_code)]
    port: u16,
    connected: Arc<StdMutex<bool>>,
}

impl SimpleMockDapAdapter {
    #[allow(dead_code)]
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            connected: Arc::new(StdMutex::new(false)),
        }
    }
}

#[async_trait::async_trait]
impl DapAdapter for SimpleMockDapAdapter {
    async fn start(&self) -> Result<()> {
        *self.connected.lock().unwrap() = true;
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        *self.connected.lock().unwrap() = false;
        Ok(())
    }

    async fn ensure_connected(&self) -> Result<()> {
        Ok(())
    }

    fn is_connected(&self) -> bool {
        *self.connected.lock().unwrap()
    }

    async fn set_metric(&self, _metric: &Metric) -> Result<SetMetricResult> {
        Ok(SetMetricResult {
            verified: true,
            line: 10,
            message: None,
        })
    }

    async fn remove_metric(&self, _metric: &Metric) -> Result<RemoveMetricResult> {
        Ok(RemoveMetricResult::success())
    }

    async fn subscribe_events(&self) -> Result<mpsc::Receiver<MetricEvent>> {
        let (_tx, rx) = mpsc::channel(1000);
        Ok(rx)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SimpleMockAdapterFactory;

#[async_trait::async_trait]
impl DapAdapterFactory for SimpleMockAdapterFactory {
    async fn create_python_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef> {
        Ok(Arc::new(SimpleMockDapAdapter::new(host, port)))
    }

    async fn create_go_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef> {
        Ok(Arc::new(SimpleMockDapAdapter::new(host, port)))
    }

    async fn create_rust_adapter(
        &self,
        host: &str,
        port: u16,
        _program: Option<&str>,
    ) -> Result<DapAdapterRef> {
        Ok(Arc::new(SimpleMockDapAdapter::new(host, port)))
    }
}

impl MockConnectionRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_connection(&self, id: &ConnectionId) -> Option<Connection> {
        self.connections.lock().await.get(id).cloned()
    }

    pub async fn connection_count(&self) -> usize {
        self.connections.lock().await.len()
    }
}

#[async_trait::async_trait]
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

    async fn list_all(&self) -> Result<Vec<Connection>> {
        let connections = self.connections.lock().await;
        Ok(connections.values().cloned().collect())
    }

    async fn update_status(&self, id: &ConnectionId, status: ConnectionStatus) -> Result<()> {
        let mut connections = self.connections.lock().await;
        if let Some(conn) = connections.get_mut(id) {
            conn.status = status;
            Ok(())
        } else {
            Err(Error::MetricNotFound(format!(
                "Connection {} not found",
                id.0
            )))
        }
    }

    async fn touch(&self, id: &ConnectionId) -> Result<()> {
        let mut connections = self.connections.lock().await;
        if let Some(conn) = connections.get_mut(id) {
            conn.touch();
            Ok(())
        } else {
            Err(Error::MetricNotFound(format!(
                "Connection {} not found",
                id.0
            )))
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

    async fn find_by_address(&self, host: &str, port: u16) -> Result<Option<Connection>> {
        let connections = self.connections.lock().await;
        Ok(connections
            .values()
            .find(|c| c.host == host && c.port == port)
            .cloned())
    }

    async fn update(&self, connection: &Connection) -> Result<()> {
        let mut connections = self.connections.lock().await;
        connections.insert(connection.id.clone(), connection.clone());
        Ok(())
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

pub struct MockMetricRepository;

impl MockMetricRepository {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
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
        _filter: &detrix_application::ports::MetricFilter,
        _limit: usize,
        _offset: usize,
    ) -> Result<(Vec<Metric>, u64)> {
        Ok((Vec::new(), 0))
    }

    async fn get_group_summaries(&self) -> Result<Vec<detrix_application::ports::GroupSummary>> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Default)]
pub struct MockEventRepository {
    events: RwLock<Vec<MetricEvent>>,
    next_id: Mutex<i64>,
}

impl MockEventRepository {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
            next_id: Mutex::new(1),
        }
    }

    #[allow(dead_code)]
    pub async fn get_events(&self) -> Vec<MetricEvent> {
        self.events.read().await.clone()
    }

    #[allow(dead_code)]
    pub async fn event_count(&self) -> usize {
        self.events.read().await.len()
    }
}

#[async_trait::async_trait]
impl EventRepository for MockEventRepository {
    async fn save(&self, event: &MetricEvent) -> Result<i64> {
        let mut event = event.clone();
        let mut next_id = self.next_id.lock().await;
        let id = *next_id;
        *next_id += 1;
        event.id = Some(id);
        self.events.write().await.push(event);
        Ok(id)
    }

    async fn save_batch(&self, events: &[MetricEvent]) -> Result<Vec<i64>> {
        let mut ids = Vec::with_capacity(events.len());

        let start_id = {
            let mut next_id = self.next_id.lock().await;
            let start = *next_id;
            *next_id += events.len() as i64;
            start
        };

        for i in 0..events.len() {
            ids.push(start_id + i as i64);
        }

        let mut stored = self.events.write().await;
        for (idx, event) in events.iter().enumerate() {
            let mut e = event.clone();
            e.id = Some(ids[idx]);
            stored.push(e);
        }

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
        _start: i64,
        _end: i64,
        limit: i64,
    ) -> Result<Vec<MetricEvent>> {
        self.find_by_metric_id(metric_id, limit).await
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

    async fn delete_older_than(&self, _timestamp: i64) -> Result<u64> {
        Ok(0)
    }

    async fn cleanup_metric_events(&self, _metric_id: MetricId, _keep_count: usize) -> Result<u64> {
        Ok(0)
    }
}
