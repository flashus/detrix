//! Go AST analysis using tree-sitter
//!
//! Parses Go expressions and detects:
//! - Assignments (prohibited in expressions)
//! - Function calls (for whitelist/blacklist checking)
//! - Variable/selector accesses
//! - Goroutines (go statement - prohibited)
//! - Defer statements (prohibited)
//! - Channel operations (prohibited)

use super::cache::{global_cache, TreeSitterCache};
use super::{node_kinds::go as nodes, node_text, traverse_tree, TreeSitterResult};
use detrix_core::SafetyLevel;
use std::collections::HashSet;
use tree_sitter::Parser;

/// Analyze a Go expression for safety (with caching)
///
/// # Arguments
/// * `expression` - The Go expression to analyze
/// * `safety_level` - The safety level (Strict requires whitelist)
/// * `allowed_functions` - Set of allowed function names for Strict mode
///
/// # Returns
/// A `TreeSitterResult` containing analysis results
pub fn analyze_go(
    expression: &str,
    safety_level: SafetyLevel,
    allowed_functions: &HashSet<String>,
) -> TreeSitterResult {
    let cache = global_cache();
    let key = TreeSitterCache::make_key(expression, "go", safety_level, allowed_functions);

    cache.get_or_insert(&key, || {
        analyze_go_uncached(expression, safety_level, allowed_functions)
    })
}

