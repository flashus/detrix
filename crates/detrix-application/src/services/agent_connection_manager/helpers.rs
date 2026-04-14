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

    SemverCmp::Compatible
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
