//! Connection entity and types for managing debugger connections (debugpy, delve, lldb-dap)

use crate::entities::SourceLanguage;
use crate::{Error, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fmt;

// =============================================================================
// Port Constants
// =============================================================================

/// Minimum unreserved port number (ports 0-1023 are reserved for system services)
pub const MIN_UNRESERVED_PORT: u16 = 1024;

/// Unique identifier for a debugger connection
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionId(pub String);

impl ConnectionId {
    /// Create a new ConnectionId
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Auto-generate connection ID from host and port
    /// Format: "host_port" (e.g., "127_0_0_1_5678")
    pub fn from_host_port(host: &str, port: u16) -> Self {
        let normalized_host = host.replace(['.', ':'], "_");
        Self(format!("{}_{}", normalized_host, port))
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for ConnectionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ConnectionId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Connection status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    /// Connection is disconnected
    Disconnected,
    /// Connection is in progress
    Connecting,
    /// Connection is established and ready
    Connected,
    /// Lost connection, trying to reconnect
    Reconnecting,
    /// Connection failed with error message
    Failed(String),
}

impl ConnectionStatus {
    /// Get the status as a string (for serialization/display)
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::Failed(_) => "failed",
        }
    }

    /// Get a full status string including error message if present
    pub fn to_status_string(&self) -> String {
        match self {
            Self::Failed(msg) => format!("failed: {}", msg),
            _ => self.as_str().to_string(),
        }
    }
}

impl fmt::Display for ConnectionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => write!(f, "disconnected"),
            Self::Connecting => write!(f, "connecting"),
            Self::Connected => write!(f, "connected"),
            Self::Reconnecting => write!(f, "reconnecting"),
            Self::Failed(msg) => write!(f, "failed: {}", msg),
        }
    }
}

impl From<&str> for ConnectionStatus {
    fn from(s: &str) -> Self {
        match s {
            "disconnected" => Self::Disconnected,
            "connecting" => Self::Connecting,
            "connected" => Self::Connected,
            "reconnecting" => Self::Reconnecting,
            s if s.starts_with("failed:") => {
                Self::Failed(s.trim_start_matches("failed:").trim().to_string())
            }
            _ => Self::Disconnected, // Default to disconnected for unknown status
        }
    }
}

/// Connection to a debugger server (debugpy, delve, lldb-dap)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// Unique connection identifier
    pub id: ConnectionId,

    /// User-friendly name (optional alias)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Host address (e.g., "127.0.0.1", "localhost")
    pub host: String,

    /// Port number (1024-65535)
    pub port: u16,

    /// Language/adapter type (Python, Go, Rust, etc.)
    pub language: SourceLanguage,

    /// Current connection status
    pub status: ConnectionStatus,

    /// Should daemon auto-reconnect when this connection is lost?
    #[serde(default = "default_auto_reconnect")]
    pub auto_reconnect: bool,

    /// When the connection was created (microseconds since epoch)
    pub created_at: i64,

    /// Last successful connection timestamp (microseconds since epoch)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_connected_at: Option<i64>,

    /// Last activity timestamp (microseconds since epoch)
    pub last_active: i64,
}

fn default_auto_reconnect() -> bool {
    true
}

impl Connection {
    /// Get current timestamp in microseconds
    fn now_micros() -> i64 {
        Utc::now().timestamp_micros()
    }

    /// Create a new connection
    pub fn new(
        id: ConnectionId,
        host: String,
        port: u16,
        language: SourceLanguage,
    ) -> Result<Self> {
        // Validate port range (MIN_UNRESERVED_PORT-65535)
        if port < MIN_UNRESERVED_PORT {
            return Err(Error::InvalidConfig(format!(
                "Port {} is below {} (reserved range)",
                port, MIN_UNRESERVED_PORT
            )));
        }

        // Validate host is not empty
        if host.is_empty() {
            return Err(Error::InvalidConfig("Host cannot be empty".to_string()));
        }

        // Validate language is known - connections require a specific language adapter
        if language == SourceLanguage::Unknown {
            return Err(Error::InvalidConfig(
                "Unknown language is not allowed for connections. Specify a valid language: python, go, rust, javascript, typescript, java, cpp, c, ruby, php".to_string()
            ));
        }

        let now = Self::now_micros();
        Ok(Self {
            id,
            name: None,
            host,
            port,
            language,
            status: ConnectionStatus::Disconnected,
            auto_reconnect: true,
            created_at: now,
            last_connected_at: None,
            last_active: now,
        })
    }

    /// Create a new connection with auto-generated ID
    pub fn new_with_auto_id(host: String, port: u16, language: SourceLanguage) -> Result<Self> {
        let id = ConnectionId::from_host_port(&host, port);
        Self::new(id, host, port, language)
    }

