//! File Inspection Types
//!
//! Domain types used by the FileInspectionService for code analysis.
//! These are use-case-specific DTOs that belong in the application layer.
//!
//! Note: `SourceLanguage` is re-exported from `detrix-core` as it's a domain concept
//! used by `Metric` and `Connection` entities.

use serde::{Deserialize, Serialize};

// Re-export SourceLanguage from detrix-core for backward compatibility
pub use detrix_core::SourceLanguage;

// ============================================================================
// Language Capabilities (application-layer extension of SourceLanguage)
// ============================================================================

/// Extension trait for SourceLanguage to provide application-layer capabilities
pub trait SourceLanguageExt {
    /// Check if full AST analysis is supported
    fn supports_ast_analysis(&self) -> bool;

    /// Get comprehensive language capabilities
    ///
    /// This is the **single source of truth** for language feature support.
    /// All code should query capabilities through this method.
    fn capabilities(&self) -> LanguageCapabilities;
}

impl SourceLanguageExt for SourceLanguage {
    fn supports_ast_analysis(&self) -> bool {
        self.capabilities().has_ast_analysis
    }

    fn capabilities(&self) -> LanguageCapabilities {
        match self {
            Self::Python => LanguageCapabilities {
                has_ast_analysis: true,
                has_scope_validation: true,
                has_safety_validator: true,
                has_lsp_purity: true,
                has_dap_adapter: true,
                has_introspection: true,
                dap_adapter_name: Some("debugpy"),
                lsp_server_name: Some("pylsp"),
            },
            Self::Go => LanguageCapabilities {
                has_ast_analysis: true,
                has_scope_validation: true,
                has_safety_validator: true,
                has_lsp_purity: true,
                has_dap_adapter: true,
                has_introspection: true,
                dap_adapter_name: Some("dlv"),
                lsp_server_name: Some("gopls"),
            },
            Self::Rust => LanguageCapabilities {
                has_ast_analysis: true,
                has_scope_validation: true,
                has_safety_validator: true,
                has_lsp_purity: true,
                has_dap_adapter: true,
                has_introspection: true,
                dap_adapter_name: Some("lldb-dap"),
                lsp_server_name: Some("rust-analyzer"),
            },
            // TODO: Add new language support here
            // When implementing JavaScript/TypeScript or other languages, add a match arm like:
            //   Self::JavaScript | Self::TypeScript => LanguageCapabilities { ... }
            // See ADD_LANGUAGE.md for full checklist of places to update.
            _ => LanguageCapabilities::default(),
        }
    }
}

/// Language capabilities - feature support
///
/// This struct defines what features are available for each language.
/// Use `SourceLanguage::capabilities()` to get these for any language.
#[derive(Debug, Clone, Default)]
pub struct LanguageCapabilities {
    /// Whether AST analysis is supported (using tree-sitter in-process)
    pub has_ast_analysis: bool,
    /// Whether scope validation is supported (checking if variables are in scope)
    pub has_scope_validation: bool,
    /// Whether expression safety validation is implemented
    pub has_safety_validator: bool,
    /// Whether LSP-based purity analysis is implemented
    pub has_lsp_purity: bool,
    /// Whether DAP adapter is available for debugging
    pub has_dap_adapter: bool,
    /// Whether introspection (variable inspection) is supported
    pub has_introspection: bool,
    /// Name of the DAP adapter binary (e.g., "debugpy", "dlv")
    pub dap_adapter_name: Option<&'static str>,
    /// Name of the LSP server (e.g., "pylsp", "gopls")
    pub lsp_server_name: Option<&'static str>,
}

// ============================================================================
// Code Context Types
// ============================================================================

/// A single line of code with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeLine {
    /// 1-based line number
    pub line_number: u32,
    /// The code content
    pub code: String,
    /// Whether this is the target/highlighted line
    pub is_target: bool,
}

/// Context around a specific line in a file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeContext {
    /// Starting line number
    pub start_line: u32,
    /// Lines with their content
    pub lines: Vec<CodeLine>,
}

// ============================================================================
// Variable and Line Inspection Results
// ============================================================================

/// Variable definition location
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableDefinition {
    /// Line number where defined
    pub line: u32,
    /// Scope name (e.g., function name, class name, "module")
    pub scope: String,
    /// The code at definition
    pub code: String,
}

/// Result of finding a variable in a file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableSearchResult {
    /// Variable name searched for
    pub variable_name: String,
    /// Found definitions (empty if not found)
    pub definitions: Vec<VariableDefinition>,
    /// Suggested lines for metric placement
    pub suggested_lines: Vec<u32>,
    /// Code context around the first definition
    pub context: Option<CodeContext>,
    /// Similar variable names (if exact match not found)
    pub similar_variables: Vec<String>,
}

/// Result of inspecting variables at a specific line
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineInspectionResult {
    /// Target line number
    pub target_line: u32,
    /// Code at the target line
    pub code_at_line: String,
    /// Variables available at this line
    pub available_variables: Vec<String>,
    /// Code context around the line
    pub context: CodeContext,
}

/// Overview of a file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOverview {
    /// Total number of lines
    pub total_lines: u32,
    /// Preview of first N lines
    pub preview_lines: Vec<CodeLine>,
}

/// Generic file search result (text-based, not AST)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSearchMatch {
    /// Line number where found
    pub line_number: u32,
    /// The code at that line
    pub code: String,
}

// ============================================================================
// File Inspection Request/Response
// ============================================================================

/// Result of file inspection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileInspectionResult {
    /// Found variable definitions (Python AST or generic text search)
    VariableSearch(VariableSearchResult),
    /// Inspected a specific line
    LineInspection(LineInspectionResult),
    /// File overview (no specific line or variable)
    Overview(FileOverview),
}

/// Request for file inspection
#[derive(Debug, Clone)]
pub struct FileInspectionRequest {
    /// Path to the file
    pub file_path: String,
    /// Optional line number to inspect
    pub line: Option<u32>,
    /// Optional variable to find
    pub find_variable: Option<String>,
}
