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
use detrix_logging::{debug, warn};
use detrix_ports::VfsRef;
use std::path::{Path, PathBuf};

/// Resolve a file path against an optional workspace root.
///
/// Resolution logic:
/// 1. If the path is absolute → use as-is
/// 2. If relative and `workspace_root` is provided → join with workspace_root
/// 3. Fallback: return the raw path (covers daemon-CWD-relative paths)
///
/// Note: Does NOT check file existence on disk. In cloud mode, the file lives
/// inside a remote container and won't exist on the daemon's filesystem.
///
/// Returns the resolved path as a string for subsequent validation.
pub fn resolve_file_path(file_path: &str, workspace_root: Option<&str>) -> String {
    let path = Path::new(file_path);

    // Absolute paths need no resolution
    if path.is_absolute() {
        return file_path.to_string();
    }

    // Resolve against workspace_root (always join when workspace_root is set)
    if let Some(root) = workspace_root {
        let resolved = Path::new(root).join(path);
        debug!(
            original = file_path,
            workspace_root = root,
            resolved = %resolved.display(),
            "Resolved relative path against workspace root"
        );
        return resolved.to_string_lossy().into_owned();
    }

    // Fallback: return as-is (validate_file_path will produce the appropriate error)
    file_path.to_string()
}

/// Lightweight path syntax validation (no disk access).
///
/// Checks length limits and null bytes only. Used for VFS-cached files
/// where disk-based canonicalization is not needed.
fn validate_path_syntax(file_path: &str) -> Result<()> {
    if file_path.len() > MAX_PATH_LENGTH {
        return Err(FileInspectionError::InvalidPath(format!(
            "Path too long: {} chars (max {})",
            file_path.len(),
            MAX_PATH_LENGTH
        ))
        .into());
    }
    if file_path.contains('\0') {
        warn!(path = file_path, "Rejected file path containing null byte");
        return Err(FileInspectionError::InvalidPath(
            "Invalid path: contains null byte".to_string(),
        )
        .into());
    }
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
    Ok(())
}

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

/// Check if a line is an assignment line for the given variable.
///
/// Detects patterns like:
/// - Python: `var = ...`, `var: Type = ...`
/// - Go: `var := ...`, `var = ...`, `var raw = ...`
/// - Rust: `let var = ...`, `let mut var = ...`
///
/// Returns false for comparison operators (`==`, `!=`, `<=`, `>=`)
/// and compound assignments (`+=`, `-=`, `*=`, `/=`).
fn is_assignment_line(line: &str, variable: &str) -> bool {
    let trimmed = line.trim();

    // Find variable in the line
    let Some(var_pos) = trimmed.find(variable) else {
        return false;
    };

    // Check character boundaries - variable must not be part of a larger identifier
    let before_ok = var_pos == 0
        || !trimmed.as_bytes()[var_pos - 1].is_ascii_alphanumeric()
            && trimmed.as_bytes()[var_pos - 1] != b'_';
    let after_pos = var_pos + variable.len();
    let after_ok = after_pos >= trimmed.len()
        || !trimmed.as_bytes()[after_pos].is_ascii_alphanumeric()
            && trimmed.as_bytes()[after_pos] != b'_';

    if !before_ok || !after_ok {
        return false;
    }

    // Look for `=` after the variable (with possible type annotation between)
    let rest = &trimmed[after_pos..];

    // Find first `=` in rest
    let Some(eq_pos) = rest.find('=') else {
        return false;
    };

    // Check it's not `==`, `!=`, `<=`, `>=`
    let eq_abs = after_pos + eq_pos;
    if eq_abs > 0 {
        let before_eq = trimmed.as_bytes()[eq_abs - 1];
        if before_eq == b'!' || before_eq == b'<' || before_eq == b'>' || before_eq == b'=' {
            return false;
        }
    }
    // Check the character after `=` is not `=` (rules out `==`)
    let after_eq = eq_abs + 1;
    if after_eq < trimmed.len() && trimmed.as_bytes()[after_eq] == b'=' {
        return false;
    }

    // Check it's not a compound assignment (`+=`, `-=`, `*=`, `/=`)
    if eq_abs > 0 {
        let before_eq = trimmed.as_bytes()[eq_abs - 1];
        if before_eq == b'+' || before_eq == b'-' || before_eq == b'*' || before_eq == b'/' {
            return false;
        }
    }

    // Go `:=` is also assignment
    // At this point we know there's a `=` after the variable, which means assignment
    true
}

