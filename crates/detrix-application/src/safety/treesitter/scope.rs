//! Tree-sitter based scope analysis
//!
//! Provides scope analysis to find variables available at a specific line in a file.
//! Used by the file inspection service for scope validation.

use super::node_text;
use detrix_core::SourceLanguage;
use std::collections::HashSet;
use tree_sitter::Parser;

/// Result of scope analysis at a specific line
#[derive(Debug, Clone, Default)]
pub struct ScopeResult {
    /// Variables available at the target line
    pub available_variables: Vec<String>,
    /// The function/method name containing the target line (if any)
    pub containing_scope: Option<String>,
}

/// Analyze scope at a specific line in a file
///
/// Returns all variables (parameters, locals) that are in scope at the given line.
pub fn analyze_scope(source: &str, target_line: u32, language: SourceLanguage) -> ScopeResult {
    match language {
        SourceLanguage::Python => analyze_python_scope(source, target_line),
        SourceLanguage::Go => analyze_go_scope(source, target_line),
        SourceLanguage::Rust => analyze_rust_scope(source, target_line),
        _ => ScopeResult::default(),
    }
}

/// Analyze Python scope
fn analyze_python_scope(source: &str, target_line: u32) -> ScopeResult {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .is_err()
    {
        return ScopeResult::default();
    }

    let tree = match parser.parse(source, None) {
        Some(tree) => tree,
        None => return ScopeResult::default(),
    };

    let root = tree.root_node();
    let source_bytes = source.as_bytes();
    let target_line_0 = target_line.saturating_sub(1) as usize; // Convert to 0-based

    let mut result = ScopeResult::default();
    let mut variables = HashSet::new();

    // Find the innermost function containing the target line
    find_python_scope(
        root,
        source_bytes,
        target_line_0,
        &mut variables,
        &mut result.containing_scope,
    );

    result.available_variables = variables.into_iter().collect();
    result.available_variables.sort();
    result
}

/// Recursively find Python scope and variables
fn find_python_scope(
    node: tree_sitter::Node,
    source: &[u8],
    target_line: usize,
    variables: &mut HashSet<String>,
    containing_scope: &mut Option<String>,
) {
    let node_start = node.start_position().row;
    let node_end = node.end_position().row;

    // Check if this node contains the target line
    if target_line < node_start || target_line > node_end {
        return;
    }

    match node.kind() {
        "function_definition" | "async_function_definition" => {
            // Get function name
            if let Some(name_node) = node.child_by_field_name("name") {
                *containing_scope = Some(node_text(&name_node, source).to_string());
            }

            // Get parameters
            if let Some(params) = node.child_by_field_name("parameters") {
                extract_python_parameters(params, source, variables);
            }

            // Get local variables defined before target line
            if let Some(body) = node.child_by_field_name("body") {
                extract_python_locals(body, source, target_line, variables);
            }

            // Check nested scopes
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                find_python_scope(child, source, target_line, variables, containing_scope);
            }
        }
        "class_definition" => {
            // Get class name
            if let Some(name_node) = node.child_by_field_name("name") {
                let class_name = node_text(&name_node, source).to_string();
                // Add 'self' for methods
                variables.insert("self".to_string());
                if containing_scope.is_none() {
                    *containing_scope = Some(class_name);
                }
            }

            // Check nested scopes
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                find_python_scope(child, source, target_line, variables, containing_scope);
            }
        }
        "for_statement" => {
            // For loop variable
            if let Some(left) = node.child_by_field_name("left") {
                if left.start_position().row <= target_line {
                    extract_python_pattern(left, source, variables);
                }
            }
            // Check body
            if let Some(body) = node.child_by_field_name("body") {
                find_python_scope(body, source, target_line, variables, containing_scope);
            }
        }
        "with_statement" => {
            // With statement variables
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "with_clause" {
                    let mut clause_cursor = child.walk();
                    for item in child.children(&mut clause_cursor) {
                        if item.kind() == "with_item" {
                            if let Some(alias) = item.child_by_field_name("alias") {
                                if alias.start_position().row <= target_line {
                                    extract_python_pattern(alias, source, variables);
                                }
                            }
                        }
                    }
                }
            }
        }
        "except_clause" => {
            // Exception variable
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" && child.start_position().row <= target_line {
                    let name = node_text(&child, source);
                    if !name.is_empty() {
                        variables.insert(name.to_string());
                    }
                }
            }
        }
        "module" | "block" | "if_statement" | "elif_clause" | "else_clause" | "try_statement"
        | "while_statement" | "match_statement" => {
            // Recurse into these nodes
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                find_python_scope(child, source, target_line, variables, containing_scope);
            }
        }
        _ => {}
    }
}

