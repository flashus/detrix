//! Group command - Manage metric groups

use crate::context::ClientContext;
use crate::grpc_client::{DaemonEndpoints, MetricsClient};
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum GroupAction {
    /// List all groups
    List,
    /// Enable all metrics in a group
    Enable {
        /// Group name
        name: String,
    },
    /// Disable all metrics in a group
    Disable {
        /// Group name
        name: String,
    },
    /// List metrics in a group
    Metrics {
        /// Group name
        name: String,
    },
}

/// Run group command
pub async fn run(
    ctx: &ClientContext,
    action: GroupAction,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    match action {
        GroupAction::List => list(&ctx.endpoints, &formatter).await,
        GroupAction::Enable { name } => enable(&ctx.endpoints, &name, &formatter).await,
        GroupAction::Disable { name } => disable(&ctx.endpoints, &name, &formatter).await,
        GroupAction::Metrics { name } => metrics(&ctx.endpoints, &name, &formatter).await,
    }
}

async fn list(endpoints: &DaemonEndpoints, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let groups = client
        .list_groups()
        .await
        .context("Failed to list groups")?;

    formatter.print_groups(&groups);

    Ok(())
}

async fn enable(endpoints: &DaemonEndpoints, name: &str, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let count = client
        .enable_group(name)
        .await
        .context("Failed to enable group")?;

    formatter.print_success(&format!("Enabled {} metrics in group '{}'", count, name));

    Ok(())
}

async fn disable(endpoints: &DaemonEndpoints, name: &str, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let count = client
        .disable_group(name)
        .await
        .context("Failed to disable group")?;

    formatter.print_success(&format!("Disabled {} metrics in group '{}'", count, name));

    Ok(())
}

async fn metrics(endpoints: &DaemonEndpoints, name: &str, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let metrics = client
        .list_metrics(Some(name.to_string()), false)
        .await
        .context("Failed to list metrics in group")?;

    formatter.print_metrics(&metrics);

    Ok(())
}
