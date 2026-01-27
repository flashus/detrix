//! Application layer error types
//!
//! Wraps domain errors and can be extended with application-specific errors.
//!
//! ## Error Handling Philosophy
//!
//! All errors and warnings are propagated upstack to the presentation layer (CLI/API).
//! This allows:
//! - Centralized logging at the top layer
//! - Consistent error formatting
//! - CLI control over verbosity (--verbose, --quiet flags)
//! - Testable error handling (assert on returned errors, not logs)
//!
//! ## Typed Error Strategy
//!
//! We use a combination of:
//! - `#[from]` for automatic conversion when no context is needed
//! - Extension traits for adding context at call sites
//! - Sub-enums for logical groupings (ConfigError, SafetyError, etc.)

use detrix_core::{ErrorCode, MetricEvent, MetricId, SourceLanguage, SystemEvent};
use std::path::PathBuf;
use thiserror::Error;
use tokio::sync::broadcast::error::SendError;

// ============================================================================
// Sub-Error Types - Typed errors for specific domains
// ============================================================================

/// Configuration-related errors
#[derive(Debug, Error, Clone)]
pub enum ConfigError {
    /// TOML parsing failed
    #[error("Invalid TOML: {0}")]
    TomlParse(String),

    /// TOML serialization failed
    #[error("Failed to serialize TOML: {0}")]
    TomlSerialize(String),

    /// Failed to read config file
    #[error("Failed to read config from {path}: {message}")]
    Read { path: PathBuf, message: String },

    /// Failed to write config file
    #[error("Failed to write config to {path}: {message}")]
    Write { path: PathBuf, message: String },

    /// Field requires server restart
    #[error("Field '{field}' requires server restart")]
    RestartRequired { field: String },

    /// Invalid configuration value
    #[error("Invalid config value for '{field}': {message}")]
    InvalidValue { field: String, message: String },

    /// Environment validation failed (version checks, dependencies)
    #[error("{0}")]
    EnvironmentValidation(String),
}

impl ConfigError {
    /// Get the machine-readable error code for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            ConfigError::TomlParse(_) => ErrorCode::ConfigTomlParse,
            ConfigError::TomlSerialize(_) => ErrorCode::ConfigTomlSerialize,
            ConfigError::Read { .. } => ErrorCode::ConfigReadFailed,
            ConfigError::Write { .. } => ErrorCode::ConfigWriteFailed,
            ConfigError::RestartRequired { .. } => ErrorCode::ConfigRestartRequired,
            ConfigError::InvalidValue { .. } => ErrorCode::ConfigInvalidValue,
            ConfigError::EnvironmentValidation(_) => ErrorCode::ConfigEnvironmentValidation,
        }
    }
}

/// Safety validation errors
#[derive(Debug, Error, Clone)]
pub enum SafetyError {
    /// No validator available for language
    #[error("No safety validator for language: {0}")]
    UnsupportedLanguage(SourceLanguage),

    /// Failed to spawn analyzer process
    #[error("Failed to spawn {command}: {message}")]
    ProcessSpawn { command: String, message: String },

    /// Analyzer process timed out
    #[error("Analyzer '{command}' timed out after {timeout_ms}ms")]
    ProcessTimeout { command: String, timeout_ms: u64 },

    /// Expression validation failed
    #[error("Expression validation failed: {}", violations.join(", "))]
    ValidationFailed {
        expression: String,
        violations: Vec<String>,
    },

    /// Failed to parse analyzer output
    #[error("Failed to parse analyzer output: {0}")]
    OutputParse(String),

    /// Process execution error (wait/signal failure)
    #[error("Process execution error: {0}")]
    ProcessExecution(String),

    /// Hash verification failed
    #[error("Hash verification failed: {0}")]
    HashVerification(String),
}

impl SafetyError {
    /// Get the machine-readable error code for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            SafetyError::UnsupportedLanguage(_) => ErrorCode::SafetyUnsupportedLanguage,
            SafetyError::ProcessSpawn { .. } => ErrorCode::SafetyProcessSpawn,
            SafetyError::ProcessTimeout { .. } => ErrorCode::SafetyProcessTimeout,
            SafetyError::ValidationFailed { .. } => ErrorCode::SafetyViolation,
            SafetyError::OutputParse(_) => ErrorCode::SafetyOutputParse,
            SafetyError::ProcessExecution(_) => ErrorCode::SafetyProcessExecution,
            SafetyError::HashVerification(_) => ErrorCode::SafetyHashVerification,
        }
    }
}