/// Analyze a Go expression without caching (internal)
fn analyze_go_uncached(
    expression: &str,
    safety_level: SafetyLevel,
    allowed_functions: &HashSet<String>,
) -> TreeSitterResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("Failed to load Go grammar");

    // Go expressions need to be wrapped in a valid Go context for parsing
    // We wrap in a minimal function body
    let wrapped = format!("package main\nfunc main() {{ _ = {} }}", expression);

    let tree = match parser.parse(&wrapped, None) {
        Some(tree) => tree,
        None => {
            return TreeSitterResult::unsafe_with_violations(vec![
                "Failed to parse Go expression".to_string()
            ]);
        }
    };

    let root = tree.root_node();
    let source = wrapped.as_bytes();

    // Check for syntax errors
    if root.has_error() {
        return TreeSitterResult::unsafe_with_violations(vec![
            "Syntax error in Go expression".to_string()
        ]);
    }

    let mut result = TreeSitterResult::safe();

    // Traverse the AST and analyze nodes
    traverse_tree(root, source, &mut |node, src| {
        analyze_go_node(node, src, safety_level, allowed_functions, &mut result);
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

/// Analyze a single Go AST node
fn analyze_go_node(
    node: tree_sitter::Node,
    source: &[u8],
    _safety_level: SafetyLevel,
    _allowed_functions: &HashSet<String>,
    result: &mut TreeSitterResult,
) {
    match node.kind() {
        // Detect assignments (prohibited in expressions)
        nodes::ASSIGNMENT_STATEMENT => {
            // Skip the wrapper assignment we added (_ = expr)
            let text = node_text(&node, source);
            if !text.starts_with("_ =") {
                result.add_violation(format!("Assignment detected: '{}'", text));
            }
        }

        // Detect short variable declarations (:=)
        nodes::SHORT_VAR_DECLARATION => {
            result.add_violation(format!(
                "Short variable declaration detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect increment/decrement (side effects)
        nodes::INC_STATEMENT | nodes::DEC_STATEMENT => {
            result.add_violation(format!(
                "Increment/decrement detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect goroutines (prohibited)
        nodes::GO_STATEMENT => {
            result.add_violation(format!(
                "Goroutine detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect defer statements (prohibited)
        nodes::DEFER_STATEMENT => {
            result.add_violation(format!(
                "Defer statement detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect channel send operations (prohibited)
        nodes::SEND_STATEMENT => {
            result.add_violation(format!(
                "Channel send detected: '{}'",
                node_text(&node, source)
            ));
        }

        // Detect function calls
        nodes::CALL_EXPRESSION => {
            if let Some(func_node) = node.child_by_field_name("function") {
                let func_name = extract_go_function_name(func_node, source);
                result.add_function_call(func_name);
            }
        }

        // Detect variable accesses (identifiers)
        nodes::IDENTIFIER => {
            // Skip if this is part of a function call or selector
            if let Some(parent) = node.parent() {
                let parent_kind = parent.kind();
                if parent_kind == nodes::CALL_EXPRESSION {
                    if let Some(func_node) = parent.child_by_field_name("function") {
                        if func_node.id() == node.id() {
                            return; // This is the function being called
                        }
                    }
                } else if parent_kind == nodes::SELECTOR_EXPRESSION {
                    return; // Will be handled by selector_expression
                } else if parent_kind == nodes::PACKAGE_CLAUSE
                    || parent_kind == nodes::FUNCTION_DECLARATION
                {
                    return; // Skip package/function names in wrapper
                }
            }
            let name = node_text(&node, source);
            // Skip the wrapper placeholder
            if name != "_" && name != "main" {
                result.add_variable_access(name.to_string());
            }
        }

        // Detect selector expressions (e.g., obj.field)
        nodes::SELECTOR_EXPRESSION => {
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
fn extract_go_function_name(node: tree_sitter::Node, source: &[u8]) -> String {
    match node.kind() {
        nodes::IDENTIFIER | nodes::SELECTOR_EXPRESSION => node_text(&node, source).to_string(),
        _ => node_text(&node, source).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed_set() -> HashSet<String> {
        [
            "len", "cap", "make", "new", "append", "copy", "delete", "close", "panic", "recover",
            "print", "println", "complex", "real", "imag", "string", "int", "float64",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn test_simple_variable() {
        let result = analyze_go("x", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.variable_accesses.contains("x"));
    }

    #[test]
    fn test_selector_expression() {
        let result = analyze_go("user.Name", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.variable_accesses.contains("user.Name"));
    }

    #[test]
    fn test_function_call_allowed() {
        let result = analyze_go("len(items)", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("len"));
    }

    #[test]
    fn test_function_call_not_allowed_strict() {
        let result = analyze_go("customFunc(x)", SafetyLevel::Strict, &allowed_set());
        assert!(!result.is_safe);
        assert!(result.function_calls.contains("customFunc"));
        assert!(result
            .violations
            .iter()
            .any(|v| v.contains("Non-whitelisted")));
    }

    #[test]
    fn test_function_call_allowed_trusted() {
        let result = analyze_go("customFunc(x)", SafetyLevel::Trusted, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("customFunc"));
    }

    #[test]
    fn test_method_call() {
        let result = analyze_go("obj.Method()", SafetyLevel::Trusted, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("obj.Method"));
    }

    #[test]
    fn test_package_function_call() {
        let result = analyze_go(
            "fmt.Sprintf(\"%d\", x)",
            SafetyLevel::Trusted,
            &allowed_set(),
        );
        assert!(result.is_safe);
        assert!(result.function_calls.contains("fmt.Sprintf"));
    }

    #[test]
    fn test_goroutine_detected() {
        // This would need to be a statement, not an expression
        // Wrapping differently for this test
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();

        let code = "package main\nfunc main() { go func(){}() }";
        let tree = parser.parse(code, None).unwrap();
        let source = code.as_bytes();

        let mut result = TreeSitterResult::safe();
        traverse_tree(tree.root_node(), source, &mut |node, src| {
            analyze_go_node(node, src, SafetyLevel::Trusted, &allowed_set(), &mut result);
        });

        assert!(!result.is_safe);
        assert!(result.violations.iter().any(|v| v.contains("Goroutine")));
    }

    #[test]
    fn test_defer_detected() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();

        let code = "package main\nfunc main() { defer cleanup() }";
        let tree = parser.parse(code, None).unwrap();
        let source = code.as_bytes();

        let mut result = TreeSitterResult::safe();
        traverse_tree(tree.root_node(), source, &mut |node, src| {
            analyze_go_node(node, src, SafetyLevel::Trusted, &allowed_set(), &mut result);
        });

        assert!(!result.is_safe);
        assert!(result.violations.iter().any(|v| v.contains("Defer")));
    }

    #[test]
    fn test_nested_function_calls() {
        let result = analyze_go("len(append(items, x))", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
        assert!(result.function_calls.contains("len"));
        assert!(result.function_calls.contains("append"));
    }

    #[test]
    fn test_struct_field_access() {
        let result = analyze_go(
            "user.Profile.Settings.Theme",
            SafetyLevel::Strict,
            &allowed_set(),
        );
        assert!(result.is_safe);
    }

    #[test]
    fn test_array_index() {
        let result = analyze_go("items[0]", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_map_access() {
        let result = analyze_go("data[\"key\"]", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    #[test]
    fn test_arithmetic() {
        let result = analyze_go("a + b * c", SafetyLevel::Strict, &allowed_set());
        assert!(result.is_safe);
    }

    // =========================================================================
    // Table-driven tests for comprehensive expression coverage
    // =========================================================================

    /// Complex safe Go expressions that should all pass validation
    const SAFE_COMPLEX_EXPRESSIONS: &[&str] = &[
        // Simple literals and variables
        "x",
        "123",
        "3.14",
        "true",
        "false",
        "nil",
        r#""hello""#,
        "`raw string`",
        "'a'",
        // Arithmetic expressions
        "a + b",
        "a - b",
        "a * b",
        "a / b",
        "a % b",
        "a + b * c - d / e",
        "(a + b) * (c - d)",
        "-x",
        "+y",
        // Comparison expressions
        "a == b",
        "a != b",
        "a < b",
        "a <= b",
        "a > b",
        "a >= b",
        // Logical expressions
        "a && b",
        "a || b",
        "!a",
        "a && b || c",
        "!a && !b",
        "(a || b) && (c || d)",
        // Bitwise expressions
        "a & b",
        "a | b",
        "a ^ b",
        "a << 2",
        "a >> 3",
        "^a",
        "a &^ b",
        // Field access (selector expressions)
        "user.Name",
        "user.Profile.Email",
        "user.Profile.Settings.Theme",
        "config.Server.Port",
        "obj.field1.field2.field3.field4",
        // Index expressions
        "items[0]",
        "items[i]",
        "items[len(items)-1]",
        "matrix[i][j]",
        "data[0][1][2]",
        // Slice expressions
        "items[:]",
        "items[1:]",
        "items[:5]",
        "items[1:5]",
        "items[1:5:10]",
        "items[:len(items)-1]",
        // Map access
        r#"data["key"]"#,
        "data[key]",
        r#"nested["a"]["b"]"#,
        // Type assertions
        "x.(int)",
        "x.(string)",
        "x.(MyType)",
        "x.(interface{})",
        // Type conversions
        "int(x)",
        "float64(y)",
        "string(b)",
        "[]byte(s)",
        "int32(n)",
        "uint64(m)",
        // Built-in function calls
        "len(items)",
        "cap(slice)",
        "append(items, x)",
        "append(items, x, y, z)",
        "append(items, other...)",
        "copy(dst, src)",
        "delete(m, key)",
        "make([]int, 10)",
        "make([]int, 10, 20)",
        "make(map[string]int)",
        "make(chan int)",
        "make(chan int, 10)",
        "new(MyStruct)",
        "complex(1.0, 2.0)",
        "real(c)",
        "imag(c)",
        "panic(err)",
        "recover()",
        "print(x)",
        "println(x, y, z)",
        "close(ch)",
        // Nested built-in calls
        "len(append(items, x))",
        "cap(make([]int, len(items)))",
        "string(append([]byte(s), 'x'))",
        // Composite literals
        "[]int{1, 2, 3}",
        "[]string{\"a\", \"b\", \"c\"}",
        "[3]int{1, 2, 3}",
        "map[string]int{\"a\": 1, \"b\": 2}",
        "struct{X int}{X: 10}",
        "MyStruct{Field: value}",
        "Point{X: 1, Y: 2}",
        "&Point{X: 1, Y: 2}",
        "[]Point{{1, 2}, {3, 4}}",
        // Pointer operations (read-only)
        "*ptr",
        "&value",
        "&obj.Field",
        "**ptrptr",
        // Method calls (Trusted mode patterns)
        "obj.Method()",
        "obj.Method(arg)",
        "obj.Method(a, b, c)",
        "obj.Field.Method()",
        "list.Len()",
        "str.ToLower()",
        // Package function calls
        r#"fmt.Sprintf("%d", x)"#,
        "strings.Contains(s, substr)",
        "math.Sqrt(x)",
        "time.Now()",
        "sort.Ints(items)",
        "json.Marshal(obj)",
        // Chained method calls
        "builder.WithField(a).WithField(b).Build()",
        "query.Where(cond).OrderBy(field).Limit(10)",
        "str.TrimSpace().ToLower()",
        // Ternary-like expressions (conditional)
        "func() int { if x { return 1 } else { return 2 } }()",
        // Anonymous functions (as expressions, not statements)
        "func(x int) int { return x * 2 }",
        "func() {}",
        // Channel receive (expression, not send)
        "<-ch",
        // Note: "value, ok := <-ch" is a short variable declaration (unsafe)
        // Range expressions used in for (the range itself)
        // Complex nested expressions
        "items[0].Field.Method().Result",
        "pkg.Func(a.B.C, d.E.F)",
        "len(items[start:end])",
        r#"fmt.Sprintf("%v %v", a.Field, b.Method())"#,
        "append(make([]int, 0, len(items)), items...)",
        // Boolean combinations
        "a > 0 && b < 100 || c == 0",
        "len(items) > 0 && items[0] != nil",
        "(a || b) && (c || d) && (e || f)",
        // Arithmetic with function calls
        "len(a) + len(b)",
        "math.Max(a, b) * 2",
        "int(math.Floor(x)) + 1",
        // Struct field arithmetic
        "point.X + point.Y",
        "rect.Width * rect.Height",
        "(a.X - b.X) * (a.X - b.X) + (a.Y - b.Y) * (a.Y - b.Y)",
    ];

    /// Test unsafe statements by parsing them directly as Go code (not as expressions)
    /// Note: Go assignments are statements, not expressions. They need direct parsing
    /// rather than through the expression analyzer which wraps in `_ = expr`.
    fn test_go_statement(statement: &str, expected_violation: &str) {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();

        let code = format!("package main\nfunc main() {{ {} }}", statement);
        let tree = parser.parse(&code, None).unwrap();
        let source = code.as_bytes();

        let mut result = TreeSitterResult::safe();
        traverse_tree(tree.root_node(), source, &mut |node, src| {
            analyze_go_node(node, src, SafetyLevel::Trusted, &allowed_set(), &mut result);
        });

        assert!(
            !result.is_safe,
            "Statement '{}' should be unsafe",
            statement
        );
        assert!(
            result.violations.iter().any(|v| v
                .to_lowercase()
                .contains(&expected_violation.to_lowercase())),
            "Statement '{}' should contain '{}' in violations, got: {:?}",
            statement,
            expected_violation,
            result.violations
        );
    }

    /// Extended allowed function set for Trusted mode testing
    fn extended_allowed_set() -> HashSet<String> {
        let mut set = allowed_set();
        // Add common standard library functions for comprehensive testing
        let extras = [
            "fmt.Sprintf",
            "fmt.Printf",
            "fmt.Println",
            "strings.Contains",
            "strings.Split",
            "strings.Join",
            "strings.TrimSpace",
            "strings.ToLower",
            "strings.ToUpper",
            "math.Sqrt",
            "math.Max",
            "math.Min",
            "math.Floor",
            "math.Ceil",
            "time.Now",
            "sort.Ints",
            "sort.Strings",
            "json.Marshal",
            "json.Unmarshal",
            "Sprintf",
        ];
        for extra in extras {
            set.insert(extra.to_string());
        }
        set
    }

    #[test]
    fn test_table_safe_complex_expressions_trusted() {
        let allowed = extended_allowed_set();

        for expr in SAFE_COMPLEX_EXPRESSIONS {
            let result = analyze_go(expr, SafetyLevel::Trusted, &allowed);
            assert!(
                result.is_safe,
                "Expression '{}' should be safe in Trusted mode, but got violations: {:?}",
                expr, result.violations
            );
        }
    }

    #[test]
    fn test_table_safe_complex_expressions_strict_builtins_only() {
        // Test expressions that only use built-in functions (should pass Strict mode)
        let strict_safe: &[&str] = &[
            "x",
            "123",
            "a + b",
            "a && b",
            "user.Name",
            "items[0]",
            "items[1:5]",
            r#"data["key"]"#,
            "len(items)",
            "cap(slice)",
            "append(items, x)",
            "len(append(items, x))",
            "make([]int, 10)",
            "new(int)",
            "*ptr",
            "&value",
        ];

        let allowed = allowed_set();
        for expr in strict_safe {
            let result = analyze_go(expr, SafetyLevel::Strict, &allowed);
            assert!(
                result.is_safe,
                "Expression '{}' should be safe in Strict mode, but got violations: {:?}",
                expr, result.violations
            );
        }
    }

    #[test]
    fn test_table_unsafe_statements() {
        // Go assignments are statements, not expressions
        // They need to be tested via direct code parsing
        let statements: &[(&str, &str)] = &[
            // Assignments
            ("x = 5", "Assignment"),
            ("x = y + z", "Assignment"),
            ("obj.Field = value", "Assignment"),
            ("items[0] = x", "Assignment"),
            // Short variable declarations
            ("x := 5", "Short variable declaration"),
            ("a, b := 1, 2", "Short variable declaration"),
            ("result := compute()", "Short variable declaration"),
            // Increment/decrement
            ("x++", "Increment"),
            ("y--", "decrement"),
            ("counter++", "Increment"),
            // Compound assignments
            ("x += 5", "Assignment"),
            ("x -= 3", "Assignment"),
            ("x *= 2", "Assignment"),
            ("x /= 4", "Assignment"),
            ("x %= 3", "Assignment"),
            ("x &= mask", "Assignment"),
            ("x |= flag", "Assignment"),
            ("x ^= bits", "Assignment"),
            ("x <<= 2", "Assignment"),
            ("x >>= 3", "Assignment"),
        ];

        for (statement, expected_violation) in statements {
            test_go_statement(statement, expected_violation);
        }
    }

    #[test]
    fn test_function_call_detection_in_complex_expressions() {
        let allowed = extended_allowed_set();

        // Test that complex expressions correctly detect all function calls
        let test_cases: &[(&str, &[&str])] = &[
            ("len(items)", &["len"]),
            ("append(items, x)", &["append"]),
            ("len(append(items, x))", &["len", "append"]),
            ("cap(make([]int, 10))", &["cap", "make"]),
            (r#"fmt.Sprintf("%d", x)"#, &["fmt.Sprintf"]),
            ("strings.Contains(s, sub)", &["strings.Contains"]),
            ("obj.Method()", &["obj.Method"]),
            (
                "obj.Method1().Method2()",
                &["obj.Method1", "obj.Method1().Method2"],
            ),
            (
                "pkg.Func(a.Method(), b.Method())",
                &["pkg.Func", "a.Method", "b.Method"],
            ),
        ];

        for (expr, expected_calls) in test_cases {
            let result = analyze_go(expr, SafetyLevel::Trusted, &allowed);
            for expected_call in *expected_calls {
                assert!(
                    result.function_calls.contains(*expected_call),
                    "Expression '{}' should detect function call '{}', but only found: {:?}",
                    expr,
                    expected_call,
                    result.function_calls
                );
            }
        }
    }

    #[test]
    fn test_variable_access_detection_in_complex_expressions() {
        let allowed = allowed_set();

        // Test that complex expressions correctly detect variable accesses
        let test_cases: &[(&str, &[&str])] = &[
            ("x", &["x"]),
            ("a + b", &["a", "b"]),
            ("user.Name", &["user.Name"]),
            ("items[i]", &["items", "i"]),
            ("a && b || c", &["a", "b", "c"]),
        ];

        for (expr, expected_vars) in test_cases {
            let result = analyze_go(expr, SafetyLevel::Strict, &allowed);
            for expected_var in *expected_vars {
                assert!(
                    result.variable_accesses.contains(*expected_var),
                    "Expression '{}' should detect variable access '{}', but only found: {:?}",
                    expr,
                    expected_var,
                    result.variable_accesses
                );
            }
        }
    }

    #[test]
    fn test_method_chaining_safety() {
        let allowed = extended_allowed_set();

        let chained_expressions = [
            "builder.Step1().Step2().Step3()",
            "query.Select().From().Where().OrderBy()",
            "list.Filter(f).Map(g).Reduce(r)",
            "str.Trim().Lower().Replace(a, b)",
        ];

        for expr in chained_expressions {
            let result = analyze_go(expr, SafetyLevel::Trusted, &allowed);
            assert!(
                result.is_safe,
                "Chained method call '{}' should be safe, but got violations: {:?}",
                expr, result.violations
            );
        }
    }

    #[test]
    fn test_composite_literal_safety() {
        let allowed = allowed_set();

        let composite_literals = [
            "[]int{1, 2, 3}",
            r#"[]string{"a", "b"}"#,
            "[3]int{1, 2, 3}",
            r#"map[string]int{"x": 1}"#,
            "Point{X: 1, Y: 2}",
            "&Config{Port: 8080}",
            "[]Point{{1, 2}, {3, 4}}",
        ];

        for expr in composite_literals {
            let result = analyze_go(expr, SafetyLevel::Trusted, &allowed);
            assert!(
                result.is_safe,
                "Composite literal '{}' should be safe, but got violations: {:?}",
                expr, result.violations
            );
        }
    }

    #[test]
    fn test_slice_expressions_safety() {
        let allowed = allowed_set();

        let slice_expressions = [
            "items[:]",
            "items[1:]",
            "items[:5]",
            "items[1:5]",
            "items[1:5:10]",
            "items[:len(items)-1]",
            "items[start:end]",
            "data[i:j:k]",
        ];

        for expr in slice_expressions {
            let result = analyze_go(expr, SafetyLevel::Strict, &allowed);
            assert!(
                result.is_safe,
                "Slice expression '{}' should be safe, but got violations: {:?}",
                expr, result.violations
            );
        }
    }

    #[test]
    fn test_type_operations_safety() {
        let allowed = allowed_set();

        let type_operations = [
            // Type assertions
            "x.(int)",
            "x.(string)",
            "x.(MyInterface)",
            // Type conversions
            "int(x)",
            "float64(y)",
            "string(bytes)",
            "[]byte(str)",
            "uint32(n)",
        ];

        for expr in type_operations {
            let result = analyze_go(expr, SafetyLevel::Trusted, &allowed);
            assert!(
                result.is_safe,
                "Type operation '{}' should be safe, but got violations: {:?}",
                expr, result.violations
            );
        }
    }
}
