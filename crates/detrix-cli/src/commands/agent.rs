//! Agent mode — start agent, scan for binaries, check status.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub action: AgentAction,
}

#[derive(Subcommand)]
pub enum AgentAction {
    /// Start the agent and connect to the detrix server.
    ///
    /// Requires an [agent] section in the config file with server_grpc_url
    /// and token_file set.
    Start {
        /// Override agent.server_grpc_url from config
        #[arg(long)]
        server: Option<String>,

        /// Override agent.token_file from config
        #[arg(long)]
        token_file: Option<PathBuf>,
    },

    /// Scan /proc for observable Go binaries.
    ///
    /// Dry-run mode — does not connect to the server. Useful for
    /// verifying which binaries the agent will discover.
    Scan {
        /// Show binaries that were skipped (with reasons)
        #[arg(long)]
        verbose: bool,
    },

    /// Print agent health metrics from the local Prometheus endpoint.
    Status {
        /// Metrics server URL (default: http://localhost:9091)
        #[arg(long, default_value = "http://localhost:9091")]
        metrics_url: String,
    },
}

/// Run the agent CLI.
pub async fn run(action: AgentAction, config_path: &str) -> Result<()> {
    match action {
        AgentAction::Start { server, token_file } => {
            run_start(server, token_file, config_path).await
        }
        AgentAction::Scan { verbose } => run_scan(verbose, config_path).await,
        AgentAction::Status { metrics_url } => run_status(&metrics_url).await,
    }
}

/// Start the agent — connects to server and begins observing.
async fn run_start(
    server_override: Option<String>,
    token_file_override: Option<PathBuf>,
    config_path: &str,
) -> Result<()> {
    use detrix_agent::agent::Agent;
    use detrix_config::load_config;
    use detrix_ebpf::CaptureConfig;
    use std::path::Path;

    let config_path = Path::new(config_path);
    let config = load_config(config_path).context("Failed to load config")?;

    let mut agent_config = config.agent.clone();

    // Apply CLI overrides
    if let Some(ref url) = server_override {
        agent_config.server_grpc_url = url.clone();
    }
    if let Some(ref path) = token_file_override {
        agent_config.token_file = Some(path.clone());
    }

    // Validate required fields
    agent_config
        .validate_for_agent()
        .map_err(|e| anyhow::anyhow!(e))?;

    let capture_config = CaptureConfig::from(&config.ebpf);

    // Start the agent
    let agent = Agent::new(agent_config, capture_config);
    agent.run().await?;

    Ok(())
}

/// Scan /proc for observable binaries (dry run).
async fn run_scan(verbose: bool, config_path: &str) -> Result<()> {
    use detrix_agent::scanner::ProcScanner;
    use detrix_config::{load_config, ScannerConfig};
    use std::path::Path;

    let config_path = Path::new(config_path);
    let config = load_config(config_path).context("Failed to load config")?;

    let scanner_config = if config.agent.scanner.include_patterns.is_empty()
        && config.agent.scanner.exclude_patterns.is_empty()
    {
        // Use defaults if no agent config
        ScannerConfig::default()
    } else {
        config.agent.scanner.clone()
    };

    let mut scanner = ProcScanner::new(&scanner_config);
    let binaries = scanner.scan_full();

    if binaries.is_empty() {
        println!("No observable binaries found.");
    } else {
        println!("Found {} observable binaries:", binaries.len());
        for binary in &binaries {
            println!(
                "  {}  pid={}  DWARF={}",
                binary.binary_path, binary.pid, binary.has_dwarf
            );
        }
    }

    if verbose {
        // Count skipped entries
        let mut total: usize = 0;
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.parse::<u32>().is_ok() {
                    total += 1;
                }
            }
        }
        let skipped = total.saturating_sub(binaries.len());
        if skipped > 0 {
            println!("\nWarning: {} of {} /proc entries skipped.", skipped, total);
            if !cfg!(target_os = "linux") {
                println!("  Running as non-root may limit visibility.");
            }
        }
    }

    Ok(())
}

/// Show agent health status from local metrics endpoint.
async fn run_status(metrics_url: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let base_url = metrics_url.trim_end_matches('/');

    // Fetch /health
    let health = client
        .get(format!("{base_url}/health"))
        .send()
        .await
        .context("Failed to connect to metrics server")?;

    if health.status().is_success() {
        println!("Agent metrics server is healthy ({metrics_url})");
    } else {
        println!("Agent metrics server returned {}", health.status());
    }

    // Fetch /metrics
    let metrics = client
        .get(format!("{base_url}/metrics"))
        .send()
        .await?
        .text()
        .await?;

    println!("\nMetrics:");
    for line in metrics.lines() {
        if !line.starts_with('#') {
            println!("  {line}");
        }
    }

    Ok(())
}
