//! Python-specific LSP purity analyzer
//!
//! Implements the PurityAnalyzer trait using a Python LSP server
//! (pylsp or pyright) for call hierarchy analysis.
//!
//! Supports optional caching of purity analysis results to avoid
//! repeated LSP calls for the same function/file combinations.
//!
//! Function classification constants are in `detrix_config`.

use crate::base::{BasePurityAnalyzer, BodyAnalysisResult, ExtractedFunction, LanguageAnalyzer};
use crate::cache::CacheRef;
use crate::common::ScopeTracker;
use crate::error::{Error, Result, ToLspResult};
use crate::mutation::{MutationTarget, PythonMutationTarget};
use async_trait::async_trait;
use detrix_application::PurityAnalyzer;
use detrix_config::{
    LspConfig, PYTHON_ACCEPTABLE_IMPURE, PYTHON_IMPURE_FUNCTIONS, PYTHON_MUTATION_METHODS,
    PYTHON_PURE_FUNCTIONS,
};
use detrix_core::{ImpureCall, PurityAnalysis, PurityLevel, SourceLanguage};
use std::collections::HashSet;
use std::path::Path;
use tracing::{trace, warn};

/// Represents the scope context for scope-aware purity analysis
/// Tracks local variables, parameters, and global declarations
///
/// Implements `ScopeTracker` for common interface while providing
/// Python-specific methods for global/nonlocal variable tracking.
#[derive(Debug, Default)]
struct ScopeContext {
    /// Variables created locally in this function (assignments)
    local_vars: HashSet<String>,
    /// Function parameters (mutations to these are impure)
    parameters: HashSet<String>,
    /// Variables declared with 'global' keyword
    global_vars: HashSet<String>,
    /// Variables declared with 'nonlocal' keyword
    nonlocal_vars: HashSet<String>,
}

impl ScopeTracker for ScopeContext {
    fn with_parameters(parameters: Vec<String>) -> Self {
        Self {
            parameters: parameters.into_iter().collect(),
            ..Default::default()
        }
    }

    fn is_local(&self, name: &str) -> bool {
        self.local_vars.contains(name)
            && !self.parameters.contains(name)
            && !self.global_vars.contains(name)
            && !self.nonlocal_vars.contains(name)
    }

    fn is_parameter(&self, name: &str) -> bool {
        self.parameters.contains(name)
    }

    fn add_local(&mut self, name: String) {
        // Don't add if it's a parameter or global/nonlocal
        if !self.parameters.contains(&name)
            && !self.global_vars.contains(&name)
            && !self.nonlocal_vars.contains(&name)
        {
            self.local_vars.insert(name);
        }
    }
}

impl ScopeContext {
    /// Create a new scope context with given parameters
    fn new(parameters: Vec<String>) -> Self {
        <Self as ScopeTracker>::with_parameters(parameters)
    }

    /// Check if a variable is declared global
    fn is_global(&self, name: &str) -> bool {
        self.global_vars.contains(name)
    }

    /// Check if a variable is declared nonlocal
    fn is_nonlocal(&self, name: &str) -> bool {
        self.nonlocal_vars.contains(name)
    }

    /// Add a global declaration
    fn add_global(&mut self, name: String) {
        // Remove from local_vars if it was there
        self.local_vars.remove(&name);
        self.global_vars.insert(name);
    }

    /// Add a nonlocal declaration
    fn add_nonlocal(&mut self, name: String) {
        // Remove from local_vars if it was there
        self.local_vars.remove(&name);
        self.nonlocal_vars.insert(name);
    }
}

// MutationTarget trait and PythonMutationTarget are imported from crate::mutation

/// Python purity analyzer using LSP
///
/// This analyzer uses the BasePurityAnalyzer for common LSP operations
/// and provides Python-specific function classification and mutation detection.
pub struct PythonPurityAnalyzer {
    /// Base analyzer with common LSP operations
    base: BasePurityAnalyzer,
}

impl PythonPurityAnalyzer {
    /// Create a new Python purity analyzer without cache
    pub fn new(config: &LspConfig) -> Result<Self> {
        Ok(Self {
            base: BasePurityAnalyzer::new(config.clone()),
        })
    }

    /// Create a new Python purity analyzer with cache
    pub fn with_cache(config: &LspConfig, cache: CacheRef) -> Result<Self> {
        Ok(Self {
            base: BasePurityAnalyzer::with_cache(config.clone(), cache),
        })
    }

    /// Set the cache after creation
    pub fn set_cache(&mut self, cache: CacheRef) {
        self.base.set_cache(cache);
    }

    /// Set the workspace root
    ///
    /// Takes ownership of the path to store it internally.
    pub async fn set_workspace_root(&self, path: std::path::PathBuf) {
        self.base.set_workspace_root(path).await;
    }

