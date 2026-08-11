//! Transactional backend/profile selection and structured diagnostics.

use crate::profile::ProfileId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackend {
    Auto,
    Dap,
    Ebpf,
}

impl CaptureBackend {
    pub fn parse(value: &str) -> Result<Self, PreflightError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "dap" => Ok(Self::Dap),
            "ebpf" => Ok(Self::Ebpf),
            other => Err(PreflightError::UnknownBackend(other.into())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendDecision {
    pub requested: CaptureBackend,
    pub selected: CaptureBackend,
    pub profile: ProfileId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreflightError {
    #[error("unknown capture backend: {0}")]
    UnknownBackend(String),
    #[error("unsupported profile: {0}")]
    UnsupportedProfile(String),
    #[error("unsupported platform for eBPF")]
    UnsupportedPlatform,
    #[error("unsupported ABI for {0}")]
    UnsupportedAbi(String),
    #[error("missing debug info: {0}")]
    MissingDebugInfo(String),
}

pub fn resolve_backend(
    requested: CaptureBackend,
    profile: ProfileId,
    ebpf_available: bool,
    debug_info_available: bool,
) -> Result<BackendDecision, PreflightError> {
    if matches!(requested, CaptureBackend::Dap) {
        return Ok(BackendDecision {
            requested,
            selected: CaptureBackend::Dap,
            profile,
            reason: "explicit dap".into(),
        });
    }
    // Go auto behavior is compatibility-sensitive. Rust auto remains DAP until
    // its privileged live gate is complete; explicit ebpf is fail-closed.
    if profile == ProfileId::Rust && requested == CaptureBackend::Auto {
        return Ok(BackendDecision {
            requested,
            selected: CaptureBackend::Dap,
            profile,
            reason: "rust eBPF remains opt-in until live gate".into(),
        });
    }
    if !ebpf_available {
        return Err(PreflightError::UnsupportedPlatform);
    }
    if !debug_info_available {
        return Err(PreflightError::MissingDebugInfo(
            "usable variable DWARF is required".into(),
        ));
    }
    Ok(BackendDecision {
        requested,
        selected: CaptureBackend::Ebpf,
        profile,
        reason: "eBPF capability preflight passed".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rust_auto_is_dap() {
        let d = resolve_backend(CaptureBackend::Auto, ProfileId::Rust, true, true).unwrap();
        assert_eq!(d.selected, CaptureBackend::Dap);
    }
    #[test]
    fn explicit_dap_wins() {
        let d = resolve_backend(CaptureBackend::Dap, ProfileId::Go, false, false).unwrap();
        assert_eq!(d.selected, CaptureBackend::Dap);
    }
    #[test]
    fn ebpf_missing_debug_info_fails_closed() {
        assert!(matches!(
            resolve_backend(CaptureBackend::Ebpf, ProfileId::Go, true, false),
            Err(PreflightError::MissingDebugInfo(_))
        ));
    }
}