/// File inspection errors
#[derive(Debug, Error, Clone)]
pub enum FileInspectionError {
    /// Failed to read file
    #[error("Failed to read '{path}': {message}")]
    Read { path: PathBuf, message: String },

    /// Failed to parse file
    #[error("Failed to parse '{path}': {message}")]
    Parse { path: PathBuf, message: String },

    /// Analyzer not found
    #[error("Analyzer not found: {analyzer}. Tried: {}", tried_paths.join(", "))]
    AnalyzerNotFound {
        analyzer: String,
        tried_paths: Vec<String>,
    },

    /// Failed to run analyzer command
    #[error("Failed to run {command}: {message}")]
    CommandFailed { command: String, message: String },

    /// Invalid path (too long, null bytes, etc.)
    #[error("{0}")]
    InvalidPath(String),

    /// File not found
    #[error("File not found: {0}")]
    NotFound(String),

    /// Path is not a file
    #[error("Not a file: {0}")]
    NotAFile(String),

    /// Line not found in file
    #[error("Line {line} not found in {path} (file has {total_lines} lines)")]
    LineNotFound {
        path: String,
        line: u32,
        total_lines: usize,
    },
}

impl FileInspectionError {
    /// Get the machine-readable error code for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            FileInspectionError::Read { .. } => ErrorCode::FileReadFailed,
            FileInspectionError::Parse { .. } => ErrorCode::FileParseFailed,
            FileInspectionError::AnalyzerNotFound { .. } => ErrorCode::AnalyzerNotFound,
            FileInspectionError::CommandFailed { .. } => ErrorCode::CommandFailed,
            FileInspectionError::InvalidPath(_) => ErrorCode::InvalidPath,
            FileInspectionError::NotFound(_) => ErrorCode::FileNotFound,
            FileInspectionError::NotAFile(_) => ErrorCode::NotAFile,
            FileInspectionError::LineNotFound { .. } => ErrorCode::LineNotFound,
        }
    }
}

/// Application layer error
#[derive(Debug, Error, Clone)]
pub enum Error {
    /// Domain layer error
    #[error(transparent)]
    Core(#[from] detrix_core::Error),

    /// Configuration error (typed)
    #[error(transparent)]
    ConfigTyped(#[from] ConfigError),

    /// Safety validation error (typed)
    #[error(transparent)]
    SafetyTyped(#[from] SafetyError),

    /// File inspection error (typed)
    #[error(transparent)]
    FileInspectionTyped(#[from] FileInspectionError),

    /// Storage/database error
    #[error("Storage error: {0}")]
    Storage(String),

    /// Connection not found
    #[error("Connection not found: {0}")]
    ConnectionNotFound(String),

    /// Output error (GELF, etc.)
    #[error("Output error: {0}")]
    Output(String),

    /// Partial failure in group operation
    #[error("Partial group operation failure: {succeeded} succeeded, {} failed", failures.len())]
    PartialGroupFailure {
        /// Number of metrics successfully modified
        succeeded: usize,
        /// Failed metrics: (metric_name, error_message)
        failures: Vec<(String, String)>,
    },

    /// Operation failed and rollback also failed
    #[error("{primary}. Additionally, rollback failed: {rollback}")]
    OperationWithRollbackFailure {
        /// The primary operation error
        primary: String,
        /// The rollback error
        rollback: String,
        /// Additional context
        context: String,
    },

    /// Subprocess timed out
    #[error("Subprocess '{command}' timed out after {timeout_ms}ms")]
    SubprocessTimeout {
        /// Command that was running
        command: String,
        /// Timeout in milliseconds
        timeout_ms: u64,
    },

    /// Authentication/authorization error
    #[error("Auth error: {0}")]
    Auth(String),
}

impl Error {
    /// Get the machine-readable error code for this error.
    ///
    /// For typed sub-errors, delegates to their code() method.
    /// For wrapped Core errors, delegates to detrix_core::Error::code().
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Core(err) => err.code(),
            Error::ConfigTyped(err) => err.code(),
            Error::SafetyTyped(err) => err.code(),
            Error::FileInspectionTyped(err) => err.code(),
            Error::Storage(_) => ErrorCode::StorageError,
            Error::ConnectionNotFound(_) => ErrorCode::ConnectionNotFound,
            Error::Output(_) => ErrorCode::OutputError,
            Error::PartialGroupFailure { .. } => ErrorCode::PartialGroupFailure,
            Error::OperationWithRollbackFailure { .. } => ErrorCode::RollbackFailure,
            Error::SubprocessTimeout { .. } => ErrorCode::SubprocessTimeout,
            Error::Auth(_) => ErrorCode::Unauthorized,
        }
    }

    /// Get the error code name (e.g., "METRIC_NOT_FOUND")
    pub fn code_name(&self) -> &'static str {
        self.code().name()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

// ============================================================================
// Extension Traits for Contextual Errors
// ============================================================================

/// Extension trait for adding config file context to IO errors
pub trait ConfigIoResultExt<T> {
    /// Add read context to an IO error
    fn config_read(self, path: impl Into<PathBuf>) -> Result<T>;
    /// Add write context to an IO error
    fn config_write(self, path: impl Into<PathBuf>) -> Result<T>;
}

/// Extension trait for config TOML operations with specific context
pub trait ConfigTomlResultExt<T> {
    /// Parse partial config TOML
    fn toml_parse_partial(self) -> Result<T>;
    /// Serialize current config to TOML
    fn toml_serialize_current(self) -> Result<T>;
    /// Parse current config from TOML
    fn toml_parse_current(self) -> Result<T>;
    /// Serialize config for file persistence
    fn toml_serialize_for_file(self) -> Result<T>;
}

impl<T> ConfigIoResultExt<T> for std::result::Result<T, std::io::Error> {
    fn config_read(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| {
            ConfigError::Read {
                path: path.into(),
                message: e.to_string(),
            }
            .into()
        })
    }

    fn config_write(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| {
            ConfigError::Write {
                path: path.into(),
                message: e.to_string(),
            }
            .into()
        })
    }
}

impl<T> ConfigTomlResultExt<T> for std::result::Result<T, toml::de::Error> {
    fn toml_parse_partial(self) -> Result<T> {
        self.map_err(|e| {
            ConfigError::TomlParse(format!("Invalid TOML in partial config: {}", e)).into()
        })
    }

