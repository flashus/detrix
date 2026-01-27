//! Event command - Query and stream metric events

use crate::context::ClientContext;
use crate::grpc_client::{DaemonEndpoints, MetricsClient, StreamingClient};
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum EventAction {
    /// Query historical events
    Query(QueryArgs),
    /// Get latest event for a metric
    Latest {
        /// Metric name
        metric_name: String,
    },
}

#[derive(Args)]
pub struct QueryArgs {
    /// Filter by metric name
    #[arg(long, short)]
    metric: Option<String>,

    /// Maximum number of events to return
    #[arg(long, short, default_value = "100")]
    limit: u32,
}

/// Run event command
pub async fn run(
    ctx: &ClientContext,
    action: EventAction,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    match action {
        EventAction::Query(args) => query(&ctx.endpoints, args, &formatter).await,
        EventAction::Latest { metric_name } => {
            latest(&ctx.endpoints, &metric_name, &formatter).await
        }
    }
}

async fn query(endpoints: &DaemonEndpoints, args: QueryArgs, formatter: &Formatter) -> Result<()> {
    // Get metric IDs first if filtering by name
    let metric_ids = if let Some(ref metric_name) = args.metric {
        let mut metrics_client = MetricsClient::with_endpoints(endpoints)
            .await
            .context("Failed to connect to daemon")?;
        let metrics = metrics_client
            .list_metrics(None, false)
            .await
            .context("Failed to list metrics")?;
        metrics
            .iter()
            .filter(|m| m.name.contains(metric_name))
            .map(|m| m.metric_id)
            .collect()
    } else {
        vec![]
    };

    let mut client = StreamingClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let events = client
        .query_metrics(metric_ids, Some(args.limit))
        .await
        .context("Failed to query events")?;

    formatter.print_events(&events);

    Ok(())
}

async fn latest(
    endpoints: &DaemonEndpoints,
    metric_name: &str,
    formatter: &Formatter,
) -> Result<()> {
    // First get the metric ID
    let mut metrics_client = MetricsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;
    let metric = metrics_client
        .get_metric(metric_name)
        .await
        .context("Failed to find metric")?;

    let mut client = StreamingClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let events = client
        .query_metrics(vec![metric.metric_id], Some(1))
        .await
        .context("Failed to query events")?;

    if events.is_empty() {
        formatter.print_success(&format!("No events for metric '{}'", metric_name));
    } else {
        formatter.print_events(&events);
    }

    Ok(())
}
