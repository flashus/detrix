//! Helper functions for Agent Connection Manager.

use super::types::OutgoingAgentMessage;

/// Short ID for display — first 8 chars of hex.
pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Semantic version comparison result.
pub(super) enum SemverCmp {
    Compatible,
    Incompatible { reason: String },
}

/// Compare agent version against minimum compatible version.
/// Major must match, minor must be >= minimum.
pub(super) fn semver_compare(agent_version: &str, min_version: &str) -> SemverCmp {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() < 2 {
            return None;
        }
        let major = parts[0].parse::<u64>().ok()?;
        let minor = parts[1].parse::<u64>().ok()?;
        let patch = parts
            .get(2)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Some((major, minor, patch))
    };

    let (Some(agent), Some(min)) = (parse(agent_version), parse(min_version)) else {
        return SemverCmp::Incompatible {
            reason: format!(
                "Invalid version format: agent={}, min={}",
                agent_version, min_version
            ),
        };
    };

    if agent.0 != min.0 {
        return SemverCmp::Incompatible {
            reason: format!(
                "Major version mismatch: agent={}, min={}",
                agent_version, min_version
            ),
        };
    }

    if agent.1 < min.1 {
        return SemverCmp::Incompatible {
            reason: format!(
                "Minor version too low: agent={}, min={}",
                agent_version, min_version
            ),
        };
    }

    if agent.1 == min.1 && agent.2 < min.2 {
        return SemverCmp::Incompatible {
            reason: format!(
                "Patch version too low: agent={}, min={}",
                agent_version, min_version
            ),
        };
    }

    SemverCmp::Compatible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_compatible(agent: &str, min: &str) -> bool {
        matches!(semver_compare(agent, min), SemverCmp::Compatible)
    }

    #[test]
    fn same_major_minor_lower_patch_incompatible() {
        assert!(!is_compatible("1.2.0", "1.2.1"));
    }

    #[test]
    fn same_major_minor_equal_patch_compatible() {
        assert!(is_compatible("1.2.3", "1.2.3"));
    }

    #[test]
    fn same_major_minor_higher_patch_compatible() {
        assert!(is_compatible("1.2.5", "1.2.3"));
    }

    #[test]
    fn higher_minor_patch_irrelevant_compatible() {
        assert!(is_compatible("1.3.0", "1.2.9"));
    }

    #[test]
    fn lower_minor_incompatible() {
        assert!(!is_compatible("1.1.9", "1.2.0"));
    }

    #[test]
    fn different_major_incompatible() {
        assert!(!is_compatible("2.0.0", "1.0.0"));
        assert!(!is_compatible("1.0.0", "2.0.0"));
    }

    #[test]
    fn invalid_version_incompatible() {
        assert!(!is_compatible("not-a-version", "1.0.0"));
        assert!(!is_compatible("1.0.0", "bad"));
    }

    #[test]
    fn missing_patch_treated_as_zero() {
        // "1.2" = 1.2.0 — compatible with min 1.2.0, not with 1.2.1
        assert!(is_compatible("1.2", "1.2.0"));
        assert!(!is_compatible("1.2", "1.2.1"));
    }
}

/// Extract request_id from an OutgoingAgentMessage variant.
pub fn extract_request_id(msg: &OutgoingAgentMessage) -> Option<&str> {
    match msg {
        OutgoingAgentMessage::SetMetric { request_id, .. } => Some(request_id),
        OutgoingAgentMessage::RemoveMetric { request_id, .. } => Some(request_id),
        OutgoingAgentMessage::ReadFile { request_id, .. } => Some(request_id),
        OutgoingAgentMessage::InspectFile { request_id, .. } => Some(request_id),
        _ => None,
    }
}
