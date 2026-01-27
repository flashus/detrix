//! Metric command - Manage metrics (observation points)

use crate::context::ClientContext;
use crate::grpc_client::{AddMetricParams, DaemonEndpoints, MetricsClient};
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum MetricAction {
    /// Add a new metric
    Add(AddArgs),
    /// List all metrics
    List(ListArgs),
    /// Get metric details
    Get {
        /// Metric name
        name: String,
    },
    /// Remove a metric
    Remove {
        /// Metric name
        name: String,
    },
    /// Enable a metric
    Enable {
        /// Metric name
        name: String,
    },
    /// Disable a metric
    Disable {
        /// Metric name
        name: String,
    },
}

#[derive(Args)]
pub struct AddArgs {
    /// Metric name
    name: String,

    /// Location in file (file.py#123 or @file.py#123)
    #[arg(long, short)]
    location: String,

    /// Expression to evaluate at the location
    #[arg(long, short)]
    expression: String,

    /// Connection ID to use for this metric
    #[arg(long, short = 'C')]
    connection: String,

    /// Optional group name
    #[arg(long, short)]
    group: Option<String>,

    /// Replace existing metric at this location
    #[arg(long)]
    replace: bool,

    /// Start metric enabled (default: true)
    #[arg(long, default_value = "true")]
    enabled: bool,
}

#[derive(Args)]
pub struct ListArgs {
    /// Filter by group
    #[arg(long, short)]
    group: Option<String>,

    /// Only show enabled metrics
    #[arg(long)]
    enabled: bool,
}

/// Run metric command
pub async fn run(
    ctx: &ClientContext,
    action: MetricAction,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    match action {
        MetricAction::Add(args) => add(&ctx.endpoints, args, &formatter).await,
        MetricAction::List(args) => list(&ctx.endpoints, args, &formatter).await,
        MetricAction::Get { name } => get(&ctx.endpoints, &name, &formatter).await,
        MetricAction::Remove { name } => remove(&ctx.endpoints, &name, &formatter).await,
        MetricAction::Enable { name } => toggle(&ctx.endpoints, &name, true, &formatter).await,
        MetricAction::Disable { name } => toggle(&ctx.endpoints, &name, false, &formatter).await,
    }
}

async fn add(endpoints: &DaemonEndpoints, args: AddArgs, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let params = AddMetricParams {
        name: args.name.clone(),
        location: args.location,
        line: None,
        expression: args.expression,
        connection_id: args.connection,
        group: args.group,
        enabled: args.enabled,
        replace: args.replace,
    };

    let metric = client
        .add_metric(params)
        .await
        .context("Failed to add metric")?;

    if formatter.quiet {
        formatter.print_id(&metric.name);
    } else {
        formatter.print_metric(&metric);
    }

    Ok(())
}

async fn list(endpoints: &DaemonEndpoints, args: ListArgs, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let metrics = client
        .list_metrics(args.group, args.enabled)
        .await
        .context("Failed to list metrics")?;

    formatter.print_metrics(&metrics);

    Ok(())
}

async fn get(endpoints: &DaemonEndpoints, name: &str, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let metric = client
        .get_metric(name)
        .await
        .context("Failed to get metric")?;

    formatter.print_metric(&metric);

    Ok(())
}

async fn remove(endpoints: &DaemonEndpoints, name: &str, formatter: &Formatter) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    client
        .remove_metric(name)
        .await
        .context("Failed to remove metric")?;

    formatter.print_success(&format!("Metric '{}' removed", name));

    Ok(())
}

async fn toggle(
    endpoints: &DaemonEndpoints,
    name: &str,
    enabled: bool,
    formatter: &Formatter,
) -> Result<()> {
    let mut client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    client
        .toggle_metric(name, enabled)
        .await
        .context(if enabled {
            "Failed to enable metric"
        } else {
            "Failed to disable metric"
        })?;

    let action = if enabled { "enabled" } else { "disabled" };
    formatter.print_success(&format!("Metric '{}' {}", name, action));

    Ok(())
}
