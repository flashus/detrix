//! Purity Analyzer Port
//!
//! Defines the trait for analyzing function purity via LSP.
//! This enables detection of side effects in user-defined functions
//! that are not in the whitelist/blacklist.
//!
//! Per Clean Architecture, this output port belongs in the Application layer.
//! Infrastructure adapters (detrix-lsp) implement this trait.

use async_trait::async_trait;
use detrix_core::{PurityAnalysis, Result, SourceLanguage};
use std::path::Path;

/// Trait for analyzing function purity via LSP
///
/// This trait abstracts the interaction with Language Server Protocol (LSP)
/// to determine whether user-defined functions have side effects.
///
/// Implementations use LSP's Call Hierarchy feature to trace function calls
/// and classify them as pure, unknown, or impure.
#[async_trait]
pub trait PurityAnalyzer: Send + Sync {
    /// Analyze the purity of a function by name
    ///
    /// Uses LSP's Call Hierarchy to recursively trace all function calls
    /// and classify them based on whitelist/blacklist rules.
    ///
    /// # Arguments
    /// * `function_name` - The function to analyze (e.g., "user.get_data")
    /// * `file_path` - The source file containing the function
    ///
    /// # Returns
    /// * `Ok(PurityAnalysis)` - Analysis result with purity level and details
    /// * `Err(Error)` - If LSP communication fails
    async fn analyze_function(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> Result<PurityAnalysis>;

    /// Check if the analyzer is available (LSP running)
    ///
    /// Returns true if the LSP server is started and responsive.
    /// This is a non-blocking check.
    fn is_available(&self) -> bool;

    /// Ensure the analyzer is ready, starting LSP if needed
    ///
    /// This method handles lazy-start of the LSP server:
    /// - If already running, returns immediately
    /// - If not started, spawns the LSP server and initializes
    /// - Returns error if server fails to start
    async fn ensure_ready(&self) -> Result<()>;

    /// Get the language this analyzer supports
    fn language(&self) -> SourceLanguage;
}
