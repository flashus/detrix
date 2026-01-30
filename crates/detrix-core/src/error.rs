//! Error types for Detrix core domain

use serde::Serialize;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

// ============================================================================
// Error Codes - Machine-readable codes for API consumers
// ============================================================================

/// Machine-readable error codes for API consumers.
///
/// Error code ranges:
/// - 1xxx: Metric errors
/// - 2xxx: Safety errors
/// - 3xxx: Config errors
/// - 4xxx: Connection errors
/// - 5xxx: Infrastructure errors
/// - 6xxx: Auth errors
/// - 9xxx: Generic errors
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(into = "u16")]
pub enum ErrorCode {
    // Metric errors (1xxx)
    /// Metric not found (1001)
    MetricNotFound = 1001,
    /// Invalid metric name (1002)
    MetricInvalidName = 1002,
    /// Invalid location format (1003)
    MetricInvalidLocation = 1003,
    /// Duplicate metric (1004)
    MetricDuplicate = 1004,
    /// Metric location conflict (1005)
    MetricLocationConflict = 1005,
    /// Invalid expression (1006)
    MetricInvalidExpression = 1006,
    /// Expression too long (1007)
    MetricExpressionTooLong = 1007,

    // Safety errors (2xxx)
    /// Safety violation detected (2001)
    SafetyViolation = 2001,
    /// AST analysis failed (2002)
    SafetyAstFailed = 2002,
    /// Generic safety error (2003)
    SafetyError = 2003,
    /// Unsupported language for validation (2004)
    SafetyUnsupportedLanguage = 2004,
    /// Process spawn failed (2005)
    SafetyProcessSpawn = 2005,
    /// Process timeout (2006)
    SafetyProcessTimeout = 2006,
    /// Output parse failed (2007)
    SafetyOutputParse = 2007,
    /// Process execution failed (2008)
    SafetyProcessExecution = 2008,
    /// Hash verification failed (2009)
    SafetyHashVerification = 2009,
    /// SafeMode violation - operation blocked (2010)
    SafeModeViolation = 2010,

    // Config errors (3xxx)
    /// Invalid configuration (3001)
    ConfigInvalid = 3001,
    /// TOML parse error (3002)
    ConfigTomlParse = 3002,
    /// TOML serialize error (3003)
    ConfigTomlSerialize = 3003,
    /// Config read error (3004)
    ConfigReadFailed = 3004,
    /// Config write error (3005)
    ConfigWriteFailed = 3005,
    /// Config requires restart (3006)
    ConfigRestartRequired = 3006,
    /// Invalid config value (3007)
    ConfigInvalidValue = 3007,
    /// Environment validation failed (3008)
    ConfigEnvironmentValidation = 3008,

    // Connection errors (4xxx)
    /// Not connected to debugger (4001)
    ConnectionNotConnected = 4001,
    /// Connection not found (4002)
    ConnectionNotFound = 4002,

    // File inspection errors (43xx)
    /// File read failed (4301)
    FileReadFailed = 4301,
    /// File parse failed (4302)
    FileParseFailed = 4302,
    /// Analyzer not found (4303)
    AnalyzerNotFound = 4303,
    /// Command failed (4304)
    CommandFailed = 4304,
    /// Invalid path (4305)
    InvalidPath = 4305,
    /// File not found (4306)
    FileNotFound = 4306,
    /// Not a file (4307)
    NotAFile = 4307,
    /// Line not found (4308)
    LineNotFound = 4308,

    // Infrastructure errors (5xxx)
    /// Database error (5001)
    DatabaseError = 5001,
    /// Adapter error (5002)
    AdapterError = 5002,
    /// I/O error (5003)
    IoError = 5003,
    /// Serialization error (5004)
    SerializationError = 5004,
    /// Output error (5005)
    OutputError = 5005,

    // Auth errors (6xxx)
    /// Unauthorized (6001)
    Unauthorized = 6001,

