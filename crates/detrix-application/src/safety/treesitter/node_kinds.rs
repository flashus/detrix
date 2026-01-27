//! Tree-sitter node kind constants
//!
//! Centralizes all magic strings used for AST node type matching.
//! Each language has its own module with constants matching the
//! tree-sitter grammar's node types.

/// Python tree-sitter node kinds
pub mod python {
    // === Comment nodes (for strip_comments) ===

    /// Line comment: `# comment`
    pub const COMMENT: &str = "comment";

    /// String literal (used for multiline comments via docstrings)
    pub const STRING: &str = "string";

    /// Expression statement (wrapper for standalone string = docstring comment)
    pub const EXPRESSION_STATEMENT: &str = "expression_statement";

    // === Statement nodes (prohibited in expressions) ===

    /// Simple assignment: `x = 1`
    pub const ASSIGNMENT: &str = "assignment";

    /// Augmented assignment: `x += 1`, `x -= 1`, etc.
    pub const AUGMENTED_ASSIGNMENT: &str = "augmented_assignment";

    /// Annotated assignment: `x: int = 1`
    pub const ANNOTATED_ASSIGNMENT: &str = "annotated_assignment";

    /// Delete statement: `del x`
    pub const DELETE_STATEMENT: &str = "delete_statement";

    /// Import statement: `import os`
    pub const IMPORT_STATEMENT: &str = "import_statement";

    /// From-import statement: `from os import path`
    pub const IMPORT_FROM_STATEMENT: &str = "import_from_statement";

    /// Global statement: `global x`
    pub const GLOBAL_STATEMENT: &str = "global_statement";

    /// Nonlocal statement: `nonlocal x`
    pub const NONLOCAL_STATEMENT: &str = "nonlocal_statement";

    // === Expression nodes ===

    /// Function/method call: `func()`, `obj.method()`
    pub const CALL: &str = "call";

    /// Simple identifier: `x`, `my_var`
    pub const IDENTIFIER: &str = "identifier";

    /// Attribute access: `obj.attr`
    pub const ATTRIBUTE: &str = "attribute";
}

/// Go tree-sitter node kinds
pub mod go {
    // === Comment nodes (for strip_comments) ===

    /// Comment: both `// line` and `/* block */` use same node kind
    pub const COMMENT: &str = "comment";

    // === Statement nodes (prohibited in expressions) ===

    /// Assignment statement: `x = 5`
    pub const ASSIGNMENT_STATEMENT: &str = "assignment_statement";

    /// Short variable declaration: `x := 5`
    pub const SHORT_VAR_DECLARATION: &str = "short_var_declaration";

    /// Increment statement: `x++`
    pub const INC_STATEMENT: &str = "inc_statement";

    /// Decrement statement: `x--`
    pub const DEC_STATEMENT: &str = "dec_statement";

    /// Go statement (goroutine): `go func()`
    pub const GO_STATEMENT: &str = "go_statement";

    /// Defer statement: `defer cleanup()`
    pub const DEFER_STATEMENT: &str = "defer_statement";

    /// Channel send: `ch <- value`
    pub const SEND_STATEMENT: &str = "send_statement";

    // === Expression nodes ===

    /// Function/method call: `func()`, `pkg.Func()`
    pub const CALL_EXPRESSION: &str = "call_expression";

    /// Simple identifier: `x`, `myVar`
    pub const IDENTIFIER: &str = "identifier";

    /// Selector expression (field/method access): `obj.Field`
    pub const SELECTOR_EXPRESSION: &str = "selector_expression";

    // === Structural nodes (skipped during analysis) ===

    /// Package clause: `package main`
    pub const PACKAGE_CLAUSE: &str = "package_clause";

    /// Function declaration: `func main() {}`
    pub const FUNCTION_DECLARATION: &str = "function_declaration";
}

/// Rust tree-sitter node kinds
pub mod rust {
    // === Comment nodes (for strip_comments) ===

    /// Line comment: `// comment` (includes `///` and `//!` doc comments)
    pub const LINE_COMMENT: &str = "line_comment";

    /// Block comment: `/* comment */` (includes `/** */` and `/*! */` doc comments)
    pub const BLOCK_COMMENT: &str = "block_comment";

    // === Statement/expression nodes (prohibited or flagged) ===

    /// Assignment expression: `x = 1`
    pub const ASSIGNMENT_EXPRESSION: &str = "assignment_expression";

    /// Compound assignment: `x += 1`, `x -= 1`, etc.
    pub const COMPOUND_ASSIGNMENT_EXPR: &str = "compound_assignment_expr";

    /// Unsafe block: `unsafe { ... }`
    pub const UNSAFE_BLOCK: &str = "unsafe_block";

    // === Expression nodes ===

    /// Function call: `func()`
    pub const CALL_EXPRESSION: &str = "call_expression";

    /// Method call: `obj.method()`
    pub const METHOD_CALL_EXPRESSION: &str = "method_call_expression";

    /// Macro invocation: `println!(...)`
    pub const MACRO_INVOCATION: &str = "macro_invocation";

    /// Simple identifier: `x`, `my_var`
    pub const IDENTIFIER: &str = "identifier";

    /// Scoped identifier: `std::mem::replace`
    pub const SCOPED_IDENTIFIER: &str = "scoped_identifier";

    /// Field expression: `obj.field`
    pub const FIELD_EXPRESSION: &str = "field_expression";

    // === Structural nodes (skipped during analysis) ===

    /// Function item: `fn main() {}`
    pub const FUNCTION_ITEM: &str = "function_item";

    /// Let declaration: `let x = ...`
    pub const LET_DECLARATION: &str = "let_declaration";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_constants_not_empty() {
        assert!(!python::ASSIGNMENT.is_empty());
        assert!(!python::CALL.is_empty());
        assert!(!python::IDENTIFIER.is_empty());
    }

    #[test]
    fn test_go_constants_not_empty() {
        assert!(!go::ASSIGNMENT_STATEMENT.is_empty());
        assert!(!go::CALL_EXPRESSION.is_empty());
        assert!(!go::IDENTIFIER.is_empty());
    }

    #[test]
    fn test_rust_constants_not_empty() {
        assert!(!rust::ASSIGNMENT_EXPRESSION.is_empty());
        assert!(!rust::CALL_EXPRESSION.is_empty());
        assert!(!rust::IDENTIFIER.is_empty());
    }
}
