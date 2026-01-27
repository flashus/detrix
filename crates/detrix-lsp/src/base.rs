//! Base purity analyzer with common LSP operations
//!
//! This module provides shared infrastructure for language-specific purity analyzers.
//! It extracts common patterns from Go and Python implementations to reduce code duplication.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  BasePurityAnalyzer                                             │
//! │  - Common struct fields (manager, config, workspace_root, cache)│
//! │  - Common workspace operations                                  │
//! │  - Common LSP operations (prepare_call_hierarchy, get_outgoing) │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  LanguageAnalyzer trait                                         │
//! │  - Language-specific function classification                   │
//! │  - Language name                                                │
//! │  - LSP server availability check                               │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

use crate::cache;
use crate::cache::CacheRef;
use crate::error::{Error, Result};
use crate::manager::LspManager;
use crate::protocol::{
    CallHierarchyItem, CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams,
    CallHierarchyPrepareParams, Position, TextDocumentIdentifier,
};
use detrix_config::LspConfig;
use detrix_core::{ImpureCall, PurityAnalysis, PurityLevel, SourceLanguage};
use serde_json::json;
use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, trace, warn};

/// Extracted function data - unified struct for all languages
///
/// Contains the function body and language-specific parameter information
/// that each analyzer can populate differently.
#[derive(Debug, Default)]
pub struct ExtractedFunction {
    /// The function body source code
    pub body: String,
    /// All parameter names
    pub parameters: Vec<String>,
    /// Mutable parameters (Rust-specific: `&mut` params)
    pub mutable_params: Vec<String>,
    /// Whether `self` is mutable (Rust-specific)
    pub is_mut_self: bool,
    /// Receiver type (Go-specific: method receiver)
    pub receiver: Option<String>,
}

impl ExtractedFunction {
    /// Create new ExtractedFunction with body and parameters
    pub fn new(body: String, parameters: Vec<String>) -> Self {
        Self {
            body,
            parameters,
            ..Default::default()
        }
    }

    /// Builder: set mutable parameters (Rust)
    pub fn with_mutable_params(mut self, mutable_params: Vec<String>) -> Self {
        self.mutable_params = mutable_params;
        self
    }

    /// Builder: set mut self (Rust)
    pub fn with_mut_self(mut self, is_mut_self: bool) -> Self {
        self.is_mut_self = is_mut_self;
        self
    }

    /// Builder: set receiver (Go)
    pub fn with_receiver(mut self, receiver: Option<String>) -> Self {
        self.receiver = receiver;
        self
    }
}

/// Result of analyzing a function body for purity
///
/// Contains the direct impure calls found and user-defined functions that need
/// recursive analysis.
pub struct BodyAnalysisResult {
    /// Whether the body itself appears pure (before recursive analysis)
    pub is_pure: bool,
    /// Direct impure calls found in the body
    pub impure_calls: Vec<ImpureCall>,
    /// User-defined function calls that need recursive analysis
    pub user_function_calls: Vec<String>,
}

/// Trait for language-specific purity analysis behavior
///
/// Implementors provide language-specific function classification lists
/// and analysis strategies.
#[async_trait::async_trait]
pub trait LanguageAnalyzer: Send + Sync {
    /// Get the language
    fn language(&self) -> SourceLanguage;

    /// Classify a function name based on language-specific lists
    ///
    /// Returns:
    /// - `PurityLevel::Pure` for known pure functions
    /// - `PurityLevel::Impure` for known impure functions
    /// - `PurityLevel::Unknown` for user-defined or unrecognized functions
    fn classify_function(&self, name: &str) -> PurityLevel;

    /// Extract function body and parameters from source file
    ///
    /// Returns `ExtractedFunction` with language-specific data populated.
    /// Each language fills in the relevant fields (e.g., Rust uses mutable_params,
    /// Go uses receiver).
    async fn extract_function(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> Result<ExtractedFunction>;

    /// Analyze extracted function body for purity
    ///
    /// Performs language-specific analysis of the function body to find
    /// impure calls and user-defined functions that need recursive analysis.
    fn analyze_body(&self, extracted: &ExtractedFunction) -> BodyAnalysisResult;

    /// Check if the LSP server is available
    ///
    /// Default implementation checks if the command is executable.
    fn is_lsp_available(&self, config: &LspConfig) -> bool {
        std::process::Command::new(&config.command)
            .arg("version")
            .output()
            .is_ok()
    }
}

/// Base purity analyzer with common LSP operations
///
/// This struct contains the shared infrastructure used by both
/// Go and Python purity analyzers.
pub struct BasePurityAnalyzer {
    /// LSP manager for server lifecycle
    pub(crate) manager: Arc<LspManager>,
    /// Configuration
    pub(crate) config: LspConfig,
    /// Workspace root
    pub(crate) workspace_root: Arc<RwLock<Option<PathBuf>>>,
    /// Optional purity cache for faster repeated lookups
    pub(crate) cache: Option<CacheRef>,
}

impl BasePurityAnalyzer {
    /// Create a new base analyzer with the given configuration
    pub fn new(config: LspConfig) -> Self {
        Self {
            manager: Arc::new(LspManager::new(config.clone())),
            config,
            workspace_root: Arc::new(RwLock::new(None)),
            cache: None,
        }
    }

