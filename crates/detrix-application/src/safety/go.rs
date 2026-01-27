//! Go expression safety validator
//!
//! Uses in-process tree-sitter parsing to validate Go expressions.
//! Detects prohibited constructs and classifies function calls by purity.

use super::base_validator::BaseValidator;
use super::treesitter::analyze_go;
use super::validation_result::ValidationResult;
use super::ExpressionValidator;
use crate::error::Result;
use detrix_config::GoSafetyConfig;
use detrix_core::{PurityLevel, SafetyLevel, SourceLanguage};
use std::collections::HashSet;

/// Go expression validator
///
/// Validates Go expressions using in-process tree-sitter parsing.
/// Detects prohibited constructs and classifies function calls.
#[derive(Debug, Clone)]
pub struct GoValidator {
    /// Functions allowed in strict mode (whitelist)
    allowed_functions: HashSet<String>,

    /// Functions always prohibited (blacklist)
    prohibited_functions: HashSet<String>,

    /// Sensitive variable patterns to block
    sensitive_patterns: Vec<String>,
}

impl GoValidator {
    /// Create a new Go validator from config
    pub fn new(config: &GoSafetyConfig) -> Self {
        Self {
            allowed_functions: config.allowed_set(),
            prohibited_functions: config.prohibited_set(),
            sensitive_patterns: config.sensitive_patterns().clone(),
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(&GoSafetyConfig::default())
    }
}

impl BaseValidator for GoValidator {
    fn allowed_functions(&self) -> &HashSet<String> {
        &self.allowed_functions
    }

    fn prohibited_functions(&self) -> &HashSet<String> {
        &self.prohibited_functions
    }

    fn sensitive_patterns(&self) -> &[String] {
        &self.sensitive_patterns
    }

    /// Go-specific sensitive pattern matching with camelCase support.
    ///
    /// Matches patterns like "api_key" against camelCase variables like "apiKey".
    fn check_sensitive_pattern(&self, var_name: &str) -> Option<&str> {
        // First try base implementation
        if let Some(pattern) = self.matches_sensitive_pattern_base(var_name) {
            return Some(pattern);
        }
        // Go-specific: Also check camelCase (e.g., "apiKey" matches pattern "api_key")
        let var_lower = var_name.to_lowercase();
        self.sensitive_patterns.iter().find_map(|pattern| {
            let pattern_camel = pattern.replace('_', "");
            if var_lower.contains(&pattern_camel.to_lowercase()) {
                Some(pattern.as_str())
            } else {
                None
            }
        })
    }
}

impl ExpressionValidator for GoValidator {
    fn language(&self) -> SourceLanguage {
        SourceLanguage::Go
    }

    fn validate(&self, expression: &str, safety_level: SafetyLevel) -> Result<ValidationResult> {
        let ast_result = analyze_go(expression, safety_level, &self.allowed_functions);
        Ok(self.validate_ast_result(ast_result, safety_level, PurityLevel::Pure))
    }

