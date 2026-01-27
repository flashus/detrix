//! Go-specific LSP purity analyzer
//!
//! Implements the PurityAnalyzer trait using gopls for call hierarchy analysis.
//!
//! Go differs from Python in several key ways:
//! - Static typing eliminates many runtime side effects
//! - No global state mutations through `global` keyword
//! - Package-level variables are the closest equivalent
//! - Methods have explicit receivers (no `self` magic)
//!
//! ## Scope-Aware Mutation Detection
//!
//! This module implements scope-aware mutation detection similar to Python:
//! - Local variable mutations (created in the function) are PURE
//! - Method receiver mutations are IMPURE (modifies struct state)
//! - Parameter mutations are IMPURE (affects caller's data)
//! - Package-level variable mutations are IMPURE (global state)
//!
//! Function classification constants are in `detrix_config`.

use crate::base::{BasePurityAnalyzer, BodyAnalysisResult, ExtractedFunction, LanguageAnalyzer};
use crate::cache::CacheRef;
use crate::common::ScopeTracker;
use crate::error::{Error, Result, ToLspResult};
use crate::mutation::{GoMutationTarget, MutationTarget};
use async_trait::async_trait;
use detrix_application::PurityAnalyzer;
use detrix_config::{
    LspConfig, GO_ACCEPTABLE_IMPURE, GO_IMPURE_FUNCTIONS, GO_MUTATION_OPERATIONS,
    GO_PURE_FUNCTIONS, GO_RECEIVER_MUTATION_METHODS,
};
use detrix_core::{ImpureCall, PurityAnalysis, PurityLevel, SourceLanguage};
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

// ============================================================================
// Static Regex Patterns (compiled once at runtime)
// ============================================================================

/// Pattern to match function calls: package.Function( or Function(
static CALL_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Pattern to match method calls: target.Method(
static METHOD_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Pattern to match field assignments: `target.field = value` or `target.field[index] = value`
static FIELD_ASSIGN_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Pattern to match direct index assignments: `target[index] = value`
static INDEX_ASSIGN_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Pattern to match field increment/decrement: target.field++ or target.field--
static FIELD_INCR_DECR_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Pattern to match direct variable increment/decrement: var++ or var--
static DIRECT_INCR_DECR_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Pattern to match direct variable assignment: var = value (not := which is local)
static DIRECT_ASSIGN_PATTERN: OnceLock<Regex> = OnceLock::new();

fn get_call_pattern() -> &'static Regex {
    CALL_PATTERN.get_or_init(|| {
        Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*(?:\.[a-zA-Z_][a-zA-Z0-9_]*)*)\s*\(")
            .expect("Static regex is valid")
    })
}

fn get_method_pattern() -> &'static Regex {
    METHOD_PATTERN.get_or_init(|| {
        Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*\.\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\(")
            .expect("Static regex is valid")
    })
}

fn get_field_assign_pattern() -> &'static Regex {
    FIELD_ASSIGN_PATTERN.get_or_init(|| {
        Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*\.\s*[a-zA-Z_][a-zA-Z0-9_]*(?:\[[^\]]*\])?\s*=")
            .expect("Static regex is valid")
    })
}

fn get_index_assign_pattern() -> &'static Regex {
    INDEX_ASSIGN_PATTERN.get_or_init(|| {
        Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*\[[^\]]*\]\s*=").expect("Static regex is valid")
    })
}

fn get_field_incr_decr_pattern() -> &'static Regex {
    FIELD_INCR_DECR_PATTERN.get_or_init(|| {
        Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*\.\s*[a-zA-Z_][a-zA-Z0-9_]*\s*(\+\+|--)")
            .expect("Static regex is valid")
    })
}

fn get_direct_incr_decr_pattern() -> &'static Regex {
    DIRECT_INCR_DECR_PATTERN.get_or_init(|| {
        Regex::new(r"^\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*(\+\+|--)").expect("Static regex is valid")
    })
}

fn get_direct_assign_pattern() -> &'static Regex {
    DIRECT_ASSIGN_PATTERN.get_or_init(|| {
        Regex::new(r"^\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*[+\-*/]?=\s*[^=]")
            .expect("Static regex is valid")
    })
}

/// Represents the scope context for scope-aware purity analysis in Go
/// Tracks local variables, parameters, and receiver for method context
///
/// Implements `ScopeTracker` for common interface while providing
/// Go-specific methods for receiver and package-level variable tracking.
///
/// Used by `classify_go_mutation_target` to determine if a mutation is pure or impure.
#[derive(Debug, Default, Clone)]
pub struct GoScopeContext {
    /// Variables created locally in this function (assignments, := declarations)
    local_vars: HashSet<String>,
    /// Function parameters (mutations to these affect caller)
    parameters: HashSet<String>,
    /// Method receiver name (e.g., "s" in "func (s *Server) Method()")
    /// Mutations to receiver are impure (modify struct state)
    receiver: Option<String>,
    /// Package-level variables referenced in this function
    /// Mutations to these are impure (global state)
    package_vars: HashSet<String>,
}

impl ScopeTracker for GoScopeContext {
    fn with_parameters(parameters: Vec<String>) -> Self {
        Self {
            parameters: parameters.into_iter().collect(),
            ..Default::default()
        }
    }

    fn is_local(&self, name: &str) -> bool {
        self.local_vars.contains(name)
            && !self.parameters.contains(name)
            && !self.is_receiver(name)
            && !self.package_vars.contains(name)
    }

    fn is_parameter(&self, name: &str) -> bool {
        self.parameters.contains(name)
    }

    fn add_local(&mut self, name: String) {
        // Don't add if it's a parameter, receiver, or package var
        if !self.parameters.contains(&name)
            && !self.is_receiver(&name)
            && !self.package_vars.contains(&name)
        {
            self.local_vars.insert(name);
        }
    }
}

impl GoScopeContext {
    /// Create a new scope context with given parameters
    pub fn new(parameters: Vec<String>) -> Self {
        <Self as ScopeTracker>::with_parameters(parameters)
    }

