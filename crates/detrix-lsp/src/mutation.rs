//! Common mutation target types for purity analyzers
//!
//! This module provides a unified `MutationTarget` trait and language-specific
//! implementations for scope-aware mutation detection.

/// Classification of a mutation target
///
/// Each language implements specific variants for its scoping rules:
/// - Python: self, parameters, globals, nonlocals
/// - Go: receiver, parameters, package-level vars
/// - Rust: self, parameters, statics, unsafe blocks
pub trait MutationTarget: std::fmt::Debug + Clone + Copy + PartialEq + Eq {
    /// Check if this mutation target makes the function impure
    ///
    /// Local mutations are typically pure; everything else is impure.
    fn is_impure(&self) -> bool;

    /// Get a human-readable reason for impurity
    ///
    /// Returns None if the mutation is pure (local).
    fn impurity_reason(&self) -> Option<&'static str>;
}

/// Python-specific mutation targets
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonMutationTarget {
    /// Local variable - mutation is PURE
    Local,
    /// self.xxx - mutation is IMPURE (instance state)
    SelfAttribute,
    /// Parameter - mutation is IMPURE (caller's data)
    Parameter,
    /// Global variable - mutation is IMPURE
    Global,
    /// Nonlocal variable - mutation is IMPURE
    Nonlocal,
    /// Unknown - conservatively treat as IMPURE
    Unknown,
}

impl MutationTarget for PythonMutationTarget {
    fn is_impure(&self) -> bool {
        !matches!(self, PythonMutationTarget::Local)
    }

    fn impurity_reason(&self) -> Option<&'static str> {
        match self {
            PythonMutationTarget::Local => None,
            PythonMutationTarget::SelfAttribute => Some("Mutation of instance state (self.xxx)"),
            PythonMutationTarget::Parameter => Some("Mutation of function parameter"),
            PythonMutationTarget::Global => Some("Mutation of global variable"),
            PythonMutationTarget::Nonlocal => Some("Mutation of nonlocal variable"),
            PythonMutationTarget::Unknown => Some("Mutation of unknown scope variable"),
        }
    }
}

/// Go-specific mutation targets
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoMutationTarget {
    /// Local variable - mutation is PURE (only affects local state)
    Local,
    /// Method receiver (e.g., s.field = x) - mutation is IMPURE
    Receiver,
    /// Parameter - mutation is IMPURE (affects caller's data)
    Parameter,
    /// Package-level variable - mutation is IMPURE (global state)
    PackageVar,
    /// Unknown scope - conservatively treat as IMPURE
    Unknown,
}

impl MutationTarget for GoMutationTarget {
    fn is_impure(&self) -> bool {
        !matches!(self, GoMutationTarget::Local)
    }

    fn impurity_reason(&self) -> Option<&'static str> {
        match self {
            GoMutationTarget::Local => None,
            GoMutationTarget::Receiver => Some("Mutation of method receiver (struct state)"),
            GoMutationTarget::Parameter => Some("Mutation of function parameter"),
            GoMutationTarget::PackageVar => Some("Mutation of package-level variable"),
            GoMutationTarget::Unknown => Some("Mutation of unknown scope variable"),
        }
    }
}

/// Rust-specific mutation targets
///
/// Rust has unique ownership and borrowing semantics:
/// - `&mut self` methods can mutate instance state
/// - `&mut T` parameters allow mutation of borrowed data
/// - `static mut` is inherently unsafe and impure
/// - Mutations through raw pointers are unsafe
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustMutationTarget {
    /// Local variable (owned) - mutation is PURE
    /// Example: `let mut x = 1; x += 1;`
    Local,
    /// Mutable self reference (&mut self) - mutation is IMPURE
    /// Example: `self.field = value;`
    MutableSelf,
    /// Mutable parameter (&mut T) - mutation is IMPURE
    /// Example: `fn foo(items: &mut Vec<i32>)`
    MutableParameter,
    /// Static variable (static mut) - mutation is IMPURE
    /// Example: `static mut COUNTER: i32 = 0;`
    StaticVariable,
    /// Unsafe mutation (raw pointers, transmute) - mutation is IMPURE
    /// Example: `unsafe { *ptr = value; }`
    UnsafeMutation,
    /// Unknown scope - conservatively treat as IMPURE
    Unknown,
}

impl MutationTarget for RustMutationTarget {
    fn is_impure(&self) -> bool {
        !matches!(self, RustMutationTarget::Local)
    }

