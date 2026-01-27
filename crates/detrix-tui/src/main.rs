//! Detrix TUI - Main entry point
//!
//! Starts the terminal user interface for monitoring Detrix metrics.

use color_eyre::eyre::{Context, Result};
use detrix_config::{load_config, paths::discover_config_path};
use detrix_logging::{debug, info, init, LogConfig};
use detrix_tui::{App, DetrixClient};

#[tokio::main]
async fn main() -> Result<()> {
    // Install color-eyre error handler
    color_eyre::install()?;

    // Initialize tracing (logs to stderr so TUI can use stdout)
    init_tracing();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let grpc_override = parse_grpc_arg(&args);

    // Discover config path using centralized discovery
    // (checks CLI args, DETRIX_CONFIG env, falls back to default location)
    let (config_path, config_source) = discover_config_path();
    debug!(
        "Config path: {} (from {})",
        config_path.display(),
        config_source
    );

    // Load configuration (strict - does not create defaults)
    info!(
        "Loading config from: {} ({})",
        config_path.display(),
        config_source
    );
    let config = load_config(&config_path).context(format!(
        "Failed to load config from {}. Run 'detrix init' to create a default config.",
        config_path.display()
    ))?;

    // Set UTC/local timestamp preference for formatting
    detrix_tui::ui::formatters::set_use_utc(config.daemon.logging.use_utc);

    // Build gRPC endpoint (allow override via --grpc flag, otherwise use config)
    // Note: If daemon uses port fallback, specify --grpc=http://127.0.0.1:<actual_port>
    let grpc_endpoint = grpc_override
        .unwrap_or_else(|| format!("http://{}:{}", config.api.grpc.host, config.api.grpc.port));

    info!("Connecting to Detrix daemon at {}", grpc_endpoint);

    // Connect to daemon with REST config for status API
    let client = DetrixClient::connect_with_http_config(
        &grpc_endpoint,
        &config.api.rest.host,
        config.api.rest.port,
    )
    .await
    .context(format!(
        "Failed to connect to Detrix daemon at {}. Is it running?",
        grpc_endpoint
    ))?;

    info!("Connected successfully to {}", grpc_endpoint);

    // Initialize terminal
    let terminal = ratatui::init();

    // Run the application
    let result = App::new(config.tui, client).run(terminal).await;

    // Restore terminal (important: do this before returning error)
    ratatui::restore();

    result
}

/// Parse --grpc flag from command line (e.g., --grpc=http://localhost:50051)
fn parse_grpc_arg(args: &[String]) -> Option<String> {
    for (i, arg) in args.iter().enumerate() {
        if arg == "--grpc" {
            return args.get(i + 1).cloned();
        }
        if let Some(endpoint) = arg.strip_prefix("--grpc=") {
            return Some(endpoint.to_string());
        }
    }
    None
}

/// Initialize tracing subscriber for logging
fn init_tracing() {
    init(LogConfig::tui());
}
