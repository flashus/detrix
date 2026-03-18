//! MetricScope — multi-tenant access control for metric operations.
//!
//! Determines what a caller can read and mutate based on their identity.

use detrix_config::UserRole;
use detrix_core::Metric;

/// Access scope for metric operations.
///
/// Derived from the authenticated user + optional agent identity.
/// Used by service methods to enforce per-user and per-agent access control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricScope {
    /// Full access to all metrics (admin role)
    Admin,
    /// User-level access (can read all own metrics, mutate own metrics)
    User(String),
    /// Agent-level access (can read all user's metrics, mutate only own)
    Agent { user_id: String, agent_id: String },
}

impl MetricScope {
    /// Check if the caller can read this metric (list/query/get events).
    ///
    /// - Admin: can read everything
    /// - User/Agent: can read metrics owned by the same user_id
    pub fn can_read(&self, metric: &Metric) -> bool {
        match self {
            MetricScope::Admin => true,
            MetricScope::User(uid) => metric.user_id.as_deref() == Some(uid.as_str()),
            MetricScope::Agent { user_id, .. } => {
                metric.user_id.as_deref() == Some(user_id.as_str())
            }
        }
    }

    /// Check if the caller can mutate this metric (update/delete/toggle).
    ///
    /// - Admin: can mutate everything
    /// - User (no agent): can mutate any of their own metrics
    /// - Agent: can only mutate metrics created by the same agent
    pub fn can_mutate(&self, metric: &Metric) -> bool {
        match self {
            MetricScope::Admin => true,
            MetricScope::User(uid) => metric.user_id.as_deref() == Some(uid.as_str()),
            MetricScope::Agent { user_id, agent_id } => {
                metric.user_id.as_deref() == Some(user_id.as_str())
                    && metric.agent_id.as_deref() == Some(agent_id.as_str())
            }
        }
    }

    /// Get the user_id for this scope, if any.
    pub fn user_id(&self) -> Option<&str> {
        match self {
            MetricScope::Admin => None,
            MetricScope::User(uid) => Some(uid),
            MetricScope::Agent { user_id, .. } => Some(user_id),
        }
    }

    /// Get the agent_id for this scope, if any.
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            MetricScope::Admin => None,
            MetricScope::User(_) => None,
            MetricScope::Agent { agent_id, .. } => Some(agent_id),
        }
    }
}

impl std::fmt::Display for MetricScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetricScope::Admin => write!(f, "Admin"),
            MetricScope::User(uid) => write!(f, "User({})", uid),
            MetricScope::Agent { user_id, agent_id } => {
                write!(f, "Agent({}/{})", user_id, agent_id)
            }
        }
    }
}

/// Defense-in-depth: check that `scope` can read `metric`, returning NotFound on failure.
///
/// API handlers perform the primary enforcement; this helper is available for
/// service-level guards where a consistent error type is preferred.
pub fn check_read_access(scope: &MetricScope, metric: &Metric) -> Result<(), detrix_core::Error> {
    if scope.can_read(metric) {
        Ok(())
    } else {
        tracing::debug!(
            scope = ?scope,
            metric_user_id = ?metric.user_id,
            metric_agent_id = ?metric.agent_id,
            metric_id = ?metric.id,
            "Read access denied by scope"
        );
        Err(detrix_core::Error::metric_not_found("Metric not found"))
    }
}

