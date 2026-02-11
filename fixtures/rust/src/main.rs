// Trading Bot Example for Rust (lldb-dap) Integration Testing
// This file matches the Python/Go trade_bot_forever algorithm exactly
//
// SINGLE FIXTURE MODE:
// This file works for both DAP tests (no client) and client tests (with client).
//
// For DAP testing (without client):
//   cargo build && detrix lldb-serve ./target/debug/detrix_example_app --listen localhost:4711
//
// For client E2E tests (with Detrix client):
//   DETRIX_CLIENT_ENABLED=1 DETRIX_DAEMON_URL=http://127.0.0.1:8090 cargo run --features client
//
// LINE NUMBER CALCULATION:
// All metric line numbers are calculated as offsets from `fn main()` (line 76).
// To update tests after modifying this file:
//   1. Find the line number of `fn main()` below (currently 76)
//   2. Update `MAIN_LINE` in dap_scenarios.rs and test_wake example
//   3. Offsets are documented with each critical line
//
// NOTE: Regular output goes to stderr to avoid broken pipe panics when
// the test harness closes stdout after reading the control plane URL.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

// Conditional import: only when "client" feature is enabled
#[cfg(feature = "client")]
use std::env;

#[cfg(feature = "client")]
use detrix_rs::{self as detrix, Config};

/// Print to stderr (avoids broken pipe panics when stdout is closed)
macro_rules! log {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

// Simple random state (seeded once at start)
static mut RAND_STATE: u64 = 0;

fn rand_init() {
    unsafe {
        RAND_STATE = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
    }
}

fn rand_next() -> u64 {
    unsafe {
        // LCG parameters from Numerical Recipes
        RAND_STATE = RAND_STATE
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        RAND_STATE
    }
}

fn rand_int(max: i32) -> i32 {
    (rand_next() % (max as u64)) as i32
}

fn rand_float() -> f64 {
    (rand_next() % 10000) as f64 / 10000.0
}

// ============================================================================
// fn main() is at LINE 76 - all test line numbers are offsets from here
// ============================================================================
fn main() {
    // Initialize Detrix client if enabled (only with "client" feature)
    #[cfg(feature = "client")]
    init_detrix_client();

    rand_init();

    // Setup signal handler for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        log!("\nReceived shutdown signal, stopping...");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    log!("Trading bot started - runs forever until Ctrl+C");
    log!("Add metrics with Detrix to observe values!");
    log!("");

    let symbols = vec!["BTCUSD", "ETHUSD", "SOLUSD"];
    let mut iteration = 0;

    // Main trading loop
    while running.load(Ordering::SeqCst) {
        iteration += 1;

        // LINE NUMBERS BELOW ARE CRITICAL FOR E2E TESTS
        // All offsets are from main() at line 76
        // -----------------------------------------------
        // main+31: symbol assignment (line 107)
        let symbol = symbols[(rand_int(3) as usize) % symbols.len()];
        // main+33: quantity assignment (line 109)
        let quantity = rand_int(50) + 1;
        // main+35: price assignment (line 111)
        let price = rand_float() * 900.0 + 100.0;

        // main+38: place_order call (line 114, symbol/quantity/price in scope)
        let order_id = place_order(symbol, quantity, price);

        // main+41: entry_price assignment (line 117)
        let entry_price = price;
        // main+43: current_price assignment (line 119)
        let current_price = price * (0.95 + rand_float() * 0.1);
        // main+45: pnl = calculate_pnl (line 121, all vars in scope)
        let pnl = calculate_pnl(entry_price, current_price, quantity);

        // Suppress unused variable warnings
        let _ = order_id;
        let _ = pnl;

        // main+52: log! with pnl (line 128, introspection point 1)
        log!("  -> P&L: ${:.2} (iteration {})", pnl, iteration);
        // main+54: stderr().flush() (line 130, introspection point 2)
        std::io::stderr().flush().ok();
        // main+56: thread::sleep (line 132, introspection point 3)
        thread::sleep(Duration::from_secs(3)); // Same as Python - 3 seconds
    }

    // Cleanup
    #[cfg(feature = "client")]
    detrix::shutdown().ok();

    log!("Trading bot stopped!");
}

fn place_order(symbol: &str, quantity: i32, price: f64) -> i32 {
    let order_id = rand_int(9000) + 1000; // 1000-9999
    let total = quantity as f64 * price;
    log!(
        "Order #{}: {} x{} @ ${:.2} = ${:.2}",
        order_id, symbol, quantity, price, total
    );
    order_id
}

fn calculate_pnl(entry_price: f64, current_price: f64, quantity: i32) -> f64 {
    (current_price - entry_price) * quantity as f64
}

/// Initialize Detrix client if DETRIX_CLIENT_ENABLED=1
/// Only compiled when "client" feature is enabled
#[cfg(feature = "client")]
fn init_detrix_client() {
    if env::var("DETRIX_CLIENT_ENABLED").unwrap_or_default() != "1" {
        return;
    }

    let control_port: u16 = env::var("DETRIX_CONTROL_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let config = Config {
        name: Some(
            env::var("DETRIX_CLIENT_NAME").unwrap_or_else(|_| "trade-bot".to_string()),
        ),
        daemon_url: env::var("DETRIX_DAEMON_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8090".to_string()),
        control_port,
        ..Config::default()
    };

    if let Err(e) = detrix::init(config) {
        eprintln!("Failed to initialize Detrix client: {}", e);
        return;
    }

    let status = detrix::status();
    println!(
        "Control plane: http://{}:{}",
        status.control_host, status.control_port
    );
}