/// Extract Python function parameters
fn extract_python_parameters(
    params: tree_sitter::Node,
    source: &[u8],
    variables: &mut HashSet<String>,
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                let name = node_text(&child, source);
                if !name.is_empty() {
                    variables.insert(name.to_string());
                }
            }
            "typed_parameter" | "default_parameter" | "typed_default_parameter" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(&name_node, source);
                    if !name.is_empty() {
                        variables.insert(name.to_string());
                    }
                } else if let Some(first_child) = child.child(0) {
                    if first_child.kind() == "identifier" {
                        let name = node_text(&first_child, source);
                        if !name.is_empty() {
                            variables.insert(name.to_string());
                        }
                    }
                }
            }
            "list_splat_pattern" | "dictionary_splat_pattern" => {
                // *args and **kwargs
                if let Some(name_node) = child.child(0) {
                    if name_node.kind() == "identifier" {
                        let name = node_text(&name_node, source);
                        if !name.is_empty() {
                            variables.insert(name.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extract Python local variables from a block before target line
///
/// Recursively searches for all assignments that occur before or on the target line.
fn extract_python_locals(
    node: tree_sitter::Node,
    source: &[u8],
    target_line: usize,
    variables: &mut HashSet<String>,
) {
    // Skip nodes that start entirely after the target line
    if node.start_position().row > target_line {
        return;
    }

    match node.kind() {
        "assignment" | "augmented_assignment" => {
            if let Some(left) = node.child_by_field_name("left") {
                extract_python_pattern(left, source, variables);
            }
        }
        "expression_statement" => {
            // Check for walrus operator (:=) or nested assignment
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "named_expression" {
                    if let Some(name) = child.child_by_field_name("name") {
                        let text = node_text(&name, source);
                        if !text.is_empty() {
                            variables.insert(text.to_string());
                        }
                    }
                } else if child.kind() == "assignment" {
                    if let Some(left) = child.child_by_field_name("left") {
                        extract_python_pattern(left, source, variables);
                    }
                }
            }
        }
        "for_statement" => {
            if let Some(left) = node.child_by_field_name("left") {
                extract_python_pattern(left, source, variables);
            }
            // Also check body for more locals
            if let Some(body) = node.child_by_field_name("body") {
                extract_python_locals(body, source, target_line, variables);
            }
        }
        "with_statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "with_clause" {
                    let mut clause_cursor = child.walk();
                    for clause_item in child.children(&mut clause_cursor) {
                        if clause_item.kind() == "with_item" {
                            if let Some(alias) = clause_item.child_by_field_name("alias") {
                                extract_python_pattern(alias, source, variables);
                            }
                        }
                    }
                }
            }
        }
        "except_clause" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    let name = node_text(&child, source);
                    if !name.is_empty() {
                        variables.insert(name.to_string());
                    }
                }
            }
        }
        _ => {}
    }

    // Always recurse into children to find more locals
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_python_locals(child, source, target_line, variables);
    }
}

/// Extract variable names from a Python pattern (identifier, tuple, list)
fn extract_python_pattern(node: tree_sitter::Node, source: &[u8], variables: &mut HashSet<String>) {
    match node.kind() {
        "identifier" => {
            let name = node_text(&node, source);
            if !name.is_empty() && name != "_" {
                variables.insert(name.to_string());
            }
        }
        "tuple_pattern" | "list_pattern" | "pattern_list" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_python_pattern(child, source, variables);
            }
        }
        _ => {}
    }
}

