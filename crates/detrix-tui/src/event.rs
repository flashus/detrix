//! Application events

use crate::client::DaemonStatus;
use crossterm::event::KeyEvent;
use detrix_core::{Connection, Metric, MetricEvent, SystemEvent};

/// Events that can occur in the application
#[derive(Debug)]
pub enum AppEvent {
    /// Keyboard input
    Key(KeyEvent),

    /// New metric event from streaming
    MetricEvent(Box<MetricEvent>),

    /// System event
    SystemEvent(Box<SystemEvent>),

    /// Metrics list updated
    MetricsUpdated(Vec<Metric>),

    /// Connections list updated
    ConnectionsUpdated(Vec<Connection>),

    /// Daemon status updated (via HTTP)
    StatusUpdated(Box<DaemonStatus>),

    /// gRPC connection status changed
    GrpcStatus(bool),

    /// Warning message (non-fatal)
    Warning(String),

    /// Error occurred
    Error(String),

    /// Tick (for periodic updates)
    Tick,

    /// Connection close result (connection_id on success, error message on failure)
    ConnectionClosed(Result<String, String>),
}