    /// Classify a function name as pure, impure, or unknown
    ///
    /// This is used for external functions (stdlib, site-packages) where we
    /// don't traverse the implementation - we just check against our lists.
    fn classify_function(name: &str) -> PurityLevel {
        // First check acceptable impure (logging) - these don't affect purity
        if PYTHON_ACCEPTABLE_IMPURE
            .iter()
            .any(|&f| name.ends_with(f) || name == f)
        {
            trace!("Acceptable impure function (logging): {}", name);
            return PurityLevel::Pure; // Treat as pure for purity analysis
        }

        // Check truly impure list (always impure regardless of context)
        if PYTHON_IMPURE_FUNCTIONS
            .iter()
            .any(|&f| name.ends_with(f) || name == f)
        {
            return PurityLevel::Impure;
        }

        // Check pure functions (exact match or ends_with for qualified names)
        if PYTHON_PURE_FUNCTIONS
            .iter()
            .any(|&p| name == p || name.ends_with(p))
        {
            return PurityLevel::Pure;
        }

        // Unknown - not in any list
        PurityLevel::Unknown
    }

    /// Extract function body with parameters for scope-aware analysis
    ///
    /// Returns (body, parameters) tuple where:
    /// - body: The function body as a string (lines after def)
    /// - parameters: Extracted parameter names from the function signature
    async fn extract_function_with_params(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> Result<(String, Vec<String>)> {
        use crate::common::extract_body_with_indent;

        let content = tokio::fs::read_to_string(file_path)
            .await
            .lsp_file_not_found(file_path)?;

        let lines: Vec<&str> = content.lines().collect();
        let patterns = [
            format!("def {}(", function_name),
            format!("async def {}(", function_name),
        ];

        // Find the function definition line
        let mut func_start = None;
        let mut base_indent = 0;
        let mut def_line = String::new();

        for (line_num, line) in lines.iter().enumerate() {
            for pattern in &patterns {
                if line.contains(pattern) {
                    func_start = Some(line_num);
                    // Calculate base indentation
                    base_indent = line.len() - line.trim_start().len();
                    def_line = (*line).to_string();
                    break;
                }
            }
            if func_start.is_some() {
                break;
            }
        }

        let start = func_start.ok_or_else(|| Error::FunctionNotFound {
            function: function_name.to_string(),
            file: file_path.display().to_string(),
        })?;

        // Extract parameters from the definition line
        let parameters = Self::extract_parameters(&def_line);

        // Use shared indentation-based body extraction
        let body = extract_body_with_indent(&lines, start, base_indent);

        Ok((body, parameters))
    }

    /// Extract function parameters from a function definition line
    fn extract_parameters(def_line: &str) -> Vec<String> {
        let mut params = Vec::new();

        // Find content between first ( and )
        if let Some(start) = def_line.find('(') {
            if let Some(end) = def_line.rfind(')') {
                let param_str = &def_line[start + 1..end];
                // Split by comma and extract parameter names
                for param in param_str.split(',') {
                    let param = param.trim();
                    if param.is_empty() {
                        continue;
                    }
                    // Handle type annotations: param: Type = default -> param
                    let name = param
                        .split(':')
                        .next()
                        .unwrap_or(param)
                        .split('=')
                        .next()
                        .unwrap_or(param)
                        .trim();

                    // Skip *args and **kwargs but keep the name without *
                    let name = name.trim_start_matches('*');
                    if !name.is_empty() {
                        params.push(name.to_string());
                    }
                }
            }
        }
        params
    }

    /// Build scope context by analyzing the function body
    /// Tracks local variable assignments, global/nonlocal declarations
    fn build_scope_context(body: &str, parameters: Vec<String>) -> ScopeContext {
        let mut ctx = ScopeContext::new(parameters);

        for line in body.lines() {
            let trimmed = line.trim();

            // Skip comments
            if trimmed.starts_with('#') {
                continue;
            }

            // Track global declarations: global x, y, z
            if trimmed.starts_with("global ") {
                let rest = trimmed.strip_prefix("global ").unwrap_or("");
                for var in rest.split(',') {
                    let var = var.trim();
                    if !var.is_empty() {
                        ctx.add_global(var.to_string());
                    }
                }
                continue;
            }

            // Track nonlocal declarations: nonlocal x, y, z
            if trimmed.starts_with("nonlocal ") {
                let rest = trimmed.strip_prefix("nonlocal ").unwrap_or("");
                for var in rest.split(',') {
                    let var = var.trim();
                    if !var.is_empty() {
                        ctx.add_nonlocal(var.to_string());
                    }
                }
                continue;
            }

            // Track assignments: var = ... or var: Type = ...
            // Simple pattern: starts with identifier, possibly with type annotation, then =
            if let Some(equals_pos) = trimmed.find('=') {
                // Make sure it's not ==, !=, <=, >=, etc.
                if equals_pos > 0 {
                    let before = &trimmed[..equals_pos];
                    let prev_char = before.chars().last().unwrap_or(' ');
                    if prev_char == '!' || prev_char == '<' || prev_char == '>' || prev_char == '='
                    {
                        continue;
                    }
                }

                // Check if after = there's another = (==)
                if trimmed.len() > equals_pos + 1 {
                    let after_eq = trimmed.chars().nth(equals_pos + 1);
                    if after_eq == Some('=') {
                        continue;
                    }
                }

                let lhs = &trimmed[..equals_pos];
                // Skip augmented assignments: +=, -=, *=, /=, etc.
                if lhs.ends_with('+')
                    || lhs.ends_with('-')
                    || lhs.ends_with('*')
                    || lhs.ends_with('/')
                    || lhs.ends_with('%')
                    || lhs.ends_with('|')
                    || lhs.ends_with('&')
                    || lhs.ends_with('^')
                {
                    continue;
                }

                // Handle type annotations: var: Type = ...
                let var_part = lhs.split(':').next().unwrap_or(lhs).trim();

                // Simple variable assignment (no dots, no brackets)
                if var_part.chars().all(|c| c.is_alphanumeric() || c == '_') && !var_part.is_empty()
                {
                    ctx.add_local(var_part.to_string());
                }
            }
        }

        ctx
    }

    /// Classify a mutation target based on scope context
    fn classify_mutation_target(target: &str, ctx: &ScopeContext) -> PythonMutationTarget {
        // Check for self.xxx pattern (instance mutation)
        if target.starts_with("self.") || target == "self" {
            return PythonMutationTarget::SelfAttribute;
        }

        // Get the base variable name (before any dots or brackets)
        let base_name = target
            .split('.')
            .next()
            .unwrap_or(target)
            .split('[')
            .next()
            .unwrap_or(target)
            .trim();

        // Check against scope context
        if ctx.is_global(base_name) {
            return PythonMutationTarget::Global;
        }
        if ctx.is_nonlocal(base_name) {
            return PythonMutationTarget::Nonlocal;
        }
        if ctx.is_parameter(base_name) {
            return PythonMutationTarget::Parameter;
        }
        if ctx.is_local(base_name) {
            return PythonMutationTarget::Local;
        }

        // Unknown - conservatively treat as impure
        PythonMutationTarget::Unknown
    }

    /// Check if a line contains a mutation method call on a non-local target
    /// Returns Some((target, method, target_type)) if impure mutation detected, None otherwise
    fn detect_impure_mutation(
        line: &str,
        ctx: &ScopeContext,
    ) -> Option<(String, String, PythonMutationTarget)> {
        // Skip comments
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }

        // Look for patterns like: target.method(
        for method in PYTHON_MUTATION_METHODS {
            let pattern = format!(".{}(", method);
            if let Some(pos) = line.find(&pattern) {
                // Extract the target (everything before the .method)
                let before = &line[..pos];

                // Simple extraction: take the last identifier/expression
                // This handles: target.method(, target.method(, foo().method(
                let target = Self::extract_mutation_target(before);

                if !target.is_empty() {
                    let target_type = Self::classify_mutation_target(&target, ctx);
                    if target_type.is_impure() {
                        return Some((target, method.to_string(), target_type));
                    }
                }
            }
        }

        None
    }