    /// Get the address in "host:port" format
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Update connection status
    pub fn set_status(&mut self, status: ConnectionStatus) {
        // Update last_connected_at when transitioning to Connected
        if matches!(status, ConnectionStatus::Connected) {
            self.last_connected_at = Some(Self::now_micros());
        }
        self.status = status;
        self.last_active = Self::now_micros();
    }

    /// Check if connection is active (connected or connecting)
    pub fn is_active(&self) -> bool {
        matches!(
            self.status,
            ConnectionStatus::Connected | ConnectionStatus::Connecting
        )
    }

    /// Check if connection should attempt reconnection
    pub fn should_reconnect(&self) -> bool {
        self.auto_reconnect
            && matches!(
                self.status,
                ConnectionStatus::Disconnected
                    | ConnectionStatus::Reconnecting
                    | ConnectionStatus::Failed(_)
            )
    }

    /// Update last active timestamp
    pub fn touch(&mut self) {
        self.last_active = Self::now_micros();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_id_from_host_port() {
        let id = ConnectionId::from_host_port("127.0.0.1", 5678);
        assert_eq!(id.0, "127_0_0_1_5678");

        let id = ConnectionId::from_host_port("localhost", 5678);
        assert_eq!(id.0, "localhost_5678");
    }

    #[test]
    fn test_connection_id_display() {
        let id = ConnectionId::new("test_connection");
        assert_eq!(format!("{}", id), "test_connection");
    }

    #[test]
    fn test_connection_id_from_string() {
        let id = ConnectionId::from("my_connection");
        assert_eq!(id.0, "my_connection");
    }

    #[test]
    fn test_connection_status_display() {
        assert_eq!(
            format!("{}", ConnectionStatus::Disconnected),
            "disconnected"
        );
        assert_eq!(format!("{}", ConnectionStatus::Connecting), "connecting");
        assert_eq!(format!("{}", ConnectionStatus::Connected), "connected");
        assert_eq!(
            format!("{}", ConnectionStatus::Reconnecting),
            "reconnecting"
        );
        assert_eq!(
            format!("{}", ConnectionStatus::Failed("timeout".to_string())),
            "failed: timeout"
        );
    }

    #[test]
    fn test_connection_status_from_str() {
        assert_eq!(
            ConnectionStatus::from("disconnected"),
            ConnectionStatus::Disconnected
        );
        assert_eq!(
            ConnectionStatus::from("connecting"),
            ConnectionStatus::Connecting
        );
        assert_eq!(
            ConnectionStatus::from("connected"),
            ConnectionStatus::Connected
        );
        assert_eq!(
            ConnectionStatus::from("reconnecting"),
            ConnectionStatus::Reconnecting
        );
        assert_eq!(
            ConnectionStatus::from("failed: timeout"),
            ConnectionStatus::Failed("timeout".to_string())
        );
    }

    #[test]
    fn test_connection_new_validates_port() {
        let id = ConnectionId::new("test");

        // Port below MIN_UNRESERVED_PORT should fail
        let result = Connection::new(
            id.clone(),
            "127.0.0.1".to_string(),
            80,
            SourceLanguage::Python,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("reserved range"));

        // Valid port should succeed
        let result = Connection::new(id, "127.0.0.1".to_string(), 5678, SourceLanguage::Python);
        assert!(result.is_ok());
    }

    #[test]
    fn test_connection_new_validates_host() {
        let id = ConnectionId::new("test");

        // Empty host should fail
        let result = Connection::new(id, String::new(), 5678, SourceLanguage::Python);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Host cannot be empty"));
    }

    #[test]
    fn test_connection_new_with_source_language() {
        let id = ConnectionId::new("test");

        // Using SourceLanguage directly
        let result = Connection::new(
            id.clone(),
            "127.0.0.1".to_string(),
            5678,
            SourceLanguage::Python,
        );
        assert!(result.is_ok());
        let conn = result.unwrap();
        assert_eq!(conn.language, SourceLanguage::Python);

        // Using Go language
        let result = Connection::new(
            ConnectionId::new("test2"),
            "127.0.0.1".to_string(),
            5679,
            SourceLanguage::Go,
        );
        assert!(result.is_ok());
        let conn = result.unwrap();
        assert_eq!(conn.language, SourceLanguage::Go);

        // Unknown language is rejected
        let result = Connection::new(
            ConnectionId::new("test3"),
            "127.0.0.1".to_string(),
            5680,
            SourceLanguage::Unknown,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Unknown language is not allowed"));
    }

    #[test]
    fn test_connection_new_sets_initial_state() {
        let id = ConnectionId::new("test");
        let conn = Connection::new(
            id.clone(),
            "127.0.0.1".to_string(),
            5678,
            SourceLanguage::Python,
        )
        .unwrap();

        assert_eq!(conn.id, id);
        assert_eq!(conn.host, "127.0.0.1");
        assert_eq!(conn.port, 5678);
        assert_eq!(conn.language, SourceLanguage::Python);
        assert_eq!(conn.status, ConnectionStatus::Disconnected);
        assert!(conn.auto_reconnect);
        assert!(conn.created_at > 0);
        assert_eq!(conn.created_at, conn.last_active);
        assert!(conn.last_connected_at.is_none());
    }

    #[test]
    fn test_connection_new_with_auto_id() {
        let conn =
            Connection::new_with_auto_id("127.0.0.1".to_string(), 5678, SourceLanguage::Python)
                .unwrap();

        assert_eq!(conn.id.0, "127_0_0_1_5678");
        assert_eq!(conn.host, "127.0.0.1");
        assert_eq!(conn.port, 5678);
        assert_eq!(conn.language, SourceLanguage::Python);
    }

    #[test]
    fn test_connection_address() {
        let conn =
            Connection::new_with_auto_id("127.0.0.1".to_string(), 5678, SourceLanguage::Python)
                .unwrap();
        assert_eq!(conn.address(), "127.0.0.1:5678");
    }

    #[test]
    fn test_connection_set_status_updates_timestamp() {
        let mut conn =
            Connection::new_with_auto_id("127.0.0.1".to_string(), 5678, SourceLanguage::Python)
                .unwrap();

        let initial_timestamp = conn.last_active;

        // Small delay to ensure timestamp changes
        std::thread::sleep(std::time::Duration::from_millis(10));

        conn.set_status(ConnectionStatus::Connected);

        assert_eq!(conn.status, ConnectionStatus::Connected);
        assert!(conn.last_active > initial_timestamp);
        assert!(conn.last_connected_at.is_some()); // Should be set on Connected
    }

    #[test]
    fn test_connection_is_active() {
        let mut conn =
            Connection::new_with_auto_id("127.0.0.1".to_string(), 5678, SourceLanguage::Python)
                .unwrap();

        // Disconnected is not active
        conn.status = ConnectionStatus::Disconnected;
        assert!(!conn.is_active());

        // Connecting is active
        conn.status = ConnectionStatus::Connecting;
        assert!(conn.is_active());

        // Connected is active
        conn.status = ConnectionStatus::Connected;
        assert!(conn.is_active());

        // Reconnecting is not active
        conn.status = ConnectionStatus::Reconnecting;
        assert!(!conn.is_active());

        // Failed is not active
        conn.status = ConnectionStatus::Failed("error".to_string());
        assert!(!conn.is_active());
    }

    #[test]
    fn test_connection_should_reconnect() {
        let mut conn =
            Connection::new_with_auto_id("127.0.0.1".to_string(), 5678, SourceLanguage::Python)
                .unwrap();

        // With auto_reconnect = true (default)
        conn.status = ConnectionStatus::Disconnected;
        assert!(conn.should_reconnect());

        conn.status = ConnectionStatus::Reconnecting;
        assert!(conn.should_reconnect());

        conn.status = ConnectionStatus::Failed("error".to_string());
        assert!(conn.should_reconnect());

        conn.status = ConnectionStatus::Connected;
        assert!(!conn.should_reconnect());

        conn.status = ConnectionStatus::Connecting;
        assert!(!conn.should_reconnect());

        // With auto_reconnect = false
        conn.auto_reconnect = false;
        conn.status = ConnectionStatus::Disconnected;
        assert!(!conn.should_reconnect());
    }

    #[test]
    fn test_connection_touch_updates_timestamp() {
        let mut conn =
            Connection::new_with_auto_id("127.0.0.1".to_string(), 5678, SourceLanguage::Python)
                .unwrap();

        let initial_timestamp = conn.last_active;

        std::thread::sleep(std::time::Duration::from_millis(10));

        conn.touch();

        assert!(conn.last_active > initial_timestamp);
    }

    #[test]
    fn test_connection_serialization() {
        let conn =
            Connection::new_with_auto_id("127.0.0.1".to_string(), 5678, SourceLanguage::Python)
                .unwrap();

        // Test serialization
        let json = serde_json::to_string(&conn).unwrap();
        assert!(json.contains("127_0_0_1_5678"));
        assert!(json.contains("127.0.0.1"));
        assert!(json.contains("5678"));
        assert!(json.contains("\"python\"")); // Serialized as lowercase

        // Test deserialization
        let deserialized: Connection = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, conn);
    }
}
