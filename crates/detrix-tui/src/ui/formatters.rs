//! Common formatting utilities for TUI rendering
//!
//! This module contains shared formatting functions used across multiple UI tabs.

// Re-export timestamp functions from detrix_core for backward compatibility
pub use detrix_core::{
    format_timestamp_short, format_timestamp_time as format_timestamp, set_use_utc,
};

/// Truncate string with ellipsis (UTF-8 safe)
///
/// Returns the original string if it fits within max_len characters,
/// otherwise truncates and adds an ellipsis.
pub fn truncate(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{}…", truncated)
    }
}

/// Pretty print JSON with indentation
///
/// Attempts to parse and re-format JSON with indentation.
/// Returns original string if parsing fails.
pub fn format_json_pretty(s: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| s.to_string())
    } else {
        s.to_string()
    }
}

/// Compact JSON for display
///
/// Formats JSON values in a compact, human-readable form for display.
pub fn compact_json(s: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        match v {
            serde_json::Value::String(s) => format!("\"{}\"", truncate(&s, 25)),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Array(arr) => format!("[{} items]", arr.len()),
            serde_json::Value::Object(obj) => {
                if obj.len() == 1 {
                    if let Some((k, v)) = obj.iter().next() {
                        return format!("{}: {}", k, compact_json(&v.to_string()));
                    }
                }
                format!("{{{} fields}}", obj.len())
            }
        }
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn test_truncate_utf8() {
        // Test with multi-byte UTF-8 characters
        assert_eq!(truncate("こんにちは", 4), "こんに…");
    }

    #[test]
    fn test_format_json_pretty() {
        let json = r#"{"key":"value"}"#;
        let pretty = format_json_pretty(json);
        assert!(pretty.contains('\n'));
        assert!(pretty.contains("key"));
    }

    #[test]
    fn test_compact_json_string() {
        let json = r#""hello""#;
        assert_eq!(compact_json(json), "\"hello\"");
    }

    #[test]
    fn test_compact_json_array() {
        let json = r#"[1, 2, 3, 4, 5]"#;
        assert_eq!(compact_json(json), "[5 items]");
    }

    #[test]
    fn test_compact_json_object() {
        let json = r#"{"a": 1, "b": 2}"#;
        assert_eq!(compact_json(json), "{2 fields}");
    }
}
