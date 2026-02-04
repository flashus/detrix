//! Client test configuration
//!
//! Language-specific configuration for testing Detrix client implementations.
//! This follows the same pattern as `DapWorkflowConfig` for unified testing
//! across different language clients.

use std::collections::HashMap;
use std::path::PathBuf;

/// Supported client languages
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientLanguage {
    Python,
    Go,
    Rust,
}

impl ClientLanguage {
    /// Display name for the language
    pub fn display_name(&self) -> &'static str {
        match self {
            ClientLanguage::Python => "Python",
            ClientLanguage::Go => "Go",
            ClientLanguage::Rust => "Rust",
        }
    }

    /// Command to spawn the client (first arg of Command::new)
    pub fn spawn_command(&self) -> &'static str {
        match self {
            // Use uv to run Python in the project's virtual environment
            ClientLanguage::Python => "uv",
            ClientLanguage::Go => "go",
            ClientLanguage::Rust => "cargo",
        }
    }

    /// Tool dependency to check before running tests
    pub fn tool_dependency(&self) -> crate::e2e::ToolDependency {
        match self {
            ClientLanguage::Python => crate::e2e::ToolDependency::Debugpy,
            ClientLanguage::Go => crate::e2e::ToolDependency::Delve,
            ClientLanguage::Rust => crate::e2e::ToolDependency::LldbDap,
        }
    }
}

/// Configuration for testing a specific client implementation
///
/// The test uses the main fixture file (e.g., trade_bot_forever.py) with
/// Detrix client enabled via environment variables. This allows testing
/// both debugger attachment and client functionality from the same fixture.
#[derive(Debug, Clone)]
pub struct ClientTestConfig {
    /// Language being tested
    pub language: ClientLanguage,
    /// Path to the test fixture (relative to workspace root)
    pub fixture_path: PathBuf,
    /// Working directory for the spawn command (relative to workspace root)
    /// This is important for tools like `uv` that need to find the project
    pub working_dir: PathBuf,
    /// Arguments to pass to spawn command (before fixture path)
    pub spawn_args_before: Vec<String>,
    /// Arguments to pass after fixture path
    pub spawn_args_after: Vec<String>,
    /// Environment variables to set for enabling Detrix client
    pub env_vars: HashMap<String, String>,
    /// Test name prefix for logging
    pub test_name: String,
}

impl ClientTestConfig {
    /// Create configuration for Python client testing
    ///
    /// Uses `fixtures/python/trade_bot_forever.py` with DETRIX_CLIENT_ENABLED=1
    /// Runs with `uv run python` to use the project's virtual environment
    pub fn python() -> Self {
        let mut env_vars = HashMap::new();
        env_vars.insert("DETRIX_CLIENT_ENABLED".to_string(), "1".to_string());
        // DETRIX_DAEMON_URL and DETRIX_CONTROL_PORT are set at spawn time

        Self {
            language: ClientLanguage::Python,
            fixture_path: PathBuf::from("fixtures/python/trade_bot_forever.py"),
            working_dir: PathBuf::from("clients/python"),
            spawn_args_before: vec!["run".to_string(), "python".to_string()],
            spawn_args_after: vec![],
            env_vars,
            test_name: "Python Client".to_string(),
        }
    }

    /// Create configuration for Go client testing
    ///
    /// Uses `fixtures/go/` with Detrix client enabled.
    /// The fixture imports the Go client via go.mod replace directive.
    pub fn go() -> Self {
        let mut env_vars = HashMap::new();
        env_vars.insert("DETRIX_CLIENT_ENABLED".to_string(), "1".to_string());

        Self {
            language: ClientLanguage::Go,
            // Use the directory containing go.mod - go run will compile and run the package
            fixture_path: PathBuf::from("fixtures/go"),
            // Working dir is fixtures/go where go.mod is located
            working_dir: PathBuf::from("fixtures/go"),
            // go run <dir> compiles and runs the package at <dir>
            spawn_args_before: vec!["run".to_string()],
            spawn_args_after: vec![],
            env_vars,
            test_name: "Go Client".to_string(),
        }
    }

