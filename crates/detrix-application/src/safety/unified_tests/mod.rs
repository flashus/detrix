//! Unified safety validator tests
//!
//! This module provides macro-based test generation for consistent
//! cross-language safety validator testing. Each language should have
//! the same set of tests with language-specific expressions.
//!
//! # Architecture
//!
//! - `LanguageTestData` trait: Defines test expressions for each language
//! - `validator_tests!` macro: Generates standard test suite for any validator
//! - Language modules: Implement `LanguageTestData` for Python, Go, Rust
//!
//! # Adding New Language Tests
//!
//! 1. Create a new impl of `LanguageTestData` for your language
//! 2. Use `validator_tests!` macro to generate tests
//! 3. All languages will have identical test coverage

mod data;

pub use data::*;

// Import validators for unified tests
use super::{GoValidator, PythonValidator};
use detrix_core::SourceLanguage;

/// Trait for language-specific test data
///
/// Each language implements this trait to provide equivalent test expressions.
/// This ensures all languages have the same test coverage.
pub trait LanguageTestData {
    /// Language
    const LANGUAGE: SourceLanguage;

    // =========================================================================
    // Basic validation expressions
    // =========================================================================

    /// Safe attribute access expression
    const SAFE_ATTRIBUTE: &'static str;

    /// Safe whitelisted function call
    const SAFE_FUNCTION: &'static str;

    /// Complex but safe expression
    const SAFE_COMPLEX: &'static str;

    // =========================================================================
    // Prohibited/blacklisted expressions
    // =========================================================================

    /// Eval-like execution (or closest equivalent, None if not applicable)
    const PROHIBITED_EVAL: Option<&'static str>;

    /// System call execution
    const PROHIBITED_SYSCALL: &'static str;

    /// File I/O operation
    const PROHIBITED_FILE_IO: &'static str;

    /// Network operation
    const PROHIBITED_NETWORK: Option<&'static str>;

    /// Process execution
    const PROHIBITED_EXEC: &'static str;

    // =========================================================================
    // Function classification
    // =========================================================================

    /// Known pure/whitelisted function
    const WHITELISTED_FUNC: &'static str;

    /// Known impure/blacklisted function (full name)
    const BLACKLISTED_FUNC: &'static str;

    /// Method call expression
    const METHOD_CALL: &'static str;

    /// Nested function calls
    const NESTED_CALLS: &'static str;

    // =========================================================================
    // Sensitive variable expressions (for blacklist testing)
    // =========================================================================

    /// Expression accessing password (should be blocked)
    const SENSITIVE_PASSWORD: &'static str;

    /// Expression accessing secret (should be blocked)
    const SENSITIVE_SECRET: &'static str;

    /// Expression accessing API key (should be blocked)
    const SENSITIVE_API_KEY: &'static str;

    /// Expression accessing token (should be blocked)
    const SENSITIVE_TOKEN: &'static str;

    /// Expression accessing credentials (should be blocked)
    const SENSITIVE_CREDENTIAL: &'static str;

    // =========================================================================
    // Purity classification expressions
    // =========================================================================

    /// Expression using only pure/whitelisted functions
    const PURE_EXPRESSION: &'static str;

    /// Expression with known impure function
    const IMPURE_EXPRESSION: &'static str;

    /// Expression with unknown function (in strict mode)
    const UNKNOWN_PURITY_STRICT: &'static str;

    /// Expression with unknown function (in trusted mode)
    const UNKNOWN_PURITY_TRUSTED: &'static str;

    /// Expression with only attribute access (no function calls)
    const ATTRIBUTE_ONLY: &'static str;

    /// Expression with safe dict/collection method
    const DICT_METHOD: &'static str;

    // =========================================================================
    // Feature flags - Language capabilities
    // =========================================================================

    /// Whether this language has eval-like dynamic execution
    const HAS_EVAL: bool;

    /// Whether this language has sensitive variable detection
    const HAS_SENSITIVE_VARS: bool;

    // =========================================================================
    // Implementation status - Used to skip tests for incomplete implementations
    // =========================================================================

