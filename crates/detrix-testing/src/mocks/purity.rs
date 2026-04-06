//! Mock PurityAnalyzer for testing LSP purity resolution
//!
//! Configurable mock that returns predetermined purity analysis results
//! for specific function names. Tracks calls for test assertions.

use async_trait::async_trait;
use detrix_core::{ImpureCall, PurityAnalysis, Result, SourceLanguage};
use detrix_ports::PurityAnalyzer;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// A configurable mock PurityAnalyzer for testing.
///
/// Pre-configure responses per function name, then inject into MetricService.
/// Tracks all calls for assertions.
///
/// # Example
///
/// ```no_run
/// use detrix_testing::MockPurityAnalyzer;
/// use detrix_core::{PurityLevel, SourceLanguage};
///
/// let mock = MockPurityAnalyzer::new(SourceLanguage::Python)
///     .with_pure("calculate_total")
///     .with_impure("save_to_database", "writes to file system");
/// ```
pub struct MockPurityAnalyzer {
    language: SourceLanguage,
    /// Pre-configured responses: function_name → PurityAnalysis
    responses: Mutex<HashMap<String, PurityAnalysis>>,
    /// Default response for functions not in the map
    default_response: Mutex<PurityAnalysis>,
    /// Whether ensure_ready() has been called
    ready: AtomicBool,
    /// Count of analyze_function() calls
    analyze_count: AtomicUsize,
    /// Count of ensure_ready() calls
    ready_count: AtomicUsize,
    /// Analyzed function names (for assertions)
    analyzed_functions: Mutex<Vec<String>>,
    /// If set, ensure_ready() will return this error
    ready_error: Mutex<Option<String>>,
}

impl MockPurityAnalyzer {
    /// Create a new mock analyzer for the given language.
    /// Default response is `PurityLevel::Unknown`.
    pub fn new(language: SourceLanguage) -> Self {
        Self {
            language,
            responses: Mutex::new(HashMap::new()),
            default_response: Mutex::new(PurityAnalysis::unknown(vec![])),
            ready: AtomicBool::new(false),
            analyze_count: AtomicUsize::new(0),
            ready_count: AtomicUsize::new(0),
            analyzed_functions: Mutex::new(Vec::new()),
            ready_error: Mutex::new(None),
        }
    }

    /// Configure a function to resolve as Pure.
    pub fn with_pure(self, function_name: &str) -> Self {
        self.responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(function_name.to_string(), PurityAnalysis::pure());
        self
    }

    /// Configure a function to resolve as Impure with a reason.
    pub fn with_impure(self, function_name: &str, reason: &str) -> Self {
        let analysis = PurityAnalysis::impure(vec![ImpureCall::new(function_name, reason)]);
        self.responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(function_name.to_string(), analysis);
        self
    }

    /// Configure ensure_ready() to fail with an error.
    pub fn with_ready_error(self, error: &str) -> Self {
        *self.ready_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
        self
    }

    /// Get the number of analyze_function() calls.
    pub fn analyze_count(&self) -> usize {
        self.analyze_count.load(Ordering::SeqCst)
    }

    /// Get the number of ensure_ready() calls.
    pub fn ready_count(&self) -> usize {
        self.ready_count.load(Ordering::SeqCst)
    }

    /// Get the list of function names that were analyzed.
    pub fn analyzed_functions(&self) -> Vec<String> {
        self.analyzed_functions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl std::fmt::Debug for MockPurityAnalyzer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockPurityAnalyzer")
            .field("language", &self.language)
            .field("analyze_count", &self.analyze_count())
            .finish()
    }
}

#[async_trait]
impl PurityAnalyzer for MockPurityAnalyzer {
    async fn analyze_function(
        &self,
        function_name: &str,
        _file_path: &Path,
    ) -> Result<PurityAnalysis> {
        self.analyze_count.fetch_add(1, Ordering::SeqCst);
        self.analyzed_functions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(function_name.to_string());

        let responses = self.responses.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(analysis) = responses.get(function_name) {
            Ok(analysis.clone())
        } else {
            Ok(self
                .default_response
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone())
        }
    }

    fn is_available(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    async fn ensure_ready(&self) -> Result<()> {
        self.ready_count.fetch_add(1, Ordering::SeqCst);
        if let Some(err) = self
            .ready_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return Err(detrix_core::Error::Io(err.clone()));
        }
        self.ready.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn language(&self) -> SourceLanguage {
        self.language
    }
}
