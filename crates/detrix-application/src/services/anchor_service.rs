//! Anchor Service Implementation
//!
//! Captures and manages metric location anchors for tracking code changes.
//! Enables metrics to "follow" code changes like print statements do.
//!
//! # Features
//!
//! - Source context reading with configurable line count
//! - SHA256 fingerprinting for fuzzy matching
//! - Levenshtein distance for similarity scoring
//! - Symbol lookup via LSP (when available)
//! - Multi-tier relocation fallback chain

use crate::ports::{AnchorService, SourceContext, SymbolInfo};
use async_trait::async_trait;
use detrix_config::constants::{
    DEFAULT_ANCHOR_CONTEXT_LINES, DEFAULT_ANCHOR_MAX_CONTEXT_BYTES, DEFAULT_ANCHOR_MIN_CONFIDENCE,
};
use detrix_config::{AnchorConfig, NormalizationConfig};
use detrix_core::entities::{Location, MetricAnchor, RelocationResult};
use detrix_core::error::{Error, Result};
use detrix_core::SourceLanguage;
use sha2::{Digest, Sha256};
use tokio::fs;
use tracing::{debug, trace, warn};

/// Parse line number from location string format "@file.py#123"
///
/// Returns `None` if the location is missing or the line number cannot be parsed.
fn parse_line_from_location(location: Option<&String>) -> Option<u32> {
    location
        .and_then(|loc| loc.rsplit_once('#'))
        .and_then(|(_, line)| line.parse().ok())
}

/// Configuration for the anchor service
#[derive(Debug, Clone)]
pub struct AnchorServiceConfig {
    /// Number of context lines to capture before/after
    pub context_lines: usize,
    /// Maximum bytes per context field
    pub max_context_bytes: usize,
    /// Minimum confidence for fingerprint fuzzy matching
    pub min_fingerprint_confidence: f32,
    /// Whether anchor tracking is enabled
    pub enabled: bool,
    /// Normalization rules for fingerprinting
    pub normalization: NormalizationConfig,
}

impl Default for AnchorServiceConfig {
    fn default() -> Self {
        Self {
            context_lines: DEFAULT_ANCHOR_CONTEXT_LINES,
            max_context_bytes: DEFAULT_ANCHOR_MAX_CONTEXT_BYTES,
            min_fingerprint_confidence: DEFAULT_ANCHOR_MIN_CONFIDENCE,
            enabled: true,
            normalization: NormalizationConfig::default(),
        }
    }
}

impl From<&AnchorConfig> for AnchorServiceConfig {
    fn from(config: &AnchorConfig) -> Self {
        Self {
            context_lines: config.context_lines,
            max_context_bytes: config.max_context_bytes,
            min_fingerprint_confidence: config.min_fingerprint_confidence,
            enabled: config.enabled,
            normalization: config.normalization.clone(),
        }
    }
}

/// LSP client trait for symbol lookup
///
/// This trait abstracts LSP operations needed by the anchor service.
/// Implementations should connect to the appropriate language server.
#[async_trait]
pub trait LspSymbolLookup: Send + Sync {
    /// Get the enclosing symbol (function/method) at a position
    async fn get_enclosing_symbol(
        &self,
        file: &str,
        line: u32,
        language: SourceLanguage,
    ) -> Result<Option<SymbolInfo>>;

    /// Find all symbols in a file (for relocation)
    async fn get_file_symbols(
        &self,
        file: &str,
        language: SourceLanguage,
    ) -> Result<Vec<SymbolInfo>>;
}

/// Null implementation of LSP lookup (for when LSP is unavailable)
pub struct NullLspLookup;

#[async_trait]
impl LspSymbolLookup for NullLspLookup {
    async fn get_enclosing_symbol(
        &self,
        _file: &str,
        _line: u32,
        _language: SourceLanguage,
    ) -> Result<Option<SymbolInfo>> {
        Ok(None)
    }

    async fn get_file_symbols(
        &self,
        _file: &str,
        _language: SourceLanguage,
    ) -> Result<Vec<SymbolInfo>> {
        Ok(vec![])
    }
}

/// Thread-safe reference to LSP symbol lookup
pub type LspSymbolLookupRef = std::sync::Arc<dyn LspSymbolLookup + Send + Sync>;

/// Default anchor service implementation
pub struct DefaultAnchorService {
    config: AnchorServiceConfig,
    lsp: Option<LspSymbolLookupRef>,
}

impl DefaultAnchorService {
    /// Create a new anchor service with default configuration
    pub fn new() -> Self {
        Self {
            config: AnchorServiceConfig::default(),
            lsp: None,
        }
    }

