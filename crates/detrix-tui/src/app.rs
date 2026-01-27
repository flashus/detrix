//! Application state and main event loop

use crate::client::{DaemonStatus, DetrixClient};
use crate::event::AppEvent;
use crate::input::handle_key;
use crate::ui;
use crate::ui::theme::Theme;
use color_eyre::Result;
use crossterm::event::{Event, EventStream, KeyEventKind};
use detrix_config::constants::{DEFAULT_TUI_EVENT_TIMES_CAPACITY, DEFAULT_TUI_LOG_CAPACITY};
use detrix_config::TuiConfig;
use detrix_core::{Connection, ConnectionId, Metric, MetricEvent, SystemEvent};
use futures::StreamExt;
use ratatui::widgets::{ListState, TableState};
use ratatui::DefaultTerminal;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Application mode
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMode {
    /// Normal running mode
    Running,
    /// Showing a popup
    Popup(PopupType),
    /// Quitting the application
    Quitting,
}

/// Type of popup being shown
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupType {
    /// Help screen
    Help,
    /// Connection switcher
    ConnectionSwitcher,
    /// Event filter
    Filter,
    /// Metric details (metric id as u64)
    MetricDetails(u64),
    /// Event details (index in events buffer)
    EventDetails(usize),
    /// Confirm close connection (connection_id)
    ConfirmCloseConnection(String),
}

/// Currently selected tab
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Events,
    Metrics,
    Connections,
    SystemLog,
}

impl Tab {
    /// Get the next tab
    pub fn next(self) -> Self {
        match self {
            Tab::Events => Tab::Metrics,
            Tab::Metrics => Tab::Connections,
            Tab::Connections => Tab::SystemLog,
            Tab::SystemLog => Tab::Events,
        }
    }

    /// Get the previous tab
    pub fn prev(self) -> Self {
        match self {
            Tab::Events => Tab::SystemLog,
            Tab::Metrics => Tab::Events,
            Tab::Connections => Tab::Metrics,
            Tab::SystemLog => Tab::Connections,
        }
    }

    /// Get tab name for display
    pub fn name(&self) -> &'static str {
        match self {
            Tab::Events => "Events",
            Tab::Metrics => "Metrics",
            Tab::Connections => "Connections",
            Tab::SystemLog => "System Log",
        }
    }

    /// Get all tabs
    pub fn all() -> &'static [Tab] {
        &[Tab::Events, Tab::Metrics, Tab::Connections, Tab::SystemLog]
    }
}

/// Main application state
pub struct App {
    // Mode and navigation
    pub mode: AppMode,
    pub selected_tab: Tab,
    pub selected_connection: Option<ConnectionId>,

    // Data buffers
    pub events: VecDeque<MetricEvent>,
    pub metrics: Vec<Metric>,
    pub connections: Vec<Connection>,
    pub system_events: VecDeque<SystemEvent>,

    // UI state for each tab
    pub events_state: TableState,
    pub metrics_state: TableState,
    pub connections_state: TableState,
    pub system_state: TableState,

    // Pinned event key for Event Details panel (set by pressing Enter)
    // Stores (metric_id, timestamp) to identify the event even as list scrolls
    // The details panel shows this event, while cursor can move freely
    pub pinned_event_key: Option<(u64, i64)>, // (metric_id, timestamp)

    // Popup state
    pub connection_switcher_state: ListState,

    // Streaming control
    pub streaming_paused: bool,
    pub events_per_second: f64,
    pub total_events: u64,
    event_times: VecDeque<Instant>,

    // Refresh control (hybrid mode)
    pub last_input_time: Instant,
    pub is_idle: bool,

    // Filter
    pub filter_text: String,

    // Configuration
    pub config: TuiConfig,
    pub theme: Theme,

    // gRPC client
    client: DetrixClient,

    // Event sender for async operations
    event_tx: Option<mpsc::UnboundedSender<AppEvent>>,

    // Connection status
    pub last_error: Option<String>,
    pub grpc_connected: bool,

