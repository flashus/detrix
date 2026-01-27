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

/// Core metric entity
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<MetricId>,
    pub name: String,
    pub connection_id: ConnectionId, // Which connection this metric belongs to
    pub group: Option<String>,
    pub location: Location,
    pub expression: String,
    pub language: SourceLanguage,
    pub enabled: bool,
    pub mode: MetricMode,
    pub condition: Option<String>,
    pub safety_level: SafetyLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>, // Microseconds since epoch

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
        if !name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        {
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

    /// Compute SHA256 hash of expression
    pub fn expression_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.expression.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Create a new metric with validation
    ///
    /// Note: Expression length validation should be done at the service layer
    /// where config is available. This only validates the name format.
    pub fn new(
        name: String,
        connection_id: ConnectionId,
        location: Location,
        expression: String,
        language: SourceLanguage,
    ) -> Result<Self> {
        Self::validate_name(&name)?;

        Ok(Metric {
            id: None,
            name,
            connection_id,
            group: None,
            location,
            expression,
            language,
            enabled: true,
            mode: MetricMode::default(),
            condition: None,
            safety_level: SafetyLevel::default(),
            created_at: None,
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

    /// Validate expression length against a maximum
    pub fn validate_expression_length(expression: &str, max_length: usize) -> Result<()> {
        if expression.len() > max_length {
            return Err(Error::ExpressionTooLong {
                len: expression.len(),
                max: max_length,
            });
        }
        Ok(())
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
            "user.id".to_string(),
            SourceLanguage::Python,
        )
        .unwrap();

        assert_eq!(metric.name, "test_metric");
        assert_eq!(metric.location.line, 10);
        assert_eq!(metric.expression, "user.id");
        assert_eq!(metric.language, SourceLanguage::Python);
        assert!(metric.enabled);
        assert_eq!(metric.mode, MetricMode::Stream);
        assert_eq!(metric.safety_level, SafetyLevel::Strict);
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
            "user.id".to_string(),
            SourceLanguage::Python,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidMetricName(_)));
    }

    #[test]
    fn test_metric_expression_hash() {
        let loc = Location {
            file: "test.py".to_string(),
            line: 10,
        };
        let metric = Metric::new(
            "test_metric".to_string(),
            ConnectionId::from("default"),
            loc,
            "user.id".to_string(),
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
            "user.id".to_string(),
            SourceLanguage::Python,
        )
        .unwrap();
        assert_eq!(metric.expression_hash(), metric2.expression_hash());
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
