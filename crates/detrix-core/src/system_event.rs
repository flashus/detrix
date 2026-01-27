//! System events for real-time streaming and persistence
//!
//! These events represent system-level occurrences like debugger crashes,
//! connection changes, and metric lifecycle events. They are streamed to
//! MCP clients via progress notifications and persisted for catch-up queries.

use crate::ConnectionId;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Types of system events
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemEventType {
    // Metric lifecycle
    MetricAdded,
    MetricRemoved,
    MetricToggled,
    /// Metric was automatically relocated due to code changes
    MetricRelocated,
    /// Metric could not be relocated and needs manual intervention
    MetricOrphaned,

    // Connection lifecycle
    ConnectionCreated,
    ConnectionClosed,
    ConnectionLost,
    ConnectionRestored,

    // Debugger health
    DebuggerCrash,

    // Audit events (C-02: Config, C-03: User actions)
    /// Configuration was updated
    ConfigUpdated,
    /// Configuration validation failed
    ConfigValidationFailed,
    /// API call was executed successfully
    ApiCallExecuted,
    /// API call failed
    ApiCallFailed,
    /// Authentication attempt failed
    AuthenticationFailed,
    /// Expression was validated (safety check)
    ExpressionValidated,
}

impl SystemEventType {
    /// Get the string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MetricAdded => "metric_added",
            Self::MetricRemoved => "metric_removed",
            Self::MetricToggled => "metric_toggled",
            Self::MetricRelocated => "metric_relocated",
            Self::MetricOrphaned => "metric_orphaned",
            Self::ConnectionCreated => "connection_created",
            Self::ConnectionClosed => "connection_closed",
            Self::ConnectionLost => "connection_lost",
            Self::ConnectionRestored => "connection_restored",
            Self::DebuggerCrash => "debugger_crash",
            // Audit events
            Self::ConfigUpdated => "config_updated",
            Self::ConfigValidationFailed => "config_validation_failed",
            Self::ApiCallExecuted => "api_call_executed",
            Self::ApiCallFailed => "api_call_failed",
            Self::AuthenticationFailed => "authentication_failed",
            Self::ExpressionValidated => "expression_validated",
        }
    }

    /// Parse from string
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "metric_added" => Some(Self::MetricAdded),
            "metric_removed" => Some(Self::MetricRemoved),
            "metric_toggled" => Some(Self::MetricToggled),
            "metric_relocated" => Some(Self::MetricRelocated),
            "metric_orphaned" => Some(Self::MetricOrphaned),
            "connection_created" => Some(Self::ConnectionCreated),
            "connection_closed" => Some(Self::ConnectionClosed),
            "connection_lost" => Some(Self::ConnectionLost),
            "connection_restored" => Some(Self::ConnectionRestored),
            "debugger_crash" => Some(Self::DebuggerCrash),
            // Audit events
            "config_updated" => Some(Self::ConfigUpdated),
            "config_validation_failed" => Some(Self::ConfigValidationFailed),
            "api_call_executed" => Some(Self::ApiCallExecuted),
            "api_call_failed" => Some(Self::ApiCallFailed),
            "authentication_failed" => Some(Self::AuthenticationFailed),
            "expression_validated" => Some(Self::ExpressionValidated),
            _ => None,
        }
    }

    /// Check if this is a critical event that should always be persisted
    pub fn is_critical(&self) -> bool {
        matches!(
            self,
            Self::DebuggerCrash | Self::ConnectionLost | Self::ConnectionRestored
        )
    }

    /// Check if this is an audit event
    pub fn is_audit(&self) -> bool {
        matches!(
            self,
            Self::ConfigUpdated
                | Self::ConfigValidationFailed
                | Self::ApiCallExecuted
                | Self::ApiCallFailed
                | Self::AuthenticationFailed
                | Self::ExpressionValidated
        )
    }
}

impl fmt::Display for SystemEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for SystemEventType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_str(s).ok_or_else(|| format!("Unknown system event type: {}", s))
    }
}

