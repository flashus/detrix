//! API response string constants
//!
//! These are HTTP/gRPC response status strings that are not related to domain entities.
//! Domain-specific string representations are handled by enum `as_str()` methods
//! (e.g., `SourceLanguage::as_str()`, `MetricMode::as_str()`).

/// Status strings for API responses
pub mod status {
    /// Operation completed successfully
    pub const OK: &str = "ok";
    /// Resource was created
    pub const CREATED: &str = "created";
    /// Resource was updated
    pub const UPDATED: &str = "updated";
    /// Resource was found
    pub const FOUND: &str = "found";
    /// System is active
    pub const ACTIVE: &str = "active";
    /// System is idle (no active connections)
    pub const IDLE: &str = "idle";
    /// All adapters disconnected successfully
    pub const DISCONNECTED: &str = "disconnected";
    /// Some adapters failed to stop
    pub const PARTIAL_FAILURE: &str = "partial_failure";
}