    /// Create scope context for a method with receiver
    pub fn for_method(receiver: String, parameters: Vec<String>) -> Self {
        Self {
            receiver: Some(receiver),
            parameters: parameters.into_iter().collect(),
            ..Default::default()
        }
    }

    /// Check if a variable is the method receiver
    pub fn is_receiver(&self, name: &str) -> bool {
        self.receiver.as_ref().is_some_and(|r| r == name)
    }

    /// Check if a variable is a package-level variable
    pub fn is_package_var(&self, name: &str) -> bool {
        self.package_vars.contains(name)
    }

    /// Add a package-level variable reference
    pub fn add_package_var(&mut self, name: String) {
        // Remove from local_vars if it was there
        self.local_vars.remove(&name);
        self.package_vars.insert(name);
    }

    /// Set the method receiver
    pub fn set_receiver(&mut self, name: String) {
        self.receiver = Some(name);
    }
}

// GoMutationTarget is imported from crate::mutation

/// Classify a mutation target based on Go scope context
pub fn classify_go_mutation_target(name: &str, ctx: &GoScopeContext) -> GoMutationTarget {
    // Check receiver first (highest priority in Go method context)
    if ctx.is_receiver(name) {
        return GoMutationTarget::Receiver;
    }

    // Then check if it's a parameter
    if ctx.is_parameter(name) {
        return GoMutationTarget::Parameter;
    }

    // Then check if it's a package-level variable
    if ctx.is_package_var(name) {
        return GoMutationTarget::PackageVar;
    }

    // Then check if it's a local variable
    if ctx.is_local(name) {
        return GoMutationTarget::Local;
    }

    // Unknown scope - conservative default
    GoMutationTarget::Unknown
}

/// Check if a method call indicates mutation (scope-aware)
///
/// Returns true if the method is a known mutation method.
pub fn is_go_mutation_method(method: &str) -> bool {
    GO_RECEIVER_MUTATION_METHODS.contains(&method) || GO_MUTATION_OPERATIONS.contains(&method)
}

/// Go purity analyzer using gopls
///
/// This analyzer uses the BasePurityAnalyzer for common LSP operations
/// and provides Go-specific function classification.
pub struct GoPurityAnalyzer {
    /// Base analyzer with common LSP operations
    base: BasePurityAnalyzer,
}

impl GoPurityAnalyzer {
    /// Create a new Go purity analyzer without cache
    pub fn new(config: &LspConfig) -> Result<Self> {
        // Create config with Go-specific defaults if command not set to gopls
        let go_config = if config.command.contains("gopls") {
            config.clone()
        } else {
            LspConfig {
                command: "gopls".to_string(),
                args: vec!["serve".to_string()],
                ..config.clone()
            }
        };

        Ok(Self {
            base: BasePurityAnalyzer::new(go_config),
        })
    }

    /// Create a new Go purity analyzer with cache
    pub fn with_cache(config: &LspConfig, cache: CacheRef) -> Result<Self> {
        // Create config with Go-specific defaults if command not set to gopls
        let go_config = if config.command.contains("gopls") {
            config.clone()
        } else {
            LspConfig {
                command: "gopls".to_string(),
                args: vec!["serve".to_string()],
                ..config.clone()
            }
        };

        Ok(Self {
            base: BasePurityAnalyzer::with_cache(go_config, cache),
        })
    }

    /// Set the cache after creation
    pub fn set_cache(&mut self, cache: CacheRef) {
        self.base.set_cache(cache);
    }

    /// Check if a function name is in the pure list
    pub fn is_pure_function(name: &str) -> bool {
        GO_PURE_FUNCTIONS.contains(&name) || GO_ACCEPTABLE_IMPURE.contains(&name)
    }

