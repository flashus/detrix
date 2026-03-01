//! Shared data for language expression validators.

use std::collections::HashSet;

/// Field data shared by all language validators.
///
/// Holds the configuration-derived collections used by every validator:
/// allowed functions (whitelist), prohibited functions (blacklist),
/// sensitive variable patterns, and an optional variable whitelist.
/// Each language validator stores this as a single `data` field instead
/// of repeating the same fields.
#[derive(Debug, Clone)]
pub(super) struct ValidatorData {
    /// Functions allowed in strict mode (whitelist)
    pub(super) allowed_functions: HashSet<String>,

    /// Functions always prohibited (blacklist)
    pub(super) prohibited_functions: HashSet<String>,

    /// Sensitive variable patterns to block
    pub(super) sensitive_patterns: Vec<String>,

    /// Variable whitelist — when non-empty, only listed variable names may be observed.
    pub(super) variable_whitelist: Vec<String>,
}

impl ValidatorData {
    pub(super) fn new(
        allowed_functions: HashSet<String>,
        prohibited_functions: HashSet<String>,
        sensitive_patterns: Vec<String>,
        variable_whitelist: Vec<String>,
    ) -> Self {
        Self {
            allowed_functions,
            prohibited_functions,
            sensitive_patterns,
            variable_whitelist,
        }
    }
}
