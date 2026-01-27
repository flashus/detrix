//! Common utilities and traits for LSP purity analyzers
//!
//! This module extracts shared patterns from language-specific analyzers
//! to reduce code duplication.

use std::collections::HashSet;

// ============================================================================
// ScopeTracker Trait
// ============================================================================

/// Trait for scope-aware variable tracking in purity analysis
///
/// Each language implements this trait for its scope context type,
/// allowing common algorithms to work with language-specific contexts.
///
/// # Language-specific extensions
///
/// While this trait defines the common interface, each language can add
/// specific methods to their scope context:
/// - Python: `is_global()`, `is_nonlocal()`, `add_global()`, `add_nonlocal()`
/// - Go: `is_receiver()`, `is_package_var()`, `set_receiver()`, `add_package_var()`
/// - Rust: `is_mut_self()`, `is_mut_parameter()`, `is_static()`, `add_mut_parameter()`, `add_static()`
pub trait ScopeTracker {
    /// Create a new scope context with given function parameters
    fn with_parameters(parameters: Vec<String>) -> Self;

    /// Check if a variable is a local variable (created in this function)
    ///
    /// Local variables are safe to mutate without affecting purity.
    fn is_local(&self, name: &str) -> bool;

    /// Check if a variable is a function parameter
    ///
    /// Mutations to parameters may affect caller's data (language-dependent).
    fn is_parameter(&self, name: &str) -> bool;

    /// Add a local variable (from assignment/declaration)
    ///
    /// The variable will only be tracked as local if it's not already
    /// tracked as a parameter or other external scope.
    fn add_local(&mut self, name: String);
}

// ============================================================================
// Common Scope Context Base
// ============================================================================

/// Base fields for scope context that all languages share
///
/// Each language's scope context contains these fields plus language-specific ones.
#[derive(Debug, Default, Clone)]
pub struct ScopeContextBase {
    /// Variables created locally in this function
    pub local_vars: HashSet<String>,
    /// Function parameters
    pub parameters: HashSet<String>,
}

impl ScopeContextBase {
    /// Create a new base scope context with given parameters
    pub fn new(parameters: Vec<String>) -> Self {
        Self {
            parameters: parameters.into_iter().collect(),
            local_vars: HashSet::new(),
        }
    }

    /// Check if a variable is in the local_vars set
    pub fn has_local(&self, name: &str) -> bool {
        self.local_vars.contains(name)
    }

    /// Check if a variable is in the parameters set
    pub fn has_parameter(&self, name: &str) -> bool {
        self.parameters.contains(name)
    }

    /// Add a variable to local_vars if not a parameter
    ///
    /// This is the basic logic - language-specific implementations
    /// may add additional checks.
    pub fn try_add_local(&mut self, name: String) -> bool {
        if !self.parameters.contains(&name) {
            self.local_vars.insert(name);
            true
        } else {
            false
        }
    }
}

// ============================================================================
// Function Classification Helpers
// ============================================================================

/// Check if a function name matches any pattern in a list
///
/// Supports both exact matches and suffix matches (for qualified names).
/// For example, "foo.bar" matches both "bar" and "foo.bar".
pub fn matches_function_list(name: &str, list: &[&str]) -> bool {
    list.iter()
        .any(|&pattern| name == pattern || name.ends_with(pattern))
}

/// Check if a function name matches any pattern with exact match only
pub fn matches_function_exact(name: &str, list: &[&str]) -> bool {
    list.contains(&name)
}

// ============================================================================
// URI/Path Utilities
// ============================================================================

/// Check if a URI points to external code that should not be traversed
///
/// External code includes:
/// - Standard library
/// - Type stubs (.pyi files)
/// - Third-party packages (site-packages, vendor, etc.)
///
/// # Arguments
/// * `uri` - The URI to check
/// * `external_patterns` - Language-specific patterns for external code
///
/// # Example
/// ```ignore
/// const PYTHON_EXTERNAL: &[&str] = &["/typeshed/", "/site-packages/", ".pyi"];
/// let is_external = is_external_uri("file:///usr/lib/python3.10/typing.pyi", PYTHON_EXTERNAL);
/// ```
pub fn is_external_uri(uri: &str, external_patterns: &[&str]) -> bool {
    external_patterns
        .iter()
        .any(|pattern| uri.contains(pattern))
}

