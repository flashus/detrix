//! Python expression safety validator
//!
//! Uses in-process tree-sitter parsing to validate Python expressions.
//! Detects prohibited constructs and classifies function calls by purity.

use super::base_validator::BaseValidator;
use super::treesitter::analyze_python;
use super::validation_result::ValidationResult;
use super::ExpressionValidator;
use crate::error::Result;
use detrix_config::PythonSafetyConfig;
use detrix_core::{PurityLevel, SafetyLevel, SourceLanguage};
use std::collections::HashSet; // Used in struct fields

/// Python expression validator
///
/// Validates Python expressions using in-process tree-sitter parsing.
/// Detects prohibited constructs and classifies function calls.
#[derive(Debug, Clone)]
pub struct PythonValidator {
    /// Functions allowed in strict mode (whitelist)
    allowed_functions: HashSet<String>,

    /// Functions always prohibited (blacklist)
    prohibited_functions: HashSet<String>,

    /// Sensitive variable patterns to block
    sensitive_patterns: Vec<String>,
}

impl PythonValidator {
    /// Create a new Python validator from config
    pub fn new(config: &PythonSafetyConfig) -> Self {
        Self {
            allowed_functions: config.allowed_set(),
            prohibited_functions: config.prohibited_set(),
            sensitive_patterns: config.sensitive_patterns().clone(),
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(&PythonSafetyConfig::default())
    }
}

impl BaseValidator for PythonValidator {
    fn allowed_functions(&self) -> &HashSet<String> {
        &self.allowed_functions
    }

    fn prohibited_functions(&self) -> &HashSet<String> {
        &self.prohibited_functions
    }

    fn sensitive_patterns(&self) -> &[String] {
        &self.sensitive_patterns
    }
}

impl ExpressionValidator for PythonValidator {
    fn language(&self) -> SourceLanguage {
        SourceLanguage::Python
    }

    fn validate(&self, expression: &str, safety_level: SafetyLevel) -> Result<ValidationResult> {
        let ast_result = analyze_python(expression, safety_level, &self.allowed_functions);
        Ok(self.validate_ast_result(ast_result, safety_level, PurityLevel::Pure))
    }

