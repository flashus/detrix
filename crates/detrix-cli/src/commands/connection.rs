//! Connection command - Manage debugger connections

use crate::context::ClientContext;
use crate::grpc_client::{ConnectionsClient, DaemonEndpoints};
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use detrix_config::constants::DEFAULT_API_HOST;

#[derive(Subcommand)]
pub enum ConnectionAction {
    /// Create a new debugger connection
    Create(CreateArgs),
    /// List all connections
    List(ListArgs),
    /// Get connection details
    Get {
        /// Connection ID
        connection_id: String,
    },
    /// Close a connection
    Close {
        /// Connection ID
        connection_id: String,
    },
    /// Cleanup stale (disconnected/failed) connections
    Cleanup,
}

#[derive(Args)]
pub struct CreateArgs {
    /// Host address (e.g., "127.0.0.1", "localhost")
    #[arg(long, short = 'H', default_value = DEFAULT_API_HOST)]
    host: String,

    /// Port number
    #[arg(long, short)]
    port: u32,

    /// Language/adapter type (python, go, rust)
    #[arg(long, short)]
    language: String,

    /// Optional custom connection ID
    #[arg(long)]
    id: Option<String>,

    /// Program path for Rust launch mode (requires lldb-dap --connection listen://HOST:PORT)
    #[arg(long)]
    program: Option<String>,

    /// SafeMode: only allow logpoints (non-blocking), disable breakpoint-based operations
    #[arg(long)]
    safe_mode: bool,
}

#[derive(Args)]
pub struct ListArgs {
    /// Only show active connections
    #[arg(long)]
    active: bool,
}

/// Run connection command
pub async fn run(
    ctx: &ClientContext,
    action: ConnectionAction,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    match action {
        ConnectionAction::Create(args) => create(&ctx.endpoints, args, &formatter).await,
        ConnectionAction::List(args) => list(&ctx.endpoints, args, &formatter).await,
        ConnectionAction::Get { connection_id } => {
            get(&ctx.endpoints, &connection_id, &formatter).await
        }
        ConnectionAction::Close { connection_id } => {
            close(&ctx.endpoints, &connection_id, &formatter).await
        }
        ConnectionAction::Cleanup => cleanup(&ctx.endpoints, &formatter).await,
    }
}

async fn create(
    endpoints: &DaemonEndpoints,
    args: CreateArgs,
    formatter: &Formatter,
) -> Result<()> {
    let mut client = ConnectionsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let connection = client
        .create(
            &args.host,
            args.port,
            &args.language,
            args.id.as_deref(),
            args.program.as_deref(),
            args.safe_mode,
        )
        .await
        .context("Failed to create connection")?;

    if formatter.quiet {
        formatter.print_id(&connection.connection_id);
    } else {
        formatter.print_connection(&connection);
    }

    Ok(())
}

async fn list(endpoints: &DaemonEndpoints, args: ListArgs, formatter: &Formatter) -> Result<()> {
    let mut client = ConnectionsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let connections = client
        .list(args.active)
        .await
        .context("Failed to list connections")?;

    formatter.print_connections(&connections);

    Ok(())
}

async fn get(
    endpoints: &DaemonEndpoints,
    connection_id: &str,
    formatter: &Formatter,
) -> Result<()> {
    let mut client = ConnectionsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let connection = client
        .get(connection_id)
        .await
        .context("Failed to get connection")?;

    formatter.print_connection(&connection);

    Ok(())
}

async fn close(
    endpoints: &DaemonEndpoints,
    connection_id: &str,
    formatter: &Formatter,
) -> Result<()> {
    let mut client = ConnectionsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    client
        .close(connection_id)
        .await
        .context("Failed to close connection")?;

    formatter.print_success(&format!("Connection {} closed", connection_id));

    Ok(())
}

async fn cleanup(endpoints: &DaemonEndpoints, formatter: &Formatter) -> Result<()> {
    let mut client = ConnectionsClient::with_endpoints(endpoints)
        .await
        .context("Failed to connect to daemon")?;

    let deleted = client
        .cleanup()
        .await
        .context("Failed to cleanup connections")?;

    if deleted == 0 {
        formatter.print_success("No stale connections to clean up");
    } else {
        formatter.print_success(&format!("Cleaned up {} stale connection(s)", deleted));
    }

    Ok(())
}