    fn impurity_reason(&self) -> Option<&'static str> {
        match self {
            RustMutationTarget::Local => None,
            RustMutationTarget::MutableSelf => Some("Mutation of &mut self (instance state)"),
            RustMutationTarget::MutableParameter => {
                Some("Mutation of &mut parameter (caller's data)")
            }
            RustMutationTarget::StaticVariable => {
                Some("Mutation of static variable (global state)")
            }
            RustMutationTarget::UnsafeMutation => {
                Some("Unsafe mutation (raw pointer or transmute)")
            }
            RustMutationTarget::Unknown => Some("Mutation of unknown scope variable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_mutation_target_impurity() {
        assert!(!PythonMutationTarget::Local.is_impure());
        assert!(PythonMutationTarget::SelfAttribute.is_impure());
        assert!(PythonMutationTarget::Parameter.is_impure());
        assert!(PythonMutationTarget::Global.is_impure());
        assert!(PythonMutationTarget::Nonlocal.is_impure());
        assert!(PythonMutationTarget::Unknown.is_impure());
    }

    #[test]
    fn test_python_mutation_target_reasons() {
        assert!(PythonMutationTarget::Local.impurity_reason().is_none());
        assert!(PythonMutationTarget::SelfAttribute
            .impurity_reason()
            .unwrap()
            .contains("instance state"));
        assert!(PythonMutationTarget::Parameter
            .impurity_reason()
            .unwrap()
            .contains("parameter"));
        assert!(PythonMutationTarget::Global
            .impurity_reason()
            .unwrap()
            .contains("global"));
        assert!(PythonMutationTarget::Nonlocal
            .impurity_reason()
            .unwrap()
            .contains("nonlocal"));
        assert!(PythonMutationTarget::Unknown
            .impurity_reason()
            .unwrap()
            .contains("unknown"));
    }

    #[test]
    fn test_go_mutation_target_impurity() {
        assert!(!GoMutationTarget::Local.is_impure());
        assert!(GoMutationTarget::Receiver.is_impure());
        assert!(GoMutationTarget::Parameter.is_impure());
        assert!(GoMutationTarget::PackageVar.is_impure());
        assert!(GoMutationTarget::Unknown.is_impure());
    }

    #[test]
    fn test_go_mutation_target_reasons() {
        assert!(GoMutationTarget::Local.impurity_reason().is_none());
        assert!(GoMutationTarget::Receiver
            .impurity_reason()
            .unwrap()
            .contains("receiver"));
        assert!(GoMutationTarget::Parameter
            .impurity_reason()
            .unwrap()
            .contains("parameter"));
        assert!(GoMutationTarget::PackageVar
            .impurity_reason()
            .unwrap()
            .contains("package-level"));
        assert!(GoMutationTarget::Unknown
            .impurity_reason()
            .unwrap()
            .contains("unknown"));
    }

    #[test]
    fn test_mutation_target_trait_polymorphism() {
        fn check_impurity<T: MutationTarget>(target: T) -> bool {
            target.is_impure()
        }

        // Python targets
        assert!(!check_impurity(PythonMutationTarget::Local));
        assert!(check_impurity(PythonMutationTarget::Parameter));

        // Go targets
        assert!(!check_impurity(GoMutationTarget::Local));
        assert!(check_impurity(GoMutationTarget::Parameter));

        // Rust targets
        assert!(!check_impurity(RustMutationTarget::Local));
        assert!(check_impurity(RustMutationTarget::MutableParameter));
    }

    #[test]
    fn test_rust_mutation_target_impurity() {
        assert!(!RustMutationTarget::Local.is_impure());
        assert!(RustMutationTarget::MutableSelf.is_impure());
        assert!(RustMutationTarget::MutableParameter.is_impure());
        assert!(RustMutationTarget::StaticVariable.is_impure());
        assert!(RustMutationTarget::UnsafeMutation.is_impure());
        assert!(RustMutationTarget::Unknown.is_impure());
    }

    #[test]
    fn test_rust_mutation_target_reasons() {
        assert!(RustMutationTarget::Local.impurity_reason().is_none());
        assert!(RustMutationTarget::MutableSelf
            .impurity_reason()
            .unwrap()
            .contains("&mut self"));
        assert!(RustMutationTarget::MutableParameter
            .impurity_reason()
            .unwrap()
            .contains("&mut parameter"));
        assert!(RustMutationTarget::StaticVariable
            .impurity_reason()
            .unwrap()
            .contains("static"));
        assert!(RustMutationTarget::UnsafeMutation
            .impurity_reason()
            .unwrap()
            .contains("Unsafe"));
        assert!(RustMutationTarget::Unknown
            .impurity_reason()
            .unwrap()
            .contains("unknown"));
    }
}
