//! Basic usage example for the Detrix Rust client.
//!
//! This example shows how to initialize the Detrix client and let it run
//! in the background while your application does its work.
//!
//! To run:
//!   cargo run --example basic_usage
//!
//! Then wake the client:
//!   curl -X POST http://127.0.0.1:<port>/detrix/wake
//!
//! Where <port> is printed in the console output.

use std::thread;
use std::time::Duration;

use detrix_rs::{self as detrix, Config};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for debug output
    tracing_subscriber::fmt()
        .with_env_filter("detrix=debug")
        .init();

    // Initialize the Detrix client
    println!("Initializing Detrix client...");
    detrix::init(Config {
        name: Some("basic-example".to_string()),
        ..Config::default()
    })?;

    // Get the status to find the control port
    let status = detrix::status();
    println!("\nDetrix client initialized:");
    println!("  State: {}", status.state);
    println!(
        "  Control plane: http://{}:{}",
        status.control_host, status.control_port
    );
    println!("  Daemon URL: {}", status.daemon_url);

    println!("\nTo wake the client, run:");
    println!(
        "  curl -X POST http://127.0.0.1:{}/detrix/wake",
        status.control_port
    );

    println!("\nRunning forever... Press Ctrl+C to stop.");

    // Your application code goes here
    let mut counter = 0;
    loop {
        counter += 1;
        println!("Iteration {}", counter);
        thread::sleep(Duration::from_secs(3));
    }
}