/// A system event for real-time streaming and persistence
///
/// These events are emitted by services (MetricService, ConnectionService,
/// AdapterLifecycleManager) and broadcast to MCP clients via progress
/// notifications. Critical events are also persisted to the database.
///
/// Audit events use additional fields: actor, resource_type, resource_id,
/// old_value, and new_value to track who changed what.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEvent {
    /// Unique event ID (set by database on insert)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,

    /// Event type
    pub event_type: SystemEventType,

    /// Associated connection ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<ConnectionId>,

    /// Associated metric ID (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_id: Option<u64>,

    /// Associated metric name (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_name: Option<String>,

    /// Event timestamp (microseconds since epoch)
    pub timestamp: i64,

    /// Additional details as JSON
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details_json: Option<String>,

    /// Whether this event has been acknowledged by client
    #[serde(default)]
    pub acknowledged: bool,

    // === Audit fields (for audit events) ===
    /// Who triggered the action (user ID, client ID, "system", etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,

    /// Type of resource affected (e.g., "config", "metric", "connection")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,

    /// ID of the affected resource
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,

    /// Previous state (JSON, for change tracking)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_value: Option<String>,

    /// New state (JSON, for change tracking)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_value: Option<String>,
}

impl SystemEvent {
    /// Create a new system event with the current timestamp
    pub fn new(event_type: SystemEventType) -> Self {
        Self {
            id: None,
            event_type,
            connection_id: None,
            metric_id: None,
            metric_name: None,
            timestamp: Utc::now().timestamp_micros(),
            details_json: None,
            acknowledged: false,
            // Audit fields
            actor: None,
            resource_type: None,
            resource_id: None,
            old_value: None,
            new_value: None,
        }
    }

    /// Set the connection ID
    pub fn with_connection(mut self, connection_id: impl Into<ConnectionId>) -> Self {
        self.connection_id = Some(connection_id.into());
        self
    }

    /// Set the metric info
    pub fn with_metric(mut self, metric_id: u64, metric_name: impl Into<String>) -> Self {
        self.metric_id = Some(metric_id);
        self.metric_name = Some(metric_name.into());
        self
    }

    /// Set additional details (will be serialized to JSON)
    pub fn with_details<T: Serialize>(mut self, details: &T) -> Self {
        self.details_json = serde_json::to_string(details).ok();
        self
    }

    /// Set raw JSON details
    pub fn with_details_json(mut self, json: impl Into<String>) -> Self {
        self.details_json = Some(json.into());
        self
    }

    /// Set the actor (who triggered the action)
    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    /// Set the resource being affected
    pub fn with_resource(
        mut self,
        resource_type: impl Into<String>,
        resource_id: impl Into<String>,
    ) -> Self {
        self.resource_type = Some(resource_type.into());
        self.resource_id = Some(resource_id.into());
        self
    }

    /// Set the change (old and new values)
    pub fn with_change(
        mut self,
        old_value: impl Into<String>,
        new_value: impl Into<String>,
    ) -> Self {
        self.old_value = Some(old_value.into());
        self.new_value = Some(new_value.into());
        self
    }

    /// Check if this event should be persisted
    pub fn should_persist(&self) -> bool {
        // All system events are persisted
        true
    }

    /// Check if this is a critical event
    pub fn is_critical(&self) -> bool {
        self.event_type.is_critical()
    }

    /// Check if this is an audit event
    pub fn is_audit(&self) -> bool {
        self.event_type.is_audit()
    }

    // === Factory methods for common events ===

    /// Create a metric added event
    pub fn metric_added(
        metric_id: u64,
        metric_name: impl Into<String>,
        connection_id: impl Into<ConnectionId>,
    ) -> Self {
        Self::new(SystemEventType::MetricAdded)
            .with_metric(metric_id, metric_name)
            .with_connection(connection_id)
    }

    /// Create a metric removed event
    pub fn metric_removed(
        metric_id: u64,
        metric_name: impl Into<String>,
        connection_id: impl Into<ConnectionId>,
    ) -> Self {
        Self::new(SystemEventType::MetricRemoved)
            .with_metric(metric_id, metric_name)
            .with_connection(connection_id)
    }

    /// Create a metric toggled event
    pub fn metric_toggled(
        metric_id: u64,
        metric_name: impl Into<String>,
        enabled: bool,
        connection_id: impl Into<ConnectionId>,
    ) -> Self {
        Self::new(SystemEventType::MetricToggled)
            .with_metric(metric_id, metric_name)
            .with_connection(connection_id)
            .with_details(&serde_json::json!({ "enabled": enabled }))
    }

    /// Create a metric relocated event (auto-relocated due to code changes)
    pub fn metric_relocated(
        metric_id: u64,
        metric_name: impl Into<String>,
        old_line: u32,
        new_line: u32,
        symbol_name: Option<String>,
        connection_id: impl Into<ConnectionId>,
    ) -> Self {
        Self::new(SystemEventType::MetricRelocated)
            .with_metric(metric_id, metric_name)
            .with_connection(connection_id)
            .with_details(&serde_json::json!({
                "old_line": old_line,
                "new_line": new_line,
                "symbol_name": symbol_name
            }))
    }

