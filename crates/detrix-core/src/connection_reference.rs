//! Connection reference counting for multi-user safety
//!
//! In cloud mode, multiple MCP bridges share the same Detrix daemon.
//! Reference counting ensures connections are only disconnected when
//! their last reference is removed.

use crate::ConnectionId;
use std::fmt;

/// Identity of a client interacting with connections.
///
/// Type-safe: Daemon identity can never come from a user header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientIdentity {
    /// MCP bridge session (UUID from X-Detrix-Client-Id header)
    Bridge(String),
    /// System/daemon-held reference (internal use only)
    Daemon,
}

/// Sentinel value used for `Daemon` identity in storage.
///
/// Reserved: external callers must never send this value via API.
pub const DAEMON_IDENTITY: &str = "__daemon__";

impl ClientIdentity {
    /// Create a bridge client identity
    pub fn bridge(id: impl Into<String>) -> Self {
        Self::Bridge(id.into())
    }

    /// Create the daemon identity
    pub fn daemon() -> Self {
        Self::Daemon
    }

    /// Check if this is the daemon identity
    pub fn is_daemon(&self) -> bool {
        matches!(self, Self::Daemon)
    }

    /// Get the string representation for storage
    pub fn as_str(&self) -> &str {
        match self {
            Self::Bridge(id) => id,
            Self::Daemon => DAEMON_IDENTITY,
        }
    }
}

impl fmt::Display for ClientIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bridge(id) => write!(f, "bridge:{}", id),
            Self::Daemon => write!(f, "daemon"),
        }
    }
}

impl From<String> for ClientIdentity {
    fn from(s: String) -> Self {
        if s == DAEMON_IDENTITY {
            Self::Daemon
        } else {
            Self::Bridge(s)
        }
    }
}

impl From<&str> for ClientIdentity {
    fn from(s: &str) -> Self {
        if s == DAEMON_IDENTITY {
            Self::Daemon
        } else {
            Self::Bridge(s.to_string())
        }
    }
}

/// Kind of connection reference
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceKind {
    /// Any bridge client reference
    Client,
    /// Daemon/system persistent reference
    Daemon,
}

impl ReferenceKind {
    /// Get string representation for storage
    pub fn as_str(&self) -> &str {
        match self {
            Self::Client => "client",
            Self::Daemon => "daemon",
        }
    }
}

impl fmt::Display for ReferenceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for ReferenceKind {
    fn from(s: &str) -> Self {
        match s {
            "daemon" => Self::Daemon,
            _ => Self::Client,
        }
    }
}

/// A reference held by a client on a connection.
///
/// Connections are only disconnected when their last reference is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionReference {
    /// Connection this reference points to
    pub connection_id: ConnectionId,
    /// Who holds this reference
    pub client_identity: ClientIdentity,
    /// Kind of reference
    pub kind: ReferenceKind,
    /// When this reference was created (microseconds since epoch)
    pub created_at: i64,
    /// When this reference was last active (microseconds since epoch)
    pub last_active: i64,
}

impl ConnectionReference {
    /// Create a new connection reference with current timestamps
    pub fn new(
        connection_id: ConnectionId,
        client_identity: ClientIdentity,
        kind: ReferenceKind,
    ) -> Self {
        let now = chrono::Utc::now().timestamp_micros();
        Self {
            connection_id,
            client_identity,
            kind,
            created_at: now,
            last_active: now,
        }
    }

    /// Update the last_active timestamp to now
    pub fn touch(&mut self) {
        self.last_active = chrono::Utc::now().timestamp_micros();
    }

    /// Check if this reference has been inactive for more than `days` calendar days.
    ///
    /// Uses the same calendar-day logic as `Connection::inactive_for_days`.
    pub fn inactive_for_days(&self, days: i64, now_micros: i64) -> bool {
        if days < 0 {
            return false; // -1 = indefinite
        }
        if days == 0 {
            return true; // 0 = remove all
        }
        let micros_per_day: i64 = 86_400 * 1_000_000;
        let elapsed = now_micros - self.last_active;
        elapsed >= days * micros_per_day
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_identity_bridge_vs_daemon() {
        let bridge = ClientIdentity::bridge("client-123");
        let daemon = ClientIdentity::daemon();

        assert!(!bridge.is_daemon());
        assert!(daemon.is_daemon());
        assert_eq!(bridge.as_str(), "client-123");
        assert_eq!(daemon.as_str(), "__daemon__");
    }

    #[test]
    fn test_client_identity_from_string() {
        let bridge: ClientIdentity = "client-456".into();
        assert!(!bridge.is_daemon());

        let daemon: ClientIdentity = "__daemon__".into();
        assert!(daemon.is_daemon());
    }

    #[test]
    fn test_client_identity_display() {
        let bridge = ClientIdentity::bridge("abc");
        assert_eq!(bridge.to_string(), "bridge:abc");

        let daemon = ClientIdentity::daemon();
        assert_eq!(daemon.to_string(), "daemon");
    }

    #[test]
    fn test_reference_new_sets_timestamps() {
        let r = ConnectionReference::new(
            ConnectionId::from("conn-1"),
            ClientIdentity::bridge("client-1"),
            ReferenceKind::Client,
        );
        assert!(r.created_at > 0);
        assert_eq!(r.created_at, r.last_active);
    }

    #[test]
    fn test_reference_touch_updates_last_active() {
        let mut r = ConnectionReference::new(
            ConnectionId::from("conn-1"),
            ClientIdentity::bridge("client-1"),
            ReferenceKind::Client,
        );
        let original = r.last_active;

        // Sleep briefly to ensure timestamp changes
        std::thread::sleep(std::time::Duration::from_millis(2));
        r.touch();

        assert!(r.last_active >= original);
        assert_eq!(r.created_at, original); // created_at unchanged
    }

    #[test]
    fn test_reference_inactive_for_days() {
        let micros_per_day: i64 = 86_400 * 1_000_000;
        let now = chrono::Utc::now().timestamp_micros();

        let r = ConnectionReference {
            connection_id: ConnectionId::from("conn-1"),
            client_identity: ClientIdentity::bridge("client-1"),
            kind: ReferenceKind::Client,
            created_at: now - 10 * micros_per_day,
            last_active: now - 10 * micros_per_day,
        };

        assert!(r.inactive_for_days(7, now)); // 10 days > 7 days
        assert!(!r.inactive_for_days(14, now)); // 10 days < 14 days
        assert!(!r.inactive_for_days(-1, now)); // indefinite = never expire
        assert!(r.inactive_for_days(0, now)); // 0 = remove all
    }

    #[test]
    fn test_reference_kind_roundtrip() {
        let client = ReferenceKind::Client;
        let daemon = ReferenceKind::Daemon;

        assert_eq!(ReferenceKind::from(client.as_str()), client);
        assert_eq!(ReferenceKind::from(daemon.as_str()), daemon);
        assert_eq!(client.to_string(), "client");
        assert_eq!(daemon.to_string(), "daemon");
    }
}