    fn toml_parse_current(self) -> Result<T> {
        self.map_err(|e| {
            ConfigError::TomlParse(format!("Failed to parse current config: {}", e)).into()
        })
    }

    fn toml_serialize_current(self) -> Result<T> {
        // This is for de::Error, not used for serialization
        unreachable!("toml_serialize_current called on de::Error")
    }

    fn toml_serialize_for_file(self) -> Result<T> {
        // This is for de::Error, not used for serialization
        unreachable!("toml_serialize_for_file called on de::Error")
    }
}

impl<T> ConfigTomlResultExt<T> for std::result::Result<T, toml::ser::Error> {
    fn toml_parse_partial(self) -> Result<T> {
        // This is for ser::Error, not used for parsing
        unreachable!("toml_parse_partial called on ser::Error")
    }

    fn toml_parse_current(self) -> Result<T> {
        // This is for ser::Error, not used for parsing
        unreachable!("toml_parse_current called on ser::Error")
    }

    fn toml_serialize_current(self) -> Result<T> {
        self.map_err(|e| {
            ConfigError::TomlSerialize(format!("Failed to serialize current config: {}", e)).into()
        })
    }

    fn toml_serialize_for_file(self) -> Result<T> {
        self.map_err(|e| {
            ConfigError::TomlSerialize(format!("Failed to serialize config for file: {}", e)).into()
        })
    }
}

/// Extension trait for adding file context to IO errors
pub trait FileIoResultExt<T> {
    /// Add read context to an IO error
    fn file_read(self, path: impl Into<PathBuf>) -> Result<T>;
    /// Add parse context to a parsing error
    fn file_parse(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> FileIoResultExt<T> for std::result::Result<T, std::io::Error> {
    fn file_read(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| {
            FileInspectionError::Read {
                path: path.into(),
                message: e.to_string(),
            }
            .into()
        })
    }

    fn file_parse(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| {
            FileInspectionError::Parse {
                path: path.into(),
                message: e.to_string(),
            }
            .into()
        })
    }
}

impl<T> FileIoResultExt<T> for std::result::Result<T, serde_json::Error> {
    fn file_read(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| {
            FileInspectionError::Read {
                path: path.into(),
                message: e.to_string(),
            }
            .into()
        })
    }

