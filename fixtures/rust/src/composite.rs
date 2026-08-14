//! Small deterministic Rust composite fixture for bounded inline eBPF capture.
//! The loop deliberately keeps a fixed-size struct on the stack and exposes a
//! stable source line where its bytes are live.

#[derive(Clone, Copy)]
#[repr(C)]
struct TradeRequest {
    quantity: u64,
    cents: u64,
}

#[derive(Clone, Copy)]
enum TradeState {
    Pending,
    Settled(u8),
}

#[inline(never)]
fn process(request: TradeRequest) {
    // Composite capture anchor: request is an inline, bounded stack value.
    let _keep = std::hint::black_box(&request);
    println!("composite quantity={} cents={}", request.quantity, request.cents);
}

#[inline(never)]
fn observe_owned(owned: String) {
    // Keep the Rust String header live at a dedicated observation statement;
    // the later formatting path is allowed to move/repack the value.
    let _keep = std::hint::black_box(&owned);
    println!("owned {}", owned);
}

#[inline(never)]
fn process_headers(tick: u64) {
    // These headers are intentionally bounded and live together at this
    // statement. Pointer contents are read by the profile's user-space
    // memory policy; the probe itself only captures the header words.
    let owned = format!("trade-{tick}");
    let _owned_keep = std::hint::black_box(&owned);
    let view: &str = "BTCUSD";
    let _view_keep = std::hint::black_box(&view);
    let values: Vec<u64> = vec![tick, tick + 1, tick + 2];
    let _values_keep = std::hint::black_box(&values);
    let view_slice: &[u64] = &values;
    let _slice_keep = std::hint::black_box(&view_slice);
    let state = if tick % 2 == 0 {
        TradeState::Settled((tick % 255) as u8)
    } else {
        TradeState::Pending
    };
    let state_tag = match state {
        TradeState::Pending => 0u8,
        TradeState::Settled(value) => value,
    };
    observe_owned(owned);
    println!("headers {} {} {} {}", view, values.len(), state_tag, tick);
}

#[inline(never)]
fn observe_state(state: TradeState) {
    // Explicit-enum capture anchor.  The black_box keeps the full enum value
    // live at this statement so DWARF can describe its discriminant/payload.
    let _state_keep = std::hint::black_box(&state);
    match state {
        TradeState::Pending => println!("state pending"),
        TradeState::Settled(value) => println!("state settled {}", value),
    }
}

fn main() {
    let interval_ms = std::env::var("RUST_COMPOSITE_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(100)
        .max(1);
    let mut tick = 1u64;
    loop {
        let request = TradeRequest {
            quantity: 10 + (tick % 40),
            cents: 10000 + (tick % 90000),
        };
        process(request);
        process_headers(tick);
        observe_state(if tick % 2 == 0 {
            TradeState::Settled((tick % 255) as u8)
        } else {
            TradeState::Pending
        });
        tick = tick.wrapping_add(1);
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
    }
}
