//! Git diff parser for extracting debug print statements.
//!
//! This module parses unified diff format (git diff) and extracts added lines
//! containing print/log statements, along with the expressions being logged.

use super::expression_extractor::{detect_language, get_extractor, Confidence};
use regex::Regex;
use std::sync::LazyLock;

/// A parsed line from a diff containing a debug statement
#[derive(Debug, Clone)]
pub struct ParsedDiffLine {
    /// The file path
    pub file: String,
    /// The line number in the new file
    pub line: u32,
    /// The extracted expression
    pub expression: String,
    /// The original statement (trimmed)
    pub original_statement: String,
    /// Confidence level for the expression
    pub confidence: Confidence,
}

/// A line that could not be parsed
#[derive(Debug, Clone)]
pub struct UnparseableLine {
    /// The file path
    pub file: String,
    /// The line number in the new file
    pub line: u32,
    /// The original statement (trimmed)
    pub original_statement: String,
    /// Why parsing failed
    pub reason: String,
}

/// Result of parsing a diff
#[derive(Debug, Default)]
pub struct DiffParseResult {
    /// Successfully parsed lines with expressions
    pub parsed: Vec<ParsedDiffLine>,
    /// Lines that contain debug statements but couldn't be fully parsed
    pub unparseable: Vec<UnparseableLine>,
}

impl DiffParseResult {
    /// Check if parsing found any debug statements
    pub fn has_debug_statements(&self) -> bool {
        !self.parsed.is_empty() || !self.unparseable.is_empty()
    }

    /// Get total count of debug statements found
    pub fn total_count(&self) -> usize {
        self.parsed.len() + self.unparseable.len()
    }
}

// Regex to parse hunk headers: @@ -start,count +start,count @@
static HUNK_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@@\s+-(\d+)(?:,\d+)?\s+\+(\d+)(?:,\d+)?\s+@@").unwrap());

// Regex to parse diff file header: diff --git a/path b/path
static DIFF_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^diff --git a/(.+) b/(.+)$").unwrap());

// Regex for +++ b/path (alternative file detection)
static FILE_HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\+\+\+ b/(.+)$").unwrap());

/// Parse a unified diff and extract debug statements.
///
/// # Arguments
/// * `diff` - The diff content in unified format (git diff output)
///
/// # Returns
/// A `DiffParseResult` containing parsed expressions and unparseable lines.
///
/// # Example
/// ```ignore
/// let diff = r#"
/// diff --git a/payment.py b/payment.py
/// @@ -125,6 +125,7 @@
///      balance = get_balance()
/// +    print(f"balance={balance}")
///      if balance >= amount:
/// "#;
///
/// let result = parse_diff(diff);
/// assert_eq!(result.parsed.len(), 1);
/// assert_eq!(result.parsed[0].expression, "balance");
/// ```
pub fn parse_diff(diff: &str) -> DiffParseResult {
    let mut result = DiffParseResult::default();
    let mut current_file: Option<String> = None;
    let mut current_new_line: u32 = 0;

    for line in diff.lines() {
        // Check for diff header (file change)
        if let Some(cap) = DIFF_HEADER.captures(line) {
            current_file = cap.get(2).map(|m| m.as_str().to_string());
            continue;
        }

        // Alternative: +++ b/path
        if let Some(cap) = FILE_HEADER.captures(line) {
            current_file = cap.get(1).map(|m| m.as_str().to_string());
            continue;
        }

        // Check for hunk header
        if let Some(cap) = HUNK_HEADER.captures(line) {
            if let Some(new_start) = cap.get(2) {
                current_new_line = new_start.as_str().parse().unwrap_or(1);
            }
            continue;
        }

        // Skip if no file context
        let Some(ref file) = current_file else {
            continue;
        };

        // Process line based on prefix
        if line.starts_with('+') && !line.starts_with("+++") {
            // Added line
            let content = &line[1..]; // Remove the + prefix
            process_added_line(file, current_new_line, content, &mut result);
            current_new_line += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            // Removed line - don't increment line counter
        } else if !line.starts_with('\\') {
            // Context line or other
            current_new_line += 1;
        }
    }

    result
}

