//! Daemon configuration management
//!
//! Supports saved daemon configurations in ~/.detrix/daemons.toml
//! for easy switching between local and cloud Detrix daemons.

use crate::paths;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for a single daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Alias for this daemon (e.g., "local", "production")
    pub alias: String,
    /// Daemon URL (e.g., "http://localhost:8090")
    pub url: String,
    /// Whether this is a production daemon (requires confirmation before switching)
    #[serde(default)]
    pub is_production: bool,
}

/// Collection of saved daemon configurations
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonsConfig {
    /// List of saved daemons
    #[serde(default)]
    pub daemon: Vec<DaemonConfig>,
}

impl DaemonsConfig {
    /// Load daemon configurations from ~/.detrix/daemons.toml
    ///
    /// Returns an empty config if the file doesn't exist.
    pub fn load_from_home() -> Result<Self, Box<dyn std::error::Error>> {
        let path = Self::default_path()?;

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)?;
        let config: DaemonsConfig = toml::from_str(&content)?;
        Ok(config)
    }

    /// Get the default path for daemons.toml (~/.detrix/daemons.toml)
    pub fn default_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
        let path = paths::detrix_home().join("daemons.toml");
        Ok(path)
    }

    /// Find a daemon by alias
    pub fn find_by_alias(&self, alias: &str) -> Option<&DaemonConfig> {
        self.daemon.iter().find(|d| d.alias == alias)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_daemons_config() {
        let toml = r#"
[[daemon]]
alias = "local"
url = "http://localhost:8090"
is_production = false

[[daemon]]
alias = "production"
url = "https://prod.example.com:8090"
is_production = true
"#;

        let config: DaemonsConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.daemon.len(), 2);

        let local = &config.daemon[0];
        assert_eq!(local.alias, "local");
        assert_eq!(local.url, "http://localhost:8090");
        assert!(!local.is_production);

        let prod = &config.daemon[1];
        assert_eq!(prod.alias, "production");
        assert_eq!(prod.url, "https://prod.example.com:8090");
        assert!(prod.is_production);
    }

    #[test]
    fn test_find_by_alias() {
        let toml = r#"
[[daemon]]
alias = "local"
url = "http://localhost:8090"

[[daemon]]
alias = "cloud"
url = "http://192.168.0.10:8090"
"#;

        let config: DaemonsConfig = toml::from_str(toml).unwrap();

        let local = config.find_by_alias("local");
        assert!(local.is_some());
        assert_eq!(local.unwrap().url, "http://localhost:8090");

        let cloud = config.find_by_alias("cloud");
        assert!(cloud.is_some());
        assert_eq!(cloud.unwrap().url, "http://192.168.0.10:8090");

        let missing = config.find_by_alias("missing");
        assert!(missing.is_none());
    }

    #[test]
    fn test_production_flag_defaults_to_false() {
        let toml = r#"
[[daemon]]
alias = "test"
url = "http://localhost:8090"
"#;

        let config: DaemonsConfig = toml::from_str(toml).unwrap();
        assert!(!config.daemon[0].is_production);
    }
}
