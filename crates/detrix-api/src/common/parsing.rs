//! Common parsing utilities for API layers
//!
//! This module provides reusable parsing functions used across MCP and REST APIs.
//!
//! ## Location Format
//!
//! Uses `#` as line separator (not `:`) to avoid conflicts with Windows paths.
//!
//! Supported formats:
//! - `"expression"@file.py#42` - full format with quoted expression
//! - `@file.py#42` - with @ prefix
//! - `file.py#42` - without @ prefix
//! - `file.py` + line=42 - separate line parameter

use detrix_core::Location;
use rmcp::ErrorData as McpError;
use std::collections::HashSet;

/// Result of parsing a location with optional expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLocationWithExpression {
    pub file: String,
    pub line: u32,
    pub expression: Option<String>,
}

/// Parse a location string in the format `@file.py#line` into a `Location`.
///
/// # Arguments
/// * `location` - A string in the format `@file.py#line` (e.g., `@auth.py#127`)
///
/// # Returns
/// * `Ok(Location)` - Successfully parsed location
/// * `Err(McpError)` - Invalid format with descriptive error message
///
/// # Example
/// ```ignore
/// let loc = parse_location("@auth.py#127")?;
/// assert_eq!(loc.file, "auth.py");
/// assert_eq!(loc.line, 127);
/// ```
pub fn parse_location(location: &str) -> Result<Location, McpError> {
    // Delegate to flexible parser with no line override
    parse_location_flexible(location, None)
}

/// Parse location from flexible input formats.
///
/// Accepts:
/// - `@file.py#123` (standard format)
/// - `file.py#123` (missing @, auto-handled)
/// - `file.py` + line=123 (separate params)
/// - `/absolute/path/file.py#123`
/// - `./relative/path/file.py#123`
/// - `C:/Windows/path.py#123` (Windows paths work naturally with # separator)
///
/// # Arguments
/// * `location` - Location string in any supported format
/// * `line_override` - Optional line number that takes precedence over line in string
///
/// # Returns
/// * `Ok(Location)` - Successfully parsed location
/// * `Err(McpError)` - Invalid format with helpful error message
///
/// # Example
/// ```ignore
/// // All of these work:
/// parse_location_flexible("@auth.py#127", None)?;
/// parse_location_flexible("auth.py#127", None)?;
/// parse_location_flexible("auth.py", Some(127))?;
/// parse_location_flexible("@auth.py#100", Some(127))?;  // Uses 127
/// parse_location_flexible("C:/Users/test.py#10", None)?;  // Windows path
/// ```
pub fn parse_location_flexible(
    location: &str,
    line_override: Option<u32>,
) -> Result<Location, McpError> {
    let location = location.trim();

    if location.is_empty() {
        return Err(McpError::invalid_params(
            "Location cannot be empty.\n\
             Expected formats:\n\
             - 'file.py#123'\n\
             - '@file.py#123'\n\
             - 'file.py' with line parameter",
            None,
        ));
    }

    // Strip @ if present (optional now)
    let location = location.strip_prefix('@').unwrap_or(location);

    // Normalize path (remove leading ./)
    let location = location.strip_prefix("./").unwrap_or(location);

    // Try to extract line from string using rsplit_once('#')
    // Using '#' instead of ':' to avoid conflicts with Windows paths like C:/
    if let Some((file, line_str)) = location.rsplit_once('#') {
        // Only treat as line if it's actually a number
        if let Ok(parsed_line) = line_str.parse::<u32>() {
            let final_line = line_override.unwrap_or(parsed_line);
            return Ok(Location {
                file: file.to_string(),
                line: final_line,
            });
        }
        // Not a number after # - treat whole thing as filename
        // Fall through to check line_override
    }

    // No line in string, must have line_override
    if let Some(line) = line_override {
        return Ok(Location {
            file: location.to_string(),
            line,
        });
    }

    // No line anywhere - return helpful error
    Err(McpError::invalid_params(
        format!(
            "Could not determine line number.\n\
             You provided: '{}'\n\
             \n\
             Expected formats:\n\
             - 'file.py#123' or '@file.py#123'\n\
             - 'file.py' with separate line=123 parameter\n\
             \n\
             Hint: Use inspect_file to find the correct line number.",
            location
        ),
        None,
    ))
}