    /// Create a metric orphaned event (could not relocate, needs manual fix)
    pub fn metric_orphaned(
        metric_id: u64,
        metric_name: impl Into<String>,
        last_known_location: impl Into<String>,
        reason: impl Into<String>,
        connection_id: impl Into<ConnectionId>,
    ) -> Self {
        Self::new(SystemEventType::MetricOrphaned)
            .with_metric(metric_id, metric_name)
            .with_connection(connection_id)
            .with_details(&serde_json::json!({
                "last_known_location": last_known_location.into(),
                "reason": reason.into()
            }))
    }

    /// Create a connection created event
    pub fn connection_created(
        connection_id: impl Into<ConnectionId>,
        host: &str,
        port: u16,
        language: &str,
    ) -> Self {
        Self::new(SystemEventType::ConnectionCreated)
            .with_connection(connection_id)
            .with_details(&serde_json::json!({
                "host": host,
                "port": port,
                "language": language
            }))
    }

    /// Create a connection closed event
    pub fn connection_closed(connection_id: impl Into<ConnectionId>) -> Self {
        Self::new(SystemEventType::ConnectionClosed).with_connection(connection_id)
    }

    /// Create a connection lost event
    pub fn connection_lost(connection_id: impl Into<ConnectionId>, error: &str) -> Self {
        Self::new(SystemEventType::ConnectionLost)
            .with_connection(connection_id)
            .with_details(&serde_json::json!({ "error": error }))
    }

    /// Create a connection restored event
    pub fn connection_restored(
        connection_id: impl Into<ConnectionId>,
        reconnect_attempts: u32,
    ) -> Self {
        Self::new(SystemEventType::ConnectionRestored)
            .with_connection(connection_id)
            .with_details(&serde_json::json!({ "reconnect_attempts": reconnect_attempts }))
    }

    /// Create a debugger crash event
    pub fn debugger_crash(
        connection_id: impl Into<ConnectionId>,
        error: &str,
        language: &str,
    ) -> Self {
        Self::new(SystemEventType::DebuggerCrash)
            .with_connection(connection_id)
            .with_details(&serde_json::json!({
                "error": error,
                "language": language
            }))
    }

    // === Audit event factory methods ===

    /// Create a config updated audit event
    pub fn config_updated(
        actor: impl Into<String>,
        old_config: impl Into<String>,
        new_config: impl Into<String>,
    ) -> Self {
        Self::new(SystemEventType::ConfigUpdated)
            .with_actor(actor)
            .with_resource("config", "global")
            .with_change(old_config, new_config)
    }

    /// Create a config validation failed audit event
    pub fn config_validation_failed(
        actor: impl Into<String>,
        error: impl Into<String>,
        attempted_config: impl Into<String>,
    ) -> Self {
        Self::new(SystemEventType::ConfigValidationFailed)
            .with_actor(actor)
            .with_resource("config", "global")
            .with_details(&serde_json::json!({
                "error": error.into(),
                "attempted_config": attempted_config.into()
            }))
    }

    /// Create an API call executed audit event
    pub fn api_call(
        actor: impl Into<String>,
        method: impl Into<String>,
        endpoint: impl Into<String>,
        resource_type: impl Into<String>,
        resource_id: Option<String>,
    ) -> Self {
        Self::new(SystemEventType::ApiCallExecuted)
            .with_actor(actor)
            .with_resource(resource_type, resource_id.unwrap_or_default())
            .with_details(&serde_json::json!({
                "method": method.into(),
                "endpoint": endpoint.into()
            }))
    }

    /// Create an API call failed audit event
    pub fn api_call_failed(
        actor: impl Into<String>,
        method: impl Into<String>,
        endpoint: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self::new(SystemEventType::ApiCallFailed)
            .with_actor(actor)
            .with_details(&serde_json::json!({
                "method": method.into(),
                "endpoint": endpoint.into(),
                "error": error.into()
            }))
    }

    /// Create an authentication failed audit event
    pub fn authentication_failed(
        actor: impl Into<String>,
        reason: impl Into<String>,
        ip_address: Option<String>,
    ) -> Self {
        let mut event = Self::new(SystemEventType::AuthenticationFailed)
            .with_actor(actor)
            .with_details(&serde_json::json!({
                "reason": reason.into(),
                "ip_address": ip_address
            }));
        event.resource_type = Some("authentication".to_string());
        event
    }

