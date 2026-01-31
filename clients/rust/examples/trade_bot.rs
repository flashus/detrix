//! Trading Bot example with Detrix client integration.
//!
//! This example mirrors the Python trade_bot_forever.py fixture, demonstrating
//! how to integrate Detrix into a real application.
//!
//! To run:
//!   cargo run --example trade_bot
//!
//! Then use Detrix to add metrics:
//!   1. Wake the client: curl -X POST http://127.0.0.1:<port>/detrix/wake
//!   2. Add a metric in the Detrix daemon to observe `pnl` at a specific line

use std::thread;
use std::time::Duration;

use detrix_client::{self as detrix, Config};
use rand::Rng;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter("detrix=info")
        .init();

    // Initialize Detrix client
    println!("Initializing Detrix client...");
    detrix::init(Config {
        name: Some("trade-bot".to_string()),
        ..Config::default()
    })?;

    let status = detrix::status();
    println!("Control plane: http://127.0.0.1:{}", status.control_port);
    println!("\nTrading bot started - runs forever until Ctrl+C");
    println!("Add metrics with Detrix to observe values!\n");

    let symbols = vec!["BTCUSD", "ETHUSD", "SOLUSD"];
    let mut rng = rand::thread_rng();
    let mut iteration = 0;

    loop {
        iteration += 1;

        let symbol = symbols[rng.gen_range(0..symbols.len())];
        let quantity: i32 = rng.gen_range(1..=50);
        let price: f64 = rng.gen_range(100.0..=1000.0);

        // Place order
        let order_id = place_order(symbol, quantity, price);

        // Calculate P&L
        let entry_price = price;
        let current_price = price * rng.gen_range(0.95..=1.05);
        let pnl = calculate_pnl(entry_price, current_price, quantity);

        // Suppress unused variable warnings
        let _ = order_id;
        let _ = pnl;

        // Introspection points
        println!("  -> P&L: ${:.2} (iteration {})", pnl, iteration);

        thread::sleep(Duration::from_secs(3));
    }
}

fn place_order(symbol: &str, quantity: i32, price: f64) -> i32 {
    let order_id = rand::thread_rng().gen_range(1000..=9999);
    let total = quantity as f64 * price;
    println!(
        "Order #{}: {} x{} @ ${:.2} = ${:.2}",
        order_id, symbol, quantity, price, total
    );
    order_id
}

fn calculate_pnl(entry_price: f64, current_price: f64, quantity: i32) -> f64 {
    (current_price - entry_price) * quantity as f64
}