/// Parse location with optional embedded expression.
///
/// Formats:
/// - `"expression"@file.py#42` → (file="file.py", line=42, expression=Some("expression"))
/// - `@file.py#42` → (file="file.py", line=42, expression=None)
/// - `file.py#42` → (file="file.py", line=42, expression=None)
/// - `file.py` + line=42 → (file="file.py", line=42, expression=None)
///
/// # Arguments
/// * `location` - Location string with optional quoted expression prefix
/// * `line_override` - Optional line number that takes precedence over line in string
///
/// # Returns
/// * `Ok(ParsedLocationWithExpression)` - Successfully parsed location with optional expression
/// * `Err(McpError)` - Invalid format with helpful error message
///
/// # Example
/// ```ignore
/// let result = parse_location_with_expression("\"balance\"@payment.py#42", None)?;
/// assert_eq!(result.file, "payment.py");
/// assert_eq!(result.line, 42);
/// assert_eq!(result.expression, Some("balance".to_string()));
/// ```
pub fn parse_location_with_expression(
    location: &str,
    line_override: Option<u32>,
) -> Result<ParsedLocationWithExpression, McpError> {
    let trimmed = location.trim();

    if trimmed.is_empty() {
        return Err(McpError::invalid_params(
            "Location cannot be empty.\n\
             Expected formats:\n\
             - 'file.py#123'\n\
             - '@file.py#123'\n\
             - '\"expression\"@file.py#123'",
            None,
        ));
    }

    // Step 1: Check for quoted expression prefix: "expr"@file#line
    let (expression, remainder) = if let Some(stripped) = trimmed.strip_prefix('"') {
        // Find closing quote
        if let Some(close_idx) = stripped.find('"') {
            let expr = &stripped[..close_idx];
            let rest = &stripped[close_idx + 1..]; // Skip closing quote
                                                   // Rest should start with @ (mandatory after expression)
            let rest = rest.strip_prefix('@').ok_or_else(|| {
                McpError::invalid_params(
                    "Expression format requires '@' after closing quote.\n\
                     Expected: '\"expression\"@file.py#123'",
                    None,
                )
            })?;
            (Some(expr.to_string()), rest)
        } else {
            return Err(McpError::invalid_params(
                "Unclosed quote in expression.\n\
                 Expected: '\"expression\"@file.py#123'",
                None,
            ));
        }
    } else {
        // No expression, strip optional @ prefix
        let rest = trimmed.strip_prefix('@').unwrap_or(trimmed);
        (None, rest)
    };

    // Step 2: Normalize path (remove leading ./)
    let remainder = remainder.strip_prefix("./").unwrap_or(remainder);

    // Step 3: Parse file#line from remainder
    if let Some((file, line_str)) = remainder.rsplit_once('#') {
        if let Ok(parsed_line) = line_str.parse::<u32>() {
            let final_line = line_override.unwrap_or(parsed_line);
            return Ok(ParsedLocationWithExpression {
                file: file.to_string(),
                line: final_line,
                expression,
            });
        }
        // Not a valid line number after #
    }

    // No line in string, check line_override
    if let Some(line) = line_override {
        return Ok(ParsedLocationWithExpression {
            file: remainder.to_string(),
            line,
            expression,
        });
    }

    // No line anywhere - return helpful error
    Err(McpError::invalid_params(
        format!(
            "Could not determine line number.\n\
             You provided: '{}'\n\
             \n\
             Expected formats:\n\
             - 'file.py#123' or '@file.py#123'\n\
             - '\"expression\"@file.py#123'\n\
             - 'file.py' with separate line=123 parameter",
            location
        ),
        None,
    ))
}

/// Extract variable names from an expression.
///
/// Identifies variable names that would need to be in scope for the expression to evaluate.
/// Excludes:
/// - Method/function names (followed by parentheses)
/// - Attribute access targets (after dots)
/// - Language keywords
/// - Numeric literals
///
/// # Example
/// ```ignore
/// extract_variable_names("user.balance + tax") // -> ["user", "tax"]
/// extract_variable_names("len(items)")         // -> ["items"]
/// ```
pub fn extract_variable_names(expression: &str) -> Vec<String> {
    let mut variables = Vec::new();
    let mut current_token = String::new();
    let chars: Vec<char> = expression.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut after_dot = false;

    while i < len {
        let c = chars[i];

        if c.is_alphanumeric() || c == '_' {
            current_token.push(c);
            i += 1;
        } else {
            if !current_token.is_empty() {
                // Check if this token should be included
                let is_function_call = c == '(';
                let is_numeric = current_token
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false);

                if !is_function_call && !after_dot && !is_keyword(&current_token) && !is_numeric {
                    variables.push(current_token.clone());
                }
                current_token.clear();
            }

            // Track if we just saw a dot (next token will be an attribute)
            after_dot = c == '.';
            i += 1;
        }
    }

    // Don't forget the last token
    if !current_token.is_empty() {
        let is_numeric = current_token
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false);
        if !after_dot && !is_keyword(&current_token) && !is_numeric {
            variables.push(current_token);
        }
    }

    // Remove duplicates while preserving order
    let mut seen = HashSet::new();
    variables.retain(|v| seen.insert(v.clone()));

    variables
}

