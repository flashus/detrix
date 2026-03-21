//! HTTP error types and response conversion
//!
//! Provides ergonomic error handling for HTTP handlers with automatic
//! status code mapping based on error type.
//!
//! ## Security
//!
//! Error messages are sanitized before being returned to clients to prevent
//! information disclosure. Full error details are logged internally for debugging.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use detrix_core::ErrorCode;
use serde::{Deserialize, Serialize};
use tracing::error;

/// HTTP API error with status code and message
#[derive(Debug)]
pub struct HttpError {
    pub status: StatusCode,
    pub message: String,
    pub error_code: ErrorCode,
}

impl HttpError {
    /// Create an internal server error
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            error_code: ErrorCode::InternalError,
        }
    }

    /// Create a not found error
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            error_code: ErrorCode::MetricNotFound,
        }
    }

    /// Create a bad request error
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            error_code: ErrorCode::ConfigInvalid,
        }
    }

    /// Create a not implemented error (501)
    pub fn unimplemented(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            message: message.into(),
            error_code: ErrorCode::InternalError,
        }
    }

    /// Create an unauthorized error (401)
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
            error_code: ErrorCode::Unauthorized,
        }
    }

    /// Create a forbidden error (403)
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
            error_code: ErrorCode::Forbidden,
        }
    }

    /// Create an error with a specific error code
    pub fn with_code(
        status: StatusCode,
        message: impl Into<String>,
        error_code: ErrorCode,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            error_code,
        }
    }
}

/// Type alias for ApiError (used by auth handlers)
pub type ApiError = HttpError;

/// Result type for API handlers
pub type ApiResult<T> = Result<T, ApiError>;

/// JSON error response body
///
/// This struct is public so it can be used in tests to parse error responses.
/// Has both Serialize (for API responses) and Deserialize (for tests).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    /// Human-readable error message
    pub error: String,
    /// Machine-readable error code (e.g., 1001)
    pub error_code: u16,
    /// Error code name (e.g., "METRIC_NOT_FOUND")
    pub error_type: String,
    /// Error category for retry logic (e.g., "retryable", "terminal")
    pub category: String,
    /// Whether this error is safe to retry
    pub retryable: bool,
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let category = self.error_code.category();
        let body = ErrorResponse {
            error: self.message,
            error_code: self.error_code.as_u16(),
            error_type: self.error_code.name().to_string(),
            category: category.name().to_string(),
            retryable: category.is_retryable(),
        };
        (self.status, Json(body)).into_response()
    }
}

/// Sanitize error messages for client responses
///
/// Returns a user-friendly message that doesn't expose internal implementation
/// details, file paths, or sensitive information.
fn sanitize_error_message(err: &detrix_application::Error) -> String {
    use detrix_application::Error;
    match err {
        // Connection errors - safe to indicate "not found"
        Error::ConnectionNotFound(id) => format!("Connection '{}' not found", id),

        // Safety errors - provide sanitized message without expression details
        Error::SafetyTyped(_) => "Expression validation failed".to_string(),

        // Storage errors - never expose database details
        Error::Storage(_) => "An internal error occurred".to_string(),

        // Config errors - never expose config paths
        Error::ConfigTyped(_) => "Configuration error".to_string(),

        // File inspection errors - don't expose file paths
        Error::FileInspectionTyped(_) => "File inspection error".to_string(),

        // Output errors - don't expose output target details
        Error::Output(_) => "Output error".to_string(),

        // Partial failures - provide count but not details
        Error::PartialGroupFailure {
            succeeded,
            failures,
        } => {
            format!(
                "Partial failure: {} succeeded, {} failed",
                succeeded,
                failures.len()
            )
        }

        // Rollback failures - don't expose internal state
        Error::OperationWithRollbackFailure { .. } => {
            "Operation failed and could not be rolled back".to_string()
        }

        // Subprocess timeout - don't expose command
        Error::SubprocessTimeout { timeout_ms, .. } => {
            format!("Operation timed out after {}ms", timeout_ms)
        }

        // Auth errors - provide generic message
        Error::Auth(_) => "Authentication error".to_string(),

        // Access denied - safe to expose reason
        Error::AccessDenied(reason) => format!("Access denied: {}", reason),

        // Core domain errors - delegate to core error handling
        Error::Core(core_err) => sanitize_core_error(core_err),
    }
}