/// Build a MetricScope from an authenticated user and optional agent identity.
pub fn extract_scope(user_id: &str, role: &UserRole, agent_id: Option<String>) -> MetricScope {
    if *role == UserRole::Admin {
        return MetricScope::Admin;
    }
    match agent_id {
        Some(aid) => MetricScope::Agent {
            user_id: user_id.to_string(),
            agent_id: aid,
        },
        None => MetricScope::User(user_id.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_core::{ConnectionId, Location, MetricMode, SafetyLevel, SourceLanguage};

    fn make_metric(user_id: Option<&str>, agent_id: Option<&str>) -> Metric {
        Metric {
            id: Some(detrix_core::MetricId(1)),
            name: "test".to_string(),
            connection_id: ConnectionId::from("conn1"),
            group: None,
            location: Location {
                file: "test.py".to_string(),
                line: 42,
            },
            expressions: vec!["x".to_string()],
            language: SourceLanguage::Python,
            enabled: true,
            mode: MetricMode::Stream,
            condition: None,
            safety_level: SafetyLevel::Strict,
            created_at: None,
            user_id: user_id.map(|s| s.to_string()),
            agent_id: agent_id.map(|s| s.to_string()),
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            anchor: None,
            anchor_status: Default::default(),
        }
    }

    // ========================================================================
    // can_read tests
    // ========================================================================

    #[test]
    fn test_admin_can_read_any_metric() {
        let scope = MetricScope::Admin;
        assert!(scope.can_read(&make_metric(Some("alice"), Some("agent1"))));
        assert!(scope.can_read(&make_metric(Some("bob"), None)));
        assert!(scope.can_read(&make_metric(None, None)));
    }

    #[test]
    fn test_user_can_read_own_metrics() {
        let scope = MetricScope::User("alice".to_string());
        assert!(scope.can_read(&make_metric(Some("alice"), Some("agent1"))));
        assert!(scope.can_read(&make_metric(Some("alice"), Some("agent2"))));
        assert!(scope.can_read(&make_metric(Some("alice"), None)));
    }

    #[test]
    fn test_user_cannot_read_other_user_metrics() {
        let scope = MetricScope::User("alice".to_string());
        assert!(!scope.can_read(&make_metric(Some("bob"), Some("agent1"))));
        assert!(!scope.can_read(&make_metric(None, None))); // unowned metrics
    }

    #[test]
    fn test_agent_can_read_all_same_user_metrics() {
        let scope = MetricScope::Agent {
            user_id: "alice".to_string(),
            agent_id: "agent1".to_string(),
        };
        // Can read own metrics
        assert!(scope.can_read(&make_metric(Some("alice"), Some("agent1"))));
        // Can read other agent's metrics (same user) — read-only
        assert!(scope.can_read(&make_metric(Some("alice"), Some("agent2"))));
        // Cannot read other user's metrics
        assert!(!scope.can_read(&make_metric(Some("bob"), Some("agent1"))));
    }

    // ========================================================================
    // can_mutate tests
    // ========================================================================

    #[test]
    fn test_admin_can_mutate_any_metric() {
        let scope = MetricScope::Admin;
        assert!(scope.can_mutate(&make_metric(Some("alice"), Some("agent1"))));
        assert!(scope.can_mutate(&make_metric(Some("bob"), None)));
        assert!(scope.can_mutate(&make_metric(None, None)));
    }

    #[test]
    fn test_user_can_mutate_own_metrics() {
        let scope = MetricScope::User("alice".to_string());
        assert!(scope.can_mutate(&make_metric(Some("alice"), Some("agent1"))));
        assert!(scope.can_mutate(&make_metric(Some("alice"), None)));
    }

    #[test]
    fn test_user_cannot_mutate_other_user_metrics() {
        let scope = MetricScope::User("alice".to_string());
        assert!(!scope.can_mutate(&make_metric(Some("bob"), Some("agent1"))));
    }

    #[test]
    fn test_agent_can_mutate_own_metrics_only() {
        let scope = MetricScope::Agent {
            user_id: "alice".to_string(),
            agent_id: "agent1".to_string(),
        };
        // Can mutate own metrics
        assert!(scope.can_mutate(&make_metric(Some("alice"), Some("agent1"))));
        // Cannot mutate other agent's metrics (same user)
        assert!(!scope.can_mutate(&make_metric(Some("alice"), Some("agent2"))));
        // Cannot mutate other user's metrics
        assert!(!scope.can_mutate(&make_metric(Some("bob"), Some("agent1"))));
    }

    #[test]
    fn test_agent_cannot_mutate_user_metric_without_agent() {
        let scope = MetricScope::Agent {
            user_id: "alice".to_string(),
            agent_id: "agent1".to_string(),
        };
        // Metric has no agent_id → agent scope cannot mutate it
        assert!(!scope.can_mutate(&make_metric(Some("alice"), None)));
    }

    // ========================================================================
    // extract_scope tests
    // ========================================================================

    #[test]
    fn test_extract_scope_admin() {
        let scope = extract_scope("admin", &UserRole::Admin, Some("agent1".to_string()));
        assert_eq!(scope, MetricScope::Admin);
    }

    #[test]
    fn test_extract_scope_user_without_agent() {
        let scope = extract_scope("alice", &UserRole::User, None);
        assert_eq!(scope, MetricScope::User("alice".to_string()));
    }

    #[test]
    fn test_extract_scope_user_with_agent() {
        let scope = extract_scope("alice", &UserRole::User, Some("agent1".to_string()));
        assert_eq!(
            scope,
            MetricScope::Agent {
                user_id: "alice".to_string(),
                agent_id: "agent1".to_string(),
            }
        );
    }

    // ========================================================================
    // accessor tests
    // ========================================================================

    #[test]
    fn test_scope_accessors() {
        assert_eq!(MetricScope::Admin.user_id(), None);
        assert_eq!(MetricScope::Admin.agent_id(), None);

        let user_scope = MetricScope::User("alice".to_string());
        assert_eq!(user_scope.user_id(), Some("alice"));
        assert_eq!(user_scope.agent_id(), None);

        let agent_scope = MetricScope::Agent {
            user_id: "alice".to_string(),
            agent_id: "agent1".to_string(),
        };
        assert_eq!(agent_scope.user_id(), Some("alice"));
        assert_eq!(agent_scope.agent_id(), Some("agent1"));
    }
}
