//! Language-specific expression extractors for print/log statements.
//!
//! This module provides extractors that parse debug print statements and extract
//! the expressions being logged. Used by the `enable_from_diff` tool to automatically
//! create metrics from print statements in git diffs.

use regex::Regex;
use std::sync::LazyLock;

/// Confidence level for extracted expressions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Simple identifier or attribute access (high confidence)
    High,
    /// Method call without arguments (medium confidence)
    Medium,
    /// Complex expression - suggest manual review (low confidence)
    Low,
}

/// An extracted expression with metadata
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedExpression {
    /// The expression string
    pub expression: String,
    /// Confidence level
    pub confidence: Confidence,
}

impl ExtractedExpression {
    pub fn high(expr: impl Into<String>) -> Self {
        Self {
            expression: expr.into(),
            confidence: Confidence::High,
        }
    }

    pub fn medium(expr: impl Into<String>) -> Self {
        Self {
            expression: expr.into(),
            confidence: Confidence::Medium,
        }
    }

    pub fn low(expr: impl Into<String>) -> Self {
        Self {
            expression: expr.into(),
            confidence: Confidence::Low,
        }
    }
}

/// Trait for language-specific expression extraction
pub trait ExpressionExtractor: Send + Sync {
    /// Extract expressions from a print/log statement
    fn extract(&self, statement: &str) -> Vec<ExtractedExpression>;

    /// Check if this statement is a debug print/log
    fn is_debug_statement(&self, statement: &str) -> bool;
}

/// Boxed expression extractor for ownership transfer
pub type ExpressionExtractorBox = Box<dyn ExpressionExtractor>;

// =============================================================================
// Python Extractor
// =============================================================================

/// Extracts expressions from Python print/log statements
pub struct PythonExtractor;

// Regex patterns for Python
// NOTE: These are static patterns validated at development time.
// Using .expect() is acceptable per CLAUDE.md guidelines for static regex compilation.
static PYTHON_FSTRING_EXPR: LazyLock<Regex> = LazyLock::new(|| {
    // Match {expression} in f-strings, but not format specs like {x:.2f}
    Regex::new(r"\{([a-zA-Z_][a-zA-Z0-9_]*(?:\.[a-zA-Z_][a-zA-Z0-9_]*)*(?:\(\))?)\}")
        .expect("Static regex pattern is valid")
});

static PYTHON_PRINT_DIRECT: LazyLock<Regex> = LazyLock::new(|| {
    // Match print(variable) where variable is a simple expression
    Regex::new(r"print\(\s*([a-zA-Z_][a-zA-Z0-9_]*(?:\.[a-zA-Z_][a-zA-Z0-9_]*)*)\s*\)")
        .expect("Static regex pattern is valid")
});

static PYTHON_DEBUG_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:print|logger\.(?:debug|info|warning|error)|logging\.(?:debug|info|warning|error))\s*\(")
        .expect("Static regex pattern is valid")
});

impl ExpressionExtractor for PythonExtractor {
    fn extract(&self, statement: &str) -> Vec<ExtractedExpression> {
        let mut results = Vec::new();

        // Try f-string extraction first
        for cap in PYTHON_FSTRING_EXPR.captures_iter(statement) {
            if let Some(expr) = cap.get(1) {
                let expr_str = expr.as_str();
                let confidence = classify_expression(expr_str);
                results.push(ExtractedExpression {
                    expression: expr_str.to_string(),
                    confidence,
                });
            }
        }

        // If no f-string expressions found, try direct print(var)
        if results.is_empty() {
            if let Some(cap) = PYTHON_PRINT_DIRECT.captures(statement) {
                if let Some(expr) = cap.get(1) {
                    let expr_str = expr.as_str();
                    let confidence = classify_expression(expr_str);
                    results.push(ExtractedExpression {
                        expression: expr_str.to_string(),
                        confidence,
                    });
                }
            }
        }

        results
    }

