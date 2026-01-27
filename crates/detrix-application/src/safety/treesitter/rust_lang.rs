//! Rust AST analysis using tree-sitter
//!
//! Parses Rust expressions and detects:
//! - Assignments (prohibited in expressions)
//! - Function calls (for whitelist/blacklist checking)
//! - Variable/field accesses
//! - Unsafe blocks (flagged)
//! - Macro invocations

use super::cache::{global_cache, TreeSitterCache};
use super::{node_kinds::rust as nodes, node_text, traverse_tree, TreeSitterResult};
use detrix_core::SafetyLevel;
use std::collections::HashSet;
use tree_sitter::Parser;

/// Analyze a Rust expression for safety (with caching)
///
/// # Arguments
/// * `expression` - The Rust expression to analyze
/// * `safety_level` - The safety level (Strict requires whitelist)
/// * `allowed_functions` - Set of allowed function names for Strict mode
///
/// # Returns
/// A `TreeSitterResult` containing analysis results
pub fn analyze_rust(
    expression: &str,
    safety_level: SafetyLevel,
    allowed_functions: &HashSet<String>,
) -> TreeSitterResult {
    let cache = global_cache();
    let key = TreeSitterCache::make_key(expression, "rust", safety_level, allowed_functions);

    cache.get_or_insert(&key, || {
        analyze_rust_uncached(expression, safety_level, allowed_functions)
    })
}

/// Analyze a Rust expression without caching (internal)
fn analyze_rust_uncached(
    expression: &str,
    safety_level: SafetyLevel,
    allowed_functions: &HashSet<String>,
) -> TreeSitterResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("Failed to load Rust grammar");

    // Rust expressions need to be wrapped in a valid Rust context for parsing
    // We wrap in a minimal function body
    let wrapped = format!("fn main() {{ let _ = {}; }}", expression);

    let tree = match parser.parse(&wrapped, None) {
        Some(tree) => tree,
        None => {
            return TreeSitterResult::unsafe_with_violations(vec![
                "Failed to parse Rust expression".to_string(),
            ]);
        }
    };

    let root = tree.root_node();
    let source = wrapped.as_bytes();

    // Check for syntax errors
    if root.has_error() {
        return TreeSitterResult::unsafe_with_violations(vec![
            "Syntax error in Rust expression".to_string()
        ]);
    }

    let mut result = TreeSitterResult::safe();

    // Traverse the AST and analyze nodes
    traverse_tree(root, source, &mut |node, src| {
        analyze_rust_node(node, src, safety_level, allowed_functions, &mut result);
    });

    // In strict mode, check that all function calls are whitelisted
    if safety_level == SafetyLevel::Strict {
        super::check_strict_mode_violations(
            &mut result,
            allowed_functions,
            super::extract_rust_base_name,
        );
    }

    result
}