    // Generic (9xxx)
    /// Internal error (9001)
    InternalError = 9001,
    /// Storage error (9002)
    StorageError = 9002,
    /// Partial group failure (9003)
    PartialGroupFailure = 9003,
    /// Rollback failure (9004)
    RollbackFailure = 9004,
    /// Subprocess timeout (9005)
    SubprocessTimeout = 9005,
}

impl From<ErrorCode> for u16 {
    fn from(code: ErrorCode) -> Self {
        code as u16
    }
}

// ============================================================================
// Error Categories - Classification for retry logic
// ============================================================================

/// Error categorization for client retry handling.
///
/// Helps clients determine whether to retry failed operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorCategory {
    /// Temporary failure, safe to retry (connection timeout, busy DB)
    Retryable,
    /// Permanent failure, don't retry (invalid input, not found)
    Terminal,
    /// Security-related, may need re-authentication
    Security,
    /// Server-side issue, contact support
    Internal,
}

impl ErrorCategory {
    /// Get the category name as a string
    pub fn name(&self) -> &'static str {
        match self {
            ErrorCategory::Retryable => "retryable",
            ErrorCategory::Terminal => "terminal",
            ErrorCategory::Security => "security",
            ErrorCategory::Internal => "internal",
        }
    }

    /// Returns true if this error category indicates the operation can be retried
    pub fn is_retryable(&self) -> bool {
        matches!(self, ErrorCategory::Retryable)
    }
}

impl ErrorCode {
    /// Get the numeric value of the error code
    pub fn as_u16(self) -> u16 {
        self as u16
    }

    /// Get the category of this error code
    pub fn category(&self) -> ErrorCategory {
        match self {
            // Retryable - temporary failures
            ErrorCode::DatabaseError => ErrorCategory::Retryable,
            ErrorCode::AdapterError => ErrorCategory::Retryable,
            ErrorCode::IoError => ErrorCategory::Retryable,
            ErrorCode::ConnectionNotConnected => ErrorCategory::Retryable,
            ErrorCode::SubprocessTimeout => ErrorCategory::Retryable,

            // Terminal - permanent failures, don't retry
            ErrorCode::MetricNotFound => ErrorCategory::Terminal,
            ErrorCode::MetricInvalidName => ErrorCategory::Terminal,
            ErrorCode::MetricInvalidLocation => ErrorCategory::Terminal,
            ErrorCode::MetricDuplicate => ErrorCategory::Terminal,
            ErrorCode::MetricLocationConflict => ErrorCategory::Terminal,
            ErrorCode::MetricInvalidExpression => ErrorCategory::Terminal,
            ErrorCode::MetricExpressionTooLong => ErrorCategory::Terminal,
            ErrorCode::SafetyViolation => ErrorCategory::Terminal,
            ErrorCode::SafetyAstFailed => ErrorCategory::Terminal,
            ErrorCode::SafetyError => ErrorCategory::Terminal,
            ErrorCode::SafetyUnsupportedLanguage => ErrorCategory::Terminal,
            ErrorCode::SafeModeViolation => ErrorCategory::Terminal,
            ErrorCode::SafetyProcessSpawn => ErrorCategory::Terminal,
            ErrorCode::SafetyProcessTimeout => ErrorCategory::Terminal,
            ErrorCode::SafetyOutputParse => ErrorCategory::Terminal,
            ErrorCode::SafetyProcessExecution => ErrorCategory::Terminal,
            ErrorCode::SafetyHashVerification => ErrorCategory::Terminal,
            ErrorCode::ConfigInvalid => ErrorCategory::Terminal,
            ErrorCode::ConfigTomlParse => ErrorCategory::Terminal,
            ErrorCode::ConfigTomlSerialize => ErrorCategory::Terminal,
            ErrorCode::ConfigReadFailed => ErrorCategory::Terminal,
            ErrorCode::ConfigWriteFailed => ErrorCategory::Terminal,
            ErrorCode::ConfigRestartRequired => ErrorCategory::Terminal,
            ErrorCode::ConfigInvalidValue => ErrorCategory::Terminal,
            ErrorCode::ConfigEnvironmentValidation => ErrorCategory::Terminal,
            ErrorCode::ConnectionNotFound => ErrorCategory::Terminal,
            ErrorCode::FileReadFailed => ErrorCategory::Terminal,
            ErrorCode::FileParseFailed => ErrorCategory::Terminal,
            ErrorCode::AnalyzerNotFound => ErrorCategory::Terminal,
            ErrorCode::CommandFailed => ErrorCategory::Terminal,
            ErrorCode::InvalidPath => ErrorCategory::Terminal,
            ErrorCode::FileNotFound => ErrorCategory::Terminal,
            ErrorCode::NotAFile => ErrorCategory::Terminal,
            ErrorCode::LineNotFound => ErrorCategory::Terminal,

            // Security - authentication/authorization failures
            ErrorCode::Unauthorized => ErrorCategory::Security,

            // Internal - server-side issues
            ErrorCode::InternalError => ErrorCategory::Internal,
            ErrorCode::StorageError => ErrorCategory::Internal,
            ErrorCode::PartialGroupFailure => ErrorCategory::Internal,
            ErrorCode::RollbackFailure => ErrorCategory::Internal,
            ErrorCode::SerializationError => ErrorCategory::Internal,
            ErrorCode::OutputError => ErrorCategory::Internal,
        }
    }

