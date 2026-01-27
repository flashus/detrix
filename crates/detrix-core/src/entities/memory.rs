//! Memory introspection types - stack traces and variable snapshots

use serde::{Deserialize, Serialize};

/// Stack trace slicing configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackTraceSlice {
    /// Capture full trace (overrides head/tail)
    #[serde(default)]
    pub full: bool,
    /// First N frames
    pub head: Option<u32>,
    /// Last N frames
    pub tail: Option<u32>,
}

impl Default for StackTraceSlice {
    fn default() -> Self {
        Self {
            full: false,
            head: Some(5), // Default: first 5 frames
            tail: None,
        }
    }
}

/// Memory snapshot scope
///
/// Defaults to `Local` - capturing local variables is the most common use case,
/// is less expensive than capturing all variables, and doesn't risk exposing
/// sensitive global state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotScope {
    /// Local variables only (default - most common, safest, least expensive)
    #[default]
    Local,
    /// Global variables only
    Global,
    /// Both local and global variables
    Both,
}

impl SnapshotScope {
    /// Return the string representation matching serde serialization
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Global => "global",
            Self::Both => "both",
        }
    }
}

/// A single stack frame in a captured stack trace
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackFrame {
    /// Frame index (0 = current frame, higher = caller frames)
    pub index: u32,
    /// Function/method name
    pub name: String,
    /// Source file path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Line number in source file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column number in source file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Module/package name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

/// Captured stack trace from a metric event
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturedStackTrace {
    /// Stack frames (index 0 = current location)
    pub frames: Vec<StackFrame>,
    /// Total number of frames (may be more than captured if slicing was used)
    pub total_frames: u32,
    /// Whether the trace was truncated due to slicing
    pub truncated: bool,
}

/// A captured variable in a memory snapshot
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturedVariable {
    /// Variable name
    pub name: String,
    /// Variable value as string representation
    pub value: String,
    /// Variable type (language-specific)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub var_type: Option<String>,
    /// Scope of the variable (local, global, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Captured memory snapshot (variables in scope)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemorySnapshot {
    /// Local variables at the capture point
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub locals: Vec<CapturedVariable>,
    /// Global variables (if captured)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub globals: Vec<CapturedVariable>,
    /// Scope of the snapshot that was requested
    pub scope: SnapshotScope,
}