/// Analyze a single Rust AST node
fn analyze_rust_node(
    node: tree_sitter::Node,
    source: &[u8],
    _safety_level: SafetyLevel,
    _allowed_functions: &HashSet<String>,
    result: &mut TreeSitterResult,
) {
    match node.kind() {
        // Detect assignments (prohibited in expressions)
        nodes::ASSIGNMENT_EXPRESSION => {
            result.add_violation(format!(
                "Assignment detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect compound assignments (+=, -=, etc.)
        nodes::COMPOUND_ASSIGNMENT_EXPR => {
            result.add_violation(format!(
                "Compound assignment detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect unsafe blocks (flagged but not necessarily prohibited)
        nodes::UNSAFE_BLOCK => {
            result.has_unsafe = true;
            result.add_violation(format!(
                "Unsafe block detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect function calls
        nodes::CALL_EXPRESSION => {
            if let Some(func_node) = node.child_by_field_name("function") {
                let func_name = extract_rust_function_name(func_node, source);
                result.add_function_call(func_name);
            }
        }

        // Detect method calls (e.g., obj.method())
        nodes::METHOD_CALL_EXPRESSION => {
            // Get the method name
            if let Some(method_node) = node.child_by_field_name("name") {
                let method_name = node_text(&method_node, source);
                // Try to get the receiver to build full path
                if let Some(receiver) = node.child_by_field_name("receiver") {
                    let receiver_text = node_text(&receiver, source);
                    result.add_function_call(format!("{}.{}", receiver_text, method_name));
                } else {
                    result.add_function_call(method_name.to_string());
                }
            }
        }

        // Detect macro invocations
        nodes::MACRO_INVOCATION => {
            if let Some(macro_node) = node.child_by_field_name("macro") {
                let macro_name = node_text(&macro_node, source);
                result.add_function_call(format!("{}!", macro_name));
            }
        }

        // Detect variable accesses (identifiers)
        nodes::IDENTIFIER => {
            // Skip if this is part of a function call or path
            if let Some(parent) = node.parent() {
                let parent_kind = parent.kind();
                if parent_kind == nodes::CALL_EXPRESSION {
                    if let Some(func_node) = parent.child_by_field_name("function") {
                        if func_node.id() == node.id() {
                            return; // This is the function being called
                        }
                    }
                } else if parent_kind == nodes::SCOPED_IDENTIFIER
                    || parent_kind == nodes::FIELD_EXPRESSION
                    || parent_kind == nodes::METHOD_CALL_EXPRESSION
                {
                    return; // Will be handled by parent
                } else if parent_kind == nodes::FUNCTION_ITEM
                    || parent_kind == nodes::LET_DECLARATION
                {
                    // Skip function names and let bindings in wrapper
                    let text = node_text(&node, source);
                    if text == "main" || text == "_" {
                        return;
                    }
                }
            }
            let name = node_text(&node, source);
            if !name.is_empty() && name != "_" && name != "main" {
                result.add_variable_access(name.to_string());
            }
        }

        // Detect field expressions (e.g., obj.field)
        nodes::FIELD_EXPRESSION => {
            // Check if this is a standalone access (not a method call)
            if let Some(parent) = node.parent() {
                if parent.kind() == nodes::METHOD_CALL_EXPRESSION {
                    if let Some(receiver) = parent.child_by_field_name("receiver") {
                        if receiver.id() == node.id() {
                            return; // Will be handled as method call
                        }
                    }
                }
            }
            let text = node_text(&node, source);
            result.add_variable_access(text.to_string());
        }

        // Detect scoped identifiers (e.g., std::io::Result)
        nodes::SCOPED_IDENTIFIER => {
            // Check if this is a standalone access (not a function call)
            if let Some(parent) = node.parent() {
                if parent.kind() == nodes::CALL_EXPRESSION {
                    if let Some(func_node) = parent.child_by_field_name("function") {
                        if func_node.id() == node.id() {
                            return; // Will be handled as function call
                        }
                    }
                }
            }
            let text = node_text(&node, source);
            result.add_variable_access(text.to_string());
        }

        _ => {}
    }
}

/// Extract the full function name from a call expression's function node
fn extract_rust_function_name(node: tree_sitter::Node, source: &[u8]) -> String {
    match node.kind() {
        nodes::IDENTIFIER | nodes::SCOPED_IDENTIFIER | nodes::FIELD_EXPRESSION => {
            node_text(&node, source).to_string()
        }
        _ => node_text(&node, source).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed_set() -> HashSet<String> {
        [
            "len",
            "is_empty",
            "to_string",
            "clone",
            "iter",
            "map",
            "filter",
            "collect",
            "unwrap",
            "expect",
            "ok",
            "err",
            "Some",
            "None",
            "Ok",
            "Err",
            "Vec",
            "String",
            "format",
            "println",
            "print",
            "dbg",
            "assert",
            "assert_eq",
            "debug_assert",
            "sum",
            "product",
            "fold",
            "reduce",
            "take",
            "skip",
            "enumerate",
            "zip",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn test_simple_variable() {
        let result = analyze_rust("x", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.variable_accesses.contains("x"));
    }

    #[test]
    fn test_field_access() {
        let result = analyze_rust("user.name", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.variable_accesses.contains("user.name"));
    }

    #[test]
    fn test_method_call_allowed() {
        let result = analyze_rust("items.len()", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("items.len"));
    }

    #[test]
    fn test_function_call_not_allowed_strict() {
        let result = analyze_rust("custom_func(x)", SafetyLevel::Strict, &allowed_set());
        assert!(!result.is_safe);
        assert!(result.function_calls.contains("custom_func"));
        assert!(result
            .violations
            .iter()
            .any(|v| v.contains("Non-whitelisted")));
    }

    #[test]
    fn test_function_call_allowed_trusted() {
        let result = analyze_rust("custom_func(x)", SafetyLevel::Trusted, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("custom_func"));
    }

    #[test]
    fn test_scoped_function_call() {
        let result = analyze_rust(
            "std::mem::replace(&mut x, y)",
            SafetyLevel::Trusted,
            &allowed_set(),
        );
        assert!(result.is_safe);
        assert!(result.function_calls.contains("std::mem::replace"));
    }

    #[test]
    fn test_unsafe_block_detected() {
        let result = analyze_rust("unsafe { *ptr }", SafetyLevel::Trusted, &allowed_set());
        assert!(!result.is_safe);
        assert!(result.has_unsafe);
        assert!(result.violations.iter().any(|v| v.contains("Unsafe")));
    }

    #[test]
    fn test_assignment_detected() {
        let result = analyze_rust("x = 1", SafetyLevel::Trusted, &allowed_set());
        assert!(!result.is_safe);
        assert!(result.violations.iter().any(|v| v.contains("Assignment")));
    }

    #[test]
    fn test_chained_method_calls() {
        let result = analyze_rust(
            "items.iter().map(|x| x + 1).collect()",
            SafetyLevel::Strict,
            &allowed_set(),
        );
        // iter, map, collect are all allowed
        assert!(result.is_safe);
    }

    #[test]
    fn test_macro_invocation() {
        let result = analyze_rust("format!(\"hello {}\")", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("format!"));
    }

    #[test]
    fn test_tuple_access() {
        let result = analyze_rust("tuple.0", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_array_index() {
        let result = analyze_rust("items[0]", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_struct_instantiation() {
        let result = analyze_rust("Point { x: 1, y: 2 }", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_match_expression() {
        let result = analyze_rust(
            "match x { Some(v) => v, None => 0 }",
            SafetyLevel::Trusted,
            &allowed_set(),
        );
        assert!(result.is_safe);
    }

    #[test]
    fn test_if_expression() {
        let result = analyze_rust(
            "if condition { 1 } else { 0 }",
            SafetyLevel::Strict,
            &allowed_set(),
        );
        assert!(result.is_safe);
    }

    #[test]
    fn test_closure() {
        let result = analyze_rust("|x| x + 1", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_reference() {
        let result = analyze_rust("&x", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_dereference() {
        let result = analyze_rust("*ptr", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_turbofish_method_call() {
        let result = analyze_rust(
            "items.iter().map(|x| x.len()).sum::<usize>()",
            SafetyLevel::Strict,
            &allowed_set(),
        );
        // iter, map, len, sum should all be detected and allowed
        assert!(result.function_calls.contains("items.iter"));
        assert!(result.is_safe, "Expression with turbofish should be safe");
    }

    // ========================================================================
    // Table-driven tests for complex expressions
    // ========================================================================

    /// Test cases for complex expressions that should be SAFE
    const SAFE_COMPLEX_EXPRESSIONS: &[&str] = &[
        // Chained iterator methods
        "items.iter().collect()",
        "items.iter().filter(|x| *x > 0).collect()",
        "items.iter().map(|x| x * 2).collect()",
        "items.iter().filter(|x| *x > 0).map(|x| x * 2).collect()",
        "items.iter().filter(f).map(g).collect()",
        "items.iter().enumerate().collect()",
        "items.iter().zip(other.iter()).collect()",
        "items.iter().take(10).skip(5).collect()",
        "items.iter().fold(0, |acc, x| acc + x)",
        "items.iter().reduce(|a, b| a + b)",
        "items.iter().sum::<i32>()",
        "items.iter().product::<i32>()",
        // Nested method calls
        "items.first().map(|x| x.len())",
        "items.get(0).unwrap_or(&default)",
        "result.ok().map(|x| x.to_string())",
        "option.as_ref().map(|x| x.clone())",
        // Complex closures
        "items.iter().filter(|x| x.len() > 0).map(|x| x.to_string()).collect()",
        "items.iter().filter_map(|x| x.ok()).collect()",
        // Multiple chained calls
        "s.chars().filter(|c| c.is_alphanumeric()).collect::<String>()",
        "vec.into_iter().flatten().collect::<Vec<_>>()",
        // Struct field access chains
        "user.profile.settings.theme",
        "config.database.connection.timeout",
        // Method on field
        "user.name.to_string()",
        "items.len().to_string()",
        // Array/slice operations
        "items[0].clone()",
        "&items[1..5]",
        // Match expressions with method calls
        "match opt { Some(x) => x.len(), None => 0 }",
        // If expressions with method calls
        "if items.is_empty() { 0 } else { items.len() }",
        // Tuple access
        "pair.0.to_string()",
        "(a, b).0",
        // Reference and dereference
        "&items.len()",
        "*boxed_value",
        // Macro invocations with complex args
        "format!(\"{:?}\", items.iter().collect::<Vec<_>>())",
        "vec![items.len(), other.len()]",
        // Range expressions
        "0..items.len()",
        "(0..10).map(|x| x * 2).collect()",
        // Option/Result chains
        "result.map_err(|e| e.to_string())",
        "option.and_then(|x| x.parse().ok())",
        "result.unwrap_or_else(|| default.clone())",
    ];

    /// Test cases for expressions that should be UNSAFE
    const UNSAFE_COMPLEX_EXPRESSIONS: &[(&str, &str)] = &[
        // Unsafe blocks
        ("unsafe { *ptr }", "Unsafe"),
        ("unsafe { std::mem::transmute(x) }", "Unsafe"),
        // Assignment expressions
        ("x = items.len()", "assignment"),
        ("*ptr = value", "assignment"),
        // Compound assignments
        ("counter += items.len()", "assignment"),
        ("total -= item.price()", "assignment"),
    ];

    #[test]
    fn test_table_safe_complex_expressions() {
        // Use Trusted mode since we're testing complex method chains that aren't in basic whitelist
        let allowed = allowed_set();
        for expr in SAFE_COMPLEX_EXPRESSIONS {
            let result = analyze_rust(expr, SafetyLevel::Trusted, &allowed);
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
            let result = analyze_rust(expr, SafetyLevel::Trusted, &allowed);
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
        let test_cases: &[(&str, &[&str])] = &[
            (
                "items.iter().filter(|x| *x > 0).map(|y| y * 2).collect()",
                &["items.iter"],
            ),
            ("a.len() + b.len()", &["a.len", "b.len"]),
            ("format!(\"{}\", x)", &["format!"]),
            ("Vec::new()", &["Vec::new"]),
        ];

        for (expr, expected_calls) in test_cases {
            let result = analyze_rust(expr, SafetyLevel::Trusted, &allowed);
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
