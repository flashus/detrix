//! Status command - Show system status

use crate::context::ClientContext;
use crate::grpc_client::{ConnectionsClient, DaemonEndpoints, MetricsClient};
use crate::output::{Formatter, OutputFormat};
use anyhow::{Context, Result};
use detrix_logging::debug;

const HEALTH_CHECK_TIMEOUT_SECS: u64 = 3;

/// Run status command
///
/// Tries gRPC first. If gRPC fails (e.g. auth error in JWT mode where no static
/// token is available), falls back to the public REST `/health` endpoint which
/// is always accessible without authentication. This makes `detrix status`
/// usable as a Docker healthcheck even for JWT-protected daemons.
pub async fn run(
    ctx: &ClientContext,
    format: OutputFormat,
    quiet: bool,
    no_color: bool,
    verbose: bool,
) -> Result<()> {
    let formatter = Formatter::new(format, quiet, no_color);

    // Try gRPC first (has full status details)
    let grpc_result = async {
        let mut client = MetricsClient::with_endpoints(&ctx.endpoints)
            .await
            .context("Failed to connect to daemon. Is the daemon running?")?;
        client.get_status().await.context("Failed to get status")
    }
    .await;

    match grpc_result {
        Ok(status) => {
            formatter.print_status(&status);
            if verbose {
                print_verbose_details(&formatter, &ctx.endpoints).await?;
            }
            Ok(())
        }
        Err(grpc_err) => {
            // Fall back to public REST /health endpoint.
            // This handles JWT-mode daemons where no static token is configured
            // locally (e.g. Docker healthchecks inside the container).
            debug!(
                error = %grpc_err,
                "gRPC status failed, falling back to /health endpoint"
            );
            let healthy = check_health_rest(&ctx.endpoints.http_endpoint()).await;

            if healthy {
                if !quiet {
                    formatter.print_success("Daemon is running (auth required for full status)");
                }
                Ok(())
            } else {
                Err(grpc_err)
            }
        }
    }
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

/// Check daemon health via the public REST `/health` endpoint.
///
/// Returns `true` when the endpoint responds with a 2xx status code,
/// `false` on any error or non-success status.
async fn check_health_rest(http_endpoint: &str) -> bool {
    let health_url = format!("{}/health", http_endpoint);
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HEALTH_CHECK_TIMEOUT_SECS))
        .build()
        .unwrap_or_default();
    http_client
        .get(&health_url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_check_health_rest_returns_true_on_200() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        assert!(check_health_rest(&server.uri()).await);
    }

    #[tokio::test]
    async fn test_check_health_rest_returns_false_on_500() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        assert!(!check_health_rest(&server.uri()).await);
    }

    #[tokio::test]
    async fn test_check_health_rest_returns_false_on_unreachable() {
        // Port 1 is almost certainly not serving HTTP
        assert!(!check_health_rest("http://127.0.0.1:1").await);
    }
}
