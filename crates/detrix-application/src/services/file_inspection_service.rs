//! File Inspection Service
//!
//! Business logic for inspecting source files to find correct metric placement.
//! This includes:
//! - Text-based file inspection (search, context extraction)
//! - Tree-sitter based scope analysis for supported languages
//!
//! The service is protocol-agnostic - it returns domain types that can be
//! converted to any presentation format (MCP, gRPC, REST, etc.)

use crate::error::{FileInspectionError, IoErrorWithContext};
use crate::safety::treesitter::analyze_scope;
use crate::services::file_inspection_types::{
    CodeContext, CodeLine, FileInspectionRequest, FileInspectionResult, FileOverview,
    LineInspectionResult, SourceLanguage, SourceLanguageExt, TextSearchMatch, VariableDefinition,
    VariableSearchResult,
};
use crate::Result;
use detrix_config::constants::{
    DEFAULT_CONTEXT_LINES, DEFAULT_PREVIEW_LINES, MAX_PATH_COMPONENT_LENGTH, MAX_PATH_LENGTH,
};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Validate and canonicalize a file path for inspection
///
/// Security checks performed:
/// 1. Path length limits (prevent DoS)
/// 2. No null bytes (prevent injection)
/// 3. Path must be absolute or resolvable to absolute
/// 4. Canonicalize to resolve symlinks and `..` components
/// 5. File must exist and be readable
///
/// Returns the canonicalized absolute path on success.
fn validate_file_path(file_path: &str) -> Result<PathBuf> {
    // Check path length limits
    if file_path.len() > MAX_PATH_LENGTH {
        return Err(FileInspectionError::InvalidPath(format!(
            "Path too long: {} chars (max {})",
            file_path.len(),
            MAX_PATH_LENGTH
        ))
        .into());
    }

    // Check for null bytes (injection attack)
    if file_path.contains('\0') {
        warn!(path = file_path, "Rejected file path containing null byte");
        return Err(FileInspectionError::InvalidPath(
            "Invalid path: contains null byte".to_string(),
        )
        .into());
    }

    // Check individual component lengths
    for component in file_path.split(['/', '\\']) {
        if component.len() > MAX_PATH_COMPONENT_LENGTH {
            return Err(FileInspectionError::InvalidPath(format!(
                "Path component too long: {} chars (max {})",
                component.len(),
                MAX_PATH_COMPONENT_LENGTH
            ))
            .into());
        }
    }

    let path = Path::new(file_path);

    // Check if file exists before canonicalizing
    if !path.exists() {
        return Err(FileInspectionError::NotFound(file_path.to_string()).into());
    }

    // Canonicalize to get absolute path and resolve symlinks/..
    // This is the key security step - it resolves path traversal attempts
    let canonical = path.canonicalize().map_err(|e| {
        FileInspectionError::InvalidPath(format!("Cannot resolve path '{}': {}", file_path, e))
    })?;

    // Verify it's a file (not a directory)
    if !canonical.is_file() {
        return Err(FileInspectionError::NotAFile(canonical.display().to_string()).into());
    }

    debug!(
        original = file_path,
        canonical = %canonical.display(),
        "Validated file path"
    );

    Ok(canonical)
}

/// Service for inspecting source files
///
/// Provides text-based analysis capabilities for finding correct metric placement.
/// All languages use the generic text-based inspection.
#[derive(Debug, Clone)]
pub struct FileInspectionService {
    /// Number of lines to include as context around target line
    context_lines: usize,
    /// Number of preview lines for file overview
    preview_lines: usize,
}

impl Default for FileInspectionService {
    fn default() -> Self {
        Self {
            context_lines: DEFAULT_CONTEXT_LINES,
            preview_lines: DEFAULT_PREVIEW_LINES,
        }
    }
}

impl FileInspectionService {
    /// Create a new file inspection service with default config
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a file inspection service from API config
    pub fn from_config(context_lines: usize, preview_lines: usize) -> Self {
        Self {
            context_lines,
            preview_lines,
        }
    }
}

