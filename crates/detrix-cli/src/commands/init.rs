//! Init command - Initialize Detrix configuration
//!
//! Creates configuration file if it doesn't exist.
//! By default, creates a minimal config. Use --full for all options.
//!
//! Path resolution priority:
//! 1. `--path <custom>` (explicit CLI argument)
//! 2. `DETRIX_HOME` environment variable
//! 3. `~/detrix/` (default)

use anyhow::{Context, Result};
use detrix_config::{
    constants::ENV_DETRIX_HOME,
    create_config,
    paths::{default_config_path, DEFAULT_CONFIG_FILENAME},
    ConfigTemplate,
};
use std::path::PathBuf;

/// Run init command
///
/// Creates configuration file at the specified or default location.
///
/// # Arguments
/// * `path` - Optional custom path for config file
/// * `force` - Whether to overwrite existing config
/// * `full` - Whether to create full config with all options
pub fn run(path: Option<PathBuf>, force: bool, full: bool) -> Result<()> {
    // Resolve config path using priority order
    let config_path = resolve_init_path(path.as_ref());

    // Determine which template to use
    let template = if full {
        ConfigTemplate::Full
    } else {
        ConfigTemplate::Minimal
    };

    // Check for DETRIX_HOME conflict warning
    if let Some(explicit_path) = &path {
        warn_if_detrix_home_conflict(explicit_path);
    }

    // Check if file already exists
    if config_path.exists() && !force {
        println!("Configuration file already exists at:");
        println!("  {}", config_path.display());
        println!();
        println!("Use --force to overwrite with default configuration.");
        return Ok(());
    }

    // If force and file exists, remove it first
    if config_path.exists() && force {
        std::fs::remove_file(&config_path).context(format!(
            "Failed to remove existing config at {}",
            config_path.display()
        ))?;
        println!("Removed existing configuration file.");
    }

    // Create the config file
    let created_path = create_config(&config_path, template).context(format!(
        "Failed to create config at {}",
        config_path.display()
    ))?;

    let config_type = if full { "full" } else { "minimal" };
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Created {} configuration file:", config_type);
    println!(
        "     {}",
        created_path
            .canonicalize()
            .unwrap_or_else(|_| created_path.clone())
            .display()
    );
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    if !full {
        println!("This is a minimal config with essential settings.");
        println!("For all options, run: detrix init --full --force");
        println!();
    }

    println!("You can now start the daemon with:");
    println!("  detrix daemon start");
    println!();
    println!("Or run MCP for LLM integration:");
    println!("  detrix mcp");

    Ok(())
}

/// Resolve config path for init command
///
/// Priority:
/// 1. Explicit --path argument
/// 2. DETRIX_HOME environment variable
/// 3. Default ~/detrix/detrix.toml
fn resolve_init_path(explicit_path: Option<&PathBuf>) -> PathBuf {
    // 1. Explicit path takes highest priority
    if let Some(path) = explicit_path {
        return path.clone();
    }

    // 2. Check DETRIX_HOME environment variable
    if let Ok(home) = std::env::var(ENV_DETRIX_HOME) {
        let home_path = PathBuf::from(home);
        return home_path.join(DEFAULT_CONFIG_FILENAME);
    }

    // 3. Default location
    default_config_path()
}

/// Warn if --path conflicts with DETRIX_HOME environment variable
fn warn_if_detrix_home_conflict(explicit_path: &std::path::Path) {
    if let Ok(detrix_home_env) = std::env::var(ENV_DETRIX_HOME) {
        let env_config_path = PathBuf::from(&detrix_home_env).join(DEFAULT_CONFIG_FILENAME);

        // Normalize paths for comparison
        let explicit_normalized = explicit_path
            .canonicalize()
            .unwrap_or_else(|_| explicit_path.to_path_buf());
        let env_normalized = env_config_path
            .canonicalize()
            .unwrap_or_else(|_| env_config_path.clone());

        if explicit_normalized != env_normalized {
            eprintln!("Warning: --path differs from DETRIX_HOME environment variable.");
            eprintln!("  --path:      {}", explicit_path.display());
            eprintln!("  DETRIX_HOME: {}", detrix_home_env);
            eprintln!("  Using --path.");
            eprintln!();
        }
    }
}
