//! LSP-based purity analysis configuration

use crate::constants::{DEFAULT_ANALYSIS_TIMEOUT_MS, DEFAULT_MAX_CALL_DEPTH};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// LSP-based purity analysis configuration
///
/// Configures the Language Server Protocol client for analyzing
/// user-defined functions to determine if they have side effects.
///
/// Each language safety config provides its own LspConfig with appropriate defaults:
/// - Python: pyright-langserver (in PythonSafetyConfig.lsp)
/// - Go: gopls (in GoSafetyConfig.lsp)
/// - Rust: rust-analyzer (in RustSafetyConfig.lsp)
///
/// The generic Default impl leaves command empty - each language must provide its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspConfig {
    /// Enable LSP-based purity analysis
    #[serde(default)]
    pub enabled: bool,

    /// Command to start the LSP server (e.g., "pyright-langserver", "gopls", "rust-analyzer")
    /// Each language provides its own default via `default_{lang}_lsp_config()`
    #[serde(default)]
    pub command: String,

    /// Arguments for the LSP server command
    #[serde(default)]
    pub args: Vec<String>,

    /// Maximum recursion depth for call hierarchy traversal
    #[serde(default = "default_max_call_depth")]
    pub max_call_depth: usize,

    /// Timeout in milliseconds for single function analysis
    #[serde(default = "default_analysis_timeout")]
    pub analysis_timeout_ms: u64,

    /// Enable caching of purity analysis results
    #[serde(default = "default_cache_enabled")]
    pub cache_enabled: bool,

    /// Working directory for LSP server (defaults to project root)
    #[serde(default)]
    pub working_directory: Option<PathBuf>,
}

fn default_max_call_depth() -> usize {
    DEFAULT_MAX_CALL_DEPTH
}

fn default_analysis_timeout() -> u64 {
    DEFAULT_ANALYSIS_TIMEOUT_MS
}

fn default_cache_enabled() -> bool {
    true
}

impl Default for LspConfig {
    /// Default LSP configuration - language-agnostic base
    /// Each language's safety config provides appropriate command via `default_{lang}_lsp_config()`
    fn default() -> Self {
        Self {
            enabled: false,
            command: String::new(), // Set by language-specific defaults
            args: Vec::new(),
            max_call_depth: DEFAULT_MAX_CALL_DEPTH,
            analysis_timeout_ms: DEFAULT_ANALYSIS_TIMEOUT_MS,
            cache_enabled: true,
            working_directory: None,
        }
    }
}

impl LspConfig {
    /// Create config for pyright (default, best call hierarchy support)
    pub fn pyright() -> Self {
        Self {
            enabled: true,
            command: "pyright-langserver".to_string(),
            args: vec!["--stdio".to_string()],
            ..Default::default()
        }
    }

    /// Create config for pylsp (fallback, limited call hierarchy support)
    pub fn pylsp() -> Self {
        Self {
            enabled: true,
            command: "pylsp".to_string(),
            args: Vec::new(),
            ..Default::default()
        }
    }

    /// Create config for jedi-language-server
    pub fn jedi() -> Self {
        Self {
            enabled: true,
            command: "jedi-language-server".to_string(),
            args: Vec::new(),
            ..Default::default()
        }
    }
}