    fn is_function_allowed(&self, func_name: &str, level: SafetyLevel) -> bool {
        self.is_function_allowed_base(func_name, level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_validator() -> PythonValidator {
        PythonValidator::new(&PythonSafetyConfig::default())
    }

    #[test]
    fn test_python_validator_new() {
        let config = PythonSafetyConfig::default();
        let validator = PythonValidator::new(&config);

        assert!(validator.allowed_functions.contains("len"));
        assert!(validator.prohibited_functions.contains("eval"));
    }

    #[test]
    fn test_language() {
        let validator = default_validator();
        assert_eq!(validator.language(), SourceLanguage::Python);
    }

    #[test]
    fn test_is_function_allowed_strict() {
        let validator = default_validator();

        // Allowed function
        assert!(validator.is_function_allowed("len", SafetyLevel::Strict));

        // Not in whitelist
        assert!(!validator.is_function_allowed("custom_func", SafetyLevel::Strict));

        // Prohibited (always blocked)
        assert!(!validator.is_function_allowed("eval", SafetyLevel::Strict));
        assert!(!validator.is_function_allowed("exec", SafetyLevel::Strict));
    }

    #[test]
    fn test_is_function_allowed_trusted() {
        let validator = default_validator();

        // Custom function allowed in trusted mode
        assert!(validator.is_function_allowed("custom_func", SafetyLevel::Trusted));

        // But prohibited still blocked
        assert!(!validator.is_function_allowed("eval", SafetyLevel::Trusted));
    }

    #[test]
    fn test_validate_safe_expression() {
        let validator = default_validator();

        // Safe attribute access
        let result = validator.validate("user.id", SafetyLevel::Strict).unwrap();

        assert!(result.is_safe);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_validate_safe_function_call() {
        let validator = default_validator();

        // Safe function call (len is whitelisted)
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

        // eval is always prohibited
        let result = validator
            .validate("eval('1+1')", SafetyLevel::Strict)
            .unwrap();

        assert!(!result.is_safe);
        assert!(!result.errors.is_empty());
        assert!(result.function_calls.contains("eval"));
    }

    #[test]
    fn test_validate_exec_prohibited() {
        let validator = default_validator();

        // exec is always prohibited
        let result = validator
            .validate("exec('print(1)')", SafetyLevel::Strict)
            .unwrap();

        assert!(!result.is_safe);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_validate_compile_prohibited() {
        let validator = default_validator();

        // compile is always prohibited
        let result = validator
            .validate("compile('1+1', '<string>', 'eval')", SafetyLevel::Strict)
            .unwrap();

        assert!(!result.is_safe);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_validate_non_whitelisted_strict() {
        let validator = default_validator();

        // custom_func is not in whitelist
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
    }

    #[test]
    fn test_validate_complex_expression() {
        let validator = default_validator();

        // Complex but safe expression
        let result = validator
            .validate("sum([len(x) for x in items])", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert!(result.function_calls.contains("sum"));
        assert!(result.function_calls.contains("len"));
    }

    #[test]
    fn test_validate_method_call() {
        let validator = default_validator();

        // Method call (attribute access + call)
        let result = validator
            .validate("user.get_id()", SafetyLevel::Trusted)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_validate_multiple_function_calls() {
        let validator = default_validator();

        // Multiple allowed function calls
        let result = validator
            .validate("str(int(float(x)))", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
        assert!(result.function_calls.contains("str"));
        assert!(result.function_calls.contains("int"));
        assert!(result.function_calls.contains("float"));
    }

    #[test]
    fn test_validate_os_system_prohibited() {
        let validator = default_validator();

        // os.system is a dangerous system call - should be blocked
        let result = validator
            .validate("os.system('ls')", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result.errors.iter().any(|e| e.contains("os.system")
            || e.contains("prohibited")
            || e.contains("blacklist")));
    }

    #[test]
    fn test_validate_subprocess_run_prohibited() {
        let validator = default_validator();

        // subprocess.run is dangerous - should be blocked
        let result = validator
            .validate("subprocess.run(['ls'])", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result.errors.iter().any(|e| {
            e.contains("subprocess") || e.contains("prohibited") || e.contains("blacklist")
        }));
    }

    #[test]
    fn test_validate_open_prohibited() {
        let validator = default_validator();

        // open() for file I/O should be blocked
        let result = validator
            .validate("open('/etc/passwd', 'r')", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result
            .errors
            .iter()
            .any(|e| e.contains("open") && e.contains("blacklist")));
    }

    #[test]
    fn test_validate_dunder_import_prohibited() {
        let validator = default_validator();

        // __import__() is a dangerous function
        let result = validator
            .validate("__import__('os')", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(
            result
                .errors
                .iter()
                .any(|e| e.contains("__import__") || e.contains("blacklist")),
            "Expected __import__ to be blocked. Errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_validate_getattr_builtin_allowed() {
        let validator = default_validator();

        // getattr is a safe builtin for attribute access
        let result = validator
            .validate("getattr(obj, 'name')", SafetyLevel::Strict)
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

    // Variable blacklist tests

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
            .any(|w| w.contains("password") || w.contains("sensitive")));
    }

    #[test]
    fn test_validate_password_attribute_blocked() {
        let validator = default_validator();

        // Attribute access to password should be blocked
        let result = validator
            .validate("user.password", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("password") || w.contains("sensitive")));
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

    // Purity classification tests

    #[test]
    fn test_purity_pure_whitelisted_functions() {
        let validator = default_validator();

        // Expression with only whitelisted functions should be Pure
        let result = validator
            .validate("len(str(x))", SafetyLevel::Strict)
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
            .validate("eval('1+1')", SafetyLevel::Trusted)
            .unwrap();

        assert!(!result.is_safe);
        assert_eq!(result.purity, PurityLevel::Impure);
        assert!(result.impure_functions.contains("eval"));
    }

    #[test]
    fn test_purity_unknown_user_function_strict() {
        let validator = default_validator();

        // In strict mode, unknown user functions cause validation to fail
        let result = validator
            .validate("user.get_data()", SafetyLevel::Strict)
            .unwrap();

        // Strict mode blocks non-whitelisted functions
        assert!(!result.is_safe);
        assert_eq!(result.purity, PurityLevel::Unknown);
        assert!(result.unknown_functions.contains("user.get_data"));
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
        assert!(result.unknown_functions.contains("user.get_data"));
        // In trusted mode, we still warn about unknown purity
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("Unknown function") && w.contains("user.get_data")));
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
}