    /// Get the error code name as a string
    pub fn name(&self) -> &'static str {
        match self {
            // Metric errors (1xxx)
            ErrorCode::MetricNotFound => "METRIC_NOT_FOUND",
            ErrorCode::MetricInvalidName => "METRIC_INVALID_NAME",
            ErrorCode::MetricInvalidLocation => "METRIC_INVALID_LOCATION",
            ErrorCode::MetricDuplicate => "METRIC_DUPLICATE",
            ErrorCode::MetricLocationConflict => "METRIC_LOCATION_CONFLICT",
            ErrorCode::MetricInvalidExpression => "METRIC_INVALID_EXPRESSION",
            ErrorCode::MetricExpressionTooLong => "METRIC_EXPRESSION_TOO_LONG",

            // Safety errors (2xxx)
            ErrorCode::SafetyViolation => "SAFETY_VIOLATION",
            ErrorCode::SafetyAstFailed => "SAFETY_AST_FAILED",
            ErrorCode::SafetyError => "SAFETY_ERROR",
            ErrorCode::SafetyUnsupportedLanguage => "SAFETY_UNSUPPORTED_LANGUAGE",
            ErrorCode::SafetyProcessSpawn => "SAFETY_PROCESS_SPAWN",
            ErrorCode::SafetyProcessTimeout => "SAFETY_PROCESS_TIMEOUT",
            ErrorCode::SafetyOutputParse => "SAFETY_OUTPUT_PARSE",
            ErrorCode::SafetyProcessExecution => "SAFETY_PROCESS_EXECUTION",
            ErrorCode::SafetyHashVerification => "SAFETY_HASH_VERIFICATION",
            ErrorCode::SafeModeViolation => "SAFE_MODE_VIOLATION",

            // Config errors (3xxx)
            ErrorCode::ConfigInvalid => "CONFIG_INVALID",
            ErrorCode::ConfigTomlParse => "CONFIG_TOML_PARSE",
            ErrorCode::ConfigTomlSerialize => "CONFIG_TOML_SERIALIZE",
            ErrorCode::ConfigReadFailed => "CONFIG_READ_FAILED",
            ErrorCode::ConfigWriteFailed => "CONFIG_WRITE_FAILED",
            ErrorCode::ConfigRestartRequired => "CONFIG_RESTART_REQUIRED",
            ErrorCode::ConfigInvalidValue => "CONFIG_INVALID_VALUE",
            ErrorCode::ConfigEnvironmentValidation => "CONFIG_ENVIRONMENT_VALIDATION",

            // Connection errors (4xxx)
            ErrorCode::ConnectionNotConnected => "CONNECTION_NOT_CONNECTED",
            ErrorCode::ConnectionNotFound => "CONNECTION_NOT_FOUND",

            // File inspection errors (43xx)
            ErrorCode::FileReadFailed => "FILE_READ_FAILED",
            ErrorCode::FileParseFailed => "FILE_PARSE_FAILED",
            ErrorCode::AnalyzerNotFound => "ANALYZER_NOT_FOUND",
            ErrorCode::CommandFailed => "COMMAND_FAILED",
            ErrorCode::InvalidPath => "INVALID_PATH",
            ErrorCode::FileNotFound => "FILE_NOT_FOUND",
            ErrorCode::NotAFile => "NOT_A_FILE",
            ErrorCode::LineNotFound => "LINE_NOT_FOUND",

            // Infrastructure errors (5xxx)
            ErrorCode::DatabaseError => "DATABASE_ERROR",
            ErrorCode::AdapterError => "ADAPTER_ERROR",
            ErrorCode::IoError => "IO_ERROR",
            ErrorCode::SerializationError => "SERIALIZATION_ERROR",
            ErrorCode::OutputError => "OUTPUT_ERROR",

            // Auth errors (6xxx)
            ErrorCode::Unauthorized => "UNAUTHORIZED",

            // Generic errors (9xxx)
            ErrorCode::InternalError => "INTERNAL_ERROR",
            ErrorCode::StorageError => "STORAGE_ERROR",
            ErrorCode::PartialGroupFailure => "PARTIAL_GROUP_FAILURE",
            ErrorCode::RollbackFailure => "ROLLBACK_FAILURE",
            ErrorCode::SubprocessTimeout => "SUBPROCESS_TIMEOUT",
        }
    }
}

