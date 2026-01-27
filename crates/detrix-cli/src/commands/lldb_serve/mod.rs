//! LLDB DAP Server Command
//!
//! Provides a wrapper around lldb-dap that behaves like debugpy or delve:
//! - Starts the Rust binary through lldb-dap (similar to `dlv exec binary --headless`)
//! - Listens for DAP connections on a specified port
//! - Optionally pauses the program at entry (like debugpy --wait-for-client)
//!
//! Usage:
//!   detrix lldb-serve ./target/debug/my_program --listen localhost:4711 --stop-on-entry
//!
//! This makes Rust debugging work the same way as Python/Go from Detrix' perspective.
//!
//! ## Debug Adapter Selection
//!
//! On Windows, this module automatically downloads and uses CodeLLDB from GitHub
//! because it has better PDB (Windows debug symbol) support than standard lldb-dap.
//!
//! The adapter discovery order is:
//! 1. Explicit path via `--lldb-dap-path`
//! 2. CodeLLDB from VSCode extensions (if installed)
//! 3. Auto-downloaded CodeLLDB from GitHub
//! 4. Standard lldb-dap from PATH

mod codelldb_installer;
mod config;
mod discovery;
mod protocol;
mod proxy;
mod socket;

#[cfg(test)]
mod tests;

// Re-exports
pub use config::LldbServeArgs;

use config::ClientResult;
use detrix_logging::{debug, error, info};
use std::net::TcpListener;

/// Run the lldb-serve command
pub fn run(args: LldbServeArgs) -> anyhow::Result<()> {
    // Note: Logging is initialized by main.rs with file+stderr output
    // (logs go to ~/detrix/log/lldb_serve_{pid}.log with date rotation)

    debug!("=== lldb-serve starting ===");
    debug!(program = ?args.program, listen = %args.listen, stop_on_entry = args.stop_on_entry, persist = args.persist, "Startup parameters");

    // Validate program exists
    if !args.program.exists() {
        error!(program = ?args.program, "Program not found");
        anyhow::bail!("Program not found: {:?}", args.program);
    }

    // Find lldb-dap
    let lldb_dap_path = discovery::find_lldb_dap(&args.lldb_dap_path)?;
    debug!(lldb_dap = ?lldb_dap_path, "Using lldb-dap");
    info!("Using lldb-dap: {:?}", lldb_dap_path);

    // Parse listen address, normalizing localhost to 127.0.0.1 for cross-platform compatibility
    // On Windows, binding to "localhost" may use IPv6 while clients connect via IPv4
    let listen_addr = socket::normalize_listen_addr(&args.listen);
    debug!(listen_addr = %listen_addr, "Normalized listen address");
    info!("Starting DAP server on {}", listen_addr);

    // Create TCP listener
    let listener = TcpListener::bind(&listen_addr)?;
    debug!(listen_addr = %listen_addr, "TCP listener bound");
    info!("Listening for DAP connections on {}", listen_addr);

    // Get absolute program path (strip Windows \\?\ prefix that LLDB doesn't understand)
    let program_path = args.program.canonicalize()?;
    let program_str = normalize_windows_path(&program_path.to_string_lossy());
    debug!(program = %program_str, "Absolute program path");

    // Print startup info (similar to debugpy/delve)
    println!("Detrix LLDB Debug Server");
    println!("Program: {}", program_str);
    println!("Listen: {}", listen_addr);
    println!("Stop on entry: {}", args.stop_on_entry);
    if args.persist {
        println!("Persist mode: enabled (will keep running after client disconnect)");
    }
    println!();
    println!("Waiting for DAP client connection...");
    debug!("Ready, waiting for DAP client connection...");

    // Accept connections
    for stream in listener.incoming() {
        match stream {
            Ok(client_stream) => {
                let peer = client_stream.peer_addr().ok();
                debug!(peer = ?peer, "Client connected");
                info!("Client connected from {:?}", peer);

                // Configure socket for Windows compatibility (read timeout + TCP keepalive)
                socket::configure_client_socket(&client_stream);

                println!("Client connected, starting lldb-dap...");

                // Handle the client connection
                let result = proxy::handle_client(
                    client_stream,
                    &lldb_dap_path,
                    &program_str,
                    &args.program_args,
                    args.stop_on_entry,
                    args.cwd.as_ref(),
                );

                match result {
                    ClientResult::SessionCompleted => {
                        debug!("DAP session completed normally");
                        info!("DAP session completed");
                        if args.persist {
                            println!("Client disconnected. Waiting for new connection...");
                            debug!("Persist mode: waiting for new connection");
                            continue;
                        } else {
                            debug!("Shutting down (no persist mode)");
                            info!("Client disconnected, shutting down");
                            break;
                        }
                    }
                    ClientResult::NoSession => {
                        // Not a real DAP client (e.g., port scan, nc -z)
                        // Always continue waiting for real clients
                        debug!("Connection closed without DAP session (port scan?)");
                        println!("Waiting for DAP client connection...");
                        continue;
                    }
                    ClientResult::Error(e) => {
                        error!(err = %e, "Client session error");
                        if args.persist {
                            println!("Session error. Waiting for new connection...");
                            debug!("Persist mode: waiting for new connection after error");
                            continue;
                        } else {
                            debug!("Shutting down due to error (no persist mode)");
                            return Err(e);
                        }
                    }
                }
            }
            Err(e) => {
                error!(err = %e, "Failed to accept connection");
            }
        }
    }

    Ok(())
}

/// Normalize Windows path by stripping the \\?\ extended-length prefix
/// that `canonicalize()` adds on Windows. LLDB doesn't understand this prefix.
#[allow(unused_variables)]
fn normalize_windows_path(path: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        // Strip \\?\ prefix that Windows canonicalize() adds
        if let Some(stripped) = path.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    path.to_string()
}
