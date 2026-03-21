//! Core metric entity and related types

use super::anchor::{AnchorStatus, MetricAnchor};
use super::language::SourceLanguage;
use super::location::Location;
use super::memory::{SnapshotScope, StackTraceSlice};
use crate::connection::ConnectionId;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

// =============================================================================
// Mode Type Constants
// =============================================================================
// These constants are the canonical string representations for MetricMode variants.
// Used for serialization/deserialization in storage and APIs.

/// String representation for Stream mode
pub const MODE_STREAM: &str = "stream";
/// String representation for Sample mode
pub const MODE_SAMPLE: &str = "sample";
/// String representation for SampleInterval mode
pub const MODE_SAMPLE_INTERVAL: &str = "sample_interval";
/// String representation for First mode
pub const MODE_FIRST: &str = "first";
/// String representation for Throttle mode
pub const MODE_THROTTLE: &str = "throttle";

// =============================================================================
// Safety Level Constants
// =============================================================================
// These constants are the canonical string representations for SafetyLevel variants.

/// String representation for Strict safety level
pub const SAFETY_STRICT: &str = "strict";
/// String representation for Trusted safety level
pub const SAFETY_TRUSTED: &str = "trusted";

// =============================================================================
// Metric Name Constants
// =============================================================================

/// Maximum length of a metric name (1-255 characters)
pub const MAX_METRIC_NAME_LEN: usize = 255;

/// Maximum length of a `user_id` or `agent_id` field (security-boundary fields).
///
/// Set to 256 because JWT `sub` claims can be up to 255 characters (per RFC 7519
/// there is no formal limit, but 255 is the practical maximum seen in OIDC providers).
/// 256 provides one byte of headroom and aligns with common OS/database username
/// length limits (e.g. Linux `LOGIN_NAME_MAX`).
pub const MAX_USER_ID_LEN: usize = 256;

/// Delimiter for multi-expression values in logpoint output and hashing.
/// ASCII Unit Separator (0x1F) - safe to use since it never appears in expression values.
pub const MULTI_EXPR_DELIMITER: char = '\x1F';

/// String form of `MULTI_EXPR_DELIMITER` for use in `join()` without heap allocation.
pub const MULTI_EXPR_DELIMITER_STR: &str = "\x1F";

/// Unique identifier for a metric (newtype for type safety)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MetricId(pub u64);

impl fmt::Display for MetricId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Metric mode determines how often the metric is captured
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum MetricMode {
    /// Capture every hit
    #[default]
    Stream,
    /// Capture every Nth hit (rate-based sampling)
    Sample { rate: u32 },
    /// Capture every N seconds (time-based sampling)
    #[serde(rename = "sample_interval")]
    SampleInterval { seconds: u32 },
    /// Capture only the first hit
    First,
    /// Rate-limited capture
    Throttle { max_per_second: u32 },
}

impl MetricMode {
    /// Get the mode type string (without config parameters)
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stream => MODE_STREAM,
            Self::Sample { .. } => MODE_SAMPLE,
            Self::SampleInterval { .. } => MODE_SAMPLE_INTERVAL,
            Self::First => MODE_FIRST,
            Self::Throttle { .. } => MODE_THROTTLE,
        }
    }
}

impl std::fmt::Display for MetricMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Safety level for expression evaluation
///
/// Two levels are supported:
/// - `Strict`: Only whitelisted functions allowed, unknown functions blocked
/// - `Trusted`: Whitelisted + unknown functions allowed, blacklisted still blocked
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SafetyLevel {
    /// Only whitelisted + @pure functions allowed.
    /// Unknown functions are blocked.
    #[default]
    Strict,
    /// User takes responsibility for unknown functions.
    /// Whitelisted + unknown functions allowed, blacklisted still blocked.
    Trusted,
}

impl SafetyLevel {
    /// Get the lowercase string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Strict => SAFETY_STRICT,
            Self::Trusted => SAFETY_TRUSTED,
        }
    }
}

impl std::fmt::Display for SafetyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for SafetyLevel {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            SAFETY_STRICT => Ok(Self::Strict),
            SAFETY_TRUSTED => Ok(Self::Trusted),
            other => Err(format!(
                "Unknown safety level: '{}'. Valid: strict, trusted",
                other
            )),
        }
    }
}

