//! Safety validation for expressions
//!
//! This module provides language-specific expression safety validation before
//! sending expressions to DAP adapters. Each language has its own validator
//! implementation using in-process tree-sitter parsing.
//!
//! # Architecture
//!
//! - `ExpressionValidator` trait defines the interface for all validators
//! - `ValidatorRegistry` manages validators and dispatches by language
//! - Language-specific validators (e.g., `PythonValidator`) implement the trait
//! - `treesitter` module provides in-process AST parsing for all languages
//!
//! # Adding New Language Support
//!
//! 1. Create a new module (e.g., `go.rs`)
//! 2. Implement `ExpressionValidator` for your validator
//! 3. Add language config section to `SafetyConfig`
//! 4. Register validator in `ValidatorRegistry::new()`
//! 5. Add tree-sitter analyzer in `treesitter/{lang}.rs`

mod base_validator;
mod go;
mod python;
mod rust;
pub mod treesitter;
#[cfg(test)]
pub mod unified_tests;
mod validation_result;

pub use base_validator::{BaseValidator, FunctionClassification};
pub use detrix_core::PurityLevel;
pub use go::GoValidator;
pub use python::PythonValidator;
pub use rust::RustValidator;
pub use treesitter::TreeSitterResult;
pub use validation_result::ValidationResult;

use crate::error::{Result, SafetyError};
use detrix_config::SafetyConfig;
use detrix_core::{SafetyLevel, SourceLanguage};
use std::collections::HashMap;
use std::sync::Arc;

/// Language-specific expression validator trait
///
/// Implement this trait to add safety validation for a new language.
pub trait ExpressionValidator: Send + Sync {
    /// Language this validator handles
    fn language(&self) -> SourceLanguage;

    /// Validate an expression for safety violations
    ///
    /// # Arguments
    /// * `expression` - The expression to validate
    /// * `safety_level` - The safety level to apply (Strict or Trusted)
    ///
    /// # Returns
    /// * `Ok(ValidationResult)` - Validation completed (check `is_safe` field)
    /// * `Err(Error)` - Validation failed to run (e.g., subprocess error)
    fn validate(&self, expression: &str, safety_level: SafetyLevel) -> Result<ValidationResult>;

    /// Check if a specific function call is allowed
    fn is_function_allowed(&self, func_name: &str, level: SafetyLevel) -> bool;
}

/// Registry of language-specific validators
///
/// Manages validators and dispatches validation requests to the appropriate
/// validator based on the expression's language.
#[derive(Clone)]
pub struct ValidatorRegistry {
    validators: HashMap<SourceLanguage, Arc<dyn ExpressionValidator>>,
}

impl std::fmt::Debug for ValidatorRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatorRegistry")
            .field("languages", &self.validators.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ValidatorRegistry {
    /// Create a new registry with default validators
    ///
    /// Validators use in-process tree-sitter parsing for AST analysis.
    pub fn new(config: &SafetyConfig) -> Self {
        let mut validators: HashMap<SourceLanguage, Arc<dyn ExpressionValidator>> = HashMap::new();

        // Register Python validator
        let python_validator = PythonValidator::new(&config.python);
        validators.insert(SourceLanguage::Python, Arc::new(python_validator));

        // Register Go validator
        let go_validator = GoValidator::new(&config.go);
        validators.insert(SourceLanguage::Go, Arc::new(go_validator));

        // Register Rust validator
        let rust_validator = RustValidator::new(&config.rust);
        validators.insert(SourceLanguage::Rust, Arc::new(rust_validator));

        Self { validators }
    }

    /// Create an empty registry (for testing)
    pub fn empty() -> Self {
        Self {
            validators: HashMap::new(),
        }
    }

    /// Register a custom validator
    pub fn register(&mut self, validator: Arc<dyn ExpressionValidator>) {
        self.validators.insert(validator.language(), validator);
    }

    /// Get validator for a language
    pub fn get(&self, language: SourceLanguage) -> Option<&Arc<dyn ExpressionValidator>> {
        self.validators.get(&language)
    }

    /// Validate expression for a specific language
    ///
    /// # Arguments
    /// * `language` - The language of the expression (e.g., "python")
    /// * `expression` - The expression to validate
    /// * `safety_level` - The safety level to apply
    ///
    /// # Returns
    /// * `Ok(ValidationResult)` - Validation result
    /// * `Err(Error::Safety)` - No validator for language or validation failed
    pub fn validate(
        &self,
        language: &str,
        expression: &str,
        safety_level: SafetyLevel,
    ) -> Result<ValidationResult> {
        let lang: SourceLanguage = language.try_into()?;
        let validator = self
            .validators
            .get(&lang)
            .ok_or(SafetyError::UnsupportedLanguage(lang))?;

        validator.validate(expression, safety_level)
    }

    /// Check if a function is allowed for a specific language
    pub fn is_function_allowed(
        &self,
        language: &str,
        func_name: &str,
        level: SafetyLevel,
    ) -> Result<bool> {
        let lang: SourceLanguage = language.try_into()?;
        let validator = self
            .validators
            .get(&lang)
            .ok_or(SafetyError::UnsupportedLanguage(lang))?;

        Ok(validator.is_function_allowed(func_name, level))
    }

    /// Get list of supported languages
    pub fn supported_languages(&self) -> Vec<&str> {
        self.validators.keys().map(|l| l.as_str()).collect()
    }

    /// Check if a language is supported
    pub fn supports_language(&self, language: &str) -> bool {
        // If parsing fails, the language is definitely not supported
        let lang: SourceLanguage = match language.parse() {
            Ok(l) => l,
            Err(_) => return false,
        };
        self.validators.contains_key(&lang)
    }
}

impl Default for ValidatorRegistry {
    fn default() -> Self {
        Self::new(&SafetyConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let config = SafetyConfig::default();
        let registry = ValidatorRegistry::new(&config);

        assert!(registry.supports_language("python"));
        assert!(registry.supports_language("go"));
        assert!(registry.supports_language("rust"));
    }

    #[test]
    fn test_registry_supported_languages() {
        let registry = ValidatorRegistry::default();
        let languages = registry.supported_languages();

        assert!(languages.contains(&"python"));
        assert!(languages.contains(&"go"));
        assert!(languages.contains(&"rust"));
    }

    #[test]
    fn test_registry_unsupported_language() {
        let registry = ValidatorRegistry::default();
        let result = registry.validate("unknown_lang", "x + 1", SafetyLevel::Strict);

        assert!(result.is_err());
        let err = result.unwrap_err();
        // Now fails at parsing stage with InvalidLanguage error, not at validator lookup
        assert!(
            err.to_string().contains("Invalid language"),
            "Expected 'Invalid language' error, got: {}",
            err
        );
    }

    #[test]
    fn test_registry_validate_python() {
        let registry = ValidatorRegistry::default();
        let result = registry
            .validate("python", "user.id", SafetyLevel::Strict)
            .unwrap();

        assert!(result.is_safe);
    }

    #[test]
    fn test_registry_validate_prohibited() {
        let registry = ValidatorRegistry::default();
        let result = registry
            .validate("python", "eval('1+1')", SafetyLevel::Strict)
            .unwrap();

        assert!(!result.is_safe);
        assert!(!result.errors.is_empty());
    }
}
