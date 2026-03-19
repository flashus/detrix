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
    Metric, MetricId, MetricMode, SafetyLevel, MAX_METRIC_NAME_LEN, MAX_USER_ID_LEN, MODE_FIRST,
    MODE_SAMPLE, MODE_SAMPLE_INTERVAL, MODE_STREAM, MODE_THROTTLE, MULTI_EXPR_DELIMITER,
    MULTI_EXPR_DELIMITER_STR, SAFETY_STRICT, SAFETY_TRUSTED,
};
pub use purity::{ImpureCall, PurityAnalysis, PurityLevel};

use crate::error::{Error, Result};

/// Validate that a single tenant-ID field is not empty, not reserved, and within length limit.
///
/// `field` is a display label used in the error message (e.g. `"user_id"`, `"agent_id"`).
///
/// Shared by [`Metric::validate_tenant_ids`] and [`Connection::validate_tenant_id`].
pub fn validate_tenant_id(field: &str, value: Option<&str>) -> Result<()> {
    if let Some(v) = value {
        if v.is_empty() {
            return Err(Error::InvalidTenantId(format!("{field} must not be empty")));
        }
        if v == crate::SYSTEM_USER_ID {
            return Err(Error::InvalidTenantId(format!(
                "{field} uses reserved value"
            )));
        }
        if v.len() > MAX_USER_ID_LEN {
            return Err(Error::InvalidTenantId(format!(
                "{} exceeds maximum length of {} characters",
                field, MAX_USER_ID_LEN
            )));
        }
    }
    Ok(())
}

// Note: File inspection types (CodeLine, CodeContext, FileInspectionRequest,
// FileInspectionResult, FileOverview, LineInspectionResult, TextSearchMatch, VariableDefinition,
// VariableSearchResult, LanguageCapabilities) are in detrix-application crate.
