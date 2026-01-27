//! Tree-sitter based comment stripping for fingerprint normalization
//!
//! Removes comments from source code while preserving structure.
//! Uses byte-offset based removal to maintain accurate line positions.
//!
//! # Supported Comment Types
//!
//! | Language | Single-line | Multi-line |
//! |----------|-------------|------------|
//! | Python   | `#`         | `'''`/`"""` (docstrings used as comments) |
//! | Go       | `//`        | `/* */`    |
//! | Rust     | `//`        | `/* */`    |

use super::node_kinds as nodes;
use detrix_core::{Error, Result, SourceLanguage};
use tree_sitter::Parser;

/// Strip comments from source code using tree-sitter
///
/// Returns the source with all comments removed.
/// Uses accurate AST parsing to distinguish comments from strings.
///
/// # Arguments
///
/// * `source` - The source code to strip comments from
/// * `language` - The programming language of the source
///
/// # Returns
///
/// The source code with comments removed
///
/// # Example
///
/// ```ignore
/// use detrix_core::SourceLanguage;
/// let source = "x = 1  # comment\ny = 2";
/// let stripped = strip_comments(source, SourceLanguage::Python)?;
/// assert_eq!(stripped, "x = 1  \ny = 2");
/// ```
pub fn strip_comments(source: &str, language: SourceLanguage) -> Result<String> {
    if source.is_empty() {
        return Ok(String::new());
    }

    match language {
        SourceLanguage::Python => strip_python_comments(source),
        SourceLanguage::Go => strip_go_comments(source),
        SourceLanguage::Rust => strip_rust_comments(source),
        // Languages without tree-sitter support - return source unchanged
        SourceLanguage::JavaScript
        | SourceLanguage::TypeScript
        | SourceLanguage::Java
        | SourceLanguage::Cpp
        | SourceLanguage::C
        | SourceLanguage::Ruby
        | SourceLanguage::Php
        | SourceLanguage::Unknown => Ok(source.to_string()),
    }
}

fn strip_python_comments(source: &str) -> Result<String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .map_err(|_| Error::AstAnalysisFailed("Failed to load Python grammar".into()))?;

    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::AstAnalysisFailed("Failed to parse Python source".into()))?;

    let mut comment_ranges: Vec<(usize, usize)> = Vec::new();

    // Traverse and collect both:
    // 1. `comment` nodes (# line comments)
    // 2. `expression_statement` > `string` nodes (docstrings used as comments)
    collect_python_comments(tree.root_node(), source.as_bytes(), &mut comment_ranges);

    // Sort and remove
    comment_ranges.sort_by_key(|(start, _)| *start);
    remove_ranges(source, &comment_ranges)
}

/// Recursively collect Python comment ranges
fn collect_python_comments(
    node: tree_sitter::Node,
    source: &[u8],
    ranges: &mut Vec<(usize, usize)>,
) {
    // # line comments
    if node.kind() == nodes::python::COMMENT {
        ranges.push((node.start_byte(), node.end_byte()));
    }
    // Standalone string literals (docstrings used as comments: '''..''' or """...""")
    // These are expression_statement nodes containing only a string child
    else if node.kind() == nodes::python::EXPRESSION_STATEMENT && node.child_count() == 1 {
        if let Some(child) = node.child(0) {
            if child.kind() == nodes::python::STRING {
                // Check if it's a triple-quoted string (multiline comment)
                let start = child.start_byte();
                let end = child.end_byte();
                if end > start && end <= source.len() {
                    let text = &source[start..end];
                    if text.starts_with(b"'''") || text.starts_with(b"\"\"\"") {
                        ranges.push((node.start_byte(), node.end_byte()));
                    }
                }
            }
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_python_comments(child, source, ranges);
    }
}

fn strip_go_comments(source: &str) -> Result<String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .map_err(|_| Error::AstAnalysisFailed("Failed to load Go grammar".into()))?;

    // Go uses "comment" node kind for BOTH // line comments AND /* */ block comments
    strip_with_parser(source, &mut parser, |kind| kind == nodes::go::COMMENT)
}

fn strip_rust_comments(source: &str) -> Result<String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|_| Error::AstAnalysisFailed("Failed to load Rust grammar".into()))?;

    // Rust uses separate node kinds:
    // - "line_comment" for // comments (including /// and //!)
    // - "block_comment" for /* */ comments (including /** */ and /*! */)
    strip_with_parser(source, &mut parser, |kind| {
        kind == nodes::rust::LINE_COMMENT || kind == nodes::rust::BLOCK_COMMENT
    })
}

/// Generic comment stripping using tree-sitter
fn strip_with_parser<F>(source: &str, parser: &mut Parser, is_comment: F) -> Result<String>
where
    F: Fn(&str) -> bool,
{
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::AstAnalysisFailed("Failed to parse source".into()))?;

    // Collect comment ranges
    let mut comment_ranges: Vec<(usize, usize)> = Vec::new();
    collect_comments(tree.root_node(), &is_comment, &mut comment_ranges);

    // Sort and remove
    comment_ranges.sort_by_key(|(start, _)| *start);
    remove_ranges(source, &comment_ranges)
}

/// Recursively collect comment ranges
fn collect_comments<F>(node: tree_sitter::Node, is_comment: &F, ranges: &mut Vec<(usize, usize)>)
where
    F: Fn(&str) -> bool,
{
    if is_comment(node.kind()) {
        ranges.push((node.start_byte(), node.end_byte()));
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_comments(child, is_comment, ranges);
    }
}