    fn file_parse(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|e| {
            FileInspectionError::Parse {
                path: path.into(),
                message: e.to_string(),
            }
            .into()
        })
    }
}

/// Extension trait for adding command context to spawn errors
pub trait SpawnResultExt<T> {
    /// Add command context to a spawn error
    fn spawn_context(self, command: impl Into<String>) -> Result<T>;
}

impl<T> SpawnResultExt<T> for std::result::Result<T, std::io::Error> {
    fn spawn_context(self, command: impl Into<String>) -> Result<T> {
        self.map_err(|e| {
            SafetyError::ProcessSpawn {
                command: command.into(),
                message: e.to_string(),
            }
            .into()
        })
    }
}

/// Extension trait for environment validation errors
pub trait EnvValidationResultExt<T> {
    /// Add environment validation context to an IO error
    fn env_context(self, context: &str) -> Result<T>;
}

impl<T> EnvValidationResultExt<T> for std::result::Result<T, std::io::Error> {
    fn env_context(self, context: &str) -> Result<T> {
        self.map_err(|e| ConfigError::EnvironmentValidation(format!("{}: {}", context, e)).into())
    }
}

/// Extension trait for converting Option to environment validation error
pub trait EnvValidationOptionExt<T> {
    /// Convert None to environment validation error
    fn env_required(self, message: impl Into<String>) -> Result<T>;
}

impl<T> EnvValidationOptionExt<T> for Option<T> {
    fn env_required(self, message: impl Into<String>) -> Result<T> {
        self.ok_or_else(|| ConfigError::EnvironmentValidation(message.into()).into())
    }
}

// ============================================================================
// From trait implementations for automatic error conversion
// ============================================================================

/// Wrapper for std::io::Error with context (file path)
#[derive(Debug)]
pub struct IoErrorWithContext {
    pub error: std::io::Error,
    pub path: String,
}

impl From<IoErrorWithContext> for Error {
    fn from(err: IoErrorWithContext) -> Self {
        FileInspectionError::Read {
            path: PathBuf::from(&err.path),
            message: err.error.to_string(),
        }
        .into()
    }
}

/// Wrapper for serde_json::Error with context
#[derive(Debug)]
pub struct JsonParseErrorWithContext {
    pub error: serde_json::Error,
    pub context: String,
}

impl From<JsonParseErrorWithContext> for Error {
    fn from(err: JsonParseErrorWithContext) -> Self {
        FileInspectionError::Parse {
            path: PathBuf::from(&err.context),
            message: err.error.to_string(),
        }
        .into()
    }
}

/// Wrapper for subprocess/command execution errors
#[derive(Debug)]
pub struct CommandErrorWithContext {
    pub error: std::io::Error,
    pub command: String,
}

impl From<CommandErrorWithContext> for Error {
    fn from(err: CommandErrorWithContext) -> Self {
        FileInspectionError::CommandFailed {
            command: err.command,
            message: err.error.to_string(),
        }
        .into()
    }
}

/// Wrapper for safety-related JSON parsing errors
#[derive(Debug)]
pub struct SafetyJsonParseError {
    pub error: serde_json::Error,
    pub context: String,
}

impl From<SafetyJsonParseError> for Error {
    fn from(err: SafetyJsonParseError) -> Self {
        SafetyError::OutputParse(format!("{}: {}", err.context, err.error)).into()
    }
}

impl From<detrix_core::ParseLanguageError> for Error {
    fn from(err: detrix_core::ParseLanguageError) -> Self {
        Error::Core(detrix_core::Error::from(err))
    }
}

// TOML error conversions
impl From<toml::de::Error> for ConfigError {
    fn from(err: toml::de::Error) -> Self {
        ConfigError::TomlParse(err.to_string())
    }
}

impl From<toml::ser::Error> for ConfigError {
    fn from(err: toml::ser::Error) -> Self {
        ConfigError::TomlSerialize(err.to_string())
    }
}

// Direct conversions to main Error type (for clean ? operator usage)
impl From<toml::de::Error> for Error {
    fn from(err: toml::de::Error) -> Self {
        Error::ConfigTyped(ConfigError::from(err))
    }
}

impl From<toml::ser::Error> for Error {
    fn from(err: toml::ser::Error) -> Self {
        Error::ConfigTyped(ConfigError::from(err))
    }
}