    /// Extract the mutation target from an expression
    /// e.g., "self.items" from "self.items.append(x)"
    /// or "result" from "result.append(x)"
    fn extract_mutation_target(expr: &str) -> String {
        let expr = expr.trim();

        // Walk backwards to find the start of the identifier/attribute chain
        let chars: Vec<char> = expr.chars().collect();
        let mut end = chars.len();
        let mut parens = 0;
        let mut brackets = 0;

        // Skip trailing whitespace
        while end > 0 && chars[end - 1].is_whitespace() {
            end -= 1;
        }

        // Find the start of the target expression
        let mut start = end;
        while start > 0 {
            let c = chars[start - 1];
            match c {
                ')' => parens += 1,
                '(' => {
                    if parens > 0 {
                        parens -= 1;
                    } else {
                        break;
                    }
                }
                ']' => brackets += 1,
                '[' => {
                    if brackets > 0 {
                        brackets -= 1;
                    } else {
                        break;
                    }
                }
                '.' | '_' => start -= 1,
                c if c.is_alphanumeric() => start -= 1,
                _ if parens > 0 || brackets > 0 => start -= 1,
                _ => break,
            }
            if start > 0 && parens == 0 && brackets == 0 {
                let prev = chars[start - 1];
                if prev.is_alphanumeric()
                    || prev == '_'
                    || prev == '.'
                    || prev == ')'
                    || prev == ']'
                {
                    start -= 1;
                }
            }
        }

        chars[start..end].iter().collect::<String>()
    }

