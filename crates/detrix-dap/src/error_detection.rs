//! Data-driven error detection for DAP output parsing
//!
//! Provides a table-driven approach for detecting language-specific errors
//! in debugger output, replacing repetitive if/else chains.

/// Pattern matching mode for error detection
#[derive(Debug, Clone, Copy)]
pub enum MatchMode {
    /// Output starts with this pattern (case-sensitive)
    StartsWith,
    /// Output starts with this pattern (case-insensitive)
    StartsWithIgnoreCase,
    /// Output contains this pattern (case-insensitive)
    Contains,
}

/// A single error detection pattern
#[derive(Debug, Clone)]
pub struct ErrorPattern {
    /// Pattern to match against
    pub pattern: &'static str,
    /// How to match the pattern
    pub mode: MatchMode,
    /// Error type name to return on match
    pub error_type: &'static str,
}

impl ErrorPattern {
    /// Create a pattern that matches if output contains the given string (case-insensitive)
    pub const fn contains(pattern: &'static str, error_type: &'static str) -> Self {
        Self {
            pattern,
            mode: MatchMode::Contains,
            error_type,
        }
    }

    /// Create a pattern that matches if output starts with the given string (case-sensitive)
    pub const fn starts_with(pattern: &'static str, error_type: &'static str) -> Self {
        Self {
            pattern,
            mode: MatchMode::StartsWith,
            error_type,
        }
    }

    /// Create a pattern that matches if output starts with the given string (case-insensitive)
    pub const fn starts_with_ignore_case(pattern: &'static str, error_type: &'static str) -> Self {
        Self {
            pattern,
            mode: MatchMode::StartsWithIgnoreCase,
            error_type,
        }
    }

    /// Check if this pattern matches the given output
    fn matches(&self, output: &str, output_lower: &str) -> bool {
        match self.mode {
            MatchMode::StartsWith => output.starts_with(self.pattern),
            MatchMode::StartsWithIgnoreCase => {
                output_lower.starts_with(&self.pattern.to_lowercase())
            }
            MatchMode::Contains => output_lower.contains(&self.pattern.to_lowercase()),
        }
    }
}

/// Detect an error in output using the given pattern table
///
/// Returns `Some((error_type, message))` if a pattern matches, `None` otherwise.
pub fn detect_error(output: &str, patterns: &[ErrorPattern]) -> Option<(&'static str, String)> {
    let output_lower = output.to_lowercase();

    for pattern in patterns {
        if pattern.matches(output, &output_lower) {
            return Some((pattern.error_type, output.to_string()));
        }
    }

    None
}

use crate::base::find_metric_for_error;
use crate::OutputEventBody;
use detrix_core::entities::{Metric, MetricEvent};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::trace;

/// Shared error output parsing for all language adapters.
///
/// This function implements the common error parsing flow:
/// 1. Trim and detect error type using language-specific patterns
/// 2. Find the associated metric by location
/// 3. Create and return a MetricEvent with error information
///
/// # Arguments
/// * `output` - The DAP output event body
/// * `active_metrics` - Currently active metrics to match against
/// * `patterns` - Language-specific error patterns to use for detection
///
/// # Returns
/// `Some(MetricEvent)` if an error was detected and matched to a metric, `None` otherwise.
pub async fn parse_error_output_common(
    output: &OutputEventBody,
    active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    patterns: &[ErrorPattern],
) -> Option<MetricEvent> {
    let text = output.output.trim();

    // Use shared error detection with language-specific patterns
    let (error_type, error_message) = detect_error(text, patterns)?;

    let metrics = active_metrics.read().await;
    let (metric_id, metric_name, connection_id) = find_metric_for_error(output, &metrics)?;

    trace!(
        "Creating error event for metric '{}': {} - {}",
        metric_name,
        error_type,
        error_message
    );

    Some(MetricEvent::error(
        metric_id,
        metric_name,
        connection_id,
        error_type.to_string(),
        error_message,
    ))
}

// ============================================================================
// Python/debugpy error patterns
// ============================================================================

