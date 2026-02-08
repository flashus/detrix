//! Core domain entities for Detrix
//!
//! This module contains the core domain model following DDD principles.
//! Each submodule groups related entities and value objects.
//!
//! # Modules
//!
//! - `location` - Location value object for source code positions
//! - `language` - Source language types and parsing utilities
//! - `purity` - Purity analysis types for expression safety
//! - `memory` - Stack trace and variable snapshot types
//! - `anchor` - Metric anchor system for tracking locations
//! - `metric` - Core metric entity and related types
//! - `event` - Metric event entity and group info

mod anchor;
mod event;
mod language;
mod location;
mod mcp_usage;
mod memory;
mod metric;
mod purity;

// Re-export all public types for backwards compatibility
pub use anchor::{AnchorStatus, MetricAnchor, RelocationResult};
pub use event::{ExpressionValue, GroupInfo, MetricEvent, TypedValue};
pub use language::{ParseLanguageError, ParseLanguageExt, ParseLanguageResultExt, SourceLanguage};
pub use location::Location;
pub use mcp_usage::{McpErrorCode, McpUsageEvent};
pub use memory::{
    CapturedStackTrace, CapturedVariable, MemorySnapshot, SnapshotScope, StackFrame,
    StackTraceSlice,
};
pub use metric::{
    Metric, MetricId, MetricMode, SafetyLevel, MAX_METRIC_NAME_LEN, MODE_FIRST, MODE_SAMPLE,
    MODE_SAMPLE_INTERVAL, MODE_STREAM, MODE_THROTTLE, MULTI_EXPR_DELIMITER,
    MULTI_EXPR_DELIMITER_STR, SAFETY_STRICT, SAFETY_TRUSTED,
};
pub use purity::{ImpureCall, PurityAnalysis, PurityLevel};

// Note: File inspection types (CodeLine, CodeContext, FileInspectionRequest,
// FileInspectionResult, FileOverview, LineInspectionResult, TextSearchMatch, VariableDefinition,
// VariableSearchResult, LanguageCapabilities) are in detrix-application crate.
