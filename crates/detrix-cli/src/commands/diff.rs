//! Diff command - Parse git diff and create metrics from debug statements
//!
//! Uses the shared diff parser from detrix-api::common::diff_parser

use crate::context::ClientContext;
use crate::grpc_client::{AddMetricParams, MetricsClient};
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};
use clap::Args;
use detrix_api::common::diff_parser::parse_diff;
use std::io::{self, Read};
use std::path::PathBuf;

#[derive(Args)]
pub struct DiffArgs {
    /// Read diff from stdin (e.g., git diff | detrix diff --stdin)
    #[arg(long)]
    pub stdin: bool,

    /// Read diff from file
    #[arg(long, short)]
    pub file: Option<PathBuf>,

    /// Connection ID to use for created metrics
    #[arg(long, short)]
    pub connection: Option<String>,

    /// Group name for created metrics
    #[arg(long, short)]
    pub group: Option<String>,

    /// Show what would be done without actually creating metrics
    #[arg(long)]
    pub dry_run: bool,
}

/// Run diff command
pub async fn run(
    ctx: &ClientContext,
    args: DiffArgs,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    // Read diff content
    let diff_content = if args.stdin {
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .context("Failed to read diff from stdin")?;
        buffer
    } else if let Some(file) = &args.file {
        std::fs::read_to_string(file)
            .with_context(|| format!("Failed to read diff from file: {}", file.display()))?
    } else {
        anyhow::bail!("Either --stdin or --file must be specified");
    };

    // Parse the diff using shared parser from detrix-api
    let parse_result = parse_diff(&diff_content);

    if !parse_result.has_debug_statements() {
        formatter.print_success("No debug statements found in diff");
        return Ok(());
    }

    // Report unparseable lines if any
    if !parse_result.unparseable.is_empty() && !formatter.quiet {
        eprintln!(
            "Warning: {} line(s) could not be parsed:",
            parse_result.unparseable.len()
        );
        for line in &parse_result.unparseable {
            eprintln!("  {}:{}: {}", line.file, line.line, line.reason);
        }
    }

    if args.dry_run {
        // Show what would be done
        println!(
            "Would create {} metric(s) from debug statements:",
            parse_result.parsed.len()
        );
        for line in &parse_result.parsed {
            println!(
                "  {}:{} -> {} (confidence: {:?})",
                line.file, line.line, line.expression, line.confidence
            );
        }
        return Ok(());
    }

    // Check for connection
    let connection_id = match args.connection {
        Some(id) => id,
        None => {
            anyhow::bail!("Connection ID is required. Use --connection to specify one.")
        }
    };

    // Create metrics using pre-discovered endpoints
    let mut client = MetricsClient::with_endpoints(&ctx.endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let mut created = 0;
    let mut failed = 0;

    for line in &parse_result.parsed {
        let name = generate_metric_name(&line.file, line.line, &line.expression);
        let location = format!("{}#{}", line.file, line.line);

        let params = AddMetricParams {
            name: name.clone(),
            location,
            line: Some(line.line),
            expressions: vec![line.expression.clone()],
            connection_id: connection_id.clone(),
            group: args.group.clone(),
            enabled: true,
            replace: true,
        };

        match client.add_metric(params).await {
            Ok(_) => {
                created += 1;
                if !formatter.quiet {
                    println!("Created: {} -> {}", name, line.expression);
                }
            }
            Err(e) => {
                failed += 1;
                if !formatter.quiet {
                    eprintln!("Failed to create {}: {}", name, e);
                }
            }
        }
    }

    formatter.print_success(&format!("Created {} metric(s), {} failed", created, failed));

    Ok(())
}

/// Generate a metric name from file and line for diff-created metrics.
///
/// Uses shared `generate_metric_name_with_prefix` with "diff" prefix.
/// Format: `diff_filename_lineNumber` (e.g., `diff_auth_py_42`)
fn generate_metric_name(file: &str, line: u32, _expression: &str) -> String {
    detrix_api::common::generate_metric_name_with_prefix("diff", file, line)
}
