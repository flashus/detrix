//! Validation result types for expression safety validation

use detrix_core::PurityLevel;
use serde::Serialize;
use std::collections::HashSet;

/// Result of expression safety validation
#[derive(Debug, Clone, Serialize)]
pub struct ValidationResult {
    /// Whether the expression is safe to execute
    pub is_safe: bool,

    /// Overall purity level of the expression
    pub purity: PurityLevel,

    /// List of safety violations found (empty if safe)
    pub errors: Vec<String>,

    /// Non-blocking warnings (expression still allowed)
    pub warnings: Vec<String>,

    /// Function calls detected in the expression
    pub function_calls: HashSet<String>,

    /// Functions with unknown purity (need LSP analysis)
    pub unknown_functions: HashSet<String>,

    /// Functions classified as impure (blocked)
    pub impure_functions: HashSet<String>,
}

impl ValidationResult {
    /// Create a safe result with no violations
    pub fn safe() -> Self {
        Self {
            is_safe: true,
            purity: PurityLevel::Pure,
            errors: Vec::new(),
            warnings: Vec::new(),
            function_calls: HashSet::new(),
            unknown_functions: HashSet::new(),
            impure_functions: HashSet::new(),
        }
    }

    /// Create a safe result with detected function calls
    pub fn safe_with_calls(function_calls: HashSet<String>) -> Self {
        Self {
            is_safe: true,
            purity: PurityLevel::Pure,
            errors: Vec::new(),
            warnings: Vec::new(),
            function_calls,
            unknown_functions: HashSet::new(),
            impure_functions: HashSet::new(),
        }
    }

    /// Create an unsafe result with violations
    pub fn unsafe_with_errors(errors: Vec<String>) -> Self {
        Self {
            is_safe: false,
            purity: PurityLevel::Impure,
            errors,
            warnings: Vec::new(),
            function_calls: HashSet::new(),
            unknown_functions: HashSet::new(),
            impure_functions: HashSet::new(),
        }
    }

    /// Create an unsafe result with violations and detected calls
    pub fn unsafe_with_details(
        errors: Vec<String>,
        warnings: Vec<String>,
        function_calls: HashSet<String>,
    ) -> Self {
        Self {
            is_safe: false,
            purity: PurityLevel::Impure,
            errors,
            warnings,
            function_calls,
            unknown_functions: HashSet::new(),
            impure_functions: HashSet::new(),
        }
    }

    /// Create a result with full purity details
    pub fn with_purity(
        is_safe: bool,
        purity: PurityLevel,
        errors: Vec<String>,
        warnings: Vec<String>,
        function_calls: HashSet<String>,
        unknown_functions: HashSet<String>,
        impure_functions: HashSet<String>,
    ) -> Self {
        Self {
            is_safe,
            purity,
            errors,
            warnings,
            function_calls,
            unknown_functions,
            impure_functions,
        }
    }

    /// Add an error and mark as unsafe
    pub fn add_error(&mut self, error: String) {
        self.errors.push(error);
        self.is_safe = false;
    }

    /// Add a warning (doesn't affect safety)
    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    /// Add a detected function call
    pub fn add_function_call(&mut self, func: String) {
        self.function_calls.insert(func);
    }

    /// Add an unknown function (needs LSP analysis)
    pub fn add_unknown_function(&mut self, func: String) {
        self.unknown_functions.insert(func);
        self.purity = self.purity.combine(PurityLevel::Unknown);
    }

    /// Add an impure function
    pub fn add_impure_function(&mut self, func: String) {
        self.impure_functions.insert(func);
        self.purity = self.purity.combine(PurityLevel::Impure);
    }

    /// Update purity level (combining with existing)
    pub fn set_purity(&mut self, purity: PurityLevel) {
        self.purity = self.purity.combine(purity);
    }
}

impl Default for ValidationResult {
    fn default() -> Self {
        Self::safe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_result() {
        let result = ValidationResult::safe();
        assert!(result.is_safe);
        assert!(result.errors.is_empty());
        assert_eq!(result.purity, PurityLevel::Pure);
    }

    #[test]
    fn test_unsafe_result() {
        let result = ValidationResult::unsafe_with_errors(vec!["eval() not allowed".to_string()]);
        assert!(!result.is_safe);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.purity, PurityLevel::Impure);
    }

