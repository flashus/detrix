//! Tree-sitter based AST analysis for expression validation
//!
//! This module provides in-process AST parsing using tree-sitter
//!
//! # Benefits
//!
//! - No external dependencies (Go compiler, Python runtime)
//! - Faster parsing (~1ms vs ~10-50ms per call)
//! - No hash verification needed (code is compiled in)
//! - Consistent behavior across platforms
//! - **Caching** - Results are cached to avoid re-parsing identical expressions
//!
//! # Supported Languages
//!
//! - Python via `tree-sitter-python`
//! - Go via `tree-sitter-go`
//! - Rust via `tree-sitter-rust`

mod cache;
mod comment_stripper;
mod go;
mod node_kinds;
mod python;
mod rust_lang;
mod scope;

pub use cache::{
    global_cache, init_global_cache, init_global_cache_from_config, CacheStats, TreeSitterCache,
};
pub use comment_stripper::strip_comments;
pub use go::analyze_go;
pub use python::analyze_python;
pub use rust_lang::analyze_rust;
pub use scope::{analyze_scope, ScopeResult};

use std::collections::HashSet;

/// Result from tree-sitter AST analysis
///
/// Mirrors the previous `AstAnalyzerResult` structure for compatibility
/// with existing validators.
#[derive(Debug, Clone, Default)]
pub struct TreeSitterResult {
    /// Whether the expression is safe according to the analyzer
    pub is_safe: bool,
    /// List of safety violations detected
    pub violations: Vec<String>,
    /// Function calls found in the expression
    pub function_calls: HashSet<String>,
    /// Variable accesses found in the expression
    pub variable_accesses: HashSet<String>,
    /// Rust-specific: whether unsafe blocks were detected
    pub has_unsafe: bool,
}

impl TreeSitterResult {
    /// Create a new safe result with no violations
    pub fn safe() -> Self {
        Self {
            is_safe: true,
            ..Default::default()
        }
    }

    /// Create a new unsafe result with violations
    pub fn unsafe_with_violations(violations: Vec<String>) -> Self {
        Self {
            is_safe: false,
            violations,
            ..Default::default()
        }
    }

    /// Add a violation and mark as unsafe
    pub fn add_violation(&mut self, violation: String) {
        self.is_safe = false;
        self.violations.push(violation);
    }

    /// Add a function call
    pub fn add_function_call(&mut self, name: String) {
        self.function_calls.insert(name);
    }

    /// Add a variable access
    pub fn add_variable_access(&mut self, name: String) {
        self.variable_accesses.insert(name);
    }
}

/// Extract text from a node given the source code
pub(crate) fn node_text<'a>(node: &tree_sitter::Node, source: &'a [u8]) -> &'a str {
    let start = node.start_byte();
    let end = node.end_byte();
    std::str::from_utf8(&source[start..end]).unwrap_or("")
}

/// Recursively traverse all nodes in the tree
pub(crate) fn traverse_tree<F>(node: tree_sitter::Node, source: &[u8], callback: &mut F)
where
    F: FnMut(tree_sitter::Node, &[u8]),
{
    callback(node, source);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        traverse_tree(child, source, callback);
    }
}

/// Check for non-whitelisted function calls in strict mode
///
/// This is a shared implementation used by Python, Go, and Rust analyzers.
/// The `extract_base_name` function handles language-specific name extraction.
///
/// # Arguments
/// * `result` - The analysis result to check and potentially add violations to
/// * `allowed_functions` - Set of whitelisted function names
/// * `extract_base_name` - Closure that extracts the base function name for comparison
pub(crate) fn check_strict_mode_violations<F>(
    result: &mut TreeSitterResult,
    allowed_functions: &HashSet<String>,
    extract_base_name: F,
) where
    F: Fn(&str) -> Vec<&str>,
{
    let non_whitelisted: Vec<String> = result
        .function_calls
        .iter()
        .filter(|func| {
            let names = extract_base_name(func);
            // Check if any extracted name is in the allowed list
            !allowed_functions.contains(*func)
                && !names.iter().any(|name| allowed_functions.contains(*name))
        })
        .cloned()
        .collect();

    for func in non_whitelisted {
        result.add_violation(format!(
            "Non-whitelisted function call '{}' in strict mode",
            func
        ));
    }
}

/// Simple name extractor for Python and Go
///
/// Extracts the base name after the last `.` separator.
/// e.g., "math.sqrt" -> ["sqrt"]
pub(crate) fn extract_simple_base_name(func: &str) -> Vec<&str> {
    vec![func.split('.').next_back().unwrap_or(func)]
}

