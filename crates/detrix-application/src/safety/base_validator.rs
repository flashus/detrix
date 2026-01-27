//! Base validator trait and function classification types
//!
//! Common validation logic shared by all language-specific validators.

use super::treesitter::TreeSitterResult;
use super::validation_result::ValidationResult;
use detrix_core::{PurityLevel, SafetyLevel};
use std::collections::HashSet;

/// Common validation logic shared by all language validators.
///
/// This trait provides default implementations for pattern matching and
/// function classification that can be reused across language-specific validators.
pub trait BaseValidator {
    /// Get the set of allowed (whitelisted) functions
    fn allowed_functions(&self) -> &HashSet<String>;

    /// Get the set of prohibited (blacklisted) functions
    fn prohibited_functions(&self) -> &HashSet<String>;

    /// Get the list of sensitive variable patterns
    fn sensitive_patterns(&self) -> &[String];

    /// Check if a function is allowed at the given safety level
    ///
    /// Precedence order:
    /// 1. Whitelist (highest priority) - user can override blacklist via config
    /// 2. Blacklist - blocks prohibited functions
    /// 3. Safety level default - Strict blocks unknown, Trusted allows
    fn is_function_allowed_base(&self, func_name: &str, level: SafetyLevel) -> bool {
        // 1. WHITELIST FIRST - user can override blacklist by adding to allowed_functions
        if self.allowed_functions().contains(func_name) {
            return true;
        }

        // 2. BLACKLIST SECOND - block prohibited functions
        if self.prohibited_functions().contains(func_name) {
            return false;
        }

        // 3. Unknown function - depends on safety level
        match level {
            SafetyLevel::Strict => false, // Block unknown in strict mode
            SafetyLevel::Trusted => true, // Allow unknown in trusted mode
        }
    }

    /// Check if a variable name matches any sensitive pattern
    ///
    /// Default implementation does case-insensitive substring matching.
    /// Override for language-specific conventions (camelCase, snake_case, etc.)
    fn matches_sensitive_pattern_base(&self, var_name: &str) -> Option<&str> {
        let var_lower = var_name.to_lowercase();
        self.sensitive_patterns()
            .iter()
            .find(|pattern| {
                let pattern_lower = pattern.to_lowercase();
                var_lower.contains(&pattern_lower)
            })
            .map(|s| s.as_str())
    }

    /// Classify a function by its purity level
    ///
    /// Precedence order (whitelist first):
    /// - `PurityLevel::Pure` if function is in allowed list (whitelist)
    /// - `PurityLevel::Impure` if function is in prohibited list (blacklist)
    /// - `PurityLevel::Unknown` if function is not in either list
    fn classify_function(&self, func_name: &str) -> PurityLevel {
        // Whitelist first - user can override blacklist
        if self.allowed_functions().contains(func_name) {
            PurityLevel::Pure
        } else if self.prohibited_functions().contains(func_name) {
            PurityLevel::Impure
        } else {
            PurityLevel::Unknown
        }
    }

    /// Classify function with suffix matching support
    ///
    /// Checks if the function name ends with any allowed/prohibited function.
    /// Useful for fully-qualified names like `std::fs::read` matching `read`.
    /// Precedence: whitelist (exact) → whitelist (suffix) → blacklist (exact) → blacklist (suffix)
    fn classify_function_with_suffix(&self, func_name: &str) -> PurityLevel {
        // 1. Exact match - whitelist first
        if self.allowed_functions().contains(func_name) {
            return PurityLevel::Pure;
        }
        if self.prohibited_functions().contains(func_name) {
            return PurityLevel::Impure;
        }

        // 2. Suffix match - whitelist first
        if self
            .allowed_functions()
            .iter()
            .any(|a| func_name.ends_with(a))
        {
            return PurityLevel::Pure;
        }

        if self
            .prohibited_functions()
            .iter()
            .any(|p| func_name.ends_with(p))
        {
            return PurityLevel::Impure;
        }

        PurityLevel::Unknown
    }