    /// Create a new base analyzer with cache
    pub fn with_cache(config: LspConfig, cache: CacheRef) -> Self {
        Self {
            manager: Arc::new(LspManager::new(config.clone())),
            config,
            workspace_root: Arc::new(RwLock::new(None)),
            cache: Some(cache),
        }
    }

    /// Set the cache after creation
    pub fn set_cache(&mut self, cache: CacheRef) {
        self.cache = Some(cache);
    }

    /// Get reference to the cache
    pub fn cache(&self) -> Option<&CacheRef> {
        self.cache.as_ref()
    }

    /// Get reference to the config
    pub fn config(&self) -> &LspConfig {
        &self.config
    }

    /// Get reference to the manager
    pub fn manager(&self) -> &LspManager {
        &self.manager
    }

    /// Set the workspace root
    ///
    /// Takes ownership of the path to store it internally.
    /// Used for file:// URIs in LSP communication.
    pub async fn set_workspace_root(&self, root: PathBuf) {
        let mut workspace = self.workspace_root.write().await;
        *workspace = Some(root);
    }

    /// Get workspace root, using current directory as fallback
    pub async fn get_workspace_root(&self) -> PathBuf {
        let root = self.workspace_root.read().await;
        root.clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()))
    }

    /// Check if the LSP server is ready
    pub async fn is_ready(&self) -> bool {
        self.manager.is_ready().await
    }

    /// Ensure the LSP server is started
    pub async fn ensure_started(&self) -> Result<()> {
        if !self.manager.is_ready().await {
            let workspace_root = self.get_workspace_root().await;
            self.manager.start(&workspace_root).await?;
        }
        Ok(())
    }

    /// Prepare call hierarchy for a function at a given position
    ///
    /// Returns `Ok(None)` if call hierarchy is not supported by the LSP server.
    /// Returns `Ok(Some(vec![]))` if no items found at the position.
    pub async fn prepare_call_hierarchy(
        &self,
        file_path: &Path,
        line: u32,
        character: u32,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let client_guard = self.manager.client().await?;
        let client = client_guard
            .as_ref()
            .ok_or_else(|| Error::ServerNotAvailable("No LSP client available".into()))?;

        let uri = format!("file://{}", file_path.display());

        let params = CallHierarchyPrepareParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        };

        let response = client
            .request("textDocument/prepareCallHierarchy", Some(json!(params)))
            .await?;

        let result = match response.into_result() {
            Ok(r) => r,
            Err(e) => {
                // LSP error -32601 means "Method Not Found" - the server doesn't support call hierarchy
                if e.code == -32601 {
                    debug!("LSP server does not support call hierarchy: {}", e.message);
                    return Ok(None);
                }
                return Err(Error::Rpc {
                    code: e.code,
                    message: e.message,
                });
            }
        };

        // Handle null result (function not found)
        if result.is_null() {
            return Ok(Some(vec![]));
        }

        let items: Vec<CallHierarchyItem> = serde_json::from_value(result)?;
        Ok(Some(items))
    }

    /// Get outgoing calls from a call hierarchy item
    pub async fn get_outgoing_calls(
        &self,
        item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyOutgoingCall>> {
        let client_guard = self.manager.client().await?;
        let client = client_guard
            .as_ref()
            .ok_or_else(|| Error::ServerNotAvailable("No LSP client available".into()))?;

        let params = CallHierarchyOutgoingCallsParams { item: item.clone() };

        let response = client
            .request("callHierarchy/outgoingCalls", Some(json!(params)))
            .await?;

        let result = response.into_result().map_err(|e| Error::Rpc {
            code: e.code,
            message: e.message,
        })?;

        // Handle null result
        if result.is_null() {
            return Ok(vec![]);
        }

        let calls: Vec<CallHierarchyOutgoingCall> = serde_json::from_value(result)?;
        Ok(calls)
    }

    /// Analyze a function with cache lookup and timeout wrapper
    ///
    /// This is the common implementation pattern used by all language-specific analyzers:
    /// 1. Check cache first (fast path)
    /// 2. Apply timeout wrapper to the inner analysis
    /// 3. Return Unknown on timeout
    ///
    /// # Arguments
    /// * `function_name` - Name of the function to analyze
    /// * `file_path` - Path to the source file
    /// * `inner_fn` - The language-specific analysis function to call
    ///
    /// # Returns
    /// The cached result, the inner function result, or Unknown on timeout
    pub async fn analyze_with_timeout<F, Fut>(
        &self,
        function_name: &str,
        file_path: &Path,
        inner_fn: F,
    ) -> detrix_core::Result<PurityAnalysis>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = detrix_core::Result<PurityAnalysis>>,
    {
        // Check cache first (using shared cache module) - outside timeout since it's fast
        if let Some(cached) =
            cache::get_cached(self.cache(), self.config(), function_name, file_path).await
        {
            return Ok(cached);
        }

        // Apply timeout to the actual analysis (LSP operations can be slow)
        let timeout_ms = self.config().analysis_timeout_ms;
        let timeout_duration = Duration::from_millis(timeout_ms);

        match tokio::time::timeout(timeout_duration, inner_fn()).await {
            Ok(result) => result,
            Err(_elapsed) => {
                warn!(
                    function = %function_name,
                    timeout_ms = timeout_ms,
                    "LSP purity analysis timed out, returning Unknown"
                );
                // Return Unknown on timeout rather than error, so caller can proceed
                Ok(PurityAnalysis {
                    level: PurityLevel::Unknown,
                    impure_calls: Vec::new(),
                    unknown_calls: Vec::new(),
                    max_depth_reached: false,
                })
            }
        }
    }

    // ========================================================================
    // Shared Purity Analysis Implementation
    // ========================================================================

    /// Analyze a function's body for purity by extracting and checking for impure calls
    ///
    /// This is the shared implementation used by all language analyzers.
    /// Language-specific behavior is delegated to the `LanguageAnalyzer` trait.
    pub async fn analyze_function_body_for_purity<A: LanguageAnalyzer + ?Sized>(
        &self,
        analyzer: &A,
        function_name: &str,
        file_path: &Path,
    ) -> Result<PurityAnalysis> {
        let mut visited = HashSet::new();
        let mut all_impure_calls = Vec::new();
        let mut unknown_calls = Vec::new();
        let mut max_depth_reached = false;

        self.analyze_function_recursively(
            analyzer,
            function_name,
            file_path,
            0,
            &mut visited,
            &mut all_impure_calls,
            &mut unknown_calls,
            &mut max_depth_reached,
            function_name,
        )
        .await?;

        let level = if !all_impure_calls.is_empty() {
            PurityLevel::Impure
        } else if !unknown_calls.is_empty() || max_depth_reached {
            PurityLevel::Unknown
        } else {
            PurityLevel::Pure
        };

        Ok(PurityAnalysis {
            level,
            impure_calls: all_impure_calls,
            unknown_calls,
            max_depth_reached,
        })
    }

    /// Recursively analyze a function and its callees for purity
    ///
    /// Shared implementation that delegates language-specific extraction and
    /// body analysis to the `LanguageAnalyzer` trait.
    #[allow(clippy::too_many_arguments)]
    pub async fn analyze_function_recursively<A: LanguageAnalyzer + ?Sized>(
        &self,
        analyzer: &A,
        function_name: &str,
        file_path: &Path,
        depth: usize,
        visited: &mut HashSet<String>,
        impure_calls: &mut Vec<ImpureCall>,
        unknown_calls: &mut Vec<String>,
        max_depth_reached: &mut bool,
        call_path: &str,
    ) -> Result<()> {
        // Check max depth
        if depth > self.config().max_call_depth {
            debug!(
                "Max call depth {} reached at {}",
                self.config().max_call_depth,
                function_name
            );
            *max_depth_reached = true;
            return Ok(());
        }

        // Prevent cycles
        let key = format!("{}:{}", file_path.display(), function_name);
        if visited.contains(&key) {
            trace!("Skipping already visited: {}", key);
            return Ok(());
        }
        visited.insert(key);

        // Extract function body with parameters (language-specific)
        let extracted = match analyzer.extract_function(function_name, file_path).await {
            Ok(result) => result,
            Err(e) => {
                trace!("Function {} not found: {}", function_name, e);
                unknown_calls.push(function_name.to_string());
                return Ok(());
            }
        };

        trace!(
            function = %function_name,
            depth = %depth,
            params = ?extracted.parameters,
            "Analyzing function body"
        );

        // Analyze the body for impure calls and user function calls (language-specific)
        let body_result = analyzer.analyze_body(&extracted);

        // Add direct impure calls with full call path
        for mut call in body_result.impure_calls {
            call.call_path = format!("{} -> {}", call_path, call.function_name);
            impure_calls.push(call);
        }

        // Recursively analyze user function calls
        for user_fn in body_result.user_function_calls {
            let new_call_path = format!("{} -> {}", call_path, user_fn);
            Box::pin(self.analyze_function_recursively(
                analyzer,
                &user_fn,
                file_path,
                depth + 1,
                visited,
                impure_calls,
                unknown_calls,
                max_depth_reached,
                &new_call_path,
            ))
            .await?;
        }

        Ok(())
    }

    // ========================================================================
    // High-Level Analysis Entry Points
    // ========================================================================

    /// Analyze a function using call hierarchy with body analysis fallback
    ///
    /// This is the shared implementation pattern used by Go and Rust analyzers:
    /// 1. Quick classification for known functions (returns early if found)
    /// 2. Ensure LSP is ready
    /// 3. Prepare call hierarchy at (0,0) with fallback on failure
    /// 4. Find matching item or fallback to body analysis
    /// 5. Traverse call hierarchy with double-check body analysis
    /// 6. Cache and return result
    ///
    /// Python uses a different pattern (precise positioning via find_function_location).
    pub async fn analyze_with_call_hierarchy<A: LanguageAnalyzer + ?Sized>(
        &self,
        analyzer: &A,
        function_name: &str,
        file_path: &Path,
        classify_fn: impl Fn(&str) -> PurityLevel,
    ) -> detrix_core::Result<PurityAnalysis> {
        // 1. Quick classification for known functions
        let classification = classify_fn(function_name);
        if classification != PurityLevel::Unknown {
            let analysis = PurityAnalysis {
                level: classification,
                impure_calls: if classification == PurityLevel::Impure {
                    vec![ImpureCall {
                        function_name: function_name.to_string(),
                        reason: "Known impure function".to_string(),
                        call_path: function_name.to_string(),
                    }]
                } else {
                    vec![]
                },
                unknown_calls: vec![],
                max_depth_reached: false,
            };
            cache::cache_result(
                self.cache(),
                self.config(),
                function_name,
                file_path,
                &analysis,
            )
            .await;
            return Ok(analysis);
        }

        // 2. Ensure LSP is ready
        self.ensure_started().await?;

        // 3. Try to find the function using call hierarchy at (0,0)
        let items = match self.prepare_call_hierarchy(file_path, 0, 0).await {
            Ok(Some(items)) => items,
            Ok(None) => {
                // Fall back to function body analysis (call hierarchy not supported)
                let analysis = self
                    .analyze_function_body_for_purity(analyzer, function_name, file_path)
                    .await?;
                debug!(
                    function = %function_name,
                    level = ?analysis.level,
                    "Using function body analysis (call hierarchy not supported)"
                );
                cache::cache_result(
                    self.cache(),
                    self.config(),
                    function_name,
                    file_path,
                    &analysis,
                )
                .await;
                return Ok(analysis);
            }
            Err(e) => {
                warn!(
                    "Failed to prepare call hierarchy: {}, falling back to body analysis",
                    e
                );
                // Fall back to function body analysis on error
                let analysis = self
                    .analyze_function_body_for_purity(analyzer, function_name, file_path)
                    .await?;
                cache::cache_result(
                    self.cache(),
                    self.config(),
                    function_name,
                    file_path,
                    &analysis,
                )
                .await;
                return Ok(analysis);
            }
        };

        // 4. Find the item matching our function name
        let item = items.iter().find(|i| i.name == function_name);

        let analysis = match item {
            Some(item) => {
                let mut visited = HashSet::new();
                let result = self
                    .traverse_call_hierarchy(analyzer, item, &mut visited, 0)
                    .await?;

                // 5. If LSP returned empty results, double-check with body analysis
                if result.impure_calls.is_empty()
                    && result.unknown_calls.is_empty()
                    && result.level == PurityLevel::Pure
                {
                    let body_result = self
                        .analyze_function_body_for_purity(analyzer, function_name, file_path)
                        .await;
                    if let Ok(body_analysis) = body_result {
                        if !body_analysis.impure_calls.is_empty() {
                            debug!(
                                function = %function_name,
                                "Body analysis found impure calls not detected by LSP"
                            );
                            cache::cache_result(
                                self.cache(),
                                self.config(),
                                function_name,
                                file_path,
                                &body_analysis,
                            )
                            .await;
                            return Ok(body_analysis);
                        }
                    }
                }
                result
            }
            None => {
                // Function not found via LSP, use body analysis
                self.analyze_function_body_for_purity(analyzer, function_name, file_path)
                    .await?
            }
        };

        // 6. Cache the result
        cache::cache_result(
            self.cache(),
            self.config(),
            function_name,
            file_path,
            &analysis,
        )
        .await;

        Ok(analysis)
    }

    /// Traverse call hierarchy recursively to determine purity
    ///
    /// Shared implementation for LSP-based call hierarchy analysis.
    /// Uses `classify_function` from the analyzer for language-specific classification.
    pub fn traverse_call_hierarchy<'a, A: LanguageAnalyzer + ?Sized>(
        &'a self,
        analyzer: &'a A,
        item: &'a CallHierarchyItem,
        visited: &'a mut HashSet<String>,
        depth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<PurityAnalysis>> + Send + 'a>>
    {
        Box::pin(async move {
            let max_depth = self.config().max_call_depth;

            // Check depth limit
            if depth > max_depth {
                return Ok(PurityAnalysis {
                    level: PurityLevel::Unknown,
                    impure_calls: vec![],
                    unknown_calls: vec![format!("Max depth {} reached", max_depth)],
                    max_depth_reached: true,
                });
            }

            // Prevent cycles
            let item_key = format!("{}:{}", item.uri, item.name);
            if visited.contains(&item_key) {
                return Ok(PurityAnalysis {
                    level: PurityLevel::Pure,
                    impure_calls: vec![],
                    unknown_calls: vec![],
                    max_depth_reached: false,
                });
            }
            visited.insert(item_key);

            // Get outgoing calls
            let outgoing_calls = self.get_outgoing_calls(item).await.unwrap_or_else(|e| {
                warn!("Failed to get outgoing calls: {}", e);
                vec![]
            });

            let mut worst_level = PurityLevel::Pure;
            let mut all_impure_calls = Vec::new();
            let mut all_unknown_calls = Vec::new();
            let mut max_depth_reached = false;

            for call in outgoing_calls {
                let called_name = &call.to.name;
                let classification = analyzer.classify_function(called_name);

                match classification {
                    PurityLevel::Impure => {
                        all_impure_calls.push(ImpureCall {
                            function_name: called_name.clone(),
                            reason: "Known impure function".to_string(),
                            call_path: format!("{} -> {}", item.name, called_name),
                        });
                        worst_level = worst_level.combine(PurityLevel::Impure);
                    }
                    PurityLevel::Pure => {
                        trace!(function = %called_name, "Known pure function");
                    }
                    PurityLevel::Unknown => {
                        // Recurse into unknown functions
                        let sub_analysis = self
                            .traverse_call_hierarchy(analyzer, &call.to, visited, depth + 1)
                            .await?;

                        if sub_analysis.max_depth_reached {
                            max_depth_reached = true;
                        }

                        match sub_analysis.level {
                            PurityLevel::Impure => {
                                // Extend the call path for nested impure calls
                                for mut nested_call in sub_analysis.impure_calls {
                                    nested_call.call_path =
                                        format!("{} -> {}", item.name, nested_call.call_path);
                                    all_impure_calls.push(nested_call);
                                }
                                worst_level = worst_level.combine(PurityLevel::Impure);
                            }
                            PurityLevel::Unknown => {
                                all_unknown_calls.extend(sub_analysis.unknown_calls);
                                worst_level = worst_level.combine(PurityLevel::Unknown);
                            }
                            PurityLevel::Pure => {
                                // Pure recursive call, no action needed
                            }
                        }
                    }
                }
            }

            Ok(PurityAnalysis {
                level: worst_level,
                impure_calls: all_impure_calls,
                unknown_calls: all_unknown_calls,
                max_depth_reached,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> LspConfig {
        LspConfig {
            enabled: true,
            command: "test".to_string(),
            args: vec![],
            max_call_depth: 10,
            analysis_timeout_ms: 5000,
            cache_enabled: true,
            working_directory: None,
        }
    }

    #[test]
    fn test_base_analyzer_creation() {
        let config = test_config();
        let analyzer = BasePurityAnalyzer::new(config.clone());

        assert_eq!(analyzer.config().command, "test");
        assert!(analyzer.cache().is_none());
    }

    #[test]
    fn test_base_analyzer_with_cache() {
        use async_trait::async_trait;
        use detrix_application::PurityCache;
        use std::sync::Arc;

        // Create a mock cache
        struct MockCache;

        #[async_trait]
        impl PurityCache for MockCache {
            async fn get(
                &self,
                _key: &detrix_application::PurityCacheKey,
            ) -> std::result::Result<Option<detrix_application::PurityCacheEntry>, detrix_core::Error>
            {
                Ok(None)
            }

            async fn set(
                &self,
                _key: &detrix_application::PurityCacheKey,
                _entry: &detrix_application::PurityCacheEntry,
            ) -> std::result::Result<(), detrix_core::Error> {
                Ok(())
            }

            async fn remove(
                &self,
                _key: &detrix_application::PurityCacheKey,
            ) -> std::result::Result<bool, detrix_core::Error> {
                Ok(false)
            }

            async fn invalidate_file(
                &self,
                _file_path: &str,
            ) -> std::result::Result<usize, detrix_core::Error> {
                Ok(0)
            }

            async fn clear(&self) -> std::result::Result<usize, detrix_core::Error> {
                Ok(0)
            }

            async fn stats(
                &self,
            ) -> std::result::Result<detrix_application::PurityCacheStats, detrix_core::Error>
            {
                Ok(detrix_application::PurityCacheStats {
                    hits: 0,
                    misses: 0,
                    entry_count: 0,
                })
            }
        }

        let config = test_config();
        let cache: CacheRef = Arc::new(MockCache);
        let analyzer = BasePurityAnalyzer::with_cache(config, cache);

        assert!(analyzer.cache().is_some());
    }

    #[tokio::test]
    async fn test_set_workspace_root() {
        let config = test_config();
        let analyzer = BasePurityAnalyzer::new(config);

        let path = PathBuf::from("/test/workspace");
        analyzer.set_workspace_root(path.clone()).await;

        let root = analyzer.get_workspace_root().await;
        assert_eq!(root, path);
    }

    #[tokio::test]
    async fn test_get_workspace_root_default() {
        let config = test_config();
        let analyzer = BasePurityAnalyzer::new(config);

        // Without setting, should return current dir or "."
        let root = analyzer.get_workspace_root().await;
        assert!(!root.as_os_str().is_empty());
    }

    #[test]
    fn test_set_cache_after_creation() {
        use async_trait::async_trait;
        use detrix_application::PurityCache;
        use std::sync::Arc;

        struct MockCache;

        #[async_trait]
        impl PurityCache for MockCache {
            async fn get(
                &self,
                _key: &detrix_application::PurityCacheKey,
            ) -> std::result::Result<Option<detrix_application::PurityCacheEntry>, detrix_core::Error>
            {
                Ok(None)
            }

            async fn set(
                &self,
                _key: &detrix_application::PurityCacheKey,
                _entry: &detrix_application::PurityCacheEntry,
            ) -> std::result::Result<(), detrix_core::Error> {
                Ok(())
            }

            async fn remove(
                &self,
                _key: &detrix_application::PurityCacheKey,
            ) -> std::result::Result<bool, detrix_core::Error> {
                Ok(false)
            }

            async fn invalidate_file(
                &self,
                _file_path: &str,
            ) -> std::result::Result<usize, detrix_core::Error> {
                Ok(0)
            }

            async fn clear(&self) -> std::result::Result<usize, detrix_core::Error> {
                Ok(0)
            }

            async fn stats(
                &self,
            ) -> std::result::Result<detrix_application::PurityCacheStats, detrix_core::Error>
            {
                Ok(detrix_application::PurityCacheStats {
                    hits: 0,
                    misses: 0,
                    entry_count: 0,
                })
            }
        }

        let config = test_config();
        let mut analyzer = BasePurityAnalyzer::new(config);

        assert!(analyzer.cache().is_none());

        let cache: CacheRef = Arc::new(MockCache);
        analyzer.set_cache(cache);

        assert!(analyzer.cache().is_some());
    }
}