    /// Whether safety validation is implemented for this language
    const HAS_SAFETY_VALIDATOR: bool = true;

    /// Whether LSP-based purity analysis is implemented
    const HAS_LSP_PURITY: bool = true;

    /// Whether scope-aware mutation detection is implemented
    const HAS_SCOPE_ANALYSIS: bool = true;

    /// Whether DAP adapter is implemented for this language
    const HAS_DAP_ADAPTER: bool = true;

    /// Whether expression introspection is implemented
    const HAS_INTROSPECTION: bool = true;
}

/// Generate standard test suite for a validator
///
/// This macro generates a comprehensive test suite that is consistent
/// across all language validators. It uses the `LanguageTestData` trait
/// to get language-specific expressions.
///
/// # Usage
///
/// ```ignore
/// validator_tests!(PythonValidator, PythonTestData);
/// validator_tests!(GoValidator, GoTestData);
/// ```
#[macro_export]
macro_rules! validator_tests {
    ($validator:ty, $test_data:ty) => {
        use super::*;
        use detrix_core::SafetyLevel;
        use $crate::safety::unified_tests::LanguageTestData;
        use $crate::safety::ExpressionValidator;

        fn create_validator() -> $validator {
            <$validator>::with_defaults()
        }

        // =====================================================================
        // Basic validation tests
        // =====================================================================

        #[test]
        fn test_safe_attribute_access() {
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::SAFE_ATTRIBUTE, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                result.is_safe,
                "Safe attribute access should be allowed: {}",
                <$test_data>::SAFE_ATTRIBUTE
            );
        }

        #[test]
        fn test_safe_whitelisted_function() {
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::SAFE_FUNCTION, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                result.is_safe,
                "Whitelisted function should be allowed: {}",
                <$test_data>::SAFE_FUNCTION
            );
        }

        #[test]
        fn test_safe_complex_expression() {
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::SAFE_COMPLEX, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                result.is_safe,
                "Complex safe expression should be allowed: {}",
                <$test_data>::SAFE_COMPLEX
            );
        }

        // =====================================================================
        // Prohibited expression tests
        // =====================================================================

        #[test]
        fn test_prohibited_syscall() {
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::PROHIBITED_SYSCALL, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "System call should be prohibited: {}",
                <$test_data>::PROHIBITED_SYSCALL
            );
            assert!(
                !result.errors.is_empty(),
                "Should have error message for prohibited syscall"
            );
        }

        #[test]
        fn test_prohibited_file_io() {
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::PROHIBITED_FILE_IO, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "File I/O should be prohibited: {}",
                <$test_data>::PROHIBITED_FILE_IO
            );
        }

        #[test]
        fn test_prohibited_exec() {
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::PROHIBITED_EXEC, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "Process execution should be prohibited: {}",
                <$test_data>::PROHIBITED_EXEC
            );
        }

        // =====================================================================
        // Conditional tests (based on language features)
        // =====================================================================

        #[test]
        fn test_prohibited_eval_if_supported() {
            if !<$test_data>::HAS_EVAL {
                // Language doesn't have eval - skip
                return;
            }

            let eval_expr = <$test_data>::PROHIBITED_EVAL
                .expect("HAS_EVAL is true but PROHIBITED_EVAL is None");

            let validator = create_validator();
            let result = validator
                .validate(eval_expr, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(!result.is_safe, "Eval should be prohibited: {}", eval_expr);
        }

        #[test]
        fn test_prohibited_network_if_supported() {
            let Some(network_expr) = <$test_data>::PROHIBITED_NETWORK else {
                // Language test data doesn't have network test - skip
                return;
            };

            let validator = create_validator();
            let result = validator
                .validate(network_expr, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "Network operation should be prohibited: {}",
                network_expr
            );
        }

        // =====================================================================
        // Function classification tests
        // =====================================================================

        #[test]
        fn test_is_function_allowed_whitelisted() {
            let validator = create_validator();

            assert!(
                validator.is_function_allowed(<$test_data>::WHITELISTED_FUNC, SafetyLevel::Strict),
                "Whitelisted function should be allowed: {}",
                <$test_data>::WHITELISTED_FUNC
            );
        }

        #[test]
        fn test_is_function_allowed_blacklisted() {
            let validator = create_validator();

            assert!(
                !validator.is_function_allowed(<$test_data>::BLACKLISTED_FUNC, SafetyLevel::Strict),
                "Blacklisted function should be prohibited: {}",
                <$test_data>::BLACKLISTED_FUNC
            );
        }

        #[test]
        fn test_method_call() {
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::METHOD_CALL, SafetyLevel::Trusted)
                .expect("Validation should succeed");

            // Method calls should be allowed in Trusted mode
            assert!(
                result.is_safe,
                "Method call should be allowed in Trusted mode: {}",
                <$test_data>::METHOD_CALL
            );
        }

        #[test]
        fn test_nested_function_calls() {
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::NESTED_CALLS, SafetyLevel::Strict)
                .expect("Validation should succeed");

            // Nested calls of whitelisted functions should be safe
            assert!(
                result.is_safe,
                "Nested whitelisted function calls should be allowed: {}",
                <$test_data>::NESTED_CALLS
            );
        }

        // =====================================================================
        // Safety level tests
        // =====================================================================

        #[test]
        fn test_strict_mode_blocks_unknown() {
            let validator = create_validator();
            // Custom function not in whitelist
            let result = validator
                .validate("custom_unknown_func(x)", SafetyLevel::Strict)
                .expect("Validation should succeed");

            // In Strict mode, unknown functions should be blocked
            assert!(
                !result.is_safe,
                "Unknown function should be blocked in Strict mode"
            );
        }

        #[test]
        fn test_trusted_mode_allows_unknown() {
            let validator = create_validator();
            // Custom function not in whitelist
            let result = validator
                .validate("custom_unknown_func(x)", SafetyLevel::Trusted)
                .expect("Validation should succeed");

            // In Trusted mode, unknown functions should be allowed
            assert!(
                result.is_safe,
                "Unknown function should be allowed in Trusted mode"
            );
        }

        #[test]
        fn test_blacklist_enforced_in_all_modes() {
            let validator = create_validator();

            // Blacklisted function should be blocked in all modes
            for level in [SafetyLevel::Strict, SafetyLevel::Trusted] {
                let result = validator
                    .validate(<$test_data>::PROHIBITED_EXEC, level)
                    .expect("Validation should succeed");

                assert!(
                    !result.is_safe,
                    "Blacklisted expression should be blocked in {:?} mode: {}",
                    level,
                    <$test_data>::PROHIBITED_EXEC
                );
            }
        }

        // =====================================================================
        // Language identifier test
        // =====================================================================

        #[test]
        fn test_language_identifier() {
            let validator = create_validator();
            assert_eq!(
                validator.language(),
                <$test_data>::LANGUAGE,
                "Language identifier should match"
            );
        }

        // =====================================================================
        // Sensitive variable tests (if language supports it)
        // =====================================================================

        #[test]
        fn test_sensitive_password_blocked() {
            if !<$test_data>::HAS_SENSITIVE_VARS {
                return;
            }

            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::SENSITIVE_PASSWORD, SafetyLevel::Trusted)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "Password access should be blocked: {}",
                <$test_data>::SENSITIVE_PASSWORD
            );
        }

        #[test]
        fn test_sensitive_secret_blocked() {
            if !<$test_data>::HAS_SENSITIVE_VARS {
                return;
            }

            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::SENSITIVE_SECRET, SafetyLevel::Trusted)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "Secret access should be blocked: {}",
                <$test_data>::SENSITIVE_SECRET
            );
        }

        #[test]
        fn test_sensitive_api_key_blocked() {
            if !<$test_data>::HAS_SENSITIVE_VARS {
                return;
            }

            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::SENSITIVE_API_KEY, SafetyLevel::Trusted)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "API key access should be blocked: {}",
                <$test_data>::SENSITIVE_API_KEY
            );
        }

        #[test]
        fn test_sensitive_token_blocked() {
            if !<$test_data>::HAS_SENSITIVE_VARS {
                return;
            }

            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::SENSITIVE_TOKEN, SafetyLevel::Trusted)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "Token access should be blocked: {}",
                <$test_data>::SENSITIVE_TOKEN
            );
        }

        #[test]
        fn test_sensitive_credential_blocked() {
            if !<$test_data>::HAS_SENSITIVE_VARS {
                return;
            }

            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::SENSITIVE_CREDENTIAL, SafetyLevel::Trusted)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "Credential access should be blocked: {}",
                <$test_data>::SENSITIVE_CREDENTIAL
            );
        }

        // =====================================================================
        // Purity classification tests
        // =====================================================================

        #[test]
        fn test_purity_pure_expression() {
            use detrix_core::PurityLevel;
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::PURE_EXPRESSION, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                result.is_safe,
                "Pure expression should be safe: {}",
                <$test_data>::PURE_EXPRESSION
            );
            assert_eq!(
                result.purity,
                PurityLevel::Pure,
                "Pure expression should have Pure purity level"
            );
        }

        #[test]
        fn test_purity_impure_expression() {
            use detrix_core::PurityLevel;
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::IMPURE_EXPRESSION, SafetyLevel::Trusted)
                .expect("Validation should succeed");

            assert!(
                !result.is_safe,
                "Impure expression should not be safe: {}",
                <$test_data>::IMPURE_EXPRESSION
            );
            assert_eq!(
                result.purity,
                PurityLevel::Impure,
                "Impure expression should have Impure purity level"
            );
        }

        #[test]
        fn test_purity_unknown_strict() {
            use detrix_core::PurityLevel;
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::UNKNOWN_PURITY_STRICT, SafetyLevel::Strict)
                .expect("Validation should succeed");

            // In strict mode, unknown functions are not safe
            assert!(
                !result.is_safe,
                "Unknown function in strict mode should not be safe: {}",
                <$test_data>::UNKNOWN_PURITY_STRICT
            );
            assert_eq!(
                result.purity,
                PurityLevel::Unknown,
                "Unknown function should have Unknown purity level"
            );
        }

        #[test]
        fn test_purity_unknown_trusted() {
            use detrix_core::PurityLevel;
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::UNKNOWN_PURITY_TRUSTED, SafetyLevel::Trusted)
                .expect("Validation should succeed");

            // In trusted mode, unknown functions are allowed but purity is still unknown
            assert!(
                result.is_safe,
                "Unknown function in trusted mode should be safe: {}",
                <$test_data>::UNKNOWN_PURITY_TRUSTED
            );
            assert_eq!(
                result.purity,
                PurityLevel::Unknown,
                "Unknown function should have Unknown purity level even in trusted mode"
            );
        }

        #[test]
        fn test_purity_attribute_only_is_pure() {
            use detrix_core::PurityLevel;
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::ATTRIBUTE_ONLY, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                result.is_safe,
                "Attribute-only expression should be safe: {}",
                <$test_data>::ATTRIBUTE_ONLY
            );
            assert_eq!(
                result.purity,
                PurityLevel::Pure,
                "Attribute-only expression should have Pure purity level"
            );
        }

        #[test]
        fn test_purity_dict_method_is_pure() {
            use detrix_core::PurityLevel;
            let validator = create_validator();
            let result = validator
                .validate(<$test_data>::DICT_METHOD, SafetyLevel::Strict)
                .expect("Validation should succeed");

            assert!(
                result.is_safe,
                "Dict method expression should be safe: {}",
                <$test_data>::DICT_METHOD
            );
            assert_eq!(
                result.purity,
                PurityLevel::Pure,
                "Dict method expression should have Pure purity level"
            );
        }
    };
}

// Re-export the macro
pub use validator_tests;

// ============================================================================
// Unified test modules - generate consistent tests for all validators
// ============================================================================

#[cfg(test)]
mod go_unified;
#[cfg(test)]
mod python_unified;
#[cfg(test)]
mod rust_unified;