    // Internal TUI log
    pub tui_logs: VecDeque<TuiLogEntry>,

    // Daemon status (from HTTP API)
    pub daemon_status: DaemonStatus,
}

/// Internal TUI log entry
#[derive(Debug, Clone)]
pub struct TuiLogEntry {
    pub timestamp: Instant,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl App {
    /// Create a new application instance
    pub fn new(config: TuiConfig, client: DetrixClient) -> Self {
        let theme = Theme::from_config(&config.theme);

        Self {
            mode: AppMode::Running,
            selected_tab: Tab::Events,
            selected_connection: None,

            events: VecDeque::with_capacity(config.event_buffer_size),
            metrics: Vec::new(),
            connections: Vec::new(),
            system_events: VecDeque::with_capacity(config.system_event_capacity),

            events_state: TableState::default(),
            metrics_state: TableState::default(),
            connections_state: TableState::default(),
            system_state: TableState::default(),

            pinned_event_key: None,

            connection_switcher_state: ListState::default(),

            streaming_paused: false,
            events_per_second: 0.0,
            total_events: 0,
            event_times: VecDeque::with_capacity(DEFAULT_TUI_EVENT_TIMES_CAPACITY),

            last_input_time: Instant::now(),
            is_idle: false,

            filter_text: String::new(),

            config,
            theme,
            client,
            event_tx: None,

            last_error: None,
            grpc_connected: false,

            tui_logs: VecDeque::with_capacity(DEFAULT_TUI_LOG_CAPACITY),

            daemon_status: DaemonStatus::default(),
        }
    }

    /// Add an info log entry
    pub fn log_info(&mut self, msg: impl Into<String>) {
        self.add_log(LogLevel::Info, msg.into());
    }

    /// Add a warning log entry
    pub fn log_warn(&mut self, msg: impl Into<String>) {
        self.add_log(LogLevel::Warn, msg.into());
    }

    /// Add an error log entry
    pub fn log_error(&mut self, msg: impl Into<String>) {
        self.add_log(LogLevel::Error, msg.into());
    }

    /// Add a log entry
    fn add_log(&mut self, level: LogLevel, message: String) {
        if self.tui_logs.len() >= DEFAULT_TUI_LOG_CAPACITY {
            self.tui_logs.pop_back();
        }
        self.tui_logs.push_front(TuiLogEntry {
            timestamp: Instant::now(),
            level,
            message,
        });
    }

    /// Run the main application loop
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        // Create event channel
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();

        // Store event sender for async operations
        self.event_tx = Some(event_tx.clone());

        // Spawn keyboard input handler
        let input_tx = event_tx.clone();
        tokio::spawn(async move {
            let mut events = EventStream::new();
            while let Some(Ok(event)) = events.next().await {
                if let Event::Key(key) = event {
                    if key.kind == KeyEventKind::Press && input_tx.send(AppEvent::Key(key)).is_err()
                    {
                        break;
                    }
                }
            }
        });

        // Spawn gRPC stream listener
        self.spawn_stream_listener(event_tx.clone()).await?;

        // Spawn background refresh task (doesn't block main loop)
        self.spawn_refresh_task(event_tx.clone());

        // Main loop - NEVER blocks on gRPC, only processes events
        let mut last_tick = Instant::now();

        while self.mode != AppMode::Quitting {
            // Render
            terminal.draw(|frame| ui::render(frame, &mut self))?;

            // Calculate dynamic tick rate
            let tick_rate = self.tick_rate();
            let timeout = tick_rate.saturating_sub(last_tick.elapsed());

            // Wait for event or timeout
            if let Ok(Some(event)) = tokio::time::timeout(timeout, event_rx.recv()).await {
                self.handle_event(event);
            }

            // Tick
            if last_tick.elapsed() >= tick_rate {
                self.on_tick();
                last_tick = Instant::now();
            }
        }

        Ok(())
    }