// Broadcast channel error conversions (for clean ? operator usage in streaming_service)
impl From<SendError<MetricEvent>> for Error {
    fn from(e: SendError<MetricEvent>) -> Self {
        Error::Core(detrix_core::Error::Adapter(format!(
            "Failed to publish event: {}",
            e
        )))
    }
}

impl From<SendError<SystemEvent>> for Error {
    fn from(e: SendError<SystemEvent>) -> Self {
        Error::Core(detrix_core::Error::Adapter(format!(
            "Failed to publish system event: {}",
            e
        )))
    }
}

/// Result of a batch group operation (enable/disable)
///
/// This type allows callers to handle partial failures gracefully.
/// Use `ensure_complete()` to convert partial failures to errors.
#[derive(Debug, Clone)]
pub struct GroupOperationResult {
    /// Number of metrics successfully modified
    pub succeeded: usize,
    /// Metrics that failed: (metric_name, error_message)
    pub failed: Vec<(String, String)>,
}

impl GroupOperationResult {
    /// Create a new result with no failures
    pub fn success(count: usize) -> Self {
        Self {
            succeeded: count,
            failed: vec![],
        }
    }

    /// Returns true if all metrics were processed successfully
    pub fn is_complete(&self) -> bool {
        self.failed.is_empty()
    }

    /// Returns true if any metrics failed
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }

    /// Total number of metrics processed (succeeded + failed)
    pub fn total(&self) -> usize {
        self.succeeded + self.failed.len()
    }

    /// Convert to error if there were any failures.
    ///
    /// Use this when you want to treat partial failures as errors:
    /// ```ignore
    /// let result = service.enable_group("trading").await?;
    /// result.ensure_complete()?;  // Returns Err if any metrics failed
    /// ```
    pub fn ensure_complete(self) -> Result<usize> {
        if self.failed.is_empty() {
            Ok(self.succeeded)
        } else {
            Err(Error::PartialGroupFailure {
                succeeded: self.succeeded,
                failures: self.failed,
            })
        }
    }
}

// ============================================================================
// Operation Warnings - Non-fatal issues that should be reported to user
// ============================================================================

/// Non-fatal warnings that occur during operations.
///
/// These are propagated upstack to CLI/API layer for reporting.
/// Unlike errors, warnings don't stop the operation but should be shown to user.
#[derive(Debug, Clone, PartialEq)]
pub enum OperationWarning {
    /// Rollback failed after a primary error
    RollbackFailed {
        /// What we tried to rollback
        context: String,
        /// The rollback error message
        error: String,
    },

    /// Metric-specific rollback failure
    MetricRollbackFailed {
        /// Metric ID if known
        metric_id: Option<MetricId>,
        /// Metric name
        metric_name: String,
        /// The rollback error message
        error: String,
    },

    /// Safety validator not available for language
    NoSafetyValidator {
        /// The language without validator
        language: String,
        /// Metric name that wasn't validated
        metric_name: String,
    },

    /// Broadcast channel closed (no active subscribers)
    BroadcastChannelClosed {
        /// Description of what we tried to broadcast
        context: String,
    },

    /// Adapter stop failed during cleanup
    AdapterStopFailed {
        /// The error message
        error: String,
    },

    /// Adapter sync failed during metric operation (non-fatal)
    AdapterSyncFailed {
        /// Metric name that failed to sync
        metric_name: String,
        /// The error message
        error: String,
    },

    /// Breakpoint was not verified by the debugger (e.g., "breakpoint already exists")
    BreakpointNotVerified {
        /// Location where breakpoint wasn't verified
        location: String,
        /// Line number where the breakpoint was actually set
        line: u32,
        /// Optional message from debugger (e.g., "breakpoint already exists")
        message: Option<String>,
    },

    /// Existing metric was replaced at the same location (when replace=true)
    MetricReplaced {
        /// Name of the metric that was replaced
        replaced_name: String,
        /// ID of the metric that was replaced
        replaced_id: u64,
        /// Location where replacement occurred (file:line)
        location: String,
    },

    /// Failed to fetch data for comparison/reference (non-fatal)
    FetchFailed {
        /// What we tried to fetch
        context: String,
        /// The error message
        error: String,
    },

    /// JSON serialization failed for optional field
    SerializationFailed {
        /// Field name that failed to serialize
        field: String,
        /// The error message
        error: String,
    },