/// Sanitize core domain errors
fn sanitize_core_error(err: &detrix_core::Error) -> String {
    use detrix_core::Error;
    match err {
        // Resource not found - safe to expose the resource type
        Error::MetricNotFound(name) => format!("Metric '{}' not found", name),

        // Location errors - don't expose file paths
        Error::InvalidLocation(_) => "Invalid location format".to_string(),

        // Metric conflicts - safe to indicate conflict exists
        Error::MetricLocationConflict { .. } => {
            "A metric already exists at this location".to_string()
        }
        Error::DuplicateMetric(name) => format!("Metric '{}' already exists", name),

        // Name/expression validation - safe user feedback
        Error::InvalidMetricName(_) => "Invalid metric name".to_string(),
        Error::InvalidExpression(_) => "Invalid expression".to_string(),

        // Safety violations - generic message, don't expose details
        Error::SafetyViolation { .. } => "Expression validation failed".to_string(),

        // Configuration errors - don't expose paths
        Error::InvalidConfig(_) => "Configuration error".to_string(),

        // Tenant ID validation - safe to expose
        Error::InvalidTenantId(msg) => format!("Invalid tenant ID: {}", msg),

        // All other core errors - generic message
        _ => "An internal error occurred".to_string(),
    }
}

/// Convert core domain errors to HTTP errors
impl From<detrix_core::Error> for HttpError {
    fn from(err: detrix_core::Error) -> Self {
        use detrix_core::Error;

        error!(error = %err, "API error occurred");

        let error_code = err.code();
        let message = sanitize_core_error(&err);
        let status = match &err {
            Error::MetricNotFound(_) => StatusCode::NOT_FOUND,
            Error::InvalidLocation(_)
            | Error::InvalidMetricName(_)
            | Error::InvalidExpression(_)
            | Error::DuplicateMetric(_)
            | Error::MetricLocationConflict { .. }
            | Error::SafetyViolation { .. }
            | Error::InvalidTenantId(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        HttpError {
            status,
            message,
            error_code,
        }
    }
}

/// Convert application errors to HTTP errors
///
/// ## Security
///
/// Full error details are logged internally but sanitized messages are
/// returned to clients to prevent information disclosure.
impl From<detrix_application::Error> for HttpError {
    fn from(err: detrix_application::Error) -> Self {
        use detrix_application::Error;

        // Log full error details internally for debugging
        error!(error = %err, "API error occurred");

        // Get error code from the application error
        let error_code = err.code();

        // Get sanitized message for client response
        let message = sanitize_error_message(&err);

        // Map to appropriate HTTP status code
        let status = match &err {
            // 404 Not Found
            Error::ConnectionNotFound(_) => StatusCode::NOT_FOUND,
            Error::Core(core_err) => match core_err {
                detrix_core::Error::MetricNotFound(_) => StatusCode::NOT_FOUND,
                detrix_core::Error::InvalidLocation(_)
                | detrix_core::Error::InvalidMetricName(_)
                | detrix_core::Error::InvalidExpression(_)
                | detrix_core::Error::DuplicateMetric(_)
                | detrix_core::Error::MetricLocationConflict { .. } => StatusCode::BAD_REQUEST,
                detrix_core::Error::SafetyViolation { .. } => StatusCode::BAD_REQUEST,
                detrix_core::Error::InvalidTenantId(_) => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },

            // 400 Bad Request
            Error::SafetyTyped(_) | Error::ConfigTyped(_) => StatusCode::BAD_REQUEST,

            // 401 Unauthorized
            Error::Auth(_) => StatusCode::UNAUTHORIZED,

            // 403 Forbidden
            Error::AccessDenied(_) => StatusCode::FORBIDDEN,

            // 500 Internal Server Error (default)
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        HttpError {
            status,
            message,
            error_code,
        }
    }
}

/// Convert proto/gRPC conversion errors to HTTP 500.
impl From<crate::grpc::conversions::ConversionError> for HttpError {
    fn from(err: crate::grpc::conversions::ConversionError) -> Self {
        HttpError::internal(format!("Conversion error: {err}"))
    }
}

/// Extension trait for converting `Result<T, E: Into<HttpError>>` to HTTP results.
///
/// Preserves the typed HTTP status code from the error conversion:
/// `AccessDenied` → 403, `MetricNotFound` → 404, etc.
///
/// - `http_context`: adds a context prefix to 5xx messages; 4xx pass through as-is.
/// - `http_err`: converts without adding context.
///
/// ```ignore
/// service.remove_metric(id, &scope).await.http_context("Failed to delete metric")?;
/// ```
pub trait ToHttpResult<T> {
    /// Convert to HTTP result, preserving 4xx status codes.
    /// For 5xx errors, `context` is prepended to the message.
    fn http_context(self, context: &str) -> Result<T, HttpError>;

    /// Convert to HTTP result using typed error conversion, without added context.
    fn http_err(self) -> Result<T, HttpError>;
}

impl<T, E: Into<HttpError>> ToHttpResult<T> for Result<T, E> {
    fn http_context(self, context: &str) -> Result<T, HttpError> {
        self.map_err(|e| {
            let http_err = e.into();
            if http_err.status.is_server_error() {
                HttpError {
                    message: format!("{}: {}", context, http_err.message),
                    ..http_err
                }
            } else {
                http_err
            }
        })
    }

    fn http_err(self) -> Result<T, HttpError> {
        self.map_err(|e| e.into())
    }
}

/// Extension trait for converting any `Result<T, E: Display>` to a 400 Bad Request.
///
/// Use this for parse/validation errors on non-application types (port parsing,
/// language strings, safety levels, etc.).
///
/// ```ignore
/// parse_language(&payload.language).http_bad_request()?;
/// ```
pub trait ToHttpBadRequest<T> {
    /// Convert to a 400 Bad Request error.
    fn http_bad_request(self) -> Result<T, HttpError>;

    /// Convert to a 400 Bad Request error with a context prefix.
    fn http_bad_request_context(self, context: &str) -> Result<T, HttpError>;
}

impl<T, E: std::fmt::Display> ToHttpBadRequest<T> for Result<T, E> {
    fn http_bad_request(self) -> Result<T, HttpError> {
        self.map_err(|e| HttpError::bad_request(e.to_string()))
    }

    fn http_bad_request_context(self, context: &str) -> Result<T, HttpError> {
        self.map_err(|e| HttpError::bad_request(format!("{}: {}", context, e)))
    }
}

/// Convert Option to HTTP NotFound error
pub trait ToHttpOption<T> {
    /// Convert None to NotFound error
    fn http_not_found(self, resource: &str) -> Result<T, HttpError>;
}

impl<T> ToHttpOption<T> for Option<T> {
    fn http_not_found(self, resource: &str) -> Result<T, HttpError> {
        self.ok_or_else(|| HttpError::not_found(format!("{} not found", resource)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_error_internal() {
        let err = HttpError::internal("something went wrong");
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message, "something went wrong");
        assert_eq!(err.error_code, ErrorCode::InternalError);
    }

    #[test]
    fn test_http_error_not_found() {
        let err = HttpError::not_found("Metric not found");
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.error_code, ErrorCode::MetricNotFound);
    }

    #[test]
    fn test_http_error_bad_request() {
        let err = HttpError::bad_request("Invalid input");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.error_code, ErrorCode::ConfigInvalid);
    }

    #[test]
    fn test_http_error_with_code() {
        let err = HttpError::with_code(
            StatusCode::CONFLICT,
            "Duplicate entry",
            ErrorCode::MetricDuplicate,
        );
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert_eq!(err.message, "Duplicate entry");
        assert_eq!(err.error_code, ErrorCode::MetricDuplicate);
        assert_eq!(err.error_code.as_u16(), 1004);
    }

    #[test]
    fn test_to_http_bad_request_with_context() {
        let result: Result<(), &str> = Err("invalid input");
        let http_result = result.http_bad_request_context("Failed to parse");
        assert!(http_result.is_err());
        let err = http_result.unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("Failed to parse"));
        assert!(err.message.contains("invalid input"));
    }

    #[test]
    fn test_to_http_result_typed_preserves_4xx() {
        let app_err = detrix_application::Error::AccessDenied("not your metric".to_string());
        let result: detrix_application::Result<()> = Err(app_err);
        let http_result = result.http_context("Failed to delete");
        let err = http_result.unwrap_err();
        // 4xx is preserved as-is — context prefix is NOT added
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(!err.message.contains("Failed to delete"));
    }

    #[test]
    fn test_to_http_result_typed_adds_context_on_5xx() {
        let app_err = detrix_application::Error::Storage("disk full".to_string());
        let result: detrix_application::Result<()> = Err(app_err);
        let http_result = result.http_context("Failed to save");
        let err = http_result.unwrap_err();
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("Failed to save"));
    }

    #[test]
    fn test_option_not_found() {
        let opt: Option<i32> = None;
        let result = opt.http_not_found("Metric 123");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert!(err.message.contains("Metric 123"));
    }

    #[test]
    fn test_application_error_connection_not_found() {
        let app_err = detrix_application::Error::ConnectionNotFound("test_conn".to_string());
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::NOT_FOUND);
        // Should contain connection ID but no internal details
        assert!(http_err.message.contains("test_conn"));
        assert!(http_err.message.contains("not found"));
    }

