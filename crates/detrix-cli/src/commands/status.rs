//! Status command - Show system status

use crate::context::ClientContext;
use crate::grpc_client::{ConnectionsClient, DaemonEndpoints, MetricsClient};
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};

/// Run status command
pub async fn run(
    ctx: &ClientContext,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
    verbose: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    // Use gRPC for status with pre-discovered endpoints
    let mut client = MetricsClient::with_endpoints(&ctx.endpoints)
        .await
        .context("Failed to connect to daemon. Is the daemon running?")?;

    let status = client.get_status().await.context("Failed to get status")?;

    formatter.print_status(&status);

    // In verbose mode, show detailed lists
    if verbose {
        print_verbose_details(&formatter, &ctx.endpoints).await?;
    }

    Ok(())
}

/// Print verbose details: connections and enabled metrics
async fn print_verbose_details(_formatter: &Formatter, endpoints: &DaemonEndpoints) -> Result<()> {
    // Fetch connections
    let connections =
        if let Ok(mut conn_client) = ConnectionsClient::with_endpoints(endpoints).await {
            conn_client.list(false).await.unwrap_or_default()
        } else {
            vec![]
        };

    // Fetch enabled metrics only
    let metrics = if let Ok(mut metrics_client) = MetricsClient::with_endpoints(endpoints).await {
        metrics_client
            .list_metrics(None, true) // enabled_only = true
            .await
            .unwrap_or_default()
    } else {
        vec![]
    };

    // Print connections
    println!();
    println!("Debugger Connections ({})", connections.len());
    println!("─────────────────────────────────────");
    if connections.is_empty() {
        println!("  No debugger connections");
    } else {
        for conn in &connections {
            let status_icon = if conn.status == "connected" {
                "●"
            } else {
                "○"
            };
            println!(
                "  {} {} → {}:{} [{}]",
                status_icon, conn.connection_id, conn.host, conn.port, conn.language
            );
        }
    }

    // Print enabled metrics
    println!();
    println!("Enabled Metrics ({})", metrics.len());
    println!("─────────────────────────────────────");
    if metrics.is_empty() {
        println!("  No enabled metrics");
    } else {
        for m in &metrics {
            println!(
                "  {} @ {}#{} → {}",
                m.name,
                m.location_file,
                m.location_line,
                m.expressions.join(", ")
            );
        }
    }

    Ok(())
}

/// Wake a remote app via its control plane
pub async fn wake(
    ctx: &ClientContext,
    app_url: &str,
    daemon_url: Option<&str>,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    let mut client = MetricsClient::with_endpoints(&ctx.endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let info = client
        .wake(app_url, daemon_url)
        .await
        .context("Failed to wake app")?;

    formatter.print_success(&format!(
        "Wake sent to {}. Status: {}, Connection: {}",
        info.app_url,
        info.status,
        info.connection_id.as_deref().unwrap_or("none")
    ));

    Ok(())
}

/// Sleep a remote app via its control plane
pub async fn sleep(
    ctx: &ClientContext,
    app_url: &str,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    let mut client = MetricsClient::with_endpoints(&ctx.endpoints)
        .await
        .context("Failed to connect to daemon")?;

    client.sleep(app_url).await.context("Failed to sleep app")?;

    formatter.print_success(&format!("Sleep sent to {}", app_url));

    Ok(())
}

/// Disconnect all local debugger adapters
pub async fn disconnect_all(
    ctx: &ClientContext,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    let mut client = MetricsClient::with_endpoints(&ctx.endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let result = client
        .disconnect_all()
        .await
        .context("Failed to disconnect all")?;

    formatter.print_success(&format!(
        "Disconnected. {} adapter(s) stopped.",
        result.adapters_stopped
    ));

    Ok(())
}
