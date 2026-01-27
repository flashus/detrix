//! Anchor Service Port
//!
//! Defines the interface for metric location anchor operations.
//! Anchors enable metrics to "follow" code changes like print statements do.
//!
//! # Architecture
//!
//! The AnchorService is responsible for:
//! 1. **Capture**: Capturing anchor data when a metric is created
//! 2. **Verify**: Verifying an anchor is still valid at its current location
//! 3. **Relocate**: Finding a new location when the original becomes invalid
//!
//! # Fallback Chain
//!
//! Relocation uses a multi-tier fallback:
//! 1. Exact line match (unchanged)
//! 2. Symbol + offset (function moved, metric follows)
//! 3. Context fingerprint (fuzzy matching)
//! 4. Orphaned (manual intervention needed)

use async_trait::async_trait;
use detrix_core::entities::{Location, MetricAnchor, RelocationResult};
use detrix_core::error::Result;
use detrix_core::SourceLanguage;

/// Captured symbol information from LSP
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    /// Name of the enclosing symbol (function/method)
    pub name: String,
    /// LSP SymbolKind (12=Function, 6=Method, 5=Class)
    pub kind: u32,
    /// Start line of the symbol (1-indexed)
    pub start_line: u32,
    /// End line of the symbol (1-indexed)
    pub end_line: u32,
}

/// Context lines around a location
#[derive(Debug, Clone)]
pub struct SourceContext {
    /// Lines before the target (typically 2-3 lines)
    pub before: String,
    /// The target line itself
    pub line: String,
    /// Lines after the target (typically 2-3 lines)
    pub after: String,
}

/// Service for capturing and managing metric location anchors
///
/// This trait defines the port for anchor operations. Implementations
/// should integrate with LSP for symbol lookup and file system for
/// context reading.
#[async_trait]
pub trait AnchorService: Send + Sync {
    /// Capture anchor data for a location
    ///
    /// This captures both symbol information (via LSP) and context fingerprint
    /// (via file reading) for the given location.
    ///
    /// # Arguments
    /// * `location` - File path and line number
    /// * `language` - Source language for LSP server selection
    ///
    /// # Returns
    /// A populated `MetricAnchor` with symbol and context data, or an error.
    /// If LSP is not available, only context fingerprint will be captured.
    async fn capture_anchor(
        &self,
        location: &Location,
        language: SourceLanguage,
    ) -> Result<MetricAnchor>;

    /// Verify an anchor at its current location
    ///
    /// Checks if the metric's anchor data still matches the code at its
    /// recorded location.
    ///
    /// # Arguments
    /// * `location` - Current location of the metric
    /// * `anchor` - Existing anchor data to verify
    /// * `language` - Source language
    ///
    /// # Returns
    /// `true` if the anchor matches the current code, `false` otherwise.
    async fn verify_anchor(
        &self,
        location: &Location,
        anchor: &MetricAnchor,
        language: SourceLanguage,
    ) -> Result<bool>;

    /// Attempt to relocate a metric using its anchor
    ///
    /// Uses the multi-tier fallback chain to find the new location:
    /// 1. Symbol + offset (find same function, apply offset)
    /// 2. Context fingerprint (sliding window match)
    /// 3. Orphaned (could not relocate)
    ///
    /// # Arguments
    /// * `file` - Path to the file to search
    /// * `anchor` - Anchor data to use for relocation
    /// * `language` - Source language
    ///
    /// # Returns
    /// A `RelocationResult` indicating success or failure mode.
    async fn relocate(
        &self,
        file: &str,
        anchor: &MetricAnchor,
        language: SourceLanguage,
    ) -> Result<RelocationResult>;

    /// Read source context around a line
    ///
    /// Reads the source file and extracts context lines around the target.
    /// Used for fingerprint computation and display.
    ///
    /// # Arguments
    /// * `file` - Path to the source file
    /// * `line` - Target line number (1-indexed)
    /// * `context_lines` - Number of lines to capture before/after
    /// * `max_bytes` - Maximum bytes per context field
    ///
    /// # Returns
    /// Source context with before/line/after content.
    async fn read_context(
        &self,
        file: &str,
        line: u32,
        context_lines: usize,
        max_bytes: usize,
    ) -> Result<SourceContext>;

    /// Get enclosing symbol at a location
    ///
    /// Uses LSP to find the function/method containing the given position.
    ///
    /// # Arguments
    /// * `file` - Path to the source file
    /// * `line` - Line number (1-indexed)
    /// * `language` - Source language for LSP selection
    ///
    /// # Returns
    /// Symbol information if found, `None` if no enclosing symbol or LSP unavailable.
    async fn get_enclosing_symbol(
        &self,
        file: &str,
        line: u32,
        language: SourceLanguage,
    ) -> Result<Option<SymbolInfo>>;

    /// Compute fingerprint for given context
    ///
    /// Computes SHA256 hash of the concatenated context strings.
    /// This provides a stable identifier for matching across file changes.
    ///
    /// # Arguments
    /// * `context` - The source context to fingerprint
    /// * `language` - Optional source language for comment stripping (if strip_comments enabled)
    fn compute_fingerprint(
        &self,
        context: &SourceContext,
        language: Option<SourceLanguage>,
    ) -> String;

    /// Calculate similarity between two context strings
    ///
    /// Uses Levenshtein distance to compute normalized similarity.
    /// Returns value between 0.0 (completely different) and 1.0 (identical).
    ///
    /// # Arguments
    /// * `a` - First context string
    /// * `b` - Second context string
    /// * `language` - Optional source language for comment stripping (if strip_comments enabled)
    fn context_similarity(&self, a: &str, b: &str, language: Option<SourceLanguage>) -> f32;
}

/// Thread-safe reference to an anchor service
pub type AnchorServiceRef = std::sync::Arc<dyn AnchorService + Send + Sync>;
