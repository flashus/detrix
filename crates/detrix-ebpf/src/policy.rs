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
    /// Built-in identity when the profile is one of Detrix's compatibility
    /// profiles. Dynamic registry profiles intentionally leave this unset;
    /// `profile_name` is the authoritative identity for them.
    pub profile: Option<ProfileId>,
    /// Registry key used for diagnostics and dynamic profiles. Built-in
    /// callers keep `profile` for compatibility with typed policy rules.
    pub profile_name: String,
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
    resolve_backend_with_rust_auto(
        requested,
        profile,
        ebpf_available,
        debug_info_available,
        false,
    )
}

/// Resolve backend selection with an explicit Rust auto-selection release gate.
/// The gate is deliberately separate from platform/DWARF preflight so a
/// deployment can enable Rust `auto` only after its native privileged matrix
/// has passed. Explicit `capture_backend=ebpf` is unaffected.
pub fn resolve_backend_with_rust_auto(
    requested: CaptureBackend,
    profile: ProfileId,
    ebpf_available: bool,
    debug_info_available: bool,
    rust_auto_enabled: bool,
) -> Result<BackendDecision, PreflightError> {
    if matches!(requested, CaptureBackend::Dap) {
        return Ok(BackendDecision {
            requested,
            selected: CaptureBackend::Dap,
            profile: Some(profile),
            profile_name: profile.as_str().into(),
            reason: "explicit dap".into(),
        });
    }
    if profile == ProfileId::Rust && requested == CaptureBackend::Auto && !rust_auto_enabled {
        return Ok(BackendDecision {
            requested,
            selected: CaptureBackend::Dap,
            profile: Some(profile),
            profile_name: profile.as_str().into(),
            reason: "rust eBPF auto-selection release gate is disabled".into(),
        });
    }
    if !ebpf_available {
        if requested == CaptureBackend::Auto {
            return Ok(BackendDecision {
                requested,
                selected: CaptureBackend::Dap,
                profile: Some(profile),
                profile_name: profile.as_str().into(),
                reason: "eBPF unavailable; auto fell back to DAP".into(),
            });
        }
        return Err(PreflightError::UnsupportedPlatform);
    }
    if !debug_info_available {
        if requested == CaptureBackend::Auto {
            return Ok(BackendDecision {
                requested,
                selected: CaptureBackend::Dap,
                profile: Some(profile),
                profile_name: profile.as_str().into(),
                reason: "usable variable DWARF unavailable; auto fell back to DAP".into(),
            });
        }
        return Err(PreflightError::MissingDebugInfo(
            "usable variable DWARF is required".into(),
        ));
    }
    Ok(BackendDecision {
        requested,
        selected: CaptureBackend::Ebpf,
        profile: Some(profile),
        profile_name: profile.as_str().into(),
        reason: "eBPF capability preflight passed".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rust_auto_remains_dap_until_release_gate() {
        let d = resolve_backend(CaptureBackend::Auto, ProfileId::Rust, true, true).unwrap();
        assert_eq!(d.selected, CaptureBackend::Dap);
    }

    #[test]
    fn rust_auto_selects_ebpf_when_release_gate_is_enabled() {
        let d =
            resolve_backend_with_rust_auto(CaptureBackend::Auto, ProfileId::Rust, true, true, true)
                .unwrap();
        assert_eq!(d.selected, CaptureBackend::Ebpf);
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

    #[test]
    fn auto_falls_back_to_dap_when_ebpf_is_unavailable() {
        let decision = resolve_backend(CaptureBackend::Auto, ProfileId::Go, false, true).unwrap();
        assert_eq!(decision.selected, CaptureBackend::Dap);
    }

    #[test]
    fn auto_falls_back_to_dap_when_variable_dwarf_is_missing() {
        let decision = resolve_backend(CaptureBackend::Auto, ProfileId::Go, true, false)
            .expect("auto must preserve a usable DAP fallback");
        assert_eq!(decision.selected, CaptureBackend::Dap);
    }

    #[test]
    fn explicit_ebpf_rejects_unavailable_platform() {
        assert!(matches!(
            resolve_backend(CaptureBackend::Ebpf, ProfileId::Go, false, true),
            Err(PreflightError::UnsupportedPlatform)
        ));
    }
}