// ============================================================================
// NotFoundError Trait - Common interface for "not found" style errors
// ============================================================================

/// Trait for "not found" style errors.
///
/// Provides a common interface to check if an error represents a resource
/// not being found and to extract the resource type and identifier.
pub trait NotFoundError {
    /// Returns true if this error represents a "not found" condition
    fn is_not_found(&self) -> bool;

    /// The type of resource that wasn't found (e.g., "metric", "connection")
    fn resource_type(&self) -> Option<&'static str>;

    /// The resource identifier that wasn't found
    fn resource_id(&self) -> Option<&str>;
}

impl NotFoundError for Error {
    fn is_not_found(&self) -> bool {
        matches!(self, Error::MetricNotFound(_))
    }

    fn resource_type(&self) -> Option<&'static str> {
        match self {
            Error::MetricNotFound(_) => Some("metric"),
            _ => None,
        }
    }

    fn resource_id(&self) -> Option<&str> {
        match self {
            Error::MetricNotFound(id) => Some(id),
            _ => None,
        }
    }
}

impl Error {
    /// Helper to create a metric not found error
    pub fn metric_not_found(name: impl Into<String>) -> Self {
        Error::MetricNotFound(name.into())
    }
}

// Error conversions
// Note: These From impls are a pragmatic trade-off to simplify error handling
// in the storage layer. Ideally sqlx would be isolated, but the ergonomic cost
// of wrapping every storage call is too high for this project's scope.
impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::Serialization(err.to_string())
    }
}

#[cfg(feature = "sqlx")]
impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        Error::Database(err.to_string())
    }
}

#[cfg(feature = "sqlx")]
impl From<sqlx::migrate::MigrateError> for Error {
    fn from(err: sqlx::migrate::MigrateError) -> Self {
        Error::Database(err.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err.to_string())
    }
}

impl From<std::string::FromUtf8Error> for Error {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Error::Serialization(format!("Invalid UTF-8: {}", err))
    }
}

impl From<crate::ParseLanguageError> for Error {
    fn from(err: crate::ParseLanguageError) -> Self {
        Error::InvalidConfig(err.to_string())
    }
}

