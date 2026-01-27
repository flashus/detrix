//! Usage command - Show MCP tool usage statistics

use crate::context::ClientContext;
use crate::grpc_client::MetricsClient;
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};

/// Run usage command - shows MCP tool usage statistics
pub async fn run(
    ctx: &ClientContext,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    let mut client = MetricsClient::with_endpoints(&ctx.endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let usage = client
        .get_mcp_usage()
        .await
        .context("Failed to get MCP usage statistics")?;

    if formatter.quiet {
        // In quiet mode, just print success rate
        println!("{:.1}%", usage.success_rate * 100.0);
        return Ok(());
    }

    // Print formatted usage stats
    match formatter.format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&usage)?);
        }
        _ => {
            println!("MCP Usage Statistics");
            println!("{}", "-".repeat(40));
            println!("Total Calls:    {}", usage.total_calls);
            println!("Successful:     {}", usage.total_success);
            println!("Failed:         {}", usage.total_errors);
            println!("Success Rate:   {:.1}%", usage.success_rate * 100.0);
            println!("Avg Latency:    {:.2}ms", usage.avg_latency_ms);
            println!();

            if !usage.top_errors.is_empty() {
                println!("Top Errors:");
                for error in &usage.top_errors {
                    println!("  {} ({}x)", error.code, error.count);
                }
                println!();
            }

            println!("Workflow Stats:");
            println!("  Correct workflows: {}", usage.workflow.correct_workflows);
            println!("  Skipped inspect:   {}", usage.workflow.skipped_inspect);
            println!("  No connection:     {}", usage.workflow.no_connection);
            println!(
                "  Adherence rate:    {:.1}%",
                usage.workflow.adherence_rate * 100.0
            );
        }
    }

    Ok(())
}