    /// Analyze function body with known parameters for scope-aware mutation detection
    /// Returns (is_pure, impure_calls, user_function_calls)
    fn analyze_function_body_with_params(
        &self,
        body: &str,
        parameters: Vec<String>,
    ) -> (bool, Vec<ImpureCall>, Vec<String>) {
        let mut impure_calls = Vec::new();
        let mut user_calls = Vec::new();

        // Build scope context for scope-aware mutation detection
        let ctx = Self::build_scope_context(body, parameters);

        // Check for global/nonlocal statements
        // Note: Having a global/nonlocal statement doesn't make a function impure by itself,
        // only if the variable is actually mutated. But we track them for now.
        let has_global = body.lines().any(|l| l.trim().starts_with("global "));
        let has_nonlocal = body.lines().any(|l| l.trim().starts_with("nonlocal "));

        if has_global {
            trace!("Function declares global variables - checking for mutations");
        }
        if has_nonlocal {
            trace!("Function declares nonlocal variables - checking for mutations");
        }

        // Check for scope-aware mutation methods
        for line in body.lines() {
            if let Some((target, method, target_type)) = Self::detect_impure_mutation(line, &ctx) {
                if let Some(reason) = target_type.impurity_reason() {
                    impure_calls.push(ImpureCall {
                        function_name: format!("{}.{}", target, method),
                        reason: reason.to_string(),
                        call_path: format!("{}.{}", target, method),
                    });
                }
            }
        }

        // Check for truly impure function calls
        for impure_fn in PYTHON_IMPURE_FUNCTIONS {
            // Skip acceptable impure functions (logging, print) - they don't affect purity
            if PYTHON_ACCEPTABLE_IMPURE.contains(impure_fn) {
                continue;
            }

            // Handle both simple calls like "open(" and method calls like "os.system("
            let patterns = [
                format!("{}(", impure_fn),   // Direct call: open(
                format!(" {}(", impure_fn),  // After space: x = open(
                format!("\t{}(", impure_fn), // After tab
                format!("({}(", impure_fn),  // Nested: foo(open(
            ];

            for pattern in &patterns {
                if body.contains(pattern) {
                    impure_calls.push(ImpureCall {
                        function_name: (*impure_fn).to_string(),
                        reason: format!("Truly impure function call: {}", impure_fn),
                        call_path: (*impure_fn).to_string(),
                    });
                    break; // Only add once per impure function
                }
            }
        }

        // Extract user function calls (simple pattern: identifier followed by "(")
        // This catches calls like: save_to_database(, write_log(, process_items(
        for line in body.lines() {
            let trimmed = line.trim();
            // Skip comments
            if trimmed.starts_with('#') {
                continue;
            }
            // Find function call patterns
            self.extract_function_calls(trimmed, &mut user_calls);
        }

        let is_pure = impure_calls.is_empty();
        (is_pure, impure_calls, user_calls)
    }

    /// Extract function call names from a line of code
    fn extract_function_calls(&self, line: &str, calls: &mut Vec<String>) {
        // Simple pattern matching for function calls: identifier(
        // Skip known pure functions, impure functions, and acceptable impure functions
        let mut i = 0;
        let chars: Vec<char> = line.chars().collect();

        while i < chars.len() {
            // Look for identifier followed by '('
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;

                // Check if this is a method call (preceded by '.')
                // Method calls like obj.method() should be skipped as they're not user function calls
                let is_method_call = start > 0 && chars[start - 1] == '.';

                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();

                // Check if followed by '('
                if i < chars.len() && chars[i] == '(' {
                    // Skip method calls (like list.append(), json.dumps(), math.floor())
                    if is_method_call {
                        i += 1;
                        continue;
                    }

                    // Skip keywords
                    let keywords = [
                        "if", "else", "elif", "for", "while", "with", "try", "except", "finally",
                        "def", "class", "return", "yield", "raise", "import", "from", "as", "in",
                        "and", "or", "not", "is", "lambda", "pass", "break", "continue", "assert",
                        "global", "nonlocal", "True", "False", "None",
                    ];
                    if keywords.contains(&name.as_str()) {
                        i += 1;
                        continue;
                    }

                    // Skip known pure functions
                    if PYTHON_PURE_FUNCTIONS
                        .iter()
                        .any(|p| name == *p || name.ends_with(p))
                    {
                        i += 1;
                        continue;
                    }

                    // Skip known truly impure functions (already tracked separately)
                    if PYTHON_IMPURE_FUNCTIONS.iter().any(|f| &name == f) {
                        i += 1;
                        continue;
                    }

                    // Skip acceptable impure functions (print, logging)
                    if PYTHON_ACCEPTABLE_IMPURE.iter().any(|f| &name == f) {
                        i += 1;
                        continue;
                    }

                    // This is a user function call
                    if !calls.contains(&name) {
                        calls.push(name);
                    }
                }
            } else {
                i += 1;
            }
        }
    }
}

#[async_trait]
impl PurityAnalyzer for PythonPurityAnalyzer {
    /// Analyze a function for purity
    ///
    /// If caching is enabled and a cache is configured, will check the cache first.
    /// Results are stored in cache for future lookups.
    ///
    /// When call hierarchy is not supported by the LSP server, falls back to
    /// static classification based on the function name only.
    ///
    /// The analysis is subject to a timeout configured via `analysis_timeout_ms`.
    async fn analyze_function(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> detrix_core::Result<PurityAnalysis> {
        self.base
            .analyze_with_timeout(function_name, file_path, || {
                self.analyze_function_inner(function_name, file_path)
            })
            .await
    }

    /// Check if the analyzer is available
    fn is_available(&self) -> bool {
        self.base.config().enabled
    }

    /// Ensure the analyzer is ready for use
    async fn ensure_ready(&self) -> detrix_core::Result<()> {
        if !self.base.config().enabled {
            return Err(detrix_core::Error::Safety(
                "LSP purity analysis is disabled".into(),
            ));
        }

        // Start the server if not already running
        self.base
            .ensure_started()
            .await
            .map_err(|e| detrix_core::Error::Safety(e.to_string()))?;

        // Verify call hierarchy is supported
        if !self.base.manager().supports_call_hierarchy().await {
            warn!("LSP server does not support call hierarchy");
            // Continue anyway - will use fallback classification
        }

        Ok(())
    }

    fn language(&self) -> SourceLanguage {
        SourceLanguage::Python
    }
}

/// Implementation of LanguageAnalyzer trait for shared purity analysis
#[async_trait]
impl LanguageAnalyzer for PythonPurityAnalyzer {
    fn language(&self) -> SourceLanguage {
        SourceLanguage::Python
    }

