//! System-event command - Query system events (crashes, connection changes, etc.)

use crate::context::ClientContext;
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Subcommand)]
pub enum SystemEventAction {
    /// Query system events
    Query(QueryArgs),
}

#[derive(Args)]
pub struct QueryArgs {
    /// Filter by event type (connection, metric, adapter, error, all)
    #[arg(long, short = 't', default_value = "all")]
    pub event_type: String,

    /// Maximum number of events to return
    #[arg(long, short, default_value = "50")]
    pub limit: u32,

    /// Only show unacknowledged events
    #[arg(long)]
    pub unacked: bool,
}

/// System event from API
#[derive(Debug, Serialize, Deserialize)]
pub struct SystemEvent {
    pub id: i64,
    pub event_type: String,
    pub severity: String,
    pub message: String,
    pub timestamp: String,
    pub acknowledged: bool,
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

/// Run system-event command
pub async fn run(
    ctx: &ClientContext,
    action: SystemEventAction,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    match action {
        SystemEventAction::Query(args) => query(ctx, args, &formatter).await,
    }
}

async fn query(ctx: &ClientContext, args: QueryArgs, formatter: &Formatter) -> Result<()> {
    // Use endpoints from context instead of reading PID file
    let host = &ctx.endpoints.host;
    let port = ctx.endpoints.http_port;

    // Build query URL
    let mut url = format!(
        "http://{}:{}/api/v1/mcp/tools/query_system_events?limit={}",
        host, port, args.limit
    );

    if args.event_type != "all" {
        url.push_str(&format!("&event_type={}", args.event_type));
    }

    if args.unacked {
        url.push_str("&unacked_only=true");
    }

    // Make HTTP request
    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "limit": args.limit,
            "event_type": if args.event_type == "all" { None } else { Some(&args.event_type) },
            "unacked_only": args.unacked
        }))
        .send()
        .await
        .context("Failed to connect to daemon HTTP API. Is the daemon running?")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("HTTP request failed ({}): {}", status, body);
    }

    let body: serde_json::Value = response.json().await.context("Failed to parse response")?;

    // Handle MCP-style response
    if let Some(content) = body.get("content") {
        if let Some(arr) = content.as_array() {
            if let Some(first) = arr.first() {
                if let Some(text) = first.get("text").and_then(|t| t.as_str()) {
                    // Parse the events from the text content
                    if let Ok(events) = serde_json::from_str::<Vec<SystemEvent>>(text) {
                        print_events(&events, formatter);
                        return Ok(());
                    }
                    // Print raw text if not JSON
                    println!("{}", text);
                    return Ok(());
                }
            }
        }
    }

    // Fallback: print raw response
    if formatter.quiet {
        println!(
            "{}",
            body.get("content")
                .map(|c| c.to_string())
                .unwrap_or_default()
        );
    } else {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }

    Ok(())
}

fn print_events(events: &[SystemEvent], formatter: &Formatter) {
    if events.is_empty() {
        formatter.print_success("No system events found");
        return;
    }

    match formatter.format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(events).unwrap_or_default()
            );
        }
        _ => {
            println!("System Events ({}):", events.len());
            for event in events {
                let ack = if event.acknowledged { "  " } else { "* " };
                let severity_icon = match event.severity.as_str() {
                    "error" | "critical" => "!",
                    "warning" => "?",
                    _ => " ",
                };
                println!(
                    "{}{} [{}] {} - {}",
                    ack, severity_icon, event.timestamp, event.event_type, event.message
                );
            }
        }
    }
}
