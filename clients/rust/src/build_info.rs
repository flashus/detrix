//! Build information detection

use std::process::Command;

// Include compile-time build info from build.rs
include!(concat!(env!("OUT_DIR"), "/build_info.rs"));

/// Detect build commit with layered fallback strategy
pub fn detect_build_commit(override_value: Option<String>) -> Option<String> {
    // Layer 1: Explicit override
    if let Some(commit) = override_value {
        if !commit.is_empty() {
            return Some(commit);
        }
    }

    // Layer 2: DETRIX_BUILD_COMMIT env var
    if let Ok(commit) = std::env::var("DETRIX_BUILD_COMMIT") {
        if !commit.is_empty() {
            return Some(commit);
        }
    }

    // Layer 3: CI-specific env vars
    if let Ok(commit) = std::env::var("GIT_COMMIT")
        .or_else(|_| std::env::var("CI_COMMIT_SHA"))
        .or_else(|_| std::env::var("GITHUB_SHA"))
    {
        if !commit.is_empty() {
            return Some(commit);
        }
    }

    // Layer 4: Compile-time value (from build.rs)
    if BUILD_COMMIT != "unknown" && !BUILD_COMMIT.is_empty() {
        return Some(BUILD_COMMIT.to_string());
    }

    // Layer 5: Runtime git detection (dev mode)
    if let Some(commit) = try_git_rev_parse() {
        return Some(commit);
    }

    None
}

/// Detect build tag with layered fallback strategy
pub fn detect_build_tag(override_value: Option<String>) -> Option<String> {
    // Layer 1: Explicit override
    if let Some(tag) = override_value {
        if !tag.is_empty() {
            return Some(tag);
        }
    }

    // Layer 2: DETRIX_BUILD_TAG env var
    if let Ok(tag) = std::env::var("DETRIX_BUILD_TAG") {
        if !tag.is_empty() {
            return Some(tag);
        }
    }

    // Layer 3: CI-specific env vars
    if let Ok(tag) = std::env::var("GIT_TAG")
        .or_else(|_| std::env::var("CI_COMMIT_TAG"))
        .or_else(|_| std::env::var("GITHUB_REF_NAME"))
    {
        if !tag.is_empty() {
            return Some(tag);
        }
    }

    // Layer 4: Compile-time value (from build.rs)
    if BUILD_TAG != "unknown" && !BUILD_TAG.is_empty() && BUILD_TAG != VERSION {
        return Some(BUILD_TAG.to_string());
    }

    None
}

/// Attempt to get current git commit via git command
/// Returns None if git is not available or not a git repo
fn try_git_rev_parse() -> Option<String> {
    let output = Command::new("git")
        .args(&["rev-parse", "HEAD"])
        .output()
        .ok()?;

    if output.status.success() {
        let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !commit.is_empty() {
            return Some(commit);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_detect_build_commit_explicit_override() {
        let commit = detect_build_commit(Some("explicit-commit".to_string()));
        assert_eq!(commit, Some("explicit-commit".to_string()));
    }

    #[test]
    fn test_detect_build_commit_env_var() {
        env::set_var("DETRIX_BUILD_COMMIT", "detrix-commit");
        let commit = detect_build_commit(None);
        assert_eq!(commit, Some("detrix-commit".to_string()));
        env::remove_var("DETRIX_BUILD_COMMIT");
    }

    #[test]
    fn test_detect_build_commit_ci_github() {
        env::set_var("GITHUB_SHA", "github-commit");
        let commit = detect_build_commit(None);
        // May be Some("github-commit") or Some(BUILD_COMMIT) depending on env
        assert!(commit.is_some());
        env::remove_var("GITHUB_SHA");
    }

    #[test]
    fn test_detect_build_tag_explicit_override() {
        let tag = detect_build_tag(Some("v1.0.0".to_string()));
        assert_eq!(tag, Some("v1.0.0".to_string()));
    }

    #[test]
    fn test_priority_detrix_over_ci() {
        env::set_var("DETRIX_BUILD_COMMIT", "detrix-commit");
        env::set_var("GITHUB_SHA", "github-commit");

        let commit = detect_build_commit(None);
        assert_eq!(commit, Some("detrix-commit".to_string()));

        env::remove_var("DETRIX_BUILD_COMMIT");
        env::remove_var("GITHUB_SHA");
    }

    #[test]
    fn test_try_git_rev_parse() {
        // Should not panic, may return Some or None
        let _result = try_git_rev_parse();
    }
}