    fn classify_function(&self, name: &str) -> PurityLevel {
        // Delegate to the static method
        Self::classify_function(name)
    }

    async fn extract_function(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> Result<ExtractedFunction> {
        let (body, parameters) = self
            .extract_function_with_params(function_name, file_path)
            .await?;

        Ok(ExtractedFunction::new(body, parameters))
    }

    fn analyze_body(&self, extracted: &ExtractedFunction) -> BodyAnalysisResult {
        let (is_pure, impure_calls, user_function_calls) =
            self.analyze_function_body_with_params(&extracted.body, extracted.parameters.clone());

        BodyAnalysisResult {
            is_pure,
            impure_calls,
            user_function_calls,
        }
    }
}

impl PythonPurityAnalyzer {
    /// Inner analysis logic (called with timeout wrapper)
    ///
    /// Uses shared `analyze_with_call_hierarchy` implementation from BasePurityAnalyzer.
    async fn analyze_function_inner(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> detrix_core::Result<PurityAnalysis> {
        self.base
            .analyze_with_call_hierarchy(self, function_name, file_path, Self::classify_function)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> LspConfig {
        LspConfig {
            enabled: true,
            command: "pylsp".to_string(),
            args: vec![],
            max_call_depth: 10,
            analysis_timeout_ms: 5000,
            cache_enabled: true,
            working_directory: None,
        }
    }

    #[test]
    fn test_classify_impure_functions() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        // Note: print is now "acceptable impurity" (Unknown for classify_function)
        assert_eq!(analyzer.classify_function("open"), PurityLevel::Impure);
        assert_eq!(analyzer.classify_function("os.system"), PurityLevel::Impure);
        assert_eq!(analyzer.classify_function("eval"), PurityLevel::Impure);
    }

    #[test]
    fn test_classify_pure_functions() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        assert_eq!(analyzer.classify_function("len"), PurityLevel::Pure);
        assert_eq!(analyzer.classify_function("math.sqrt"), PurityLevel::Pure);
        assert_eq!(analyzer.classify_function("json.loads"), PurityLevel::Pure);
        assert_eq!(analyzer.classify_function("sorted"), PurityLevel::Pure);
    }

    #[test]
    fn test_classify_unknown_functions() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        assert_eq!(
            analyzer.classify_function("my_custom_func"),
            PurityLevel::Unknown
        );
        assert_eq!(
            analyzer.classify_function("calculate_total"),
            PurityLevel::Unknown
        );
    }

    #[test]
    fn test_analyzer_creation() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        assert!(analyzer.is_available());
        assert_eq!(PurityAnalyzer::language(&analyzer), SourceLanguage::Python);
    }

    #[test]
    fn test_disabled_analyzer() {
        let mut config = test_config();
        config.enabled = false;
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        assert!(!analyzer.is_available());
    }

    #[tokio::test]
    async fn test_set_workspace_root() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        let path = std::path::PathBuf::from("/test/workspace");
        analyzer.set_workspace_root(path.clone()).await;