/// Generate a metric name from file and line context.
///
/// Format: `filename_lineNumber` (e.g., `auth_py_42`)
///
/// The filename is extracted from the path and sanitized:
/// - Uses the file basename (last path component)
/// - Replaces dots with underscores for metric-safe names
///
/// # Arguments
/// * `file` - File path (can be absolute or relative)
/// * `line` - Line number in the file
///
/// # Example
/// ```ignore
/// generate_metric_name("/path/to/auth.py", 42) // -> "auth_py_42"
/// generate_metric_name("src/api/handler.rs", 100) // -> "handler_rs_100"
/// ```
pub fn generate_metric_name(file: &str, line: u32) -> String {
    // Extract filename (with extension)
    // Handle both Unix (/) and Windows (\) path separators
    let filename = file.rsplit(['/', '\\']).next().unwrap_or("unknown");

    // Sanitize filename for metric name (replace dots with underscores)
    let sanitized = filename.replace('.', "_");
    format!("{}_{}", sanitized, line)
}

/// Generate a metric name with a prefix.
///
/// Format: `prefix_filename_lineNumber` (e.g., `diff_auth_py_42`)
///
/// Useful for distinguishing metrics created by different sources.
///
/// # Arguments
/// * `prefix` - Prefix to prepend (e.g., "diff", "auto")
/// * `file` - File path (can be absolute or relative)
/// * `line` - Line number in the file
///
/// # Example
/// ```ignore
/// generate_metric_name_with_prefix("diff", "/path/to/auth.py", 42) // -> "diff_auth_py_42"
/// ```
pub fn generate_metric_name_with_prefix(prefix: &str, file: &str, line: u32) -> String {
    let base_name = generate_metric_name(file, line);
    format!("{}_{}", prefix, base_name)
}

