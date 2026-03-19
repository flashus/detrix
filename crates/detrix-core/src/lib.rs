//! Detrix Core - Domain entities and business logic
//!
//! This crate contains the core domain model following DDD principles.
//! It has minimal dependencies and no infrastructure concerns.
//!
//! # Architecture
//!
//! - `entities` - Domain entities (Metric, MetricEvent, etc.)
//! - `error` - Domain error types
//! - `connection` - Connection entities and types
//! - `connection_identity` - Connection identity for stable UUID generation
//! - `system_event` - System events for real-time streaming
//!
//! # Related Crates
//!
//! - Config types: `detrix-config` crate
//! - Safety validation: `detrix-application` crate
//! - Port traits (DapAdapter, MetricRepository, etc.): `detrix-application::ports`
//!   per Clean Architecture (output ports belong in the Application layer)

pub mod connection;
pub mod connection_identity;
pub mod connection_reference;
pub mod entities;
pub mod error;
pub mod expressions;
pub mod formatting;
pub mod system_event;

pub use connection::{
    Connection, ConnectionId, ConnectionStatus, MIN_UNRESERVED_PORT, UNKNOWN_WORKSPACE_ROOT,
};
pub use connection_identity::ConnectionIdentity;
pub use connection_reference::{
    ClientIdentity, ConnectionReference, ReferenceKind, DAEMON_IDENTITY,
};
pub use entities::{
    // Tenant ID validation
    validate_tenant_id,
    AnchorStatus,
    CapturedStackTrace,
    CapturedVariable,
    ExpressionValue,
    GroupInfo,
    ImpureCall,
    Location,
    McpErrorCode,
    McpUsageEvent,
    MemorySnapshot,
    Metric,
    MetricAnchor,
    MetricEvent,
    MetricId,
    MetricMode,
    ParseLanguageError,
    ParseLanguageExt,
    ParseLanguageResultExt,
    PurityAnalysis,
    PurityLevel,
    RelocationResult,
    SafetyLevel,
    SnapshotScope,
    SourceLanguage,
    StackFrame,
    StackTraceSlice,
    TypedValue,
    // Metric/tenant ID length constants
    MAX_METRIC_NAME_LEN,
    MAX_USER_ID_LEN,
    // Mode type constants
    MODE_FIRST,
    MODE_SAMPLE,
    MODE_SAMPLE_INTERVAL,
    MODE_STREAM,
    MODE_THROTTLE,
    // Multi-expression delimiter
    MULTI_EXPR_DELIMITER,
    MULTI_EXPR_DELIMITER_STR,
    // Safety level constants
    SAFETY_STRICT,
    SAFETY_TRUSTED,
};
pub use error::{Error, ErrorCategory, ErrorCode, NotFoundError, Result};
pub use expressions::expression_contains_function_call;
pub use formatting::{
    format_timestamp_full, format_timestamp_micros, format_timestamp_short, format_timestamp_time,
    format_uptime, is_use_utc, set_use_utc, SECS_PER_DAY, SECS_PER_HOUR, SECS_PER_MINUTE,
};
pub use system_event::{SystemEvent, SystemEventType};

/// Default group name for ungrouped metrics.
pub const DEFAULT_GROUP_NAME: &str = "default";

/// Sentinel `user_id` substituted for `None` in storage to ensure uniqueness
/// constraint correctness (e.g. SQL `ON CONFLICT` treats NULLs as distinct).
///
/// Also rejected by [`validate_tenant_id`] to prevent API callers from
/// impersonating the "no owner" state.
pub const SYSTEM_USER_ID: &str = "__system__";