/// Common patterns for Python external code
pub const PYTHON_EXTERNAL_PATTERNS: &[&str] = &[
    "/typeshed/",
    "/typeshed-fallback/",
    "/bundled/stubs/",
    "/site-packages/",
    "/lib/python",
    "/Lib/",        // Windows Python stdlib
    "builtins.pyi", // Built-in stubs
    "_collections_abc.pyi",
    "typing.pyi",
    "typing_extensions.pyi",
    ".pyi", // Any stub file is external
];

/// Common patterns for Go external code
pub const GO_EXTERNAL_PATTERNS: &[&str] = &[
    "/go/src/",        // GOROOT stdlib
    "/go/pkg/",        // GOROOT packages
    "/pkg/mod/",       // Go modules cache
    "/vendor/",        // Vendored dependencies
    "go.googlesource", // Go project sources
];

/// Common patterns for Rust external code
pub const RUST_EXTERNAL_PATTERNS: &[&str] = &[
    "/rustlib/",         // Rust stdlib
    "/registry/src/",    // Cargo registry (crates.io)
    "/cargo/registry/",  // Alternative cargo path
    "/.cargo/registry/", // Home-relative cargo path
    "/rust/library/",    // Rust source
    "/toolchains/",      // Rustup toolchains
];

// ============================================================================
// Function Extraction Helpers
// ============================================================================

/// Result of finding a function definition in source code
pub struct FunctionLocation {
    /// Line number where the function starts (0-indexed)
    pub start_line: usize,
    /// The definition line(s) as a single string
    pub definition: String,
    /// Line number where the signature ends (where '{' is found)
    pub signature_end: usize,
}

/// Find a function definition in source lines using pattern matching
///
/// Searches for any of the given patterns and handles multi-line signatures
/// by continuing until a `{` is found.
///
/// # Arguments
/// * `lines` - Source file lines
/// * `patterns` - Patterns to match (e.g., `["fn foo(", "pub fn foo("]`)
///
/// # Returns
/// `Some(FunctionLocation)` if found, `None` otherwise
pub fn find_function_definition(lines: &[&str], patterns: &[String]) -> Option<FunctionLocation> {
    let mut func_start = None;
    let mut def_line = String::new();

    // Find the function definition line
    for (line_num, line) in lines.iter().enumerate() {
        for pattern in patterns {
            if line.contains(pattern) {
                func_start = Some(line_num);
                def_line = (*line).to_string();
                break;
            }
        }
        if func_start.is_some() {
            break;
        }
    }

    let start = func_start?;

    // Handle multi-line signatures - find the opening brace
    let mut signature_end = start;
    for (i, line) in lines.iter().enumerate().skip(start) {
        if line.contains('{') {
            signature_end = i;
            break;
        }
        // Build complete signature for parameter extraction
        if i > start {
            def_line.push(' ');
            def_line.push_str(line.trim());
        }
    }

    Some(FunctionLocation {
        start_line: start,
        definition: def_line,
        signature_end,
    })
}

/// Extract function body using brace counting (for Go and Rust)
///
/// Starts from the function start line and collects lines until braces balance.
///
/// # Arguments
/// * `lines` - Source file lines
/// * `start_line` - Line number where the function starts
/// * `signature_end` - Line number where the signature ends (where '{' is found)
///
/// # Returns
/// The function body as a string (lines between signature and closing brace)
pub fn extract_body_with_braces(lines: &[&str], start_line: usize, signature_end: usize) -> String {
    let mut body_lines = Vec::new();
    let mut brace_count = 0;
    let mut found_opening_brace = false;

    for (i, line) in lines.iter().enumerate().skip(start_line) {
        // Track braces
        for ch in line.chars() {
            if ch == '{' {
                brace_count += 1;
                found_opening_brace = true;
            } else if ch == '}' {
                brace_count -= 1;
            }
        }

        // Skip lines before the function body starts
        if !found_opening_brace {
            continue;
        }

        // After the signature, collect body lines
        if i > signature_end {
            body_lines.push(*line);
        }

        // End of function when braces balance
        if found_opening_brace && brace_count == 0 {
            break;
        }
    }

    body_lines.join("\n")
}