    /// Check if a function name is in the impure list
    pub fn is_impure_function(name: &str) -> bool {
        GO_IMPURE_FUNCTIONS.contains(&name)
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
    ///
    /// This is the main entry point for scope-aware mutation analysis.
    /// Given a variable name and the scope context, determines if mutating
    /// that variable would make the function impure.
    ///
    /// # Returns
    /// - `(is_impure, Option<reason>)` - Whether the mutation is impure and why
    pub fn analyze_mutation(
        target_name: &str,
        scope: &GoScopeContext,
    ) -> (bool, Option<&'static str>) {
        let target = classify_go_mutation_target(target_name, scope);
        (target.is_impure(), target.impurity_reason())
    }

    /// Check if a method call is a mutation operation
    ///
    /// Used to detect mutations through method calls like `Lock()`, `Write()`, etc.
    pub fn is_mutation_method(method: &str) -> bool {
        is_go_mutation_method(method)
    }

    /// Set the workspace root
    ///
    /// Takes ownership of the path to store it internally.
    /// Used for file:// URIs in LSP communication.
    pub async fn set_workspace_root(&self, root: std::path::PathBuf) {
        self.base.set_workspace_root(root).await;
    }

    // =========================================================================
    // Body Analysis Fallback Methods
    // =========================================================================

    /// Extract function body with parameters and optional receiver for scope-aware analysis
    ///
    /// Returns (body, parameters, receiver) tuple where:
    /// - body: The function body as a string
    /// - parameters: Extracted parameter names from the function signature
    /// - receiver: Optional receiver name for methods
    async fn extract_function_with_params(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> Result<(String, Vec<String>, Option<String>)> {
        use crate::common::extract_body_with_braces;

        let content = tokio::fs::read_to_string(file_path)
            .await
            .lsp_file_not_found(file_path)?;

        let lines: Vec<&str> = content.lines().collect();

        // Find the function definition line
        // Go function patterns: func name(, func (r *Type) name(
        let mut func_start = None;
        let mut def_line = String::new();

        for (line_num, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            // Check for method: func (receiver Type) name(
            if trimmed.starts_with("func ")
                && (trimmed.contains(&format!(" {}(", function_name))
                    || trimmed.contains(&format!(") {}(", function_name)))
            {
                func_start = Some(line_num);
                def_line = (*line).to_string();
                break;
            }
        }

        let start = func_start.ok_or_else(|| Error::FunctionNotFound {
            function: function_name.to_string(),
            file: file_path.display().to_string(),
        })?;

        // Handle multi-line signatures - find the opening brace
        let mut signature_end = start;
        for (i, line) in lines.iter().enumerate().skip(start) {
            if line.contains('{') {
                signature_end = i;
                break;
            }
            if i > start {
                def_line.push_str(line);
            }
        }

        // Extract receiver and parameters
        let (receiver, parameters) = Self::extract_go_signature(&def_line);

        // Use shared brace-based body extraction
        let body = extract_body_with_braces(&lines, start, signature_end);

        Ok((body, parameters, receiver))
    }

    /// Extract receiver and parameters from a Go function signature
    /// Returns (receiver, parameters)
    fn extract_go_signature(def_line: &str) -> (Option<String>, Vec<String>) {
        let mut receiver = None;
        let mut params = Vec::new();

        // Check for method receiver: func (r *Type) or func (r Type)
        if let Some(func_idx) = def_line.find("func ") {
            let after_func = &def_line[func_idx + 5..];

            if after_func.trim().starts_with('(') {
                // This is a method with receiver
                if let Some(close_paren) = after_func.find(')') {
                    let receiver_part = &after_func[1..close_paren];
                    // Extract receiver name (before the type)
                    let recv_name = receiver_part
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_start_matches('*');
                    if !recv_name.is_empty() {
                        receiver = Some(recv_name.to_string());
                    }
                }
            }
        }

        // Find the parameters (after function name and before return type/body)
        // Look for the parameter list between ( and )
        let func_name_start = if receiver.is_some() {
            // After the receiver, find the next (
            def_line.find(") ").map(|i| i + 2).unwrap_or(0)
        } else {
            def_line.find("func ").map(|i| i + 5).unwrap_or(0)
        };

        let rest = &def_line[func_name_start..];
        if let Some(paren_start) = rest.find('(') {
            // Find matching closing paren
            let after_paren = &rest[paren_start + 1..];
            let mut depth = 1;
            let mut end = after_paren.len();
            for (i, ch) in after_paren.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }

            let param_str = &after_paren[..end];
            // Parse Go parameter list: name Type, name Type, ...
            // Or: name, name2 Type (grouped parameters)
            for part in param_str.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }

                // Get the first word (parameter name)
                // Handle both "name Type" and "name, name2 Type"
                let tokens: Vec<&str> = part.split_whitespace().collect();
                if !tokens.is_empty() {
                    let name = tokens[0].trim_end_matches(',');
                    if !name.is_empty() && name != "..." {
                        params.push(name.to_string());
                    }
                }
            }
        }