/// Check if a line can host a logpoint (is "stoppable").
///
/// A line is NOT stoppable if it's:
/// - Empty or whitespace only
/// - Comment only (`#`, `//`, `///`, `/* ... */`)
/// - Closing delimiter only (`}`, `)`, `]` with optional whitespace/semicolons)
fn is_stoppable_line(line: &str) -> bool {
    let trimmed = line.trim();

    // Empty or whitespace only
    if trimmed.is_empty() {
        return false;
    }

    // Comment only
    if trimmed.starts_with('#')
        || trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
    {
        return false;
    }

    // Closing delimiter only (with optional trailing semicolons/commas)
    let stripped = trimmed.trim_end_matches([';', ',', ' ']);
    if stripped == "}" || stripped == ")" || stripped == "]" || stripped == "})" {
        return false;
    }

    true
}

/// Find the next stoppable line after `from_line` (1-indexed).
///
/// Scans up to 5 lines forward. Returns the original line if no stoppable line is found.
fn next_stoppable_line(lines: &[&str], from_line: u32) -> u32 {
    let from_idx = from_line as usize; // from_line is 1-indexed, so from_idx points to NEXT line (0-indexed)
    let max_scan = 5;
    let end = (from_idx + max_scan).min(lines.len());

    for (idx, line) in lines.iter().enumerate().take(end).skip(from_idx) {
        if is_stoppable_line(line) {
            return (idx + 1) as u32; // Convert back to 1-indexed
        }
    }

    // No stoppable line found, return original
    from_line
}

/// Service for inspecting source files
///
/// Provides text-based analysis capabilities for finding correct metric placement.
/// All languages use the generic text-based inspection.
///
/// File content is resolved via the Virtual File System (VFS):
/// - VFS cache hit → use cached content (cloud mode)
/// - VFS cache miss → disk fallback (local daemon mode)
#[derive(Clone)]
pub struct FileInspectionService {
    /// Number of lines to include as context around target line
    context_lines: usize,
    /// Number of preview lines for file overview
    preview_lines: usize,
    /// Virtual File System for reading source files
    vfs: VfsRef,
}

impl std::fmt::Debug for FileInspectionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileInspectionService")
            .field("context_lines", &self.context_lines)
            .field("preview_lines", &self.preview_lines)
            .field("vfs", &"<VFS>")
            .finish()
    }
}

impl FileInspectionService {
    /// Create a new file inspection service with a VFS and default config
    pub fn new(vfs: VfsRef) -> Self {
        Self {
            context_lines: DEFAULT_CONTEXT_LINES,
            preview_lines: DEFAULT_PREVIEW_LINES,
            vfs,
        }
    }

    /// Create a file inspection service from API config with VFS
    pub fn from_config(context_lines: usize, preview_lines: usize, vfs: VfsRef) -> Self {
        Self {
            context_lines,
            preview_lines,
            vfs,
        }
    }

    /// Get a reference to the underlying VFS
    pub fn vfs(&self) -> &dyn detrix_ports::VirtualFileSystem {
        &*self.vfs
    }
}

