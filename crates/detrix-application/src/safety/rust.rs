//! Rust expression safety validator
//!
//! Uses in-process tree-sitter parsing to validate Rust expressions.
//! Detects prohibited constructs and classifies function calls by purity.
//!
//! Since Rust is a compiled language with static typing and memory safety,
//! the safety analysis focuses on:
//! - Detecting unsafe blocks/operations
//! - Identifying I/O operations (file, network, process)
//! - Tracking mutable references and state mutations
//! - Blocking sensitive variable access patterns

use super::base_validator::BaseValidator;
use super::treesitter::analyze_rust;
use super::validation_result::ValidationResult;
use super::ExpressionValidator;
use crate::error::Result;
use detrix_config::RustSafetyConfig;
use detrix_core::{PurityLevel, SafetyLevel, SourceLanguage};
use std::collections::HashSet;

/// Rust expression validator
///
/// Validates Rust expressions using in-process tree-sitter parsing.
/// Detects prohibited constructs and classifies function calls.
///
/// Rust has unique safety characteristics:
/// - No dynamic code execution (no eval/exec)
/// - Ownership system prevents data races
/// - `unsafe` blocks mark potentially dangerous code
/// - Static typing catches many errors at compile time
#[derive(Debug, Clone)]
pub struct RustValidator {
    /// Functions allowed in strict mode (whitelist)
    allowed_functions: HashSet<String>,

    /// Functions always prohibited (blacklist)
    prohibited_functions: HashSet<String>,

    /// Sensitive variable patterns to block
    sensitive_patterns: Vec<String>,
}

impl RustValidator {
    /// Create a new Rust validator from config
    pub fn new(config: &RustSafetyConfig) -> Self {
        Self {
            allowed_functions: config.allowed_set(),
            prohibited_functions: config.prohibited_set(),
            sensitive_patterns: config.sensitive_patterns().clone(),
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(&RustSafetyConfig::default())
    }
}

impl BaseValidator for RustValidator {
    fn allowed_functions(&self) -> &HashSet<String> {
        &self.allowed_functions
    }

    fn prohibited_functions(&self) -> &HashSet<String> {
        &self.prohibited_functions
    }

    fn sensitive_patterns(&self) -> &[String] {
        &self.sensitive_patterns
    }

    /// Rust-specific base name extraction for `::` and `.` separators.
    ///
    /// Handles paths like "Vec::push" → "push" and "items.len" → "len".
    fn extract_base_name<'a>(&self, func_name: &'a str) -> &'a str {
        let base_name = func_name.rsplit("::").next().unwrap_or(func_name);
        base_name.split('.').next_back().unwrap_or(base_name)
    }

    /// Rust-specific sensitive pattern matching with snake_case support.
    ///
    /// Matches patterns like "api-key" against snake_case variables like "api_key".
    fn check_sensitive_pattern(&self, var_name: &str) -> Option<&str> {
        // First try base implementation
        if let Some(pattern) = self.matches_sensitive_pattern_base(var_name) {
            return Some(pattern);
        }
        // Rust-specific: Also check snake_case patterns (e.g., "api-key" as "api_key")
        let var_lower = var_name.to_lowercase();
        self.sensitive_patterns
            .iter()
            .find(|pattern| {
                let pattern_snake = pattern.replace('-', "_");
                var_lower.contains(&pattern_snake.to_lowercase())
            })
            .map(|s| s.as_str())
    }
}

impl ExpressionValidator for RustValidator {
    fn language(&self) -> SourceLanguage {
        SourceLanguage::Rust
    }

    fn validate(&self, expression: &str, safety_level: SafetyLevel) -> Result<ValidationResult> {
        let ast_result = analyze_rust(expression, safety_level, &self.allowed_functions);

        // Rust-specific: unsafe blocks make purity Impure from the start
        let initial_purity = if ast_result.has_unsafe {
            PurityLevel::Impure
        } else {
            PurityLevel::Pure
        };

        Ok(self.validate_ast_result(ast_result, safety_level, initial_purity))
    }

