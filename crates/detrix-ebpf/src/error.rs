//! Error types for detrix-ebpf
//!
//! Maps to `detrix_core::Error::Adapter` for upstream propagation.

use detrix_core::Error as CoreError;

pub type Result<T> = std::result::Result<T, Error>;

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