    /// Create a new anchor service with custom configuration
    pub fn with_config(config: AnchorServiceConfig) -> Self {
        Self { config, lsp: None }
    }

    /// Create a new anchor service with LSP integration
    pub fn with_lsp(config: AnchorServiceConfig, lsp: LspSymbolLookupRef) -> Self {
        Self {
            config,
            lsp: Some(lsp),
        }
    }

    /// Check if anchor tracking is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Read all lines from a file
    async fn read_file_lines(&self, file: &str) -> Result<Vec<String>> {
        let content = fs::read_to_string(file)
            .await
            .map_err(|e| Error::InvalidConfig(format!("Failed to read file '{}': {}", file, e)))?;
        Ok(content.lines().map(|l| l.to_string()).collect())
    }

    /// Truncate a string to max bytes while preserving UTF-8 boundary
    fn truncate_to_bytes(s: &str, max_bytes: usize) -> String {
        if s.len() <= max_bytes {
            return s.to_string();
        }

        // Find the last valid UTF-8 boundary before max_bytes
        let mut truncated = String::new();
        for c in s.chars() {
            if truncated.len() + c.len_utf8() > max_bytes {
                break;
            }
            truncated.push(c);
        }
        truncated
    }

    /// Relocate anchor using symbol + offset strategy
    ///
    /// Returns the new line number if the symbol is found and offset is valid.
    async fn relocate_by_symbol_offset(
        &self,
        anchor: &MetricAnchor,
        file: &str,
        language: SourceLanguage,
    ) -> Option<u32> {
        let symbol_name = anchor.enclosing_symbol.as_ref()?;
        let offset = anchor.offset_in_symbol?;
        let lsp = self.lsp.as_ref()?;

        let symbols = lsp.get_file_symbols(file, language).await.ok()?;
        let symbol = symbols.iter().find(|s| &s.name == symbol_name)?;

        let new_line = symbol.start_line + offset;
        if new_line <= symbol.end_line {
            Some(new_line)
        } else {
            None
        }
    }

    /// Relocate anchor using context fingerprint matching
    ///
    /// Returns the new line number and confidence if a matching context is found.
    async fn relocate_by_context(
        &self,
        anchor: &MetricAnchor,
        file: &str,
        language: SourceLanguage,
    ) -> Option<(u32, f32)> {
        // Only try if we have fingerprint or source line
        if anchor.context_fingerprint.is_none() && anchor.source_line.is_none() {
            return None;
        }

        let lines = self.read_file_lines(file).await.ok()?;

        // Combine context parts with newlines (matching compute_fingerprint format)
        let stored_context = format!(
            "{}\n{}\n{}",
            anchor.context_before.as_deref().unwrap_or(""),
            anchor.source_line.as_deref().unwrap_or(""),
            anchor.context_after.as_deref().unwrap_or("")
        );

        // Get symbols for verification (if we have LSP)
        let symbols = match &self.lsp {
            Some(lsp) => lsp.get_file_symbols(file, language).await.ok(),
            None => None,
        };

        let mut best_match: Option<(u32, f32)> = None;

        // Sliding window through file
        for line_num in 1..=lines.len() as u32 {
            let ctx = match self
                .read_context(
                    file,
                    line_num,
                    self.config.context_lines,
                    self.config.max_context_bytes,
                )
                .await
            {
                Ok(ctx) => ctx,
                Err(_) => continue,
            };

            // Combine context parts with newlines (matching compute_fingerprint format)
            let current_context = format!("{}\n{}\n{}", ctx.before, ctx.line, ctx.after);
            let similarity =
                self.context_similarity(&stored_context, &current_context, Some(language));

            if similarity < self.config.min_fingerprint_confidence {
                continue;
            }

            // Verify the candidate line is still in the expected symbol.
            // Fail-open: If symbol information is unavailable (LSP not running, etc.),
            // we assume the candidate is valid to allow relocation to proceed.
            // Context fingerprint matching is the primary mechanism; symbol verification
            // is an additional safety check that shouldn't block relocation entirely.
            let in_expected_symbol = symbols
                .as_ref()
                .map(|syms| {
                    Self::verify_relocation_in_symbol(
                        syms,
                        line_num,
                        anchor.enclosing_symbol.as_deref(),
                    )
                })
                .unwrap_or_else(|| {
                    trace!(
                        line = line_num,
                        "Symbol information unavailable, skipping symbol verification (fail-open)"
                    );
                    true
                });

            if !in_expected_symbol {
                trace!(
                    line = line_num,
                    expected_symbol = anchor.enclosing_symbol.as_deref(),
                    "Skipping candidate line - not in expected symbol"
                );
                continue;
            }

            if best_match.map_or(true, |(_, best_sim)| similarity > best_sim) {
                best_match = Some((line_num, similarity));
            }
        }

        best_match
    }