    /// Extract base name from a fully-qualified function name.
    ///
    /// Default implementation handles dot-separated paths (e.g., "fmt.Sprintf" → "Sprintf").
    /// Override for language-specific conventions (e.g., Rust's "::" separator).
    fn extract_base_name<'a>(&self, func_name: &'a str) -> &'a str {
        func_name.split('.').next_back().unwrap_or(func_name)
    }

    /// Check if a variable name matches any sensitive pattern.
    ///
    /// Returns the matched pattern if found.
    /// Default implementation uses `matches_sensitive_pattern_base`.
    /// Override for language-specific conventions (camelCase, snake_case).
    fn check_sensitive_pattern(&self, var_name: &str) -> Option<&str> {
        self.matches_sensitive_pattern_base(var_name)
    }

    /// Shared validation logic for all language validators.
    ///
    /// Takes the tree-sitter AST result and produces a ValidationResult.
    /// This implements the common validation flow:
    /// 1. Start with AST violations as errors
    /// 2. Classify each function call (whitelist → blacklist → unknown)
    /// 3. Check variables against sensitive patterns
    /// 4. Compute final safety based on errors and sensitive variables
    ///
    /// Language-specific behavior is handled via:
    /// - `extract_base_name`: Override for different path separators
    /// - `check_sensitive_pattern`: Override for camelCase/snake_case
    /// - `initial_purity`: Pass PurityLevel::Impure if has_unsafe (Rust)
    fn validate_ast_result(
        &self,
        ast_result: TreeSitterResult,
        _safety_level: SafetyLevel,
        initial_purity: PurityLevel,
    ) -> ValidationResult {
        let function_calls = ast_result.function_calls;
        let variable_accesses = ast_result.variable_accesses;

        // Start with AST violations
        let mut errors = ast_result.violations;
        let mut warnings = Vec::new();
        let mut unknown_functions = HashSet::new();
        let mut impure_functions = HashSet::new();
        let mut purity = initial_purity;

        // Classify each function by purity
        // Precedence: whitelist first (user can override blacklist), then blacklist, then unknown
        for func in &function_calls {
            let base_name = self.extract_base_name(func);

            // 1. WHITELIST FIRST - user can override blacklist by adding to allowed_functions
            if self.allowed_functions().contains(func)
                || self.allowed_functions().contains(base_name)
            {
                // Function is whitelisted - pure
                // No action needed, purity stays as is
            }
            // 2. BLACKLIST SECOND - check if function is prohibited (impure)
            else if self.prohibited_functions().contains(func) {
                errors.push(format!(
                    "Prohibited function '{}' detected (blacklisted)",
                    func
                ));
                impure_functions.insert(func.clone());
                purity = purity.combine(PurityLevel::Impure);
            } else if self.prohibited_functions().contains(base_name)
                && !errors.iter().any(|e| e.contains(func))
            {
                errors.push(format!(
                    "Prohibited function '{}' detected (blacklisted)",
                    base_name
                ));
                impure_functions.insert(func.clone());
                purity = purity.combine(PurityLevel::Impure);
            }
            // 3. UNKNOWN - needs LSP analysis
            else {
                unknown_functions.insert(func.clone());
                purity = purity.combine(PurityLevel::Unknown);

                // Add warning for unknown functions (applies in all modes)
                warnings.push(format!(
                    "Unknown function '{}' - purity cannot be determined without LSP analysis",
                    func
                ));
            }
        }

        // Check variable/attribute accesses against sensitive patterns
        for var in &variable_accesses {
            if let Some(pattern) = self.check_sensitive_pattern(var) {
                warnings.push(format!(
                    "Sensitive variable '{}' detected (matches pattern '{}')",
                    var, pattern
                ));
            }
        }

        // Sensitive variables cause the expression to be unsafe
        let has_sensitive_vars = warnings.iter().any(|w| w.contains("Sensitive variable"));

        // Final safety determination
        let is_safe = errors.is_empty() && ast_result.is_safe && !has_sensitive_vars;

        ValidationResult {
            is_safe,
            purity,
            errors,
            warnings,
            function_calls,
            unknown_functions,
            impure_functions,
        }
    }
}

/// Result of classifying a function during validation
#[derive(Debug, Clone)]
pub struct FunctionClassification {
    /// Name of the function
    pub name: String,

    /// Purity level of the function
    pub purity: PurityLevel,

    /// Error message if function is prohibited
    pub error: Option<String>,

    /// Warning message if function is unknown
    pub warning: Option<String>,
}