impl From<&detrix_config::ApiConfig> for FileInspectionService {
    fn from(config: &detrix_config::ApiConfig) -> Self {
        Self::from_config(config.context_lines, config.preview_lines)
    }
}

impl FileInspectionService {
    /// Inspect a source file
    ///
    /// Uses text-based context extraction for all file types.
    ///
    /// # Security
    ///
    /// The file path is validated and canonicalized before any file access:
    /// - Path length limits enforced
    /// - Null bytes rejected
    /// - Path traversal (`..`) resolved via canonicalization
    /// - Symlinks resolved to their target
    pub fn inspect(
        &self,
        request: FileInspectionRequest,
    ) -> Result<(SourceLanguage, FileInspectionResult)> {
        // Validate and canonicalize path before any file access
        let canonical_path = validate_file_path(&request.file_path)?;

        let extension = canonical_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let language = SourceLanguage::from_extension(&extension);

        // Create a modified request with the canonical path
        let validated_request = FileInspectionRequest {
            file_path: canonical_path.to_string_lossy().into_owned(),
            line: request.line,
            find_variable: request.find_variable,
        };

        // All languages use generic text-based inspection
        let result = self.inspect_generic(&validated_request, language)?;

        Ok((language, result))
    }

    /// Inspect file using text-based analysis with optional tree-sitter scope analysis
    fn inspect_generic(
        &self,
        request: &FileInspectionRequest,
        language: SourceLanguage,
    ) -> Result<FileInspectionResult> {
        // Read the file
        let contents =
            std::fs::read_to_string(&request.file_path).map_err(|e| IoErrorWithContext {
                error: e,
                path: request.file_path.clone(),
            })?;

        let lines: Vec<&str> = contents.lines().collect();
        let total_lines = lines.len();

        if let Some(target_line) = request.line {
            self.inspect_generic_line(
                &contents,
                &lines,
                target_line as usize,
                total_lines,
                language,
            )
        } else if let Some(ref var) = request.find_variable {
            self.inspect_generic_variable(&lines, var, total_lines)
        } else {
            self.inspect_generic_overview(&lines, total_lines)
        }
    }

    /// Inspect a specific line in a file
    ///
    /// Uses tree-sitter scope analysis for supported languages to find available variables.
    fn inspect_generic_line(
        &self,
        contents: &str,
        lines: &[&str],
        target_line: usize,
        total_lines: usize,
        language: SourceLanguage,
    ) -> Result<FileInspectionResult> {
        if target_line == 0 || target_line > total_lines {
            return Err(FileInspectionError::LineNotFound {
                path: "file".to_string(), // We don't have the path here
                line: target_line as u32,
                total_lines,
            }
            .into());
        }

        let target_idx = target_line - 1; // Convert to 0-based index
        let start = target_idx.saturating_sub(self.context_lines);
        let end = (target_idx + self.context_lines + 1).min(total_lines);

        let context_lines: Vec<CodeLine> = lines
            .iter()
            .enumerate()
            .skip(start)
            .take(end - start)
            .map(|(i, code)| CodeLine {
                line_number: (i + 1) as u32,
                code: code.to_string(),
                is_target: i == target_idx,
            })
            .collect();

        // Use tree-sitter scope analysis for supported languages
        let available_variables = if language.capabilities().has_ast_analysis {
            let scope_result = analyze_scope(contents, target_line as u32, language);
            scope_result.available_variables
        } else {
            vec![]
        };

        Ok(FileInspectionResult::LineInspection(LineInspectionResult {
            target_line: target_line as u32,
            code_at_line: lines.get(target_idx).unwrap_or(&"").to_string(),
            available_variables,
            context: CodeContext {
                start_line: (start + 1) as u32,
                lines: context_lines,
            },
        }))
    }

    /// Search for a variable in a generic file (text-based)
    fn inspect_generic_variable(
        &self,
        lines: &[&str],
        variable: &str,
        _total_lines: usize,
    ) -> Result<FileInspectionResult> {
        let matches: Vec<TextSearchMatch> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.contains(variable))
            .map(|(i, line)| TextSearchMatch {
                line_number: (i + 1) as u32,
                code: line.to_string(),
            })
            .collect();

