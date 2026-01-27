//! Python validator unified tests
//!
//! These tests are generated from the same structure as Go/Rust to ensure
//! consistent test coverage across all language validators.

use super::*;
use crate::safety::unified_tests::LanguageTestData;
use crate::safety::ExpressionValidator;
use detrix_core::{PurityLevel, SafetyLevel};

fn create_validator() -> PythonValidator {
    PythonValidator::with_defaults()
}

type TestData = PythonTestData;

// =========================================================================
// Basic validation tests
// =========================================================================

#[test]
fn test_unified_safe_attribute_access() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::SAFE_ATTRIBUTE, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Safe attribute access should be allowed: {}",
        TestData::SAFE_ATTRIBUTE
    );
}

#[test]
fn test_unified_safe_whitelisted_function() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::SAFE_FUNCTION, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Whitelisted function should be allowed: {}",
        TestData::SAFE_FUNCTION
    );
}

#[test]
fn test_unified_safe_complex_expression() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::SAFE_COMPLEX, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Complex safe expression should be allowed: {}",
        TestData::SAFE_COMPLEX
    );
}

// =========================================================================
// Prohibited expression tests
// =========================================================================

#[test]
fn test_unified_prohibited_syscall() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::PROHIBITED_SYSCALL, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "System call should be prohibited: {}",
        TestData::PROHIBITED_SYSCALL
    );
}

#[test]
fn test_unified_prohibited_eval() {
    if !TestData::HAS_EVAL {
        return;
    }

    let eval_expr = TestData::PROHIBITED_EVAL.expect("HAS_EVAL is true");

    let validator = create_validator();
    let result = validator
        .validate(eval_expr, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(!result.is_safe, "Eval should be prohibited: {}", eval_expr);
}

#[test]
fn test_unified_prohibited_file_io() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::PROHIBITED_FILE_IO, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "File I/O should be prohibited: {}",
        TestData::PROHIBITED_FILE_IO
    );
}

#[test]
fn test_unified_prohibited_exec() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::PROHIBITED_EXEC, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "Process execution should be prohibited: {}",
        TestData::PROHIBITED_EXEC
    );
}

// =========================================================================
// Function classification tests
// =========================================================================

#[test]
fn test_unified_language_identifier() {
    let validator = create_validator();
    assert_eq!(validator.language(), TestData::LANGUAGE);
}

#[test]
fn test_unified_is_function_allowed_whitelisted() {
    let validator = create_validator();
    assert!(
        validator.is_function_allowed(TestData::WHITELISTED_FUNC, SafetyLevel::Strict),
        "Whitelisted function should be allowed: {}",
        TestData::WHITELISTED_FUNC
    );
}

#[test]
fn test_unified_is_function_allowed_blacklisted() {
    let validator = create_validator();
    assert!(
        !validator.is_function_allowed(TestData::BLACKLISTED_FUNC, SafetyLevel::Strict),
        "Blacklisted function should be prohibited: {}",
        TestData::BLACKLISTED_FUNC
    );
}

#[test]
fn test_unified_method_call_trusted() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::METHOD_CALL, SafetyLevel::Trusted)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Method call should be allowed in Trusted mode: {}",
        TestData::METHOD_CALL
    );
}

#[test]
fn test_unified_nested_function_calls() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::NESTED_CALLS, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Nested whitelisted function calls should be allowed: {}",
        TestData::NESTED_CALLS
    );
}

// =========================================================================
// Safety level tests
// =========================================================================

#[test]
fn test_unified_strict_blocks_unknown() {
    let validator = create_validator();
    let result = validator
        .validate("custom_unknown_func(x)", SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "Unknown function should be blocked in Strict mode"
    );
}

#[test]
fn test_unified_trusted_allows_unknown() {
    let validator = create_validator();
    let result = validator
        .validate("custom_unknown_func(x)", SafetyLevel::Trusted)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Unknown function should be allowed in Trusted mode"
    );
}

// =========================================================================
// Sensitive variable tests
// =========================================================================

#[test]
fn test_unified_sensitive_password_blocked() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::SENSITIVE_PASSWORD, SafetyLevel::Trusted)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "Password access should be blocked: {}",
        TestData::SENSITIVE_PASSWORD
    );
}

#[test]
fn test_unified_sensitive_secret_blocked() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::SENSITIVE_SECRET, SafetyLevel::Trusted)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "Secret access should be blocked: {}",
        TestData::SENSITIVE_SECRET
    );
}

#[test]
fn test_unified_sensitive_api_key_blocked() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::SENSITIVE_API_KEY, SafetyLevel::Trusted)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "API key access should be blocked: {}",
        TestData::SENSITIVE_API_KEY
    );
}

#[test]
fn test_unified_sensitive_token_blocked() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::SENSITIVE_TOKEN, SafetyLevel::Trusted)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "Token access should be blocked: {}",
        TestData::SENSITIVE_TOKEN
    );
}

#[test]
fn test_unified_sensitive_credential_blocked() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::SENSITIVE_CREDENTIAL, SafetyLevel::Trusted)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "Credential access should be blocked: {}",
        TestData::SENSITIVE_CREDENTIAL
    );
}

// =========================================================================
// Purity classification tests
// =========================================================================

#[test]
fn test_unified_purity_pure_expression() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::PURE_EXPRESSION, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Pure expression should be safe: {}",
        TestData::PURE_EXPRESSION
    );
    assert_eq!(
        result.purity,
        PurityLevel::Pure,
        "Pure expression should have Pure purity level"
    );
}

#[test]
fn test_unified_purity_impure_expression() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::IMPURE_EXPRESSION, SafetyLevel::Trusted)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "Impure expression should not be safe: {}",
        TestData::IMPURE_EXPRESSION
    );
    assert_eq!(
        result.purity,
        PurityLevel::Impure,
        "Impure expression should have Impure purity level"
    );
}

#[test]
fn test_unified_purity_unknown_strict() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::UNKNOWN_PURITY_STRICT, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        !result.is_safe,
        "Unknown function in strict mode should not be safe: {}",
        TestData::UNKNOWN_PURITY_STRICT
    );
    assert_eq!(
        result.purity,
        PurityLevel::Unknown,
        "Unknown function should have Unknown purity level"
    );
}

#[test]
fn test_unified_purity_unknown_trusted() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::UNKNOWN_PURITY_TRUSTED, SafetyLevel::Trusted)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Unknown function in trusted mode should be safe: {}",
        TestData::UNKNOWN_PURITY_TRUSTED
    );
    assert_eq!(
        result.purity,
        PurityLevel::Unknown,
        "Unknown function should have Unknown purity level even in trusted mode"
    );
}

#[test]
fn test_unified_purity_attribute_only() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::ATTRIBUTE_ONLY, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Attribute-only expression should be safe: {}",
        TestData::ATTRIBUTE_ONLY
    );
    assert_eq!(
        result.purity,
        PurityLevel::Pure,
        "Attribute-only expression should have Pure purity level"
    );
}

#[test]
fn test_unified_purity_dict_method() {
    let validator = create_validator();
    let result = validator
        .validate(TestData::DICT_METHOD, SafetyLevel::Strict)
        .expect("Validation should succeed");

    assert!(
        result.is_safe,
        "Dict method expression should be safe: {}",
        TestData::DICT_METHOD
    );
    assert_eq!(
        result.purity,
        PurityLevel::Pure,
        "Dict method expression should have Pure purity level"
    );
}
