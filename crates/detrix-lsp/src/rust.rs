//! Rust-specific LSP purity analyzer
//!
//! Implements the PurityAnalyzer trait using rust-analyzer for call hierarchy analysis.
//!
//! Rust differs from Python/Go in several key ways:
//! - Static typing with ownership/borrowing system
//! - No runtime side effects through dynamic dispatch
//! - `unsafe` blocks explicitly mark dangerous code
//! - Method signatures (`&self`, `&mut self`) indicate mutability
//!
//! ## Scope-Aware Mutation Detection
//!
//! This module implements scope-aware mutation detection similar to Python/Go:
//! - Local variable mutations (owned values) are PURE
//! - `&mut self` method calls are IMPURE (modifies instance state)
//! - `&mut T` parameter mutations are IMPURE (affects caller's data)
//! - `static mut` access is IMPURE (global state)
//! - Unsafe mutations are IMPURE (memory safety concerns)
//!
//! Function classification constants are in `detrix_config`.

use crate::base::{BasePurityAnalyzer, BodyAnalysisResult, ExtractedFunction, LanguageAnalyzer};
use crate::cache::CacheRef;
use crate::common::ScopeTracker;
use crate::error::{Error, Result, ToLspResult};
use crate::mutation::{MutationTarget, RustMutationTarget};
use async_trait::async_trait;
use detrix_application::PurityAnalyzer;
use detrix_config::{
    LspConfig, RUST_ACCEPTABLE_IMPURE, RUST_IMPURE_FUNCTIONS, RUST_MUTATION_METHODS,
    RUST_PURE_FUNCTIONS,
};
use detrix_core::{ImpureCall, PurityAnalysis, PurityLevel, SourceLanguage};
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

// ============================================================================
// Static Regex Patterns (compiled once at runtime)
// ============================================================================

/// Pattern to match function calls
static RUST_CALL_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Pattern to match method calls: target.method(
static RUST_METHOD_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Pattern to match dereference mutations: *var = ... or *var op= ...
static RUST_DEREF_PATTERN: OnceLock<Regex> = OnceLock::new();

fn get_rust_call_pattern() -> &'static Regex {
    RUST_CALL_PATTERN.get_or_init(|| {
        Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*(?:::[a-zA-Z_][a-zA-Z0-9_]*)*)\s*[(<]")
            .expect("Static regex is valid")
    })
}

fn get_rust_method_pattern() -> &'static Regex {
    RUST_METHOD_PATTERN.get_or_init(|| {
        Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*\.\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\(")
            .expect("Static regex is valid")
    })
}

fn get_rust_deref_pattern() -> &'static Regex {
    RUST_DEREF_PATTERN.get_or_init(|| {
        Regex::new(r"\*([a-zA-Z_][a-zA-Z0-9_]*)\s*[\+\-\*/%&\|]?=").expect("Static regex is valid")
    })
}

/// Represents the scope context for scope-aware purity analysis in Rust
/// Tracks local variables, parameters, and self reference
///
/// Implements `ScopeTracker` for common interface while providing
/// Rust-specific methods for mutable parameters and static variable tracking.
#[derive(Debug, Default, Clone)]
pub struct RustScopeContext {
    /// Variables created locally in this function (let bindings)
    local_vars: HashSet<String>,
    /// Function parameters (mutations to &mut params affect caller)
    parameters: HashSet<String>,
    /// Mutable parameters specifically (&mut T)
    mutable_parameters: HashSet<String>,
    /// Whether this is a method with &mut self
    has_mut_self: bool,
    /// Static variables referenced
    static_vars: HashSet<String>,
}

impl ScopeTracker for RustScopeContext {
    fn with_parameters(parameters: Vec<String>) -> Self {
        Self {
            parameters: parameters.into_iter().collect(),
            ..Default::default()
        }
    }

    fn is_local(&self, name: &str) -> bool {
        self.local_vars.contains(name)
            && !self.parameters.contains(name)
            && !self.static_vars.contains(name)
    }

    fn is_parameter(&self, name: &str) -> bool {
        self.parameters.contains(name)
    }

    fn add_local(&mut self, name: String) {
        if !self.parameters.contains(&name) && !self.static_vars.contains(&name) {
            self.local_vars.insert(name);
        }
    }
}

impl RustScopeContext {
    /// Create a new scope context with given parameters
    pub fn new(parameters: Vec<String>) -> Self {
        <Self as ScopeTracker>::with_parameters(parameters)
    }

    /// Create scope context for a method with &mut self
    pub fn for_mut_method(parameters: Vec<String>) -> Self {
        Self {
            has_mut_self: true,
            parameters: parameters.into_iter().collect(),
            ..Default::default()
        }
    }

    /// Check if this is a &mut self method
    pub fn is_mut_self(&self) -> bool {
        self.has_mut_self
    }

    /// Check if a variable is a mutable parameter
    pub fn is_mut_parameter(&self, name: &str) -> bool {
        self.mutable_parameters.contains(name)
    }

    /// Check if a variable is a static variable
    pub fn is_static(&self, name: &str) -> bool {
        self.static_vars.contains(name)
    }

    /// Add a mutable parameter
    pub fn add_mut_parameter(&mut self, name: String) {
        self.parameters.insert(name.clone());
        self.mutable_parameters.insert(name);
    }

    /// Add a static variable reference
    pub fn add_static(&mut self, name: String) {
        self.local_vars.remove(&name);
        self.static_vars.insert(name);
    }
}