    /// Create configuration for Rust client testing
    ///
    /// Uses `fixtures/rust/Cargo.toml` with Detrix client enabled
    /// The fixture app is a trading bot that prints control plane URL on startup.
    pub fn rust() -> Self {
        let mut env_vars = HashMap::new();
        env_vars.insert("DETRIX_CLIENT_ENABLED".to_string(), "1".to_string());

        Self {
            language: ClientLanguage::Rust,
            // Use Cargo.toml as fixture path since we use --manifest-path
            fixture_path: PathBuf::from("fixtures/rust/Cargo.toml"),
            // Working dir is fixtures/rust for cargo commands
            working_dir: PathBuf::from("fixtures/rust"),
            spawn_args_before: vec!["run".to_string(), "--manifest-path".to_string()],
            // Enable the "client" feature to compile Detrix client code
            spawn_args_after: vec!["--features".to_string(), "client".to_string()],
            env_vars,
            test_name: "Rust Client".to_string(),
        }
    }

    /// Build environment variables for spawning, including daemon URL and control port
    pub fn build_env_vars(&self, daemon_url: &str, control_port: u16) -> HashMap<String, String> {
        let mut env = self.env_vars.clone();
        env.insert("DETRIX_DAEMON_URL".to_string(), daemon_url.to_string());
        env.insert("DETRIX_CONTROL_PORT".to_string(), control_port.to_string());
        env.insert("DETRIX_CLIENT_NAME".to_string(), "test-client".to_string());
        env
    }

    /// Build a pre-build command for compiled languages (Rust, Go).
    ///
    /// Returns `Some((command, args))` if the language requires compilation,
    /// or `None` for interpreted languages (Python). Running the build step
    /// separately ensures compilation time doesn't eat into the control plane
    /// URL detection timeout.
    pub fn build_prebuild_args(
        &self,
        fixture_full_path: &std::path::Path,
    ) -> Option<(String, Vec<String>)> {
        match self.language {
            ClientLanguage::Rust => {
                // cargo build --manifest-path <path> --features client
                let mut args = vec!["build".to_string(), "--manifest-path".to_string()];
                args.push(fixture_full_path.to_string_lossy().to_string());
                args.extend(self.spawn_args_after.clone());
                Some((self.language.spawn_command().to_string(), args))
            }
            ClientLanguage::Go => {
                // go build <fixture_path>
                let mut args = vec!["build".to_string()];
                args.push(fixture_full_path.to_string_lossy().to_string());
                Some((self.language.spawn_command().to_string(), args))
            }
            ClientLanguage::Python => None,
        }
    }

    /// Build the full command arguments for spawning
    ///
    /// Returns (command, args) where args includes:
    /// - spawn_args_before
    /// - fixture path
    /// - spawn_args_after
    pub fn build_spawn_args(&self, fixture_full_path: &std::path::Path) -> (String, Vec<String>) {
        let mut args = self.spawn_args_before.clone();
        args.push(fixture_full_path.to_string_lossy().to_string());
        args.extend(self.spawn_args_after.clone());

        (self.language.spawn_command().to_string(), args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_config() {
        let config = ClientTestConfig::python();
        assert_eq!(config.language, ClientLanguage::Python);
        assert!(config
            .fixture_path
            .to_string_lossy()
            .contains("trade_bot_forever"));
        assert!(config.env_vars.contains_key("DETRIX_CLIENT_ENABLED"));
    }

    #[test]
    fn test_build_env_vars() {
        let config = ClientTestConfig::python();
        let env = config.build_env_vars("http://127.0.0.1:8090", 9000);

        assert_eq!(env.get("DETRIX_CLIENT_ENABLED"), Some(&"1".to_string()));
        assert_eq!(
            env.get("DETRIX_DAEMON_URL"),
            Some(&"http://127.0.0.1:8090".to_string())
        );
        assert_eq!(env.get("DETRIX_CONTROL_PORT"), Some(&"9000".to_string()));
    }

    #[test]
    fn test_build_spawn_args() {
        let config = ClientTestConfig::python();
        let (cmd, args) = config.build_spawn_args(std::path::Path::new(
            "/workspace/fixtures/python/trade_bot_forever.py",
        ));

        // Python uses uv to run in the project's virtual environment
        assert_eq!(cmd, "uv");
        // Args: ["run", "python", "/path/to/trade_bot_forever.py"]
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "run");
        assert_eq!(args[1], "python");
        assert!(args[2].contains("trade_bot_forever.py"));
    }

    #[test]
    fn test_go_config() {
        let config = ClientTestConfig::go();
        assert_eq!(config.language, ClientLanguage::Go);
        assert!(config.env_vars.contains_key("DETRIX_CLIENT_ENABLED"));
    }
}
