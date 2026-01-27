//! Error handling utilities for output adapters
//!
//! Provides extension traits for ergonomic error conversion to output errors.

use detrix_core::Error;

/// Extension trait for converting Results to Output errors with context
///
/// # Example
/// ```ignore
/// // Before
/// let client = builder.build().map_err(|e| {
///     detrix_core::Error::Output(format!("Failed to create HTTP client: {}", e))
/// })?;
///
/// // After
/// let client = builder.build().output_context("Failed to create HTTP client")?;
/// ```
pub trait ToOutputResult<T> {
    /// Convert a Result to an output error with context message
    fn output_context(self, context: &str) -> Result<T, Error>;
}

impl<T, E: std::fmt::Display> ToOutputResult<T> for Result<T, E> {
    fn output_context(self, context: &str) -> Result<T, Error> {
        self.map_err(|e| Error::Output(format!("{}: {}", context, e)))
    }
}

/// Extension trait for converting Options to Output errors
pub trait OptionToOutputResult<T> {
    /// Convert None to an output error
    fn output_ok_or(self, message: &str) -> Result<T, Error>;
}

impl<T> OptionToOutputResult<T> for Option<T> {
    fn output_ok_or(self, message: &str) -> Result<T, Error> {
        self.ok_or_else(|| Error::Output(message.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_context_ok() {
        let result: Result<i32, &str> = Ok(42);
        let output_result = result.output_context("Test context");
        assert!(output_result.is_ok());
        assert_eq!(output_result.unwrap(), 42);
    }

    #[test]
    fn test_output_context_err() {
        let result: Result<i32, &str> = Err("original error");
        let output_result = result.output_context("Failed to do thing");
        assert!(output_result.is_err());
        match output_result.unwrap_err() {
            Error::Output(msg) => {
                assert!(msg.contains("Failed to do thing"));
                assert!(msg.contains("original error"));
            }
            _ => panic!("Expected Output error"),
        }
    }

    #[test]
    fn test_option_output_ok_or_some() {
        let opt: Option<i32> = Some(42);
        let result = opt.output_ok_or("Not found");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_option_output_ok_or_none() {
        let opt: Option<i32> = None;
        let result = opt.output_ok_or("Resource not found");
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Output(msg) => {
                assert_eq!(msg, "Resource not found");
            }
            _ => panic!("Expected Output error"),
        }
    }
}
