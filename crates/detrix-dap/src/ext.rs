//! Extension traits for DAP operations.
//!
//! Provides ergonomic helpers for best-effort operations where failure
//! should be logged but not propagate errors.

/// Extension trait for Result types in best-effort operations.
///
/// Use this for introspection and other optional operations where
/// failure should be logged at debug level but not propagate errors.
///
/// # Example
///
/// ```text
/// // Before:
/// let args_json = match serde_json::to_value(&args) {
///     Ok(v) => v,
///     Err(e) => {
///         debug!("Failed to serialize args: {}", e);
///         return None;
///     }
/// };
///
/// // After:
/// let args_json = serde_json::to_value(&args)
///     .debug_ok("Failed to serialize args")?;
/// ```
pub trait DebugResult<T> {
    /// Convert to Option, logging error at debug level if Err.
    ///
    /// Useful in functions returning `Option<T>` where errors are
    /// expected and should be logged but not propagated.
    fn debug_ok(self, context: &str) -> Option<T>;
}

impl<T, E: std::fmt::Display> DebugResult<T> for Result<T, E> {
    fn debug_ok(self, context: &str) -> Option<T> {
        match self {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::debug!("{}: {}", context, e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug_ok_success() {
        let result: Result<i32, &str> = Ok(42);
        assert_eq!(result.debug_ok("test"), Some(42));
    }

    #[test]
    fn test_debug_ok_error() {
        let result: Result<i32, &str> = Err("error");
        assert_eq!(result.debug_ok("test"), None);
    }
}