        let root = analyzer.base.get_workspace_root().await;
        assert_eq!(root, path);
    }

    #[test]
    fn test_truly_impure_functions_list() {
        // Note: print is now in PYTHON_ACCEPTABLE_IMPURE, not PYTHON_IMPURE_FUNCTIONS
        assert!(PYTHON_IMPURE_FUNCTIONS.contains(&"eval"));
        assert!(PYTHON_IMPURE_FUNCTIONS.contains(&"open"));
        assert!(PYTHON_IMPURE_FUNCTIONS.contains(&"os.system"));
    }

    #[test]
    fn test_acceptable_impure_functions_list() {
        // These are I/O but don't affect program state
        assert!(PYTHON_ACCEPTABLE_IMPURE.contains(&"print"));
        assert!(PYTHON_ACCEPTABLE_IMPURE.contains(&"logging.info"));
    }

    #[test]
    fn test_pure_functions_list() {
        // Test specific pure functions from the explicit list
        assert!(PYTHON_PURE_FUNCTIONS.contains(&"math.sin"));
        assert!(PYTHON_PURE_FUNCTIONS.contains(&"math.cos"));
        assert!(PYTHON_PURE_FUNCTIONS.contains(&"len"));
        assert!(PYTHON_PURE_FUNCTIONS.contains(&"json.loads"));
        assert!(PYTHON_PURE_FUNCTIONS.contains(&"json.dumps"));
        // Built-in exception constructors
        assert!(PYTHON_PURE_FUNCTIONS.contains(&"ValueError"));
        assert!(PYTHON_PURE_FUNCTIONS.contains(&"TypeError"));
        // String methods
        assert!(PYTHON_PURE_FUNCTIONS.contains(&"str.format"));
    }

    // ========================================================================
    // SCOPE-AWARE PURITY ANALYSIS TESTS
    // ========================================================================

    #[test]
    fn test_scope_context_new() {
        let ctx = ScopeContext::new(vec!["items".to_string(), "value".to_string()]);
        assert!(ctx.is_parameter("items"));
        assert!(ctx.is_parameter("value"));
        assert!(!ctx.is_parameter("other"));
    }

    #[test]
    fn test_scope_context_local_vars() {
        let mut ctx = ScopeContext::new(vec!["param".to_string()]);
        ctx.add_local("result".to_string());
        ctx.add_local("temp".to_string());

        assert!(ctx.is_local("result"));
        assert!(ctx.is_local("temp"));
        // Parameter is NOT local
        assert!(!ctx.is_local("param"));
    }

    #[test]
    fn test_scope_context_global_vars() {
        let mut ctx = ScopeContext::new(vec![]);
        ctx.add_local("cache".to_string()); // First add as local
        ctx.add_global("cache".to_string()); // Then declare global

        // Should now be global, not local
        assert!(ctx.is_global("cache"));
        assert!(!ctx.is_local("cache"));
    }

    #[test]
    fn test_scope_context_nonlocal_vars() {
        let mut ctx = ScopeContext::new(vec![]);
        ctx.add_local("counter".to_string()); // First add as local
        ctx.add_nonlocal("counter".to_string()); // Then declare nonlocal

        // Should now be nonlocal, not local
        assert!(ctx.is_nonlocal("counter"));
        assert!(!ctx.is_local("counter"));
    }

    #[test]
    fn test_mutation_target_is_impure() {
        assert!(!PythonMutationTarget::Local.is_impure()); // Local mutation is PURE
        assert!(PythonMutationTarget::SelfAttribute.is_impure());
        assert!(PythonMutationTarget::Parameter.is_impure());
        assert!(PythonMutationTarget::Global.is_impure());
        assert!(PythonMutationTarget::Nonlocal.is_impure());
        assert!(PythonMutationTarget::Unknown.is_impure()); // Conservative
    }

    #[test]
    fn test_classify_mutation_target_self() {
        let ctx = ScopeContext::new(vec![]);

        // self.xxx patterns are always instance mutations
        assert_eq!(
            PythonPurityAnalyzer::classify_mutation_target("self.items", &ctx),
            PythonMutationTarget::SelfAttribute
        );
        assert_eq!(
            PythonPurityAnalyzer::classify_mutation_target("self.data.nested", &ctx),
            PythonMutationTarget::SelfAttribute
        );
    }

    #[test]
    fn test_classify_mutation_target_parameter() {
        let ctx = ScopeContext::new(vec!["items".to_string(), "data".to_string()]);

        assert_eq!(
            PythonPurityAnalyzer::classify_mutation_target("items", &ctx),
            PythonMutationTarget::Parameter
        );
        assert_eq!(
            PythonPurityAnalyzer::classify_mutation_target("data", &ctx),
            PythonMutationTarget::Parameter
        );
    }

    #[test]
    fn test_classify_mutation_target_local() {
        let mut ctx = ScopeContext::new(vec![]);
        ctx.add_local("result".to_string());
        ctx.add_local("temp".to_string());

        assert_eq!(
            PythonPurityAnalyzer::classify_mutation_target("result", &ctx),
            PythonMutationTarget::Local
        );
        assert_eq!(
            PythonPurityAnalyzer::classify_mutation_target("temp", &ctx),
            PythonMutationTarget::Local
        );
    }

    #[test]
    fn test_classify_mutation_target_global() {
        let mut ctx = ScopeContext::new(vec![]);
        ctx.add_global("cache".to_string());

        assert_eq!(
            PythonPurityAnalyzer::classify_mutation_target("cache", &ctx),
            PythonMutationTarget::Global
        );
    }

    #[test]
    fn test_build_scope_context() {
        let body = r#"
    global cache
    result = []
    temp: List[int] = []
    for item in items:
        result.append(item * 2)
    return result
"#;
        let ctx = PythonPurityAnalyzer::build_scope_context(body, vec!["items".to_string()]);

        // 'items' is a parameter
        assert!(ctx.is_parameter("items"));
        // 'result' and 'temp' are local variables
        assert!(ctx.is_local("result"));
        assert!(ctx.is_local("temp"));
        // 'cache' is declared global
        assert!(ctx.is_global("cache"));
    }

    #[test]
    fn test_detect_impure_mutation_on_param() {
        let ctx = ScopeContext::new(vec!["items".to_string()]);

        // items.append(x) - mutation of parameter - IMPURE
        let result = PythonPurityAnalyzer::detect_impure_mutation("items.append(x)", &ctx);
        assert!(result.is_some());
        let (target, method, target_type) = result.unwrap();
        assert_eq!(target, "items");
        assert_eq!(method, "append");
        assert_eq!(target_type, PythonMutationTarget::Parameter);
    }

    #[test]
    fn test_detect_impure_mutation_on_self() {
        let ctx = ScopeContext::new(vec![]);

        // self.items.append(x) - mutation of instance state - IMPURE
        let result = PythonPurityAnalyzer::detect_impure_mutation("self.items.append(x)", &ctx);
        assert!(result.is_some());
        let (target, method, target_type) = result.unwrap();
        assert_eq!(target, "self.items");
        assert_eq!(method, "append");
        assert_eq!(target_type, PythonMutationTarget::SelfAttribute);
    }

    #[test]
    fn test_detect_pure_mutation_on_local() {
        let mut ctx = ScopeContext::new(vec![]);
        ctx.add_local("result".to_string());

        // result.append(x) - mutation of local variable - PURE
        let result = PythonPurityAnalyzer::detect_impure_mutation("result.append(x)", &ctx);
        // Should return None because local mutation is NOT impure
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_mutation_target() {
        assert_eq!(
            PythonPurityAnalyzer::extract_mutation_target("result"),
            "result"
        );
        assert_eq!(
            PythonPurityAnalyzer::extract_mutation_target("self.items"),
            "self.items"
        );
        assert_eq!(
            PythonPurityAnalyzer::extract_mutation_target("  items  "),
            "items"
        );
    }

    #[test]
    fn test_extract_parameters() {
        // Simple function
        assert_eq!(
            PythonPurityAnalyzer::extract_parameters("def foo(a, b, c):"),
            vec!["a", "b", "c"]
        );

        // With type annotations
        assert_eq!(
            PythonPurityAnalyzer::extract_parameters(
                "def bar(items: List[int], value: int) -> int:"
            ),
            vec!["items", "value"]
        );

        // With default values
        assert_eq!(
            PythonPurityAnalyzer::extract_parameters("def baz(x, y=10, z=None):"),
            vec!["x", "y", "z"]
        );

        // With *args and **kwargs
        assert_eq!(
            PythonPurityAnalyzer::extract_parameters("def qux(*args, **kwargs):"),
            vec!["args", "kwargs"]
        );

        // Method with self
        assert_eq!(
            PythonPurityAnalyzer::extract_parameters("def method(self, value):"),
            vec!["self", "value"]
        );
    }

    #[test]
    fn test_mutation_methods_list() {
        // Verify key mutation methods are in the list
        assert!(PYTHON_MUTATION_METHODS.contains(&"append"));
        assert!(PYTHON_MUTATION_METHODS.contains(&"extend"));
        assert!(PYTHON_MUTATION_METHODS.contains(&"pop"));
        assert!(PYTHON_MUTATION_METHODS.contains(&"clear"));
        assert!(PYTHON_MUTATION_METHODS.contains(&"update"));
        assert!(PYTHON_MUTATION_METHODS.contains(&"add"));
    }

    #[test]
    fn test_analyze_function_body_with_params_pure() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        // Function with local mutation - should be PURE
        let body = r#"
    result = []
    for item in items:
        result.append(item * 2)
    return result
"#;
        let (is_pure, impure_calls, _) =
            analyzer.analyze_function_body_with_params(body, vec!["items".to_string()]);
        assert!(is_pure, "Local mutation should be pure");
        assert!(impure_calls.is_empty());
    }

    #[test]
    fn test_analyze_function_body_with_params_impure_param() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        // Function mutating parameter - should be IMPURE
        let body = r#"
    items.append(value)
    return items
