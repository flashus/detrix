//! Core output formatting types and Formatter implementation

use crate::grpc_client::{ConnectionInfo, EventInfo, GroupInfo, MetricInfo, StatusInfo};

/// Output format for CLI commands
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable table format (default)
    #[default]
    Table,
    /// JSON format for scripting
    Json,
    /// LLM-optimized format (matches MCP output)
    Toon,
}

/// Unified formatter for CLI output
#[derive(Clone, Debug)]
pub struct Formatter {
    pub format: OutputFormat,
    pub quiet: bool,
    pub no_color: bool,
}

impl Formatter {
    /// Create a new formatter with the given settings
    pub fn new(format: OutputFormat, quiet: bool, no_color: bool) -> Self {
        Self {
            format,
            quiet,
            no_color,
        }
    }

    /// Print a single ID (for quiet mode after create operations)
    pub fn print_id(&self, id: &str) {
        println!("{}", id);
    }

    /// Print a success message (respects quiet mode)
    pub fn print_success(&self, message: &str) {
        if !self.quiet {
            println!("{}", message);
        }
    }

    /// Print metrics list
    pub fn print_metrics(&self, metrics: &[MetricInfo]) {
        if self.quiet {
            for m in metrics {
                println!("{}", m.name);
            }
            return;
        }

        match self.format {
            OutputFormat::Table => super::table::print_metrics(metrics, self.no_color),
            OutputFormat::Json => super::json::print_metrics(metrics),
            OutputFormat::Toon => super::table::print_metrics_toon(metrics),
        }
    }

    /// Print single metric
    pub fn print_metric(&self, metric: &MetricInfo) {
        if self.quiet {
            println!("{}", metric.name);
            return;
        }

        match self.format {
            OutputFormat::Table => super::table::print_metric_detail(metric, self.no_color),
            OutputFormat::Json => super::json::print_metric(metric),
            OutputFormat::Toon => super::table::print_metric_detail(metric, self.no_color),
        }
    }

    /// Print connections list
    pub fn print_connections(&self, connections: &[ConnectionInfo]) {
        if self.quiet {
            for c in connections {
                println!("{}", c.connection_id);
            }
            return;
        }

        match self.format {
            OutputFormat::Table => super::table::print_connections(connections, self.no_color),
            OutputFormat::Json => super::json::print_connections(connections),
            OutputFormat::Toon => super::table::print_connections_toon(connections),
        }
    }

    /// Print single connection
    pub fn print_connection(&self, connection: &ConnectionInfo) {
        if self.quiet {
            println!("{}", connection.connection_id);
            return;
        }

        match self.format {
            OutputFormat::Table => super::table::print_connection_detail(connection, self.no_color),
            OutputFormat::Json => super::json::print_connection(connection),
            OutputFormat::Toon => super::table::print_connection_detail(connection, self.no_color),
        }
    }

    /// Print events list
    pub fn print_events(&self, events: &[EventInfo]) {
        if self.quiet {
            for e in events {
                println!("{}", e.metric_name);
            }
            return;
        }

        match self.format {
            OutputFormat::Table => super::table::print_events(events, self.no_color),
            OutputFormat::Json => super::json::print_events(events),
            OutputFormat::Toon => super::table::print_events_toon(events),
        }
    }

    /// Print groups list
    pub fn print_groups(&self, groups: &[GroupInfo]) {
        if self.quiet {
            for g in groups {
                println!("{}", g.name);
            }
            return;
        }

        match self.format {
            OutputFormat::Table => super::table::print_groups(groups, self.no_color),
            OutputFormat::Json => super::json::print_groups(groups),
            OutputFormat::Toon => super::table::print_groups_toon(groups),
        }
    }

    /// Print system status
    pub fn print_status(&self, status: &StatusInfo) {
        match self.format {
            OutputFormat::Table => super::table::print_status(status, self.no_color),
            OutputFormat::Json => super::json::print_status(status),
            OutputFormat::Toon => super::table::print_status(status, self.no_color),
        }
    }

    /// Print validation result
    pub fn print_validation(&self, valid: bool, message: &str) {
        if self.quiet {
            println!("{}", if valid { "valid" } else { "invalid" });
            return;
        }

        match self.format {
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({"valid": valid, "message": message})
                );
            }
            _ => {
                if valid {
                    println!("✓ {}", message);
                } else {
                    println!("✗ {}", message);
                }
            }
        }
    }

    /// Print config (TOML or JSON)
    pub fn print_config(&self, config: &str) {
        match self.format {
            OutputFormat::Json => {
                // Parse TOML and convert to JSON
                if let Ok(value) = toml::from_str::<toml::Value>(config) {
                    if let Ok(json) = serde_json::to_string_pretty(&value) {
                        println!("{}", json);
                        return;
                    }
                }
                // Fallback: print as-is
                println!("{}", config);
            }
            _ => {
                println!("{}", config);
            }
        }
    }
}

impl Default for Formatter {
    fn default() -> Self {
        Self::new(OutputFormat::Table, false, false)
    }
}
