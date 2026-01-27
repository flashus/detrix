//! Inspect command - Discover observable variables in source files

use crate::context::ClientContext;
use crate::grpc_client::MetricsClient;
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Run inspect command
pub async fn run(
    ctx: &ClientContext,
    file: PathBuf,
    line: Option<u32>,
    find_variable: Option<String>,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    let mut client = MetricsClient::with_endpoints(&ctx.endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let file_path = file
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid file path"))?;

    let result = client
        .inspect_file(file_path, line, find_variable.as_deref())
        .await
        .context("Failed to inspect file")?;

    // Output based on format
    match format {
        OutputFormat::Json => {
            println!("{}", result.content);
        }
        OutputFormat::Table | OutputFormat::Toon => {
            if quiet {
                // Just output success
                println!("success");
            } else {
                // Pretty print the content (it's JSON)
                println!("{}", result.content);
            }
        }
    }

    formatter.print_success("File inspected");
    Ok(())
}
