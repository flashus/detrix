//! Anchor configuration for metric location tracking
//!
//! The anchor system enables metrics to "follow" code changes like print
//! statements do. When source files are modified, metrics are automatically
//! relocated using a multi-tier fallback system.
//!
//! # Configuration
//!
//! ```toml
//! [anchor]
//! enabled = true                    # Master switch (ON by default)
//! context_lines = 2                 # Lines of context to capture
//! max_context_bytes = 200           # Max bytes per context field
//! min_fingerprint_confidence = 0.8  # Fuzzy match threshold
//! auto_relocate = true              # Relocate on file change
//! file_change_debounce_ms = 500     # Debounce rapid changes
//! ```

use serde::{Deserialize, Serialize};

use crate::constants::{
    DEFAULT_ANCHOR_CONTEXT_LINES, DEFAULT_ANCHOR_MAX_CONTEXT_BYTES, DEFAULT_ANCHOR_MIN_CONFIDENCE,
    DEFAULT_FILE_CHANGE_DEBOUNCE_MS, DEFAULT_FILE_WATCHER_CHANNEL_SIZE,
};

// ============================================================================
// Normalization Configuration
// ============================================================================

/// Configuration for normalizing context before fingerprinting
///
/// Normalization makes fingerprinting more robust against formatting changes
/// (blank lines, trailing whitespace) that don't affect code semantics.
///
/// # Example TOML
///
/// ```toml
/// [anchor.normalization]
/// strip_blank_lines = true           # Remove blank/whitespace-only lines
/// strip_trailing_whitespace = true   # Remove trailing spaces from lines
/// normalize_internal_whitespace = false  # Don't collapse internal spaces
/// strip_comments = false             # Keep comments in fingerprint
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizationConfig {
    /// Strip blank lines (lines that are empty or contain only whitespace)
    ///
    /// Default: true
    ///
    /// This makes fingerprinting robust against adding/removing blank lines
    /// between code statements.
    #[serde(default = "default_true")]
    pub strip_blank_lines: bool,

    /// Strip trailing whitespace from each line
    ///
    /// Default: true
    ///
    /// Removes spaces/tabs at the end of lines before fingerprinting.
    #[serde(default = "default_true")]
    pub strip_trailing_whitespace: bool,

    /// Normalize internal whitespace (collapse multiple spaces to single)
    ///
    /// Default: false
    ///
    /// When enabled, "x  =  1" becomes "x = 1" for fingerprinting.
    /// Note: This preserves leading indentation (important for Python).
    #[serde(default)]
    pub normalize_internal_whitespace: bool,

    /// Strip comments before fingerprinting
    ///
    /// Default: false
    ///
    /// When enabled, comments are removed before computing the fingerprint.
    /// This makes fingerprinting robust against comment changes.
    /// Note: Language-specific comment detection is required.
    #[serde(default)]
    pub strip_comments: bool,
}

impl Default for NormalizationConfig {
    fn default() -> Self {
        Self {
            strip_blank_lines: true,
            strip_trailing_whitespace: true,
            normalize_internal_whitespace: false,
            strip_comments: false,
        }
    }
}

impl NormalizationConfig {
    /// Create an aggressive normalization config
    ///
    /// Enables all normalization options for maximum robustness
    /// against formatting changes.
    pub fn aggressive() -> Self {
        Self {
            strip_blank_lines: true,
            strip_trailing_whitespace: true,
            normalize_internal_whitespace: true,
            strip_comments: true,
        }
    }

    /// Create a conservative normalization config
    ///
    /// Disables all normalization - fingerprint is computed on raw content.
    /// Use when exact whitespace matching is required.
    pub fn conservative() -> Self {
        Self {
            strip_blank_lines: false,
            strip_trailing_whitespace: false,
            normalize_internal_whitespace: false,
            strip_comments: false,
        }
    }

    /// Check if any normalization is enabled
    pub fn is_any_enabled(&self) -> bool {
        self.strip_blank_lines
            || self.strip_trailing_whitespace
            || self.normalize_internal_whitespace
            || self.strip_comments
    }
}

fn default_true() -> bool {
    true
}