    fn is_debug_statement(&self, statement: &str) -> bool {
        PYTHON_DEBUG_PATTERN.is_match(statement)
    }
}

// =============================================================================
// Go Extractor
// =============================================================================

/// Extracts expressions from Go fmt/log statements
pub struct GoExtractor;

static GO_PRINTF_ARGS: LazyLock<Regex> = LazyLock::new(|| {
    // Match fmt.Printf("...", expr) or log.Printf("...", expr)
    // Capture the argument after the format string
    Regex::new(r"(?:fmt|log)\.(?:Printf?|Println)\s*\([^,]+,\s*([a-zA-Z_][a-zA-Z0-9_]*(?:\.[a-zA-Z_][a-zA-Z0-9_]*)*)\s*\)")
        .expect("Static regex pattern is valid")
});

static GO_DEBUG_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:fmt|log)\.(?:Print|Printf|Println)\s*\(")
        .expect("Static regex pattern is valid")
});

impl ExpressionExtractor for GoExtractor {
    fn extract(&self, statement: &str) -> Vec<ExtractedExpression> {
        let mut results = Vec::new();

        for cap in GO_PRINTF_ARGS.captures_iter(statement) {
            if let Some(expr) = cap.get(1) {
                let expr_str = expr.as_str();
                let confidence = classify_expression(expr_str);
                results.push(ExtractedExpression {
                    expression: expr_str.to_string(),
                    confidence,
                });
            }
        }

        results
    }

    fn is_debug_statement(&self, statement: &str) -> bool {
        GO_DEBUG_PATTERN.is_match(statement)
    }
}

// =============================================================================
// Rust Extractor
// =============================================================================

/// Extracts expressions from Rust println!/dbg! statements
pub struct RustExtractor;

static RUST_DBG_MACRO: LazyLock<Regex> = LazyLock::new(|| {
    // Match dbg!(expr)
    Regex::new(r"dbg!\s*\(\s*([a-zA-Z_][a-zA-Z0-9_]*(?:\.[a-zA-Z_][a-zA-Z0-9_]*)*)\s*\)")
        .expect("Static regex pattern is valid")
});

static RUST_PRINTLN_ARG: LazyLock<Regex> = LazyLock::new(|| {
    // Match println!("{:?}", expr) or println!("{}", expr)
    Regex::new(r"println!\s*\([^,]+,\s*([a-zA-Z_][a-zA-Z0-9_]*(?:\.[a-zA-Z_][a-zA-Z0-9_]*)*)\s*\)")
        .expect("Static regex pattern is valid")
});

static RUST_DEBUG_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:println!|eprintln!|dbg!|print!|eprint!)\s*\(")
        .expect("Static regex pattern is valid")
});

impl ExpressionExtractor for RustExtractor {
    fn extract(&self, statement: &str) -> Vec<ExtractedExpression> {
        let mut results = Vec::new();

        // Try dbg! first
        for cap in RUST_DBG_MACRO.captures_iter(statement) {
            if let Some(expr) = cap.get(1) {
                let expr_str = expr.as_str();
                let confidence = classify_expression(expr_str);
                results.push(ExtractedExpression {
                    expression: expr_str.to_string(),
                    confidence,
                });
            }
        }

        // Try println! with format args
        for cap in RUST_PRINTLN_ARG.captures_iter(statement) {
            if let Some(expr) = cap.get(1) {
                let expr_str = expr.as_str();
                // Avoid duplicates from dbg!
                if !results.iter().any(|e| e.expression == expr_str) {
                    let confidence = classify_expression(expr_str);
                    results.push(ExtractedExpression {
                        expression: expr_str.to_string(),
                        confidence,
                    });
                }
            }
        }

        results
    }

