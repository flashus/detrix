//! Output command - Show output configuration status

use crate::context::ClientContext;
use anyhow::{Context, Result};
use detrix_logging::info;

pub async fn run(ctx: &ClientContext, test_connection: bool) -> Result<()> {
    let gelf = &ctx.config.output.gelf;

    println!("Output Configuration:");
    println!("=====================");
    println!();

    // GELF Output status
    if gelf.enabled {
        println!("GELF Output:");
        println!("  Status:    ENABLED");
        println!("  Transport: {}", gelf.transport);
        println!("  Host:      {}", gelf.host);
        println!("  Port:      {}", gelf.port);

        if let Some(ref source) = gelf.source_host {
            println!("  Source:    {}", source);
        }

        // Show transport-specific settings
        match gelf.transport.to_lowercase().as_str() {
            "tcp" => {
                println!();
                println!("  TCP Settings:");
                println!("    Connect timeout: {}ms", gelf.tcp.connect_timeout_ms);
                println!("    Write timeout:   {}ms", gelf.tcp.write_timeout_ms);
                println!("    Auto reconnect:  {}", gelf.tcp.auto_reconnect);
            }
            "udp" => {
                println!();
                println!("  UDP Settings:");
                println!("    Compress:          {}", gelf.udp.compress);
                println!("    Compression level: {}", gelf.udp.compression_level);
            }
            "http" => {
                println!();
                println!("  HTTP Settings:");
                println!("    Path:           {}", gelf.http.path);
                println!("    Timeout:        {}ms", gelf.http.timeout_ms);
                println!("    Batch size:     {}", gelf.http.batch_size);
                println!("    Flush interval: {}ms", gelf.http.flush_interval_ms);
                println!("    Compress:       {}", gelf.http.compress);
                if gelf.http.auth_token.is_some() {
                    println!("    Auth:           Bearer token configured");
                }
            }
            _ => {}
        }

        // Show extra fields if any
        if !gelf.extra_fields.is_empty() {
            println!();
            println!("  Extra Fields:");
            for (key, value) in &gelf.extra_fields {
                println!("    _{}: {}", key, value);
            }
        }

        // Show routes if any
        if !gelf.routes.is_empty() {
            println!();
            println!("  Routes:");
            for route in &gelf.routes {
                println!("    - Stream: {}", route.stream);
                let m = &route.match_rule;
                if m.connection_id.is_some()
                    || m.group.is_some()
                    || m.metric_name.is_some()
                    || m.errors_only
                {
                    print!("      Match: ");
                    let mut parts = Vec::new();
                    if let Some(ref conn) = m.connection_id {
                        parts.push(format!("connection={}", conn));
                    }
                    if let Some(ref group) = m.group {
                        parts.push(format!("group={}", group));
                    }
                    if let Some(ref name) = m.metric_name {
                        parts.push(format!("name={}", name));
                    }
                    if m.errors_only {
                        parts.push("errors_only".to_string());
                    }
                    println!("{}", parts.join(", "));
                }
            }
        }

        // Test connection if requested
        if test_connection {
            println!();
            println!("Testing connection...");

            match crate::utils::output::build_gelf_output(gelf).await {
                Ok(Some(_output)) => {
                    println!("  ✓ Connection successful!");
                    info!("GELF connection test passed");
                }
                Ok(None) => {
                    println!("  ⚠ Output not enabled");
                }
                Err(e) => {
                    println!("  ✗ Connection failed: {}", e);
                    return Err(e).context("GELF connection test failed");
                }
            }
        }
    } else {
        println!("GELF Output:");
        println!("  Status: DISABLED");
        println!();
        println!("To enable GELF output, add to your config:");
        println!();
        println!("  [output.gelf]");
        println!("  enabled = true");
        println!("  transport = \"tcp\"  # or \"udp\"");
        println!("  host = \"localhost\"");
        println!("  port = 12201");
    }

    println!();
    Ok(())
}