    #[test]
    fn test_add_error() {
        let mut result = ValidationResult::safe();
        assert!(result.is_safe);

        result.add_error("violation".to_string());
        assert!(!result.is_safe);
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn test_add_warning() {
        let mut result = ValidationResult::safe();
        result.add_warning("deprecated function".to_string());

        assert!(result.is_safe); // Warnings don't affect safety
        assert_eq!(result.warnings.len(), 1);
    }

    #[test]
    fn test_add_unknown_function() {
        let mut result = ValidationResult::safe();
        assert_eq!(result.purity, PurityLevel::Pure);

        result.add_unknown_function("user.get_data".to_string());
        assert_eq!(result.purity, PurityLevel::Unknown);
        assert!(result.unknown_functions.contains("user.get_data"));
    }

    #[test]
    fn test_add_impure_function() {
        let mut result = ValidationResult::safe();
        result.add_impure_function("os.system".to_string());

        assert_eq!(result.purity, PurityLevel::Impure);
        assert!(result.impure_functions.contains("os.system"));
    }

    #[test]
    fn test_purity_combination() {
        // Pure + Unknown = Unknown
        let mut result = ValidationResult::safe();
        result.add_unknown_function("foo".to_string());
        assert_eq!(result.purity, PurityLevel::Unknown);

        // Unknown + Impure = Impure
        result.add_impure_function("bar".to_string());
        assert_eq!(result.purity, PurityLevel::Impure);
    }

    #[test]
    fn test_with_purity_constructor() {
        let mut funcs = HashSet::new();
        funcs.insert("len".to_string());
        funcs.insert("user.process".to_string());

        let mut unknown = HashSet::new();
        unknown.insert("user.process".to_string());

        let result = ValidationResult::with_purity(
            true,
            PurityLevel::Unknown,
            vec![],
            vec!["Unknown function: user.process".to_string()],
            funcs,
            unknown,
            HashSet::new(),
        );

        assert!(result.is_safe);
        assert_eq!(result.purity, PurityLevel::Unknown);
        assert!(result.unknown_functions.contains("user.process"));
        assert_eq!(result.warnings.len(), 1);
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// ValidationResult: add_error always makes is_safe=false
        #[test]
        fn proptest_add_error_makes_unsafe(error in "[a-zA-Z0-9 ]{1,50}") {
            let mut result = ValidationResult::safe();
            prop_assert!(result.is_safe);

            result.add_error(error);
            prop_assert!(!result.is_safe, "add_error should set is_safe=false");
        }

        /// ValidationResult: add_warning never changes is_safe
        #[test]
        fn proptest_add_warning_preserves_safety(
            warning in "[a-zA-Z0-9 ]{1,50}",
            initially_safe in proptest::bool::ANY
        ) {
            let mut result = if initially_safe {
                ValidationResult::safe()
            } else {
                ValidationResult::unsafe_with_errors(vec!["initial error".to_string()])
            };

            let was_safe = result.is_safe;
            result.add_warning(warning);

            prop_assert_eq!(
                result.is_safe,
                was_safe,
                "add_warning should not change is_safe"
            );
        }

        /// ValidationResult: purity only degrades (Pure -> Unknown -> Impure)
        #[test]
        fn proptest_purity_only_degrades(
            add_unknown in 0usize..3,
            add_impure in 0usize..3
        ) {
            let mut result = ValidationResult::safe();
            prop_assert_eq!(result.purity, PurityLevel::Pure);

            // Add unknown functions
            for i in 0..add_unknown {
                result.add_unknown_function(format!("unknown_{}", i));
            }

            // After adding unknowns, purity should be at least Unknown
            if add_unknown > 0 {
                prop_assert!(
                    result.purity == PurityLevel::Unknown || result.purity == PurityLevel::Impure,
                    "Purity should degrade to Unknown after adding unknown functions"
                );
            }

            let purity_before_impure = result.purity;

            // Add impure functions
            for i in 0..add_impure {
                result.add_impure_function(format!("impure_{}", i));
            }

            // After adding impure, purity should be Impure
            if add_impure > 0 {
                prop_assert_eq!(
                    result.purity,
                    PurityLevel::Impure,
                    "Purity should be Impure after adding impure functions"
                );
            } else {
                // Purity should not improve
                prop_assert!(
                    result.purity == purity_before_impure || result.purity == PurityLevel::Impure,
                    "Purity should not improve"
                );
            }
        }
    }
}
