//! Comprehensive Rust eBPF composite fixture.
//!
//! The observation point intentionally keeps every representation live at
//! once: nested structs, fixed arrays, Vecs, borrowed slices, String/&str,
//! HashMaps, Box/Option pointers, and an enum nested inside the object graph.

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

#[derive(Clone)]
struct Tag {
    key: String,
    value: String,
}

#[derive(Clone)]
struct LineItem {
    sku: String,
    quantity: u32,
    prices: [f64; 4],
    tags: Vec<Tag>,
}

#[derive(Clone)]
enum OrderState {
    Pending,
    Settled { code: u8 },
}

#[derive(Clone)]
struct Order {
    id: u64,
    customer: String,
    note: &'static str,
    primary: Box<LineItem>,
    lines: Vec<LineItem>,
    line_slice: &'static [u64],
    tags: HashMap<String, Tag>,
    state: OrderState,
}

#[derive(Clone)]
struct Snapshot {
    order: Box<Order>,
    order_ptr: Box<Order>,
    orders: Vec<Order>,
    prices: [f64; 5],
    labels: Vec<String>,
    label_slice: &'static [&'static str],
    attributes: HashMap<String, String>,
}

fn tag(key: &str, value: &str) -> Tag {
    Tag {
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

fn line_item(seed: u64) -> LineItem {
    LineItem {
        sku: format!("SKU-{seed}"),
        quantity: (seed % 9 + 1) as u32,
        prices: [100.5 + seed as f64, 101.2, 99.8, 102.1],
        tags: vec![tag("kind", "instrument"), tag("source", "fixture")],
    }
}

fn order(seed: u64, line_slice: &'static [u64]) -> Order {
    let mut tags = HashMap::new();
    tags.insert("desk".to_owned(), tag("desk", "alpha"));
    tags.insert("risk".to_owned(), tag("risk", "low"));
    Order {
        id: 1000 + seed,
        customer: "detrix".to_owned(),
        note: "BTCUSD",
        primary: Box::new(line_item(seed)),
        lines: vec![line_item(seed + 1), line_item(seed + 2)],
        line_slice,
        tags,
        state: if seed % 2 == 0 {
            OrderState::Settled { code: 7 }
        } else {
            OrderState::Pending
        },
    }
}

#[inline(never)]
fn observe(snapshot: Snapshot) -> Snapshot {
    // Stable eBPF anchor. All fields below are intentionally consumed by
    // black_box before formatting so DWARF retains their aggregate locations.
    let snapshot = black_box(snapshot);
    println!(
        "nested id={} prices={} labels={} attrs={} state={}",
        snapshot.order.id,
        snapshot.prices.len(),
        snapshot.labels.len(),
        snapshot.attributes.len(),
        match snapshot.order.state {
            OrderState::Pending => 0,
            OrderState::Settled { code } => code,
        }
    );
    std::thread::sleep(std::time::Duration::from_millis(250));
    snapshot
}

fn main() {
    static LINE_SLICE: [u64; 3] = [7, 11, 13];
    static LABEL_SLICE: [&str; 3] = ["btc", "usd", "spot"];
    // eBPF transports aggregate bytes first and resolves pointed-to heap data
    // asynchronously. Retain a bounded history so owned String/Vec/HashMap
    // allocations remain valid while the daemon decodes the event.
    static RETAINED: OnceLock<Mutex<Vec<Snapshot>>> = OnceLock::new();
    let retained = RETAINED.get_or_init(|| Mutex::new(Vec::with_capacity(64)));
    loop {
        // Keep the observation deterministic so the eBPF test can validate
        // every decoded scalar/container element exactly.
        let root = order(1, &LINE_SLICE);
        let mut attributes = HashMap::new();
        attributes.insert("venue".to_owned(), "test".to_owned());
        attributes.insert("account".to_owned(), "paper".to_owned());
        let snapshot = Snapshot {
            order: Box::new(root.clone()),
            order_ptr: Box::new(root.clone()),
            orders: vec![root.clone(), order(2, &LINE_SLICE)],
            prices: [100.5, 101.2, 99.8, 102.1, 100.0],
            labels: vec!["btc".to_owned(), "usd".to_owned(), "spot".to_owned()],
            label_slice: &LABEL_SLICE,
            attributes,
        };
        // Observe first, then retain that exact value. Cloning before the
        // observation gives the probe pointers into a different allocation
        // which is dropped immediately after the function returns.
        let observed = observe(snapshot);
        if let Ok(mut history) = retained.lock() {
            history.push(observed);
            if history.len() > 64 {
                history.remove(0);
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}