impl FileInspectionService {
    /// Inspect a source file
    ///
    /// File content is resolved via VFS (cache → disk fallback).
    ///
    /// # Security
    ///
    /// For disk-backed files, the path is validated and canonicalized:
    /// - Path length limits enforced
    /// - Null bytes rejected
    /// - Path traversal (`..`) resolved via canonicalization
    /// - Symlinks resolved to their target
    ///
    /// For VFS-cached files (cloud mode), path validation is lighter since
    /// the content was already provided by the agent.
    pub fn inspect(
        &self,
        request: FileInspectionRequest,
    ) -> Result<(SourceLanguage, FileInspectionResult)> {
        // Resolve relative paths against workspace_root before validation
        let resolved_path =
            resolve_file_path(&request.file_path, request.workspace_root.as_deref());

        // Determine language from extension
        let extension = Path::new(&resolved_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let language = SourceLanguage::from_extension(&extension);

        // Try reading from VFS first (handles cache + disk fallback)
        // If VFS returns content, skip heavy disk validation (canonicalize etc.)
        // since the content is already available.
        let file_path = if self.vfs.exists(&resolved_path)? {
            // VFS has it (cached or on disk) — use resolved path directly
            // Still do lightweight validation (length, null bytes)
            validate_path_syntax(&resolved_path)?;
            resolved_path
        } else {
            // Not in VFS, try disk with full security validation
            let canonical = validate_file_path(&resolved_path)?;
            canonical.to_string_lossy().into_owned()
        };

        let validated_request = FileInspectionRequest {
            file_path,
            line: request.line,
            find_variable: request.find_variable,
            workspace_root: None,
        };

        let result = self.inspect_generic(&validated_request, language)?;
        Ok((language, result))
    }

    /// Inspect file using text-based analysis with optional tree-sitter scope analysis
    fn inspect_generic(
        &self,
        request: &FileInspectionRequest,
        language: SourceLanguage,
    ) -> Result<FileInspectionResult> {
        // Read the file via VFS (cache → disk fallback)
        let contents = self.vfs.read_to_string(&request.file_path).map_err(|e| {
            // Wrap as IoErrorWithContext for consistent error handling
            let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string());
            IoErrorWithContext {
                error: io_err,
                path: request.file_path.clone(),
            }
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
            self.inspect_generic_variable(&lines, var, total_lines, &contents, language)
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
    ///
    /// Prefers usage lines over assignment lines for logpoint placement,
    /// since logpoints fire BEFORE the line executes (so a variable isn't
    /// defined yet on its assignment line).
    fn inspect_generic_variable(
        &self,
        lines: &[&str],
        variable: &str,
        _total_lines: usize,
        contents: &str,
        language: SourceLanguage,
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

        // Partition matches into usage and assignment lines
        let mut usage_defs = Vec::new();
        let mut assignment_defs = Vec::new();

        for m in matches.iter().take(10) {
            if is_assignment_line(&m.code, variable) {
                // For assignment lines, bump to the next stoppable line
                let bumped_line = next_stoppable_line(lines, m.line_number);
                let bumped_code = if bumped_line != m.line_number {
                    lines
                        .get((bumped_line - 1) as usize)
                        .unwrap_or(&"")
                        .to_string()
                } else {
                    m.code.clone()
                };
                assignment_defs.push(VariableDefinition {
                    line: bumped_line,
                    scope: "assignment".to_string(),
                    code: bumped_code,
                });
            } else {
                usage_defs.push(VariableDefinition {
                    line: m.line_number,
                    scope: "usage".to_string(),
                    code: m.code.clone(),
                });
            }
        }

        // Usage lines first, then bumped assignment lines
        let mut definitions = usage_defs;
        definitions.extend(assignment_defs);

        // Deduplicate by line number (keep first occurrence)
        let mut seen_lines = std::collections::HashSet::new();
        definitions.retain(|d| seen_lines.insert(d.line));

        // Scope-based re-ordering: prefer lines inside function bodies over
        // struct/type definitions and function signatures.
        //
        // Three tiers (best → worst for logpoint placement):
        //   1. Function body usage — executable code inside a function
        //   2. Function signature — parameter list / return type (useless for logpoints)
        //   3. Out of scope — struct fields, package-level declarations
        if language.capabilities().has_ast_analysis && !definitions.is_empty() {
            let mut body_defs = Vec::new();
            let mut sig_defs = Vec::new();
            let mut out_defs = Vec::new();

            for def in definitions {
                let scope_result = analyze_scope(contents, def.line, language);
                if scope_result.containing_scope.is_some() && !scope_result.is_function_signature {
                    body_defs.push(def);
                } else if scope_result.is_function_signature {
                    sig_defs.push(def);
                } else {
                    out_defs.push(def);
                }
            }

            definitions = body_defs;
            definitions.extend(sig_defs);
            definitions.extend(out_defs);
        }

        let suggested_lines: Vec<u32> = definitions.iter().map(|d| d.line).collect();

        // Build context around first definition if any
        let context = if let Some(first) = definitions.first() {
            let target_idx = (first.line - 1) as usize;
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
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    use detrix_ports::VirtualFileSystem;

    /// Simple disk-only VFS for tests (no caching, just std::fs)
    struct TestDiskVfs;

    impl VirtualFileSystem for TestDiskVfs {
        fn read_to_string(&self, path: &str) -> detrix_core::Result<String> {
            std::fs::read_to_string(path)
                .map_err(|e| detrix_core::Error::FileNotFound(format!("{}: {}", path, e)))
        }
        fn exists(&self, path: &str) -> detrix_core::Result<bool> {
            Ok(std::path::Path::new(path).exists())
        }
        fn store(&self, _: &str, _: &str, _: String) {}
        fn cached_hashes(&self, _: &str) -> Vec<(String, String)> {
            vec![]
        }
        fn validate_hashes(&self, _: &str, _: &[(String, String)]) -> Vec<String> {
            vec![]
        }
        fn mark_stale(&self, _: &str) {}
        fn clear_connection(&self, _: &str) {}
    }

    fn test_service() -> FileInspectionService {
        FileInspectionService::new(Arc::new(TestDiskVfs))
    }

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

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: Some(2),
            find_variable: None,
            workspace_root: None,
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

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("userID".to_string()),
            workspace_root: None,
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

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: None,
            workspace_root: None,
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

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: Some(100),
            find_variable: None,
            workspace_root: None,
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

    // ========================================================================
    // Assignment Detection Tests
    // ========================================================================

    #[test]
    fn test_assignment_detection_python() {
        assert!(is_assignment_line("    raw = json.loads(resp)", "raw"));
        assert!(is_assignment_line(
            "    raw: dict = json.loads(resp)",
            "raw"
        ));
        assert!(is_assignment_line("x = 42", "x"));
        // Not assignments
        assert!(!is_assignment_line("    print(raw)", "raw"));
        assert!(!is_assignment_line("    if raw == expected:", "raw"));
        assert!(!is_assignment_line("    if raw != None:", "raw"));
    }

    #[test]
    fn test_assignment_detection_go() {
        assert!(is_assignment_line("    raw := json.Unmarshal(data)", "raw"));
        assert!(is_assignment_line("    raw = getData()", "raw"));
        assert!(is_assignment_line("    var raw = getData()", "raw"));
        // Not assignments
        assert!(!is_assignment_line("    fmt.Println(raw)", "raw"));
        assert!(!is_assignment_line("    if raw == nil {", "raw"));
    }

    #[test]
    fn test_assignment_detection_rust() {
        assert!(is_assignment_line(
            "    let raw = serde_json::from_str(&s);",
            "raw"
        ));
        assert!(is_assignment_line("    let mut raw = Vec::new();", "raw"));
        // Not assignments
        assert!(!is_assignment_line("    process(raw);", "raw"));
        assert!(!is_assignment_line("    if raw == expected {", "raw"));
    }

    #[test]
    fn test_assignment_detection_compound_operators() {
        assert!(!is_assignment_line("    count += 1", "count"));
        assert!(!is_assignment_line("    total -= amount", "total"));
        assert!(!is_assignment_line("    result *= factor", "result"));
        assert!(!is_assignment_line("    value /= divisor", "value"));
    }

    #[test]
    fn test_assignment_detection_comparison() {
        assert!(!is_assignment_line("    if x == 5:", "x"));
        assert!(!is_assignment_line("    if x != 5:", "x"));
        assert!(!is_assignment_line("    if x <= 5:", "x"));
        assert!(!is_assignment_line("    if x >= 5:", "x"));
    }

    #[test]
    fn test_assignment_no_partial_match() {
        // "raw" should not match "raw_data"
        assert!(!is_assignment_line("    raw_data = 42", "raw"));
        // "x" should not match "extra"
        assert!(!is_assignment_line("    extra = 42", "x"));
    }

    // ========================================================================
    // Stoppable Line Detection Tests
    // ========================================================================

    #[test]
    fn test_stoppable_line_detection() {
        // Stoppable lines
        assert!(is_stoppable_line("    x = 42"));
        assert!(is_stoppable_line("    return normalize(raw)"));
        assert!(is_stoppable_line("    print(result)"));
        assert!(is_stoppable_line("    if x > 0:"));

        // NOT stoppable
        assert!(!is_stoppable_line(""));
        assert!(!is_stoppable_line("   "));
        assert!(!is_stoppable_line("# comment"));
        assert!(!is_stoppable_line("// comment"));
        assert!(!is_stoppable_line("/// doc comment"));
        assert!(!is_stoppable_line("/* block comment */"));
        assert!(!is_stoppable_line("}"));
        assert!(!is_stoppable_line("    }"));
        assert!(!is_stoppable_line("    )"));
        assert!(!is_stoppable_line("    ]"));
        assert!(!is_stoppable_line("    };"));
        assert!(!is_stoppable_line("    })"));
    }

    // ========================================================================
    // Variable Search Smart Line Suggestion Tests
    // ========================================================================

    #[test]
    fn test_variable_search_prefers_usage() {
        // File where variable appears in both assignment and usage
        let mut file = NamedTempFile::with_suffix(".py").unwrap();
        writeln!(file, "def process():").unwrap();
        writeln!(file, "    raw = json.loads(resp)").unwrap();
        writeln!(file, "    return normalize(raw)").unwrap();

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("raw".to_string()),
            workspace_root: None,
        };

        let (_lang, result) = service.inspect(request).unwrap();
        if let FileInspectionResult::VariableSearch(search) = result {
            assert!(!search.definitions.is_empty());
            // First definition should be usage line (line 3), not assignment line (line 2)
            assert_eq!(search.definitions[0].line, 3);
            assert_eq!(search.definitions[0].scope, "usage");
        } else {
            panic!("Expected VariableSearch result");
        }
    }

    #[test]
    fn test_variable_search_bumps_assignment() {
        // File where variable only appears in assignment
        let mut file = NamedTempFile::with_suffix(".py").unwrap();
        writeln!(file, "def process():").unwrap();
        writeln!(file, "    raw = json.loads(resp)").unwrap();
        writeln!(file, "    return normalize(data)").unwrap();

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("raw".to_string()),
            workspace_root: None,
        };

        let (_lang, result) = service.inspect(request).unwrap();
        if let FileInspectionResult::VariableSearch(search) = result {
            assert!(!search.definitions.is_empty());
            // Assignment at line 2 should be bumped to line 3 (next stoppable line)
            assert_eq!(search.definitions[0].line, 3);
            assert_eq!(search.definitions[0].scope, "assignment");
        } else {
            panic!("Expected VariableSearch result");
        }
    }

    #[test]
    fn test_variable_search_skips_blanks_and_braces() {
        // Assignment followed by blank line, then closing brace, then real code
        let mut file = NamedTempFile::with_suffix(".py").unwrap();
        writeln!(file, "def process():").unwrap();
        writeln!(file, "    raw = json.loads(resp)").unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, "    # comment line").unwrap();
        writeln!(file, "    return normalize(data)").unwrap();

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("raw".to_string()),
            workspace_root: None,
        };

        let (_lang, result) = service.inspect(request).unwrap();
        if let FileInspectionResult::VariableSearch(search) = result {
            assert!(!search.definitions.is_empty());
            // Assignment at line 2, blank at 3, comment at 4, stoppable at 5
            assert_eq!(search.definitions[0].line, 5);
        } else {
            panic!("Expected VariableSearch result");
        }
    }

    // ========================================================================
    // Path Resolution Tests
    // ========================================================================

    #[test]
    fn test_resolve_file_path_absolute() {
        let result = resolve_file_path("/absolute/path/file.py", Some("/workspace"));
        assert_eq!(result, "/absolute/path/file.py");
    }

    #[test]
    fn test_resolve_file_path_relative_with_workspace() {
        // Create a temp file to resolve against
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        std::fs::write(&file_path, "x = 1").unwrap();

        let result = resolve_file_path("test.py", Some(&dir.path().to_string_lossy()));
        assert_eq!(result, file_path.to_string_lossy());
    }

    #[test]
    fn test_resolve_file_path_relative_no_workspace() {
        let result = resolve_file_path("test.py", None);
        assert_eq!(result, "test.py");
    }

    #[test]
    fn test_resolve_file_path_relative_not_found_in_workspace() {
        let result = resolve_file_path("nonexistent.py", Some("/tmp"));
        // Always resolves against workspace_root (file may be in remote container)
        assert_eq!(result, "/tmp/nonexistent.py");
    }

    #[test]
    fn test_resolve_file_path_subdirectory() {
        // Create nested directory structure
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let file_path = sub.join("app.py");
        std::fs::write(&file_path, "x = 1").unwrap();

        let result = resolve_file_path("sub/app.py", Some(&dir.path().to_string_lossy()));
        assert_eq!(result, file_path.to_string_lossy());
    }

    // ========================================================================
    // Full inspect() integration with workspace_root
    // ========================================================================

    #[test]
    fn test_inspect_with_workspace_root_relative_path() {
        // Create a temp file in a temp directory
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.py");
        std::fs::write(&file_path, "x = 42\nprint(x)\n").unwrap();

        let service = test_service();
        // Use relative filename with workspace_root
        let request = FileInspectionRequest {
            file_path: "test.py".to_string(),
            line: Some(1),
            find_variable: None,
            workspace_root: Some(dir.path().to_string_lossy().to_string()),
        };

        let (lang, result) = service.inspect(request).unwrap();
        assert_eq!(lang, SourceLanguage::Python);
        if let FileInspectionResult::LineInspection(inspection) = result {
            assert_eq!(inspection.target_line, 1);
            assert!(inspection.code_at_line.contains("x = 42"));
        } else {
            panic!("Expected LineInspection result");
        }
    }

    #[test]
    fn test_inspect_with_workspace_root_variable_search() {
        // Create a file with a variable
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("weather.py");
        std::fs::write(
            &file_path,
            "raw = json.loads(resp)\nreturn normalize(raw)\n",
        )
        .unwrap();

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: "weather.py".to_string(),
            line: None,
            find_variable: Some("raw".to_string()),
            workspace_root: Some(dir.path().to_string_lossy().to_string()),
        };

        let (_lang, result) = service.inspect(request).unwrap();
        if let FileInspectionResult::VariableSearch(search) = result {
            assert_eq!(search.variable_name, "raw");
            assert!(!search.definitions.is_empty());
            // Usage line should be preferred
            assert_eq!(search.definitions[0].line, 2);
        } else {
            panic!("Expected VariableSearch result");
        }
    }