/// Error patterns for Python/debugpy output
///
/// These match standard Python exception messages from debugpy output.
pub static PYTHON_ERROR_PATTERNS: &[ErrorPattern] = &[
    // Name errors (undefined variable)
    ErrorPattern::contains("' is not defined", "NameError"),
    ErrorPattern::contains("name '", "NameError"),
    // Attribute errors
    ErrorPattern::contains("AttributeError:", "AttributeError"),
    ErrorPattern::contains("has no attribute", "AttributeError"),
    // Type errors
    ErrorPattern::contains("TypeError:", "TypeError"),
    ErrorPattern::contains("unsupported operand type", "TypeError"),
    // Key errors (dict missing key)
    ErrorPattern::contains("KeyError:", "KeyError"),
    // Index errors (list out of bounds)
    ErrorPattern::contains("IndexError:", "IndexError"),
    ErrorPattern::contains("list index out of range", "IndexError"),
    // Value errors
    ErrorPattern::contains("ValueError:", "ValueError"),
    ErrorPattern::contains("invalid literal", "ValueError"),
    // Zero division
    ErrorPattern::contains("ZeroDivisionError:", "ZeroDivisionError"),
    ErrorPattern::contains("division by zero", "ZeroDivisionError"),
    // Import errors
    ErrorPattern::contains("ImportError:", "ImportError"),
    ErrorPattern::contains("ModuleNotFoundError:", "ModuleNotFoundError"),
    ErrorPattern::contains("No module named", "ModuleNotFoundError"),
    // File/IO errors
    ErrorPattern::contains("FileNotFoundError:", "FileNotFoundError"),
    ErrorPattern::contains("PermissionError:", "PermissionError"),
    ErrorPattern::contains("IOError:", "IOError"),
    // Runtime errors
    ErrorPattern::contains("RuntimeError:", "RuntimeError"),
    ErrorPattern::contains("RecursionError:", "RecursionError"),
    ErrorPattern::contains("maximum recursion depth", "RecursionError"),
    // Memory errors
    ErrorPattern::contains("MemoryError:", "MemoryError"),
    // Assertion errors
    ErrorPattern::contains("AssertionError:", "AssertionError"),
    // Stop iteration
    ErrorPattern::contains("StopIteration:", "StopIteration"),
    // Syntax/parsing (less common in runtime but possible)
    ErrorPattern::contains("SyntaxError:", "SyntaxError"),
    ErrorPattern::contains("IndentationError:", "IndentationError"),
];

// ============================================================================
// Go/Delve error patterns
// ============================================================================

/// Error patterns for Go/Delve debugger output
pub static GO_ERROR_PATTERNS: &[ErrorPattern] = &[
    // Delve evaluation errors (usually start with "could not")
    ErrorPattern::starts_with_ignore_case("could not evaluate", "EvalError"),
    ErrorPattern::starts_with("Command failed:", "EvalError"),
    // Nil pointer dereference
    ErrorPattern::contains("nil pointer dereference", "NilPointerError"),
    ErrorPattern::contains("invalid memory address", "NilPointerError"),
    ErrorPattern::contains("runtime error: invalid memory", "NilPointerError"),
    // Index out of range
    ErrorPattern::contains("index out of range", "IndexError"),
    ErrorPattern::contains("slice bounds out of range", "IndexError"),
    ErrorPattern::contains("runtime error: index out of bounds", "IndexError"),
    // Type assertion errors
    ErrorPattern::contains("interface conversion", "TypeAssertionError"),
    ErrorPattern::contains("type assertion failed", "TypeAssertionError"),
    // Division by zero
    ErrorPattern::contains("integer divide by zero", "DivisionByZeroError"),
    ErrorPattern::contains("division by zero", "DivisionByZeroError"),
    ErrorPattern::contains(
        "runtime error: integer divide by zero",
        "DivisionByZeroError",
    ),
    // Stack overflow
    ErrorPattern::contains("stack overflow", "StackOverflowError"),
    ErrorPattern::contains("goroutine stack", "StackOverflowError"),
    // Panic
    ErrorPattern::contains("panic:", "PanicError"),
    ErrorPattern::starts_with_ignore_case("panic(", "PanicError"),
    // Undefined variable/symbol
    ErrorPattern::contains("undefined:", "UndefinedError"),
    ErrorPattern::contains("not found in scope", "UndefinedError"),
    // Channel errors
    ErrorPattern::contains("send on closed channel", "ChannelError"),
    ErrorPattern::contains("close of closed channel", "ChannelError"),
    ErrorPattern::contains("receive from closed channel", "ChannelError"),
    // Map errors
    ErrorPattern::contains("assignment to entry in nil map", "MapError"),
    // Deadlock
    ErrorPattern::contains(
        "fatal error: all goroutines are asleep - deadlock",
        "DeadlockError",
    ),
];

// ============================================================================
// Rust/LLDB error patterns
// ============================================================================

