//! GELF Output builder from configuration
//!
//! Creates GELF output instances based on detrix configuration.

use anyhow::{Context, Result};
use detrix_application::ports::EventOutputRef;
use detrix_config::{GelfOutputConfig, GelfRouteConfig};
use detrix_logging::{debug, info, warn};
use detrix_output::{
    CircuitBreakerConfig, GelfHttpBuilder, GelfTcpBuilder, OutputBuilder, RoutedGelfTcpOutput,
    RouterConfig, StreamRule,
};
use std::sync::Arc;

/// Build a GELF output from configuration
///
/// Returns `None` if GELF output is not enabled.
/// Returns `Ok(Some(output))` if enabled and successfully connected.
/// Returns `Err` if enabled but connection fails.
pub async fn build_gelf_output(config: &GelfOutputConfig) -> Result<Option<EventOutputRef>> {
    if !config.enabled {
        debug!("GELF output disabled in configuration");
        return Ok(None);
    }

    let transport = config.transport.to_lowercase();
    info!(
        "📤 Initializing GELF output ({}) to {}:{}",
        transport, config.host, config.port
    );

    match transport.as_str() {
        "tcp" => {
            let mut builder = GelfTcpBuilder::new(&config.host, config.port)
                .connect_timeout_ms(config.tcp.connect_timeout_ms)
                .write_timeout_ms(config.tcp.write_timeout_ms)
                .auto_reconnect(config.tcp.auto_reconnect);

            if let Some(ref source_host) = config.source_host {
                builder = builder.source_host(source_host);
            }

            // Check if routing is configured
            if !config.routes.is_empty() || !config.extra_fields.is_empty() {
                let tcp_output = builder
                    .connect()
                    .await
                    .context("Failed to connect to GELF TCP endpoint")?;

                let rules = config
                    .routes
                    .iter()
                    .filter_map(convert_route_config)
                    .collect::<Vec<_>>();

                let router_config = RouterConfig {
                    rules,
                    add_connection_field: true,
                };

                let routed_output = RoutedGelfTcpOutput::new(
                    tcp_output,
                    router_config,
                    config.extra_fields.clone(),
                );

                // Wrap with circuit breaker if configured
                let output: EventOutputRef = if let Some(ref cb_config) = config.tcp.circuit_breaker
                {
                    let cb = CircuitBreakerConfig::new(
                        cb_config.failure_threshold,
                        cb_config.reset_timeout_ms,
                    )
                    .with_success_threshold(cb_config.success_threshold)
                    .with_name("gelf-tcp-routed");

                    info!(
                        "✓ GELF TCP output connected to {}:{} with {} routing rules and circuit breaker (threshold: {}, timeout: {}ms)",
                        config.host, config.port, config.routes.len(),
                        cb_config.failure_threshold, cb_config.reset_timeout_ms
                    );

                    Arc::new(
                        OutputBuilder::new(routed_output)
                            .with_circuit_breaker(cb)
                            .build(),
                    )
                } else {
                    info!(
                        "✓ GELF TCP output connected to {}:{} with {} routing rules",
                        config.host,
                        config.port,
                        config.routes.len()
                    );
                    Arc::new(routed_output)
                };

                Ok(Some(output))
            } else {
                // Build the TCP output (not connected yet)
                let tcp_output = builder.build();

                // Connect first
                tcp_output
                    .connect()
                    .await
                    .context("Failed to connect to GELF TCP endpoint")?;

                // Wrap with middleware if configured using fluent builder
                let output: EventOutputRef = if let Some(ref cb_config) = config.tcp.circuit_breaker
                {
                    let cb = CircuitBreakerConfig::new(
                        cb_config.failure_threshold,
                        cb_config.reset_timeout_ms,
                    )
                    .with_success_threshold(cb_config.success_threshold)
                    .with_name("gelf-tcp");

                    info!(
                        "✓ GELF TCP output connected to {}:{} with circuit breaker (threshold: {}, timeout: {}ms)",
                        config.host, config.port, cb_config.failure_threshold, cb_config.reset_timeout_ms
                    );

                    // Use fluent builder API
                    Arc::new(
                        OutputBuilder::new(tcp_output)
                            .with_circuit_breaker(cb)
                            .build(),
                    )
                } else {
                    info!(
                        "✓ GELF TCP output connected to {}:{}",
                        config.host, config.port
                    );
                    Arc::new(tcp_output)
                };
                Ok(Some(output))
            }
        }
        "udp" => {
            // UDP uses combined host:port address format
            let address = format!("{}:{}", config.host, config.port);

            let mut builder = detrix_output::GelfUdpBuilder::new(&address)
                .compress(config.udp.compress)
                .compression_level(config.udp.compression_level);

            if let Some(ref source_host) = config.source_host {
                builder = builder.source_host(source_host);
            }

            // Use bind() to create the socket AND bind it
            let output = Arc::new(
                builder
                    .bind()
                    .await
                    .context("Failed to bind GELF UDP socket")?,
            );

            info!(
                "✓ GELF UDP output initialized for {}:{}",
                config.host, config.port
            );
            Ok(Some(output as EventOutputRef))
        }
        "http" => {
            let url = format!("http://{}:{}{}", config.host, config.port, config.http.path);

            let mut builder = GelfHttpBuilder::new(&url)
                .timeout_ms(config.http.timeout_ms)
                .batch_size(config.http.batch_size)
                .flush_interval_ms(config.http.flush_interval_ms)
                .compress(config.http.compress);

            if let Some(ref source_host) = config.source_host {
                builder = builder.source_host(source_host);
            }

            if let Some(ref auth_token) = config.http.auth_token {
                builder = builder.auth_token(auth_token);
            }

            let http_output = builder
                .build()
                .context("Failed to create GELF HTTP output")?;

            // Wrap with circuit breaker if configured
            // Note: GelfHttpBuilder::build() returns Arc<GelfHttpOutput>
            let output: EventOutputRef = if let Some(ref cb_config) = config.http.circuit_breaker {
                let cb = CircuitBreakerConfig::new(
                    cb_config.failure_threshold,
                    cb_config.reset_timeout_ms,
                )
                .with_success_threshold(cb_config.success_threshold)
                .with_name("gelf-http");

                info!(
                        "✓ GELF HTTP output initialized for {} with circuit breaker (threshold: {}, timeout: {}ms)",
                        url, cb_config.failure_threshold, cb_config.reset_timeout_ms
                    );

                // http_output is Arc<GelfHttpOutput>, wrap it with circuit breaker
                Arc::new(
                    OutputBuilder::new(http_output)
                        .with_circuit_breaker(cb)
                        .build(),
                )
            } else {
                info!("✓ GELF HTTP output initialized for {}", url);
                http_output
            };

            Ok(Some(output))
        }
        other => {
            anyhow::bail!(
                "Unknown GELF transport '{}'. Supported: tcp, udp, http",
                other
            );
        }
    }
}

/// Convert a GELF route config to a StreamRule
///
/// Returns None if the config cannot be converted (e.g., invalid regex).
fn convert_route_config(config: &GelfRouteConfig) -> Option<StreamRule> {
    // Priority: connection_id > metric_name > errors_only > default (no match criteria)
    if let Some(ref connection_id) = config.match_rule.connection_id {
        Some(StreamRule::ConnectionId {
            connection_id: connection_id.clone(),
            stream: config.stream.clone(),
        })
    } else if let Some(ref metric_name) = config.match_rule.metric_name {
        // Try to compile the regex
        match regex::Regex::new(metric_name) {
            Ok(pattern) => Some(StreamRule::MetricPattern {
                pattern,
                stream: config.stream.clone(),
            }),
            Err(e) => {
                warn!(
                    "Invalid regex pattern '{}' in GELF route config: {}",
                    metric_name, e
                );
                None
            }
        }
    } else if config.match_rule.errors_only {
        Some(StreamRule::ErrorsTo {
            stream: config.stream.clone(),
        })
    } else {
        // Default rule (matches everything)
        Some(StreamRule::Default {
            stream: config.stream.clone(),
        })
    }
}