"#;
        let (is_pure, impure_calls, _) = analyzer.analyze_function_body_with_params(
            body,
            vec!["items".to_string(), "value".to_string()],
        );
        assert!(!is_pure, "Parameter mutation should be impure");
        assert!(!impure_calls.is_empty());
        assert!(impure_calls[0].reason.contains("parameter"));
    }

    #[test]
    fn test_analyze_function_body_with_params_impure_self() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        // Method mutating self - should be IMPURE
        let body = r#"
    self.items.append(value)
"#;
        let (is_pure, impure_calls, _) = analyzer
            .analyze_function_body_with_params(body, vec!["self".to_string(), "value".to_string()]);
        assert!(!is_pure, "Self mutation should be impure");
        assert!(!impure_calls.is_empty());
        assert!(impure_calls[0].reason.contains("instance state"));
    }

    // =========================================================================
    // Integration tests for extract_function_with_params
    // =========================================================================

    #[tokio::test]
    async fn test_extract_function_with_params_simple() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_extract_params.py");

        // Create test file
        let content = r#"
def process(items: list, value: int) -> list:
    result = []
    for item in items:
        result.append(item * value)
    return result
"#;
        tokio::fs::write(&test_file, content).await.unwrap();

        // Extract function with params
        let (body, params) = analyzer
            .extract_function_with_params("process", &test_file)
            .await
            .unwrap();

        // Verify parameters were extracted
        assert_eq!(params, vec!["items", "value"]);

        // Verify body contains expected content
        assert!(body.contains("result = []"));
        assert!(body.contains("result.append"));

        // Cleanup
        let _ = tokio::fs::remove_file(&test_file).await;
    }

    #[tokio::test]
    async fn test_extract_function_with_params_defaults_and_types() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_extract_params2.py");

        // Create test file with complex signature
        let content = r#"
