//! Response formatting for MCP tool outputs
//!
//! This module provides TOON (Token-Oriented Object Notation) and JSON formatting
//! for MCP responses. TOON is optimized for LLM consumption, reducing token usage
//! by ~40% for tabular data while maintaining full data fidelity.
//!
//! # Clean Architecture
//!
//! This is a PRESENTATION concern - it belongs in the Interface/API layer because:
//! - Formatting is about how data is presented to consumers (LLMs)
//! - Domain entities are format-agnostic
//! - Only the interface layer knows about client preferences
//!
//! # Usage
//!
//! ```ignore
//! use detrix_api::mcp::format::{ResponseFormat, format_response};
//!
//! let metrics = vec![metric1, metric2, metric3];
//! let output = format_response(&metrics, ResponseFormat::Toon)?;
//! ```

use serde::Serialize;

/// Response format options for MCP tool outputs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResponseFormat {
    /// TOON format - optimized for LLM token efficiency (~40% fewer tokens)
    /// Best for: arrays of uniform objects (metrics, events, connections)
    #[default]
    Toon,
    /// JSON format - human-readable, useful for debugging
    /// Best for: inspection, debugging, non-LLM consumers
    Json,
}

impl ResponseFormat {
    /// Parse format from string (case-insensitive)
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.map(|s| s.to_lowercase()).as_deref() {
            Some("json") => Self::Json,
            Some("toon") => Self::Toon,
            _ => Self::default(), // TOON is default for LLM efficiency
        }
    }
}

/// Format a serializable value for MCP response
///
/// For TOON format, falls back to JSON if TOON encoding fails.
/// This ensures responses are always returned even if TOON has issues
/// with complex nested structures.
pub fn format_response<T: Serialize>(value: &T, format: ResponseFormat) -> String {
    match format {
        ResponseFormat::Toon => format_as_toon(value),
        ResponseFormat::Json => format_as_json(value),
    }
}

/// Format value as TOON, with JSON fallback
///
/// Uses toon-format's encode_default which accepts any Serialize type directly.
fn format_as_toon<T: Serialize>(value: &T) -> String {
    toon_format::encode_default(value).unwrap_or_else(|e| {
        tracing::debug!("TOON encoding failed, falling back to JSON: {}", e);
        format_as_json(value)
    })
}