    /// Spawn background refresh task that periodically fetches metrics and connections.
    /// Runs in a separate tokio task (greenthread) so it never blocks the main loop.
    fn spawn_refresh_task(&self, tx: mpsc::UnboundedSender<AppEvent>) {
        let mut client = self.client.clone();
        // Use config values for timing (copied before spawning task)
        let refresh_interval_connected =
            Duration::from_millis(self.config.data_refresh_connected_ms);
        let refresh_interval_disconnected =
            Duration::from_millis(self.config.data_refresh_disconnected_ms);
        let grpc_timeout = Duration::from_millis(self.config.grpc_timeout_ms);

        tokio::spawn(async move {
            let mut connected = true;

            loop {
                // Adaptive refresh interval based on connection status
                let interval = if connected {
                    refresh_interval_connected
                } else {
                    refresh_interval_disconnected
                };

                tokio::time::sleep(interval).await;

                let mut current_connected = true;

                // Fetch metrics (with timeout)
                match tokio::time::timeout(grpc_timeout, client.list_metrics()).await {
                    Ok(Ok(metrics)) => {
                        let _ = tx.send(AppEvent::MetricsUpdated(metrics));
                    }
                    Ok(Err(e)) => {
                        current_connected = false;
                        let _ = tx.send(AppEvent::Error(format!("Metrics: {}", e)));
                    }
                    Err(_) => {
                        current_connected = false;
                        let _ = tx.send(AppEvent::Warning("Metrics request timeout".to_string()));
                    }
                }

                // Fetch connections (with timeout)
                match tokio::time::timeout(grpc_timeout, client.list_connections()).await {
                    Ok(Ok(connections)) => {
                        let _ = tx.send(AppEvent::ConnectionsUpdated(connections));
                    }
                    Ok(Err(e)) => {
                        current_connected = false;
                        let _ = tx.send(AppEvent::Error(format!("Connections: {}", e)));
                    }
                    Err(_) => {
                        current_connected = false;
                        let _ =
                            tx.send(AppEvent::Warning("Connections request timeout".to_string()));
                    }
                }

                // Fetch daemon status via HTTP (includes MCP clients)
                match tokio::time::timeout(grpc_timeout, client.get_status()).await {
                    Ok(Ok(status)) => {
                        let _ = tx.send(AppEvent::StatusUpdated(Box::new(status)));
                    }
                    Ok(Err(_e)) => {
                        // Silently ignore status fetch errors (HTTP might not be available)
                    }
                    Err(_) => {
                        // Timeout - ignore silently
                    }
                }

                // Update connection status
                if current_connected != connected {
                    connected = current_connected;
                    let _ = tx.send(AppEvent::GrpcStatus(connected));
                }

                // Check if channel is closed (app exiting)
                if tx.is_closed() {
                    break;
                }
            }
        });
    }

    /// Get the current tick rate based on idle state
    pub fn tick_rate(&self) -> Duration {
        if self.is_idle {
            Duration::from_millis(self.config.refresh_rate_idle_ms)
        } else {
            Duration::from_millis(self.config.refresh_rate_active_ms)
        }
    }

    /// Called on user input to reset idle timer
    pub fn on_input(&mut self) {
        self.last_input_time = Instant::now();
        self.is_idle = false;
    }

    /// Check if we should switch to idle mode
    fn check_idle(&mut self) {
        if self.last_input_time.elapsed() > Duration::from_millis(self.config.idle_timeout_ms) {
            self.is_idle = true;
        }
    }

    /// Called on each tick
    fn on_tick(&mut self) {
        self.check_idle();
        self.update_events_per_second();
    }

    /// Update events per second calculation
    fn update_events_per_second(&mut self) {
        let now = Instant::now();
        let one_second_ago = now - Duration::from_secs(1);

        // Remove old timestamps
        while self
            .event_times
            .front()
            .map(|t| *t < one_second_ago)
            .unwrap_or(false)
        {
            self.event_times.pop_front();
        }

        self.events_per_second = self.event_times.len() as f64;
    }