    /// JSON deserialization failed for optional field
    DeserializationFailed {
        /// Field name that failed to deserialize
        field: String,
        /// The error message
        error: String,
    },

    /// Anchor capture failed for a metric (non-fatal, metric still works)
    AnchorCaptureFailed {
        /// Name of the metric
        metric_name: String,
        /// Location where anchor capture failed
        location: String,
        /// The error message
        error: String,
    },
}

impl std::fmt::Display for OperationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RollbackFailed { context, error } => {
                write!(f, "Rollback failed for {}: {}", context, error)
            }
            Self::MetricRollbackFailed {
                metric_id,
                metric_name,
                error,
            } => {
                if let Some(id) = metric_id {
                    write!(
                        f,
                        "Rollback failed for metric '{}' (ID: {}): {}",
                        metric_name, id, error
                    )
                } else {
                    write!(f, "Rollback failed for metric '{}': {}", metric_name, error)
                }
            }
            Self::NoSafetyValidator {
                language,
                metric_name,
            } => {
                write!(
                    f,
                    "No safety validator for language '{}'. Metric '{}' expression not validated",
                    language, metric_name
                )
            }
            Self::BroadcastChannelClosed { context } => {
                write!(f, "Broadcast channel closed: {}", context)
            }
            Self::AdapterStopFailed { error } => {
                write!(f, "Adapter stop failed: {}", error)
            }
            Self::AdapterSyncFailed { metric_name, error } => {
                write!(
                    f,
                    "Adapter sync failed for metric '{}': {}",
                    metric_name, error
                )
            }
            Self::BreakpointNotVerified {
                location,
                line,
                message,
            } => {
                if let Some(msg) = message {
                    write!(
                        f,
                        "Breakpoint not verified at {}:{} - {}",
                        location, line, msg
                    )
                } else {
                    write!(f, "Breakpoint not verified at {}:{}", location, line)
                }
            }
            Self::MetricReplaced {
                replaced_name,
                replaced_id,
                location,
            } => {
                write!(
                    f,
                    "Replaced existing metric '{}' (ID: {}) at {}",
                    replaced_name, replaced_id, location
                )
            }
            Self::FetchFailed { context, error } => {
                write!(f, "Failed to fetch {}: {}", context, error)
            }
            Self::SerializationFailed { field, error } => {
                write!(f, "Failed to serialize '{}': {}", field, error)
            }
            Self::DeserializationFailed { field, error } => {
                write!(f, "Failed to deserialize '{}': {}", field, error)
            }
            Self::AnchorCaptureFailed {
                metric_name,
                location,
                error,
            } => {
                write!(
                    f,
                    "Anchor capture failed for metric '{}' at {}: {}",
                    metric_name, location, error
                )
            }
        }
    }
}

// ============================================================================
// Operation Outcome - Result with accumulated warnings
// ============================================================================

/// Result of an operation that can succeed with warnings.
///
/// Use this when an operation completes successfully but had non-fatal issues
/// that should be reported to the user.
///
/// # Example
///
/// ```ignore
/// let outcome = service.add_metric(metric).await?;
/// for warning in &outcome.warnings {
///     eprintln!("Warning: {}", warning);
/// }
/// let metric_id = outcome.value;
/// ```
#[derive(Debug, Clone)]
pub struct OperationOutcome<T> {
    /// The successful result value
    pub value: T,
    /// Warnings accumulated during the operation
    pub warnings: Vec<OperationWarning>,
}

impl<T> OperationOutcome<T> {
    /// Create outcome with no warnings
    pub fn ok(value: T) -> Self {
        Self {
            value,
            warnings: vec![],
        }
    }

    /// Create outcome with warnings
    pub fn with_warnings(value: T, warnings: Vec<OperationWarning>) -> Self {
        Self { value, warnings }
    }

    /// Add a warning to the outcome
    pub fn add_warning(&mut self, warning: OperationWarning) {
        self.warnings.push(warning);
    }

    /// Returns true if there are no warnings
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }

    /// Returns true if there are warnings
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// Map the value, preserving warnings
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> OperationOutcome<U> {
        OperationOutcome {
            value: f(self.value),
            warnings: self.warnings,
        }
    }

    /// Extract value, discarding warnings (use with caution)
    pub fn into_value(self) -> T {
        self.value
    }
}