/// Analyze Go scope
fn analyze_go_scope(source: &str, target_line: u32) -> ScopeResult {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .is_err()
    {
        return ScopeResult::default();
    }

    let tree = match parser.parse(source, None) {
        Some(tree) => tree,
        None => return ScopeResult::default(),
    };

    let root = tree.root_node();
    let source_bytes = source.as_bytes();
    let target_line_0 = target_line.saturating_sub(1) as usize;

    let mut result = ScopeResult::default();
    let mut variables = HashSet::new();

    find_go_scope(
        root,
        source_bytes,
        target_line_0,
        &mut variables,
        &mut result.containing_scope,
    );

    result.available_variables = variables.into_iter().collect();
    result.available_variables.sort();
    result
}

/// Recursively find Go scope and variables
fn find_go_scope(
    node: tree_sitter::Node,
    source: &[u8],
    target_line: usize,
    variables: &mut HashSet<String>,
    containing_scope: &mut Option<String>,
) {
    let node_start = node.start_position().row;
    let node_end = node.end_position().row;

    if target_line < node_start || target_line > node_end {
        return;
    }

    match node.kind() {
        "function_declaration" | "method_declaration" => {
            // Get function name
            if let Some(name_node) = node.child_by_field_name("name") {
                *containing_scope = Some(node_text(&name_node, source).to_string());
            }

            // Get receiver (for methods)
            if let Some(receiver) = node.child_by_field_name("receiver") {
                extract_go_parameter_list(receiver, source, variables);
            }

            // Get parameters
            if let Some(params) = node.child_by_field_name("parameters") {
                extract_go_parameter_list(params, source, variables);
            }

            // Get body and extract locals
            if let Some(body) = node.child_by_field_name("body") {
                extract_go_locals(body, source, target_line, variables);
                find_go_scope(body, source, target_line, variables, containing_scope);
            }
        }
        "func_literal" => {
            // Anonymous function
            if let Some(params) = node.child_by_field_name("parameters") {
                extract_go_parameter_list(params, source, variables);
            }
            if let Some(body) = node.child_by_field_name("body") {
                extract_go_locals(body, source, target_line, variables);
                find_go_scope(body, source, target_line, variables, containing_scope);
            }
        }
        "for_statement" => {
            // For loop variables
            if let Some(init) = node.child_by_field_name("initializer") {
                if init.start_position().row <= target_line {
                    extract_go_short_var_decl(init, source, variables);
                }
            }
            // Range clause
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "range_clause" {
                    if let Some(left) = child.child_by_field_name("left") {
                        if left.start_position().row <= target_line {
                            extract_go_expression_list(left, source, variables);
                        }
                    }
                }
                find_go_scope(child, source, target_line, variables, containing_scope);
            }
        }
        "if_statement" => {
            // If statement initializer
            if let Some(init) = node.child_by_field_name("initializer") {
                if init.start_position().row <= target_line {
                    extract_go_short_var_decl(init, source, variables);
                }
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                find_go_scope(child, source, target_line, variables, containing_scope);
            }
        }
        "source_file" | "block" | "switch_statement" | "select_statement" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                find_go_scope(child, source, target_line, variables, containing_scope);
            }
        }
        _ => {}
    }
}

/// Extract Go parameter list
fn extract_go_parameter_list(
    node: tree_sitter::Node,
    source: &[u8],
    variables: &mut HashSet<String>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            // Get the name(s)
            let mut param_cursor = child.walk();
            for param_child in child.children(&mut param_cursor) {
                if param_child.kind() == "identifier" {
                    let name = node_text(&param_child, source);
                    if !name.is_empty() && name != "_" {
                        variables.insert(name.to_string());
                    }
                }
            }
        }
    }
}