    fn is_function_allowed(&self, func_name: &str, level: SafetyLevel) -> bool {
        self.is_function_allowed_base(func_name, level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_validator() -> RustValidator {
        RustValidator::new(&RustSafetyConfig::default())
    }

    #[test]
    fn test_rust_validator_new() {
        let config = RustSafetyConfig::default();
        let validator = RustValidator::new(&config);

        assert!(validator.allowed_functions.contains("len"));
        assert!(validator.allowed_functions.contains("to_string"));
        assert!(validator
            .prohibited_functions
            .contains("std::process::exit"));
        assert!(validator.prohibited_functions.contains("std::fs::write"));
    }

    #[test]
    fn test_language() {
        let validator = default_validator();
        assert_eq!(validator.language(), SourceLanguage::Rust);
    }

    #[test]
    fn test_is_function_allowed_strict() {
        let validator = default_validator();

        // Allowed functions
        assert!(validator.is_function_allowed("len", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("to_string", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("clone", SafetyLevel::Strict));

        // Not in whitelist
        assert!(!validator.is_function_allowed("custom_func", SafetyLevel::Strict));

        // Prohibited (always blocked)
        assert!(!validator.is_function_allowed("std::process::exit", SafetyLevel::Strict));
        assert!(!validator.is_function_allowed("std::fs::write", SafetyLevel::Strict));
    }

    #[test]
    fn test_is_function_allowed_trusted() {
        let validator = default_validator();

        // Custom function allowed in trusted mode
        assert!(validator.is_function_allowed("custom_func", SafetyLevel::Trusted));

        // But prohibited still blocked
        assert!(!validator.is_function_allowed("std::process::exit", SafetyLevel::Trusted));
        assert!(!validator.is_function_allowed("std::fs::write", SafetyLevel::Trusted));
    }

    #[test]
    fn test_check_sensitive_pattern() {
        let validator = default_validator();

        // Direct match
        assert!(validator.check_sensitive_pattern("password").is_some());
        assert!(validator.check_sensitive_pattern("secret").is_some());

        // Contains match (Rust style: snake_case)
        assert!(validator.check_sensitive_pattern("user_password").is_some());
        assert!(validator.check_sensitive_pattern("api_key").is_some());
        assert!(validator.check_sensitive_pattern("access_token").is_some());

        // Safe variables
        assert!(validator.check_sensitive_pattern("user_name").is_none());
        assert!(validator.check_sensitive_pattern("count").is_none());
    }

    #[test]
    fn test_validate_safe_expression() {
        let validator = default_validator();

        // Safe expression using whitelisted function
        let result = validator
            .validate("items.len()", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_validate_unsafe_keyword() {
        let validator = default_validator();

        // Expression with unsafe keyword
        let result = validator
            .validate("unsafe { *ptr }", SafetyLevel::Strict)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("Unsafe") || e.contains("unsafe")));
    }

    #[test]
    fn test_validate_prohibited_function() {
        let validator = default_validator();

        // std::process::exit is always prohibited
        let result = validator
            .validate("std::process::exit(1)", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_validate_sensitive_variable() {
        let validator = default_validator();

        // Access to password variable
        let result = validator
            .validate("user_password", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result.warnings.iter().any(|w| w.contains("Sensitive")));
    }

    #[test]
    fn test_validate_non_whitelisted_strict() {
        let validator = default_validator();

        // custom_func is not in whitelist - should fail in strict mode
        let result = validator
            .validate("custom_func(x)", SafetyLevel::Strict)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result.errors.iter().any(|e| e.contains("Non-whitelisted")));
    }

    #[test]
    fn test_validate_non_whitelisted_trusted() {
        let validator = default_validator();

        // custom_func allowed in trusted mode
        let result = validator
            .validate("custom_func(x)", SafetyLevel::Trusted)
            .unwrap();

        // In trusted mode, non-whitelisted functions are allowed
        assert!(result.is_safe);
        assert!(result.unknown_functions.contains("custom_func"));
    }

    #[test]
    fn test_purity_classification() {
        let validator = default_validator();

        // Pure expression (whitelisted functions only)
        let result = validator
            .validate("items.iter().collect()", SafetyLevel::Strict)
            .unwrap();

        // iter and collect are whitelisted
        assert!(result.is_safe);
        assert_eq!(result.purity, PurityLevel::Pure);
    }

    #[test]
    fn test_purity_impure_function() {
        let validator = default_validator();

        // Expression with prohibited function should be Impure
        let result = validator
            .validate("std::fs::write(path, data)", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert_eq!(result.purity, PurityLevel::Impure);
    }

    #[test]
    fn test_rust_specific_allowed_functions() {
        let validator = default_validator();

        // Iterator methods
        assert!(validator.is_function_allowed("iter", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("map", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("filter", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("collect", SafetyLevel::Strict));

        // String methods
        assert!(validator.is_function_allowed("to_string", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("trim", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("contains", SafetyLevel::Strict));

        // Option/Result methods
        assert!(validator.is_function_allowed("is_some", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("unwrap_or", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("map_err", SafetyLevel::Strict));
    }

    #[test]
    fn test_rust_specific_prohibited_functions() {
        let validator = default_validator();

        // Process operations
        assert!(!validator.is_function_allowed("std::process::exit", SafetyLevel::Trusted));
        assert!(!validator.is_function_allowed("std::process::abort", SafetyLevel::Trusted));

        // File I/O
        assert!(!validator.is_function_allowed("std::fs::write", SafetyLevel::Trusted));
        assert!(!validator.is_function_allowed("std::fs::remove_file", SafetyLevel::Trusted));

        // Network
        assert!(
            !validator.is_function_allowed("std::net::TcpStream::connect", SafetyLevel::Trusted)
        );

        // Unsafe memory operations
        assert!(!validator.is_function_allowed("std::mem::transmute", SafetyLevel::Trusted));
        assert!(!validator.is_function_allowed("std::ptr::write", SafetyLevel::Trusted));
    }

    #[test]
    fn test_validate_complex_expression() {
        let validator = default_validator();

        // Complex but safe expression using chained methods
        let result = validator
            .validate(
                "items.iter().filter(|x| x > 0).collect()",
                SafetyLevel::Strict,
            )
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_method_call() {
        let validator = default_validator();

        // Method call in trusted mode
        let result = validator
            .validate("user.get_id()", SafetyLevel::Trusted)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_nested_attribute_access() {
        let validator = default_validator();

        // Nested attribute access should be safe
        let result = validator
            .validate("user.profile.settings.theme", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_password_attribute_blocked() {
        let validator = default_validator();

        // Attribute access to password should be blocked
        let result = validator
            .validate("user.password_hash", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("password") || w.contains("Sensitive")));
    }

    #[test]
    fn test_validate_secret_variable_blocked() {
        let validator = default_validator();

        // Secret variable should be blocked
        let result = validator
            .validate("secret_key", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("secret") || w.contains("Sensitive")));
    }

    #[test]
    fn test_validate_safe_variable_allowed() {
        let validator = default_validator();

        // Non-sensitive variables should be allowed
        let result = validator
            .validate("user.name", SafetyLevel::Trusted)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_purity_pure_whitelisted_functions() {
        let validator = default_validator();

        // Expression with only whitelisted functions should be Pure
        let result = validator
            .validate("items.iter().collect()", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert_eq!(result.purity, PurityLevel::Pure);
        assert!(result.unknown_functions.is_empty());
        assert!(result.impure_functions.is_empty());
    }

    #[test]
    fn test_purity_impure_blacklisted_function() {
        let validator = default_validator();

        // Expression with blacklisted function should be Impure
        let result = validator
            .validate("std::process::exit(1)", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert_eq!(result.purity, PurityLevel::Impure);
        assert!(result.impure_functions.contains("std::process::exit"));
    }

    #[test]
    fn test_purity_unknown_user_function_trusted() {
        let validator = default_validator();

        // In trusted mode, unknown user functions are allowed but tracked
        let result = validator
            .validate("user.get_data()", SafetyLevel::Trusted)
            .unwrap();

        // Trusted mode allows non-whitelisted functions
        assert!(result.is_safe);
        assert_eq!(result.purity, PurityLevel::Unknown);
    }

    #[test]
    fn test_purity_no_functions_is_pure() {
        let validator = default_validator();

        // Expression with no function calls should be Pure
        let result = validator
            .validate("user.name", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert_eq!(result.purity, PurityLevel::Pure);
        assert!(result.function_calls.is_empty());
        assert!(result.unknown_functions.is_empty());
    }

    #[test]
    fn test_validate_option_methods_safe() {
        let validator = default_validator();

        // Option methods are safe
        let result = validator
            .validate("value.is_some()", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_result_methods_safe() {
        let validator = default_validator();

        // Result methods are safe
        let result = validator
            .validate("result.is_ok()", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_vec_methods_safe() {
        let validator = default_validator();

        // Vec read-only methods are safe
        let result = validator
            .validate("items.len()", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }
}