    #[test]
    fn test_inspect_relative_path_without_workspace_fails() {
        let service = test_service();
        let request = FileInspectionRequest {
            file_path: "nonexistent_relative.py".to_string(),
            line: Some(1),
            find_variable: None,
            workspace_root: None,
        };

        let result = service.inspect(request);
        assert!(result.is_err());
    }

    // ========================================================================
    // Scope-aware Variable Search Tests
    // ========================================================================

    #[test]
    fn test_variable_search_go_struct_field_deprioritized() {
        // Go file where `Amount` appears as struct field (line 3) and in function body (line 8)
        // The struct field is NOT in scope — it's a type definition, not a variable.
        let mut file = NamedTempFile::with_suffix(".go").unwrap();
        writeln!(file, "package main").unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, "type Transaction struct {{").unwrap();
        writeln!(file, "	Amount float64").unwrap();
        writeln!(file, "}}").unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, "func process(txn Transaction) {{").unwrap();
        writeln!(file, "	fmt.Println(txn.Amount)").unwrap();
        writeln!(file, "}}").unwrap();

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("Amount".to_string()),
            workspace_root: None,
        };

        let (_lang, result) = service.inspect(request).unwrap();
        if let FileInspectionResult::VariableSearch(search) = result {
            assert!(!search.definitions.is_empty());
            // The function body line (8) should come first because `txn` is in scope there
            // The struct field line (4) should be deprioritized (no variables in scope)
            assert_eq!(
                search.definitions[0].line, 8,
                "Expected function body line first, got line {} (scope: {})",
                search.definitions[0].line, search.definitions[0].scope
            );
        } else {
            panic!("Expected VariableSearch result");
        }
    }

    #[test]
    fn test_variable_search_rust_struct_field_deprioritized() {
        // Rust file where `amount` appears as struct field and in function body
        let mut file = NamedTempFile::with_suffix(".rs").unwrap();
        writeln!(file, "struct Transaction {{").unwrap();
        writeln!(file, "    amount: f64,").unwrap();
        writeln!(file, "}}").unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, "fn process(txn: Transaction) {{").unwrap();
        writeln!(file, "    println!(\"{{:?}}\", txn.amount);").unwrap();
        writeln!(file, "}}").unwrap();

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("amount".to_string()),
            workspace_root: None,
        };

        let (_lang, result) = service.inspect(request).unwrap();
        if let FileInspectionResult::VariableSearch(search) = result {
            assert!(!search.definitions.is_empty());
            // Function body line (6) should come first — `txn` is in scope
            // Struct field line (2) should be deprioritized
            assert_eq!(
                search.definitions[0].line, 6,
                "Expected function body line first, got line {} (scope: {})",
                search.definitions[0].line, search.definitions[0].scope
            );
        } else {
            panic!("Expected VariableSearch result");
        }
    }

    #[test]
    fn test_variable_search_python_scope_aware() {
        // Python file where variable appears inside a function — should stay in scope
        let mut file = NamedTempFile::with_suffix(".py").unwrap();
        writeln!(file, "# module level comment about amount").unwrap();
        writeln!(file, "def process(txn):").unwrap();
        writeln!(file, "    amount = txn.amount").unwrap();
        writeln!(file, "    print(amount)").unwrap();

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("amount".to_string()),
            workspace_root: None,
        };

        let (_lang, result) = service.inspect(request).unwrap();
        if let FileInspectionResult::VariableSearch(search) = result {
            assert!(!search.definitions.is_empty());
            // Usage line (4) should come first, assignment bumped line next
            // Comment line (1) should be deprioritized (not in scope)
            assert_eq!(
                search.definitions[0].line, 4,
                "Expected usage line first, got line {} (scope: {})",
                search.definitions[0].line, search.definitions[0].scope
            );
        } else {
            panic!("Expected VariableSearch result");
        }
    }

    #[test]
    fn test_variable_search_no_ast_language_unchanged() {
        // Java (no AST analysis) — behavior should be unchanged (usage-first ordering)
        let mut file = NamedTempFile::with_suffix(".java").unwrap();
        writeln!(file, "class Txn {{").unwrap();
        writeln!(file, "    double amount;").unwrap();
        writeln!(file, "    void process() {{").unwrap();
        writeln!(file, "        System.out.println(amount);").unwrap();
        writeln!(file, "    }}").unwrap();
        writeln!(file, "}}").unwrap();

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("amount".to_string()),
            workspace_root: None,
        };

        let (_lang, result) = service.inspect(request).unwrap();
        if let FileInspectionResult::VariableSearch(search) = result {
            assert!(!search.definitions.is_empty());
            // Both lines are "usage" (no `=`), so order should be original order
            assert_eq!(search.definitions[0].line, 2);
        } else {
            panic!("Expected VariableSearch result");
        }
    }

    #[test]
    fn test_go_scope_aware_struct_and_func_header_deprioritized() {
        // Go file with three places "symbol" appears:
        //   1. Struct field definition (line 3)  — out of scope
        //   2. Function signature/header (line 6) — useless for logpoints
        //   3. Function body usage (line 11)      — best for logpoints
        // The inspector should pick the function body usage first.
        let mut file = NamedTempFile::with_suffix(".go").unwrap();
        writeln!(file, "package main").unwrap(); // 1
        writeln!(file, "type Order struct {{").unwrap(); // 2
        writeln!(file, "    symbol string").unwrap(); // 3
        writeln!(file, "    price  float64").unwrap(); // 4
        writeln!(file, "}}").unwrap(); // 5
        writeln!(file, "func placeOrder(symbol string) int {{").unwrap(); // 6
        writeln!(file, "    return 42").unwrap(); // 7
        writeln!(file, "}}").unwrap(); // 8
        writeln!(file, "func main() {{").unwrap(); // 9
        writeln!(file, "    symbols := []string{{\"BTC\"}}").unwrap(); // 10
        writeln!(file, "    symbol := symbols[0]").unwrap(); // 11
        writeln!(file, "    _ = symbol").unwrap(); // 12
        writeln!(file, "}}").unwrap(); // 13

        let service = test_service();
        let request = FileInspectionRequest {
            file_path: file.path().to_string_lossy().to_string(),
            line: None,
            find_variable: Some("symbol".to_string()),
            workspace_root: None,
        };

        let (lang, result) = service.inspect(request).unwrap();
        assert_eq!(lang, SourceLanguage::Go);

        if let FileInspectionResult::VariableSearch(search) = result {
            assert!(
                search.definitions.len() >= 3,
                "Expected at least 3 matches, got {}",
                search.definitions.len()
            );

            // First definition should be in main() body (line 12 = usage of symbol,
            // or bumped line from assignment at line 11).
            // It must NOT be the struct field (line 3) or func header (line 6).
            let first = &search.definitions[0];
            assert!(
                first.line >= 9,
                "First match should be inside main() (line >= 9), got line {} (code: {})",
                first.line,
                first.code
            );

            // Struct field (line 3) and func header (line 6) should be after body matches
            let struct_pos = search
                .definitions
                .iter()
                .position(|d| d.line == 3)
                .expect("struct field should be in results");
            let header_pos = search
                .definitions
                .iter()
                .position(|d| d.line == 6)
                .expect("func header should be in results");
            let body_pos = search
                .definitions
                .iter()
                .position(|d| d.line >= 9)
                .expect("body usage should be in results");

            assert!(
                body_pos < header_pos,
                "body usage (pos {}) should come before func header (pos {})",
                body_pos,
                header_pos
            );
            assert!(
                body_pos < struct_pos,
                "body usage (pos {}) should come before struct field (pos {})",
                body_pos,
                struct_pos
            );
        } else {
            panic!("Expected VariableSearch result");
        }
    }
}