    #[test]
    fn test_application_error_safety() {
        let app_err = detrix_application::Error::SafetyTyped(
            detrix_application::SafetyError::ValidationFailed {
                expression: "__import__('os')".to_string(),
                violations: vec!["__import__ is not allowed".to_string()],
            },
        );
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::BAD_REQUEST);
        // Should not expose expression details
        assert_eq!(http_err.message, "Expression validation failed");
        assert!(!http_err.message.contains("__import__"));
    }

    #[test]
    fn test_application_error_storage() {
        let app_err =
            detrix_application::Error::Storage("SQLITE_BUSY: /path/to/db.sqlite".to_string());
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::INTERNAL_SERVER_ERROR);
        // Should not expose database details or paths
        assert_eq!(http_err.message, "An internal error occurred");
        assert!(!http_err.message.contains("SQLITE"));
        assert!(!http_err.message.contains("/path"));
    }

    #[test]
    fn test_application_error_adapter() {
        let core_err =
            detrix_core::Error::Adapter("debugpy connection failed at 127.0.0.1:5678".to_string());
        let app_err = detrix_application::Error::Core(core_err);
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::INTERNAL_SERVER_ERROR);
        // Should not expose adapter implementation details
        assert_eq!(http_err.message, "An internal error occurred");
        assert!(!http_err.message.contains("debugpy"));
        assert!(!http_err.message.contains("127.0.0.1"));
    }

    #[test]
    fn test_application_error_config() {
        let app_err =
            detrix_application::Error::ConfigTyped(detrix_application::ConfigError::Read {
                path: "/etc/detrix/config.toml".into(),
                message: "permission denied".to_string(),
            });
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::BAD_REQUEST);
        // Should not expose config paths
        assert_eq!(http_err.message, "Configuration error");
        assert!(!http_err.message.contains("/etc"));
    }

    #[test]
    fn test_application_error_file_inspection() {
        let app_err = detrix_application::Error::FileInspectionTyped(
            detrix_application::FileInspectionError::Read {
                path: "/home/user/secret.py".into(),
                message: "permission denied".to_string(),
            },
        );
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::INTERNAL_SERVER_ERROR);
        // Should not expose file paths
        assert_eq!(http_err.message, "File inspection error");
        assert!(!http_err.message.contains("/home"));
    }

    #[test]
    fn test_application_error_output() {
        let app_err = detrix_application::Error::Output(
            "GELF connection failed to 192.168.1.100:12201".to_string(),
        );
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::INTERNAL_SERVER_ERROR);
        // Should not expose network details
        assert_eq!(http_err.message, "Output error");
        assert!(!http_err.message.contains("192.168"));
    }

    #[test]
    fn test_application_error_core_metric_not_found() {
        let core_err = detrix_core::Error::MetricNotFound("cpu_usage".to_string());
        let app_err = detrix_application::Error::Core(core_err);
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::NOT_FOUND);
        // Metric name is safe to expose
        assert!(http_err.message.contains("cpu_usage"));
        assert!(http_err.message.contains("not found"));
        // Error code should be preserved from core error
        assert_eq!(http_err.error_code, ErrorCode::MetricNotFound);
        assert_eq!(http_err.error_code.as_u16(), 1001);
    }

    #[test]
    fn test_application_error_core_invalid_location() {
        let core_err =
            detrix_core::Error::InvalidLocation("/usr/local/bin/secret_script.py:42".to_string());
        let app_err = detrix_application::Error::Core(core_err);
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::BAD_REQUEST);
        // Should not expose file paths
        assert_eq!(http_err.message, "Invalid location format");
        assert!(!http_err.message.contains("/usr"));
    }

    #[test]
    fn test_application_error_core_safety_violation() {
        let core_err = detrix_core::Error::SafetyViolation {
            violations: vec!["eval() is not allowed".to_string()],
        };
        let app_err = detrix_application::Error::Core(core_err);
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::BAD_REQUEST);
        // Should not expose violation details
        assert_eq!(http_err.message, "Expression validation failed");
        assert!(!http_err.message.contains("eval"));
    }

    #[test]
    fn test_application_error_core_duplicate_metric() {
        let core_err = detrix_core::Error::DuplicateMetric("request_count".to_string());
        let app_err = detrix_application::Error::Core(core_err);
        let http_err: HttpError = app_err.into();
        assert_eq!(http_err.status, StatusCode::BAD_REQUEST);
        // Metric name is safe to expose
        assert!(http_err.message.contains("request_count"));
        assert!(http_err.message.contains("already exists"));
    }
}
