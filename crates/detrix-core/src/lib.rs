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
pub mod entities;
pub mod error;
pub mod expressions;
pub mod formatting;
pub mod system_event;

pub use connection::{Connection, ConnectionId, ConnectionStatus, MIN_UNRESERVED_PORT};
pub use connection_identity::ConnectionIdentity;
pub use entities::{
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
    // Metric name constants
    MAX_METRIC_NAME_LEN,
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

// Port traits are in detrix-application crate:
// use detrix_application::{
//     DapAdapter, DapAdapterFactory, MetricRepository, EventRepository, ConnectionRepository
// };