    /// Calculate Levenshtein distance between two strings
    fn levenshtein_distance(a: &str, b: &str) -> usize {
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        let a_len = a_chars.len();
        let b_len = b_chars.len();

        if a_len == 0 {
            return b_len;
        }
        if b_len == 0 {
            return a_len;
        }

        // Use two rows instead of full matrix for memory efficiency
        let mut prev_row: Vec<usize> = (0..=b_len).collect();
        let mut curr_row: Vec<usize> = vec![0; b_len + 1];

        for (i, a_char) in a_chars.iter().enumerate() {
            curr_row[0] = i + 1;

            for (j, b_char) in b_chars.iter().enumerate() {
                let cost = if a_char == b_char { 0 } else { 1 };
                curr_row[j + 1] = (prev_row[j + 1] + 1) // deletion
                    .min(curr_row[j] + 1) // insertion
                    .min(prev_row[j] + cost); // substitution
            }

            std::mem::swap(&mut prev_row, &mut curr_row);
        }

        prev_row[b_len]
    }

    /// Find symbol containing the given line
    ///
    /// Used to verify that a relocated line is still within the expected symbol,
    /// ensuring metric relocation is valid.
    fn find_enclosing_symbol(symbols: &[SymbolInfo], line: u32) -> Option<&SymbolInfo> {
        symbols
            .iter()
            .filter(|s| s.start_line <= line && line <= s.end_line)
            .min_by_key(|s| s.end_line - s.start_line) // Prefer innermost (smallest) symbol
    }

    /// Verify that the relocated line is within the expected symbol
    ///
    /// Returns true if:
    /// - No expected symbol was specified (original anchor didn't have symbol info)
    /// - The new line is within a symbol with the expected name
    fn verify_relocation_in_symbol(
        symbols: &[SymbolInfo],
        new_line: u32,
        expected_symbol: Option<&str>,
    ) -> bool {
        match expected_symbol {
            None => true, // No symbol constraint
            Some(expected_name) => {
                if let Some(enclosing) = Self::find_enclosing_symbol(symbols, new_line) {
                    enclosing.name == expected_name
                } else {
                    // Line is not in any symbol - this is suspicious but might be OK
                    // for module-level metrics
                    false
                }
            }
        }
    }

