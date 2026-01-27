//! Safety configuration types for expression validation
//!
//! Contains language-specific safety settings including:
//! - Python safety config with AST analysis
//! - Go safety config with prohibited functions
//! - Rust safety config with unsafe detection
//! - LSP-based purity analysis settings
//!
//! # Function Classification Constants
//!
//! This module exports function classification constants for all supported languages,
//! used by both tree-sitter validation and LSP purity analysis:
//!
//! ## Rust
//! - `RUST_ACCEPTABLE_IMPURE`: I/O but safe for metrics (logging)
//! - `RUST_PURE_FUNCTIONS`: No side effects
//! - `RUST_IMPURE_FUNCTIONS`: Side effects that matter
//! - `RUST_MUTATION_METHODS`: Scope-aware mutation detection
//! - `RUST_LLDB_TYPE_FORMATTERS`: LLDB type summary commands for readable Rust types
//!
//! ## Go
//! - `GO_ACCEPTABLE_IMPURE`, `GO_PURE_FUNCTIONS`, `GO_IMPURE_FUNCTIONS`
//! - `GO_MUTATION_OPERATIONS`, `GO_RECEIVER_MUTATION_METHODS`
//!
//! ## Python
//! - `PYTHON_ACCEPTABLE_IMPURE`, `PYTHON_PURE_FUNCTIONS`, `PYTHON_IMPURE_FUNCTIONS`
//! - `PYTHON_MUTATION_METHODS`

mod go;
mod lsp;
mod python;
mod rust;

// Re-export language configs
pub use go::GoSafetyConfig;
pub use lsp::LspConfig;
pub use python::PythonSafetyConfig;
pub use rust::RustSafetyConfig;

// Re-export Rust function classification constants
pub use rust::{
    RUST_ACCEPTABLE_IMPURE, RUST_IMPURE_FUNCTIONS, RUST_LLDB_TYPE_FORMATTERS,
    RUST_MUTATION_METHODS, RUST_PURE_FUNCTIONS,
};

// Re-export Go function classification constants
pub use go::{
    GO_ACCEPTABLE_IMPURE, GO_IMPURE_FUNCTIONS, GO_MUTATION_OPERATIONS, GO_PURE_FUNCTIONS,
    GO_RECEIVER_MUTATION_METHODS,
};

// Re-export Python function classification constants
pub use python::{
    PYTHON_ACCEPTABLE_IMPURE, PYTHON_IMPURE_FUNCTIONS, PYTHON_MUTATION_METHODS,
    PYTHON_PURE_FUNCTIONS,
};

use crate::constants::{
    DEFAULT_MAX_MEMORY_MB, DEFAULT_MAX_RECURSION_DEPTH, DEFAULT_WHITELIST_MODE,
};
use serde::{Deserialize, Serialize};

// ============================================================================
// Safety Config
// ============================================================================

/// Global safety configuration
///
/// Contains general settings and language-specific configurations.
/// Each language has its own section for allowed/prohibited functions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    /// Enable AST-based analysis (recommended)
    #[serde(default = "default_true")]
    pub enable_ast_analysis: bool,

    /// Whitelist mode: "strict" (only allowed), "trusted" (allow custom)
    #[serde(default = "default_whitelist_mode")]
    pub whitelist_mode: String,

    /// Maximum recursion depth for expression evaluation
    #[serde(default = "default_max_recursion")]
    pub max_recursion_depth: u32,

    /// Maximum memory usage in MB
    #[serde(default = "default_max_memory_mb")]
    pub max_memory_mb: u32,

    /// Maximum entries in the AST parse cache for tree-sitter analysis
    #[serde(default = "default_ast_cache_max_entries")]
    pub ast_cache_max_entries: usize,

    // Language-specific configurations
    /// Python-specific safety settings (includes LSP config at python.lsp)
    #[serde(default)]
    pub python: PythonSafetyConfig,
    /// Go-specific safety settings (includes LSP config at go.lsp)
    #[serde(default)]
    pub go: GoSafetyConfig,
    /// Rust-specific safety settings (includes LSP config at rust.lsp)
    #[serde(default)]
    pub rust: RustSafetyConfig,
}

fn default_true() -> bool {
    true
}

fn default_whitelist_mode() -> String {
    DEFAULT_WHITELIST_MODE.to_string()
}

fn default_max_recursion() -> u32 {
    DEFAULT_MAX_RECURSION_DEPTH
}

fn default_max_memory_mb() -> u32 {
    DEFAULT_MAX_MEMORY_MB
}

fn default_ast_cache_max_entries() -> usize {
    crate::constants::DEFAULT_AST_CACHE_MAX_ENTRIES
}

impl Default for SafetyConfig {
    fn default() -> Self {
        SafetyConfig {
            enable_ast_analysis: true,
            whitelist_mode: DEFAULT_WHITELIST_MODE.to_string(),
            max_recursion_depth: DEFAULT_MAX_RECURSION_DEPTH,
            max_memory_mb: DEFAULT_MAX_MEMORY_MB,
            ast_cache_max_entries: default_ast_cache_max_entries(),
            python: PythonSafetyConfig::default(),
            go: GoSafetyConfig::default(),
            rust: RustSafetyConfig::default(),
        }
    }
}

impl SafetyConfig {
    /// Get the configured default safety level from whitelist_mode
    ///
    /// Returns the SafetyLevel corresponding to the configured whitelist_mode:
    /// - "strict" → SafetyLevel::Strict
    /// - "trusted" → SafetyLevel::Trusted
    ///
    /// Falls back to SafetyLevel::Strict for unknown values.
    pub fn default_safety_level(&self) -> detrix_core::SafetyLevel {
        self.whitelist_mode
            .parse()
            .unwrap_or(detrix_core::SafetyLevel::Strict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detrix_core::SafetyLevel;

    #[test]
    fn test_safety_config_sets() {
        let safety = SafetyConfig::default();
        let allowed = safety.python.allowed_set();
        let prohibited = safety.python.prohibited_set();

        assert!(allowed.contains("len"));
        assert!(allowed.contains("str"));
        assert!(prohibited.contains("eval"));
        assert!(prohibited.contains("exec"));
    }

    #[test]
    fn test_default_safety_level_strict() {
        let safety = SafetyConfig::default();
        assert_eq!(safety.default_safety_level(), SafetyLevel::Strict);
    }

    #[test]
    fn test_default_safety_level_trusted() {
        let safety = SafetyConfig {
            whitelist_mode: "trusted".to_string(),
            ..Default::default()
        };
        assert_eq!(safety.default_safety_level(), SafetyLevel::Trusted);
    }

    #[test]
    fn test_default_safety_level_unknown_falls_back_to_strict() {
        let safety = SafetyConfig {
            whitelist_mode: "invalid".to_string(),
            ..Default::default()
        };
        assert_eq!(safety.default_safety_level(), SafetyLevel::Strict);
    }
}