    fn is_function_allowed(&self, func_name: &str, level: SafetyLevel) -> bool {
        self.is_function_allowed_base(func_name, level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_validator() -> GoValidator {
        GoValidator::new(&GoSafetyConfig::default())
    }

    #[test]
    fn test_go_validator_new() {
        let config = GoSafetyConfig::default();
        let validator = GoValidator::new(&config);

        assert!(validator.allowed_functions.contains("len"));
        assert!(validator.allowed_functions.contains("fmt.Sprintf"));
        assert!(validator.prohibited_functions.contains("os.Exit"));
        assert!(validator.prohibited_functions.contains("exec.Command"));
    }

    #[test]
    fn test_language() {
        let validator = default_validator();
        assert_eq!(validator.language(), SourceLanguage::Go);
    }

    #[test]
    fn test_is_function_allowed_strict() {
        let validator = default_validator();

        // Allowed builtin function
        assert!(validator.is_function_allowed("len", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("cap", SafetyLevel::Strict));

        // Allowed stdlib function
        assert!(validator.is_function_allowed("fmt.Sprintf", SafetyLevel::Strict));
        assert!(validator.is_function_allowed("strings.Contains", SafetyLevel::Strict));

        // Not in whitelist
        assert!(!validator.is_function_allowed("custom_func", SafetyLevel::Strict));

        // Prohibited (always blocked)
        assert!(!validator.is_function_allowed("os.Exit", SafetyLevel::Strict));
        assert!(!validator.is_function_allowed("exec.Command", SafetyLevel::Strict));
    }

    #[test]
    fn test_is_function_allowed_trusted() {
        let validator = default_validator();

        // Custom function allowed in trusted mode
        assert!(validator.is_function_allowed("custom_func", SafetyLevel::Trusted));

        // But prohibited still blocked
        assert!(!validator.is_function_allowed("os.Exit", SafetyLevel::Trusted));
        assert!(!validator.is_function_allowed("exec.Command", SafetyLevel::Trusted));
    }

    #[test]
    fn test_check_sensitive_pattern() {
        let validator = default_validator();

        // Direct match
        assert!(validator.check_sensitive_pattern("password").is_some());
        assert!(validator.check_sensitive_pattern("secret").is_some());

        // Contains match (Go style: camelCase)
        assert!(validator.check_sensitive_pattern("userPassword").is_some());
        assert!(validator.check_sensitive_pattern("apiKey").is_some());
        assert!(validator.check_sensitive_pattern("AccessToken").is_some());

        // Safe variables
        assert!(validator.check_sensitive_pattern("userName").is_none());
        assert!(validator.check_sensitive_pattern("count").is_none());
    }

    #[test]
    fn test_validate_safe_expression() {
        let validator = default_validator();

        // Safe expression using builtin
        let result = validator
            .validate("len(items)", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert!(result.errors.is_empty());
        assert!(result.function_calls.contains("len"));
    }

    #[test]
    fn test_validate_prohibited_function() {
        let validator = default_validator();

        // os.Exit is always prohibited
        let result = validator
            .validate("os.Exit(1)", SafetyLevel::Strict)
            .unwrap();

        assert!(!result.is_safe);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_validate_exec_command_prohibited() {
        let validator = default_validator();

        // exec.Command is prohibited
        let result = validator
            .validate("exec.Command(\"ls\")", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result.errors.iter().any(|e| e.contains("exec.Command")
            || e.contains("prohibited")
            || e.contains("blacklist")));
    }

    #[test]
    fn test_validate_fmt_sprintf_allowed() {
        let validator = default_validator();

        // fmt.Sprintf is safe
        let result = validator
            .validate("fmt.Sprintf(\"%d\", x)", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert!(result.function_calls.contains("fmt.Sprintf"));
    }

    #[test]
    fn test_validate_strings_operations() {
        let validator = default_validator();

        // strings package operations are safe
        let result = validator
            .validate("strings.ToLower(s)", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_math_operations() {
        let validator = default_validator();

        // math package operations are safe
        let result = validator
            .validate("math.Sqrt(x)", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_network_prohibited() {
        let validator = default_validator();

        // Network operations are prohibited
        let result = validator
            .validate("http.Get(url)", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result.errors.iter().any(|e| e.contains("http.Get")
            || e.contains("prohibited")
            || e.contains("blacklist")));
    }

    #[test]
    fn test_purity_classification() {
        let validator = default_validator();

        // Pure expression (whitelisted functions only)
        let result = validator
            .validate("len(append(items, x))", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert_eq!(result.purity, PurityLevel::Pure);
    }

    #[test]
    fn test_purity_unknown_function() {
        let validator = default_validator();

        // Unknown function in trusted mode
        let result = validator
            .validate("customFunc(x)", SafetyLevel::Trusted)
            .unwrap();

        assert!(result.is_safe);
        assert_eq!(result.purity, PurityLevel::Unknown);
        assert!(result.unknown_functions.contains("customFunc"));
    }

    #[test]
    fn test_validate_complex_expression() {
        let validator = default_validator();

        // Complex but safe expression using builtin functions
        let result = validator
            .validate("len(append(items, x))", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert!(result.function_calls.contains("len"));
        assert!(result.function_calls.contains("append"));
    }

    #[test]
    fn test_validate_method_call_trusted() {
        let validator = default_validator();

        // Method call in trusted mode
        let result = validator
            .validate("user.GetID()", SafetyLevel::Trusted)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_non_whitelisted_strict() {
        let validator = default_validator();

        // customFunc is not in whitelist - should fail in strict mode
        let result = validator
            .validate("customFunc(x)", SafetyLevel::Strict)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result.errors.iter().any(|e| e.contains("Non-whitelisted")));
    }

    #[test]
    fn test_validate_non_whitelisted_trusted() {
        let validator = default_validator();

        // customFunc allowed in trusted mode
        let result = validator
            .validate("customFunc(x)", SafetyLevel::Trusted)
            .unwrap();

        // In trusted mode, non-whitelisted functions are allowed
        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_nested_attribute_access() {
        let validator = default_validator();

        // Nested attribute access should be safe
        let result = validator
            .validate("user.Profile.Settings.Theme", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_password_variable_blocked() {
        let validator = default_validator();

        // Direct access to password variable should be blocked
        let result = validator
            .validate("password", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("password") || w.contains("Sensitive")));
    }

    #[test]
    fn test_validate_safe_variable_allowed() {
        let validator = default_validator();

        // Non-sensitive variables should be allowed
        let result = validator
            .validate("user.Name", SafetyLevel::Trusted)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_purity_pure_whitelisted_functions() {
        let validator = default_validator();

        // Expression with only whitelisted functions should be Pure
        let result = validator
            .validate("len(append(items, x))", SafetyLevel::Strict)
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
            .validate("os.Exit(1)", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert_eq!(result.purity, PurityLevel::Impure);
        assert!(result.impure_functions.contains("os.Exit"));
    }

    #[test]
    fn test_purity_unknown_user_function_trusted() {
        let validator = default_validator();

        // In trusted mode, unknown user functions are allowed but tracked
        let result = validator
            .validate("user.GetData()", SafetyLevel::Trusted)
            .unwrap();

        // Trusted mode allows non-whitelisted functions
        assert!(result.is_safe);
        assert_eq!(result.purity, PurityLevel::Unknown);
        assert!(result.unknown_functions.contains("user.GetData"));
    }

    #[test]
    fn test_purity_no_functions_is_pure() {
        let validator = default_validator();

        // Expression with no function calls should be Pure
        let result = validator
            .validate("user.Name", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert_eq!(result.purity, PurityLevel::Pure);
        assert!(result.function_calls.is_empty());
        assert!(result.unknown_functions.is_empty());
    }

    #[test]
    fn test_validate_json_marshal_safe() {
        let validator = default_validator();

        // json.Marshal is safe (creates new bytes)
        let result = validator
            .validate("json.Marshal(data)", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_time_now_safe() {
        let validator = default_validator();

        // time.Now is safe (read-only)
        let result = validator
            .validate("time.Now()", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_errors_new_safe() {
        let validator = default_validator();

        // errors.New is safe (creates new error)
        let result = validator
            .validate("errors.New(\"error message\")", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }
}
