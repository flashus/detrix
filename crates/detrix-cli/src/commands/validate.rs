//! Validate command - Validate expressions for safety

use crate::context::ClientContext;
use crate::grpc_client::MetricsClient;
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};

/// Run validate command
pub async fn run(
    ctx: &ClientContext,
    expression: String,
    language: String,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    let mut client = MetricsClient::with_endpoints(&ctx.endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let result = client
        .validate_expression(&expression, &language)
        .await
        .context("Failed to validate expression")?;

    if result.is_safe {
        formatter.print_validation(true, &format!("Expression '{}' is safe", expression));
    } else {
        let reason = if result.violations.is_empty() {
            "Unknown reason".to_string()
        } else {
            result.violations.join("; ")
        };
        formatter.print_validation(
            false,
            &format!("Expression '{}' is unsafe: {}", expression, reason),
        );
    }

    Ok(())
}