// validate_tenant_id is defined in entities/mod.rs (shared by Metric and Connection)

/// Core metric entity
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<MetricId>,
    pub name: String,
    pub connection_id: ConnectionId, // Which connection this metric belongs to
    pub group: Option<String>,
    pub location: Location,
    pub expressions: Vec<String>,
    pub language: SourceLanguage,
    pub enabled: bool,
    pub mode: MetricMode,
    pub condition: Option<String>,
    pub safety_level: SafetyLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>, // Microseconds since epoch

    /// Authenticated user identity (from token or JWT sub claim).
    /// Used for multi-tenant metric ownership and access control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,

    /// Agent/session identity (from MCP client_id or X-Detrix-Agent-Id header).
    /// Identifies which agent/session created this metric within a user's scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    // Stack trace capture options
    #[serde(default)]
    pub capture_stack_trace: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace_ttl: Option<u64>, // TTL in seconds, None = continuous
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace_slice: Option<StackTraceSlice>,

    // Memory snapshot capture options
    #[serde(default)]
    pub capture_memory_snapshot: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_scope: Option<SnapshotScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_ttl: Option<u64>, // TTL in seconds, None = continuous

    // Location tracking anchor (for following code changes)
    /// Anchor data for tracking location across code changes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<MetricAnchor>,
    /// Current status of the anchor
    #[serde(default)]
    pub anchor_status: AnchorStatus,
}

impl Metric {
    /// Validate metric name according to rules
    pub fn validate_name(name: &str) -> Result<()> {
        if name.is_empty() || name.len() > MAX_METRIC_NAME_LEN {
            return Err(Error::InvalidMetricName(format!(
                "Length must be 1-{} characters",
                MAX_METRIC_NAME_LEN
            )));
        }

        // Must start with letter or underscore
        if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            return Err(Error::InvalidMetricName(
                "Must start with letter or underscore".to_string(),
            ));
        }

