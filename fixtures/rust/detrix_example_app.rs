// Trading Bot Example for Rust (lldb-dap) Integration Testing
// This file matches the Python trade_bot_forever.py algorithm exactly
//
// To compile with debug symbols:
//   rustc -g ./fixtures/rust/detrix_example_app.rs -o ./fixtures/rust/detrix_example_app
//
// To run with detrix lldb-serve:
//   detrix lldb-serve ./fixtures/rust/detrix_example_app --listen localhost:4711

use std::io::Write;
use std::thread;
use std::time::Duration;
use std::time::SystemTime;

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
        RAND_STATE = RAND_STATE.wrapping_mul(6364136223846793005).wrapping_add(1);
        RAND_STATE
    }
}

fn rand_int(max: i32) -> i32 {
    (rand_next() % (max as u64)) as i32
}

fn rand_float() -> f64 {
    (rand_next() % 10000) as f64 / 10000.0
}

fn place_order(symbol: &str, quantity: i32, price: f64) -> i32 {
    let order_id = rand_int(9000) + 1000; // 1000-9999
    let total = quantity as f64 * price;
    println!("Order #{}: {} x{} @ ${:.2} = ${:.2}", order_id, symbol, quantity, price, total);
    order_id
}

fn calculate_pnl(entry_price: f64, current_price: f64, quantity: i32) -> f64 {
    (current_price - entry_price) * quantity as f64
}

fn main() {
    rand_init();

    println!("Trading bot started - runs forever until Ctrl+C");
    println!("Add metrics with Detrix to observe values!");
    println!();

    let symbols = vec!["BTCUSD", "ETHUSD", "SOLUSD"];
    let mut iteration = 0;

    loop {
        iteration += 1;
        let symbol = symbols[(rand_int(3) as usize) % symbols.len()];
        let quantity = rand_int(50) + 1;                              // 1-50
        let price = rand_float() * 900.0 + 100.0;                     // 100-1000

        // Line 67 - place_order call (matches Python line 46)
        let order_id = place_order(symbol, quantity, price);

        // Calculate pnl (matches Python line 51)
        let entry_price = price;
        let current_price = price * (0.95 + rand_float() * 0.1);      // 0.95-1.05
        let pnl = calculate_pnl(entry_price, current_price, quantity);

        // Suppress unused variable warnings
        let _ = order_id;
        let _ = pnl;

        // Introspection point 1 (line 81) - println with pnl
        println!("  -> P&L: ${:.2} (iteration {})", pnl, iteration);
        // Introspection point 2 (line 83) - function call to force new instruction
        std::io::stdout().flush().ok();
        // Introspection point 3 (line 85) - sleep call
        thread::sleep(Duration::from_secs(3)); // Same as Python - 3 seconds
    }
}