/// Error patterns for Rust/LLDB debugger output
pub static RUST_ERROR_PATTERNS: &[ErrorPattern] = &[
    // LLDB expression evaluation errors
    ErrorPattern::starts_with("error:", "LldbError"),
    ErrorPattern::starts_with("expression error:", "ExpressionError"),
    // Undefined variable/symbol errors
    ErrorPattern::contains("use of undeclared identifier", "UndefinedError"),
    ErrorPattern::contains("is not defined", "UndefinedError"),
    ErrorPattern::contains("cannot find", "UndefinedError"),
    ErrorPattern::contains("not found in this scope", "UndefinedError"),
    // Type mismatch errors
    ErrorPattern::contains("type mismatch", "TypeError"),
    ErrorPattern::contains("mismatched types", "TypeError"),
    ErrorPattern::contains("expected type", "TypeError"),
    // Null pointer / invalid reference errors
    ErrorPattern::contains("null pointer", "NullPointerError"),
    ErrorPattern::contains("invalid reference", "NullPointerError"),
    ErrorPattern::contains("dangling reference", "NullPointerError"),
    ErrorPattern::contains("use after free", "NullPointerError"),
    // Index out of bounds
    ErrorPattern::contains("index out of bounds", "IndexError"),
    ErrorPattern::contains("out of range", "IndexError"),
    ErrorPattern::contains("slice index", "IndexError"),
    // Division by zero
    ErrorPattern::contains("division by zero", "DivisionByZeroError"),
    ErrorPattern::contains("divide by zero", "DivisionByZeroError"),
    ErrorPattern::contains("attempt to divide by zero", "DivisionByZeroError"),
    // Overflow errors
    ErrorPattern::contains("overflow", "OverflowError"),
    ErrorPattern::contains("arithmetic operation", "OverflowError"),
    ErrorPattern::contains("integer overflow", "OverflowError"),
    // Unwrap/Option errors (panic)
    ErrorPattern::contains("called `option::unwrap()` on a `none` value", "PanicError"),
    ErrorPattern::contains("called `result::unwrap()` on an `err` value", "PanicError"),
    ErrorPattern::contains("panicked at", "PanicError"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_nil_pointer() {
        let output = "runtime error: invalid memory address or nil pointer dereference";
        let result = detect_error(output, GO_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "NilPointerError");
    }

    #[test]
    fn test_go_eval_error() {
        let output = "could not evaluate expression: variable not found";
        let result = detect_error(output, GO_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "EvalError");
    }

    #[test]
    fn test_go_panic() {
        let output = "panic: runtime error: slice bounds out of range";
        let result = detect_error(output, GO_ERROR_PATTERNS);
        assert!(result.is_some());
        // panic: matches first, then slice bounds would match IndexError
        // But since panic: comes first in the output and patterns, PanicError wins
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "IndexError"); // slice bounds matches first in pattern order
    }

    #[test]
    fn test_rust_lldb_error() {
        let output = "error: use of undeclared identifier 'foo'";
        let result = detect_error(output, RUST_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "LldbError"); // error: matches first
    }

    #[test]
    fn test_rust_panic() {
        let output = "thread 'main' panicked at 'called `Option::unwrap()` on a `None` value'";
        let result = detect_error(output, RUST_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "PanicError");
    }

    #[test]
    fn test_no_error() {
        let output = "42";
        assert!(detect_error(output, GO_ERROR_PATTERNS).is_none());
        assert!(detect_error(output, RUST_ERROR_PATTERNS).is_none());
        assert!(detect_error(output, PYTHON_ERROR_PATTERNS).is_none());
    }

    #[test]
    fn test_case_insensitive_contains() {
        let output = "PANIC: something went wrong";
        let result = detect_error(output, GO_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "PanicError");
    }

    // ========================================================================
    // Python error pattern tests
    // ========================================================================

    #[test]
    fn test_python_name_error() {
        let output = "name 'undefined_var' is not defined";
        let result = detect_error(output, PYTHON_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "NameError");
    }

    #[test]
    fn test_python_attribute_error() {
        let output = "AttributeError: 'NoneType' object has no attribute 'foo'";
        let result = detect_error(output, PYTHON_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "AttributeError");
    }

    #[test]
    fn test_python_type_error() {
        let output = "TypeError: unsupported operand type(s) for +: 'int' and 'str'";
        let result = detect_error(output, PYTHON_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "TypeError");
    }

    #[test]
    fn test_python_key_error() {
        let output = "KeyError: 'missing_key'";
        let result = detect_error(output, PYTHON_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "KeyError");
    }

    #[test]
    fn test_python_index_error() {
        let output = "IndexError: list index out of range";
        let result = detect_error(output, PYTHON_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "IndexError");
    }

    #[test]
    fn test_python_zero_division() {
        let output = "ZeroDivisionError: division by zero";
        let result = detect_error(output, PYTHON_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "ZeroDivisionError");
    }

    #[test]
    fn test_python_import_error() {
        let output = "ModuleNotFoundError: No module named 'nonexistent'";
        let result = detect_error(output, PYTHON_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "ModuleNotFoundError");
    }

    #[test]
    fn test_python_recursion_error() {
        let output = "RecursionError: maximum recursion depth exceeded";
        let result = detect_error(output, PYTHON_ERROR_PATTERNS);
        assert!(result.is_some());
        let (error_type, _) = result.unwrap();
        assert_eq!(error_type, "RecursionError");
    }
}