/// Extract function body using indentation (for Python)
///
/// Collects lines with greater indentation than the definition line.
///
/// # Arguments
/// * `lines` - Source file lines
/// * `start_line` - Line number where the function starts
/// * `base_indent` - Indentation level of the `def` line
///
/// # Returns
/// The function body as a string
pub fn extract_body_with_indent(lines: &[&str], start_line: usize, base_indent: usize) -> String {
    let mut body_lines = Vec::new();

    for line in lines.iter().skip(start_line + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Empty lines are part of the body
            continue;
        }

        let current_indent = line.len() - line.trim_start().len();
        if current_indent <= base_indent && !trimmed.is_empty() {
            // End of function - found a line with same or less indentation
            break;
        }

        body_lines.push(*line);
    }

    body_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scope_context_base() {
        let mut ctx = ScopeContextBase::new(vec!["a".to_string(), "b".to_string()]);

        assert!(ctx.has_parameter("a"));
        assert!(ctx.has_parameter("b"));
        assert!(!ctx.has_parameter("c"));

        // Can't add parameter as local
        assert!(!ctx.try_add_local("a".to_string()));
        assert!(!ctx.has_local("a"));

        // Can add new variable as local
        assert!(ctx.try_add_local("c".to_string()));
        assert!(ctx.has_local("c"));
    }

    #[test]
    fn test_matches_function_list() {
        let list = &["print", "logging.info", "os.path.join"];

        // Exact matches
        assert!(matches_function_list("print", list));
        assert!(matches_function_list("logging.info", list));

        // Suffix matches
        assert!(matches_function_list("builtins.print", list));
        assert!(matches_function_list("mymodule.logging.info", list));

        // No match
        assert!(!matches_function_list("printf", list));
        assert!(!matches_function_list("info", list));
    }

    #[test]
    fn test_is_external_uri() {
        assert!(is_external_uri(
            "file:///usr/lib/python3.10/site-packages/numpy/__init__.py",
            PYTHON_EXTERNAL_PATTERNS
        ));
        assert!(is_external_uri(
            "file:///home/user/.local/lib/python3.10/site-packages/requests/api.py",
            PYTHON_EXTERNAL_PATTERNS
        ));
        assert!(is_external_uri(
            "file:///usr/share/typeshed/stdlib/typing.pyi",
            PYTHON_EXTERNAL_PATTERNS
        ));
        assert!(!is_external_uri(
            "file:///home/user/project/src/mymodule.py",
            PYTHON_EXTERNAL_PATTERNS
        ));
    }

    #[test]
    fn test_find_function_definition() {
        let source = r#"
package main

func helper() {
    return
}

func processData(x int, y string) {
    // body
}
"#;
        let lines: Vec<&str> = source.lines().collect();
        let patterns = vec!["func processData(".to_string()];

        let loc = find_function_definition(&lines, &patterns).expect("Should find function");
        assert!(loc.definition.contains("func processData"));
        assert!(loc.definition.contains("x int"));
    }

    #[test]
    fn test_find_function_definition_not_found() {
        let source = "func other() {}";
        let lines: Vec<&str> = source.lines().collect();
        let patterns = vec!["func notFound(".to_string()];

        assert!(find_function_definition(&lines, &patterns).is_none());
    }

    #[test]
    fn test_extract_body_with_braces() {
        let source = r#"func example() {
    line1
    line2
    if true {
        nested
    }
    line3
}
next_func"#;
        let lines: Vec<&str> = source.lines().collect();
        let body = extract_body_with_braces(&lines, 0, 0);

        assert!(body.contains("line1"));
        assert!(body.contains("line2"));
        assert!(body.contains("nested"));
        assert!(body.contains("line3"));
        assert!(!body.contains("next_func"));
    }

    #[test]
    fn test_extract_body_with_indent() {
        let source = r#"def example():
    line1
    line2
    if True:
        nested
    line3

def next_func():
    pass"#;
        let lines: Vec<&str> = source.lines().collect();
        let body = extract_body_with_indent(&lines, 0, 0);

        assert!(body.contains("line1"));
        assert!(body.contains("line2"));
        assert!(body.contains("nested"));
        assert!(body.contains("line3"));
        assert!(!body.contains("next_func"));
    }
}
