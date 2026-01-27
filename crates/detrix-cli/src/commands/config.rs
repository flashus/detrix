//! Config command - Manage configuration

use crate::context::ClientContext;
use crate::grpc_client::{DaemonEndpoints, MetricsClient};
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use detrix_config::load_config;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Get current configuration
    Get(GetArgs),
    /// Update configuration
    Update(UpdateArgs),
    /// Validate configuration file or content
    Validate(ValidateArgs),
    /// Reload configuration from disk
    Reload,
}

#[derive(Args)]
pub struct UpdateArgs {
    /// TOML configuration string
    #[arg(long)]
    pub toml: Option<String>,

    /// Read configuration from file
    #[arg(long, short)]
    pub file: Option<PathBuf>,

    /// Read configuration from stdin
    #[arg(long)]
    pub stdin: bool,

    /// Merge with existing configuration (default: replace)
    #[arg(long)]
    pub merge: bool,

    /// Persist changes to disk
    #[arg(long)]
    pub persist: bool,
}

#[derive(Args)]
pub struct GetArgs {
    /// Get specific config key (e.g., "storage.path")
    #[arg(long)]
    key: Option<String>,
}

#[derive(Args)]
pub struct ValidateArgs {
    /// Path to configuration file (defaults to detrix.toml)
    #[arg(long, short)]
    pub file: Option<PathBuf>,

    /// TOML content to validate (via daemon)
    #[arg(long, conflicts_with = "file")]
    pub content: Option<String>,

    /// Read TOML content from stdin (via daemon)
    #[arg(long, conflicts_with_all = ["file", "content"])]
    pub stdin: bool,
}

/// Run config command
pub async fn run(
    ctx: &ClientContext,
    action: ConfigAction,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    match action {
        ConfigAction::Get(args) => get(&ctx.endpoints, args, &formatter).await,
        ConfigAction::Update(args) => update(&ctx.endpoints, args, &formatter).await,
        ConfigAction::Validate(args) => {
            validate(&ctx.endpoints, ctx.config_path.as_path(), args, &formatter).await
        }
        ConfigAction::Reload => reload(&ctx.endpoints, &formatter).await,
    }
}

async fn get(endpoints: &DaemonEndpoints, _args: GetArgs, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let config_toml = client
        .get_config()
        .await
        .context("Failed to get configuration")?;

    formatter.print_config(&config_toml);

    Ok(())
}

async fn update(
    endpoints: &DaemonEndpoints,
    args: UpdateArgs,
    formatter: &Formatter,
) -> Result<()> {
    use std::io::Read;

    // Read config from one of the sources
    let config_toml = if args.stdin {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("Failed to read config from stdin")?;
        buffer
    } else if let Some(file) = &args.file {
        std::fs::read_to_string(file)
            .with_context(|| format!("Failed to read config file: {}", file.display()))?
    } else if let Some(toml) = &args.toml {
        toml.clone()
    } else {
        anyhow::bail!("Either --toml, --file, or --stdin must be specified");
    };

    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    client
        .update_config(&config_toml, args.merge, args.persist)
        .await
        .context("Failed to update configuration")?;

    formatter.print_success("Configuration updated");

    Ok(())
}

async fn validate(
    endpoints: &DaemonEndpoints,
    default_config_path: &std::path::Path,
    args: ValidateArgs,
    formatter: &Formatter,
) -> Result<()> {
    use std::io::Read;

    // Check if we're validating content (via daemon) or file (local)
    let content_to_validate = if args.stdin {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("Failed to read config from stdin")?;
        Some(buffer)
    } else {
        args.content
    };

    if let Some(toml_content) = content_to_validate {
        // Remote validation via daemon
        let mut client = MetricsClient::with_endpoints(endpoints)
            .await
            .context("Failed to connect to daemon")?;

        let result = client
            .validate_config(&toml_content)
            .await
            .context("Failed to validate config via daemon")?;

        if result.valid {
            formatter.print_validation(true, "Configuration is valid");
        } else {
            let mut message = String::from("Configuration is invalid");
            for error in &result.errors {
                message.push_str(&format!("\n  - {}", error));
            }
            formatter.print_validation(false, &message);
        }

        // Show warnings even if valid
        for warning in &result.warnings {
            eprintln!("Warning: {}", warning);
        }
    } else {
        // Local file validation (no daemon required)
        let path = args
            .file
            .unwrap_or_else(|| default_config_path.to_path_buf());

        match load_config(&path) {
            Ok(_config) => {
                formatter.print_validation(
                    true,
                    &format!("Configuration at {} is valid", path.display()),
                );
            }
            Err(detrix_config::ConfigError::NotFound(p)) => {
                formatter.print_validation(
                    false,
                    &format!(
                        "Config file not found: {}\nRun 'detrix init' to create default configuration.",
                        p.display()
                    ),
                );
            }
            Err(detrix_config::ConfigError::ParseError(e)) => {
                formatter.print_validation(false, &format!("Parse error: {}", e));
            }
            Err(detrix_config::ConfigError::ValidationError(e)) => {
                formatter.print_validation(false, &format!("Validation error: {}", e));
            }
            Err(e) => {
                formatter.print_validation(false, &format!("Error: {}", e));
            }
        }
    }

    Ok(())
}

async fn reload(endpoints: &DaemonEndpoints, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    client
        .reload_config()
        .await
        .context("Failed to reload configuration")?;

    formatter.print_success("Configuration reloaded");

    Ok(())
}