    /// Create an expression validated audit event
    pub fn expression_validated(
        actor: impl Into<String>,
        expression: impl Into<String>,
        language: impl Into<String>,
        is_safe: bool,
        rejection_reason: Option<String>,
    ) -> Self {
        Self::new(SystemEventType::ExpressionValidated)
            .with_actor(actor)
            .with_resource("expression", "")
            .with_details(&serde_json::json!({
                "expression": expression.into(),
                "language": language.into(),
                "is_safe": is_safe,
                "rejection_reason": rejection_reason
            }))
    }
}

impl fmt::Display for SystemEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.event_type)?;
        if let Some(ref conn) = self.connection_id {
            write!(f, " [{}]", conn)?;
        }
        if let Some(ref name) = self.metric_name {
            write!(f, " metric={}", name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_event_type_as_str() {
        assert_eq!(SystemEventType::MetricAdded.as_str(), "metric_added");
        assert_eq!(SystemEventType::DebuggerCrash.as_str(), "debugger_crash");
    }

    #[test]
    fn test_system_event_type_parse_str() {
        assert_eq!(
            SystemEventType::parse_str("metric_added"),
            Some(SystemEventType::MetricAdded)
        );
        assert_eq!(
            SystemEventType::parse_str("debugger_crash"),
            Some(SystemEventType::DebuggerCrash)
        );
        assert_eq!(SystemEventType::parse_str("unknown"), None);
    }

    #[test]
    fn test_system_event_type_is_critical() {
        assert!(SystemEventType::DebuggerCrash.is_critical());
        assert!(SystemEventType::ConnectionLost.is_critical());
        assert!(SystemEventType::ConnectionRestored.is_critical());
        assert!(!SystemEventType::MetricAdded.is_critical());
        assert!(!SystemEventType::ConnectionCreated.is_critical());
    }

    #[test]
    fn test_system_event_new() {
        let event = SystemEvent::new(SystemEventType::MetricAdded);
        assert_eq!(event.event_type, SystemEventType::MetricAdded);
        assert!(event.timestamp > 0);
        assert!(event.id.is_none());
        assert!(!event.acknowledged);
    }

    #[test]
    fn test_system_event_with_connection() {
        let event =
            SystemEvent::new(SystemEventType::ConnectionCreated).with_connection("test-conn");
        assert_eq!(event.connection_id, Some(ConnectionId::from("test-conn")));
    }

    #[test]
    fn test_system_event_with_metric() {
        let event = SystemEvent::new(SystemEventType::MetricAdded).with_metric(42, "my_metric");
        assert_eq!(event.metric_id, Some(42));
        assert_eq!(event.metric_name, Some("my_metric".to_string()));
    }

    #[test]
    fn test_system_event_with_details() {
        let event = SystemEvent::new(SystemEventType::DebuggerCrash)
            .with_details(&serde_json::json!({"error": "connection reset"}));
        assert!(event.details_json.is_some());
        let details: serde_json::Value =
            serde_json::from_str(event.details_json.as_ref().unwrap()).unwrap();
        assert_eq!(details["error"], "connection reset");
    }

    #[test]
    fn test_system_event_factory_metric_added() {
        let event = SystemEvent::metric_added(1, "test_metric", "conn-1");
        assert_eq!(event.event_type, SystemEventType::MetricAdded);
        assert_eq!(event.metric_id, Some(1));
        assert_eq!(event.metric_name, Some("test_metric".to_string()));
        assert_eq!(event.connection_id, Some(ConnectionId::from("conn-1")));
    }

    #[test]
    fn test_system_event_factory_debugger_crash() {
        let event = SystemEvent::debugger_crash("conn-1", "connection reset", "python");
        assert_eq!(event.event_type, SystemEventType::DebuggerCrash);
        assert_eq!(event.connection_id, Some(ConnectionId::from("conn-1")));
        assert!(event.is_critical());

        let details: serde_json::Value =
            serde_json::from_str(event.details_json.as_ref().unwrap()).unwrap();
        assert_eq!(details["error"], "connection reset");
        assert_eq!(details["language"], "python");
    }

    #[test]
    fn test_system_event_serialization() {
        let event = SystemEvent::metric_added(1, "test", "conn");
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: SystemEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.event_type, event.event_type);
        assert_eq!(deserialized.metric_id, event.metric_id);
        assert_eq!(deserialized.metric_name, event.metric_name);
    }

    #[test]
    fn test_system_event_display() {
        let event = SystemEvent::metric_added(1, "user_count", "python-main");
        let display = format!("{}", event);
        assert!(display.contains("metric_added"));
        assert!(display.contains("python-main"));
        assert!(display.contains("user_count"));
    }

    // === Audit event tests ===

    #[test]
    fn test_audit_event_type_is_audit() {
        assert!(SystemEventType::ConfigUpdated.is_audit());
        assert!(SystemEventType::ConfigValidationFailed.is_audit());
        assert!(SystemEventType::ApiCallExecuted.is_audit());
        assert!(SystemEventType::ApiCallFailed.is_audit());
        assert!(SystemEventType::AuthenticationFailed.is_audit());
        assert!(SystemEventType::ExpressionValidated.is_audit());
        // Non-audit events
        assert!(!SystemEventType::MetricAdded.is_audit());
        assert!(!SystemEventType::DebuggerCrash.is_audit());
    }

    #[test]
    fn test_audit_event_type_as_str() {
        assert_eq!(SystemEventType::ConfigUpdated.as_str(), "config_updated");
        assert_eq!(
            SystemEventType::AuthenticationFailed.as_str(),
            "authentication_failed"
        );
    }

    #[test]
    fn test_audit_event_type_parse_str() {
        assert_eq!(
            SystemEventType::parse_str("config_updated"),
            Some(SystemEventType::ConfigUpdated)
        );
        assert_eq!(
            SystemEventType::parse_str("api_call_executed"),
            Some(SystemEventType::ApiCallExecuted)
        );
    }

    #[test]
    fn test_config_updated_event() {
        let event = SystemEvent::config_updated("user-123", "old config", "new config");

        assert_eq!(event.event_type, SystemEventType::ConfigUpdated);
        assert_eq!(event.actor, Some("user-123".to_string()));
        assert_eq!(event.resource_type, Some("config".to_string()));
        assert_eq!(event.resource_id, Some("global".to_string()));
        assert_eq!(event.old_value, Some("old config".to_string()));
        assert_eq!(event.new_value, Some("new config".to_string()));
        assert!(event.is_audit());
    }

    #[test]
    fn test_config_validation_failed_event() {
        let event = SystemEvent::config_validation_failed("user-123", "invalid TOML", "bad config");

        assert_eq!(event.event_type, SystemEventType::ConfigValidationFailed);
        assert_eq!(event.actor, Some("user-123".to_string()));
        assert!(event.details_json.is_some());

        let details: serde_json::Value =
            serde_json::from_str(event.details_json.as_ref().unwrap()).unwrap();
        assert_eq!(details["error"], "invalid TOML");
    }

    #[test]
    fn test_api_call_event() {
        let event = SystemEvent::api_call(
            "client-abc",
            "POST",
            "/api/metrics",
            "metric",
            Some("metric-1".to_string()),
        );

        assert_eq!(event.event_type, SystemEventType::ApiCallExecuted);
        assert_eq!(event.actor, Some("client-abc".to_string()));
        assert_eq!(event.resource_type, Some("metric".to_string()));
        assert_eq!(event.resource_id, Some("metric-1".to_string()));
    }

    #[test]
    fn test_authentication_failed_event() {
        let event = SystemEvent::authentication_failed(
            "unknown",
            "invalid token",
            Some("192.168.1.1".to_string()),
        );

        assert_eq!(event.event_type, SystemEventType::AuthenticationFailed);
        assert_eq!(event.actor, Some("unknown".to_string()));
        assert_eq!(event.resource_type, Some("authentication".to_string()));

        let details: serde_json::Value =
            serde_json::from_str(event.details_json.as_ref().unwrap()).unwrap();
        assert_eq!(details["reason"], "invalid token");
        assert_eq!(details["ip_address"], "192.168.1.1");
    }

    #[test]
    fn test_expression_validated_event() {
        let event = SystemEvent::expression_validated(
            "system",
            "os.system('rm -rf /')",
            "python",
            false,
            Some("prohibited function: os.system".to_string()),
        );

        assert_eq!(event.event_type, SystemEventType::ExpressionValidated);
        assert!(event.is_audit());

        let details: serde_json::Value =
            serde_json::from_str(event.details_json.as_ref().unwrap()).unwrap();
        assert_eq!(details["is_safe"], false);
        assert!(details["rejection_reason"]
            .as_str()
            .unwrap()
            .contains("prohibited"));
    }

    #[test]
    fn test_audit_event_serialization() {
        let event = SystemEvent::config_updated("user-1", "old", "new");
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: SystemEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.event_type, event.event_type);
        assert_eq!(deserialized.actor, event.actor);
        assert_eq!(deserialized.resource_type, event.resource_type);
        assert_eq!(deserialized.old_value, event.old_value);
        assert_eq!(deserialized.new_value, event.new_value);
    }
}