        // Only alphanumeric, underscore, hyphen
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(Error::InvalidMetricName(
                "Can only contain letters, numbers, underscore, hyphen".to_string(),
            ));
        }

        // No consecutive special chars
        if name.contains("--") || name.contains("__") {
            return Err(Error::InvalidMetricName(
                "No consecutive special characters allowed".to_string(),
            ));
        }

        // Reserved names
        let reserved = ["all", "none", "system", "detrix", "internal"];
        if reserved.contains(&name.to_lowercase().as_str()) {
            return Err(Error::InvalidMetricName(format!(
                "'{}' is a reserved name",
                name
            )));
        }

        Ok(())
    }

    /// Compute SHA256 hash of all expressions.
    ///
    /// Uses length-prefixed encoding (`len:expr`) to prevent collisions when
    /// expressions contain the delimiter character.
    pub fn expression_hash(&self) -> String {
        let mut hasher = Sha256::new();
        for expr in &self.expressions {
            hasher.update(format!("{}:", expr.len()).as_bytes());
            hasher.update(expr.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    /// Convenience: get the first expression (safe even if deserialized with empty vec)
    pub fn expression(&self) -> &str {
        self.expressions.first().map(String::as_str).unwrap_or("")
    }

    /// Create a new metric with validation
    ///
    /// Note: Expression length validation should be done at the service layer
    /// where config is available. This only validates the name format and
    /// that expressions is non-empty.
    pub fn new(
        name: String,
        connection_id: ConnectionId,
        location: Location,
        expressions: Vec<String>,
        language: SourceLanguage,
    ) -> Result<Self> {
        Self::validate_name(&name)?;
        if expressions.is_empty() {
            return Err(Error::InvalidExpression(
                "At least one expression is required".to_string(),
            ));
        }

        Ok(Metric {
            id: None,
            name,
            connection_id,
            group: None,
            location,
            expressions,
            language,
            enabled: true,
            mode: MetricMode::default(),
            condition: None,
            safety_level: SafetyLevel::default(),
            created_at: None,
            user_id: None,
            agent_id: None,
            // Stack trace defaults
            capture_stack_trace: false,
            stack_trace_ttl: None,
            stack_trace_slice: None,
            // Memory snapshot defaults
            capture_memory_snapshot: false,
            snapshot_scope: None,
            snapshot_ttl: None,
            // Anchor tracking defaults (anchor captured later when metric is set)
            anchor: None,
            anchor_status: AnchorStatus::default(),
        })
    }

    /// Validate expression length against a maximum (checks each expression)
    pub fn validate_expression_length(expression: &str, max_length: usize) -> Result<()> {
        if expression.len() > max_length {
            return Err(Error::ExpressionTooLong {
                len: expression.len(),
                max: max_length,
            });
        }
        Ok(())
    }

    /// Validate all expressions' lengths against a maximum
    pub fn validate_expressions_length(expressions: &[String], max_length: usize) -> Result<()> {
        for expr in expressions {
            Self::validate_expression_length(expr, max_length)?;
        }
        Ok(())
    }

    /// Validate that `user_id` and `agent_id` do not exceed the maximum length.
    ///
    /// These are security-boundary fields used for multi-tenant access control.
    pub fn validate_tenant_ids(user_id: Option<&str>, agent_id: Option<&str>) -> Result<()> {
        super::validate_tenant_id("user_id", user_id)?;
        super::validate_tenant_id("agent_id", agent_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metric_validate_name_valid() {
        assert!(Metric::validate_name("user_auth").is_ok());
        assert!(Metric::validate_name("order-placed").is_ok());
        assert!(Metric::validate_name("_private").is_ok());
        assert!(Metric::validate_name("metric123").is_ok());
    }

    #[test]
    fn test_metric_validate_name_invalid_start() {
        let err = Metric::validate_name("123metric").unwrap_err();
        assert!(matches!(err, Error::InvalidMetricName(_)));
    }

    #[test]
    fn test_metric_validate_name_invalid_chars() {
        let err = Metric::validate_name("metric@name").unwrap_err();
        assert!(matches!(err, Error::InvalidMetricName(_)));
    }

    #[test]
    fn test_metric_validate_name_consecutive_special() {
        let err = Metric::validate_name("metric--name").unwrap_err();
        assert!(matches!(err, Error::InvalidMetricName(_)));

        let err = Metric::validate_name("metric__name").unwrap_err();
        assert!(matches!(err, Error::InvalidMetricName(_)));
    }

    #[test]
    fn test_metric_validate_name_reserved() {
        let err = Metric::validate_name("system").unwrap_err();
        assert!(matches!(err, Error::InvalidMetricName(_)));
        assert!(err.to_string().contains("reserved"));
    }

    #[test]
    fn test_metric_validate_name_too_long() {
        let long_name = "a".repeat(256);
        let err = Metric::validate_name(&long_name).unwrap_err();
        assert!(matches!(err, Error::InvalidMetricName(_)));
    }

    #[test]
    fn test_metric_new_valid() {
        let loc = Location {
            file: "test.py".to_string(),
            line: 10,
        };
        let metric = Metric::new(
            "test_metric".to_string(),
            ConnectionId::from("default"),
            loc,
            vec!["user.id".to_string()],
            SourceLanguage::Python,
        )
        .unwrap();

        assert_eq!(metric.name, "test_metric");
        assert_eq!(metric.location.line, 10);
        assert_eq!(metric.expression(), "user.id");
        assert_eq!(metric.expressions, vec!["user.id".to_string()]);
        assert_eq!(metric.language, SourceLanguage::Python);
        assert!(metric.enabled);
        assert_eq!(metric.mode, MetricMode::Stream);
        assert_eq!(metric.safety_level, SafetyLevel::Strict);
    }

    #[test]
    fn test_metric_single_expression() {
        let metric = Metric::new(
            "test_metric".to_string(),
            ConnectionId::from("default"),
            Location {
                file: "test.py".to_string(),
                line: 10,
            },
            vec!["x".to_string()],
            SourceLanguage::Python,
        )
        .unwrap();

        assert_eq!(metric.expression(), "x");
        assert_eq!(metric.expressions.len(), 1);
    }

    #[test]
    fn test_metric_multi_expressions() {
        let metric = Metric::new(
            "test_metric".to_string(),
            ConnectionId::from("default"),
            Location {
                file: "test.py".to_string(),
                line: 10,
            },
            vec!["x".to_string(), "y".to_string(), "z".to_string()],
            SourceLanguage::Python,
        )
        .unwrap();

        assert_eq!(metric.expressions.len(), 3);
        assert_eq!(metric.expression(), "x");
        assert_eq!(metric.expressions[1], "y");
        assert_eq!(metric.expressions[2], "z");
    }

    #[test]
    fn test_metric_new_empty_expressions_fails() {
        let result = Metric::new(
            "test_metric".to_string(),
            ConnectionId::from("default"),
            Location {
                file: "test.py".to_string(),
                line: 10,
            },
            vec![],
            SourceLanguage::Python,
        );
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidExpression(_)));
    }

    #[test]
    fn test_metric_new_invalid_name() {
        let loc = Location {
            file: "test.py".to_string(),
            line: 10,
        };
        let err = Metric::new(
            "123invalid".to_string(),
            ConnectionId::from("default"),
            loc,
            vec!["user.id".to_string()],
            SourceLanguage::Python,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidMetricName(_)));
    }

    #[test]
    fn test_metric_expression_hash_single() {
        let loc = Location {
            file: "test.py".to_string(),
            line: 10,
        };
        let metric = Metric::new(
            "test_metric".to_string(),
            ConnectionId::from("default"),
            loc,
            vec!["user.id".to_string()],
            SourceLanguage::Python,
        )
        .unwrap();

        let hash = metric.expression_hash();
        assert_eq!(hash.len(), 64); // SHA256 hex string

        // Same expression should have same hash
        let metric2 = Metric::new(
            "other_name".to_string(),
            ConnectionId::from("default"),
            Location {
                file: "other.py".to_string(),
                line: 20,
            },
            vec!["user.id".to_string()],
            SourceLanguage::Python,
        )
        .unwrap();
        assert_eq!(metric.expression_hash(), metric2.expression_hash());
    }

    #[test]
    fn test_metric_expression_hash_multi() {
        let single = Metric::new(
            "m1".to_string(),
            ConnectionId::from("default"),
            Location {
                file: "t.py".to_string(),
                line: 1,
            },
            vec!["x".to_string()],
            SourceLanguage::Python,
        )
        .unwrap();

        let multi = Metric::new(
            "m2".to_string(),
            ConnectionId::from("default"),
            Location {
                file: "t.py".to_string(),
                line: 1,
            },
            vec!["x".to_string(), "y".to_string()],
            SourceLanguage::Python,
        )
        .unwrap();

        // Single "x" vs multi "x|y" should have different hashes
        assert_ne!(single.expression_hash(), multi.expression_hash());
    }

    #[test]
    fn test_metric_mode_serialization() {
        let mode = MetricMode::Sample { rate: 100 };
        let json = serde_json::to_string(&mode).unwrap();
        assert!(json.contains("\"mode\":\"sample\""));
        assert!(json.contains("\"rate\":100"));

        let deserialized: MetricMode = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, mode);
    }

    #[test]
    fn test_metric_user_id_agent_id_default_to_none() {
        let metric = Metric::new(
            "test_metric".to_string(),
            ConnectionId::from("conn"),
            Location {
                file: "f.py".to_string(),
                line: 1,
            },
            vec!["x".to_string()],
            SourceLanguage::Python,
        )
        .unwrap();
        assert!(metric.user_id.is_none());
        assert!(metric.agent_id.is_none());
    }

    #[test]
    fn test_metric_user_id_agent_id_serde_round_trip() {
        let mut metric = Metric::new(
            "m".to_string(),
            ConnectionId::from("c"),
            Location {
                file: "f.py".to_string(),
                line: 1,
            },
            vec!["x".to_string()],
            SourceLanguage::Python,
        )
        .unwrap();
        metric.user_id = Some("alice".to_string());
        metric.agent_id = Some("agent-42".to_string());

        let serialized = serde_json::to_string(&metric).unwrap();
        let deserialized: Metric = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.user_id, Some("alice".to_string()));
        assert_eq!(deserialized.agent_id, Some("agent-42".to_string()));
        // Confirm the json contains the user_id and agent_id
        assert!(serialized.contains("alice"));
        assert!(serialized.contains("agent-42"));
    }

    #[test]
    fn test_validate_tenant_ids_both_none() {
        assert!(Metric::validate_tenant_ids(None, None).is_ok());
    }

    #[test]
    fn test_validate_tenant_ids_valid() {
        assert!(Metric::validate_tenant_ids(Some("alice"), Some("agent1")).is_ok());
    }

    #[test]
    fn test_validate_tenant_ids_at_max_length() {
        let max_str = "a".repeat(MAX_USER_ID_LEN);
        assert!(Metric::validate_tenant_ids(Some(&max_str), Some(&max_str)).is_ok());
    }

    #[test]
    fn test_validate_tenant_ids_user_id_too_long() {
        let long_str = "a".repeat(MAX_USER_ID_LEN + 1);
        let result = Metric::validate_tenant_ids(Some(&long_str), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("user_id exceeds maximum length"));
    }

    #[test]
    fn test_validate_tenant_ids_agent_id_too_long() {
        let long_str = "a".repeat(MAX_USER_ID_LEN + 1);
        let result = Metric::validate_tenant_ids(None, Some(&long_str));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("agent_id exceeds maximum length"));
    }

    #[test]
    fn test_validate_tenant_ids_empty_strings() {
        let result = Metric::validate_tenant_ids(Some(""), Some("agent1"));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must not be empty"));
    }

    #[test]
    fn test_validate_tenant_ids_reserved_system() {
        let result = Metric::validate_tenant_ids(Some("__system__"), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("reserved __*__ pattern"));
    }

    #[test]
    fn test_validate_tenant_ids_reserved_pattern() {
        // Any __*__ pattern is reserved
        let result = Metric::validate_tenant_ids(Some("__admin__"), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("reserved __*__ pattern"));

        // Too short (just "____") is not matched by len > 4 guard
        assert!(Metric::validate_tenant_ids(Some("____"), None).is_ok());
    }

    #[test]
    fn test_validate_tenant_ids_whitespace_only() {
        let result = Metric::validate_tenant_ids(Some("   "), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("whitespace-only"));

        let result = Metric::validate_tenant_ids(Some("\t"), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_tenant_ids_control_chars() {
        let result = Metric::validate_tenant_ids(Some("user\0id"), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("control characters"));
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// MetricMode serde roundtrip: deserialize(serialize(mode)) == mode
        #[test]
        fn proptest_metric_mode_serde_roundtrip(
            variant in 0..5u8,
            rate in 1..10000u32,
            seconds in 1..86400u32,
            max_per_second in 1..10000u32,
        ) {
            let mode = match variant {
                0 => MetricMode::Stream,
                1 => MetricMode::First,
                2 => MetricMode::Sample { rate },
                3 => MetricMode::SampleInterval { seconds },
                _ => MetricMode::Throttle { max_per_second },
            };

            let json = serde_json::to_string(&mode).unwrap();
            let deserialized: MetricMode = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(deserialized, mode);
        }

        /// Valid metric names: start with letter/underscore, contain only allowed chars
        #[test]
        fn proptest_valid_metric_names_accepted(
            first in "[a-zA-Z_]",
            rest in "[a-zA-Z0-9]{0,30}"  // Use only alphanumeric for rest to avoid rejection
        ) {
            let name = format!("{}{}", first, rest);
            let reserved = ["all", "none", "system", "detrix", "internal"];
            prop_assume!(!reserved.contains(&name.to_lowercase().as_str()));

            let result = Metric::validate_name(&name);
            prop_assert!(result.is_ok(), "Should accept valid name: {}", name);
        }

        /// Names starting with digit are rejected
        #[test]
        fn proptest_metric_names_starting_with_digit_rejected(
            digit in "[0-9]",
            rest in "[a-zA-Z0-9_-]{0,50}"
        ) {
            let name = format!("{}{}", digit, rest);
            let result = Metric::validate_name(&name);
            prop_assert!(result.is_err(), "Should reject name starting with digit: {}", name);
        }

        /// Names with invalid characters are rejected
        #[test]
        fn proptest_metric_names_with_special_chars_rejected(
            prefix in "[a-zA-Z_][a-zA-Z0-9_-]{0,10}",
            special in "[!@#$%^&*()+=\\[\\]{}|;':\"<>,?/~`]",
            suffix in "[a-zA-Z0-9_-]{0,10}"
        ) {
            let name = format!("{}{}{}", prefix, special, suffix);
            let result = Metric::validate_name(&name);
            prop_assert!(result.is_err(), "Should reject name with special char: {}", name);
        }

        /// Metric name validation never panics
        #[test]
        fn proptest_metric_name_validation_never_panics(s in "\\PC{0,300}") {
            // Should not panic
            let _ = Metric::validate_name(&s);
        }
    }
}