/// Check if a string is a language keyword or builtin constant
fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        // Python
        "True" | "False" | "None" |
        // JavaScript/JSON
        "true" | "false" | "null" | "undefined" |
        // Go
        "nil" |
        // Common keywords across languages
        "if" | "else" | "for" | "while" | "return" | "break" | "continue" |
        "def" | "class" | "import" | "from" | "as" | "in" | "is" | "not" |
        "and" | "or" | "try" | "except" | "finally" | "with" | "lambda" |
        "self" | "this"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // parse_location tests
    // ========================================================================

    #[test]
    fn test_parse_location_valid() {
        let loc = parse_location("@auth.py#127").unwrap();
        assert_eq!(loc.file, "auth.py");
        assert_eq!(loc.line, 127);
    }

    #[test]
    fn test_parse_location_with_path() {
        let loc = parse_location("@src/utils/helper.py#42").unwrap();
        assert_eq!(loc.file, "src/utils/helper.py");
        assert_eq!(loc.line, 42);
    }

    #[test]
    fn test_parse_location_without_at_now_works() {
        // Changed behavior: @ is now optional
        let loc = parse_location("auth.py#127").unwrap();
        assert_eq!(loc.file, "auth.py");
        assert_eq!(loc.line, 127);
    }

    #[test]
    fn test_parse_location_missing_line() {
        let result = parse_location("@auth.py");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Could not determine line"));
    }

    #[test]
    fn test_parse_location_invalid_line() {
        let result = parse_location("@auth.py#abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_location_empty() {
        let result = parse_location("");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("cannot be empty"));
    }

    #[test]
    fn test_parse_location_windows_path() {
        // Windows-style path with drive letter - works naturally with # separator
        let loc = parse_location("@C:/Users/test.py#10").unwrap();
        assert_eq!(loc.file, "C:/Users/test.py");
        assert_eq!(loc.line, 10);
    }

    // ========================================================================
    // parse_location_flexible tests
    // ========================================================================

    #[test]
    fn test_flexible_with_at_prefix() {
        let loc = parse_location_flexible("@file.py#123", None).unwrap();
        assert_eq!(loc.file, "file.py");
        assert_eq!(loc.line, 123);
    }

    #[test]
    fn test_flexible_without_at_prefix() {
        let loc = parse_location_flexible("file.py#123", None).unwrap();
        assert_eq!(loc.file, "file.py");
        assert_eq!(loc.line, 123);
    }

    #[test]
    fn test_flexible_separate_line_param() {
        let loc = parse_location_flexible("file.py", Some(123)).unwrap();
        assert_eq!(loc.file, "file.py");
        assert_eq!(loc.line, 123);
    }

    #[test]
    fn test_flexible_line_override_takes_precedence() {
        // line param overrides line in string
        let loc = parse_location_flexible("file.py#100", Some(200)).unwrap();
        assert_eq!(loc.file, "file.py");
        assert_eq!(loc.line, 200);
    }

    #[test]
    fn test_flexible_absolute_path() {
        let loc = parse_location_flexible("/home/user/project/file.py#42", None).unwrap();
        assert_eq!(loc.file, "/home/user/project/file.py");
        assert_eq!(loc.line, 42);
    }

    #[test]
    fn test_flexible_relative_path_with_dot_slash() {
        let loc = parse_location_flexible("./src/main.py#10", None).unwrap();
        assert_eq!(loc.file, "src/main.py"); // Normalized
        assert_eq!(loc.line, 10);
    }

    #[test]
    fn test_flexible_windows_path() {
        // Windows path with drive letter works naturally with # separator
        let loc = parse_location_flexible("C:/Users/test.py#10", None).unwrap();
        assert_eq!(loc.file, "C:/Users/test.py");
        assert_eq!(loc.line, 10);
    }

    #[test]
    fn test_flexible_error_no_line() {
        let err = parse_location_flexible("file.py", None).unwrap_err();
        assert!(err.message.contains("Could not determine line"));
        assert!(err.message.contains("file.py")); // echoes input
    }

    #[test]
    fn test_flexible_error_empty() {
        let err = parse_location_flexible("", None).unwrap_err();
        assert!(err.message.contains("cannot be empty"));
    }

    #[test]
    fn test_flexible_whitespace_trimmed() {
        let loc = parse_location_flexible("  file.py#42  ", None).unwrap();
        assert_eq!(loc.file, "file.py");
        assert_eq!(loc.line, 42);
    }

    // ========================================================================
    // parse_location_with_expression tests
    // ========================================================================

    #[test]
    fn test_with_expression_full_format() {
        let result = parse_location_with_expression("\"balance\"@payment.py#42", None).unwrap();
        assert_eq!(result.file, "payment.py");
        assert_eq!(result.line, 42);
        assert_eq!(result.expression, Some("balance".to_string()));
    }

    #[test]
    fn test_with_expression_complex_expression() {
        let result =
            parse_location_with_expression("\"user.profile.email\"@auth.py#100", None).unwrap();
        assert_eq!(result.file, "auth.py");
        assert_eq!(result.line, 100);
        assert_eq!(result.expression, Some("user.profile.email".to_string()));
    }

    #[test]
    fn test_with_expression_without_expression() {
        let result = parse_location_with_expression("@file.py#42", None).unwrap();
        assert_eq!(result.file, "file.py");
        assert_eq!(result.line, 42);
        assert_eq!(result.expression, None);
    }

    #[test]
    fn test_with_expression_without_at_prefix() {
        let result = parse_location_with_expression("file.py#42", None).unwrap();
        assert_eq!(result.file, "file.py");
        assert_eq!(result.line, 42);
        assert_eq!(result.expression, None);
    }

    #[test]
    fn test_with_expression_line_override() {
        let result = parse_location_with_expression("\"x\"@file.py#10", Some(99)).unwrap();
        assert_eq!(result.file, "file.py");
        assert_eq!(result.line, 99); // Override takes precedence
        assert_eq!(result.expression, Some("x".to_string()));
    }

    #[test]
    fn test_with_expression_separate_line_param() {
        let result = parse_location_with_expression("\"balance\"@payment.py", Some(42)).unwrap();
        assert_eq!(result.file, "payment.py");
        assert_eq!(result.line, 42);
        assert_eq!(result.expression, Some("balance".to_string()));
    }

    #[test]
    fn test_with_expression_windows_path() {
        let result = parse_location_with_expression("\"var\"@C:/Users/test.py#10", None).unwrap();
        assert_eq!(result.file, "C:/Users/test.py");
        assert_eq!(result.line, 10);
        assert_eq!(result.expression, Some("var".to_string()));
    }

    #[test]
    fn test_with_expression_unclosed_quote() {
        let result = parse_location_with_expression("\"balance@file.py#42", None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Unclosed quote"));
    }

    #[test]
    fn test_with_expression_missing_at_after_quote() {
        let result = parse_location_with_expression("\"balance\"file.py#42", None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("requires '@' after closing quote"));
    }

    #[test]
    fn test_with_expression_empty() {
        let result = parse_location_with_expression("", None);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("cannot be empty"));
    }

    // ========================================================================
    // extract_variable_names tests
    // ========================================================================

    #[test]
    fn test_extract_variables_simple() {
        let vars = extract_variable_names("user");
        assert_eq!(vars, vec!["user"]);
    }

    #[test]
    fn test_extract_variables_attribute_access() {
        let vars = extract_variable_names("user.name");
        assert_eq!(vars, vec!["user"]);
        // "name" excluded because it's after a dot
    }

    #[test]
    fn test_extract_variables_expression() {
        let vars = extract_variable_names("amount + tax * rate");
        assert!(vars.contains(&"amount".to_string()));
        assert!(vars.contains(&"tax".to_string()));
        assert!(vars.contains(&"rate".to_string()));
    }

    #[test]
    fn test_extract_variables_method_call() {
        let vars = extract_variable_names("user.get_balance()");
        assert_eq!(vars, vec!["user"]);
        // get_balance excluded because followed by (
    }

    #[test]
    fn test_extract_variables_function_call() {
        let vars = extract_variable_names("len(items)");
        assert_eq!(vars, vec!["items"]);
        // len excluded because followed by (
    }

    #[test]
    fn test_extract_variables_nested() {
        let vars = extract_variable_names("user.orders[0].total");
        assert_eq!(vars, vec!["user"]);
    }

    #[test]
    fn test_extract_variables_dict_literal() {
        let vars = extract_variable_names("{'a': x, 'b': y}");
        assert!(vars.contains(&"x".to_string()));
        assert!(vars.contains(&"y".to_string()));
    }

    #[test]
    fn test_extract_variables_excludes_keywords() {
        let vars = extract_variable_names("x if condition else y");
        assert!(vars.contains(&"x".to_string()));
        assert!(vars.contains(&"condition".to_string()));
        assert!(vars.contains(&"y".to_string()));
        assert!(!vars.contains(&"if".to_string()));
        assert!(!vars.contains(&"else".to_string()));
    }

    #[test]
    fn test_extract_variables_excludes_constants() {
        let vars = extract_variable_names("x + True + None");
        assert_eq!(vars, vec!["x"]);
    }

    #[test]
    fn test_extract_variables_no_duplicates() {
        let vars = extract_variable_names("x + x + x");
        assert_eq!(vars, vec!["x"]);
    }

    #[test]
    fn test_extract_variables_complex() {
        let vars = extract_variable_names("transaction.amount if user.is_active else 0");
        assert!(vars.contains(&"transaction".to_string()));
        assert!(vars.contains(&"user".to_string()));
        assert!(!vars.contains(&"amount".to_string())); // after dot
        assert!(!vars.contains(&"is_active".to_string())); // after dot
    }

    // ========================================================================
    // generate_metric_name tests
    // ========================================================================

    #[test]
    fn test_generate_metric_name_simple() {
        assert_eq!(generate_metric_name("auth.py", 42), "auth_py_42");
    }

    #[test]
    fn test_generate_metric_name_with_path() {
        assert_eq!(generate_metric_name("/path/to/auth.py", 42), "auth_py_42");
    }

    #[test]
    fn test_generate_metric_name_rust_file() {
        assert_eq!(
            generate_metric_name("src/api/handler.rs", 100),
            "handler_rs_100"
        );
    }

    #[test]
    fn test_generate_metric_name_windows_path() {
        assert_eq!(
            generate_metric_name("C:\\Users\\test\\file.py", 10),
            "file_py_10"
        );
    }

    #[test]
    fn test_generate_metric_name_with_prefix() {
        assert_eq!(
            generate_metric_name_with_prefix("diff", "/path/to/auth.py", 42),
            "diff_auth_py_42"
        );
    }

    #[test]
    fn test_generate_metric_name_with_prefix_auto() {
        assert_eq!(
            generate_metric_name_with_prefix("auto", "handler.rs", 100),
            "auto_handler_rs_100"
        );
    }
}