#[derive(Error, Debug, Clone, PartialEq)]
pub enum Error {
    // Metric errors
    #[error("Invalid metric name: {0}")]
    InvalidMetricName(String),

    #[error("Invalid location format: {0}. Expected @file.py#line")]
    InvalidLocation(String),

    #[error("Metric not found: {0}")]
    MetricNotFound(String),

    #[error("Duplicate metric: {0}")]
    DuplicateMetric(String),

    /// Metric already exists at this location (file:line) for this connection.
    /// DAP only supports one logpoint per line.
    /// Use `replace=true` to replace the existing metric, or use `update_metric` to modify it.
    #[error("Metric already exists at {location} for connection {connection_id}: existing metric '{existing_name}' (ID: {existing_id})")]
    MetricLocationConflict {
        /// The conflicting location (file:line)
        location: String,
        /// Connection ID where the conflict occurred
        connection_id: String,
        /// Name of the existing metric
        existing_name: String,
        /// ID of the existing metric
        existing_id: u64,
    },

    // Safety errors
    #[error("Safety violation: {violations:?}")]
    SafetyViolation { violations: Vec<String> },

    #[error("AST analysis failed: {0}")]
    AstAnalysisFailed(String),

    #[error("Safety error: {0}")]
    Safety(String),

    /// SafeMode violation: operation blocked because connection is in SafeMode.
    /// SafeMode only allows logpoints (non-blocking), and blocks operations that
    /// require breakpoints: function calls, stack traces, memory snapshots.
    #[error("SafeMode: {operation} blocked - connection '{connection_id}' is in SafeMode (only logpoints allowed)")]
    SafeModeViolation {
        /// The operation that was blocked
        operation: String,
        /// The connection ID in SafeMode
        connection_id: String,
    },

    // Configuration errors
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    // Expression errors
    #[error("Invalid expression: {0}")]
    InvalidExpression(String),

    #[error("Expression too long: {len} > {max}")]
    ExpressionTooLong { len: usize, max: usize },

    // Storage errors
    #[error("Database error: {0}")]
    Database(String),

    // Serialization errors
    #[error("Serialization error: {0}")]
    Serialization(String),

    // Adapter errors (DAP communication, connection, etc.)
    #[error("Adapter error: {0}")]
    Adapter(String),

    // Connection errors
    #[error("Not connected: {0}")]
    NotConnected(String),

    // I/O errors
    #[error("I/O error: {0}")]
    Io(String),

    // Output errors (GELF, etc.)
    #[error("Output error: {0}")]
    Output(String),
}

impl Error {
    /// Get the machine-readable error code for this error.
    ///
    /// Error codes are stable and can be used for client-side error handling,
    /// internationalization, and monitoring.
    pub fn code(&self) -> ErrorCode {
        match self {
            // Metric errors (1xxx)
            Error::MetricNotFound(_) => ErrorCode::MetricNotFound,
            Error::InvalidMetricName(_) => ErrorCode::MetricInvalidName,
            Error::InvalidLocation(_) => ErrorCode::MetricInvalidLocation,
            Error::DuplicateMetric(_) => ErrorCode::MetricDuplicate,
            Error::MetricLocationConflict { .. } => ErrorCode::MetricLocationConflict,
            Error::InvalidExpression(_) => ErrorCode::MetricInvalidExpression,
            Error::ExpressionTooLong { .. } => ErrorCode::MetricExpressionTooLong,

            // Safety errors (2xxx)
            Error::SafetyViolation { .. } => ErrorCode::SafetyViolation,
            Error::AstAnalysisFailed(_) => ErrorCode::SafetyAstFailed,
            Error::Safety(_) => ErrorCode::SafetyError,
            Error::SafeModeViolation { .. } => ErrorCode::SafeModeViolation,

            // Config errors (3xxx)
            Error::InvalidConfig(_) => ErrorCode::ConfigInvalid,

            // Connection errors (4xxx)
            Error::NotConnected(_) => ErrorCode::ConnectionNotConnected,

            // Infrastructure errors (5xxx)
            Error::Database(_) => ErrorCode::DatabaseError,
            Error::Adapter(_) => ErrorCode::AdapterError,
            Error::Io(_) => ErrorCode::IoError,
            Error::Serialization(_) => ErrorCode::SerializationError,
            Error::Output(_) => ErrorCode::OutputError,
        }
    }