    /// Handle an application event
    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => {
                self.on_input();
                handle_key(self, key);
            }
            AppEvent::MetricEvent(metric_event) => {
                if !self.streaming_paused {
                    // Log first event
                    if self.total_events == 0 {
                        self.log_info(format!(
                            "First event received: {}",
                            metric_event.metric_name
                        ));
                    }
                    self.add_event(*metric_event);
                }
            }
            AppEvent::SystemEvent(system_event) => {
                self.log_info(format!("System event: {:?}", system_event.event_type));
                self.add_system_event(*system_event);
            }
            AppEvent::MetricsUpdated(metrics) => {
                if metrics.len() != self.metrics.len() {
                    self.log_info(format!("Metrics updated: {} total", metrics.len()));
                }
                self.metrics = metrics;
                // Auto-select first metric if nothing selected
                if self.metrics_state.selected().is_none() && !self.metrics.is_empty() {
                    self.metrics_state.select(Some(0));
                }
                // Mark as connected since we got data
                self.grpc_connected = true;
                self.last_error = None;
            }
            AppEvent::ConnectionsUpdated(connections) => {
                if connections.len() != self.connections.len() {
                    self.log_info(format!("Connections updated: {} total", connections.len()));
                }
                self.connections = connections;
                // Auto-select first connection if nothing selected
                if self.connections_state.selected().is_none() && !self.connections.is_empty() {
                    self.connections_state.select(Some(0));
                }
            }
            AppEvent::StatusUpdated(status) => {
                // Update daemon status (includes MCP clients)
                self.daemon_status = *status;
            }
            AppEvent::GrpcStatus(connected) => {
                if connected != self.grpc_connected {
                    self.grpc_connected = connected;
                    if connected {
                        self.log_info("gRPC connection restored");
                        self.last_error = None;
                    } else {
                        self.log_warn("gRPC connection lost");
                    }
                }
            }
            AppEvent::Warning(msg) => {
                // Don't spam logs with repeated warnings
                if self.last_error.as_ref() != Some(&msg) {
                    self.log_warn(&msg);
                    self.last_error = Some(msg);
                }
            }
            AppEvent::Error(error) => {
                // Don't spam logs with repeated errors
                if self.last_error.as_ref() != Some(&error) {
                    self.log_error(format!("Stream error: {}", error));
                    self.last_error = Some(error);
                }
            }
            AppEvent::Tick => {
                self.on_tick();
            }
            AppEvent::ConnectionClosed(result) => match result {
                Ok(conn_id) => {
                    self.log_info(format!("Connection closed: {}", conn_id));
                    // Connection will be removed on next refresh
                }
                Err(err) => {
                    self.log_error(format!("Failed to close connection: {}", err));
                }
            },
        }
    }

    /// Add a metric event to the buffer
    fn add_event(&mut self, event: MetricEvent) {
        // Track timing for rate calculation
        self.event_times.push_back(Instant::now());
        self.total_events += 1;

        // Add to buffer with rotation
        if self.events.len() >= self.config.event_buffer_size {
            self.events.pop_back();
        }
        self.events.push_front(event);

        // Auto-select first if nothing selected
        if self.events_state.selected().is_none() && !self.events.is_empty() {
            self.events_state.select(Some(0));
        }
    }

    /// Add a system event to the buffer
    fn add_system_event(&mut self, event: SystemEvent) {
        if self.system_events.len() >= self.config.system_event_capacity {
            self.system_events.pop_back();
        }
        self.system_events.push_front(event);
    }

    /// Spawn the gRPC stream listener task
    async fn spawn_stream_listener(&mut self, tx: mpsc::UnboundedSender<AppEvent>) -> Result<()> {
        let mut client = self.client.clone();

        tokio::spawn(async move {
            loop {
                match client.stream_all().await {
                    Ok(mut stream) => {
                        while let Some(result) = stream.next().await {
                            match result {
                                Ok(event) => {
                                    if tx.send(AppEvent::MetricEvent(Box::new(event))).is_err() {
                                        return;
                                    }
                                }
                                Err(e) => {
                                    let _ =
                                        tx.send(AppEvent::Error(format!("Stream error: {}", e)));
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ =
                            tx.send(AppEvent::Error(format!("Failed to connect stream: {}", e)));
                    }
                }

                // Wait before reconnecting
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        Ok(())
    }

    /// Navigate to next tab
    pub fn next_tab(&mut self) {
        self.selected_tab = self.selected_tab.next();
    }

    /// Navigate to previous tab
    pub fn prev_tab(&mut self) {
        self.selected_tab = self.selected_tab.prev();
    }

    /// Clear all events
    pub fn clear_events(&mut self) {
        self.events.clear();
        self.events_state.select(None);
        self.pinned_event_key = None;
        self.total_events = 0;
        self.event_times.clear();
    }

    /// Pin the currently selected event for the details panel
    pub fn pin_selected_event(&mut self) {
        if let Some(event) = self.selected_event() {
            self.pinned_event_key = Some((event.metric_id.0, event.timestamp));
        }
    }

    /// Unpin the event (details will follow cursor again)
    pub fn unpin_event(&mut self) {
        self.pinned_event_key = None;
    }

    /// Check if the selected event is the pinned one
    pub fn is_selected_pinned(&self) -> bool {
        if let (Some(event), Some((metric_id, timestamp))) =
            (self.selected_event(), self.pinned_event_key)
        {
            event.metric_id.0 == metric_id && event.timestamp == timestamp
        } else {
            false
        }
    }

    /// Find the current index of the pinned event (for rendering the marker)
    pub fn pinned_event_index(&self) -> Option<usize> {
        let (metric_id, timestamp) = self.pinned_event_key?;
        self.events
            .iter()
            .position(|e| e.metric_id.0 == metric_id && e.timestamp == timestamp)
    }

    /// Get the pinned event (for details panel)
    /// Returns the pinned event if set and still in buffer, otherwise the selected event
    pub fn pinned_event(&self) -> Option<&MetricEvent> {
        if let Some((metric_id, timestamp)) = self.pinned_event_key {
            // Find the event by its key
            if let Some(event) = self
                .events
                .iter()
                .find(|e| e.metric_id.0 == metric_id && e.timestamp == timestamp)
            {
                return Some(event);
            }
            // Event scrolled out of buffer, pinned_event_key is stale
        }
        // Fall back to selected event
        self.selected_event()
    }

    /// Check if there is a valid pinned event
    pub fn has_pinned_event(&self) -> bool {
        if let Some((metric_id, timestamp)) = self.pinned_event_key {
            self.events
                .iter()
                .any(|e| e.metric_id.0 == metric_id && e.timestamp == timestamp)
        } else {
            false
        }
    }

    /// Get the currently selected event (cursor position)
    pub fn selected_event(&self) -> Option<&MetricEvent> {
        self.events_state
            .selected()
            .and_then(|i| self.events.get(i))
    }

    /// Get the currently selected metric
    pub fn selected_metric(&self) -> Option<&Metric> {
        self.metrics_state
            .selected()
            .and_then(|i| self.metrics.get(i))
    }

    /// Get the currently selected connection
    pub fn selected_connection_info(&self) -> Option<&Connection> {
        self.connections_state
            .selected()
            .and_then(|i| self.connections.get(i))
    }

    /// Get the currently selected connection ID (as string)
    pub fn get_selected_connection_id(&self) -> Option<String> {
        self.selected_connection_info().map(|c| c.id.0.clone())
    }

    /// Get client clone for async operations
    pub fn client(&self) -> DetrixClient {
        self.client.clone()
    }

    /// Close a connection asynchronously (spawns a background task)
    pub fn close_connection_async(&self, connection_id: String) {
        let Some(event_tx) = self.event_tx.clone() else {
            return;
        };

        let mut client = self.client.clone();
        let conn_id = connection_id.clone();

        tokio::spawn(async move {
            let result = match client.close_connection(&conn_id).await {
                Ok(_) => Ok(conn_id),
                Err(e) => Err(e.to_string()),
            };
            let _ = event_tx.send(AppEvent::ConnectionClosed(result));
        });
    }
}