    /// Normalize content before fingerprinting
    ///
    /// Applies normalization rules to make fingerprinting robust against
    /// formatting changes like blank lines and trailing whitespace.
    ///
    /// # Arguments
    /// * `content` - The raw content to normalize
    /// * `config` - Normalization rules to apply
    /// * `language` - Optional source language for comment stripping
    ///
    /// # Returns
    /// Normalized content ready for fingerprinting
    pub fn normalize_for_fingerprint(
        content: &str,
        config: &NormalizationConfig,
        language: Option<SourceLanguage>,
    ) -> String {
        // Strip comments first if enabled and language is known
        let content = if config.strip_comments {
            if let Some(lang) = language {
                match crate::safety::treesitter::strip_comments(content, lang) {
                    Ok(stripped) => stripped,
                    Err(e) => {
                        tracing::warn!("Failed to strip comments: {}, using original", e);
                        content.to_string()
                    }
                }
            } else {
                content.to_string()
            }
        } else {
            content.to_string()
        };

        content
            .lines()
            .filter(|line| {
                // Filter out blank lines if enabled
                if config.strip_blank_lines && line.trim().is_empty() {
                    return false;
                }
                true
            })
            .map(|line| {
                let mut processed = line.to_string();

                // Strip trailing whitespace if enabled
                if config.strip_trailing_whitespace {
                    processed = processed.trim_end().to_string();
                }

                // Normalize internal whitespace if enabled
                // (preserves leading indentation)
                if config.normalize_internal_whitespace {
                    // Find leading whitespace
                    let leading_ws: String = processed
                        .chars()
                        .take_while(|c| c.is_whitespace())
                        .collect();

                    // Get the rest after leading whitespace
                    let rest = &processed[leading_ws.len()..];

                    // Collapse multiple spaces/tabs to single space in the rest
                    let normalized_rest: String =
                        rest.split_whitespace().collect::<Vec<_>>().join(" ");

                    processed = format!("{}{}", leading_ws, normalized_rest);
                }

                processed
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Compute fingerprint from already-normalized content
    ///
    /// Uses SHA256 to create a stable fingerprint.
    pub fn compute_fingerprint_from_normalized(normalized_content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(normalized_content.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Compute fingerprint with normalization
    ///
    /// Convenience method that normalizes content and computes fingerprint.
    pub fn compute_normalized_fingerprint(
        content: &str,
        config: &NormalizationConfig,
        language: Option<SourceLanguage>,
    ) -> String {
        let normalized = Self::normalize_for_fingerprint(content, config, language);
        Self::compute_fingerprint_from_normalized(&normalized)
    }
}

impl Default for DefaultAnchorService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AnchorService for DefaultAnchorService {
    async fn capture_anchor(
        &self,
        location: &Location,
        language: SourceLanguage,
    ) -> Result<MetricAnchor> {
        if !self.config.enabled {
            return Ok(MetricAnchor::default());
        }

        debug!(
            file = %location.file,
            line = location.line,
            "Capturing anchor for metric"
        );

        // Read source context
        let context = self
            .read_context(
                &location.file,
                location.line,
                self.config.context_lines,
                self.config.max_context_bytes,
            )
            .await?;

        // Compute fingerprint with language for comment stripping
        let fingerprint = self.compute_fingerprint(&context, Some(language));

        // Try to get enclosing symbol via LSP
        let symbol_info = self
            .get_enclosing_symbol(&location.file, location.line, language)
            .await?;

        // Build anchor
        let mut anchor = MetricAnchor {
            context_fingerprint: Some(fingerprint),
            context_before: Some(context.before),
            source_line: Some(context.line),
            context_after: Some(context.after),
            last_verified_at: Some(chrono::Utc::now().timestamp_micros()),
            original_location: Some(location.to_string_format()),
            ..Default::default()
        };

        // Add symbol info if available
        if let Some(symbol) = symbol_info {
            anchor.enclosing_symbol = Some(symbol.name);
            anchor.symbol_kind = Some(symbol.kind);
            anchor.symbol_range = Some((symbol.start_line, symbol.end_line));
            anchor.offset_in_symbol = Some(location.line.saturating_sub(symbol.start_line));
        }

        trace!(
            has_symbol = anchor.enclosing_symbol.is_some(),
            fingerprint = anchor.context_fingerprint.as_deref(),
            "Anchor captured"
        );

        Ok(anchor)
    }

    async fn verify_anchor(
        &self,
        location: &Location,
        anchor: &MetricAnchor,
        language: SourceLanguage,
    ) -> Result<bool> {
        if !self.config.enabled {
            return Ok(true); // Always valid when disabled
        }

        // Read current context
        let current_context = match self
            .read_context(
                &location.file,
                location.line,
                self.config.context_lines,
                self.config.max_context_bytes,
            )
            .await
        {
            Ok(ctx) => ctx,
            Err(_) => return Ok(false), // File unreadable = invalid
        };

        // Check fingerprint match
        if let Some(ref stored_fingerprint) = anchor.context_fingerprint {
            let current_fingerprint = self.compute_fingerprint(&current_context, Some(language));
            if &current_fingerprint == stored_fingerprint {
                return Ok(true); // Exact match
            }

            // Check fuzzy match
            let similarity = self.context_similarity(
                &format!(
                    "{}{}{}",
                    anchor.context_before.as_deref().unwrap_or(""),
                    anchor.source_line.as_deref().unwrap_or(""),
                    anchor.context_after.as_deref().unwrap_or("")
                ),
                &format!(
                    "{}{}{}",
                    current_context.before, current_context.line, current_context.after
                ),
                Some(language),
            );

            if similarity >= self.config.min_fingerprint_confidence {
                return Ok(true); // Fuzzy match
            }
        }

        Ok(false)
    }

    async fn relocate(
        &self,
        file: &str,
        anchor: &MetricAnchor,
        language: SourceLanguage,
    ) -> Result<RelocationResult> {
        if !self.config.enabled {
            return Ok(RelocationResult::Orphaned {
                last_known_location: anchor
                    .original_location
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                reason: "Anchor tracking disabled".to_string(),
            });
        }

        // Parse original location for old line number
        let old_line =
            parse_line_from_location(anchor.original_location.as_ref()).unwrap_or_else(|| {
                warn!(
                    file = %file,
                    original_location = ?anchor.original_location,
                    "Could not parse line number from original_location, using 0"
                );
                0
            });

        debug!(
            file = %file,
            old_line = old_line,
            symbol = anchor.enclosing_symbol.as_deref(),
            "Attempting relocation"
        );

        // Strategy 1: Symbol + offset
        if let Some(new_line) = self.relocate_by_symbol_offset(anchor, file, language).await {
            let symbol_name = anchor.enclosing_symbol.clone().unwrap_or_default();
            debug!(
                new_line = new_line,
                symbol = %symbol_name,
                "Relocated via symbol + offset"
            );
            return Ok(RelocationResult::RelocatedInSymbol {
                old_line,
                new_line,
                symbol_name,
            });
        }

        // Strategy 2: Context fingerprint matching
        if let Some((new_line, confidence)) = self.relocate_by_context(anchor, file, language).await
        {
            debug!(
                new_line = new_line,
                confidence = confidence,
                expected_symbol = anchor.enclosing_symbol.as_deref(),
                "Relocated via context fingerprint (verified in symbol)"
            );
            return Ok(RelocationResult::RelocatedByContext {
                old_line,
                new_line,
                confidence,
            });
        }

        // Strategy 3: Orphaned
        warn!(
            file = %file,
            old_line = old_line,
            "Could not relocate metric"
        );
        Ok(RelocationResult::Orphaned {
            last_known_location: anchor
                .original_location
                .clone()
                .unwrap_or_else(|| format!("@{}#{}", file, old_line)),
            reason: "No matching symbol or context found".to_string(),
        })
    }

    async fn read_context(
        &self,
        file: &str,
        line: u32,
        context_lines: usize,
        max_bytes: usize,
    ) -> Result<SourceContext> {
        let lines = self.read_file_lines(file).await?;

        // Line is 1-indexed
        let line_idx = (line as usize).saturating_sub(1);

        if line_idx >= lines.len() {
            return Err(Error::InvalidConfig(format!(
                "Line {} out of range (file has {} lines)",
                line,
                lines.len()
            )));
        }

        // Get the target line
        let target_line = Self::truncate_to_bytes(&lines[line_idx], max_bytes);

        // Get context before
        let before_start = line_idx.saturating_sub(context_lines);
        let before_lines: Vec<&str> = lines[before_start..line_idx]
            .iter()
            .map(|s| s.as_str())
            .collect();
        let before = Self::truncate_to_bytes(&before_lines.join("\n"), max_bytes);

        // Get context after
        let after_end = (line_idx + 1 + context_lines).min(lines.len());
        let after_lines: Vec<&str> = lines[line_idx + 1..after_end]
            .iter()
            .map(|s| s.as_str())
            .collect();
        let after = Self::truncate_to_bytes(&after_lines.join("\n"), max_bytes);

        Ok(SourceContext {
            before,
            line: target_line,
            after,
        })
    }

    async fn get_enclosing_symbol(
        &self,
        file: &str,
        line: u32,
        language: SourceLanguage,
    ) -> Result<Option<SymbolInfo>> {
        if let Some(ref lsp) = self.lsp {
            lsp.get_enclosing_symbol(file, line, language).await
        } else {
            Ok(None)
        }
    }

    /// Compute fingerprint from context using normalization
    ///
    /// Normalizes the context content before computing the fingerprint,
    /// making it robust against blank lines, trailing whitespace, and optionally comments.
    ///
    /// # Arguments
    /// * `context` - The source context to fingerprint
    /// * `language` - Optional source language for comment stripping (if strip_comments enabled)
    fn compute_fingerprint(
        &self,
        context: &SourceContext,
        language: Option<SourceLanguage>,
    ) -> String {
        // Combine context parts for normalization
        let combined = format!("{}\n{}\n{}", context.before, context.line, context.after);

        // Normalize using the service's config with language for comment stripping
        let normalized =
            Self::normalize_for_fingerprint(&combined, &self.config.normalization, language);

        // Compute fingerprint from normalized content
        Self::compute_fingerprint_from_normalized(&normalized)
    }

    /// Compute similarity between two context strings
    ///
    /// Normalizes both strings before comparing to ignore formatting differences.
    ///
    /// # Arguments
    /// * `a` - First context string
    /// * `b` - Second context string
    /// * `language` - Optional source language for comment stripping (if strip_comments enabled)
    fn context_similarity(&self, a: &str, b: &str, language: Option<SourceLanguage>) -> f32 {
        // Normalize both strings for fair comparison with optional comment stripping
        let norm_a = Self::normalize_for_fingerprint(a, &self.config.normalization, language);
        let norm_b = Self::normalize_for_fingerprint(b, &self.config.normalization, language);

        if norm_a.is_empty() && norm_b.is_empty() {
            return 1.0;
        }
        if norm_a.is_empty() || norm_b.is_empty() {
            return 0.0;
        }

        let distance = Self::levenshtein_distance(&norm_a, &norm_b);
        let max_len = norm_a.len().max(norm_b.len());
        1.0 - (distance as f32 / max_len as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_file(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    #[tokio::test]
    async fn test_read_context() {
        let content = "line1\nline2\nline3\nline4\nline5\n";
        let file = create_test_file(content);

        let service = DefaultAnchorService::new();
        let context = service
            .read_context(file.path().to_str().unwrap(), 3, 2, 200)
            .await
            .unwrap();

        assert_eq!(context.before, "line1\nline2");
        assert_eq!(context.line, "line3");
        assert_eq!(context.after, "line4\nline5");
    }

    #[tokio::test]
    async fn test_read_context_at_start() {
        let content = "line1\nline2\nline3\n";
        let file = create_test_file(content);

        let service = DefaultAnchorService::new();
        let context = service
            .read_context(file.path().to_str().unwrap(), 1, 2, 200)
            .await
            .unwrap();

        assert_eq!(context.before, "");
        assert_eq!(context.line, "line1");
        assert_eq!(context.after, "line2\nline3");
    }

    #[tokio::test]
    async fn test_read_context_at_end() {
        let content = "line1\nline2\nline3\n";
        let file = create_test_file(content);

        let service = DefaultAnchorService::new();
        let context = service
            .read_context(file.path().to_str().unwrap(), 3, 2, 200)
            .await
            .unwrap();

        assert_eq!(context.before, "line1\nline2");
        assert_eq!(context.line, "line3");
        assert_eq!(context.after, "");
    }

    #[test]
    fn test_compute_fingerprint() {
        let service = DefaultAnchorService::new();
        let context = SourceContext {
            before: "before".to_string(),
            line: "target".to_string(),
            after: "after".to_string(),
        };

        let fp1 = service.compute_fingerprint(&context, None);
        let fp2 = service.compute_fingerprint(&context, None);

        assert_eq!(fp1, fp2); // Same input = same output
        assert_eq!(fp1.len(), 64); // SHA256 hex = 64 chars
    }

    #[test]
    fn test_context_similarity() {
        let service = DefaultAnchorService::new();

        // Identical strings
        assert!((service.context_similarity("hello", "hello", None) - 1.0).abs() < 0.001);

        // Completely different
        assert!(service.context_similarity("abc", "xyz", None) < 0.5);

        // Similar strings
        let sim = service.context_similarity("hello world", "hello there", None);
        assert!(sim > 0.5 && sim < 1.0);

        // Empty strings
        assert!((service.context_similarity("", "", None) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(DefaultAnchorService::levenshtein_distance("", ""), 0);
        assert_eq!(DefaultAnchorService::levenshtein_distance("abc", ""), 3);
        assert_eq!(DefaultAnchorService::levenshtein_distance("", "abc"), 3);
        assert_eq!(DefaultAnchorService::levenshtein_distance("abc", "abc"), 0);
        assert_eq!(DefaultAnchorService::levenshtein_distance("abc", "abd"), 1);
        assert_eq!(
            DefaultAnchorService::levenshtein_distance("kitten", "sitting"),
            3
        );
    }

    #[tokio::test]
    async fn test_capture_anchor() {
        let content = "def foo():\n    x = 1\n    y = 2\n    return x + y\n";
        let file = create_test_file(content);

        let service = DefaultAnchorService::new();
        let location = Location {
            file: file.path().to_str().unwrap().to_string(),
            line: 3,
        };

        let anchor = service
            .capture_anchor(&location, SourceLanguage::Python)
            .await
            .unwrap();

        assert!(anchor.context_fingerprint.is_some());
        assert!(anchor.source_line.is_some());
        assert_eq!(anchor.source_line.as_deref(), Some("    y = 2"));
        assert!(anchor.last_verified_at.is_some());
        assert!(anchor.original_location.is_some());
    }

    #[tokio::test]
    async fn test_verify_anchor_exact_match() {
        let content = "line1\nline2\nline3\n";
        let file = create_test_file(content);
        let file_path = file.path().to_str().unwrap().to_string();

        let service = DefaultAnchorService::new();
        let location = Location {
            file: file_path.clone(),
            line: 2,
        };

        // Capture anchor
        let anchor = service
            .capture_anchor(&location, SourceLanguage::Python)
            .await
            .unwrap();

        // Verify (should match)
        let valid = service
            .verify_anchor(&location, &anchor, SourceLanguage::Python)
            .await
            .unwrap();

        assert!(valid);
    }

    #[test]
    fn test_truncate_to_bytes() {
        // ASCII
        assert_eq!(
            DefaultAnchorService::truncate_to_bytes("hello", 10),
            "hello"
        );
        assert_eq!(DefaultAnchorService::truncate_to_bytes("hello", 3), "hel");

        // UTF-8 (emoji = 4 bytes)
        let emoji = "a🎉b";
        assert_eq!(DefaultAnchorService::truncate_to_bytes(emoji, 10), emoji);
        assert_eq!(DefaultAnchorService::truncate_to_bytes(emoji, 5), "a🎉");
        assert_eq!(DefaultAnchorService::truncate_to_bytes(emoji, 2), "a");
    }

    // ============================================================================
    // verify_relocation_in_symbol tests
    // ============================================================================

    #[test]
    fn test_verify_no_expected_symbol_accepts_any_line() {
        let symbols = vec![
            SymbolInfo {
                name: "foo".to_string(),
                kind: 12,
                start_line: 1,
                end_line: 5,
            },
            SymbolInfo {
                name: "bar".to_string(),
                kind: 12,
                start_line: 7,
                end_line: 10,
            },
        ];

        // No expected symbol - should accept any line
        assert!(DefaultAnchorService::verify_relocation_in_symbol(
            &symbols, 3, None
        ));
        assert!(DefaultAnchorService::verify_relocation_in_symbol(
            &symbols, 8, None
        ));
        assert!(DefaultAnchorService::verify_relocation_in_symbol(
            &symbols, 100, None
        ));
    }

    #[test]
    fn test_verify_expected_symbol_accepts_correct_function() {
        let symbols = vec![
            SymbolInfo {
                name: "foo".to_string(),
                kind: 12,
                start_line: 1,
                end_line: 5,
            },
            SymbolInfo {
                name: "bar".to_string(),
                kind: 12,
                start_line: 7,
                end_line: 10,
            },
        ];

        // Line 3 is in "foo"
        assert!(DefaultAnchorService::verify_relocation_in_symbol(
            &symbols,
            3,
            Some("foo")
        ));

        // Line 8 is in "bar"
        assert!(DefaultAnchorService::verify_relocation_in_symbol(
            &symbols,
            8,
            Some("bar")
        ));
    }

    #[test]
    fn test_verify_expected_symbol_rejects_wrong_function() {
        let symbols = vec![
            SymbolInfo {
                name: "foo".to_string(),
                kind: 12,
                start_line: 1,
                end_line: 5,
            },
            SymbolInfo {
                name: "bar".to_string(),
                kind: 12,
                start_line: 7,
                end_line: 10,
            },
        ];

        // Line 3 is in "foo", but we expect "bar" - should reject
        assert!(!DefaultAnchorService::verify_relocation_in_symbol(
            &symbols,
            3,
            Some("bar")
        ));

        // Line 8 is in "bar", but we expect "foo" - should reject
        assert!(!DefaultAnchorService::verify_relocation_in_symbol(
            &symbols,
            8,
            Some("foo")
        ));
    }

    #[test]
    fn test_verify_line_outside_all_symbols_rejected() {
        let symbols = vec![SymbolInfo {
            name: "foo".to_string(),
            kind: 12,
            start_line: 1,
            end_line: 5,
        }];

        // Line 10 is outside all symbols, and we expect "foo"
        assert!(!DefaultAnchorService::verify_relocation_in_symbol(
            &symbols,
            10,
            Some("foo")
        ));
    }

    #[test]
    fn test_verify_nested_symbols_prefers_innermost() {
        let symbols = vec![
            SymbolInfo {
                name: "outer".to_string(),
                kind: 12,
                start_line: 1,
                end_line: 10,
            },
            SymbolInfo {
                name: "inner".to_string(),
                kind: 12,
                start_line: 3,
                end_line: 6,
            },
        ];

        // Line 4 is in both, but inner is smaller
        assert!(DefaultAnchorService::verify_relocation_in_symbol(
            &symbols,
            4,
            Some("inner")
        ));

        // Expecting "outer" when inside "inner" should fail
        // (because innermost symbol is "inner", not "outer")
        assert!(!DefaultAnchorService::verify_relocation_in_symbol(
            &symbols,
            4,
            Some("outer")
        ));
    }

    // ============================================================================
    // Normalization tests (TDD - tests written first)
    // ============================================================================

    #[test]
    fn test_normalize_strips_blank_lines() {
        let config = NormalizationConfig {
            strip_blank_lines: true,
            ..Default::default()
        };

        let input = "line1\n\n\nline2\n   \nline3";
        let result = DefaultAnchorService::normalize_for_fingerprint(input, &config, None);

        assert_eq!(result, "line1\nline2\nline3");
    }

    #[test]
    fn test_normalize_preserves_lines_when_strip_blank_disabled() {
        let config = NormalizationConfig {
            strip_blank_lines: false,
            strip_trailing_whitespace: false,
            ..Default::default()
        };

        let input = "line1\n\nline2";
        let result = DefaultAnchorService::normalize_for_fingerprint(input, &config, None);

        assert_eq!(result, input);
    }

    #[test]
    fn test_normalize_strips_trailing_whitespace() {
        let config = NormalizationConfig {
            strip_trailing_whitespace: true,
            strip_blank_lines: false,
            ..Default::default()
        };

        let input = "line1   \nline2\t\t\nline3";
        let result = DefaultAnchorService::normalize_for_fingerprint(input, &config, None);

        assert_eq!(result, "line1\nline2\nline3");
    }

    #[test]
    fn test_normalize_preserves_leading_indentation() {
        let config = NormalizationConfig::default();

        // Python-style indentation should be preserved
        let input = "def foo():\n    x = 1\n    return x";
        let result = DefaultAnchorService::normalize_for_fingerprint(input, &config, None);

        assert_eq!(result, input); // Leading whitespace preserved
    }

    #[test]
    fn test_normalize_internal_whitespace() {
        let config = NormalizationConfig {
            normalize_internal_whitespace: true,
            ..Default::default()
        };

        let input = "x  =  1\n    y   =   2";
        let result = DefaultAnchorService::normalize_for_fingerprint(input, &config, None);

        // Leading indent preserved, internal spaces collapsed
        assert_eq!(result, "x = 1\n    y = 2");
    }

    #[test]
    fn test_normalize_combined_options() {
        let config = NormalizationConfig {
            strip_blank_lines: true,
            strip_trailing_whitespace: true,
            normalize_internal_whitespace: false,
            strip_comments: false,
        };

        let input = "line1  \n\n   \nline2   ";
        let result = DefaultAnchorService::normalize_for_fingerprint(input, &config, None);

        assert_eq!(result, "line1\nline2");
    }

    #[test]
    fn test_normalize_empty_input() {
        let config = NormalizationConfig::default();
        let result = DefaultAnchorService::normalize_for_fingerprint("", &config, None);
        assert_eq!(result, "");
    }

    #[test]
    fn test_normalize_only_blank_lines() {
        let config = NormalizationConfig::default();
        let result =
            DefaultAnchorService::normalize_for_fingerprint("\n\n   \n\t\n", &config, None);
        assert_eq!(result, "");
    }

    #[test]
    fn test_fingerprint_with_normalization_identical_for_blank_line_differences() {
        let config = NormalizationConfig::default();

        let content1 = "boo = \"123\"\n\n\nfoo = \"55\"";
        let content2 = "boo = \"123\"\nfoo = \"55\"";

        let norm1 = DefaultAnchorService::normalize_for_fingerprint(content1, &config, None);
        let norm2 = DefaultAnchorService::normalize_for_fingerprint(content2, &config, None);

        // After normalization, they should be identical
        assert_eq!(norm1, norm2);

        // And thus produce the same fingerprint
        let fp1 = DefaultAnchorService::compute_fingerprint_from_normalized(&norm1);
        let fp2 = DefaultAnchorService::compute_fingerprint_from_normalized(&norm2);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_normalize_with_strip_comments_python() {
        use crate::SourceLanguage;

        let config = NormalizationConfig {
            strip_comments: true,
            strip_blank_lines: true,
            strip_trailing_whitespace: true,
            normalize_internal_whitespace: false,
        };

        // Content without comments
        let without_comments = r#"    order_id = random.randint(10000, 99999)
    order_value = quantity * price
    return order_id, order_value"#;

        // Content with comments interspersed
        let with_comments = r#"    order_id = random.randint(10000, 99999)
    # Calculate total order value
    order_value = quantity * price
    # Return the computed values
    return order_id, order_value"#;

        let norm1 =
            DefaultAnchorService::normalize_for_fingerprint(without_comments, &config, None);
        let norm2 = DefaultAnchorService::normalize_for_fingerprint(
            with_comments,
            &config,
            Some(SourceLanguage::Python),
        );

        println!("norm1:\n{}", norm1);
        println!("norm2:\n{}", norm2);

        // After normalization with strip_comments, they should be identical
        assert_eq!(norm1, norm2);

        // And thus produce the same fingerprint
        let fp1 = DefaultAnchorService::compute_fingerprint_from_normalized(&norm1);
        let fp2 = DefaultAnchorService::compute_fingerprint_from_normalized(&norm2);
        assert_eq!(fp1, fp2);
    }
}