/// Classify a mutation target based on Rust scope context
pub fn classify_rust_mutation_target(name: &str, ctx: &RustScopeContext) -> RustMutationTarget {
    // Check self reference first
    if name == "self" && ctx.is_mut_self() {
        return RustMutationTarget::MutableSelf;
    }

    // Then check if it's a mutable parameter
    if ctx.is_mut_parameter(name) {
        return RustMutationTarget::MutableParameter;
    }

    // Then check if it's a static variable
    if ctx.is_static(name) {
        return RustMutationTarget::StaticVariable;
    }

    // Then check if it's a local variable
    if ctx.is_local(name) {
        return RustMutationTarget::Local;
    }

    // Unknown scope - conservative default
    RustMutationTarget::Unknown
}

/// Check if a method call indicates mutation (scope-aware)
pub fn is_rust_mutation_method(method: &str) -> bool {
    RUST_MUTATION_METHODS.contains(&method)
}

/// Rust purity analyzer using rust-analyzer
///
/// This analyzer uses the BasePurityAnalyzer for common LSP operations
/// and provides Rust-specific function classification.
pub struct RustPurityAnalyzer {
    /// Base analyzer with common LSP operations
    base: BasePurityAnalyzer,
}

impl RustPurityAnalyzer {
    /// Create a new Rust purity analyzer without cache
    pub fn new(config: &LspConfig) -> Result<Self> {
        // Create config with Rust-specific defaults if command not set to rust-analyzer
        let rust_config = if config.command.contains("rust-analyzer") {
            config.clone()
        } else {
            LspConfig {
                command: "rust-analyzer".to_string(),
                args: vec![],
                ..config.clone()
            }
        };

        Ok(Self {
            base: BasePurityAnalyzer::new(rust_config),
        })
    }

    /// Create a new Rust purity analyzer with cache
    pub fn with_cache(config: &LspConfig, cache: CacheRef) -> Result<Self> {
        // Create config with Rust-specific defaults if command not set to rust-analyzer
        let rust_config = if config.command.contains("rust-analyzer") {
            config.clone()
        } else {
            LspConfig {
                command: "rust-analyzer".to_string(),
                args: vec![],
                ..config.clone()
            }
        };

        Ok(Self {
            base: BasePurityAnalyzer::with_cache(rust_config, cache),
        })
    }

    /// Set the cache after creation
    pub fn set_cache(&mut self, cache: CacheRef) {
        self.base.set_cache(cache);
    }

    /// Check if a function name is in the pure list
    ///
    /// Uses proper :: boundary matching to avoid false positives
    /// (e.g., "execute_command" should NOT match "and")
    pub fn is_pure_function(name: &str) -> bool {
        // Helper function to check for proper :: boundary match
        fn matches_with_boundary(name: &str, pattern: &str) -> bool {
            if name == pattern {
                return true;
            }
            if name.ends_with(pattern) {
                let prefix_len = name.len() - pattern.len();
                if prefix_len == 0 {
                    return true;
                }
                // Check that the character before the suffix is '::'
                // or it's a method call (preceded by '.')
                let prefix = &name[..prefix_len];
                prefix.ends_with("::") || prefix.ends_with('.')
            } else {
                false
            }
        }

        RUST_PURE_FUNCTIONS.contains(&name)
            || RUST_PURE_FUNCTIONS
                .iter()
                .any(|p| matches_with_boundary(name, p))
            || RUST_ACCEPTABLE_IMPURE.contains(&name)
            || RUST_ACCEPTABLE_IMPURE
                .iter()
                .any(|p| matches_with_boundary(name, p))
    }

    /// Check if a function name is in the impure list
    ///
    /// Handles both full paths (std::fs::File::open) and short forms (File::open)
    /// by checking for proper :: boundary matching.
    pub fn is_impure_function(name: &str) -> bool {
        RUST_IMPURE_FUNCTIONS.contains(&name)
            || RUST_IMPURE_FUNCTIONS.iter().any(|p| {
                // Check if name is a proper suffix with :: boundary
                // e.g., "File::open" matches "std::fs::File::open"
                // but "to_string" should NOT match "read_to_string"
                if p.ends_with(name) {
                    let prefix_len = p.len() - name.len();
                    if prefix_len == 0 {
                        return true; // Exact match
                    }
                    // Check that the character before the suffix is '::'
                    p[..prefix_len].ends_with("::")
                } else {
                    false
                }
            })
    }

    /// Classify a function call
    fn classify_function(name: &str) -> PurityLevel {
        if Self::is_impure_function(name) {
            PurityLevel::Impure
        } else if Self::is_pure_function(name) {
            PurityLevel::Pure
        } else {
            // Unknown function - could be user-defined
            PurityLevel::Unknown
        }
    }

    /// Analyze a mutation for purity using scope context
    pub fn analyze_mutation(
        target_name: &str,
        scope: &RustScopeContext,
    ) -> (bool, Option<&'static str>) {
        let target = classify_rust_mutation_target(target_name, scope);
        (target.is_impure(), target.impurity_reason())
    }

    /// Check if a method call is a mutation operation
    pub fn is_mutation_method(method: &str) -> bool {
        is_rust_mutation_method(method)
    }

    /// Set the workspace root
    pub async fn set_workspace_root(&self, root: std::path::PathBuf) {
        self.base.set_workspace_root(root).await;
    }