def complex_func(items: list, factor: int = 1, *args, **kwargs) -> dict:
    return {"items": items, "factor": factor}
"#;
        tokio::fs::write(&test_file, content).await.unwrap();

        // Extract function with params
        let (_, params) = analyzer
            .extract_function_with_params("complex_func", &test_file)
            .await
            .unwrap();

        // Verify all parameters were extracted (including *args, **kwargs without *)
        assert_eq!(params, vec!["items", "factor", "args", "kwargs"]);

        // Cleanup
        let _ = tokio::fs::remove_file(&test_file).await;
    }

    #[tokio::test]
    async fn test_extract_function_with_params_method() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_extract_params3.py");

        // Create test file with method
        let content = r#"
class MyClass:
    def add_item(self, item: str) -> None:
        self.items.append(item)
"#;
        tokio::fs::write(&test_file, content).await.unwrap();

        // Extract method with params
        let (body, params) = analyzer
            .extract_function_with_params("add_item", &test_file)
            .await
            .unwrap();

        // Verify self and item parameters
        assert_eq!(params, vec!["self", "item"]);

        // Verify body contains mutation
        assert!(body.contains("self.items.append"));

        // Cleanup
        let _ = tokio::fs::remove_file(&test_file).await;
    }

    /// E2E test: Full integration of parameter extraction with scope-aware analysis
    /// This test verifies the complete flow:
    /// 1. Extract function definition and body
    /// 2. Parse parameters from definition
    /// 3. Analyze body with scope awareness
    /// 4. Correctly identify parameter mutations as impure
    #[tokio::test]
    async fn test_e2e_param_mutation_detection() {
        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();

        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_e2e_param_mutation.py");

        // Create comprehensive test file
        let content = r#"
def mutates_param(items: list, value: int) -> None:
    """Impure: Mutates the 'items' parameter."""
    items.append(value)


def pure_local_mutation(count: int) -> list:
    """Pure: Only mutates local variable 'result'."""
    result = []
    result.append(count)
    result.append(count + 1)
    return result


def pure_read_only(items: list) -> int:
    """Pure: Only reads 'items', no mutation."""
    return len(items)


def impure_self_mutation(self, value: int) -> None:
    """Impure: Mutates instance state via self."""
    self.data.append(value)
"#;
        tokio::fs::write(&test_file, content).await.unwrap();

        // Test 1: mutates_param should be IMPURE
        {
            let (body, params) = analyzer
                .extract_function_with_params("mutates_param", &test_file)
                .await
                .unwrap();
            assert_eq!(params, vec!["items", "value"]);

            let (is_pure, impure_calls, _) =
                analyzer.analyze_function_body_with_params(&body, params);
            assert!(
                !is_pure,
                "mutates_param should be impure (mutates 'items' parameter)"
            );
            assert!(
                impure_calls
                    .iter()
                    .any(|c| c.function_name == "items.append"),
                "Should detect items.append as impure"
            );
        }

        // Test 2: pure_local_mutation should be PURE
        {
            let (body, params) = analyzer
                .extract_function_with_params("pure_local_mutation", &test_file)
                .await
                .unwrap();
            assert_eq!(params, vec!["count"]);

            let (is_pure, impure_calls, _) =
                analyzer.analyze_function_body_with_params(&body, params);
            assert!(
                is_pure,
                "pure_local_mutation should be pure (only local mutations). Impure calls: {:?}",
                impure_calls
            );
        }

        // Test 3: pure_read_only should be PURE
        {
            let (body, params) = analyzer
                .extract_function_with_params("pure_read_only", &test_file)
                .await
                .unwrap();
            assert_eq!(params, vec!["items"]);

            let (is_pure, _, _) = analyzer.analyze_function_body_with_params(&body, params);
            assert!(is_pure, "pure_read_only should be pure (read-only access)");
        }

        // Test 4: impure_self_mutation should be IMPURE
        {
            let (body, params) = analyzer
                .extract_function_with_params("impure_self_mutation", &test_file)
                .await
                .unwrap();
            assert_eq!(params, vec!["self", "value"]);

            let (is_pure, impure_calls, _) =
                analyzer.analyze_function_body_with_params(&body, params);
            assert!(
                !is_pure,
                "impure_self_mutation should be impure (mutates self)"
            );
            assert!(
                impure_calls
                    .iter()
                    .any(|c| c.reason.contains("instance state")),
                "Should detect self mutation as instance state modification"
            );
        }

        // Cleanup
        let _ = tokio::fs::remove_file(&test_file).await;
    }
}
