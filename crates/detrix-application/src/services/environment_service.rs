//! Environment validation service
//!
//! Validates that the runtime environment meets compatibility requirements
//! before spawning debug adapters.

use crate::error::{ConfigError, EnvValidationOptionExt, EnvValidationResultExt};
use crate::Result;
use detrix_config::CompatibilityConfig;
use std::process::Command;

/// Service for validating runtime environment requirements
pub struct EnvironmentService {
    config: CompatibilityConfig,
}

impl EnvironmentService {
    /// Create a new environment service with the given compatibility config
    pub fn new(config: CompatibilityConfig) -> Self {
        Self { config }
    }

    /// Check if Python meets the minimum version requirement
    ///
    /// Returns the detected Python version on success, or an error if:
    /// - Python is not found
    /// - Python version is below minimum
    /// - Python version is above maximum (if configured)
    pub fn check_python(&self) -> Result<String> {
        let output = Command::new("python3")
            .args(["--version"])
            .output()
            .env_context("Failed to check Python version")?;

        if !output.status.success() {
            return Err(ConfigError::EnvironmentValidation(
                "Python3 not found. Install Python 3.8+ to use debugpy adapter.".to_string(),
            )
            .into());
        }

        let version_output = String::from_utf8_lossy(&output.stdout);
        let version = parse_python_version(&version_output).env_required(format!(
            "Could not parse Python version from: {}",
            version_output
        ))?;

        let min_version = parse_semver(&self.config.python_min).env_required(format!(
            "Invalid python_min config: {}",
            self.config.python_min
        ))?;

        if version < min_version {
            return Err(ConfigError::EnvironmentValidation(format!(
                "Python {} is below minimum required version {}",
                format_version(&version),
                self.config.python_min
            ))
            .into());
        }

        if !self.config.python_max.is_empty() {
            let max_version = parse_semver(&self.config.python_max).env_required(format!(
                "Invalid python_max config: {}",
                self.config.python_max
            ))?;

            if version > max_version {
                return Err(ConfigError::EnvironmentValidation(format!(
                    "Python {} is above maximum supported version {}",
                    format_version(&version),
                    self.config.python_max
                ))
                .into());
            }
        }

        Ok(format_version(&version))
    }

    /// Check if Go meets the minimum version requirement
    ///
    /// Returns the detected Go version on success, or an error if:
    /// - Go is not found
    /// - Go version is below minimum
    pub fn check_go(&self) -> Result<String> {
        if self.config.go_min.is_empty() {
            return Ok("not configured".to_string());
        }

        let output = Command::new("go")
            .args(["version"])
            .output()
            .env_context("Failed to check Go version")?;

        if !output.status.success() {
            return Err(ConfigError::EnvironmentValidation(
                "Go not found. Install Go 1.18+ to use delve adapter.".to_string(),
            )
            .into());
        }

        let version_output = String::from_utf8_lossy(&output.stdout);
        let version = parse_go_version(&version_output).env_required(format!(
            "Could not parse Go version from: {}",
            version_output
        ))?;

        let min_version = parse_semver(&self.config.go_min)
            .env_required(format!("Invalid go_min config: {}", self.config.go_min))?;

        if version < min_version {
            return Err(ConfigError::EnvironmentValidation(format!(
                "Go {} is below minimum required version {}",
                format_version(&version),
                self.config.go_min
            ))
            .into());
        }

        Ok(format_version(&version))
    }

    /// Check all configured language requirements
    ///
    /// Returns a summary of detected versions
    pub fn check_all(&self) -> EnvironmentCheckResult {
        let python = self.check_python();
        let go = self.check_go();

        EnvironmentCheckResult { python, go }
    }
}

/// Result of checking all environment requirements
#[derive(Debug)]
pub struct EnvironmentCheckResult {
    pub python: Result<String>,
    pub go: Result<String>,
}

impl EnvironmentCheckResult {
    /// Check if all requirements are met
    pub fn is_ok(&self) -> bool {
        self.python.is_ok() && self.go.is_ok()
    }

    /// Get a summary of the check results
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();

        match &self.python {
            Ok(v) => lines.push(format!("Python: {} ✓", v)),
            Err(e) => lines.push(format!("Python: {} ✗", e)),
        }

        match &self.go {
            Ok(v) => lines.push(format!("Go: {} ✓", v)),
            Err(e) => lines.push(format!("Go: {} ✗", e)),
        }

        lines.join("\n")
    }
}

// Simple version tuple (major, minor, patch)
type Version = (u32, u32, u32);

fn parse_semver(s: &str) -> Option<Version> {
    let parts: Vec<&str> = s.trim().split('.').collect();
    if parts.is_empty() {
        return None;
    }

    let major = parts.first()?.parse().ok()?;
    let minor = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let patch = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    Some((major, minor, patch))
}

fn parse_python_version(output: &str) -> Option<Version> {
    // Format: "Python 3.11.4"
    let version_str = output.trim().strip_prefix("Python ")?;
    parse_semver(version_str)
}

fn parse_go_version(output: &str) -> Option<Version> {
    // Format: "go version go1.21.5 darwin/arm64"
    let parts: Vec<&str> = output.split_whitespace().collect();
    let go_version = parts.get(2)?;
    let version_str = go_version.strip_prefix("go")?;
    parse_semver(version_str)
}

fn format_version(v: &Version) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_python_version() {
        assert_eq!(parse_python_version("Python 3.11.4"), Some((3, 11, 4)));
        assert_eq!(parse_python_version("Python 3.8.0"), Some((3, 8, 0)));
        assert_eq!(parse_python_version("Python 3.12"), Some((3, 12, 0)));
    }

    #[test]
    fn test_parse_go_version() {
        assert_eq!(
            parse_go_version("go version go1.21.5 darwin/arm64"),
            Some((1, 21, 5))
        );
        assert_eq!(
            parse_go_version("go version go1.18.0 linux/amd64"),
            Some((1, 18, 0))
        );
    }

    #[test]
    fn test_parse_semver() {
        assert_eq!(parse_semver("3.8"), Some((3, 8, 0)));
        assert_eq!(parse_semver("3.11.4"), Some((3, 11, 4)));
        assert_eq!(parse_semver("1.21"), Some((1, 21, 0)));
    }

    #[test]
    fn test_version_comparison() {
        assert!((3, 11, 0) > (3, 8, 0));
        assert!((3, 8, 0) < (3, 13, 0));
        assert!((1, 21, 5) > (1, 18, 0));
    }

    #[test]
    fn test_check_python_in_range() {
        let config = CompatibilityConfig {
            python_min: "3.8".to_string(),
            python_max: "3.13".to_string(),
            go_min: "1.18".to_string(),
        };
        let service = EnvironmentService::new(config);

        // This test requires Python to be installed
        // It will pass if Python 3.8-3.13 is available
        let result = service.check_python();
        // Just check it doesn't panic - actual version depends on system
        let _ = result;
    }
}