        // Convert text matches to variable definitions (approximate)
        let definitions: Vec<VariableDefinition> = matches
            .iter()
            .take(10) // Limit to first 10
            .map(|m| VariableDefinition {
                line: m.line_number,
                scope: "unknown".to_string(), // No scope info without AST
                code: m.code.clone(),
            })
            .collect();

        let suggested_lines: Vec<u32> = definitions.iter().map(|d| d.line).collect();

        // Build context around first match if any
        let context = if let Some(first) = matches.first() {
            let target_idx = (first.line_number - 1) as usize;
            let start = target_idx.saturating_sub(2);
            let end = (target_idx + 5).min(lines.len());

            Some(CodeContext {
                start_line: (start + 1) as u32,
                lines: lines
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(end - start)
                    .map(|(i, code)| CodeLine {
                        line_number: (i + 1) as u32,
                        code: code.to_string(),
                        is_target: i == target_idx,
                    })
                    .collect(),
            })
        } else {
            None
        };

        Ok(FileInspectionResult::VariableSearch(VariableSearchResult {
            variable_name: variable.to_string(),
            definitions,
            suggested_lines,
            context,
            similar_variables: vec![], // Not available without AST
        }))
    }

    /// Get overview of a generic file
    fn inspect_generic_overview(
        &self,
        lines: &[&str],
        total_lines: usize,
    ) -> Result<FileInspectionResult> {
        let preview_count = total_lines.min(self.preview_lines);
        let preview_lines: Vec<CodeLine> = lines
            .iter()
            .enumerate()
            .take(preview_count)
            .map(|(i, code)| CodeLine {
                line_number: (i + 1) as u32,
                code: code.to_string(),
                is_target: false,
            })
            .collect();

        Ok(FileInspectionResult::Overview(FileOverview {
            total_lines: total_lines as u32,
            preview_lines,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::file_inspection_types::SourceLanguageExt;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_source_language_from_extension() {
        assert_eq!(SourceLanguage::from_extension("py"), SourceLanguage::Python);
        assert_eq!(SourceLanguage::from_extension("go"), SourceLanguage::Go);
        assert_eq!(SourceLanguage::from_extension("rs"), SourceLanguage::Rust);
        assert_eq!(
            SourceLanguage::from_extension("js"),
            SourceLanguage::JavaScript
        );
        assert_eq!(
            SourceLanguage::from_extension("unknown"),
            SourceLanguage::Unknown
        );
    }

    #[test]
    fn test_source_language_supports_ast() {
        assert!(SourceLanguage::Python.supports_ast_analysis());
        assert!(SourceLanguage::Go.supports_ast_analysis());
        assert!(SourceLanguage::Rust.supports_ast_analysis());
    }

    #[test]
    fn test_inspect_generic_line() {
        let mut file = NamedTempFile::with_suffix(".rs").unwrap();
        writeln!(file, "fn main() {{").unwrap();
        writeln!(file, "    let x = 42;").unwrap();
        writeln!(file, "    println!(\"{{:?}}\", x);").unwrap();
        writeln!(file, "}}").unwrap();

        let service = FileInspectionService::new();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: Some(2),
            find_variable: None,
        };

        let (lang, result) = service.inspect(request).unwrap();
        assert_eq!(lang, SourceLanguage::Rust);

        if let FileInspectionResult::LineInspection(inspection) = result {
            assert_eq!(inspection.target_line, 2);
            assert!(inspection.code_at_line.contains("let x = 42"));
        } else {
            panic!("Expected LineInspection result");
        }
    }

    #[test]
    fn test_inspect_generic_variable() {
        // Use Java file to test generic (text-based) variable search
        // Go now has AST support so we use a language without AST
        let mut file = NamedTempFile::with_suffix(".java").unwrap();
        writeln!(file, "public class Main {{").unwrap();
        writeln!(file, "    public static void main(String[] args) {{").unwrap();
        writeln!(file, "        int userID = 123;").unwrap();
        writeln!(file, "        System.out.println(userID);").unwrap();
        writeln!(file, "    }}").unwrap();
        writeln!(file, "}}").unwrap();

        let service = FileInspectionService::new();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("userID".to_string()),
        };

        let (lang, result) = service.inspect(request).unwrap();
        assert_eq!(lang, SourceLanguage::Java);

        if let FileInspectionResult::VariableSearch(search) = result {
            assert_eq!(search.variable_name, "userID");
            assert!(!search.definitions.is_empty());
            assert!(search.definitions.iter().any(|d| d.code.contains("userID")));
        } else {
            panic!("Expected VariableSearch result");
        }
    }

    #[test]
    fn test_inspect_generic_overview() {
        let mut file = NamedTempFile::with_suffix(".ts").unwrap();
        for i in 1..=30 {
            writeln!(file, "// Line {}", i).unwrap();
        }

        let service = FileInspectionService::new();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: None,
        };

        let (lang, result) = service.inspect(request).unwrap();
        assert_eq!(lang, SourceLanguage::TypeScript);

        if let FileInspectionResult::Overview(overview) = result {
            assert_eq!(overview.total_lines, 30);
            assert_eq!(overview.preview_lines.len(), DEFAULT_PREVIEW_LINES);
        } else {
            panic!("Expected Overview result");
        }
    }

    #[test]
    fn test_inspect_line_out_of_range() {
        // All AST analyzers should return error for out-of-range lines
        let mut file = NamedTempFile::with_suffix(".rs").unwrap();
        writeln!(file, "fn main() {{}}").unwrap();

        let service = FileInspectionService::new();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: Some(100),
            find_variable: None,
        };

        let result = service.inspect(request);
        assert!(result.is_err());
    }

    // ========================================================================
    // Path Validation Tests
    // ========================================================================

    #[test]
    fn test_validate_file_path_valid() {
        let mut file = NamedTempFile::with_suffix(".py").unwrap();
        writeln!(file, "x = 1").unwrap();

        let result = validate_file_path(&file.path().to_string_lossy());
        assert!(result.is_ok());
        let canonical = result.unwrap();
        assert!(canonical.is_absolute());
        assert!(canonical.exists());
    }

    #[test]
    fn test_validate_file_path_not_found() {
        let result = validate_file_path("/nonexistent/path/to/file.py");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("File not found"));
    }

    #[test]
    fn test_validate_file_path_null_byte() {
        let result = validate_file_path("/some/path\0/file.py");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("null byte"));
    }

    #[test]
    fn test_validate_file_path_too_long() {
        let long_path = "a".repeat(MAX_PATH_LENGTH + 1);
        let result = validate_file_path(&long_path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Path too long"));
    }

    #[test]
    fn test_validate_file_path_component_too_long() {
        let long_component = "a".repeat(MAX_PATH_COMPONENT_LENGTH + 1);
        let path = format!("/tmp/{}/file.py", long_component);
        let result = validate_file_path(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("component too long"));
    }

    #[test]
    fn test_validate_file_path_is_directory() {
        // Create a temp directory
        let dir = tempfile::tempdir().unwrap();
        let result = validate_file_path(&dir.path().to_string_lossy());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Not a file"));
    }

    #[test]
    fn test_validate_file_path_traversal_resolved() {
        // Create a temp file
        let file = NamedTempFile::with_suffix(".py").unwrap();
        let file_path = file.path().to_string_lossy().to_string();

        // Create a path with traversal that resolves to the same file
        let parent = file.path().parent().unwrap();
        let filename = file.path().file_name().unwrap().to_string_lossy();
        let traversal_path = format!(
            "{}/../{}/{}",
            parent.display(),
            parent.file_name().unwrap().to_string_lossy(),
            filename
        );

        let result = validate_file_path(&traversal_path);
        assert!(result.is_ok());
        let canonical = result.unwrap();

        // Should resolve to the same canonical path
        let expected = std::path::Path::new(&file_path).canonicalize().unwrap();
        assert_eq!(canonical, expected);
    }
}