    // =========================================================================
    // Body Analysis Fallback Methods
    // =========================================================================

    /// Extract function body with parameters for scope-aware analysis
    ///
    /// Returns (body, parameters, is_mut_self) tuple where:
    /// - body: The function body as a string
    /// - parameters: Extracted parameter names from the function signature
    /// - is_mut_self: Whether this is a method with `&mut self`
    async fn extract_function_with_params(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> Result<(String, Vec<String>, Vec<String>, bool)> {
        use crate::common::extract_body_with_braces;

        let content = tokio::fs::read_to_string(file_path)
            .await
            .lsp_file_not_found(file_path)?;

        let lines: Vec<&str> = content.lines().collect();

        // Patterns for Rust function definitions
        let patterns = [
            format!("fn {}(", function_name),
            format!("fn {}<", function_name), // Generic function
            format!("pub fn {}(", function_name),
            format!("pub fn {}<", function_name),
            format!("pub(crate) fn {}(", function_name),
            format!("async fn {}(", function_name),
            format!("pub async fn {}(", function_name),
        ];

        // Find the function definition line
        let mut func_start = None;
        let mut def_line = String::new();
        let mut in_signature = false;

        for (line_num, line) in lines.iter().enumerate() {
            for pattern in &patterns {
                if line.contains(pattern) {
                    func_start = Some(line_num);
                    def_line = (*line).to_string();
                    in_signature = true;
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

        // Handle multi-line signatures - find the opening brace
        let mut signature_end = start;
        if in_signature {
            for (i, line) in lines.iter().enumerate().skip(start) {
                if line.contains('{') {
                    signature_end = i;
                    break;
                }
                // Build complete signature for parameter extraction
                if i > start {
                    def_line.push_str(line);
                }
            }
        }

        // Extract parameters and check for &mut self
        let (parameters, mut_parameters, is_mut_self) = Self::extract_parameters_rust(&def_line);

        // Use shared brace-based body extraction
        let body = extract_body_with_braces(&lines, start, signature_end);

        Ok((body, parameters, mut_parameters, is_mut_self))
    }

    /// Extract parameters from a Rust function signature
    /// Returns (parameters, mutable_parameters, is_mut_self)
    fn extract_parameters_rust(def_line: &str) -> (Vec<String>, Vec<String>, bool) {
        let mut params = Vec::new();
        let mut mut_params = Vec::new();
        let mut is_mut_self = false;

        // Helper to check if a parameter has &mut T type
        fn is_mut_ref_param(param: &str) -> bool {
            // Check if the type (after :) starts with &mut
            if let Some(colon_pos) = param.find(':') {
                let type_part = param[colon_pos + 1..].trim();
                type_part.starts_with("&mut ")
            } else {
                false
            }
        }

        // Find content between first ( and matching )
        if let Some(start) = def_line.find('(') {
            // Find matching closing paren, handling nested parens
            let mut depth = 0;
            let mut end = start;
            for (i, ch) in def_line[start..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = start + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }

            let param_str = &def_line[start + 1..end];

            // Split by comma (handling generics)
            let mut current = String::new();
            let mut angle_depth = 0;
            let mut paren_depth = 0;

            for ch in param_str.chars() {
                match ch {
                    '<' => {
                        angle_depth += 1;
                        current.push(ch);
                    }
                    '>' => {
                        angle_depth -= 1;
                        current.push(ch);
                    }
                    '(' => {
                        paren_depth += 1;
                        current.push(ch);
                    }
                    ')' => {
                        paren_depth -= 1;
                        current.push(ch);
                    }
                    ',' if angle_depth == 0 && paren_depth == 0 => {
                        let param = current.trim().to_string();
                        if !param.is_empty() {
                            if let Some(extracted) = Self::extract_param_name(&param) {
                                if extracted == "self" {
                                    // Check if it's &mut self
                                    if param.contains("&mut self") {
                                        is_mut_self = true;
                                    }
                                } else {
                                    params.push(extracted.clone());
                                    // Check if it's a &mut T parameter
                                    if is_mut_ref_param(&param) {
                                        mut_params.push(extracted);
                                    }
                                }
                            }
                        }
                        current.clear();
                    }
                    _ => current.push(ch),
                }
            }

            // Handle last parameter
            let param = current.trim().to_string();
            if !param.is_empty() {
                if let Some(extracted) = Self::extract_param_name(&param) {
                    if extracted == "self" {
                        if param.contains("&mut self") {
                            is_mut_self = true;
                        }
                    } else {
                        params.push(extracted.clone());
                        // Check if it's a &mut T parameter
                        if is_mut_ref_param(&param) {
                            mut_params.push(extracted);
                        }
                    }
                }
            }
        }

        (params, mut_params, is_mut_self)
    }

    /// Extract parameter name from a Rust parameter string
    /// Handles patterns like: `name: Type`, `mut name: Type`, `&mut name: Type`
    fn extract_param_name(param: &str) -> Option<String> {
        let param = param.trim();

        // Handle self variants
        if param == "self" || param == "&self" || param == "&mut self" || param == "mut self" {
            return Some("self".to_string());
        }

        // Split on ':' to get name and type
        let name_part = param.split(':').next()?.trim();

        // Remove 'mut' prefix if present
        let name = name_part
            .trim_start_matches("mut ")
            .trim_start_matches("&mut ")
            .trim_start_matches('&')
            .trim();

        if name.is_empty() || name.starts_with('_') && name.len() == 1 {
            None
        } else {
            Some(name.to_string())
        }
    }

    /// Build scope context by analyzing the function body
    fn build_scope_context(
        body: &str,
        parameters: Vec<String>,
        mut_parameters: Vec<String>,
        is_mut_self: bool,
    ) -> RustScopeContext {
        let mut ctx = if is_mut_self {
            RustScopeContext::for_mut_method(parameters)
        } else {
            RustScopeContext::new(parameters)
        };

        // Register mutable parameters (e.g., counter: &mut i32)
        for param in mut_parameters {
            ctx.add_mut_parameter(param);
        }

        for line in body.lines() {
            let trimmed = line.trim();

            // Skip comments
            if trimmed.starts_with("//") {
                continue;
            }

            // Track let bindings: let [mut] name [: Type] = ...
            if trimmed.starts_with("let ") {
                let rest = trimmed.strip_prefix("let ").unwrap_or(trimmed);
                let rest = rest.trim_start_matches("mut ");

                // Extract variable name (before : or =)
                let var_name = rest
                    .split(':')
                    .next()
                    .unwrap_or(rest)
                    .split('=')
                    .next()
                    .unwrap_or("")
                    .trim();

                if !var_name.is_empty() && !var_name.starts_with('_') {
                    ctx.add_local(var_name.to_string());
                }
            }

            // Track static mut references
            if trimmed.contains("static mut ") || trimmed.contains("STATIC_") {
                // Try to extract static variable name
                if let Some(idx) = trimmed.find("STATIC_") {
                    let rest = &trimmed[idx..];
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        ctx.add_static(name);
                    }
                }
            }
        }

        ctx
    }

    /// Analyze function body for impure calls with scope awareness
    ///
    /// Returns (is_pure, impure_calls, user_function_calls)
    /// This is the public static version used by tests and the LanguageAnalyzer trait impl.
    pub fn analyze_function_body_with_params_static(
        body: &str,
        parameters: Vec<String>,
        mut_parameters: Vec<String>,
        is_mut_self: bool,
    ) -> (bool, Vec<ImpureCall>, Vec<String>) {
        let ctx = Self::build_scope_context(body, parameters, mut_parameters, is_mut_self);
        let mut impure_calls = Vec::new();
        let mut user_function_calls = Vec::new();
        let mut is_pure = true;

        // Check for unsafe blocks
        if body.contains("unsafe {") || body.contains("unsafe{") {
            impure_calls.push(ImpureCall {
                function_name: "unsafe".to_string(),
                reason: "Contains unsafe block".to_string(),
                call_path: String::new(),
            });
            is_pure = false;
        }

        // Use static patterns for performance (compiled once)
        let call_pattern = get_rust_call_pattern();
        let method_pattern = get_rust_method_pattern();
        let deref_pattern = get_rust_deref_pattern();

        for line in body.lines() {
            let trimmed = line.trim();

            // Skip comments
            if trimmed.starts_with("//") {
                continue;
            }

            // Check for known impure function calls
            for cap in call_pattern.captures_iter(line) {
                if let Some(func_match) = cap.get(1) {
                    let func_name = func_match.as_str();

                    // Check against known impure functions
                    if Self::is_impure_function(func_name) {
                        impure_calls.push(ImpureCall {
                            function_name: func_name.to_string(),
                            reason: "Known impure function".to_string(),
                            call_path: String::new(),
                        });
                        is_pure = false;
                    } else if !Self::is_pure_function(func_name) {
                        // Unknown function - might be user-defined
                        // Check if it looks like a user function (lowercase, no ::)
                        if !func_name.contains("::")
                            && func_name
                                .chars()
                                .next()
                                .map(|c| c.is_lowercase())
                                .unwrap_or(false)
                        {
                            user_function_calls.push(func_name.to_string());
                        }
                    }
                }
            }

            // Check for method calls on non-local targets
            for cap in method_pattern.captures_iter(line) {
                if let (Some(target_match), Some(method_match)) = (cap.get(1), cap.get(2)) {
                    let target = target_match.as_str();
                    let method = method_match.as_str();

                    // Check if this is a mutation method
                    if is_rust_mutation_method(method) {
                        let mutation_target = classify_rust_mutation_target(target, &ctx);
                        if mutation_target.is_impure() {
                            if let Some(reason) = mutation_target.impurity_reason() {
                                impure_calls.push(ImpureCall {
                                    function_name: format!("{}.{}", target, method),
                                    reason: reason.to_string(),
                                    call_path: String::new(),
                                });
                                is_pure = false;
                            }
                        }
                    }
                }
            }

            // Check for dereference mutations: *var = ... or *var += ...
            // This catches mutations through &mut parameters like *counter += 1
            for cap in deref_pattern.captures_iter(line) {
                if let Some(var_match) = cap.get(1) {
                    let var_name = var_match.as_str();
                    let mutation_target = classify_rust_mutation_target(var_name, &ctx);
                    if mutation_target.is_impure() {
                        if let Some(reason) = mutation_target.impurity_reason() {
                            impure_calls.push(ImpureCall {
                                function_name: format!("*{}", var_name),
                                reason: reason.to_string(),
                                call_path: String::new(),
                            });
                            is_pure = false;
                        }
                    }
                }
            }
        }

        (is_pure, impure_calls, user_function_calls)
    }
}

#[async_trait]
impl PurityAnalyzer for RustPurityAnalyzer {
    /// Analyze a function for purity with timeout support
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

    fn is_available(&self) -> bool {
        // Check if rust-analyzer is installed
        std::process::Command::new(&self.base.config().command)
            .arg("--version")
            .output()
            .is_ok()
    }

    async fn ensure_ready(&self) -> detrix_core::Result<()> {
        self.base.ensure_started().await?;
        Ok(())
    }

    fn language(&self) -> SourceLanguage {
        SourceLanguage::Rust
    }
}

/// Implementation of LanguageAnalyzer trait for shared purity analysis
#[async_trait]
impl LanguageAnalyzer for RustPurityAnalyzer {
    fn language(&self) -> SourceLanguage {
        SourceLanguage::Rust
    }

    fn classify_function(&self, name: &str) -> PurityLevel {
        Self::classify_function(name)
    }

    async fn extract_function(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> Result<ExtractedFunction> {
        let (body, parameters, mut_parameters, is_mut_self) = self
            .extract_function_with_params(function_name, file_path)
            .await?;

        Ok(ExtractedFunction::new(body, parameters)
            .with_mutable_params(mut_parameters)
            .with_mut_self(is_mut_self))
    }

    fn analyze_body(&self, extracted: &ExtractedFunction) -> BodyAnalysisResult {
        let (is_pure, impure_calls, user_function_calls) =
            Self::analyze_function_body_with_params_static(
                &extracted.body,
                extracted.parameters.clone(),
                extracted.mutable_params.clone(),
                extracted.is_mut_self,
            );

        BodyAnalysisResult {
            is_pure,
            impure_calls,
            user_function_calls,
        }
    }
}

impl RustPurityAnalyzer {
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

    /// Validates that all static regex patterns compile correctly.
    /// This test forces lazy initialization of all patterns, ensuring they are valid.
    #[test]
    fn all_static_regex_patterns_compile() {
        // Force lazy initialization - will panic if any pattern is invalid
        let _ = get_rust_call_pattern();
        let _ = get_rust_method_pattern();
        let _ = get_rust_deref_pattern();
    }

    #[test]
    fn test_pure_functions_classification() {
        // Core type constructors
        assert!(RustPurityAnalyzer::is_pure_function("String::new"));
        assert!(RustPurityAnalyzer::is_pure_function("Vec::new"));
        assert!(RustPurityAnalyzer::is_pure_function("Box::new"));

        // Iterator methods
        assert!(RustPurityAnalyzer::is_pure_function("map"));
        assert!(RustPurityAnalyzer::is_pure_function("filter"));
        assert!(RustPurityAnalyzer::is_pure_function("collect"));

        // Option/Result methods
        assert!(RustPurityAnalyzer::is_pure_function("is_some"));
        assert!(RustPurityAnalyzer::is_pure_function("unwrap_or"));
    }

    #[test]
    fn test_impure_functions_classification() {
        // Process operations - full paths
        assert!(RustPurityAnalyzer::is_impure_function("std::process::exit"));
        assert!(RustPurityAnalyzer::is_impure_function(
            "std::process::Command::new"
        ));

        // File I/O - full paths
        assert!(RustPurityAnalyzer::is_impure_function("std::fs::write"));
        assert!(RustPurityAnalyzer::is_impure_function(
            "std::fs::remove_file"
        ));

        // Network
        assert!(RustPurityAnalyzer::is_impure_function(
            "std::net::TcpStream::connect"
        ));
    }

    #[test]
    fn test_impure_functions_short_forms() {
        // Short forms (as captured from imported types)
        assert!(
            RustPurityAnalyzer::is_impure_function("Command::new"),
            "Command::new should be detected as impure"
        );
        assert!(
            RustPurityAnalyzer::is_impure_function("Command::output"),
            "Command::output should be detected as impure"
        );
        assert!(
            RustPurityAnalyzer::is_impure_function("File::open"),
            "File::open should be detected as impure"
        );
        assert!(
            RustPurityAnalyzer::is_impure_function("File::create"),
            "File::create should be detected as impure"
        );
        assert!(
            RustPurityAnalyzer::is_impure_function("TcpStream::connect"),
            "TcpStream::connect should be detected as impure"
        );
    }

    #[test]
    fn test_body_analysis_detects_command_new() {
        let body = r#"
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map_err(|e| e.to_string())?;

    String::from_utf8(output.stdout).map_err(|e| e.to_string())
"#;
        let (is_pure, impure_calls, _user_calls) =
            RustPurityAnalyzer::analyze_function_body_with_params_static(
                body,
                vec![],
                vec![],
                false,
            );

        assert!(
            !is_pure,
            "Body with Command::new should be impure, got impure_calls: {:?}",
            impure_calls
        );
        assert!(
            impure_calls
                .iter()
                .any(|c| c.function_name.contains("Command::new")),
            "Should detect Command::new as impure, got: {:?}",
            impure_calls
        );
    }

    #[test]
    fn test_acceptable_impure_functions() {
        // Printing is acceptable for metrics
        assert!(RustPurityAnalyzer::is_pure_function("println"));
        assert!(RustPurityAnalyzer::is_pure_function("dbg"));
        assert!(RustPurityAnalyzer::is_pure_function("tracing::info"));
    }

    #[test]
    fn test_classify_function() {
        assert_eq!(
            RustPurityAnalyzer::classify_function("len"),
            PurityLevel::Pure
        );
        assert_eq!(
            RustPurityAnalyzer::classify_function("std::process::exit"),
            PurityLevel::Impure
        );
        assert_eq!(
            RustPurityAnalyzer::classify_function("my_custom_func"),
            PurityLevel::Unknown
        );
    }

    #[test]
    fn test_unknown_function() {
        assert_eq!(
            RustPurityAnalyzer::classify_function("mymodule::process_data"),
            PurityLevel::Unknown
        );
        assert_eq!(
            RustPurityAnalyzer::classify_function("CustomHandler"),
            PurityLevel::Unknown
        );
    }

    // Scope-aware mutation detection tests

    #[test]
    fn test_scope_context_new() {
        let ctx = RustScopeContext::new(vec!["param1".to_string(), "param2".to_string()]);

        assert!(ctx.is_parameter("param1"));
        assert!(ctx.is_parameter("param2"));
        assert!(!ctx.is_parameter("other"));
    }

    #[test]
    fn test_scope_context_for_mut_method() {
        let ctx = RustScopeContext::for_mut_method(vec!["arg1".to_string()]);

        assert!(ctx.is_mut_self());
        assert!(ctx.is_parameter("arg1"));
    }

    #[test]
    fn test_scope_context_local_vars() {
        let mut ctx = RustScopeContext::new(vec!["param".to_string()]);

        ctx.add_local("result".to_string());
        ctx.add_local("temp".to_string());

        assert!(ctx.is_local("result"));
        assert!(ctx.is_local("temp"));
        assert!(!ctx.is_local("param"));
    }

    #[test]
    fn test_scope_context_mut_parameters() {
        let mut ctx = RustScopeContext::new(vec![]);
        ctx.add_mut_parameter("items".to_string());

        assert!(ctx.is_mut_parameter("items"));
        assert!(ctx.is_parameter("items"));
    }

    #[test]
    fn test_scope_context_static_vars() {
        let mut ctx = RustScopeContext::new(vec![]);
        ctx.add_local("my_var".to_string());
        assert!(ctx.is_local("my_var"));

        ctx.add_static("my_var".to_string());
        assert!(ctx.is_static("my_var"));
        assert!(!ctx.is_local("my_var"));
    }

    #[test]
    fn test_classify_mutation_target_local() {
        let mut ctx = RustScopeContext::new(vec![]);
        ctx.add_local("result".to_string());

        let target = classify_rust_mutation_target("result", &ctx);
        assert_eq!(target, RustMutationTarget::Local);
        assert!(!target.is_impure());
    }

    #[test]
    fn test_classify_mutation_target_mut_self() {
        let ctx = RustScopeContext::for_mut_method(vec![]);

        let target = classify_rust_mutation_target("self", &ctx);
        assert_eq!(target, RustMutationTarget::MutableSelf);
        assert!(target.is_impure());
    }

    #[test]
    fn test_classify_mutation_target_mut_parameter() {
        let mut ctx = RustScopeContext::new(vec![]);
        ctx.add_mut_parameter("items".to_string());

        let target = classify_rust_mutation_target("items", &ctx);
        assert_eq!(target, RustMutationTarget::MutableParameter);
        assert!(target.is_impure());
    }

    #[test]
    fn test_classify_mutation_target_static() {
        let mut ctx = RustScopeContext::new(vec![]);
        ctx.add_static("GLOBAL_CONFIG".to_string());

        let target = classify_rust_mutation_target("GLOBAL_CONFIG", &ctx);
        assert_eq!(target, RustMutationTarget::StaticVariable);
        assert!(target.is_impure());
    }

    #[test]
    fn test_classify_mutation_target_unknown() {
        let ctx = RustScopeContext::new(vec![]);

        let target = classify_rust_mutation_target("unknown_var", &ctx);
        assert_eq!(target, RustMutationTarget::Unknown);
        assert!(target.is_impure());
    }

    #[test]
    fn test_is_rust_mutation_method() {
        // Vec mutations
        assert!(is_rust_mutation_method("push"));
        assert!(is_rust_mutation_method("pop"));
        assert!(is_rust_mutation_method("clear"));

        // Non-mutation methods
        assert!(!is_rust_mutation_method("len"));
        assert!(!is_rust_mutation_method("is_empty"));
        assert!(!is_rust_mutation_method("iter"));
    }

    #[test]
    fn test_mutation_target_impurity_reasons() {
        assert!(RustMutationTarget::Local.impurity_reason().is_none());
        assert!(RustMutationTarget::MutableSelf
            .impurity_reason()
            .unwrap()
            .contains("&mut self"));
        assert!(RustMutationTarget::MutableParameter
            .impurity_reason()
            .unwrap()
            .contains("&mut parameter"));
        assert!(RustMutationTarget::StaticVariable
            .impurity_reason()
            .unwrap()
            .contains("static"));
        assert!(RustMutationTarget::Unknown
            .impurity_reason()
            .unwrap()
            .contains("unknown"));
    }

    #[test]
    fn test_scope_context_complex_scenario() {
        // Simulate: fn process(&mut self, items: &mut Vec<i32>) -> i32 {
        //     let mut result = 0;      // local
        //     let temp = items[0];     // local
        //     self.count += 1;         // &mut self mutation - IMPURE
        //     items.push(temp);        // &mut param mutation - IMPURE
        //     result += temp;          // local mutation - PURE
        //     result
        // }
        let mut ctx = RustScopeContext::for_mut_method(vec![]);
        ctx.add_mut_parameter("items".to_string());
        ctx.add_local("result".to_string());
        ctx.add_local("temp".to_string());

        assert_eq!(
            classify_rust_mutation_target("self", &ctx),
            RustMutationTarget::MutableSelf
        );
        assert_eq!(
            classify_rust_mutation_target("items", &ctx),
            RustMutationTarget::MutableParameter
        );
        assert_eq!(
            classify_rust_mutation_target("result", &ctx),
            RustMutationTarget::Local
        );
        assert_eq!(
            classify_rust_mutation_target("temp", &ctx),
            RustMutationTarget::Local
        );

        // Verify purity implications
        assert!(classify_rust_mutation_target("self", &ctx).is_impure());
        assert!(classify_rust_mutation_target("items", &ctx).is_impure());
        assert!(!classify_rust_mutation_target("result", &ctx).is_impure());
        assert!(!classify_rust_mutation_target("temp", &ctx).is_impure());
    }

    #[test]
    fn test_classify_function_pure() {
        assert_eq!(
            RustPurityAnalyzer::classify_function("len"),
            PurityLevel::Pure
        );
        assert_eq!(
            RustPurityAnalyzer::classify_function("clone"),
            PurityLevel::Pure
        );
        assert_eq!(
            RustPurityAnalyzer::classify_function("to_string"),
            PurityLevel::Pure
        );
    }

    #[test]
    fn test_classify_function_impure() {
        assert_eq!(
            RustPurityAnalyzer::classify_function("std::process::exit"),
            PurityLevel::Impure
        );
        assert_eq!(
            RustPurityAnalyzer::classify_function("std::fs::write"),
            PurityLevel::Impure
        );
    }

    #[test]
    fn test_classify_function_unknown() {
        assert_eq!(
            RustPurityAnalyzer::classify_function("custom_func"),
            PurityLevel::Unknown
        );
        assert_eq!(
            RustPurityAnalyzer::classify_function("my_crate::process"),
            PurityLevel::Unknown
        );
    }

    #[test]
    fn test_classify_function_acceptable_impure() {
        // Acceptable impure functions (logging) are treated as pure
        assert_eq!(
            RustPurityAnalyzer::classify_function("println"),
            PurityLevel::Pure
        );
        assert_eq!(
            RustPurityAnalyzer::classify_function("dbg"),
            PurityLevel::Pure
        );
    }

    // =========================================================================
    // Body Analysis Tests
    // =========================================================================

    #[test]
    fn test_extract_parameters_rust_simple() {
        let (params, mut_params, is_mut_self) =
            RustPurityAnalyzer::extract_parameters_rust("fn process(x: i32, y: String) {");
        assert_eq!(params, vec!["x", "y"]);
        assert!(mut_params.is_empty());
        assert!(!is_mut_self);
    }

    #[test]
    fn test_extract_parameters_rust_self() {
        let (params, mut_params, is_mut_self) =
            RustPurityAnalyzer::extract_parameters_rust("fn process(&self, x: i32) {");
        assert_eq!(params, vec!["x"]);
        assert!(mut_params.is_empty());
        assert!(!is_mut_self);
    }

    #[test]
    fn test_extract_parameters_rust_mut_self() {
        let (params, mut_params, is_mut_self) =
            RustPurityAnalyzer::extract_parameters_rust("fn process(&mut self, x: i32) {");
        assert_eq!(params, vec!["x"]);
        assert!(mut_params.is_empty());
        assert!(is_mut_self);
    }

    #[test]
    fn test_extract_parameters_rust_generics() {
        let (params, mut_params, is_mut_self) = RustPurityAnalyzer::extract_parameters_rust(
            "fn process<T: Clone>(items: Vec<T>, count: usize) {",
        );
        assert_eq!(params, vec!["items", "count"]);
        assert!(mut_params.is_empty());
        assert!(!is_mut_self);
    }

    #[test]
    fn test_extract_parameters_rust_complex() {
        let (params, mut_params, is_mut_self) = RustPurityAnalyzer::extract_parameters_rust(
            "pub async fn handle(&mut self, req: Request<Body>, ctx: Context<State>) -> Result<Response> {",
        );
        assert_eq!(params, vec!["req", "ctx"]);
        assert!(mut_params.is_empty());
        assert!(is_mut_self);
    }

    #[test]
    fn test_extract_parameters_rust_mut_ref_param() {
        let (params, mut_params, is_mut_self) = RustPurityAnalyzer::extract_parameters_rust(
            "pub fn increment_counter(counter: &mut i32) {",
        );
        assert_eq!(params, vec!["counter"]);
        assert_eq!(mut_params, vec!["counter"]);
        assert!(!is_mut_self);
    }

    #[test]
    fn test_extract_param_name_simple() {
        assert_eq!(
            RustPurityAnalyzer::extract_param_name("x: i32"),
            Some("x".to_string())
        );
        assert_eq!(
            RustPurityAnalyzer::extract_param_name("name: String"),
            Some("name".to_string())
        );
    }

    #[test]
    fn test_extract_param_name_mut() {
        assert_eq!(
            RustPurityAnalyzer::extract_param_name("mut x: i32"),
            Some("x".to_string())
        );
    }

    #[test]
    fn test_extract_param_name_self_variants() {
        assert_eq!(
            RustPurityAnalyzer::extract_param_name("self"),
            Some("self".to_string())
        );
        assert_eq!(
            RustPurityAnalyzer::extract_param_name("&self"),
            Some("self".to_string())
        );
        assert_eq!(
            RustPurityAnalyzer::extract_param_name("&mut self"),
            Some("self".to_string())
        );
    }

    #[test]
    fn test_build_scope_context_basic() {
        let body = r#"
            let result = 0;
            let mut temp = vec![];
            temp.push(1);
            result + temp.len()
        "#;

        let ctx =
            RustPurityAnalyzer::build_scope_context(body, vec!["param".to_string()], vec![], false);
        assert!(ctx.is_local("result"));
        assert!(ctx.is_local("temp"));
        assert!(ctx.is_parameter("param"));
        assert!(!ctx.is_local("param"));
    }

    #[test]
    fn test_build_scope_context_mut_method() {
        let body = r#"
            let count = self.items.len();
            self.items.push(value);
        "#;

        let ctx =
            RustPurityAnalyzer::build_scope_context(body, vec!["value".to_string()], vec![], true);
        assert!(ctx.is_mut_self());
        assert!(ctx.is_local("count"));
        assert!(ctx.is_parameter("value"));
    }

    #[test]
    fn test_analyze_function_body_pure() {
        use crate::base::{ExtractedFunction, LanguageAnalyzer};
        use detrix_config::LspConfig;

        let config = LspConfig {
            command: "rust-analyzer".to_string(),
            ..Default::default()
        };
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();

        let body = r#"
            let result = items.iter().map(|x| x * 2).collect();
            result
        "#;

        let extracted = ExtractedFunction {
            body: body.to_string(),
            parameters: vec!["items".to_string()],
            mutable_params: vec![],
            is_mut_self: false,
            ..Default::default()
        };

        let result = analyzer.analyze_body(&extracted);

        assert!(result.is_pure);
        assert!(result.impure_calls.is_empty());
    }

    #[test]
    fn test_analyze_function_body_impure_known() {
        use crate::base::{ExtractedFunction, LanguageAnalyzer};
        use detrix_config::LspConfig;

        let config = LspConfig {
            command: "rust-analyzer".to_string(),
            ..Default::default()
        };
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();

        let body = r#"
            let contents = std::fs::read_to_string(path).unwrap();
            contents.len()
        "#;

        let extracted = ExtractedFunction {
            body: body.to_string(),
            parameters: vec!["path".to_string()],
            mutable_params: vec![],
            is_mut_self: false,
            ..Default::default()
        };

        let result = analyzer.analyze_body(&extracted);

        assert!(!result.is_pure);
        assert!(result
            .impure_calls
            .iter()
            .any(|c| c.function_name.contains("std::fs::read_to_string")));
    }

    #[test]
    fn test_analyze_function_body_unsafe() {
        use crate::base::{ExtractedFunction, LanguageAnalyzer};
        use detrix_config::LspConfig;

        let config = LspConfig {
            command: "rust-analyzer".to_string(),
            ..Default::default()
        };
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();

        let body = r#"
            let ptr = data.as_ptr();
            unsafe {
                *ptr.add(1)
            }
        "#;

        let extracted = ExtractedFunction {
            body: body.to_string(),
            parameters: vec!["data".to_string()],
            mutable_params: vec![],
            is_mut_self: false,
            ..Default::default()
        };

        let result = analyzer.analyze_body(&extracted);

        assert!(!result.is_pure);
        assert!(result
            .impure_calls
            .iter()
            .any(|c| c.function_name == "unsafe"));
    }

    #[test]
    fn test_analyze_function_body_mutation_impure() {
        use crate::base::{ExtractedFunction, LanguageAnalyzer};
        use detrix_config::LspConfig;

        let config = LspConfig {
            command: "rust-analyzer".to_string(),
            ..Default::default()
        };
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();

        // &mut self method that modifies self - use self.push() directly
        let body = r#"
            self.push(value);
        "#;

        let extracted = ExtractedFunction {
            body: body.to_string(),
            parameters: vec!["value".to_string()],
            mutable_params: vec![],
            is_mut_self: true,
            ..Default::default()
        };

        let result = analyzer.analyze_body(&extracted);

        // Should detect self.push as impure due to &mut self
        assert!(!result.is_pure);
        assert!(result
            .impure_calls
            .iter()
            .any(|c| c.function_name.contains("self")));
    }

    #[test]
    fn test_analyze_function_body_local_mutation_pure() {
        use crate::base::{ExtractedFunction, LanguageAnalyzer};
        use detrix_config::LspConfig;

        let config = LspConfig {
            command: "rust-analyzer".to_string(),
            ..Default::default()
        };
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();

        // Local variable mutations are pure
        let body = r#"
            let mut result = Vec::new();
            result.push(1);
            result.push(2);
            result
        "#;

        let extracted = ExtractedFunction {
            body: body.to_string(),
            parameters: vec![],
            mutable_params: vec![],
            is_mut_self: false,
            ..Default::default()
        };

        let result = analyzer.analyze_body(&extracted);

        // Local mutations should be pure
        assert!(
            result.is_pure,
            "Local mutations should be pure, got: {:?}",
            result.impure_calls
        );
    }
}