    fn is_debug_statement(&self, statement: &str) -> bool {
        RUST_DEBUG_PATTERN.is_match(statement)
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Classify an expression by confidence level
fn classify_expression(expr: &str) -> Confidence {
    // Simple identifier: x, balance, user_id
    if expr.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Confidence::High;
    }

    // Attribute access: user.name, order.total.amount
    let parts: Vec<&str> = expr.split('.').collect();
    if parts
        .iter()
        .all(|part| part.chars().all(|c| c.is_alphanumeric() || c == '_'))
    {
        return Confidence::High;
    }

    // Method call without args: user.get_id()
    if let Some(base) = expr.strip_suffix("()") {
        if base
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            return Confidence::Medium;
        }
    }

    // Everything else is low confidence
    Confidence::Low
}

/// Get the appropriate extractor for a language
pub fn get_extractor(language: &str) -> Option<ExpressionExtractorBox> {
    match language.to_lowercase().as_str() {
        "python" | "py" => Some(Box::new(PythonExtractor)),
        "go" | "golang" => Some(Box::new(GoExtractor)),
        "rust" | "rs" => Some(Box::new(RustExtractor)),
        _ => None,
    }
}

/// Detect language from file extension
pub fn detect_language(file_path: &str) -> Option<&'static str> {
    let ext = file_path.rsplit('.').next()?;
    match ext.to_lowercase().as_str() {
        "py" => Some("python"),
        "go" => Some("go"),
        "rs" => Some("rust"),
        _ => None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Static Regex Validation
    // =========================================================================

    /// Validates that all static regex patterns compile correctly.
    /// This test forces lazy initialization of all patterns, ensuring they are valid.
    /// If any pattern is invalid, this test will panic during initialization.
    #[test]
    fn all_static_regex_patterns_compile() {
        // Force lazy initialization - will panic if any pattern is invalid
        let _ = &*PYTHON_FSTRING_EXPR;
        let _ = &*PYTHON_PRINT_DIRECT;
        let _ = &*PYTHON_DEBUG_PATTERN;
        let _ = &*GO_PRINTF_ARGS;
        let _ = &*GO_DEBUG_PATTERN;
        let _ = &*RUST_DBG_MACRO;
        let _ = &*RUST_PRINTLN_ARG;
        let _ = &*RUST_DEBUG_PATTERN;
    }

    // =========================================================================
    // Python Extractor Tests
    // =========================================================================

    #[test]
    fn test_python_fstring_simple() {
        let stmt = r#"print(f"balance={balance}")"#;
        let exprs = PythonExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "balance");
        assert_eq!(exprs[0].confidence, Confidence::High);
    }

    #[test]
    fn test_python_fstring_attribute() {
        let stmt = r#"print(f"user.name={user.name}")"#;
        let exprs = PythonExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "user.name");
        assert_eq!(exprs[0].confidence, Confidence::High);
    }

    #[test]
    fn test_python_fstring_multiple() {
        let stmt = r#"print(f"x={x}, y={y}")"#;
        let exprs = PythonExtractor.extract(stmt);
        assert_eq!(exprs.len(), 2);
        assert_eq!(exprs[0].expression, "x");
        assert_eq!(exprs[1].expression, "y");
    }

    #[test]
    fn test_python_logger_debug() {
        let stmt = r#"logger.debug(f"Processing {order_id}")"#;
        let exprs = PythonExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "order_id");
    }

    #[test]
    fn test_python_print_direct_variable() {
        let stmt = "print(balance)";
        let exprs = PythonExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "balance");
    }

    #[test]
    fn test_python_print_direct_attribute() {
        let stmt = "print(user.name)";
        let exprs = PythonExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "user.name");
    }

    #[test]
    fn test_python_method_call() {
        let stmt = r#"print(f"id={user.get_id()}")"#;
        let exprs = PythonExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "user.get_id()");
        assert_eq!(exprs[0].confidence, Confidence::Medium);
    }

    #[test]
    fn test_python_is_debug_statement() {
        assert!(PythonExtractor.is_debug_statement("print(x)"));
        assert!(PythonExtractor.is_debug_statement("logger.debug(x)"));
        assert!(PythonExtractor.is_debug_statement("logging.info(x)"));
        assert!(!PythonExtractor.is_debug_statement("return x"));
    }

    // =========================================================================
    // Go Extractor Tests
    // =========================================================================

    #[test]
    fn test_go_printf_simple() {
        let stmt = r#"fmt.Printf("balance: %v\n", balance)"#;
        let exprs = GoExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "balance");
    }

    #[test]
    fn test_go_printf_attribute() {
        let stmt = r#"fmt.Printf("user: %v\n", user.Name)"#;
        let exprs = GoExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "user.Name");
    }

    #[test]
    fn test_go_log_printf() {
        let stmt = r#"log.Printf("order: %v", order)"#;
        let exprs = GoExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "order");
    }

    #[test]
    fn test_go_is_debug_statement() {
        assert!(GoExtractor.is_debug_statement("fmt.Printf(x)"));
        assert!(GoExtractor.is_debug_statement("fmt.Println(x)"));
        assert!(GoExtractor.is_debug_statement("log.Print(x)"));
        assert!(!GoExtractor.is_debug_statement("return x"));
    }

    // =========================================================================
    // Rust Extractor Tests
    // =========================================================================

    #[test]
    fn test_rust_dbg_macro() {
        let stmt = "dbg!(user);";
        let exprs = RustExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "user");
    }

    #[test]
    fn test_rust_dbg_attribute() {
        let stmt = "dbg!(user.name);";
        let exprs = RustExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "user.name");
    }

    #[test]
    fn test_rust_println_debug() {
        let stmt = r#"println!("{:?}", balance);"#;
        let exprs = RustExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "balance");
    }

    #[test]
    fn test_rust_println_display() {
        let stmt = r#"println!("{}", user.name);"#;
        let exprs = RustExtractor.extract(stmt);
        assert_eq!(exprs.len(), 1);
        assert_eq!(exprs[0].expression, "user.name");
    }

    #[test]
    fn test_rust_is_debug_statement() {
        assert!(RustExtractor.is_debug_statement("println!(x)"));
        assert!(RustExtractor.is_debug_statement("dbg!(x)"));
        assert!(RustExtractor.is_debug_statement("eprintln!(x)"));
        assert!(!RustExtractor.is_debug_statement("return x;"));
    }

    // =========================================================================
    // Confidence Classification Tests
    // =========================================================================

    #[test]
    fn test_confidence_simple_identifier() {
        assert_eq!(classify_expression("balance"), Confidence::High);
        assert_eq!(classify_expression("user_id"), Confidence::High);
        assert_eq!(classify_expression("x"), Confidence::High);
    }

    #[test]
    fn test_confidence_attribute_access() {
        assert_eq!(classify_expression("user.name"), Confidence::High);
        assert_eq!(classify_expression("order.total.amount"), Confidence::High);
    }

    #[test]
    fn test_confidence_method_call() {
        assert_eq!(classify_expression("user.get_id()"), Confidence::Medium);
        assert_eq!(classify_expression("get_value()"), Confidence::Medium);
    }

    #[test]
    fn test_confidence_complex() {
        assert_eq!(classify_expression("a + b"), Confidence::Low);
        assert_eq!(classify_expression("func(x, y)"), Confidence::Low);
    }

    // =========================================================================
    // Language Detection Tests
    // =========================================================================

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language("payment.py"), Some("python"));
        assert_eq!(detect_language("main.go"), Some("go"));
        assert_eq!(detect_language("lib.rs"), Some("rust"));
        assert_eq!(detect_language("app.js"), None);
        assert_eq!(detect_language("/path/to/file.py"), Some("python"));
    }

    #[test]
    fn test_get_extractor() {
        assert!(get_extractor("python").is_some());
        assert!(get_extractor("py").is_some());
        assert!(get_extractor("go").is_some());
        assert!(get_extractor("rust").is_some());
        assert!(get_extractor("javascript").is_none());
    }
}
