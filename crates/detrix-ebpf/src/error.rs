//! Error types for detrix-ebpf
//!
//! Maps to `detrix_core::Error::Adapter` for upstream propagation.

use detrix_core::Error as CoreError;

pub type Result<T> = std::result::Result<T, Error>;

// ── .context() extension trait for ergonomic error context enrichment ─────────
//
// Replaces the repetitive pattern:
//   .map_err(|e| Error::X(format!("context: {e}")))?
// With:
//   .context("context")?
//
// The target error variant is inferred via the `ErrContext` trait, which is
// implemented for each source error type (gimli::Error, std::io::Error, etc.)
// with the appropriate Error::Xxx constructor.

/// Extension trait for adding context to errors. Auto-detects the error variant
/// via trait impl — call sites only provide the context string.
pub trait ErrContext<T, E> {
    /// Add context to the error message. Replaces
    /// `.map_err(|e| Error::Xxx(format!("context: {e}")))`.
    fn context(self, ctx: &str) -> Result<T>;
}

impl<T, E: std::fmt::Display + 'static> ErrContext<T, E> for std::result::Result<T, E> {
    fn context(self, ctx: &str) -> Result<T> {
        self.map_err(|e| {
            // Try downcasting the source error to apply a targeted context prefix
            // based on the error type. Each error type gets its own mapping.
            wrap_with_context::<E>(ctx, &e)
        })
    }
}

/// Internal: map a source error to `Error` with context prefix, using type inference.
fn wrap_with_context<E: std::fmt::Display + 'static>(ctx: &str, e: &E) -> Error {
    // Try downcasting known error types to their canonical Error variant.
    // This keeps the mapping centralized and type-safe.
    use std::any::Any;
    let any: &dyn Any = e;

    if let Some(e) = any.downcast_ref::<gimli::Error>() {
        return Error::DwarfParse(format!("{ctx}: {e}"));
    }
    if let Some(e) = any.downcast_ref::<object::read::Error>() {
        return Error::DwarfParse(format!("{ctx}: {e}"));
    }
    if let Some(e) = any.downcast_ref::<std::io::Error>() {
        return Error::DwarfParse(format!("{ctx}: {e}"));
    }
    if let Some(e) = any.downcast_ref::<std::string::FromUtf8Error>() {
        return Error::DwarfParse(format!("{ctx}: {e}"));
    }
    if let Some(e) = any.downcast_ref::<std::str::Utf8Error>() {
        return Error::DwarfParse(format!("{ctx}: {e}"));
    }
    #[cfg(target_os = "linux")]
    if let Some(e) = any.downcast_ref::<aya::EbpfError>() {
        return Error::Ebpf(format!("{ctx}: {e}"));
    }
    #[cfg(target_os = "linux")]
    if let Some(e) = any.downcast_ref::<aya::programs::ProgramError>() {
        return Error::Ebpf(format!("{ctx}: {e}"));
    }
    #[cfg(target_os = "linux")]
    if let Some(e) = any.downcast_ref::<aya::maps::MapError>() {
        return Error::Ebpf(format!("{ctx}: {e}"));
    }
    #[cfg(target_os = "linux")]
    if let Some(e) = any.downcast_ref::<tempfile::PathPersistError>() {
        return Error::Ebpf(format!("{ctx}: {e}"));
    }

    // Fallback: unknown error type — use DwarfParse as default
    Error::DwarfParse(format!("{ctx}: {e}"))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// DWARF parsing or resolution failure.
    #[error("DWARF parse error: {0}")]
    DwarfParse(String),

    /// ELF binary is missing required debug info.
    #[error("Missing debug info: {0}")]
    MissingDebugInfo(String),

    /// Variable not found at the requested source location.
    #[error("Variable not found: {0}")]
    VariableNotFound(String),

    /// eBPF program load/attach failure (Linux only).
    #[error("eBPF error: {0}")]
    Ebpf(String),

    /// Ring buffer read error.
    #[error("Ring buffer error: {0}")]
    RingBuffer(String),

    /// Adapter lifecycle error.
    #[error("Adapter error: {0}")]
    Adapter(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// DWARF library error (gimli).
    #[error("DWARF library error: {0}")]
    Gimli(#[from] gimli::Error),

    /// Binary path contains non-UTF-8 bytes.
    #[error("Invalid binary path: {0}")]
    InvalidBinaryPath(String),
}

// ── From impls for common error sources (project policy: no .map_err chains) ──

impl From<std::string::FromUtf8Error> for Error {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Error::DwarfParse(err.to_string())
    }
}

impl From<std::str::Utf8Error> for Error {
    fn from(err: std::str::Utf8Error) -> Self {
        Error::DwarfParse(err.to_string())
    }
}

impl From<object::read::Error> for Error {
    fn from(err: object::read::Error) -> Self {
        Error::DwarfParse(err.to_string())
    }
}

#[cfg(target_os = "linux")]
impl From<aya::EbpfError> for Error {
    fn from(err: aya::EbpfError) -> Self {
        Error::Ebpf(err.to_string())
    }
}

#[cfg(target_os = "linux")]
impl From<aya::programs::ProgramError> for Error {
    fn from(err: aya::programs::ProgramError) -> Self {
        Error::Ebpf(err.to_string())
    }
}

#[cfg(target_os = "linux")]
impl From<aya::maps::MapError> for Error {
    fn from(err: aya::maps::MapError) -> Self {
        Error::Ebpf(err.to_string())
    }
}

#[cfg(target_os = "linux")]
impl From<tempfile::PathPersistError> for Error {
    fn from(err: tempfile::PathPersistError) -> Self {
        Error::Ebpf(err.to_string())
    }
}

impl From<Error> for CoreError {
    fn from(err: Error) -> Self {
        CoreError::Adapter(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_to_core_adapter() {
        let err = Error::DwarfParse("bad DWARF".to_string());
        let core_err: CoreError = err.into();
        assert!(matches!(core_err, CoreError::Adapter(_)));
        assert!(core_err.to_string().contains("DWARF parse error"));
    }

    #[test]
    fn error_display() {
        let err = Error::Ebpf("verifier rejected".to_string());
        assert_eq!(err.to_string(), "eBPF error: verifier rejected");
    }

    #[test]
    fn io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
    }
}
