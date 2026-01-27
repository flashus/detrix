use detrix_core::ErrorCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Core error: {0}")]
    Core(#[from] detrix_core::Error),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("gRPC status error: {0}")]
    Status(#[from] tonic::Status),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Get the machine-readable error code for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Core(err) => err.code(),
            Error::Storage(_) => ErrorCode::StorageError,
            Error::Transport(_) => ErrorCode::AdapterError,
            Error::Status(_) => ErrorCode::InternalError,
            Error::Serialization(_) => ErrorCode::SerializationError,
            Error::InvalidRequest(_) => ErrorCode::ConfigInvalid,
            Error::NotFound(_) => ErrorCode::MetricNotFound,
            Error::Internal(_) => ErrorCode::InternalError,
            Error::Unauthorized(_) => ErrorCode::Unauthorized,
        }
    }

    /// Get the error code name (e.g., "METRIC_NOT_FOUND")
    pub fn code_name(&self) -> &'static str {
        self.code().name()
    }
}

// Convert ParseLanguageError to API error (allows ? operator to work)
impl From<detrix_core::ParseLanguageError> for Error {
    fn from(err: detrix_core::ParseLanguageError) -> Self {
        Error::InvalidRequest(err.to_string())
    }
}

// Convert application errors to API errors (allows ? operator to work)
impl From<detrix_application::Error> for Error {
    fn from(err: detrix_application::Error) -> Self {
        match err {
            detrix_application::Error::Core(core_err) => Error::Core(core_err),
            detrix_application::Error::Storage(msg) => Error::Storage(msg),
            detrix_application::Error::PartialGroupFailure {
                succeeded,
                failures,
            } => Error::Internal(format!(
                "Partial group failure: {} succeeded, {} failed: {:?}",
                succeeded,
                failures.len(),
                failures
            )),
            detrix_application::Error::OperationWithRollbackFailure {
                primary,
                rollback,
                context,
            } => Error::Internal(format!(
                "Operation failed: {}. Rollback also failed: {} ({})",
                primary, rollback, context
            )),
            detrix_application::Error::Output(msg) => {
                Error::Internal(format!("Output error: {}", msg))
            }
            detrix_application::Error::ConnectionNotFound(id) => {
                Error::NotFound(format!("Connection not found: {}", id))
            }
            detrix_application::Error::ConfigTyped(err) => {
                Error::InvalidRequest(format!("Config error: {}", err))
            }
            detrix_application::Error::SafetyTyped(err) => {
                Error::InvalidRequest(format!("Safety error: {}", err))
            }
            detrix_application::Error::FileInspectionTyped(
                detrix_application::FileInspectionError::AnalyzerNotFound {
                    analyzer,
                    tried_paths,
                },
            ) => Error::Internal(format!(
                "Analyzer not found: {}. Tried: {}",
                analyzer,
                tried_paths.join(", ")
            )),
            detrix_application::Error::FileInspectionTyped(err) => {
                Error::Internal(format!("File inspection error: {}", err))
            }
            detrix_application::Error::SubprocessTimeout {
                command,
                timeout_ms,
            } => Error::Internal(format!(
                "Subprocess '{}' timed out after {}ms",
                command, timeout_ms
            )),
            detrix_application::Error::Auth(msg) => Error::Unauthorized(msg),
        }
    }
}

/// Extension trait for converting application Results to gRPC Status Results.
/// Enables ergonomic error handling: `.to_status()?` instead of `.map_err(app_err_to_status)?`
pub trait ToStatusResult<T> {
    fn to_status(self) -> std::result::Result<T, tonic::Status>;
}

impl<T> ToStatusResult<T> for std::result::Result<T, detrix_application::Error> {
    fn to_status(self) -> std::result::Result<T, tonic::Status> {
        self.map_err(|err| {
            let api_err: Error = err.into();
            api_err.into()
        })
    }
}

// Convert API errors to gRPC Status
impl From<Error> for tonic::Status {
    fn from(err: Error) -> Self {
        match err {
            Error::NotFound(msg) => tonic::Status::not_found(msg),
            Error::InvalidRequest(msg) => tonic::Status::invalid_argument(msg),
            Error::Core(detrix_core::Error::MetricNotFound(msg)) => tonic::Status::not_found(msg),
            Error::Core(detrix_core::Error::InvalidMetricName(msg)) => {
                tonic::Status::invalid_argument(msg)
            }
            Error::Core(detrix_core::Error::InvalidLocation(msg)) => {
                tonic::Status::invalid_argument(msg)
            }
            Error::Core(detrix_core::Error::DuplicateMetric(msg)) => {
                tonic::Status::already_exists(msg)
            }
            Error::Core(detrix_core::Error::SafetyViolation { violations }) => {
                tonic::Status::invalid_argument(format!("Safety violations: {:?}", violations))
            }
            Error::Core(detrix_core::Error::InvalidExpression(msg)) => {
                tonic::Status::invalid_argument(msg)
            }
            Error::Core(detrix_core::Error::Adapter(msg)) => {
                tonic::Status::unavailable(format!("Adapter error: {}", msg))
            }
            _ => tonic::Status::internal(err.to_string()),
        }
    }
}

// Convert API errors to HTTP status codes (for REST API)
impl Error {
    pub fn status_code(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;

        match self {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Error::Core(detrix_core::Error::MetricNotFound(_)) => StatusCode::NOT_FOUND,
            Error::Core(detrix_core::Error::InvalidMetricName(_)) => StatusCode::BAD_REQUEST,
            Error::Core(detrix_core::Error::Adapter(_)) => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