/// Complex name extractor for Rust
///
/// Handles:
/// - Turbofish: `sum::<usize>` -> `sum`
/// - Paths: `std::mem::replace` -> `replace`
/// - Methods: `obj.method` -> `method`
/// - Closures: `map(|x| x.len())` -> `map`
/// - Macros: `println!` -> `println`
pub(crate) fn extract_rust_base_name(func: &str) -> Vec<&str> {
    // Strip turbofish type parameters
    let without_turbofish = func.find("::<").map_or(func, |pos| &func[..pos]);

    // Get base name after :: (for paths)
    let after_path = without_turbofish
        .rsplit("::")
        .next()
        .unwrap_or(without_turbofish);

    // Get base name after . (for method calls)
    let base_name = after_path.split('.').next_back().unwrap_or(after_path);

    // Handle closures in the name
    let base_name = base_name
        .find('(')
        .map_or(base_name, |pos| &base_name[..pos]);

    // For macros, also return the version without '!'
    if let Some(macro_base) = base_name.strip_suffix('!') {
        vec![base_name, macro_base]
    } else {
        vec![base_name]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tree_sitter_result_safe() {
        let result = TreeSitterResult::safe();
        assert!(result.is_safe);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn test_tree_sitter_result_unsafe() {
        let result = TreeSitterResult::unsafe_with_violations(vec!["Dangerous call".to_string()]);
        assert!(!result.is_safe);
        assert_eq!(result.violations.len(), 1);
    }

    #[test]
    fn test_add_violation() {
        let mut result = TreeSitterResult::safe();
        result.add_violation("Test violation".to_string());
        assert!(!result.is_safe);
        assert!(result.violations.contains(&"Test violation".to_string()));
    }

    #[test]
    fn test_add_function_call() {
        let mut result = TreeSitterResult::safe();
        result.add_function_call("len".to_string());
        result.add_function_call("str".to_string());
        assert!(result.function_calls.contains("len"));
        assert!(result.function_calls.contains("str"));
    }

    #[test]
    fn test_extract_simple_base_name() {
        // Simple function
        assert_eq!(extract_simple_base_name("len"), vec!["len"]);

        // Method call
        assert_eq!(extract_simple_base_name("items.len"), vec!["len"]);

        // Chained methods
        assert_eq!(extract_simple_base_name("items.iter.map"), vec!["map"]);

        // Module-qualified function
        assert_eq!(extract_simple_base_name("math.sqrt"), vec!["sqrt"]);
    }

    #[test]
    fn test_extract_rust_base_name() {
        // Simple function
        assert_eq!(extract_rust_base_name("len"), vec!["len"]);

        // Method call
        assert_eq!(extract_rust_base_name("items.len"), vec!["len"]);

        // Path-qualified function
        assert_eq!(extract_rust_base_name("std::mem::replace"), vec!["replace"]);

        // Turbofish syntax
        assert_eq!(extract_rust_base_name("sum::<usize>"), vec!["sum"]);
        assert_eq!(
            extract_rust_base_name("iter.collect::<Vec<_>>"),
            vec!["collect"]
        );

        // Macro invocation
        assert_eq!(
            extract_rust_base_name("println!"),
            vec!["println!", "println"]
        );
        assert_eq!(extract_rust_base_name("format!"), vec!["format!", "format"]);

        // Complex case: path + turbofish
        assert_eq!(
            extract_rust_base_name("std::iter::Iterator::sum::<i32>"),
            vec!["sum"]
        );
    }

    #[test]
    fn test_check_strict_mode_violations() {
        let allowed: HashSet<String> = ["len", "clone", "to_string"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Test with allowed functions only
        let mut result = TreeSitterResult::safe();
        result.add_function_call("len".to_string());
        result.add_function_call("clone".to_string());
        check_strict_mode_violations(&mut result, &allowed, extract_simple_base_name);
        assert!(result.is_safe);
        assert!(result.violations.is_empty());

        // Test with non-whitelisted function
        let mut result = TreeSitterResult::safe();
        result.add_function_call("custom_func".to_string());
        check_strict_mode_violations(&mut result, &allowed, extract_simple_base_name);
        assert!(!result.is_safe);
        assert!(result
            .violations
            .iter()
            .any(|v| v.contains("Non-whitelisted")));

        // Test that method base name is checked
        let mut result = TreeSitterResult::safe();
        result.add_function_call("items.len".to_string()); // "len" is allowed
        check_strict_mode_violations(&mut result, &allowed, extract_simple_base_name);
        assert!(result.is_safe);
    }
}