/// Configuration for the anchor system (metric location tracking)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorConfig {
    /// Master switch for anchor tracking (default: true)
    ///
    /// When enabled, anchors are captured when metrics are created and
    /// used to relocate metrics when files change.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Number of context lines to capture before/after the metric line (default: 2)
    ///
    /// More lines provide better matching accuracy but use more storage.
    /// Recommended: 2-3 lines.
    #[serde(default = "default_context_lines")]
    pub context_lines: usize,

    /// Maximum bytes for each context field (default: 200)
    ///
    /// Limits the size of context_before, source_line, and context_after.
    /// Longer lines are truncated at UTF-8 character boundaries.
    #[serde(default = "default_max_context_bytes")]
    pub max_context_bytes: usize,

    /// Minimum confidence for fingerprint fuzzy matching (default: 0.8)
    ///
    /// Value between 0.0 and 1.0. Higher values require closer matches.
    /// - 1.0 = exact match only
    /// - 0.8 = allow ~20% difference (recommended)
    /// - 0.5 = allow ~50% difference (loose)
    #[serde(default = "default_min_fingerprint_confidence")]
    pub min_fingerprint_confidence: f32,

    /// Automatically relocate metrics when source files change (default: true)
    ///
    /// When enabled, file changes trigger automatic metric relocation.
    /// Relocated metrics emit warning events.
    #[serde(default = "default_true")]
    pub auto_relocate: bool,

    /// Debounce time for file change events in milliseconds (default: 500)
    ///
    /// Prevents processing the same file multiple times during rapid edits.
    /// Lower values = faster response, higher values = less CPU usage.
    #[serde(default = "default_file_change_debounce_ms")]
    pub file_change_debounce_ms: u64,

    /// Channel buffer size for file watcher events (default: 256)
    ///
    /// Size of the internal channel buffer for file change notifications.
    /// Increase if file changes are being dropped during rapid edits.
    #[serde(default = "default_file_watcher_channel_size")]
    pub file_watcher_channel_size: usize,

    /// Watch for file changes in these directories (default: empty = watch all metric files)
    ///
    /// If empty, watches the directories containing files with metrics.
    /// Specify explicit paths to limit watching scope.
    #[serde(default)]
    pub watch_paths: Vec<String>,

    /// Disable auto-relocation for these metric groups (default: empty)
    ///
    /// Metrics in these groups will not be automatically relocated.
    /// Use for stable metrics that shouldn't move.
    #[serde(default)]
    pub exclude_groups: Vec<String>,

    /// Normalization rules for fingerprinting (default: strip blanks + trailing whitespace)
    ///
    /// Controls how context is normalized before computing the fingerprint.
    /// Normalization makes fingerprinting robust against formatting changes.
    #[serde(default)]
    pub normalization: NormalizationConfig,
}

fn default_context_lines() -> usize {
    DEFAULT_ANCHOR_CONTEXT_LINES
}

fn default_max_context_bytes() -> usize {
    DEFAULT_ANCHOR_MAX_CONTEXT_BYTES
}

fn default_min_fingerprint_confidence() -> f32 {
    DEFAULT_ANCHOR_MIN_CONFIDENCE
}

fn default_file_change_debounce_ms() -> u64 {
    DEFAULT_FILE_CHANGE_DEBOUNCE_MS
}

fn default_file_watcher_channel_size() -> usize {
    DEFAULT_FILE_WATCHER_CHANNEL_SIZE
}

impl Default for AnchorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            context_lines: DEFAULT_ANCHOR_CONTEXT_LINES,
            max_context_bytes: DEFAULT_ANCHOR_MAX_CONTEXT_BYTES,
            min_fingerprint_confidence: DEFAULT_ANCHOR_MIN_CONFIDENCE,
            auto_relocate: true,
            file_change_debounce_ms: DEFAULT_FILE_CHANGE_DEBOUNCE_MS,
            file_watcher_channel_size: DEFAULT_FILE_WATCHER_CHANNEL_SIZE,
            watch_paths: Vec::new(),
            exclude_groups: Vec::new(),
            normalization: NormalizationConfig::default(),
        }
    }
}

impl AnchorConfig {
    /// Create a disabled anchor config
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    /// Check if anchor tracking is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Check if a group is excluded from auto-relocation
    pub fn is_group_excluded(&self, group: &str) -> bool {
        self.exclude_groups.iter().any(|g| g == group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_group_excluded() {
        let config = AnchorConfig {
            exclude_groups: vec!["stable".to_string(), "production".to_string()],
            ..Default::default()
        };

        assert!(config.is_group_excluded("stable"));
        assert!(config.is_group_excluded("production"));
        assert!(!config.is_group_excluded("development"));
    }

    #[test]
    fn test_anchor_config_from_toml() {
        let toml_str = r#"
            enabled = true
            context_lines = 3
            max_context_bytes = 300
            min_fingerprint_confidence = 0.9
            auto_relocate = true
            file_change_debounce_ms = 1000
            watch_paths = ["/src", "/lib"]
            exclude_groups = ["stable"]
        "#;

        let config: AnchorConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.context_lines, 3);
        assert_eq!(config.max_context_bytes, 300);
        assert!((config.min_fingerprint_confidence - 0.9).abs() < 0.001);
        assert!(config.auto_relocate);
        assert_eq!(config.file_change_debounce_ms, 1000);
        assert_eq!(config.watch_paths, vec!["/src", "/lib"]);
        assert_eq!(config.exclude_groups, vec!["stable"]);
    }

    #[test]
    fn test_anchor_config_serialization() {
        let config = AnchorConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("enabled = true"));
        assert!(toml_str.contains("context_lines = 2"));
    }

    #[test]
    fn test_normalization_config_from_toml() {
        let toml_str = r#"
            strip_blank_lines = false
            strip_trailing_whitespace = true
            normalize_internal_whitespace = true
            strip_comments = true
        "#;

        let config: NormalizationConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.strip_blank_lines);
        assert!(config.strip_trailing_whitespace);
        assert!(config.normalize_internal_whitespace);
        assert!(config.strip_comments);
    }

    #[test]
    fn test_anchor_config_with_normalization_from_toml() {
        let toml_str = r#"
            enabled = true
            context_lines = 2

            [normalization]
            strip_blank_lines = true
            strip_trailing_whitespace = true
            normalize_internal_whitespace = false
            strip_comments = false
        "#;

        let config: AnchorConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert!(config.normalization.strip_blank_lines);
        assert!(config.normalization.strip_trailing_whitespace);
        assert!(!config.normalization.normalize_internal_whitespace);
        assert!(!config.normalization.strip_comments);
    }
}