/// Process an added line and extract expressions if it's a debug statement
fn process_added_line(file: &str, line: u32, content: &str, result: &mut DiffParseResult) {
    let content = content.trim();

    // Detect language from file extension
    let Some(language) = detect_language(file) else {
        return; // Unknown language, skip
    };

    // Get the appropriate extractor
    let Some(extractor) = get_extractor(language) else {
        return;
    };

    // Check if this is a debug statement
    if !extractor.is_debug_statement(content) {
        return;
    }

    // Extract expressions
    let expressions = extractor.extract(content);

    if expressions.is_empty() {
        // Is a debug statement but couldn't extract expressions
        result.unparseable.push(UnparseableLine {
            file: file.to_string(),
            line,
            original_statement: content.to_string(),
            reason: "Could not extract expressions from statement".to_string(),
        });
    } else {
        // Add each extracted expression
        for expr in expressions {
            result.parsed.push(ParsedDiffLine {
                file: file.to_string(),
                line,
                expression: expr.expression,
                original_statement: content.to_string(),
                confidence: expr.confidence,
            });
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Basic Parsing Tests
    // =========================================================================

    #[test]
    fn test_parse_simple_python_diff() {
        let diff = r#"
diff --git a/payment.py b/payment.py
index 1234567..abcdefg 100644
--- a/payment.py
+++ b/payment.py
@@ -125,6 +125,7 @@ def process_payment(amount):
     balance = get_balance()
+    print(f"balance={balance}")
     if balance >= amount:
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 1);
        assert_eq!(result.parsed[0].file, "payment.py");
        assert_eq!(result.parsed[0].line, 126);
        assert_eq!(result.parsed[0].expression, "balance");
        assert_eq!(result.parsed[0].confidence, Confidence::High);
    }

    #[test]
    fn test_parse_multiple_expressions_same_line() {
        let diff = r#"
diff --git a/app.py b/app.py
@@ -10,6 +10,7 @@
     x = 1
+    print(f"x={x}, y={y}")
     return
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 2);
        assert_eq!(result.parsed[0].expression, "x");
        assert_eq!(result.parsed[1].expression, "y");
        // Both should have the same line number
        assert_eq!(result.parsed[0].line, result.parsed[1].line);
    }

    #[test]
    fn test_parse_multiple_files() {
        let diff = r#"
diff --git a/auth.py b/auth.py
@@ -10,6 +10,7 @@
     user = get_user()
+    print(f"user={user}")
     return user

diff --git a/payment.py b/payment.py
@@ -20,6 +20,7 @@
     amount = calculate()
+    print(f"amount={amount}")
     return amount
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 2);
        assert_eq!(result.parsed[0].file, "auth.py");
        assert_eq!(result.parsed[0].expression, "user");
        assert_eq!(result.parsed[1].file, "payment.py");
        assert_eq!(result.parsed[1].expression, "amount");
    }

    #[test]
    fn test_parse_go_diff() {
        let diff = r#"
diff --git a/main.go b/main.go
@@ -50,6 +50,7 @@
     balance := getBalance()
+    fmt.Printf("balance: %v\n", balance)
     return balance
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 1);
        assert_eq!(result.parsed[0].file, "main.go");
        assert_eq!(result.parsed[0].expression, "balance");
    }

    #[test]
    fn test_parse_rust_diff() {
        let diff = r#"
diff --git a/lib.rs b/lib.rs
@@ -100,6 +100,7 @@
     let user = get_user();
+    dbg!(user);
     user
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 1);
        assert_eq!(result.parsed[0].file, "lib.rs");
        assert_eq!(result.parsed[0].expression, "user");
    }

    // =========================================================================
    // Line Number Calculation Tests
    // =========================================================================

    #[test]
    fn test_line_number_from_hunk_header() {
        let diff = r#"
diff --git a/file.py b/file.py
@@ -100,6 +100,7 @@
     x = 1
+    print(f"x={x}")
     return
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed[0].line, 101); // 100 (start) + 1 (context line)
    }

    #[test]
    fn test_line_number_multiple_added_lines() {
        let diff = r#"
diff --git a/file.py b/file.py
@@ -10,6 +10,8 @@
     start
+    print(f"a={a}")
+    print(f"b={b}")
     end
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 2);
        assert_eq!(result.parsed[0].line, 11);
        assert_eq!(result.parsed[1].line, 12);
    }

    #[test]
    fn test_line_number_with_removed_lines() {
        let diff = r#"
diff --git a/file.py b/file.py
@@ -10,7 +10,7 @@
     start
-    old_line
+    print(f"x={x}")
     end
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 1);
        // Removed line doesn't affect new file line numbers
        assert_eq!(result.parsed[0].line, 11);
    }

    // =========================================================================
    // Unparseable Lines Tests
    // =========================================================================

    #[test]
    fn test_unparseable_complex_expression() {
        let diff = r#"
diff --git a/file.py b/file.py
@@ -10,6 +10,7 @@
     x = 1
+    print(compute(a, b, c))
     return
"#;
        let result = parse_diff(diff);
        // print() with complex expression might not extract cleanly
        // This depends on the extractor implementation
        assert!(result.has_debug_statements());
    }

    #[test]
    fn test_no_debug_statements() {
        let diff = r#"
diff --git a/file.py b/file.py
@@ -10,6 +10,7 @@
     x = 1
+    y = 2
     return
"#;
        let result = parse_diff(diff);
        assert!(!result.has_debug_statements());
        assert_eq!(result.parsed.len(), 0);
        assert_eq!(result.unparseable.len(), 0);
    }

    // =========================================================================
    // Edge Cases
    // =========================================================================

    #[test]
    fn test_empty_diff() {
        let result = parse_diff("");
        assert_eq!(result.parsed.len(), 0);
        assert_eq!(result.unparseable.len(), 0);
    }

    #[test]
    fn test_diff_without_hunks() {
        let diff = r#"
diff --git a/file.py b/file.py
index 1234567..abcdefg 100644
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 0);
    }

    #[test]
    fn test_unknown_file_type() {
        let diff = r#"
diff --git a/file.txt b/file.txt
@@ -10,6 +10,7 @@
     text
+    print(x)
     more text
"#;
        let result = parse_diff(diff);
        // .txt files don't have an extractor
        assert_eq!(result.parsed.len(), 0);
    }

    #[test]
    fn test_multiple_hunks() {
        let diff = r#"
diff --git a/file.py b/file.py
@@ -10,6 +10,7 @@
     first
+    print(f"a={a}")
     end

@@ -50,6 +51,7 @@
     second
+    print(f"b={b}")
     end
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 2);
        assert_eq!(result.parsed[0].line, 11);
        assert_eq!(result.parsed[1].line, 52); // 51 (new start) + 1 (context)
    }

    #[test]
    fn test_attribute_access_expression() {
        let diff = r#"
diff --git a/file.py b/file.py
@@ -10,6 +10,7 @@
     user = get_user()
+    print(f"name={user.profile.name}")
     return
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 1);
        assert_eq!(result.parsed[0].expression, "user.profile.name");
        assert_eq!(result.parsed[0].confidence, Confidence::High);
    }

    #[test]
    fn test_method_call_expression() {
        let diff = r#"
diff --git a/file.py b/file.py
@@ -10,6 +10,7 @@
     user = get_user()
+    print(f"id={user.get_id()}")
     return
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 1);
        assert_eq!(result.parsed[0].expression, "user.get_id()");
        assert_eq!(result.parsed[0].confidence, Confidence::Medium);
    }

    // =========================================================================
    // Alternative File Header Format Tests
    // =========================================================================

    #[test]
    fn test_plus_plus_plus_file_header() {
        let diff = r#"
--- a/old.py
+++ b/new.py
@@ -10,6 +10,7 @@
     x = 1
+    print(f"x={x}")
     return
"#;
        let result = parse_diff(diff);
        assert_eq!(result.parsed.len(), 1);
        assert_eq!(result.parsed[0].file, "new.py");
    }

    #[test]
    fn test_total_count() {
        let diff = r#"
diff --git a/file.py b/file.py
@@ -10,6 +10,8 @@
     x = 1
+    print(f"a={a}")
+    print(f"b={b}")
     return
"#;
        let result = parse_diff(diff);
        assert_eq!(result.total_count(), 2);
    }
}