/// Extract Go locals from block
fn extract_go_locals(
    block: tree_sitter::Node,
    source: &[u8],
    target_line: usize,
    variables: &mut HashSet<String>,
) {
    let mut cursor = block.walk();
    for child in block.children(&mut cursor) {
        if child.start_position().row > target_line {
            continue;
        }

        match child.kind() {
            "short_var_declaration" => {
                extract_go_short_var_decl(child, source, variables);
            }
            "var_declaration" => {
                let mut var_cursor = child.walk();
                for spec in child.children(&mut var_cursor) {
                    if spec.kind() == "var_spec" {
                        if let Some(name) = spec.child_by_field_name("name") {
                            extract_go_expression_list(name, source, variables);
                        }
                    }
                }
            }
            "const_declaration" => {
                let mut const_cursor = child.walk();
                for spec in child.children(&mut const_cursor) {
                    if spec.kind() == "const_spec" {
                        if let Some(name) = spec.child_by_field_name("name") {
                            extract_go_expression_list(name, source, variables);
                        }
                    }
                }
            }
            _ => {
                extract_go_locals(child, source, target_line, variables);
            }
        }
    }
}

/// Extract from short var declaration
fn extract_go_short_var_decl(
    node: tree_sitter::Node,
    source: &[u8],
    variables: &mut HashSet<String>,
) {
    if node.kind() == "short_var_declaration" {
        if let Some(left) = node.child_by_field_name("left") {
            extract_go_expression_list(left, source, variables);
        }
    }
}

/// Extract identifiers from expression list
fn extract_go_expression_list(
    node: tree_sitter::Node,
    source: &[u8],
    variables: &mut HashSet<String>,
) {
    match node.kind() {
        "identifier" => {
            let name = node_text(&node, source);
            if !name.is_empty() && name != "_" {
                variables.insert(name.to_string());
            }
        }
        "expression_list" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "identifier" {
                    let name = node_text(&child, source);
                    if !name.is_empty() && name != "_" {
                        variables.insert(name.to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

/// Analyze Rust scope
fn analyze_rust_scope(source: &str, target_line: u32) -> ScopeResult {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return ScopeResult::default();
    }

    let tree = match parser.parse(source, None) {
        Some(tree) => tree,
        None => return ScopeResult::default(),
    };

    let root = tree.root_node();
    let source_bytes = source.as_bytes();
    let target_line_0 = target_line.saturating_sub(1) as usize;

    let mut result = ScopeResult::default();
    let mut variables = HashSet::new();

    find_rust_scope(
        root,
        source_bytes,
        target_line_0,
        &mut variables,
        &mut result.containing_scope,
    );

    result.available_variables = variables.into_iter().collect();
    result.available_variables.sort();
    result
}

/// Recursively find Rust scope and variables
fn find_rust_scope(
    node: tree_sitter::Node,
    source: &[u8],
    target_line: usize,
    variables: &mut HashSet<String>,
    containing_scope: &mut Option<String>,
) {
    let node_start = node.start_position().row;
    let node_end = node.end_position().row;

    if target_line < node_start || target_line > node_end {
        return;
    }

    match node.kind() {
        "function_item" => {
            // Get function name
            if let Some(name_node) = node.child_by_field_name("name") {
                *containing_scope = Some(node_text(&name_node, source).to_string());
            }

            // Get parameters
            if let Some(params) = node.child_by_field_name("parameters") {
                extract_rust_parameters(params, source, variables);
            }

            // Get body
            if let Some(body) = node.child_by_field_name("body") {
                extract_rust_locals(body, source, target_line, variables);
                find_rust_scope(body, source, target_line, variables, containing_scope);
            }
        }
        "impl_item" => {
            // Add 'self' if we're inside an impl
            variables.insert("self".to_string());
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                find_rust_scope(child, source, target_line, variables, containing_scope);
            }
        }
        "closure_expression" => {
            // Closure parameters
            if let Some(params) = node.child_by_field_name("parameters") {
                extract_rust_closure_params(params, source, variables);
            }
            if let Some(body) = node.child_by_field_name("body") {
                find_rust_scope(body, source, target_line, variables, containing_scope);
            }
        }
        "for_expression" => {
            // For loop pattern
            if let Some(pattern) = node.child_by_field_name("pattern") {
                if pattern.start_position().row <= target_line {
                    extract_rust_pattern(pattern, source, variables);
                }
            }
            if let Some(body) = node.child_by_field_name("body") {
                extract_rust_locals(body, source, target_line, variables);
                find_rust_scope(body, source, target_line, variables, containing_scope);
            }
        }
        "if_expression" | "match_expression" | "while_expression" | "loop_expression" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                find_rust_scope(child, source, target_line, variables, containing_scope);
            }
        }
        "match_arm" => {
            // Match arm pattern
            if let Some(pattern) = node.child_by_field_name("pattern") {
                if pattern.start_position().row <= target_line {
                    extract_rust_pattern(pattern, source, variables);
                }
            }
        }
        "source_file" | "block" => {
            // Extract locals and recurse
            extract_rust_locals(node, source, target_line, variables);
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                find_rust_scope(child, source, target_line, variables, containing_scope);
            }
        }
        _ => {
            // Recurse into unknown nodes to find nested functions/expressions
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                find_rust_scope(child, source, target_line, variables, containing_scope);
            }
        }
    }
}

