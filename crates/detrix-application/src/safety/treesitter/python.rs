//! Python AST analysis using tree-sitter
//!
//! Parses Python expressions and detects:
//! - Assignments (prohibited in expressions)
//! - Function calls (for whitelist/blacklist checking)
//! - Variable/attribute accesses
//! - Import statements (prohibited)
//! - Delete statements (prohibited)

use super::cache::{global_cache, TreeSitterCache};
use super::{node_kinds::python as nodes, node_text, traverse_tree, TreeSitterResult};
use detrix_core::SafetyLevel;
use std::collections::HashSet;
use tree_sitter::Parser;

/// Analyze a Python expression for safety (with caching)
///
/// # Arguments
/// * `expression` - The Python expression to analyze
/// * `safety_level` - The safety level (Strict requires whitelist)
/// * `allowed_functions` - Set of allowed function names for Strict mode
///
/// # Returns
/// A `TreeSitterResult` containing analysis results
pub fn analyze_python(
    expression: &str,
    safety_level: SafetyLevel,
    allowed_functions: &HashSet<String>,
) -> TreeSitterResult {
    let cache = global_cache();
    let key = TreeSitterCache::make_key(expression, "python", safety_level, allowed_functions);

    cache.get_or_insert(&key, || {
        analyze_python_uncached(expression, safety_level, allowed_functions)
    })
}

/// Analyze a Python expression without caching (internal)
fn analyze_python_uncached(
    expression: &str,
    safety_level: SafetyLevel,
    allowed_functions: &HashSet<String>,
) -> TreeSitterResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("Failed to load Python grammar");

    // Parse the expression
    let tree = match parser.parse(expression, None) {
        Some(tree) => tree,
        None => {
            return TreeSitterResult::unsafe_with_violations(vec![
                "Failed to parse Python expression".to_string(),
            ]);
        }
    };

    let root = tree.root_node();
    let source = expression.as_bytes();

    // Check for syntax errors
    if root.has_error() {
        return TreeSitterResult::unsafe_with_violations(vec![
            "Syntax error in Python expression".to_string()
        ]);
    }

    let mut result = TreeSitterResult::safe();

    // Traverse the AST and analyze nodes
    traverse_tree(root, source, &mut |node, src| {
        analyze_python_node(node, src, safety_level, allowed_functions, &mut result);
    });

    // In strict mode, check that all function calls are whitelisted
    if safety_level == SafetyLevel::Strict {
        super::check_strict_mode_violations(
            &mut result,
            allowed_functions,
            super::extract_simple_base_name,
        );
    }

    result
}