impl FunctionClassification {
    /// Create a classification for a pure (whitelisted) function
    pub fn pure(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            purity: PurityLevel::Pure,
            error: None,
            warning: None,
        }
    }

    /// Create a classification for an impure (prohibited) function
    pub fn impure(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            error: Some(format!(
                "Prohibited function '{}' detected (blacklisted)",
                name
            )),
            name,
            purity: PurityLevel::Impure,
            warning: None,
        }
    }

    /// Create a classification for an unknown function
    pub fn unknown(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            warning: Some(format!(
                "Unknown function '{}' - purity cannot be determined",
                name
            )),
            name,
            purity: PurityLevel::Unknown,
            error: None,
        }
    }

    /// Create a classification for an unknown function with a custom warning
    pub fn unknown_with_warning(name: impl Into<String>, warning: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            purity: PurityLevel::Unknown,
            error: None,
            warning: Some(warning.into()),
        }
    }

    /// Create a classification for an unknown function that errors in strict mode
    pub fn unknown_strict_error(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            error: Some(format!(
                "Non-whitelisted function '{}' in strict mode",
                name
            )),
            warning: None,
            name,
            purity: PurityLevel::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // FunctionClassification tests
    // =========================================================================

    #[test]
    fn test_function_classification_pure() {
        let fc = FunctionClassification::pure("len");
        assert_eq!(fc.name, "len");
        assert_eq!(fc.purity, PurityLevel::Pure);
        assert!(fc.error.is_none());
        assert!(fc.warning.is_none());
    }

    #[test]
    fn test_function_classification_impure() {
        let fc = FunctionClassification::impure("eval");
        assert_eq!(fc.name, "eval");
        assert_eq!(fc.purity, PurityLevel::Impure);
        assert!(fc.error.is_some());
        assert!(fc.error.unwrap().contains("blacklisted"));
        assert!(fc.warning.is_none());
    }

    #[test]
    fn test_function_classification_unknown() {
        let fc = FunctionClassification::unknown("custom_func");
        assert_eq!(fc.name, "custom_func");
        assert_eq!(fc.purity, PurityLevel::Unknown);
        assert!(fc.error.is_none());
        assert!(fc.warning.is_some());
        assert!(fc.warning.unwrap().contains("purity cannot be determined"));
    }

    #[test]
    fn test_function_classification_unknown_with_warning() {
        let fc = FunctionClassification::unknown_with_warning("custom", "Custom warning message");
        assert_eq!(fc.name, "custom");
        assert_eq!(fc.purity, PurityLevel::Unknown);
        assert!(fc.error.is_none());
        assert_eq!(fc.warning.unwrap(), "Custom warning message");
    }

    #[test]
    fn test_function_classification_unknown_strict_error() {
        let fc = FunctionClassification::unknown_strict_error("custom_func");
        assert_eq!(fc.name, "custom_func");
        assert_eq!(fc.purity, PurityLevel::Unknown);
        assert!(fc.error.is_some());
        assert!(fc.error.unwrap().contains("Non-whitelisted"));
        assert!(fc.warning.is_none());
    }

    // =========================================================================
    // BaseValidator tests (using a test implementation)
    // =========================================================================

    struct TestValidator {
        allowed: HashSet<String>,
        prohibited: HashSet<String>,
        patterns: Vec<String>,
    }

    impl TestValidator {
        fn new() -> Self {
            let mut allowed = HashSet::new();
            allowed.insert("len".to_string());
            allowed.insert("str".to_string());
            allowed.insert("int".to_string());

            let mut prohibited = HashSet::new();
            prohibited.insert("eval".to_string());
            prohibited.insert("exec".to_string());
            prohibited.insert("os.system".to_string());

            let patterns = vec![
                "password".to_string(),
                "secret".to_string(),
                "api_key".to_string(),
            ];

            Self {
                allowed,
                prohibited,
                patterns,
            }
        }
    }

    impl BaseValidator for TestValidator {
        fn allowed_functions(&self) -> &HashSet<String> {
            &self.allowed
        }

        fn prohibited_functions(&self) -> &HashSet<String> {
            &self.prohibited
        }

        fn sensitive_patterns(&self) -> &[String] {
            &self.patterns
        }
    }

    #[test]
    fn test_base_validator_is_function_allowed_strict() {
        let v = TestValidator::new();

        // Allowed function in strict mode
        assert!(v.is_function_allowed_base("len", SafetyLevel::Strict));
        assert!(v.is_function_allowed_base("str", SafetyLevel::Strict));

        // Not in whitelist - blocked in strict mode
        assert!(!v.is_function_allowed_base("custom", SafetyLevel::Strict));

        // Prohibited - always blocked
        assert!(!v.is_function_allowed_base("eval", SafetyLevel::Strict));
    }

    #[test]
    fn test_base_validator_is_function_allowed_trusted() {
        let v = TestValidator::new();

        // Allowed function
        assert!(v.is_function_allowed_base("len", SafetyLevel::Trusted));

        // Not in whitelist - allowed in trusted mode
        assert!(v.is_function_allowed_base("custom", SafetyLevel::Trusted));

        // Prohibited - always blocked
        assert!(!v.is_function_allowed_base("eval", SafetyLevel::Trusted));
        assert!(!v.is_function_allowed_base("exec", SafetyLevel::Trusted));
    }

    #[test]
    fn test_whitelist_overrides_blacklist() {
        // Test that whitelist has higher precedence than blacklist
        // This allows users to override default blacklist via config
        let mut allowed = HashSet::new();
        allowed.insert("eval".to_string()); // User whitelists eval (normally blacklisted)
        allowed.insert("len".to_string());

        let mut prohibited = HashSet::new();
        prohibited.insert("eval".to_string()); // eval is also on blacklist
        prohibited.insert("exec".to_string());

        struct OverrideValidator {
            allowed: HashSet<String>,
            prohibited: HashSet<String>,
        }

        impl BaseValidator for OverrideValidator {
            fn allowed_functions(&self) -> &HashSet<String> {
                &self.allowed
            }
            fn prohibited_functions(&self) -> &HashSet<String> {
                &self.prohibited
            }
            fn sensitive_patterns(&self) -> &[String] {
                &[]
            }
        }

        let v = OverrideValidator {
            allowed,
            prohibited,
        };

        // eval is on BOTH lists - whitelist should win (user override)
        assert!(
            v.is_function_allowed_base("eval", SafetyLevel::Strict),
            "Whitelist should override blacklist - eval should be allowed"
        );
        assert!(
            v.is_function_allowed_base("eval", SafetyLevel::Trusted),
            "Whitelist should override blacklist in trusted mode too"
        );

        // exec is only on blacklist - should be blocked
        assert!(
            !v.is_function_allowed_base("exec", SafetyLevel::Strict),
            "exec should be blocked (only on blacklist)"
        );

        // Verify classify_function also respects precedence
        assert_eq!(
            v.classify_function("eval"),
            PurityLevel::Pure,
            "Whitelist should make eval Pure, not Impure"
        );
        assert_eq!(
            v.classify_function("exec"),
            PurityLevel::Impure,
            "exec should be Impure (only on blacklist)"
        );
    }

    #[test]
    fn test_base_validator_matches_sensitive_pattern() {
        let v = TestValidator::new();

        // Direct match
        assert!(v.matches_sensitive_pattern_base("password").is_some());
        assert!(v.matches_sensitive_pattern_base("secret").is_some());

        // Substring match
        assert!(v.matches_sensitive_pattern_base("user_password").is_some());
        assert!(v.matches_sensitive_pattern_base("secretKey").is_some());
        assert!(v.matches_sensitive_pattern_base("my_api_key").is_some());

        // No match
        assert!(v.matches_sensitive_pattern_base("username").is_none());
        assert!(v.matches_sensitive_pattern_base("count").is_none());
    }

    #[test]
    fn test_base_validator_classify_function() {
        let v = TestValidator::new();

        // Allowed function -> Pure
        assert_eq!(v.classify_function("len"), PurityLevel::Pure);
        assert_eq!(v.classify_function("str"), PurityLevel::Pure);

        // Prohibited function -> Impure
        assert_eq!(v.classify_function("eval"), PurityLevel::Impure);
        assert_eq!(v.classify_function("os.system"), PurityLevel::Impure);

        // Unknown function -> Unknown
        assert_eq!(v.classify_function("custom_func"), PurityLevel::Unknown);
    }

    #[test]
    fn test_base_validator_classify_function_with_suffix() {
        let v = TestValidator::new();

        // Exact match
        assert_eq!(v.classify_function_with_suffix("len"), PurityLevel::Pure);
        assert_eq!(v.classify_function_with_suffix("eval"), PurityLevel::Impure);

        // Suffix match - prohibited
        assert_eq!(
            v.classify_function_with_suffix("some.os.system"),
            PurityLevel::Impure
        );

        // Suffix match - allowed
        assert_eq!(
            v.classify_function_with_suffix("items.len"),
            PurityLevel::Pure
        );

        // No match
        assert_eq!(
            v.classify_function_with_suffix("custom_func"),
            PurityLevel::Unknown
        );
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    // === Test Validator for Property Tests ===

    struct PropTestValidator {
        allowed: HashSet<String>,
        prohibited: HashSet<String>,
        patterns: Vec<String>,
    }

    impl PropTestValidator {
        fn new() -> Self {
            let mut allowed = HashSet::new();
            allowed.insert("len".to_string());
            allowed.insert("str".to_string());
            allowed.insert("int".to_string());
            allowed.insert("float".to_string());
            allowed.insert("bool".to_string());

            let mut prohibited = HashSet::new();
            prohibited.insert("eval".to_string());
            prohibited.insert("exec".to_string());
            prohibited.insert("compile".to_string());
            prohibited.insert("open".to_string());
            prohibited.insert("os.system".to_string());

            let patterns = vec![
                "password".to_string(),
                "secret".to_string(),
                "api_key".to_string(),
                "token".to_string(),
            ];

            Self {
                allowed,
                prohibited,
                patterns,
            }
        }
    }

    impl BaseValidator for PropTestValidator {
        fn allowed_functions(&self) -> &HashSet<String> {
            &self.allowed
        }

        fn prohibited_functions(&self) -> &HashSet<String> {
            &self.prohibited
        }

        fn sensitive_patterns(&self) -> &[String] {
            &self.patterns
        }
    }

    // === Generators ===

    /// Generate a prohibited function name
    fn prohibited_func() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("eval".to_string()),
            Just("exec".to_string()),
            Just("compile".to_string()),
            Just("open".to_string()),
            Just("os.system".to_string()),
        ]
    }

    /// Generate an allowed function name
    fn allowed_func() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("len".to_string()),
            Just("str".to_string()),
            Just("int".to_string()),
            Just("float".to_string()),
            Just("bool".to_string()),
        ]
    }

    /// Generate any safety level
    fn any_safety_level() -> impl Strategy<Value = SafetyLevel> {
        prop_oneof![Just(SafetyLevel::Strict), Just(SafetyLevel::Trusted),]
    }

    /// Generate a sensitive pattern variant
    fn sensitive_variant() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("password".to_string()),
            Just("user_password".to_string()),
            Just("PASSWORD".to_string()),
            Just("my_secret".to_string()),
            Just("SECRET_KEY".to_string()),
            Just("api_key".to_string()),
            Just("API_KEY_VALUE".to_string()),
            Just("access_token".to_string()),
            Just("TOKEN".to_string()),
        ]
    }

    /// Generate a non-sensitive variable name
    fn non_sensitive_var() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("user_name".to_string()),
            Just("count".to_string()),
            Just("index".to_string()),
            Just("result".to_string()),
            Just("value".to_string()),
            Just("data".to_string()),
        ]
    }

    // === Properties ===

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Prohibited functions are NEVER allowed, regardless of safety level
        #[test]
        fn proptest_prohibited_never_allowed(
            func in prohibited_func(),
            level in any_safety_level()
        ) {
            let v = PropTestValidator::new();
            prop_assert!(
                !v.is_function_allowed_base(&func, level),
                "Prohibited function '{}' should be blocked at {:?}",
                func,
                level
            );
        }

        /// Allowed functions are always allowed in strict mode
        #[test]
        fn proptest_allowed_pass_strict(func in allowed_func()) {
            let v = PropTestValidator::new();
            prop_assert!(
                v.is_function_allowed_base(&func, SafetyLevel::Strict),
                "Allowed function '{}' should pass strict mode",
                func
            );
        }

        /// Allowed functions are always allowed in trusted mode
        #[test]
        fn proptest_allowed_pass_trusted(func in allowed_func()) {
            let v = PropTestValidator::new();
            prop_assert!(
                v.is_function_allowed_base(&func, SafetyLevel::Trusted),
                "Allowed function '{}' should pass trusted mode",
                func
            );
        }

        /// Unknown functions are blocked in strict mode
        #[test]
        fn proptest_unknown_blocked_strict(func in "[a-z_]{1,20}") {
            let v = PropTestValidator::new();
            // Skip if it happens to be in allowed or prohibited
            if v.allowed_functions().contains(&func) || v.prohibited_functions().contains(&func) {
                return Ok(());
            }
            prop_assert!(
                !v.is_function_allowed_base(&func, SafetyLevel::Strict),
                "Unknown function '{}' should be blocked in strict mode",
                func
            );
        }

        /// Unknown functions are allowed in trusted mode
        #[test]
        fn proptest_unknown_allowed_trusted(func in "[a-z_]{1,20}") {
            let v = PropTestValidator::new();
            // Skip if it happens to be in prohibited
            if v.prohibited_functions().contains(&func) {
                return Ok(());
            }
            prop_assert!(
                v.is_function_allowed_base(&func, SafetyLevel::Trusted),
                "Unknown function '{}' should be allowed in trusted mode",
                func
            );
        }

        /// Sensitive patterns are always detected (case-insensitive)
        #[test]
        fn proptest_sensitive_patterns_detected(var in sensitive_variant()) {
            let v = PropTestValidator::new();
            prop_assert!(
                v.matches_sensitive_pattern_base(&var).is_some(),
                "Sensitive variable '{}' should be detected",
                var
            );
        }

        /// Non-sensitive variables are not flagged
        #[test]
        fn proptest_non_sensitive_not_flagged(var in non_sensitive_var()) {
            let v = PropTestValidator::new();
            prop_assert!(
                v.matches_sensitive_pattern_base(&var).is_none(),
                "Non-sensitive variable '{}' should not be flagged",
                var
            );
        }

        /// classify_function returns consistent results with is_function_allowed
        #[test]
        fn proptest_classify_consistent_with_allowed(func in "[a-z_.]{1,20}") {
            let v = PropTestValidator::new();
            let purity = v.classify_function(&func);

            match purity {
                PurityLevel::Impure => {
                    // Impure functions should never be allowed
                    prop_assert!(!v.is_function_allowed_base(&func, SafetyLevel::Trusted));
                }
                PurityLevel::Pure => {
                    // Pure functions should be in allowed set
                    prop_assert!(v.allowed_functions().contains(&func));
                }
                PurityLevel::Unknown => {
                    // Unknown functions are allowed in trusted, blocked in strict
                    prop_assert!(v.is_function_allowed_base(&func, SafetyLevel::Trusted));
                    prop_assert!(!v.is_function_allowed_base(&func, SafetyLevel::Strict));
                }
            }
        }

        /// Suffix classification is consistent with exact classification
        #[test]
        fn proptest_suffix_classification_consistency(
            prefix in "[a-z]{1,10}",
            func in allowed_func()
        ) {
            let v = PropTestValidator::new();
            let suffixed = format!("{}.{}", prefix, func);

            // If func is pure, suffixed version should also be pure
            let exact_purity = v.classify_function(&func);
            let suffix_purity = v.classify_function_with_suffix(&suffixed);

            if exact_purity == PurityLevel::Pure {
                prop_assert_eq!(
                    suffix_purity,
                    PurityLevel::Pure,
                    "Suffix '{}' should inherit purity from '{}'",
                    suffixed,
                    func
                );
            }
        }

        /// Prohibited suffix classification
        #[test]
        fn proptest_prohibited_suffix_impure(
            prefix in "[a-z]{1,10}",
            func in prohibited_func()
        ) {
            let v = PropTestValidator::new();
            let suffixed = format!("{}.{}", prefix, func);

            prop_assert_eq!(
                v.classify_function_with_suffix(&suffixed),
                PurityLevel::Impure,
                "Suffix '{}' should be Impure",
                suffixed
            );
        }
    }
}
