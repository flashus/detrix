//! Metric anchor system - tracks metric locations across code changes

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// Anchor information to track metric location across code changes
///
/// This enables metrics to "follow" code like print statements do.
/// When code is refactored (lines added/removed, functions moved),
/// the anchor data helps relocate metrics to their new positions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MetricAnchor {
    // === Symbol Anchor (from LSP) ===
    /// Enclosing function/method name (e.g., "process_payment")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_symbol: Option<String>,

    /// LSP SymbolKind (12=Function, 6=Method, 5=Class)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_kind: Option<u32>,

    /// Range of the enclosing symbol (start_line, end_line)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_range: Option<(u32, u32)>,

    /// Line offset from symbol start (metric_line - symbol_start_line)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_in_symbol: Option<u32>,

    // === Context Fingerprint ===
    /// SHA256 hash of context_before + source_line + context_after
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_fingerprint: Option<String>,

    /// 2-3 lines before the metric (actual source text, ~200 bytes max)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_before: Option<String>,

    /// The actual line where the metric is set (source text, ~100 bytes max)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_line: Option<String>,

    /// 2-3 lines after the metric (actual source text, ~200 bytes max)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_after: Option<String>,

    // === Tracking Metadata ===
    /// Timestamp when anchor was last verified valid (microseconds since epoch)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_verified_at: Option<i64>,

    /// Original location when metric was created (for audit trail)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_location: Option<String>,
}

impl MetricAnchor {
    /// Create a new empty anchor
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if this anchor has symbol information from LSP
    pub fn has_symbol_anchor(&self) -> bool {
        self.enclosing_symbol.is_some() && self.offset_in_symbol.is_some()
    }

    /// Check if this anchor has a context fingerprint
    pub fn has_context_fingerprint(&self) -> bool {
        self.context_fingerprint.is_some()
    }

    /// Compute fingerprint from the stored context
    pub fn compute_fingerprint(&self) -> Option<String> {
        let before = self.context_before.as_deref().unwrap_or("");
        let source = self.source_line.as_deref().unwrap_or("");
        let after = self.context_after.as_deref().unwrap_or("");

        if before.is_empty() && source.is_empty() && after.is_empty() {
            return None;
        }

        let mut hasher = Sha256::new();
        hasher.update(before.as_bytes());
        hasher.update(source.as_bytes());
        hasher.update(after.as_bytes());
        Some(format!("{:x}", hasher.finalize()))
    }

    /// Update the fingerprint based on current context fields
    pub fn refresh_fingerprint(&mut self) {
        self.context_fingerprint = self.compute_fingerprint();
    }
}

/// Status of a metric's anchor
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnchorStatus {
    /// No anchor data captured yet
    #[default]
    Unanchored,
    /// Anchor verified at current location
    Verified,
    /// Metric was relocated to new line, user was warned
    Relocated,
    /// Could not relocate - metric needs manual fix
    Orphaned,
}

impl fmt::Display for AnchorStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnchorStatus::Unanchored => write!(f, "unanchored"),
            AnchorStatus::Verified => write!(f, "verified"),
            AnchorStatus::Relocated => write!(f, "relocated"),
            AnchorStatus::Orphaned => write!(f, "orphaned"),
        }
    }
}

/// Result of attempting to relocate a metric
#[derive(Debug, Clone)]
pub enum RelocationResult {
    /// Exact match - line unchanged, no relocation needed
    ExactMatch,

    /// Relocated within the same enclosing symbol using offset
    RelocatedInSymbol {
        old_line: u32,
        new_line: u32,
        symbol_name: String,
    },

    /// Relocated using context fingerprint matching
    RelocatedByContext {
        old_line: u32,
        new_line: u32,
        /// Confidence score 0.0-1.0 (1.0 = exact fingerprint match)
        confidence: f32,
    },

    /// Could not relocate - metric is now orphaned
    Orphaned {
        last_known_location: String,
        reason: String,
    },
}

impl RelocationResult {
    /// Check if relocation was successful (not orphaned)
    pub fn is_success(&self) -> bool {
        !matches!(self, RelocationResult::Orphaned { .. })
    }

    /// Get the new line number if relocated
    pub fn new_line(&self) -> Option<u32> {
        match self {
            RelocationResult::ExactMatch => None,
            RelocationResult::RelocatedInSymbol { new_line, .. } => Some(*new_line),
            RelocationResult::RelocatedByContext { new_line, .. } => Some(*new_line),
            RelocationResult::Orphaned { .. } => None,
        }
    }
}
