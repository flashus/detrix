//! Rust layout-contract fixture for bounded eBPF observation.
//!
//! The values deliberately expose only ABI facts: a nullable niche pointer,
//! a trait-object data/vtable pair, and an explicit async-state byte. Detrix
//! must not call methods or chase either pointer.

use std::fmt::Display;
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy)]
#[repr(C)]
struct DetrixAsyncState {
    state: u8,
    payload: [u8; 7],
}

fn render(value: &dyn Display) -> (Option<&u64>, &dyn Display, DetrixAsyncState) {
    static PAYLOAD: u64 = 42;
    let maybe = Some(&PAYLOAD);
    let state = DetrixAsyncState {
        state: 3,
        payload: [7; 7],
    };
    (maybe, value, state)
}

fn main() {
    let text = String::from("detrix-special-layout");
    loop {
        let (maybe, object, state) = render(&text);
        // Keep all values live at a stable source location for the fixture.
        println!(
            "special option={} object={} state={}",
            maybe.map(|value| *value).unwrap_or_default(),
            object,
            state.state
        );
        thread::sleep(Duration::from_millis(100));
    }
}
