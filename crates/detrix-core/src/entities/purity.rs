//! Purity analysis types for expression safety validation

use serde::{Deserialize, Serialize};

/// Purity level of an expression or function
///
/// Determines whether an expression has side effects when evaluated.
/// Used by the safety system to classify expressions before allowing execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PurityLevel {
    /// Expression is pure - no side effects, safe to execute
    #[default]
    Pure,
    /// Purity unknown - contains user-defined functions not yet analyzed
    /// Will be blocked in strict mode until LSP analysis is available
    Unknown,
    /// Expression has side effects (I/O, network, state mutation)
    /// Always blocked unless explicitly whitelisted
    Impure,
}

impl PurityLevel {
    /// Returns true if the expression is safe to execute
    pub fn is_safe(&self) -> bool {
        matches!(self, PurityLevel::Pure)
    }

    /// Returns true if the expression needs LSP analysis
    pub fn needs_analysis(&self) -> bool {
        matches!(self, PurityLevel::Unknown)
    }

    /// Returns true if the expression should be blocked
    pub fn is_blocked(&self) -> bool {
        matches!(self, PurityLevel::Impure)
    }

    /// Combine two purity levels, returning the "worst" (most impure)
    pub fn combine(self, other: PurityLevel) -> PurityLevel {
        match (self, other) {
            (PurityLevel::Impure, _) | (_, PurityLevel::Impure) => PurityLevel::Impure,
            (PurityLevel::Unknown, _) | (_, PurityLevel::Unknown) => PurityLevel::Unknown,
            (PurityLevel::Pure, PurityLevel::Pure) => PurityLevel::Pure,
        }
    }
}

/// Detailed purity analysis result
///
/// Contains information about the purity level of an expression,
/// including any impure or unknown function calls detected.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PurityAnalysis {
    /// Overall purity level (worst of all calls)
    pub level: PurityLevel,

    /// Impure function calls found (with reasons)
    pub impure_calls: Vec<ImpureCall>,

    /// Function names that couldn't be analyzed
    pub unknown_calls: Vec<String>,

    /// Whether max call depth was reached during analysis
    pub max_depth_reached: bool,
}

impl PurityAnalysis {
    /// Create a pure analysis result
    pub fn pure() -> Self {
        Self {
            level: PurityLevel::Pure,
            impure_calls: Vec::new(),
            unknown_calls: Vec::new(),
            max_depth_reached: false,
        }
    }

    /// Create an unknown analysis result
    pub fn unknown(calls: Vec<String>) -> Self {
        Self {
            level: PurityLevel::Unknown,
            impure_calls: Vec::new(),
            unknown_calls: calls,
            max_depth_reached: false,
        }
    }

    /// Create an impure analysis result
    pub fn impure(calls: Vec<ImpureCall>) -> Self {
        Self {
            level: PurityLevel::Impure,
            impure_calls: calls,
            unknown_calls: Vec::new(),
            max_depth_reached: false,
        }
    }

    /// Add an impure call and update the level
    pub fn add_impure_call(&mut self, call: ImpureCall) {
        self.impure_calls.push(call);
        self.level = PurityLevel::Impure;
    }

    /// Add an unknown call and update the level (if not already impure)
    pub fn add_unknown_call(&mut self, name: String) {
        self.unknown_calls.push(name);
        if self.level != PurityLevel::Impure {
            self.level = PurityLevel::Unknown;
        }
    }

    /// Combine with another analysis, taking the worst purity level
    pub fn combine(mut self, other: PurityAnalysis) -> Self {
        self.level = self.level.combine(other.level);
        self.impure_calls.extend(other.impure_calls);
        self.unknown_calls.extend(other.unknown_calls);
        self.max_depth_reached = self.max_depth_reached || other.max_depth_reached;
        self
    }
}

/// Represents an impure function call with details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpureCall {
    /// Fully qualified function name
    pub function_name: String,

    /// Why it's considered impure
    pub reason: String,

    /// Location in call tree (e.g., "user.get_data -> db.query")
    pub call_path: String,
}

impl ImpureCall {
    /// Create a new impure call
    pub fn new(function_name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            function_name: function_name.into(),
            reason: reason.into(),
            call_path: String::new(),
        }
    }

    /// Create with a call path
    pub fn with_path(
        function_name: impl Into<String>,
        reason: impl Into<String>,
        call_path: impl Into<String>,
    ) -> Self {
        Self {
            function_name: function_name.into(),
            reason: reason.into(),
            call_path: call_path.into(),
        }
    }
}
