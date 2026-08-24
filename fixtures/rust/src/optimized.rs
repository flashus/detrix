//! Optimized-out Rust DWARF fixture.
//!
//! This binary is built separately with `-C opt-level=3`.  `optimized_out` is
//! deliberately computed but never observed; a probe requesting it must fail
//! closed rather than accepting a DIE with an empty or out-of-range location.

use std::hint::black_box;
use std::thread;
use std::time::Duration;

#[inline(never)]
fn tick(iteration: u64) {
    let live = black_box(iteration.wrapping_mul(3));
    // Observation anchor: this local is intentionally dead under opt-level=3.
    #[allow(unused_variables)]
    let optimized_out = iteration.wrapping_add(0xdead_beef);
    println!("optimized fixture live={live}");
    thread::sleep(Duration::from_millis(20));
    let _ = black_box(live);
    // Keep the loop observable without keeping `optimized_out` alive.
}

fn main() {
    let mut iteration = 0u64;
    loop {
        tick(iteration);
        iteration = iteration.wrapping_add(1);
    }
}