        (receiver, params)
    }

    /// Build scope context by analyzing the function body
    fn build_scope_context(
        body: &str,
        parameters: Vec<String>,
        receiver: Option<String>,
    ) -> GoScopeContext {
        let mut ctx = if let Some(recv) = receiver {
            GoScopeContext::for_method(recv, parameters)
        } else {
            GoScopeContext::new(parameters)
        };

        for line in body.lines() {
            let trimmed = line.trim();

            // Skip comments
            if trimmed.starts_with("//") {
                continue;
            }

            // Track short variable declarations: name := value
            if let Some(idx) = trimmed.find(":=") {
                let lhs = &trimmed[..idx].trim();
                // Handle multiple assignments: a, b := ...
                for var in lhs.split(',') {
                    let var = var.trim();
                    if !var.is_empty() && !var.contains('.') {
                        ctx.add_local(var.to_string());
                    }
                }
            }

            // Track var declarations: var name Type = value
            if trimmed.starts_with("var ") {
                let rest = trimmed.strip_prefix("var ").unwrap_or(trimmed);
                let var_name = rest
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(',');
                if !var_name.is_empty() {
                    ctx.add_local(var_name.to_string());
                }
            }
        }

        ctx
    }

    /// Check if a direct variable mutation (var++ or var--) is impure
    fn check_direct_mutation(
        pattern: &regex::Regex,
        line: &str,
        ctx: &GoScopeContext,
        mutation_type: &str,
    ) -> Option<ImpureCall> {
        let cap = pattern.captures(line)?;
        let var_name = cap.get(1)?.as_str();
        let mutation_target = classify_go_mutation_target(var_name, ctx);

        if !mutation_target.is_impure() {
            return None;
        }

        let reason = mutation_target.impurity_reason()?;
        Some(ImpureCall {
            function_name: format!("{} ({})", var_name, mutation_type),
            reason: reason.to_string(),
            call_path: String::new(),
        })
    }

    /// Check if a direct variable assignment (var = value) is impure
    fn check_direct_assignment(
        pattern: &regex::Regex,
        line: &str,
        ctx: &GoScopeContext,
    ) -> Option<ImpureCall> {
        let cap = pattern.captures(line)?;
        let var_name = cap.get(1)?.as_str();

        // Skip common local variable patterns
        if var_name == "err" || var_name == "_" {
            return None;
        }

        let mutation_target = classify_go_mutation_target(var_name, ctx);
        if !mutation_target.is_impure() {
            return None;
        }

        let reason = mutation_target.impurity_reason()?;
        Some(ImpureCall {
            function_name: format!("{} (direct assignment)", var_name),
            reason: reason.to_string(),
            call_path: String::new(),
        })
    }

    /// Analyze function body for impure calls with scope awareness
    ///
    /// Returns (is_pure, impure_calls, user_function_calls)
    fn analyze_function_body_with_params(
        body: &str,
        parameters: Vec<String>,
        receiver: Option<String>,
    ) -> (bool, Vec<ImpureCall>, Vec<String>) {
        let ctx = Self::build_scope_context(body, parameters, receiver);
        let mut impure_calls = Vec::new();
        let mut user_function_calls = Vec::new();
        let mut is_pure = true;

        // Use static patterns for performance (compiled once)
        let call_pattern = get_call_pattern();
        let method_pattern = get_method_pattern();
        let field_assign_pattern = get_field_assign_pattern();
        let index_assign_pattern = get_index_assign_pattern();
        let field_incr_decr_pattern = get_field_incr_decr_pattern();
        let direct_incr_decr_pattern = get_direct_incr_decr_pattern();
        let direct_assign_pattern = get_direct_assign_pattern();

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
                        // Check if it looks like a user function
                        if !func_name.starts_with("fmt.")
                            && !func_name.starts_with("log.")
                            && !func_name.starts_with("math.")
                            && !func_name.starts_with("strings.")
                        {
                            // Looks like user-defined function
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
                    if is_go_mutation_method(method) {
                        let mutation_target = classify_go_mutation_target(target, &ctx);
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

            // Check for field assignments (target.field = value)
            for cap in field_assign_pattern.captures_iter(line) {
                if let Some(target_match) = cap.get(1) {
                    let target = target_match.as_str();
                    let mutation_target = classify_go_mutation_target(target, &ctx);
                    if mutation_target.is_impure() {
                        if let Some(reason) = mutation_target.impurity_reason() {
                            impure_calls.push(ImpureCall {
                                function_name: format!("{} (field assignment)", target),
                                reason: reason.to_string(),
                                call_path: String::new(),
                            });
                            is_pure = false;
                        }
                    }
                }
            }

            // Check for index assignments (target[index] = value)
            for cap in index_assign_pattern.captures_iter(line) {
                if let Some(target_match) = cap.get(1) {
                    let target = target_match.as_str();
                    let mutation_target = classify_go_mutation_target(target, &ctx);
                    if mutation_target.is_impure() {
                        if let Some(reason) = mutation_target.impurity_reason() {
                            impure_calls.push(ImpureCall {
                                function_name: format!("{} (index assignment)", target),
                                reason: reason.to_string(),
                                call_path: String::new(),
                            });
                            is_pure = false;
                        }
                    }
                }
            }

            // Check for field increment/decrement (target.field++ or target.field--)
            for cap in field_incr_decr_pattern.captures_iter(line) {
                if let Some(target_match) = cap.get(1) {
                    let target = target_match.as_str();
                    let mutation_target = classify_go_mutation_target(target, &ctx);
                    if mutation_target.is_impure() {
                        if let Some(reason) = mutation_target.impurity_reason() {
                            impure_calls.push(ImpureCall {
                                function_name: format!("{} (field increment/decrement)", target),
                                reason: reason.to_string(),
                                call_path: String::new(),
                            });
                            is_pure = false;
                        }
                    }
                }
            }

            // Check for direct variable increment/decrement (var++ or var--)
            // This catches package-level variable mutations like: orderCount++
            if let Some(call) = Self::check_direct_mutation(
                direct_incr_decr_pattern,
                trimmed,
                &ctx,
                "direct increment/decrement",
            ) {
                impure_calls.push(call);
                is_pure = false;
            }

            // Check for direct variable assignment (var = value)
            // Skip short declarations (:=) which create locals, and common patterns
            if !trimmed.contains(":=") {
                if let Some(call) =
                    Self::check_direct_assignment(direct_assign_pattern, trimmed, &ctx)
                {
                    impure_calls.push(call);
                    is_pure = false;
                }
            }
        }

        (is_pure, impure_calls, user_function_calls)
    }
}

#[async_trait]
impl PurityAnalyzer for GoPurityAnalyzer {
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
        // Check if gopls is installed
        std::process::Command::new(&self.base.config().command)
            .arg("version")
            .output()
            .is_ok()
    }

    async fn ensure_ready(&self) -> detrix_core::Result<()> {
        self.base.ensure_started().await?;
        Ok(())
    }

    fn language(&self) -> SourceLanguage {
        SourceLanguage::Go
    }
}

/// Implementation of LanguageAnalyzer trait for shared purity analysis
#[async_trait]
impl LanguageAnalyzer for GoPurityAnalyzer {
    fn language(&self) -> SourceLanguage {
        SourceLanguage::Go
    }

    fn classify_function(&self, name: &str) -> PurityLevel {
        Self::classify_function(name)
    }

    async fn extract_function(
        &self,
        function_name: &str,
        file_path: &Path,
    ) -> Result<ExtractedFunction> {
        let (body, parameters, receiver) = self
            .extract_function_with_params(function_name, file_path)
            .await?;

        Ok(ExtractedFunction::new(body, parameters).with_receiver(receiver))
    }

    fn analyze_body(&self, extracted: &ExtractedFunction) -> BodyAnalysisResult {
        let (is_pure, impure_calls, user_function_calls) = Self::analyze_function_body_with_params(
            &extracted.body,
            extracted.parameters.clone(),
            extracted.receiver.clone(),
        );

        BodyAnalysisResult {
            is_pure,
            impure_calls,
            user_function_calls,
        }
    }
}