/// Format value as pretty-printed JSON
fn format_as_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| {
        tracing::error!("JSON serialization failed: {}", e);
        "Error: serialization failed".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    // Test structs that mirror our domain types
    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestMetric {
        id: i64,
        name: String,
        location: String,
        enabled: bool,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestEvent {
        metric_id: i64,
        timestamp: i64,
        value: String,
    }

    // =========================================================================
    // ResponseFormat parsing tests
    // =========================================================================

    #[test]
    fn test_format_from_str_default_is_toon() {
        assert_eq!(ResponseFormat::from_str_opt(None), ResponseFormat::Toon);
    }

    #[test]
    fn test_format_from_str_json() {
        assert_eq!(
            ResponseFormat::from_str_opt(Some("json")),
            ResponseFormat::Json
        );
        assert_eq!(
            ResponseFormat::from_str_opt(Some("JSON")),
            ResponseFormat::Json
        );
        assert_eq!(
            ResponseFormat::from_str_opt(Some("Json")),
            ResponseFormat::Json
        );
    }

    #[test]
    fn test_format_from_str_toon() {
        assert_eq!(
            ResponseFormat::from_str_opt(Some("toon")),
            ResponseFormat::Toon
        );
        assert_eq!(
            ResponseFormat::from_str_opt(Some("TOON")),
            ResponseFormat::Toon
        );
    }

    #[test]
    fn test_format_from_str_unknown_defaults_to_toon() {
        assert_eq!(
            ResponseFormat::from_str_opt(Some("xml")),
            ResponseFormat::Toon
        );
        assert_eq!(
            ResponseFormat::from_str_opt(Some("yaml")),
            ResponseFormat::Toon
        );
    }

    // =========================================================================
    // TOON format tests - verify token efficiency for tabular data
    // =========================================================================

    #[test]
    fn test_toon_format_metric_array() {
        let metrics = vec![
            TestMetric {
                id: 1,
                name: "request_count".to_string(),
                location: "@app.py#42".to_string(),
                enabled: true,
            },
            TestMetric {
                id: 2,
                name: "error_rate".to_string(),
                location: "@handler.py#100".to_string(),
                enabled: false,
            },
            TestMetric {
                id: 3,
                name: "latency_ms".to_string(),
                location: "@service.py#25".to_string(),
                enabled: true,
            },
        ];

        let toon_output = format_response(&metrics, ResponseFormat::Toon);
        let json_output = format_response(&metrics, ResponseFormat::Json);

        // TOON should be significantly shorter
        assert!(
            toon_output.len() < json_output.len(),
            "TOON ({} bytes) should be shorter than JSON ({} bytes)",
            toon_output.len(),
            json_output.len()
        );

        // Verify TOON output contains tabular format markers
        // TOON arrays use format like: [3]{field1,field2,...}:
        assert!(
            toon_output.contains("[3]"),
            "TOON should contain array length marker [3]: {}",
            toon_output
        );
    }

    #[test]
    fn test_toon_format_event_array() {
        let events = vec![
            TestEvent {
                metric_id: 1,
                timestamp: 1733900000000,
                value: "user_123".to_string(),
            },
            TestEvent {
                metric_id: 1,
                timestamp: 1733900001000,
                value: "user_456".to_string(),
            },
        ];

        let toon_output = format_response(&events, ResponseFormat::Toon);
        let json_output = format_response(&events, ResponseFormat::Json);

        // TOON should be shorter
        assert!(
            toon_output.len() < json_output.len(),
            "TOON should be shorter than JSON for event arrays"
        );

        // Should contain array marker
        assert!(toon_output.contains("[2]"));
    }

    #[test]
    fn test_toon_format_single_object() {
        let metric = TestMetric {
            id: 1,
            name: "test".to_string(),
            location: "@test.py#1".to_string(),
            enabled: true,
        };

        let toon_output = format_response(&metric, ResponseFormat::Toon);

        // Single object should still serialize correctly
        assert!(!toon_output.is_empty());
        // Should contain the field values
        assert!(toon_output.contains("test"));
    }

    #[test]
    fn test_toon_format_empty_array() {
        let metrics: Vec<TestMetric> = vec![];

        let toon_output = format_response(&metrics, ResponseFormat::Toon);

        // Empty array should have minimal output
        println!("TOON empty array output: '{}'", toon_output);
        assert!(!toon_output.is_empty());
        // TOON empty array should be "[0]:" (not JSON "[]")
        assert!(
            toon_output.contains("[0]"),
            "Empty array should be '[0]:' in TOON format, got: {}",
            toon_output
        );
    }

    // =========================================================================
    // JSON format tests
    // =========================================================================

    #[test]
    fn test_json_format_metric_array() {
        let metrics = vec![TestMetric {
            id: 1,
            name: "test".to_string(),
            location: "@test.py#1".to_string(),
            enabled: true,
        }];

        let json_output = format_response(&metrics, ResponseFormat::Json);

        // Should be valid JSON
        let parsed: Vec<TestMetric> = serde_json::from_str(&json_output).unwrap();
        assert_eq!(parsed, metrics);
    }

    #[test]
    fn test_json_format_is_pretty_printed() {
        let metric = TestMetric {
            id: 1,
            name: "test".to_string(),
            location: "@test.py#1".to_string(),
            enabled: true,
        };

        let json_output = format_response(&metric, ResponseFormat::Json);

        // Pretty printed JSON has newlines
        assert!(json_output.contains('\n'), "JSON should be pretty-printed");
    }

    // =========================================================================
    // Round-trip tests - verify data integrity
    // =========================================================================

    #[test]
    fn test_toon_roundtrip_preserves_data() {
        let metrics = vec![
            TestMetric {
                id: 1,
                name: "metric_one".to_string(),
                location: "@file.py#10".to_string(),
                enabled: true,
            },
            TestMetric {
                id: 2,
                name: "metric_two".to_string(),
                location: "@other.py#20".to_string(),
                enabled: false,
            },
        ];

        let toon_output = format_response(&metrics, ResponseFormat::Toon);

        // Decode back from TOON directly to our type (v0.4 supports Deserialize)
        let decoded: Vec<TestMetric> = toon_format::decode_default(&toon_output)
            .expect("TOON should decode back to original data");

        assert_eq!(decoded, metrics, "Round-trip should preserve data");
    }

    #[test]
    fn test_json_roundtrip_preserves_data() {
        let metrics = vec![TestMetric {
            id: 1,
            name: "test".to_string(),
            location: "@test.py#1".to_string(),
            enabled: true,
        }];

        let json_output = format_response(&metrics, ResponseFormat::Json);
        let decoded: Vec<TestMetric> = serde_json::from_str(&json_output).unwrap();

        assert_eq!(decoded, metrics);
    }

    // =========================================================================
    // Token efficiency comparison test
    // =========================================================================

    #[test]
    fn test_toon_token_efficiency_for_large_arrays() {
        // Create a larger array to demonstrate token savings
        let metrics: Vec<TestMetric> = (0..10)
            .map(|i| TestMetric {
                id: i,
                name: format!("metric_{}", i),
                location: format!("@file_{}.py#{}", i, i * 10),
                enabled: i % 2 == 0,
            })
            .collect();

        let toon_output = format_response(&metrics, ResponseFormat::Toon);
        let json_output = format_response(&metrics, ResponseFormat::Json);

        let toon_len = toon_output.len();
        let json_len = json_output.len();
        let savings_pct = (1.0 - (toon_len as f64 / json_len as f64)) * 100.0;

        println!("TOON: {} bytes", toon_len);
        println!("JSON: {} bytes", json_len);
        println!("Savings: {:.1}%", savings_pct);

        // TOON should save at least 20% for tabular data
        assert!(
            savings_pct > 20.0,
            "TOON should save at least 20% for tabular data, got {:.1}%",
            savings_pct
        );
    }
}
