//! Shared data for language expression validators.

use std::collections::HashSet;

/// Field data shared by all language validators.
///
/// Holds the three configuration-derived collections used by every validator:
/// allowed functions (whitelist), prohibited functions (blacklist), and
/// sensitive variable patterns. Each language validator stores this as a
/// single `data` field instead of repeating the same three fields.
#[derive(Debug, Clone)]
pub(super) struct ValidatorData {
    /// Functions allowed in strict mode (whitelist)
    pub(super) allowed_functions: HashSet<String>,

    /// Functions always prohibited (blacklist)
    pub(super) prohibited_functions: HashSet<String>,

    /// Sensitive variable patterns to block
    pub(super) sensitive_patterns: Vec<String>,
}

impl ValidatorData {
    pub(super) fn new(
        allowed_functions: HashSet<String>,
        prohibited_functions: HashSet<String>,
        sensitive_patterns: Vec<String>,
    ) -> Self {
        Self {
            allowed_functions,
            prohibited_functions,
            sensitive_patterns,
        }
    }
}