impl GoPurityAnalyzer {
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
        let _ = get_call_pattern();
        let _ = get_method_pattern();
        let _ = get_field_assign_pattern();
        let _ = get_index_assign_pattern();
        let _ = get_field_incr_decr_pattern();
        let _ = get_direct_incr_decr_pattern();
        let _ = get_direct_assign_pattern();
    }

    #[test]
    fn test_pure_functions_classification() {
        // Builtin pure functions
        assert!(GoPurityAnalyzer::is_pure_function("len"));
        assert!(GoPurityAnalyzer::is_pure_function("cap"));
        assert!(GoPurityAnalyzer::is_pure_function("append"));

        // Math functions
        assert!(GoPurityAnalyzer::is_pure_function("math.Sqrt"));
        assert!(GoPurityAnalyzer::is_pure_function("math.Abs"));

        // String functions
        assert!(GoPurityAnalyzer::is_pure_function("strings.ToLower"));
        assert!(GoPurityAnalyzer::is_pure_function("strings.Contains"));

        // fmt.Sprint* are pure (no I/O)
        assert!(GoPurityAnalyzer::is_pure_function("fmt.Sprintf"));
        assert!(GoPurityAnalyzer::is_pure_function("fmt.Sprint"));
    }

    #[test]
    fn test_impure_functions_classification() {
        // OS operations
        assert!(GoPurityAnalyzer::is_impure_function("os.Exit"));
        assert!(GoPurityAnalyzer::is_impure_function("os.Open"));
        assert!(GoPurityAnalyzer::is_impure_function("os.WriteFile"));

        // Network operations
        assert!(GoPurityAnalyzer::is_impure_function("http.Get"));
        assert!(GoPurityAnalyzer::is_impure_function("net.Dial"));

        // Exec operations
        assert!(GoPurityAnalyzer::is_impure_function("exec.Command"));
    }

    #[test]
    fn test_acceptable_impure_functions() {
        // Logging/printing is acceptable for metrics
        assert!(GoPurityAnalyzer::is_pure_function("fmt.Println"));
        assert!(GoPurityAnalyzer::is_pure_function("log.Print"));
    }

    #[test]
    fn test_classify_function() {
        assert_eq!(
            GoPurityAnalyzer::classify_function("len"),
            PurityLevel::Pure
        );
        assert_eq!(
            GoPurityAnalyzer::classify_function("os.Exit"),
            PurityLevel::Impure
        );
        assert_eq!(
            GoPurityAnalyzer::classify_function("myCustomFunc"),
            PurityLevel::Unknown
        );
    }

    #[test]
    fn test_unknown_function() {
        // User-defined functions should be Unknown
        assert_eq!(
            GoPurityAnalyzer::classify_function("mypackage.ProcessData"),
            PurityLevel::Unknown
        );
        assert_eq!(
            GoPurityAnalyzer::classify_function("CustomHandler"),
            PurityLevel::Unknown
        );
    }

    // =========================================================================
    // Scope-aware mutation detection tests
    // =========================================================================

    #[test]
    fn test_scope_context_new() {
        let ctx = GoScopeContext::new(vec!["param1".to_string(), "param2".to_string()]);

        assert!(ctx.is_parameter("param1"));
        assert!(ctx.is_parameter("param2"));
        assert!(!ctx.is_parameter("other"));
        assert!(!ctx.is_local("param1")); // Parameters are not locals
    }

    #[test]
    fn test_scope_context_for_method() {
        let ctx = GoScopeContext::for_method(
            "s".to_string(),
            vec!["arg1".to_string(), "arg2".to_string()],
        );

        assert!(ctx.is_receiver("s"));
        assert!(!ctx.is_receiver("other"));
        assert!(ctx.is_parameter("arg1"));
        assert!(ctx.is_parameter("arg2"));
    }

    #[test]
    fn test_scope_context_local_vars() {
        let mut ctx = GoScopeContext::new(vec!["param".to_string()]);

        // Add local variables
        ctx.add_local("result".to_string());
        ctx.add_local("temp".to_string());

        assert!(ctx.is_local("result"));
        assert!(ctx.is_local("temp"));
        assert!(!ctx.is_local("param")); // Parameter, not local
        assert!(!ctx.is_local("unknown"));
    }

    #[test]
    fn test_scope_context_parameter_not_local() {
        let mut ctx = GoScopeContext::new(vec!["items".to_string()]);

        // Try to add parameter as local (should be ignored)
        ctx.add_local("items".to_string());

        // Should still be parameter, not local
        assert!(ctx.is_parameter("items"));
        assert!(!ctx.is_local("items"));
    }

    #[test]
    fn test_scope_context_package_vars() {
        let mut ctx = GoScopeContext::new(vec![]);

        ctx.add_local("myVar".to_string());
        assert!(ctx.is_local("myVar"));

        // Mark as package var (overrides local)
        ctx.add_package_var("myVar".to_string());
        assert!(ctx.is_package_var("myVar"));
        assert!(!ctx.is_local("myVar")); // No longer local
    }

    #[test]
    fn test_classify_mutation_target_local() {
        let mut ctx = GoScopeContext::new(vec![]);
        ctx.add_local("result".to_string());

        let target = classify_go_mutation_target("result", &ctx);
        assert_eq!(target, GoMutationTarget::Local);
        assert!(!target.is_impure());
        assert!(target.impurity_reason().is_none());
    }

    #[test]
    fn test_classify_mutation_target_receiver() {
        let ctx = GoScopeContext::for_method("s".to_string(), vec![]);

        let target = classify_go_mutation_target("s", &ctx);
        assert_eq!(target, GoMutationTarget::Receiver);
        assert!(target.is_impure());
        assert!(target.impurity_reason().unwrap().contains("receiver"));
    }

    #[test]
    fn test_classify_mutation_target_parameter() {
        let ctx = GoScopeContext::new(vec!["items".to_string()]);

        let target = classify_go_mutation_target("items", &ctx);
        assert_eq!(target, GoMutationTarget::Parameter);
        assert!(target.is_impure());
        assert!(target.impurity_reason().unwrap().contains("parameter"));
    }

    #[test]
    fn test_classify_mutation_target_package_var() {
        let mut ctx = GoScopeContext::new(vec![]);
        ctx.add_package_var("globalConfig".to_string());

        let target = classify_go_mutation_target("globalConfig", &ctx);
        assert_eq!(target, GoMutationTarget::PackageVar);
        assert!(target.is_impure());
        assert!(target.impurity_reason().unwrap().contains("package-level"));
    }

    #[test]
    fn test_classify_mutation_target_unknown() {
        let ctx = GoScopeContext::new(vec![]);

        let target = classify_go_mutation_target("unknownVar", &ctx);
        assert_eq!(target, GoMutationTarget::Unknown);
        assert!(target.is_impure()); // Conservative
    }

    #[test]
    fn test_classify_mutation_priority() {
        // Test priority: receiver > parameter > package_var > local
        let mut ctx = GoScopeContext::for_method("s".to_string(), vec!["s".to_string()]);
        ctx.add_local("s".to_string());
        ctx.add_package_var("s".to_string());

        // Receiver takes highest priority
        let target = classify_go_mutation_target("s", &ctx);
        assert_eq!(target, GoMutationTarget::Receiver);
    }

    #[test]
    fn test_is_go_mutation_method() {
        // Sync package methods
        assert!(is_go_mutation_method("Lock"));
        assert!(is_go_mutation_method("Unlock"));
        assert!(is_go_mutation_method("Add"));
        assert!(is_go_mutation_method("Done"));

        // Buffer methods
        assert!(is_go_mutation_method("Write"));
        assert!(is_go_mutation_method("Reset"));

        // Non-mutation methods
        assert!(!is_go_mutation_method("String"));
        assert!(!is_go_mutation_method("Len"));
        assert!(!is_go_mutation_method("Read")); // Read typically doesn't mutate
    }

    #[test]
    fn test_mutation_target_impurity_reasons() {
        assert!(GoMutationTarget::Local.impurity_reason().is_none());
        assert!(GoMutationTarget::Receiver
            .impurity_reason()
            .unwrap()
            .contains("receiver"));
        assert!(GoMutationTarget::Parameter
            .impurity_reason()
            .unwrap()
            .contains("parameter"));
        assert!(GoMutationTarget::PackageVar
            .impurity_reason()
            .unwrap()
            .contains("package-level"));
        assert!(GoMutationTarget::Unknown
            .impurity_reason()
            .unwrap()
            .contains("unknown"));
    }

    #[test]
    fn test_scope_context_complex_scenario() {
        // Simulate: func (s *Server) Process(items []Item) error {
        //     result := []Item{}    // local
        //     temp := items[0]      // local
        //     s.count++             // receiver mutation - IMPURE
        //     items[0] = temp       // parameter mutation - IMPURE
        //     result = append(result, temp)  // local mutation - PURE
        //     return nil
        // }
        let mut ctx = GoScopeContext::for_method("s".to_string(), vec!["items".to_string()]);
        ctx.add_local("result".to_string());
        ctx.add_local("temp".to_string());

        // Check each mutation target
        assert_eq!(
            classify_go_mutation_target("s", &ctx),
            GoMutationTarget::Receiver
        );
        assert_eq!(
            classify_go_mutation_target("items", &ctx),
            GoMutationTarget::Parameter
        );
        assert_eq!(
            classify_go_mutation_target("result", &ctx),
            GoMutationTarget::Local
        );
        assert_eq!(
            classify_go_mutation_target("temp", &ctx),
            GoMutationTarget::Local
        );

        // Verify purity implications
        assert!(classify_go_mutation_target("s", &ctx).is_impure());
        assert!(classify_go_mutation_target("items", &ctx).is_impure());
        assert!(!classify_go_mutation_target("result", &ctx).is_impure()); // Local - PURE
        assert!(!classify_go_mutation_target("temp", &ctx).is_impure()); // Local - PURE
    }

    #[test]
    fn test_scope_context_function_vs_method() {
        // Function (no receiver)
        let func_ctx = GoScopeContext::new(vec!["a".to_string(), "b".to_string()]);
        assert!(!func_ctx.is_receiver("a"));
        assert!(func_ctx.is_parameter("a"));

        // Method (with receiver)
        let method_ctx =
            GoScopeContext::for_method("self".to_string(), vec!["a".to_string(), "b".to_string()]);
        assert!(method_ctx.is_receiver("self"));
        assert!(method_ctx.is_parameter("a"));
        assert!(!method_ctx.is_receiver("a"));
    }

    #[test]
    fn test_receiver_mutation_methods_list() {
        // Verify all sync methods are covered
        let sync_methods = vec!["Lock", "Unlock", "RLock", "RUnlock", "Add", "Done", "Wait"];
        for method in sync_methods {
            assert!(
                is_go_mutation_method(method),
                "Expected {} to be a mutation method",
                method
            );
        }

        // Verify buffer methods
        let buffer_methods = vec!["Write", "WriteByte", "WriteString", "Reset", "Truncate"];
        for method in buffer_methods {
            assert!(
                is_go_mutation_method(method),
                "Expected {} to be a mutation method",
                method
            );
        }
    }

    // =========================================================================
    // Additional tests for parity with Python purity analyzer
    // =========================================================================

    #[test]
    fn test_truly_impure_functions_list() {
        // os.Exit, exec.Command should be truly impure
        assert!(GoPurityAnalyzer::is_impure_function("os.Exit"));
        assert!(GoPurityAnalyzer::is_impure_function("exec.Command"));
    }

    #[test]
    fn test_acceptable_impure_functions_list() {
        // fmt.Println, log.Print should be acceptable impure (logging)
        // These are marked as "pure" by is_pure_function because they're in ACCEPTABLE_IMPURE
        assert!(GoPurityAnalyzer::is_pure_function("fmt.Println"));
        assert!(GoPurityAnalyzer::is_pure_function("log.Print"));
    }

    #[test]
    fn test_pure_functions_list() {
        // len, append, math.Sqrt should be pure
        assert!(GoPurityAnalyzer::is_pure_function("len"));
        assert!(GoPurityAnalyzer::is_pure_function("append"));
        assert!(GoPurityAnalyzer::is_pure_function("math.Sqrt"));
    }

    #[test]
    fn test_scope_context_with_receiver_and_params() {
        let ctx = GoScopeContext::for_method(
            "s".to_string(),
            vec!["name".to_string(), "value".to_string()],
        );

        // Receiver should be identified
        assert!(ctx.is_receiver("s"));
        // Parameters should be identified
        assert!(ctx.is_parameter("name"));
        assert!(ctx.is_parameter("value"));
        // But not confused with each other
        assert!(!ctx.is_receiver("name"));
        assert!(!ctx.is_parameter("s"));
    }

    #[test]
    fn test_mutation_target_is_impure() {
        // Local mutations are pure
        assert!(!GoMutationTarget::Local.is_impure());
        // Everything else is impure
        assert!(GoMutationTarget::Receiver.is_impure());
        assert!(GoMutationTarget::Parameter.is_impure());
        assert!(GoMutationTarget::PackageVar.is_impure());
        assert!(GoMutationTarget::Unknown.is_impure());
    }

    #[test]
    fn test_mutation_target_impurity_reason() {
        // Local has no impurity reason (it's pure)
        assert!(GoMutationTarget::Local.impurity_reason().is_none());
        // Others have reasons
        assert!(GoMutationTarget::Receiver.impurity_reason().is_some());
        assert!(GoMutationTarget::Parameter.impurity_reason().is_some());
        assert!(GoMutationTarget::PackageVar.impurity_reason().is_some());
        assert!(GoMutationTarget::Unknown.impurity_reason().is_some());
    }

    #[test]
    fn test_classify_mutation_receiver_direct() {
        // Direct receiver access like s = newValue
        let ctx = GoScopeContext::for_method("s".to_string(), vec![]);
        let target = classify_go_mutation_target("s", &ctx);
        // Direct receiver mutation
        assert_eq!(target, GoMutationTarget::Receiver);
    }

    #[test]
    fn test_classify_mutation_local_direct() {
        // Direct local variable access
        let mut ctx = GoScopeContext::new(vec![]);
        ctx.add_local("items".to_string());
        let target = classify_go_mutation_target("items", &ctx);
        // Direct local mutation
        assert_eq!(target, GoMutationTarget::Local);
    }

    #[test]
    fn test_non_mutation_methods() {
        // Read-only methods should not be mutation methods
        assert!(!is_go_mutation_method("Read"));
        // Note: "Get" is in RECEIVER_MUTATION_METHODS (sync.Pool.Get)
        assert!(!is_go_mutation_method("Len"));
        assert!(!is_go_mutation_method("String"));
        assert!(!is_go_mutation_method("Error"));
        assert!(!is_go_mutation_method("Value")); // Not a mutation method
    }

    #[test]
    fn test_scope_context_empty_params() {
        let ctx = GoScopeContext::new(vec![]);
        // No parameters
        assert!(!ctx.is_parameter("x"));
        assert!(!ctx.is_parameter("y"));
    }

    #[test]
    fn test_scope_context_no_receiver() {
        let ctx = GoScopeContext::new(vec![]);
        // No receiver
        assert!(!ctx.is_receiver("s"));
        assert!(!ctx.is_receiver("self"));
    }

    #[test]
    fn test_classify_unknown_from_function_context() {
        // When we don't have context about a variable, it's unknown
        let ctx = GoScopeContext::new(vec![]);
        let target = classify_go_mutation_target("someUnknownVar", &ctx);
        assert_eq!(target, GoMutationTarget::Unknown);
    }

    #[test]
    fn test_classify_function_pure() {
        use detrix_core::PurityLevel;
        // Pure functions should be classified as Pure
        assert_eq!(
            GoPurityAnalyzer::classify_function("len"),
            PurityLevel::Pure
        );
        assert_eq!(
            GoPurityAnalyzer::classify_function("append"),
            PurityLevel::Pure
        );
        assert_eq!(
            GoPurityAnalyzer::classify_function("math.Sqrt"),
            PurityLevel::Pure
        );
    }

    #[test]
    fn test_classify_function_impure() {
        use detrix_core::PurityLevel;
        // Impure functions should be classified as Impure
        assert_eq!(
            GoPurityAnalyzer::classify_function("os.Exit"),
            PurityLevel::Impure
        );
        assert_eq!(
            GoPurityAnalyzer::classify_function("exec.Command"),
            PurityLevel::Impure
        );
        assert_eq!(
            GoPurityAnalyzer::classify_function("os.Open"),
            PurityLevel::Impure
        );
    }

    #[test]
    fn test_classify_function_unknown() {
        use detrix_core::PurityLevel;
        // Unknown functions should be classified as Unknown
        assert_eq!(
            GoPurityAnalyzer::classify_function("customFunc"),
            PurityLevel::Unknown
        );
        assert_eq!(
            GoPurityAnalyzer::classify_function("mypackage.Process"),
            PurityLevel::Unknown
        );
    }

    #[test]
    fn test_classify_function_acceptable_impure() {
        use detrix_core::PurityLevel;
        // Acceptable impure functions (logging) are treated as pure
        assert_eq!(
            GoPurityAnalyzer::classify_function("fmt.Println"),
            PurityLevel::Pure
        );
        assert_eq!(
            GoPurityAnalyzer::classify_function("log.Print"),
            PurityLevel::Pure
        );
    }

    // =========================================================================
    // Body analysis tests
    // =========================================================================

    #[test]
    fn test_extract_go_signature_function() {
        // Simple function
        let (receiver, params) = GoPurityAnalyzer::extract_go_signature("func Add(a, b int) int {");
        assert!(receiver.is_none());
        assert_eq!(params, vec!["a", "b"]);
    }

    #[test]
    fn test_extract_go_signature_method() {
        // Method with receiver
        let (receiver, params) =
            GoPurityAnalyzer::extract_go_signature("func (s *Server) Start(port int) error {");
        assert_eq!(receiver, Some("s".to_string()));
        assert_eq!(params, vec!["port"]);
    }

    #[test]
    fn test_extract_go_signature_pointer_receiver() {
        let (receiver, params) =
            GoPurityAnalyzer::extract_go_signature("func (c *Config) Get(key string) string {");
        assert_eq!(receiver, Some("c".to_string()));
        assert_eq!(params, vec!["key"]);
    }

    #[test]
    fn test_extract_go_signature_value_receiver() {
        let (receiver, params) =
            GoPurityAnalyzer::extract_go_signature("func (p Point) Distance() float64 {");
        assert_eq!(receiver, Some("p".to_string()));
        assert!(params.is_empty());
    }

    #[test]
    fn test_extract_go_signature_multiple_params() {
        let (receiver, params) = GoPurityAnalyzer::extract_go_signature(
            "func Process(name string, count int, data []byte) error {",
        );
        assert!(receiver.is_none());
        assert_eq!(params, vec!["name", "count", "data"]);
    }

    #[test]
    fn test_extract_go_signature_no_params() {
        let (receiver, params) = GoPurityAnalyzer::extract_go_signature("func GetAll() []int {");
        assert!(receiver.is_none());
        assert!(params.is_empty());
    }

    #[test]
    fn test_build_scope_context_function() {
        let body = r#"
            result := 0
            for _, v := range items {
                result += v
            }
            return result
        "#;
        let params = vec!["items".to_string()];
        let ctx = GoPurityAnalyzer::build_scope_context(body, params, None);

        assert!(ctx.is_parameter("items"));
        assert!(ctx.is_local("result"));
        assert!(ctx.is_local("v"));
        assert!(!ctx.is_receiver("self"));
    }

    #[test]
    fn test_build_scope_context_method() {
        let body = r#"
            s.count++
            return s.count
        "#;
        let params = vec![];
        let ctx = GoPurityAnalyzer::build_scope_context(body, params, Some("s".to_string()));

        assert!(ctx.is_receiver("s"));
        assert!(!ctx.is_local("s"));
    }

    #[test]
    fn test_analyze_function_body_pure() {
        let body = r#"
            result := a + b
            return result
        "#;
        let params = vec!["a".to_string(), "b".to_string()];
        let (is_pure, impure_calls, _user_calls) =
            GoPurityAnalyzer::analyze_function_body_with_params(body, params, None);

        assert!(is_pure);
        assert!(impure_calls.is_empty());
    }

    #[test]
    fn test_analyze_function_body_impure_call() {
        let body = r#"
            data, err := os.ReadFile(path)
            if err != nil {
                return nil, err
            }
            return data, nil
        "#;
        let params = vec!["path".to_string()];
        let (is_pure, impure_calls, _user_calls) =
            GoPurityAnalyzer::analyze_function_body_with_params(body, params, None);

        assert!(!is_pure);
        assert!(!impure_calls.is_empty());
        assert!(impure_calls
            .iter()
            .any(|c| c.function_name.contains("os.ReadFile")));
    }

    #[test]
    fn test_analyze_function_body_receiver_mutation() {
        let body = r#"
            s.value = newValue
            s.count++
        "#;
        let params = vec!["newValue".to_string()];
        let (is_pure, impure_calls, _user_calls) =
            GoPurityAnalyzer::analyze_function_body_with_params(
                body,
                params,
                Some("s".to_string()),
            );

        assert!(!is_pure);
        // Should detect receiver mutation
        assert!(impure_calls.iter().any(|c| c.reason.contains("receiver")));
    }

    #[test]
    fn test_analyze_function_body_local_mutation_pure() {
        let body = r#"
            result := 0
            result = result + 1
            result++
            return result
        "#;
        let params = vec![];
        let (is_pure, impure_calls, _user_calls) =
            GoPurityAnalyzer::analyze_function_body_with_params(body, params, None);

        // Local mutation is pure
        assert!(is_pure);
        assert!(impure_calls.is_empty());
    }

    #[test]
    fn test_analyze_function_body_parameter_mutation_impure() {
        let body = r#"
            items[0] = newItem
            return items
        "#;
        let params = vec!["items".to_string(), "newItem".to_string()];
        let (is_pure, impure_calls, _user_calls) =
            GoPurityAnalyzer::analyze_function_body_with_params(body, params, None);

        assert!(!is_pure);
        // Should detect parameter mutation
        assert!(impure_calls.iter().any(|c| c.reason.contains("parameter")));
    }

    #[test]
    fn test_analyze_function_body_with_pure_stdlib() {
        let body = r#"
            result := len(items)
            total := math.Sqrt(float64(result))
            return total
        "#;
        let params = vec!["items".to_string()];
        let (is_pure, impure_calls, _user_calls) =
            GoPurityAnalyzer::analyze_function_body_with_params(body, params, None);

        assert!(is_pure);
        assert!(impure_calls.is_empty());
    }

    #[test]
    fn test_analyze_function_body_user_calls_detected() {
        let body = r#"
            result := processData(input)
            return result
        "#;
        let params = vec!["input".to_string()];
        let (_is_pure, _impure_calls, user_calls) =
            GoPurityAnalyzer::analyze_function_body_with_params(body, params, None);

        // Should detect user-defined function call
        assert!(user_calls.contains(&"processData".to_string()));
    }

    #[test]
    fn test_analyze_function_body_method_call_on_receiver() {
        let body = r#"
            s.Lock()
            defer s.Unlock()
            s.data = append(s.data, item)
        "#;
        let params = vec!["item".to_string()];
        let (is_pure, impure_calls, _user_calls) =
            GoPurityAnalyzer::analyze_function_body_with_params(
                body,
                params,
                Some("s".to_string()),
            );

        assert!(!is_pure);
        // Should detect Lock/Unlock as mutation methods
        assert!(impure_calls
            .iter()
            .any(|c| c.function_name.contains("Lock")));
    }

    #[test]
    fn test_analyze_function_body_http_call() {
        let body = r#"
            resp, err := http.Get(url)
            if err != nil {
                return nil, err
            }
            defer resp.Body.Close()
            return io.ReadAll(resp.Body)
        "#;
        let params = vec!["url".to_string()];
        let (is_pure, impure_calls, _user_calls) =
            GoPurityAnalyzer::analyze_function_body_with_params(body, params, None);

        assert!(!is_pure);
        assert!(impure_calls
            .iter()
            .any(|c| c.function_name.contains("http.Get")));
    }
}