/// Remove byte ranges from source, returning the result
fn remove_ranges(source: &str, ranges: &[(usize, usize)]) -> Result<String> {
    let mut result = String::with_capacity(source.len());
    let mut last_end = 0;

    for &(start, end) in ranges {
        if start > last_end && start <= source.len() {
            result.push_str(&source[last_end..start]);
        }
        last_end = end.min(source.len());
    }

    // Add remaining content
    if last_end < source.len() {
        result.push_str(&source[last_end..]);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== Python Tests =====

    #[test]
    fn test_strip_python_line_comment() {
        let source = "x = 1  # this is a comment\ny = 2";
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, "x = 1  \ny = 2");
    }

    #[test]
    fn test_strip_python_multiline_comment_triple_single_quotes() {
        let source = "x = 1\n'''\nThis is a multiline\ncomment.\n'''\ny = 2";
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, "x = 1\n\ny = 2");
    }

    #[test]
    fn test_strip_python_multiline_comment_triple_double_quotes() {
        let source = "x = 1\n\"\"\"\nThis is a multiline\ncomment.\n\"\"\"\ny = 2";
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, "x = 1\n\ny = 2");
    }

    #[test]
    fn test_strip_python_preserves_string_with_hash() {
        let source = r#"s = "hello # world""#;
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, source); // String should be preserved
    }

    #[test]
    fn test_strip_python_preserves_assigned_docstring() {
        // Assigned strings are NOT comments - they should be preserved
        let source = r#"doc = """This is assigned""""#;
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, source); // Assigned string preserved
    }

    #[test]
    fn test_strip_python_multiple_comments() {
        let source = "x = 1  # first\ny = 2  # second\nz = 3";
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, "x = 1  \ny = 2  \nz = 3");
    }

    // ===== Go Tests =====

    #[test]
    fn test_strip_go_line_comment() {
        let source = "x := 1 // comment\ny := 2";
        let result = strip_comments(source, SourceLanguage::Go).unwrap();
        assert_eq!(result, "x := 1 \ny := 2");
    }

    #[test]
    fn test_strip_go_block_comment_inline() {
        let source = "x := 1 /* inline */ + 2";
        let result = strip_comments(source, SourceLanguage::Go).unwrap();
        assert_eq!(result, "x := 1  + 2");
    }

    #[test]
    fn test_strip_go_block_comment_multiline() {
        let source = "x := 1\n/* This is a\nmultiline\ncomment */\ny := 2";
        let result = strip_comments(source, SourceLanguage::Go).unwrap();
        assert_eq!(result, "x := 1\n\ny := 2");
    }

    #[test]
    fn test_strip_go_preserves_string_with_slash() {
        let source = r#"s := "hello // world""#;
        let result = strip_comments(source, SourceLanguage::Go).unwrap();
        assert_eq!(result, source);
    }

    // ===== Rust Tests =====

    #[test]
    fn test_strip_rust_line_comment() {
        let source = "let x = 1; // comment\nlet y = 2;";
        let result = strip_comments(source, SourceLanguage::Rust).unwrap();
        assert_eq!(result, "let x = 1; \nlet y = 2;");
    }

    #[test]
    fn test_strip_rust_block_comment_inline() {
        let source = "let x = 1 /* inline */ + 2;";
        let result = strip_comments(source, SourceLanguage::Rust).unwrap();
        assert_eq!(result, "let x = 1  + 2;");
    }

    #[test]
    fn test_strip_rust_block_comment_multiline() {
        let source = "let x = 1;\n/* This is a\nmultiline\ncomment */\nlet y = 2;";
        let result = strip_comments(source, SourceLanguage::Rust).unwrap();
        assert_eq!(result, "let x = 1;\n\nlet y = 2;");
    }

    #[test]
    fn test_strip_rust_doc_comment() {
        // Doc comments (/// and //!) are also line_comment nodes
        // The comment node doesn't include the trailing newline
        let source = "/// Documentation\nfn foo() {}";
        let result = strip_comments(source, SourceLanguage::Rust).unwrap();
        // After stripping "/// Documentation", we get "\nfn foo() {}"
        // but tree-sitter may include the newline in the comment range
        assert!(
            result == "\nfn foo() {}" || result == "fn foo() {}",
            "Expected comment stripped, got: {:?}",
            result
        );
        // Verify no comment characters remain
        assert!(!result.contains("///"));
    }

    #[test]
    fn test_strip_rust_preserves_string_with_slash() {
        let source = r#"let s = "hello // world";"#;
        let result = strip_comments(source, SourceLanguage::Rust).unwrap();
        assert_eq!(result, source);
    }

    // ===== Edge Cases =====

    #[test]
    fn test_no_comments() {
        let source = "x = 1\ny = 2";
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, source);
    }

    #[test]
    fn test_empty_source() {
        let result = strip_comments("", SourceLanguage::Python).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_only_comment() {
        let source = "# just a comment";
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_comment_at_start() {
        let source = "# comment\nx = 1";
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, "\nx = 1");
    }

    #[test]
    fn test_comment_at_end() {
        let source = "x = 1\n# comment";
        let result = strip_comments(source, SourceLanguage::Python).unwrap();
        assert_eq!(result, "x = 1\n");
    }
}