/// Analyze a single Python AST node
fn analyze_python_node(
    node: tree_sitter::Node,
    source: &[u8],
    _safety_level: SafetyLevel,
    _allowed_functions: &HashSet<String>,
    result: &mut TreeSitterResult,
) {
    match node.kind() {
        // Detect assignments (prohibited in expressions)
        nodes::ASSIGNMENT | nodes::AUGMENTED_ASSIGNMENT | nodes::ANNOTATED_ASSIGNMENT => {
            result.add_violation(format!(
                "Assignment detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect delete statements (prohibited)
        nodes::DELETE_STATEMENT => {
            result.add_violation(format!(
                "Delete statement detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect import statements (prohibited)
        nodes::IMPORT_STATEMENT | nodes::IMPORT_FROM_STATEMENT => {
            result.add_violation(format!(
                "Import statement detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect global/nonlocal statements (prohibited)
        nodes::GLOBAL_STATEMENT | nodes::NONLOCAL_STATEMENT => {
            result.add_violation(format!(
                "Global/nonlocal statement detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect function calls
        nodes::CALL => {
            if let Some(func_node) = node.child_by_field_name("function") {
                let func_name = extract_function_name(func_node, source);
                result.add_function_call(func_name);
            }
        }

        // Detect variable accesses (identifiers not part of function calls)
        nodes::IDENTIFIER => {
            // Check if this identifier is not part of a call's function position
            if let Some(parent) = node.parent() {
                // Skip if this is the function name in a call expression
                if parent.kind() == nodes::CALL {
                    if let Some(func_node) = parent.child_by_field_name("function") {
                        if func_node.id() == node.id() {
                            return; // Skip - this is the function being called
                        }
                    }
                }
                // Skip if this is part of an attribute chain that's a function call
                if parent.kind() == nodes::ATTRIBUTE {
                    return; // Will be handled by attribute node
                }
            }
            let name = node_text(&node, source);
            if !name.is_empty() {
                result.add_variable_access(name.to_string());
            }
        }

        // Detect attribute accesses (e.g., obj.attr)
        nodes::ATTRIBUTE => {
            // Check if this is a standalone attribute access (not a function call)
            if let Some(parent) = node.parent() {
                if parent.kind() == nodes::CALL {
                    if let Some(func_node) = parent.child_by_field_name("function") {
                        if func_node.id() == node.id() {
                            return; // Skip - this will be handled as a function call
                        }
                    }
                }
            }
            let attr_text = node_text(&node, source);
            result.add_variable_access(attr_text.to_string());
        }

        _ => {}
    }
}

/// Extract the full function name from a call expression's function node
fn extract_function_name(node: tree_sitter::Node, source: &[u8]) -> String {
    match node.kind() {
        nodes::IDENTIFIER | nodes::ATTRIBUTE => node_text(&node, source).to_string(),
        _ => node_text(&node, source).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed_set() -> HashSet<String> {
        [
            "len",
            "str",
            "int",
            "float",
            "bool",
            "list",
            "dict",
            "set",
            "tuple",
            "sum",
            "min",
            "max",
            "abs",
            "round",
            "sorted",
            "reversed",
            "enumerate",
            "zip",
            "map",
            "filter",
            "range",
            "type",
            "isinstance",
            "hasattr",
            "getattr",
            "repr",
            "format",
            "items",
            "keys",
            "values",
            "get",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn test_simple_variable() {
        let result = analyze_python("x", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.variable_accesses.contains("x"));
    }

    #[test]
    fn test_attribute_access() {
        let result = analyze_python("user.name", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.variable_accesses.contains("user.name"));
    }

    #[test]
    fn test_function_call_allowed() {
        let result = analyze_python("len(items)", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("len"));
    }

    #[test]
    fn test_function_call_not_allowed_strict() {
        let result = analyze_python("custom_func(x)", SafetyLevel::Strict, &allowed_set());
        assert!(!result.is_safe);
        assert!(result.function_calls.contains("custom_func"));
        assert!(result
            .violations
            .iter()
            .any(|v| v.contains("Non-whitelisted")));
    }

    #[test]
    fn test_function_call_allowed_trusted() {
        let result = analyze_python("custom_func(x)", SafetyLevel::Trusted, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("custom_func"));
    }

    #[test]
    fn test_method_call() {
        let result = analyze_python("obj.method()", SafetyLevel::Trusted, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("obj.method"));
    }

    #[test]
    fn test_assignment_detected() {
        let result = analyze_python("x = 1", SafetyLevel::Trusted, &allowed_set());
        assert!(!result.is_safe);
        assert!(result.violations.iter().any(|v| v.contains("Assignment")));
    }

    #[test]
    fn test_augmented_assignment_detected() {
        let result = analyze_python("x += 1", SafetyLevel::Trusted, &allowed_set());
        assert!(!result.is_safe);
        assert!(result.violations.iter().any(|v| v.contains("Assignment")));
    }

    #[test]
    fn test_import_detected() {
        let result = analyze_python("import os", SafetyLevel::Trusted, &allowed_set());
        assert!(!result.is_safe);
        assert!(result.violations.iter().any(|v| v.contains("Import")));
    }

    #[test]
    fn test_delete_detected() {
        let result = analyze_python("del x", SafetyLevel::Trusted, &allowed_set());
        assert!(!result.is_safe);
        assert!(result.violations.iter().any(|v| v.contains("Delete")));
    }

    #[test]
    fn test_nested_function_calls() {
        let result = analyze_python("len(str(x))", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("len"));
        assert!(result.function_calls.contains("str"));
    }

    #[test]
    fn test_list_comprehension() {
        let result = analyze_python(
            "[len(x) for x in items]",
            SafetyLevel::Strict,
            &allowed_set(),
        );
        assert!(result.is_safe);
        assert!(result.function_calls.contains("len"));
    }

    #[test]
    fn test_dict_comprehension() {
        let result = analyze_python(
            "{k: str(v) for k, v in items.items()}",
            SafetyLevel::Strict,
            &allowed_set(),
        );
        assert!(result.is_safe);
        assert!(result.function_calls.contains("str"));
        assert!(result.function_calls.contains("items.items"));
    }

    #[test]
    fn test_chained_attribute() {
        let result = analyze_python("a.b.c.d", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_syntax_error() {
        let result = analyze_python("def (", SafetyLevel::Strict, &allowed_set());
        assert!(!result.is_safe);
        assert!(result
            .violations
            .iter()
            .any(|v| v.contains("Syntax error") || v.contains("parse")));
    }

    #[test]
    fn test_fstring() {
        let result = analyze_python("f'Hello {name}'", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_conditional_expression() {
        let result = analyze_python("x if condition else y", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    // ========================================================================
    // Table-driven tests for complex expressions
    // ========================================================================

    /// Test cases for complex expressions that should be SAFE
    const SAFE_COMPLEX_EXPRESSIONS: &[&str] = &[
        // List comprehensions
        "[x for x in items]",
        "[x * 2 for x in items]",
        "[x for x in items if x > 0]",
        "[len(x) for x in items]",
        "[str(x) for x in items if x > 0]",
        "[x.strip() for x in lines]",
        // Nested list comprehensions
        "[[y for y in x] for x in matrix]",
        "[item for sublist in nested for item in sublist]",
        // Dict comprehensions
        "{k: v for k, v in items.items()}",
        "{str(k): len(v) for k, v in data.items()}",
        "{x: x * 2 for x in range(10)}",
        // Set comprehensions
        "{x for x in items}",
        "{len(x) for x in items}",
        // Generator expressions
        "(x for x in items)",
        "(x * 2 for x in items if x > 0)",
        "sum(x for x in items)",
        "max(len(x) for x in items)",
        // Chained method calls
        "text.strip().lower()",
        "text.strip().split()",
        "items.copy().sort()",
        "data.get('key').strip()",
        // Nested function calls
        "len(str(x))",
        "int(float(x))",
        "sum(map(len, items))",
        "list(filter(bool, items))",
        "sorted(map(str, items))",
        "list(zip(a, b))",
        "dict(enumerate(items))",
        // Attribute chains
        "user.profile.settings",
        "obj.attr.subattr.value",
        "response.data.items",
        // Method calls on attributes
        "user.name.upper()",
        "config.path.exists()",
        "data.values.tolist()",
        // Subscript access
        "items[0]",
        "data['key']",
        "matrix[0][1]",
        "items[0].value",
        "data['key'].strip()",
        // Slice operations
        "items[1:5]",
        "text[::2]",
        "items[-1]",
        // Conditional expressions
        "x if condition else y",
        "len(x) if x else 0",
        "x.strip() if x else ''",
        // Lambda expressions (used as arguments)
        "sorted(items, key=lambda x: x.value)",
        "filter(lambda x: x > 0, items)",
        "map(lambda x: x * 2, items)",
        // F-strings
        "f'{name} is {age} years old'",
        "f'{user.name}: {user.email}'",
        "f'{len(items)} items'",
        // Complex arithmetic
        "sum(prices) / len(prices)",
        "max(a, b) - min(a, b)",
        "abs(x - y) / max(abs(x), abs(y))",
        // Boolean operations
        "any(x > 0 for x in items)",
        "all(isinstance(x, int) for x in items)",
        "len(items) > 0 and items[0] > 0",
        // Walrus operator in comprehension
        "[y for x in items if (y := len(x)) > 0]",
        // Tuple unpacking in comprehension
        "[a + b for a, b in pairs]",
        // Multiple function calls
        "min(len(a), len(b), len(c))",
        "sum([len(x) for x in items])",
    ];

    /// Test cases for expressions that should be UNSAFE
    const UNSAFE_COMPLEX_EXPRESSIONS: &[(&str, &str)] = &[
        // Simple assignments
        ("x = 1", "Assignment"),
        ("x = len(items)", "Assignment"),
        // Augmented assignments
        ("x += 1", "Assignment"),
        ("x -= len(items)", "Assignment"),
        ("x *= 2", "Assignment"),
        // Multiple assignments
        ("a = b = 1", "Assignment"),
        // Import statements
        ("import os", "Import"),
        ("from sys import path", "Import"),
        // Delete statements
        ("del x", "Delete"),
        ("del items[0]", "Delete"),
        // Global/nonlocal
        ("global x", "Global"),
        ("nonlocal y", "nonlocal"),
    ];

    #[test]
    fn test_table_safe_complex_expressions() {
        // Use Trusted mode since we're testing complex expressions with method calls
        let allowed = allowed_set();
        for expr in SAFE_COMPLEX_EXPRESSIONS {
            let result = analyze_python(expr, SafetyLevel::Trusted, &allowed);
            assert!(
                result.is_safe,
                "Expression '{}' should be SAFE, but got errors: {:?}",
                expr, result.violations
            );
        }
    }

    #[test]
    fn test_table_unsafe_complex_expressions() {
        let allowed = allowed_set();
        for (expr, expected_error) in UNSAFE_COMPLEX_EXPRESSIONS {
            let result = analyze_python(expr, SafetyLevel::Trusted, &allowed);
            assert!(!result.is_safe, "Expression '{}' should be UNSAFE", expr);
            assert!(
                result
                    .violations
                    .iter()
                    .any(|v| v.to_lowercase().contains(&expected_error.to_lowercase())),
                "Expression '{}' should contain '{}' in violations, got: {:?}",
                expr,
                expected_error,
                result.violations
            );
        }
    }

    /// Additional test: verify function call detection in complex expressions
    #[test]
    fn test_function_call_detection_in_complex_expressions() {
        let allowed = allowed_set();

        // Test that all expected functions are detected
        // Note: In `map(len, items)`, `len` is passed as a reference, not called directly
        let test_cases: &[(&str, &[&str])] = &[
            ("len(str(x))", &["len", "str"]),
            ("sum(map(len, items))", &["sum", "map"]), // len is reference, not call
            ("[len(x) for x in items]", &["len"]),
            (
                "user.profile.get_settings()",
                &["user.profile.get_settings"],
            ),
            ("sorted(items, key=lambda x: x.value)", &["sorted"]),
            ("list(map(str, items))", &["list", "map"]), // str is reference
        ];

        for (expr, expected_calls) in test_cases {
            let result = analyze_python(expr, SafetyLevel::Trusted, &allowed);
            for call in *expected_calls {
                assert!(
                    result.function_calls.iter().any(|c| c.contains(call)),
                    "Expression '{}' should detect call '{}', got: {:?}",
                    expr,
                    call,
                    result.function_calls
                );
            }
        }
    }
}