/// Extract Rust function parameters
fn extract_rust_parameters(
    node: tree_sitter::Node,
    source: &[u8],
    variables: &mut HashSet<String>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "parameter" => {
                if let Some(pattern) = child.child_by_field_name("pattern") {
                    extract_rust_pattern(pattern, source, variables);
                }
            }
            "self_parameter" => {
                variables.insert("self".to_string());
            }
            _ => {}
        }
    }
}

/// Extract Rust closure parameters
fn extract_rust_closure_params(
    node: tree_sitter::Node,
    source: &[u8],
    variables: &mut HashSet<String>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "parameter" {
            if let Some(pattern) = child.child_by_field_name("pattern") {
                extract_rust_pattern(pattern, source, variables);
            }
        } else if child.kind() == "identifier" {
            let name = node_text(&child, source);
            if !name.is_empty() && name != "_" {
                variables.insert(name.to_string());
            }
        }
    }
}

/// Extract Rust locals from block
///
/// Recursively searches for all let declarations that occur before or on the target line.
fn extract_rust_locals(
    node: tree_sitter::Node,
    source: &[u8],
    target_line: usize,
    variables: &mut HashSet<String>,
) {
    // Skip nodes that start entirely after the target line
    if node.start_position().row > target_line {
        return;
    }

    if node.kind() == "let_declaration" {
        if let Some(pattern) = node.child_by_field_name("pattern") {
            extract_rust_pattern(pattern, source, variables);
        }
    }

    // Always recurse into children to find more locals
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_rust_locals(child, source, target_line, variables);
    }
}