    /// Get the error code name (e.g., "METRIC_NOT_FOUND")
    pub fn code_name(&self) -> &'static str {
        self.code().name()
    }

    /// Get the error category for retry logic.
    pub fn category(&self) -> ErrorCategory {
        self.code().category()
    }

    /// Returns true if this error is safe to retry.
    pub fn is_retryable(&self) -> bool {
        self.category().is_retryable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = Error::InvalidMetricName("test-metric".to_string());
        assert_eq!(err.to_string(), "Invalid metric name: test-metric");
    }

    #[test]
    fn test_safety_violation() {
        let err = Error::SafetyViolation {
            violations: vec!["eval()".to_string(), "exec()".to_string()],
        };
        assert!(err.to_string().contains("eval()"));
    }

    #[test]
    fn test_error_code_metric_not_found() {
        let err = Error::MetricNotFound("test".to_string());
        assert_eq!(err.code(), ErrorCode::MetricNotFound);
        assert_eq!(err.code().as_u16(), 1001);
        assert_eq!(err.code_name(), "METRIC_NOT_FOUND");
    }

    #[test]
    fn test_error_code_safety_violation() {
        let err = Error::SafetyViolation {
            violations: vec!["eval()".to_string()],
        };
        assert_eq!(err.code(), ErrorCode::SafetyViolation);
        assert_eq!(err.code().as_u16(), 2001);
        assert_eq!(err.code_name(), "SAFETY_VIOLATION");
    }

    #[test]
    fn test_error_code_database() {
        let err = Error::Database("connection failed".to_string());
        assert_eq!(err.code(), ErrorCode::DatabaseError);
        assert_eq!(err.code().as_u16(), 5001);
        assert_eq!(err.code_name(), "DATABASE_ERROR");
    }

    #[test]
    fn test_error_code_serialization() {
        let code = ErrorCode::MetricNotFound;
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(json, "1001");
    }

    #[test]
    fn test_error_category_retryable() {
        let err = Error::Database("connection failed".to_string());
        assert_eq!(err.category(), ErrorCategory::Retryable);
        assert!(err.is_retryable());
    }

    #[test]
    fn test_error_category_terminal() {
        let err = Error::MetricNotFound("test".to_string());
        assert_eq!(err.category(), ErrorCategory::Terminal);
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_error_category_serialization() {
        let category = ErrorCategory::Retryable;
        let json = serde_json::to_string(&category).unwrap();
        assert_eq!(json, "\"retryable\"");
    }

    #[test]
    fn test_error_code_category() {
        assert_eq!(
            ErrorCode::DatabaseError.category(),
            ErrorCategory::Retryable
        );
        assert_eq!(
            ErrorCode::MetricNotFound.category(),
            ErrorCategory::Terminal
        );
        assert_eq!(ErrorCode::Unauthorized.category(), ErrorCategory::Security);
        assert_eq!(ErrorCode::InternalError.category(), ErrorCategory::Internal);
    }

    #[test]
    fn test_not_found_error_trait() {
        let err = Error::metric_not_found("cpu_usage");
        assert!(err.is_not_found());
        assert_eq!(err.resource_type(), Some("metric"));
        assert_eq!(err.resource_id(), Some("cpu_usage"));

        // Non-not-found errors
        let other_err = Error::Database("connection failed".to_string());
        assert!(!other_err.is_not_found());
        assert_eq!(other_err.resource_type(), None);
        assert_eq!(other_err.resource_id(), None);
    }
}