/// Extract variable names from Rust pattern
fn extract_rust_pattern(node: tree_sitter::Node, source: &[u8], variables: &mut HashSet<String>) {
    match node.kind() {
        "identifier" => {
            let name = node_text(&node, source);
            if !name.is_empty() && name != "_" {
                variables.insert(name.to_string());
            }
        }
        "tuple_pattern" | "slice_pattern" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_rust_pattern(child, source, variables);
            }
        }
        "struct_pattern" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "field_pattern" {
                    // Check for shorthand (field) vs full (field: pattern)
                    if let Some(pattern) = child.child_by_field_name("pattern") {
                        extract_rust_pattern(pattern, source, variables);
                    } else if let Some(name) = child.child_by_field_name("name") {
                        let text = node_text(&name, source);
                        if !text.is_empty() && text != "_" {
                            variables.insert(text.to_string());
                        }
                    }
                }
            }
        }
        "ref_pattern" | "mut_pattern" => {
            // ref x or mut x
            if let Some(inner) = node.child(1) {
                extract_rust_pattern(inner, source, variables);
            }
        }
        "or_pattern" => {
            // x | y - extract from first alternative
            if let Some(first) = node.child(0) {
                extract_rust_pattern(first, source, variables);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_function_params() {
        let source = r#"
def calculate(x, y, z):
    result = x + y
    return result
"#;
        let result = analyze_scope(source, 3, SourceLanguage::Python);
        assert!(result.available_variables.contains(&"x".to_string()));
        assert!(result.available_variables.contains(&"y".to_string()));
        assert!(result.available_variables.contains(&"z".to_string()));
        assert!(result.available_variables.contains(&"result".to_string()));
        assert_eq!(result.containing_scope, Some("calculate".to_string()));
    }

    #[test]
    fn test_python_for_loop() {
        let source = r#"
def process(items):
    for item in items:
        value = item * 2
        print(value)
"#;
        let result = analyze_scope(source, 4, SourceLanguage::Python);
        assert!(result.available_variables.contains(&"items".to_string()));
        assert!(result.available_variables.contains(&"item".to_string()));
        assert!(result.available_variables.contains(&"value".to_string()));
    }

    #[test]
    fn test_python_method_with_self() {
        let source = r#"
class Calculator:
    def add(self, a, b):
        result = a + b
        return result
"#;
        let result = analyze_scope(source, 4, SourceLanguage::Python);
        assert!(result.available_variables.contains(&"self".to_string()));
        assert!(result.available_variables.contains(&"a".to_string()));
        assert!(result.available_variables.contains(&"b".to_string()));
    }

    #[test]
    fn test_go_function_params() {
        let source = r#"
func calculate(x int, y int) int {
    result := x + y
    return result
}
"#;
        let result = analyze_scope(source, 3, SourceLanguage::Go);
        assert!(result.available_variables.contains(&"x".to_string()));
        assert!(result.available_variables.contains(&"y".to_string()));
        assert!(result.available_variables.contains(&"result".to_string()));
        assert_eq!(result.containing_scope, Some("calculate".to_string()));
    }

    #[test]
    fn test_go_method_receiver() {
        let source = r#"
func (c *Calculator) Add(a, b int) int {
    result := a + b
    return result
}
"#;
        let result = analyze_scope(source, 3, SourceLanguage::Go);
        assert!(result.available_variables.contains(&"c".to_string()));
        assert!(result.available_variables.contains(&"a".to_string()));
        assert!(result.available_variables.contains(&"b".to_string()));
    }

    #[test]
    fn test_go_for_range() {
        let source = r#"
func process(items []int) {
    for i, v := range items {
        fmt.Println(i, v)
    }
}
"#;
        let result = analyze_scope(source, 4, SourceLanguage::Go);
        assert!(result.available_variables.contains(&"items".to_string()));
        assert!(result.available_variables.contains(&"i".to_string()));
        assert!(result.available_variables.contains(&"v".to_string()));
    }

    #[test]
    fn test_rust_function_params() {
        let source = r#"
fn calculate(x: i32, y: i32) -> i32 {
    let result = x + y;
    result
}
"#;
        let result = analyze_scope(source, 3, SourceLanguage::Rust);
        assert!(result.available_variables.contains(&"x".to_string()));
        assert!(result.available_variables.contains(&"y".to_string()));
        assert!(result.available_variables.contains(&"result".to_string()));
        assert_eq!(result.containing_scope, Some("calculate".to_string()));
    }

    #[test]
    fn test_rust_method_with_self() {
        let source = r#"
impl Calculator {
    fn add(&self, a: i32, b: i32) -> i32 {
        let result = a + b;
        result
    }
}
"#;
        let result = analyze_scope(source, 4, SourceLanguage::Rust);
        assert!(result.available_variables.contains(&"self".to_string()));
        assert!(result.available_variables.contains(&"a".to_string()));
        assert!(result.available_variables.contains(&"b".to_string()));
    }

    #[test]
    fn test_rust_for_loop() {
        let source = r#"
fn process(items: Vec<i32>) {
    for item in items.iter() {
        let value = item * 2;
        println!("{}", value);
    }
}
"#;
        let result = analyze_scope(source, 4, SourceLanguage::Rust);
        assert!(result.available_variables.contains(&"items".to_string()));
        assert!(result.available_variables.contains(&"item".to_string()));
        assert!(result.available_variables.contains(&"value".to_string()));
    }

    #[test]
    fn test_rust_let_destructuring() {
        let source = r#"
fn process(pair: (i32, i32)) {
    let (x, y) = pair;
    let result = x + y;
    result
}
"#;
        let result = analyze_scope(source, 4, SourceLanguage::Rust);
        assert!(result.available_variables.contains(&"pair".to_string()));
        assert!(result.available_variables.contains(&"x".to_string()));
        assert!(result.available_variables.contains(&"y".to_string()));
    }
}
