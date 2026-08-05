//! DAP (Debug Adapter Protocol) Workflow Scenarios
//!
//! Provides reusable test configurations and workflows for testing Detrix
//! with different language DAP adapters (Python/debugpy, Go/delve, Rust/lldb-dap).
//!
//! These scenarios are DAP-specific and should only run with MCP backend,
//! as they test the actual debugger integration rather than just API endpoints.

use super::client::ApiClient;
use super::reporter::TestReporter;
use detrix_application::services::file_inspection_types::SourceLanguageExt;
use detrix_application::SourceLanguage;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

/// Extension trait for SourceLanguage to add DAP-specific display names
pub trait DapLanguageExt {
    /// Get DAP adapter display name (e.g., "Python (debugpy)")
    fn dap_display_name(&self) -> &'static str;

    /// Get language string for API calls (e.g., "python")
    fn as_api_str(&self) -> &'static str;
}

impl DapLanguageExt for SourceLanguage {
    fn dap_display_name(&self) -> &'static str {
        match self {
            SourceLanguage::Python => "Python (debugpy)",
            SourceLanguage::Go => "Go (delve)",
            SourceLanguage::Rust => "Rust (lldb-dap)",
            _ => self.display_name(),
        }
    }

    fn as_api_str(&self) -> &'static str {
        match self {
            SourceLanguage::Python => "python",
            SourceLanguage::Go => "go",
            SourceLanguage::Rust => "rust",
            SourceLanguage::JavaScript => "javascript",
            SourceLanguage::TypeScript => "typescript",
            SourceLanguage::Java => "java",
            SourceLanguage::Cpp => "cpp",
            SourceLanguage::C => "c",
            SourceLanguage::Ruby => "ruby",
            SourceLanguage::Php => "php",
            SourceLanguage::Unknown => "unknown",
        }
    }
}

/// Code map for a language fixture: maps symbol names to their declaration line
/// and the first safe logpoint line.
///
/// Background: DAP logpoints fire at the START of a statement, BEFORE the RHS
/// executes. So a variable declared as `x := expr` is NOT yet in scope at that
/// statement's line — it becomes safe to observe only from the NEXT statement.
///
/// Usage:
///   - `find_decl("symbol")` → absolute line where "symbol" is declared.
///     Use this to identify a particular "slot" in the fixture by its variable name.
///   - `find_logpoint("symbol")` → first line where "symbol" is safely in scope.
///     Use this when you want to measure "symbol" itself.
///   - `line(offset)` → absolute line from a raw offset (base + offset).
pub struct FixtureCodeMap {
    base: u32,
    /// (symbol_name, decl_offset, first_safe_logpoint_offset)
    symbols: &'static [(&'static str, u32, u32)],
}

impl FixtureCodeMap {
    pub const fn new(base: u32, symbols: &'static [(&'static str, u32, u32)]) -> Self {
        Self { base, symbols }
    }

    /// Returns the absolute line number where `symbol` is declared.
    /// Panics if `symbol` is not in the map.
    pub fn find_decl(&self, symbol: &str) -> u32 {
        for (name, decl_offset, _) in self.symbols {
            if *name == symbol {
                return self.base + decl_offset;
            }
        }
        let available: Vec<&str> = self.symbols.iter().map(|(n, _, _)| *n).collect();
        panic!(
            "Symbol '{}' not found in FixtureCodeMap. Available: {:?}",
            symbol, available
        )
    }

    /// Returns the first absolute line number where `symbol` is safely in scope
    /// (i.e., after its declaration has fully executed).
    /// Panics if `symbol` is not in the map.
    pub fn find_logpoint(&self, symbol: &str) -> u32 {
        for (name, _, lp_offset) in self.symbols {
            if *name == symbol {
                return self.base + lp_offset;
            }
        }
        let available: Vec<&str> = self.symbols.iter().map(|(n, _, _)| *n).collect();
        panic!(
            "Symbol '{}' not found in FixtureCodeMap. Available: {:?}",
            symbol, available
        )
    }

    /// Returns the absolute line number for a raw offset from the base line.
    pub fn line(&self, offset: u32) -> u32 {
        self.base + offset
    }
}

// Go fixture line numbers — single source of truth.
// Update MAIN_LINE if you add/remove lines before `func tradeTick()` in
// fixtures/go/string_capture/main.go
pub mod go_lines {
    use super::FixtureCodeMap;

    /// Line of `func tradeTick()` in fixtures/go/string_capture/main.go.
    /// All offsets are relative to this line.
    /// Update this if you add/remove lines before `tradeTick`.
    pub const MAIN_LINE: u32 = 103;

    /// Last line of the Order struct (`}` on line 87).
    /// Used by scope-aware tests to verify struct fields are deprioritized.
    pub const ORDER_STRUCT_END: u32 = 87;

    /// Symbol map: (name, decl_offset, first_safe_logpoint_offset)
    ///
    /// Each entry describes a named point in func tradeTick():
    ///   - decl_offset: offset from MAIN_LINE where the symbol is assigned/declared.
    ///   - first_safe_logpoint_offset: first offset where the symbol is safely in scope.
    ///
    /// Background: Delve fires logpoints at the START of a statement (before the
    /// RHS executes), so a variable is NOT yet in scope at its own declaration line.
    const SYMBOL_MAP: &[(&str, u32, u32)] = &[
        // (name,             decl, logpt)
        // Offsets are relative to MAIN_LINE (func tradeTick line).
        ("iteration", 4, 5),      // *iteration++; safe from +5 (symbol)
        ("symbol", 5, 6),         // symbol := ...; safe from +6 (quantity)
        ("quantity", 6, 7),       // quantity := ...; safe from +7 (price)
        ("price", 7, 8),          // price := ...; safe from +8 (direction)
        ("direction", 8, 12),     // direction := ...; safe from +12 (labelConcat)
        ("labelConcat", 12, 17),  // labelConcat := ...; safe from +17 (orderID)
        ("labelSprintf", 14, 17), // labelSprintf := ...; safe from +17 (orderID)
        ("orderID", 17, 20),      // orderID := ...; safe from +20 (entryPrice)
        ("entryPrice", 20, 21),   // entryPrice := ...; safe from +21 (currentPrice)
        ("currentPrice", 21, 22), // currentPrice := ...; safe from +22 (pnl)
        ("pnl", 22, 25),          // pnl := ...; safe from +25 (totalPnl)
        ("totalPnl", 25, 26),     // totalPnl = ...; safe from +26 (lastOrderID)
        ("lastOrderID", 26, 29),  // lastOrderID := ...; safe from +29 (log)
        ("log", 29, 29),          // log(...) call — all vars in scope
        ("sleep", 31, 31),        // time.Sleep — all vars in scope
    ];

    /// Code map for the Go fixture.
    /// Use `CODEMAP.find_decl("x")` to get the line where "x" is declared.
    /// Use `CODEMAP.find_logpoint("x")` to get the first line where "x" is safely in scope.
    pub static CODEMAP: FixtureCodeMap = FixtureCodeMap::new(MAIN_LINE, SYMBOL_MAP);

    /// Helper: absolute line from raw offset (MAIN_LINE + offset).
    /// Prefer `CODEMAP.find_decl()` or `CODEMAP.find_logpoint()` for named points.
    pub const fn line(offset: u32) -> u32 {
        MAIN_LINE + offset
    }
}

// Nested types fixture line numbers — for ebpf_nested_types_e2e.rs tests
// Update NESTED_MAIN_LINE if you add/remove lines before `func main()` in
// fixtures/go/nested_types/main.go
pub mod go_nested_lines {
    use super::FixtureCodeMap;

    /// Line of `func main()` in nested_types/main.go
    pub const NESTED_MAIN_LINE: u32 = 126;

    /// Symbol map for nested types fixture
    const SYMBOL_MAP: &[(&str, u32, u32)] = &[
        // (name,             decl, logpt)
        ("order", 28, 30),      // order := createRandomOrder()
        ("history", 35, 36),    // history := PriceHistory{...}
        ("ptrWrapper", 39, 40), // ptrWrapper := OrderPtr{...}
        ("logOrder", 42, 42),   // logOrder(order) call
        ("i", 46, 46),          // for i, item := range order.Items
        ("item", 46, 46),
        ("key", 52, 52), // for key, tag := range order.Tags
        ("tag", 52, 52),
        ("status", 57, 57),       // status := order.Status
        ("categoryName", 60, 60), // categoryName := order.Product.Category.Name
        ("timestamp", 63, 63),    // timestamp := order.Timestamp
    ];

    /// Code map for the nested types fixture.
    pub static CODEMAP: FixtureCodeMap = FixtureCodeMap::new(NESTED_MAIN_LINE, SYMBOL_MAP);

    pub const fn line(offset: u32) -> u32 {
        NESTED_MAIN_LINE + offset
    }
}

// Classic map fixture line numbers — for ebpf_classic_map_e2e.rs tests
// Tests map capture with Go < 1.24 classic hash map implementation (hmap/bmap).
// Update CLASSIC_MAIN_LINE if you add/remove lines before `func main()` in
// fixtures/go/classic_map/main.go
//
// DWARF line verification (go tool objdump -s main.main):
//   main.go:30 - func main() starts (MAIN_LINE)
//   main.go:37 - iteration := 0
//   main.go:39 - iteration++
//   main.go:42 - order := Order{...} struct creation begins
//   main.go:48 - order struct creation ends (Tags map assigned)
//   main.go:51 - var nilMap map[string]int
//   main.go:54 - logOrder(order) call
//   main.go:57 - logNilMap(nilMap) call
//   main.go:59 - time.Sleep(3 * time.Second)
//
// Key insight: The struct wrapper pattern (Order.Tags) is proven working.
// Standalone maps may have incomplete DWARF location info in Go 1.23.
pub mod go_classic_lines {
    use super::FixtureCodeMap;

    /// Line of `func main()` in classic_map/main.go
    pub const CLASSIC_MAIN_LINE: u32 = 30;

    /// Symbol map for classic map fixture.
    /// Uses struct-wrapped map pattern: Order struct contains Tags map[string]string.
    const SYMBOL_MAP: &[(&str, u32, u32)] = &[
        // (name,            decl, logpt)
        // iteration: simple int, good for smoke testing uprobe mechanism
        ("iteration", 7, 9), // line 37: iteration := 0; line 39: iteration++
        // order: struct with map field (struct wrapper pattern)
        ("order", 12, 24), // line 42: order := Order{...}; line 54: logOrder(order)
        // nilMap: nil map pointer (edge case)
        ("nilMap", 21, 27), // line 51: var nilMap; line 57: logNilMap(nilMap)
    ];

    /// Code map for the classic map fixture.
    pub static CODEMAP: FixtureCodeMap = FixtureCodeMap::new(CLASSIC_MAIN_LINE, SYMBOL_MAP);

    pub const fn line(offset: u32) -> u32 {
        CLASSIC_MAIN_LINE + offset
    }
}

/// Python fixture line numbers — single source of truth.
/// Update MAIN_LINE if you add/remove lines before `def main()` in
/// fixtures/python/trade_bot_forever.py
pub mod python_lines {
    use super::FixtureCodeMap;

    /// Line of `def main():` in trade_bot_forever.py.
    pub const MAIN_LINE: u32 = 44;

    /// Symbol map: (name, decl_offset_from_main, first_safe_logpoint_offset)
    ///
    /// Python uses absolute line numbers as offsets with MAIN_LINE = 44.
    /// Note: debugpy moves breakpoints on blank/comment lines to the next
    /// executable line automatically, so those are valid logpoint targets.
    const SYMBOL_MAP: &[(&str, u32, u32)] = &[
        // (name,             decl, logpt)  absolute line = MAIN_LINE + offset
        ("symbol", 11, 19), // L55: symbol = ...; logpoint at L63 (entry_price line)
        ("quantity", 12, 18), // L56: quantity = ...; logpoint at L62 (comment → L63)
        ("price", 13, 15),  // L57: price = ...; logpoint at L59 (comment → L60)
        ("order_id", 16, 22), // L60: order_id = ...; logpoint at L66 (blank → L67)
        ("entry_price", 19, 20), // L63: entry_price = ...; logpoint at L64 (current_price)
        ("current_price", 20, 21), // L64: current_price = ...; logpoint at L65 (pnl)
        ("pnl", 21, 23),    // L65: pnl = ...; logpoint at L67 (first print)
        ("print1", 23, 23), // L67: print(...) — all vars in scope
        ("sleep", 25, 25),  // L69: blank → L70 (time.sleep) — all vars in scope
    ];

    /// Code map for the Python fixture.
    pub static CODEMAP: FixtureCodeMap = FixtureCodeMap::new(MAIN_LINE, SYMBOL_MAP);
}

/// Rust fixture line numbers — single source of truth.
/// Update MAIN_LINE if you add/remove lines before `fn main()` in
/// fixtures/rust/src/main.rs
pub mod rust_lines {
    use super::FixtureCodeMap;

    /// Line of `fn main()` in fixtures/rust/src/main.rs.
    pub const MAIN_LINE: u32 = 76;

    /// Symbol map: (name, decl_offset, first_safe_logpoint_offset)
    ///
    /// lldb-dap fires logpoints at the START of a statement (before the
    /// RHS executes), so a variable is NOT yet in scope at its own declaration.
    const SYMBOL_MAP: &[(&str, u32, u32)] = &[
        // (name,             decl, logpt)
        ("symbol", 31, 33),   // L107: let symbol = ...; safe from +33 (quantity)
        ("quantity", 33, 35), // L109: let quantity = ...; safe from +35 (price)
        ("price", 35, 38),    // L111: let price = ...; safe from +38 (order_id)
        ("order_id", 38, 41), // L114: let order_id = ...; safe from +41 (entry_price)
        ("entry_price", 41, 43), // L117: let entry_price = ...; safe from +43 (current_price)
        ("current_price", 43, 45), // L119: let current_price = ...; safe from +45 (pnl)
        ("pnl", 45, 52),      // L121: let pnl = ...; safe from +52 (log!)
        ("log", 52, 52),      // L128: log! macro — all vars in scope
        ("flush", 54, 54),    // L130: stderr().flush() — all vars in scope
        ("sleep", 56, 56),    // L132: thread::sleep() — all vars in scope
    ];

    /// Code map for the Rust fixture.
    pub static CODEMAP: FixtureCodeMap = FixtureCodeMap::new(MAIN_LINE, SYMBOL_MAP);
}

/// Configuration for a metric point in a specific language
#[derive(Debug, Clone)]
pub struct MetricPoint {
    /// Name of the metric
    pub name: String,
    /// Line number in the source file
    pub line: u32,
    /// Expression to evaluate
    pub expression: String,
    /// Additional expressions for multi-expression metrics
    pub extra_expressions: Vec<String>,
    /// Optional group name
    pub group: Option<String>,
    /// Whether to capture stack trace
    pub capture_stack_trace: bool,
    /// Whether to capture memory snapshot
    pub capture_memory_snapshot: bool,
}

impl MetricPoint {
    pub fn new(name: &str, line: u32, expression: &str) -> Self {
        Self {
            name: name.to_string(),
            line,
            expression: expression.to_string(),
            extra_expressions: vec![],
            group: None,
            capture_stack_trace: false,
            capture_memory_snapshot: false,
        }
    }

    pub fn with_extra_expressions(mut self, extra: Vec<&str>) -> Self {
        self.extra_expressions = extra.into_iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn with_group(mut self, group: &str) -> Self {
        self.group = Some(group.to_string());
        self
    }

    pub fn with_stack_trace(mut self) -> Self {
        self.capture_stack_trace = true;
        self
    }

    pub fn with_memory_snapshot(mut self) -> Self {
        self.capture_memory_snapshot = true;
        self
    }

    pub fn with_introspection(mut self) -> Self {
        self.capture_stack_trace = true;
        self.capture_memory_snapshot = true;
        self
    }
}

/// Configuration for a language-specific DAP workflow test
#[derive(Debug, Clone)]
pub struct DapWorkflowConfig {
    /// Language being tested (uses SourceLanguage from detrix-core)
    pub language: SourceLanguage,
    /// Path to the source file (relative to workspace root)
    pub source_file: PathBuf,
    /// Metrics to add during the test
    pub metrics: Vec<MetricPoint>,
    /// Metrics with introspection (stack trace + memory snapshot) enabled
    pub introspection_metrics: Vec<MetricPoint>,
    /// Line to inspect before setting metrics
    pub inspect_line: u32,
    /// Variable to check scope for
    pub inspect_variable: String,
    /// Invalid metric (variable not in scope) for error testing
    pub invalid_metric: Option<MetricPoint>,
    /// Group name for the workflow
    pub group_name: String,
    /// How long to wait for events (seconds)
    pub event_wait_secs: u64,
    /// How long the target program runs per iteration (for timing estimates)
    pub iteration_duration_ms: u64,
}

impl DapWorkflowConfig {
    /// Create a Python workflow config using trade_bot_forever.py
    ///
    /// Line numbers are derived from `python_lines::CODEMAP`.
    /// Update `python_lines::MAIN_LINE` if you add/remove lines before `def main()`.
    ///
    /// IMPORTANT: DAP logpoints evaluate BEFORE the line executes. debugpy moves
    /// breakpoints on blank/comment lines to the next executable line. We exploit
    /// this to place logpoints at comment/blank lines that fall after all needed
    /// variable declarations have executed.
    pub fn python() -> Self {
        use python_lines::CODEMAP;
        Self {
            language: SourceLanguage::Python,
            source_file: PathBuf::from("fixtures/python/trade_bot_forever.py"),
            metrics: vec![
                // order_id assigned at L60; logpoint at L66 (blank → L67 print) — order_id in scope
                MetricPoint::new(
                    "order_metric",
                    CODEMAP.find_logpoint("order_id"),
                    "order_id",
                )
                .with_group("python_workflow"),
                // price assigned at L57; logpoint at L59 (comment → L60 order_id) — price in scope
                MetricPoint::new("price_metric", CODEMAP.find_logpoint("price"), "price")
                    .with_group("python_workflow"),
                // IMPORTANT: Each metric must be on a DIFFERENT line (one logpoint per line).
                // quantity assigned at L56; logpoint at L62 (comment → L63 entry_price) — in scope
                MetricPoint::new(
                    "quantity_metric",
                    CODEMAP.find_logpoint("quantity"),
                    "quantity",
                )
                .with_group("python_workflow"),
                // symbol assigned at L55; logpoint at L63 (entry_price line) — symbol in scope
                MetricPoint::new("symbol_metric", CODEMAP.find_logpoint("symbol"), "symbol")
                    .with_group("python_workflow"),
                // Multi-expression: L64 (current_price line) — symbol, quantity, price all in scope
                MetricPoint::new(
                    "trade_details",
                    CODEMAP.find_logpoint("entry_price"),
                    "symbol",
                )
                .with_extra_expressions(vec!["quantity", "price"])
                .with_group("python_workflow"),
            ],
            // Introspection metrics: stack trace and memory snapshot capture.
            // Each metric MUST be on a different line (one metric per line).
            introspection_metrics: vec![
                // L66 (blank → L67 print) — all vars in scope
                MetricPoint::new(
                    "stack_trace_metric",
                    CODEMAP.find_logpoint("order_id"),
                    "order_id",
                )
                .with_group("python_introspection")
                .with_stack_trace(),
                // L67 (first print) — all vars in scope
                MetricPoint::new(
                    "memory_snapshot_metric",
                    CODEMAP.find_logpoint("pnl"),
                    "price",
                )
                .with_group("python_introspection")
                .with_memory_snapshot(),
                // L69 (blank → L70 time.sleep) — all vars in scope
                MetricPoint::new(
                    "full_introspection_metric",
                    CODEMAP.find_logpoint("sleep"),
                    "quantity",
                )
                .with_group("python_introspection")
                .with_introspection(),
            ],
            inspect_line: CODEMAP.find_logpoint("order_id"),
            inspect_variable: "price".to_string(),
            invalid_metric: Some(
                MetricPoint::new(
                    "bad_metric",
                    CODEMAP.find_logpoint("order_id"),
                    "nonexistent_var",
                )
                .with_group("python_workflow"),
            ),
            group_name: "python_workflow".to_string(),
            event_wait_secs: 15,
            iteration_duration_ms: 3000, // 3 seconds per trade
        }
    }

    /// Create a Go workflow config using detrix_example_app.go
    ///
    /// Line numbers are derived from `go_lines::CODEMAP`.
    /// Update `go_lines::MAIN_LINE` if you add/remove lines before `func main()`.
    ///
    /// IMPORTANT: Delve fires logpoints at the START of a statement (before the RHS
    /// executes), so a variable is NOT yet in scope at its own declaration line.
    /// We identify each "slot" (line) by the variable declared there via `find_decl`,
    /// and place a metric for a DIFFERENT variable that is already in scope at that slot.
    pub fn go() -> Self {
        use go_lines::CODEMAP;

        Self {
            language: SourceLanguage::Go,
            source_file: PathBuf::from("fixtures/go/string_capture/main.go"),
            metrics: vec![
                // Slot: pnl declaration line — orderID is already in scope here
                MetricPoint::new("order_metric", CODEMAP.find_decl("pnl"), "orderID")
                    .with_group("go_workflow"),
                // Slot: orderID declaration line — price is already in scope here
                MetricPoint::new("price_metric", CODEMAP.find_decl("orderID"), "price")
                    .with_group("go_workflow"),
                // IMPORTANT: Each metric must be on a DIFFERENT line (one logpoint per line).
                // Slot: entryPrice declaration line — quantity is already in scope here
                MetricPoint::new(
                    "quantity_metric",
                    CODEMAP.find_decl("entryPrice"),
                    "quantity",
                )
                .with_group("go_workflow"),
                // Slot: currentPrice declaration line — symbol is already in scope here
                MetricPoint::new("symbol_metric", CODEMAP.find_decl("currentPrice"), "symbol")
                    .with_group("go_workflow"),
                // Multi-expression: time.Sleep line — all vars in scope
                MetricPoint::new("trade_details", CODEMAP.find_decl("sleep"), "symbol")
                    .with_extra_expressions(vec!["quantity", "price"])
                    .with_group("go_workflow"),
            ],
            // Introspection metrics: stack trace and memory snapshot capture.
            // Each metric MUST be on a different line (one metric per line).
            // Lines must be REAL executable statements — Delve requires actual code.
            introspection_metrics: vec![
                // totalPnl update: `totalPnl = totalPnl + pnl` — real assignment
                MetricPoint::new(
                    "stack_trace_metric",
                    CODEMAP.find_decl("totalPnl"),
                    "orderID",
                )
                .with_group("go_introspection")
                .with_stack_trace(),
                // lastOrderID declaration: `lastOrderID := orderID` — real assignment
                MetricPoint::new(
                    "memory_snapshot_metric",
                    CODEMAP.find_decl("lastOrderID"),
                    "price",
                )
                .with_group("go_introspection")
                .with_memory_snapshot(),
                // log() call — function call, all vars in scope
                MetricPoint::new(
                    "full_introspection_metric",
                    CODEMAP.find_decl("log"),
                    "quantity",
                )
                .with_group("go_introspection")
                .with_introspection(),
            ],
            inspect_line: CODEMAP.find_decl("pnl"),
            inspect_variable: "price".to_string(),
            invalid_metric: Some(
                // symbol line: good for "not in scope" test (expression "nonexistent_var" won't be found)
                MetricPoint::new("bad_metric", CODEMAP.find_decl("symbol"), "nonexistent_var")
                    .with_group("go_workflow"),
            ),
            group_name: "go_workflow".to_string(),
            event_wait_secs: 15,
            iteration_duration_ms: 3000, // 3 seconds per trade (same as Python)
        }
    }

    /// Create a Rust workflow config using fixtures/rust/src/main.rs
    ///
    /// Line numbers are derived from `rust_lines::CODEMAP`.
    /// Update `rust_lines::MAIN_LINE` if you add/remove lines before `fn main()`.
    ///
    /// IMPORTANT: lldb-dap fires logpoints at the START of a statement (before the
    /// RHS executes), so a variable is NOT yet in scope at its own declaration line.
    /// We identify each "slot" (line) by the variable declared there via `find_decl`,
    /// and place a metric for a DIFFERENT variable that is already in scope at that slot.
    pub fn rust() -> Self {
        use rust_lines::CODEMAP;

        Self {
            language: SourceLanguage::Rust,
            source_file: PathBuf::from("fixtures/rust/src/main.rs"),
            metrics: vec![
                // Slot: entry_price declaration line — order_id is already in scope here
                MetricPoint::new("order_metric", CODEMAP.find_decl("entry_price"), "order_id")
                    .with_group("rust_workflow"),
                // Slot: order_id declaration line — price is already in scope here
                MetricPoint::new("price_metric", CODEMAP.find_decl("order_id"), "price")
                    .with_group("rust_workflow"),
                // IMPORTANT: Each metric must be on a DIFFERENT line (one logpoint per line).
                // Slot: current_price declaration line — quantity is already in scope here
                MetricPoint::new(
                    "quantity_metric",
                    CODEMAP.find_decl("current_price"),
                    "quantity",
                )
                .with_group("rust_workflow"),
                // Slot: pnl declaration line — symbol is already in scope here
                MetricPoint::new("symbol_metric", CODEMAP.find_decl("pnl"), "symbol")
                    .with_group("rust_workflow"),
                // NOTE: No multi-expression metric for Rust — all executable lines are taken
                // by other metrics, and `let _ = x` dead assignments are unreliable breakpoint
                // targets with lldb-dap. Multi-expression DAP coverage is provided by Python
                // and Go workflows.
            ],
            // Introspection metrics: stack trace and memory snapshot capture.
            // Use distinct function call lines for reliable breakpoint placement.
            introspection_metrics: vec![
                // log! macro — all variables in scope
                MetricPoint::new("stack_trace_metric", CODEMAP.find_decl("log"), "order_id")
                    .with_group("rust_introspection")
                    .with_stack_trace(),
                // stderr().flush() — I/O syscall
                MetricPoint::new(
                    "memory_snapshot_metric",
                    CODEMAP.find_decl("flush"),
                    "price",
                )
                .with_group("rust_introspection")
                .with_memory_snapshot(),
                // thread::sleep() — function call
                MetricPoint::new(
                    "full_introspection_metric",
                    CODEMAP.find_decl("sleep"),
                    "quantity",
                )
                .with_group("rust_introspection")
                .with_introspection(),
            ],
            inspect_line: CODEMAP.find_decl("log"),
            inspect_variable: "price".to_string(),
            invalid_metric: Some(
                MetricPoint::new(
                    "bad_metric",
                    CODEMAP.find_decl("entry_price"),
                    "nonexistent_var",
                )
                .with_group("rust_workflow"),
            ),
            group_name: "rust_workflow".to_string(),
            event_wait_secs: 15,
            iteration_duration_ms: 3000, // 3 seconds per trade (same as Python)
        }
    }
}

/// DAP Workflow Scenarios - reusable test logic for multi-language DAP testing
pub struct DapWorkflowScenarios;

impl DapWorkflowScenarios {
    /// Run the complete workflow scenario for a given language configuration
    ///
    /// This is a DAP-specific test that:
    /// 1. Connects to the debugger
    /// 2. Inspects code before setting metrics
    /// 3. Adds multiple metrics to a group
    /// 4. Tests invalid metric handling
    /// 5. Enables group and captures events
    /// 6. Disables individual metric and verifies
    /// 7. Disables group and verifies
    /// 8. Cleans up
    ///
    /// # Arguments
    /// * `program_path` - Optional path to program binary (required for Rust direct lldb-dap mode)
    #[allow(clippy::too_many_arguments)]
    pub async fn run_workflow<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        config: &DapWorkflowConfig,
        debugger_port: u16,
        source_file_path: &std::path::Path,
        program_path: Option<&str>,
    ) -> Result<WorkflowResult, String> {
        let mut result = WorkflowResult::default();

        // ========================================================================
        // PHASE 1: Setup & Connection
        // ========================================================================
        reporter.section(&format!(
            "PHASE 1: SETUP & CONNECTION ({})",
            config.language.dap_display_name()
        ));

        // Step 1: Health check
        let step = reporter.step_start("Health Check", "Verify API is healthy");
        match client.health().await {
            Ok(true) => reporter.step_success(step, Some("API healthy")),
            Ok(false) => {
                reporter.step_failed(step, "API unhealthy");
                return Err("API unhealthy".to_string());
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                return Err(format!("Health check failed: {}", e));
            }
        }

        // Step 2: Connect to debugger
        let step = reporter.step_start(
            "Create Connection",
            &format!(
                "Connect to {} at 127.0.0.1:{}",
                config.language.dap_display_name(),
                debugger_port
            ),
        );
        reporter.step_request(
            "create_connection",
            Some(&format!(
                "port={}, language={}, program={:?}",
                debugger_port,
                config.language.as_api_str(),
                program_path
            )),
        );

        let connection_id = match client
            .create_connection_with_program(
                "127.0.0.1",
                debugger_port,
                config.language.as_api_str(),
                program_path,
            )
            .await
        {
            Ok(r) => {
                reporter.step_response("OK", Some(&format!("id={}", r.data.connection_id)));
                reporter.step_success(step, Some(&format!("Connected: {}", r.data.connection_id)));
                r.data.connection_id
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                return Err(format!("Failed to connect: {}", e));
            }
        };

        // Wait for DAP handshake
        tokio::time::sleep(Duration::from_secs(2)).await;

        // ========================================================================
        // PHASE 2: Code Inspection
        // ========================================================================
        reporter.section("PHASE 2: CODE INSPECTION");

        // Inspect code at target line
        let step = reporter.step_start(
            "Inspect Code",
            &format!(
                "View code around line {} in {}",
                config.inspect_line,
                config.source_file.display()
            ),
        );
        reporter.step_request(
            "inspect_file",
            Some(&format!(
                "file={}, line={}",
                source_file_path.display(),
                config.inspect_line
            )),
        );

        match client
            .inspect_file(
                source_file_path.to_str().unwrap(),
                Some(config.inspect_line),
                None,
            )
            .await
        {
            Ok(r) => {
                reporter.step_response("OK", Some(&format!("{} chars of context", r.data.len())));
                if !r.data.is_empty() {
                    let preview: String = r.data.chars().take(100).collect();
                    reporter.info(&format!("Code preview: {}...", preview));
                }
                reporter.step_success(step, Some("Code context retrieved"));
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.warn("Continuing without code inspection...");
            }
        }

        // Inspect variable scope
        let step = reporter.step_start(
            "Inspect Variable",
            &format!(
                "Check if '{}' is in scope at line {}",
                config.inspect_variable, config.inspect_line
            ),
        );
        reporter.step_request(
            "inspect_file",
            Some(&format!(
                "file={}, variable={}",
                source_file_path.display(),
                config.inspect_variable
            )),
        );

        match client
            .inspect_file(
                source_file_path.to_str().unwrap(),
                None,
                Some(&config.inspect_variable),
            )
            .await
        {
            Ok(r) => {
                reporter.step_response("OK", Some(&format!("{} chars", r.data.len())));
                reporter.step_success(step, Some("Variable info retrieved"));
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.warn("Continuing without variable inspection...");
            }
        }

        // ========================================================================
        // PHASE 3: Add Metrics
        // ========================================================================
        reporter.section("PHASE 3: ADD METRICS TO GROUP");

        // Add valid metrics as DISABLED (will be enabled via enable_group to test group operations)
        for metric in &config.metrics {
            let location = format!("@{}#{}", source_file_path.display(), metric.line);
            let step = reporter.step_start(
                "Add Metric (Disabled)",
                &format!(
                    "Add '{}' at line {} for '{}' (disabled, will enable via group)",
                    metric.name, metric.line, metric.expression
                ),
            );
            reporter.step_request(
                "add_metric",
                Some(&format!(
                    "name={}, location={}, expr={}, enabled=false",
                    metric.name, location, metric.expression
                )),
            );

            let mut request = if metric.extra_expressions.is_empty() {
                super::client::AddMetricRequest::new(
                    &metric.name,
                    &location,
                    &metric.expression,
                    &connection_id,
                )
            } else {
                let mut all_exprs = vec![metric.expression.clone()];
                all_exprs.extend(metric.extra_expressions.clone());
                super::client::AddMetricRequest::new_multi(
                    &metric.name,
                    &location,
                    all_exprs,
                    &connection_id,
                )
            };
            request.language = Some(config.language.as_api_str().to_string());
            request.group = metric.group.clone();
            request.enabled = Some(false); // Add as disabled - will enable via enable_group

            match client.add_metric(request).await {
                Ok(r) => {
                    reporter.step_response("OK", Some(&format!("id={} (disabled)", r.data)));
                    reporter.step_success(
                        step,
                        Some(&format!(
                            "Added '{}' as disabled (id={})",
                            metric.name, r.data
                        )),
                    );
                    result.metrics_added += 1;
                }
                Err(e) => {
                    reporter.step_failed(step, &e.to_string());
                    return Err(format!("Failed to add metric '{}': {}", metric.name, e));
                }
            }
        }

        // Try adding invalid metric (expect error due to scope validation)
        if let Some(invalid) = &config.invalid_metric {
            let location = format!("@{}#{}", source_file_path.display(), invalid.line);
            let step = reporter.step_start(
                "Add Invalid Metric",
                if config.language.capabilities().has_scope_validation {
                    "Try adding metric for variable not in scope - MUST fail"
                } else {
                    "Try adding metric for variable not in scope - may or may not fail"
                },
            );
            reporter.step_request(
                "add_metric",
                Some(&format!(
                    "name={}, expr={} (NOT IN SCOPE)",
                    invalid.name, invalid.expression
                )),
            );

            let mut invalid_request = super::client::AddMetricRequest::new(
                &invalid.name,
                &location,
                &invalid.expression,
                &connection_id,
            );
            invalid_request.language = Some(config.language.as_api_str().to_string());
            invalid_request.group = invalid.group.clone();

            match client.add_metric(invalid_request).await {
                Ok(r) => {
                    if config.language.capabilities().has_scope_validation {
                        // Scope validation was expected but metric was added - this is a test FAILURE
                        reporter.step_response(
                            "ERROR",
                            Some(&format!(
                                "Added anyway (id={}) but should have failed!",
                                r.data
                            )),
                        );
                        reporter.step_failed(
                            step,
                            "Scope validation should have rejected invalid variable",
                        );
                        return Err(format!(
                            "Invalid metric '{}' was accepted but {} supports scope validation",
                            invalid.name,
                            config.language.dap_display_name()
                        ));
                    } else {
                        // Scope validation not supported - metric being added is expected
                        reporter
                            .step_response("OK", Some(&format!("Added anyway (id={})", r.data)));
                        reporter
                            .warn("Scope validation not supported for this language (expected)");
                    }
                }
                Err(e) => {
                    // Expected: scope validation should reject invalid variable
                    let error_str = e.to_string();
                    if error_str.contains("not in scope") || error_str.contains("Invalid") {
                        reporter.step_response("ERROR", Some("Variable not in scope (expected)"));
                        reporter.step_success(
                            step,
                            Some("Scope validation correctly rejected invalid variable"),
                        );
                    } else {
                        reporter.step_response("ERROR", Some(&error_str));
                        reporter.step_success(
                            step,
                            Some("Metric rejected (possibly for different reason)"),
                        );
                    }
                }
            }
        }

        // ========================================================================
        // PHASE 4: Enable Group & Capture Events
        // ========================================================================
        reporter.section("PHASE 4: ENABLE GROUP & CAPTURE EVENTS");

        // Enable the group - should enable the disabled metrics we added
        let expected_enabled = config.metrics.len();
        let step = reporter.step_start(
            "Enable Group",
            &format!(
                "Enable '{}' (expecting {} metrics to be enabled)",
                config.group_name, expected_enabled
            ),
        );
        reporter.step_request("enable_group", Some(&config.group_name));

        match client.enable_group(&config.group_name).await {
            Ok(r) => {
                let enabled_count = r.data;
                reporter.step_response("OK", Some(&format!("{} metrics enabled", enabled_count)));

                // Verify that metrics were actually enabled - FAIL if count doesn't match
                if enabled_count != expected_enabled {
                    let error_msg = if enabled_count == 0 {
                        format!(
                            "enable_group returned 0 metrics - expected {} to be enabled. \
                            This indicates group name mismatch or metrics not saved with correct group.",
                            expected_enabled
                        )
                    } else {
                        format!(
                            "enable_group returned {} metrics - expected {} to be enabled",
                            enabled_count, expected_enabled
                        )
                    };
                    reporter.step_failed(step, &error_msg);
                    return Err(error_msg);
                }

                reporter.step_success(step, Some(&format!("{} metrics enabled", enabled_count)));
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                return Err(format!("Failed to enable group: {}", e));
            }
        }

        // Wait for events
        let step = reporter.step_start(
            "Wait for Events",
            &format!(
                "Wait for metrics to capture data (up to {}s)",
                config.event_wait_secs
            ),
        );
        reporter.info(&format!(
            "Target runs every ~{}ms...",
            config.iteration_duration_ms
        ));

        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(config.event_wait_secs);

        loop {
            if start.elapsed() > timeout {
                // Check which metrics failed to capture events
                let mut missing_metrics = Vec::new();
                for metric in &config.metrics {
                    let count = client
                        .query_events(&metric.name, 100)
                        .await
                        .map(|r| r.data.len())
                        .unwrap_or(0);
                    if count == 0 {
                        missing_metrics.push(metric.name.clone());
                    }
                }
                if !missing_metrics.is_empty() {
                    let error_msg = format!(
                        "Timeout: metrics failed to capture events after {}s: {}. \
                        All metrics must capture at least one event for isolation tests to work.",
                        config.event_wait_secs,
                        missing_metrics.join(", ")
                    );
                    reporter.step_failed(step, &error_msg);
                    return Err(error_msg);
                }
                break;
            }

            // Check events for each metric - require ALL metrics to have at least 1 event
            let mut total_events = 0;
            let mut metrics_with_events = 0;
            for metric in &config.metrics {
                if let Ok(r) = client.query_events(&metric.name, 100).await {
                    let count = r.data.len();
                    if count > 0 {
                        reporter.info(&format!("  {} captured {} event(s)", metric.name, count));
                        total_events += count;
                        metrics_with_events += 1;

                        // Log event details
                        for (i, event) in r.data.iter().enumerate() {
                            // Print to stdout for test visibility
                            reporter.info(&format!(
                                "    [Event {}] value={}, timestamp={}, is_error={}",
                                i, event.value, event.timestamp_iso, event.is_error
                            ));
                            debug!(
                                metric = %metric.name,
                                event_index = i,
                                value = %event.value,
                                timestamp = %event.timestamp_iso,
                                is_error = event.is_error,
                                "Event captured"
                            );
                        }
                    }
                }
            }

            // Require ALL metrics to have at least 1 event for proper isolation testing
            if metrics_with_events == config.metrics.len() {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "{} total events captured ({}/{} metrics active)",
                        total_events,
                        metrics_with_events,
                        config.metrics.len()
                    )),
                );
                result.total_events = total_events;
                break;
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        // Verify multi-expression metrics have correct expression count AND event values
        for metric in &config.metrics {
            if !metric.extra_expressions.is_empty() {
                let expected_expr_count = 1 + metric.extra_expressions.len();

                // Check metric has correct expression count
                let step = reporter.step_start(
                    "Verify Multi-Expr Metric",
                    &format!(
                        "Check '{}' has {} expressions",
                        metric.name, expected_expr_count
                    ),
                );

                match client.list_metrics().await {
                    Ok(metrics_list) => {
                        if let Some(found) =
                            metrics_list.data.iter().find(|m| m.name == metric.name)
                        {
                            if found.expressions.len() == expected_expr_count {
                                reporter.step_success(
                                    step,
                                    Some(&format!(
                                        "Multi-expr metric '{}' has {} expressions: [{}]",
                                        metric.name,
                                        found.expressions.len(),
                                        found.expressions.join(", ")
                                    )),
                                );
                            } else {
                                reporter.warn(&format!(
                                    "Multi-expr metric '{}' has {} expressions (expected {}): [{}]",
                                    metric.name,
                                    found.expressions.len(),
                                    expected_expr_count,
                                    found.expressions.join(", ")
                                ));
                            }
                        } else {
                            reporter.warn(&format!(
                                "Multi-expr metric '{}' not found in list",
                                metric.name
                            ));
                        }
                    }
                    Err(e) => {
                        reporter.warn(&format!(
                            "Failed to list metrics for multi-expr check: {}",
                            e
                        ));
                    }
                }

                // Check events have correct number of expression values
                let step = reporter.step_start(
                    "Verify Multi-Expr Events",
                    &format!(
                        "Check '{}' events have {} expression values",
                        metric.name, expected_expr_count
                    ),
                );

                match client.query_events(&metric.name, 10).await {
                    Ok(events) => {
                        if let Some(first_event) = events.data.first() {
                            reporter.info(&format!(
                                "  Event values count={}, values={:?}",
                                first_event.values.len(),
                                first_event.values,
                            ));

                            if first_event.values.len() >= expected_expr_count {
                                // Verify each value has non-empty valueJson
                                let mut all_valid = true;
                                for (i, val) in first_event.values.iter().enumerate() {
                                    let value_json = val
                                        .get("valueJson")
                                        .or_else(|| val.get("value_json"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    if value_json.is_empty() {
                                        reporter.warn(&format!(
                                            "  Expression [{}] has empty valueJson: {:?}",
                                            i, val
                                        ));
                                        all_valid = false;
                                    }
                                }

                                if all_valid {
                                    reporter.step_success(
                                        step,
                                        Some(&format!(
                                            "Multi-expr metric '{}' events have {} values, all non-empty",
                                            metric.name,
                                            first_event.values.len(),
                                        )),
                                    );
                                } else {
                                    reporter.warn(&format!(
                                        "Multi-expr metric '{}' has some empty expression values",
                                        metric.name
                                    ));
                                }
                            } else {
                                reporter.warn(&format!(
                                    "Multi-expr metric '{}' events have {} values (expected {})",
                                    metric.name,
                                    first_event.values.len(),
                                    expected_expr_count,
                                ));
                            }
                        } else {
                            reporter.warn(&format!(
                                "No events found for multi-expr metric '{}'",
                                metric.name
                            ));
                        }
                    }
                    Err(e) => {
                        reporter.warn(&format!(
                            "Failed to query events for multi-expr check: {}",
                            e
                        ));
                    }
                }
            }
        }

        // ========================================================================
        // PHASE 5: Disable Individual Metric
        // ========================================================================
        reporter.section("PHASE 5: DISABLE INDIVIDUAL METRIC");

        if let Some(first_metric) = config.metrics.first() {
            // Get baseline event counts for ALL metrics
            let baseline_disabled = client
                .query_events(&first_metric.name, 100)
                .await
                .map(|r| r.data.len())
                .unwrap_or(0);

            // Get baseline for another metric (if exists) to verify it keeps capturing
            let other_metric = config.metrics.get(1);
            let baseline_other = if let Some(other) = other_metric {
                client
                    .query_events(&other.name, 100)
                    .await
                    .map(|r| r.data.len())
                    .unwrap_or(0)
            } else {
                0
            };

            let step = reporter.step_start(
                "Disable One Metric",
                &format!("Disable '{}' (should stop capturing)", first_metric.name),
            );
            reporter.step_request(
                "toggle_metric",
                Some(&format!("name={}, enabled=false", first_metric.name)),
            );

            match client.toggle_metric(&first_metric.name, false).await {
                Ok(_) => {
                    reporter.step_response("OK", Some("Metric disabled"));
                    reporter.step_success(step, Some(&format!("'{}' disabled", first_metric.name)));
                }
                Err(e) => {
                    reporter.step_failed(step, &e.to_string());
                }
            }

            // Wait for at least 2 iterations to ensure reliable event capture
            // Using iteration_duration_ms * 2.5 to account for timing variability
            let wait_ms = (config.iteration_duration_ms as f64 * 2.5) as u64;
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;

            // Verify disabled metric stopped
            let step = reporter.step_start(
                "Verify Metric Stopped",
                "Check that disabled metric doesn't capture new events",
            );
            let new_count_disabled = client
                .query_events(&first_metric.name, 100)
                .await
                .map(|r| r.data.len())
                .unwrap_or(0);

            // Calculate allowed tolerance based on wait time and iteration duration
            // Allow for: in-flight events + DAP propagation delay + buffer
            // wait_ms / iteration_duration_ms = ~2.5 iterations waited
            //
            // Tolerance calculation:
            //   - ceil(iterations_waited) for events that could fire during the wait
            //   - +7 buffer for:
            //     * Events in-flight when disable was called
            //     * REST/HTTP/gRPC processing latency (multi-layer)
            //     * DAP adapter communication delay
            //     * Timing variability between API layers
            //     * Parallel test interference
            //     * Event queue flush delays
            //
            // Note: The generous buffer is needed because disable is async -
            // the API returns success before DAP actually removes the breakpoint
            let max_allowed_new_events =
                ((wait_ms as f64 / config.iteration_duration_ms as f64).ceil() as usize) + 7;
            let threshold = baseline_disabled + max_allowed_new_events;

            if new_count_disabled <= threshold {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Metric capture reduced (before={}, after={}, threshold={})",
                        baseline_disabled, new_count_disabled, threshold
                    )),
                );
            } else {
                let error_msg = format!(
                    "Disabled metric '{}' still capturing too many events (before={}, after={}, threshold={})",
                    first_metric.name, baseline_disabled, new_count_disabled, threshold
                );
                reporter.step_failed(step, &error_msg);
                return Err(error_msg);
            }

            // Verify OTHER metrics CONTINUE capturing (important test!)
            if let Some(other) = other_metric {
                let step = reporter.step_start(
                    "Verify Others Continue",
                    &format!("Check that '{}' still captures events", other.name),
                );
                let new_count_other = client
                    .query_events(&other.name, 100)
                    .await
                    .map(|r| r.data.len())
                    .unwrap_or(0);

                if new_count_other > baseline_other {
                    reporter.step_success(
                        step,
                        Some(&format!(
                            "Other metric still capturing (before={}, after={})",
                            baseline_other, new_count_other
                        )),
                    );
                } else if baseline_other == 0 {
                    // If baseline was already 0, the metric wasn't working - this is a test failure
                    // All metrics must capture events for isolation testing to be valid
                    let error_msg = format!(
                        "Cannot verify metric isolation: '{}' had no events before disable (baseline=0). \
                        All metrics must capture at least one event during Phase 4 for isolation tests to work. \
                        Check that the metric line number is on a real executable statement.",
                        other.name
                    );
                    reporter.step_failed(step, &error_msg);
                    return Err(error_msg);
                } else {
                    // baseline_other > 0 but new_count_other <= baseline_other
                    // This is the actual bug - metric was working, then stopped
                    let error_msg = format!(
                        "Other metric stopped capturing after single metric disable (before={}, after={}). \
                        Disabling metric '{}' caused other metric '{}' to stop capturing events. \
                        This indicates a bug - disabling one metric should not affect others.",
                        baseline_other, new_count_other, first_metric.name, other.name
                    );
                    reporter.error(&error_msg);
                    return Err(error_msg);
                }
            }
        }

        // ========================================================================
        // PHASE 6: Disable Group
        // ========================================================================
        reporter.section("PHASE 6: DISABLE ENTIRE GROUP");

        // Get baseline event counts for all metrics BEFORE disabling
        let mut baseline_counts: Vec<(String, usize)> = Vec::new();
        for metric in &config.metrics {
            let count = client
                .query_events(&metric.name, 100)
                .await
                .map(|r| r.data.len())
                .unwrap_or(0);
            baseline_counts.push((metric.name.clone(), count));
        }

        let step = reporter.step_start(
            "Disable Group",
            &format!("Disable '{}' (all metrics should stop)", config.group_name),
        );
        reporter.step_request("disable_group", Some(&config.group_name));

        match client.disable_group(&config.group_name).await {
            Ok(r) => {
                reporter.step_response("OK", Some(&format!("{} metrics disabled", r.data)));
                reporter.step_success(
                    step,
                    Some(&format!("Group '{}' disabled", config.group_name)),
                );
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
            }
        }

        // Wait for some iterations to pass (should NOT capture any events)
        // Using iteration_duration_ms * 2 to ensure at least one iteration cycle
        let wait_ms = config.iteration_duration_ms * 2;
        tokio::time::sleep(Duration::from_millis(wait_ms)).await;

        // Calculate allowed tolerance based on wait time and iteration duration
        // Allow for: in-flight events + DAP propagation delay + buffer
        // (See single metric tolerance calculation for detailed explanation)
        let max_allowed_new_events =
            ((wait_ms as f64 / config.iteration_duration_ms as f64).ceil() as usize) + 7;

        // Verify NO metrics captured new events after group was disabled
        let step = reporter.step_start(
            "Verify Group Stopped",
            "Wait and verify no new events for any metric",
        );

        let mut metrics_still_capturing: Vec<String> = Vec::new();
        for (metric_name, baseline) in &baseline_counts {
            let new_count = client
                .query_events(metric_name, 100)
                .await
                .map(|r| r.data.len())
                .unwrap_or(0);

            let threshold = *baseline + max_allowed_new_events;
            if new_count > threshold {
                metrics_still_capturing.push(format!(
                    "'{}' (before={}, after={}, threshold={})",
                    metric_name, baseline, new_count, threshold
                ));
            }
        }

        if !metrics_still_capturing.is_empty() {
            let error_msg = format!(
                "Metrics still capturing after group disabled: {}",
                metrics_still_capturing.join(", ")
            );
            reporter.step_failed(step, &error_msg);
            return Err(error_msg);
        } else {
            reporter.step_success(step, Some("Metrics stopped capturing after group disabled"));
        }

        // ========================================================================
        // PHASE 7: Introspection (Stack Trace & Memory Snapshot)
        // ========================================================================
        if !config.introspection_metrics.is_empty() {
            reporter.section("PHASE 7: INTROSPECTION (STACK TRACE & MEMORY SNAPSHOT)");

            // Add introspection metrics
            for metric in &config.introspection_metrics {
                let location = format!("@{}#{}", source_file_path.display(), metric.line);
                let feature = if metric.capture_stack_trace && metric.capture_memory_snapshot {
                    "stack trace + memory snapshot"
                } else if metric.capture_stack_trace {
                    "stack trace"
                } else if metric.capture_memory_snapshot {
                    "memory snapshot"
                } else {
                    "no introspection"
                };

                let step = reporter.step_start(
                    "Add Introspection Metric",
                    &format!(
                        "Add '{}' with {} at line {}",
                        metric.name, feature, metric.line
                    ),
                );

                let mut request = super::client::AddMetricRequest::new(
                    &metric.name,
                    &location,
                    &metric.expression,
                    &connection_id,
                );
                request.language = Some(config.language.as_api_str().to_string());
                request.group = metric.group.clone();
                request.enabled = Some(true); // Enable immediately for introspection tests

                // Set introspection flags
                if metric.capture_stack_trace {
                    request.capture_stack_trace = Some(true);
                }
                if metric.capture_memory_snapshot {
                    request.capture_memory_snapshot = Some(true);
                }

                match client.add_metric(request).await {
                    Ok(r) => {
                        reporter.step_response("OK", Some(&format!("id={}", r.data)));
                        reporter.step_success(
                            step,
                            Some(&format!("Added '{}' with {}", metric.name, feature)),
                        );
                        result.introspection_metrics_added += 1;
                    }
                    Err(e) => {
                        reporter.step_failed(step, &e.to_string());
                        reporter.warn(&format!(
                            "Failed to add introspection metric '{}': {}",
                            metric.name, e
                        ));
                    }
                }
            }

            // Wait for introspection events
            if result.introspection_metrics_added > 0 {
                let step = reporter.step_start(
                    "Wait for Introspection Events",
                    &format!(
                        "Wait for events with introspection data (up to {}s)",
                        config.event_wait_secs / 2
                    ),
                );

                let start = std::time::Instant::now();
                let timeout = Duration::from_secs(config.event_wait_secs / 2);

                loop {
                    if start.elapsed() > timeout {
                        reporter.warn("Timeout waiting for introspection events");
                        break;
                    }

                    // Check events for each introspection metric
                    // Reset counters each iteration to avoid double-counting
                    let mut total_introspection_events = 0;
                    let mut iter_stack_trace_events = 0;
                    let mut iter_memory_snapshot_events = 0;
                    for metric in &config.introspection_metrics {
                        if let Ok(r) = client.query_events(&metric.name, 100).await {
                            let count = r.data.len();
                            if count > 0 {
                                reporter.info(&format!(
                                    "  {} captured {} introspection event(s)",
                                    metric.name, count
                                ));
                                total_introspection_events += count;

                                // Track specific introspection types
                                if metric.capture_stack_trace {
                                    iter_stack_trace_events += count;
                                }
                                if metric.capture_memory_snapshot {
                                    iter_memory_snapshot_events += count;
                                }

                                // Log introspection event details
                                for (i, event) in r.data.iter().enumerate() {
                                    // Print basic event info
                                    reporter.info(&format!(
                                        "    [Event {}] value=\"{}\", timestamp={}, is_error={}",
                                        i, event.value, event.timestamp_iso, event.is_error
                                    ));

                                    // Display stack trace if present
                                    if let Some(ref st) = event.stack_trace {
                                        reporter.info(&format!(
                                            "      Stack trace ({} frames):",
                                            st.frames.len()
                                        ));
                                        for (fi, frame) in st.frames.iter().take(5).enumerate() {
                                            reporter.info(&format!(
                                                "        [{}] {}:{} in {}",
                                                fi,
                                                frame.file.as_deref().unwrap_or("<unknown>"),
                                                frame.line.unwrap_or(0),
                                                frame.name
                                            ));
                                        }
                                        if st.frames.len() > 5 {
                                            reporter.info(&format!(
                                                "        ... and {} more frames",
                                                st.frames.len() - 5
                                            ));
                                        }
                                    }

                                    // Display memory snapshot if present
                                    if let Some(ref ms) = event.memory_snapshot {
                                        let local_count = ms.locals.len();
                                        let global_count = ms.globals.len();
                                        reporter.info(&format!(
                                            "      Memory snapshot ({} locals, {} globals):",
                                            local_count, global_count
                                        ));
                                        // Show up to 10 local variables
                                        for var in ms.locals.iter().take(10) {
                                            reporter.info(&format!(
                                                "        local: {} = {}",
                                                var.name,
                                                var.value.chars().take(50).collect::<String>()
                                            ));
                                        }
                                        if local_count > 10 {
                                            reporter.info(&format!(
                                                "        ... and {} more locals",
                                                local_count - 10
                                            ));
                                        }
                                        // Show up to 5 global variables
                                        for var in ms.globals.iter().take(5) {
                                            reporter.info(&format!(
                                                "        global: {} = {}",
                                                var.name,
                                                var.value.chars().take(50).collect::<String>()
                                            ));
                                        }
                                        if global_count > 5 {
                                            reporter.info(&format!(
                                                "        ... and {} more globals",
                                                global_count - 5
                                            ));
                                        }
                                    }

                                    debug!(
                                        metric = %metric.name,
                                        event_index = i,
                                        value = %event.value,
                                        timestamp = %event.timestamp_iso,
                                        is_error = event.is_error,
                                        has_stack_trace = event.stack_trace.is_some(),
                                        has_memory_snapshot = event.memory_snapshot.is_some(),
                                        "Introspection event captured"
                                    );
                                }
                            }
                        }
                    }

                    // Update result with latest snapshot (not accumulating)
                    result.introspection_events = total_introspection_events;
                    result.stack_trace_events = iter_stack_trace_events;
                    result.memory_snapshot_events = iter_memory_snapshot_events;

                    if total_introspection_events >= config.introspection_metrics.len() {
                        reporter.step_success(
                            step,
                            Some(&format!(
                                "{} introspection events captured (stack_trace: {}, memory_snapshot: {})",
                                total_introspection_events,
                                result.stack_trace_events,
                                result.memory_snapshot_events
                            )),
                        );
                        break;
                    }

                    tokio::time::sleep(Duration::from_secs(2)).await;
                }

                // Verify introspection events were received
                let step = reporter.step_start(
                    "Verify Introspection",
                    "Verify that introspection data was captured",
                );

                if result.introspection_events > 0 {
                    reporter.step_success(
                        step,
                        Some(&format!(
                            "Introspection working: {} total events (stack_trace: {}, memory_snapshot: {})",
                            result.introspection_events,
                            result.stack_trace_events,
                            result.memory_snapshot_events
                        )),
                    );
                } else {
                    reporter.warn("No introspection events captured - this may be expected if DAP adapter doesn't support introspection");
                }
            }

            // Cleanup introspection metrics
            for metric in &config.introspection_metrics {
                let _ = client.remove_metric(&metric.name).await;
            }
        }

        // ========================================================================
        // PHASE 8: Cleanup
        // ========================================================================
        reporter.section("PHASE 8: CLEANUP");

        // Remove metrics
        for metric in &config.metrics {
            let step = reporter.step_start("Remove Metric", &format!("Remove '{}'", metric.name));
            match client.remove_metric(&metric.name).await {
                Ok(_) => reporter.step_success(step, Some("Removed")),
                Err(e) => reporter.step_failed(step, &e.to_string()),
            }
        }

        // Remove invalid metric if added
        if let Some(invalid) = &config.invalid_metric {
            let _ = client.remove_metric(&invalid.name).await;
        }

        // Close connection
        let step = reporter.step_start("Close Connection", &format!("Close '{}'", connection_id));
        match client.close_connection(&connection_id).await {
            Ok(_) => reporter.step_success(step, Some("Connection closed")),
            Err(e) => reporter.step_failed(step, &e.to_string()),
        }

        // ========================================================================
        // Summary
        // ========================================================================
        reporter.section(&format!(
            "WORKFLOW COMPLETE ({})",
            config.language.dap_display_name()
        ));
        reporter.info(&format!("Metrics added: {}", result.metrics_added));
        reporter.info(&format!("Total events captured: {}", result.total_events));
        if result.introspection_metrics_added > 0 {
            reporter.info(&format!(
                "Introspection metrics: {} (events: {}, stack_trace: {}, memory_snapshot: {})",
                result.introspection_metrics_added,
                result.introspection_events,
                result.stack_trace_events,
                result.memory_snapshot_events
            ));
        }

        Ok(result)
    }
}

/// Result of a workflow run
#[derive(Debug, Default)]
pub struct WorkflowResult {
    pub metrics_added: usize,
    pub total_events: usize,
    pub introspection_metrics_added: usize,
    pub introspection_events: usize,
    pub stack_trace_events: usize,
    pub memory_snapshot_events: usize,
}

/// Result of a reconnection workflow run
#[derive(Debug, Default)]
pub struct ReconnectionResult {
    pub metrics_added: usize,
    pub reconnection_cycles: usize,
    pub events_per_cycle: Vec<usize>,
    pub total_events: usize,
}

/// Async callback for restarting the debugger between reconnection cycles
/// Returns Ok(()) if debugger restarted successfully, Err with message otherwise
pub type RestartDebuggerFn = Box<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

impl DapWorkflowScenarios {
    /// Run the reconnection scenario to verify metrics persist across disconnect/reconnect
    ///
    /// This test verifies that:
    /// 1. Metrics continue to work after DAP adapter disconnect/reconnect
    /// 2. Events are captured in each cycle after reconnection
    /// 3. The system handles multiple reconnection cycles gracefully
    ///
    /// Flow:
    /// 1. Connect to debugger, add metrics, wait for events
    /// 2. Close connection (disconnect)
    /// 3. \[Optional\] Restart debugger if callback provided (for Go/Rust single-session debuggers)
    /// 4. Reconnect to debugger
    /// 5. Verify metrics stream events again
    /// 6. Repeat disconnect/reconnect cycle
    ///
    /// # Arguments
    /// * `restart_debugger` - Optional callback to restart the debugger between cycles.
    ///   - For Python (debugpy): None, as debugpy stays running after disconnect
    ///   - For Go/Rust: Provide callback to restart delve/lldb-dap (single-session)
    /// * `program_path` - Optional path to program binary (required for Rust direct lldb-dap mode)
    #[allow(clippy::too_many_arguments)]
    pub async fn run_reconnection_workflow<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        config: &DapWorkflowConfig,
        debugger_port: u16,
        source_file_path: &std::path::Path,
        reconnection_cycles: usize,
        restart_debugger: Option<RestartDebuggerFn>,
        program_path: Option<&str>,
    ) -> Result<ReconnectionResult, String> {
        let mut result = ReconnectionResult::default();

        // ========================================================================
        // PHASE 1: Initial Setup & Connection
        // ========================================================================
        reporter.section(&format!(
            "PHASE 1: INITIAL SETUP ({})",
            config.language.dap_display_name()
        ));

        // Health check
        let step = reporter.step_start("Health Check", "Verify API is healthy");
        match client.health().await {
            Ok(true) => reporter.step_success(step, Some("API healthy")),
            Ok(false) => {
                reporter.step_failed(step, "API unhealthy");
                return Err("API unhealthy".to_string());
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                return Err(format!("Health check failed: {}", e));
            }
        }

        // Initial connection
        let step = reporter.step_start(
            "Initial Connection",
            &format!(
                "Connect to {} at 127.0.0.1:{} (program={:?})",
                config.language.dap_display_name(),
                debugger_port,
                program_path
            ),
        );

        let mut connection_id = match client
            .create_connection_with_program(
                "127.0.0.1",
                debugger_port,
                config.language.as_api_str(),
                program_path,
            )
            .await
        {
            Ok(r) => {
                reporter.step_success(step, Some(&format!("Connected: {}", r.data.connection_id)));
                r.data.connection_id
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                return Err(format!("Failed to connect: {}", e));
            }
        };

        // Wait for DAP handshake
        tokio::time::sleep(Duration::from_secs(2)).await;

        // ========================================================================
        // PHASE 2: Add Metrics
        // ========================================================================
        reporter.section("PHASE 2: ADD METRICS");

        // Use only first 2 metrics for simplicity
        let test_metrics: Vec<_> = config.metrics.iter().take(2).collect();

        for metric in &test_metrics {
            let location = format!("@{}#{}", source_file_path.display(), metric.line);
            let step = reporter.step_start(
                "Add Metric",
                &format!("Add '{}' at line {}", metric.name, metric.line),
            );

            let mut request = super::client::AddMetricRequest::new(
                &metric.name,
                &location,
                &metric.expression,
                &connection_id,
            );
            request.language = Some(config.language.as_api_str().to_string());
            request.group = metric.group.clone();
            request.enabled = Some(true); // Enable immediately

            match client.add_metric(request).await {
                Ok(r) => {
                    reporter.step_success(step, Some(&format!("Added id={}", r.data)));
                    result.metrics_added += 1;
                }
                Err(e) => {
                    reporter.step_failed(step, &e.to_string());
                    return Err(format!("Failed to add metric '{}': {}", metric.name, e));
                }
            }
        }

        // ========================================================================
        // PHASE 3: Initial Event Capture
        // ========================================================================
        reporter.section("PHASE 3: INITIAL EVENT CAPTURE");

        let initial_events = Self::wait_for_events(client, reporter, &test_metrics, config).await?;
        result.events_per_cycle.push(initial_events);
        result.total_events += initial_events;

        reporter.info(&format!("Initial cycle captured {} events", initial_events));

        // ========================================================================
        // PHASE 4: Reconnection Cycles
        // ========================================================================
        for cycle in 1..=reconnection_cycles {
            reporter.section(&format!("PHASE 4.{}: RECONNECTION CYCLE {}", cycle, cycle));

            // Step 4a: Close connection (disconnect)
            let step = reporter.step_start(
                "Disconnect",
                &format!("Close connection '{}'", connection_id),
            );

            match client.close_connection(&connection_id).await {
                Ok(_) => {
                    reporter.step_success(step, Some("Connection closed"));
                }
                Err(e) => {
                    reporter.step_failed(step, &e.to_string());
                    reporter.warn("Continuing despite close error...");
                }
            }

            // Brief pause to ensure disconnect is processed
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Step 4b: Restart debugger if needed (Go/Rust have single-session debuggers)
            if let Some(ref restart_fn) = restart_debugger {
                let step = reporter.step_start(
                    "Restart Debugger",
                    &format!("Restart {} debugger", config.language.dap_display_name()),
                );

                match restart_fn().await {
                    Ok(()) => {
                        reporter.step_success(step, Some("Debugger restarted"));
                        // Wait for debugger to be ready
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    Err(e) => {
                        reporter.step_failed(step, &e);
                        return Err(format!(
                            "Failed to restart debugger (cycle {}): {}",
                            cycle, e
                        ));
                    }
                }
            }

            // Step 4c: Reconnect to debugger
            let step = reporter.step_start(
                "Reconnect",
                &format!(
                    "Reconnect to {} at 127.0.0.1:{} (program={:?})",
                    config.language.dap_display_name(),
                    debugger_port,
                    program_path
                ),
            );

            connection_id = match client
                .create_connection_with_program(
                    "127.0.0.1",
                    debugger_port,
                    config.language.as_api_str(),
                    program_path,
                )
                .await
            {
                Ok(r) => {
                    reporter.step_success(
                        step,
                        Some(&format!("Reconnected: {}", r.data.connection_id)),
                    );
                    r.data.connection_id
                }
                Err(e) => {
                    reporter.step_failed(step, &e.to_string());
                    return Err(format!("Failed to reconnect (cycle {}): {}", cycle, e));
                }
            };

            // Wait for DAP handshake
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Step 4d: Verify metrics still work - capture events
            let step = reporter.step_start(
                "Verify Events",
                &format!("Capture events after reconnection (cycle {})", cycle),
            );

            let cycle_events =
                Self::wait_for_events(client, reporter, &test_metrics, config).await?;

            if cycle_events > 0 {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Captured {} events after reconnection",
                        cycle_events
                    )),
                );
                result.events_per_cycle.push(cycle_events);
                result.total_events += cycle_events;
                result.reconnection_cycles += 1;
            } else {
                reporter.step_failed(step, "No events captured after reconnection");
                return Err(format!(
                    "Reconnection cycle {} failed: no events captured",
                    cycle
                ));
            }
        }

        // ========================================================================
        // PHASE 5: Cleanup
        // ========================================================================
        reporter.section("PHASE 5: CLEANUP");

        // Remove metrics
        for metric in &test_metrics {
            let step = reporter.step_start("Remove Metric", &format!("Remove '{}'", metric.name));
            match client.remove_metric(&metric.name).await {
                Ok(_) => reporter.step_success(step, Some("Removed")),
                Err(e) => reporter.step_failed(step, &e.to_string()),
            }
        }

        // Close final connection
        let step = reporter.step_start("Close Connection", &format!("Close '{}'", connection_id));
        match client.close_connection(&connection_id).await {
            Ok(_) => reporter.step_success(step, Some("Connection closed")),
            Err(e) => reporter.step_failed(step, &e.to_string()),
        }

        // ========================================================================
        // Summary
        // ========================================================================
        reporter.section(&format!(
            "RECONNECTION TEST COMPLETE ({})",
            config.language.dap_display_name()
        ));
        reporter.info(&format!("Metrics added: {}", result.metrics_added));
        reporter.info(&format!(
            "Reconnection cycles completed: {}",
            result.reconnection_cycles
        ));
        reporter.info(&format!("Events per cycle: {:?}", result.events_per_cycle));
        reporter.info(&format!("Total events: {}", result.total_events));

        Ok(result)
    }

    /// Helper: Wait for events from metrics
    async fn wait_for_events<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        metrics: &[&MetricPoint],
        config: &DapWorkflowConfig,
    ) -> Result<usize, String> {
        let step = reporter.step_start(
            "Wait for Events",
            &format!("Wait for events (up to {}s)", config.event_wait_secs / 2),
        );

        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(config.event_wait_secs / 2);

        loop {
            if start.elapsed() > timeout {
                // Check if we got any events at all
                let mut total = 0;
                for metric in metrics {
                    if let Ok(r) = client.query_events(&metric.name, 100).await {
                        total += r.data.len();
                    }
                }
                if total > 0 {
                    reporter.step_success(step, Some(&format!("{} events (timeout)", total)));
                    return Ok(total);
                }
                reporter.step_failed(step, "Timeout: no events captured");
                return Err("Timeout waiting for events".to_string());
            }

            // Check events for each metric
            let mut total_events = 0;
            let mut all_have_events = true;

            for metric in metrics {
                if let Ok(r) = client.query_events(&metric.name, 100).await {
                    let count = r.data.len();
                    if count > 0 {
                        total_events += count;
                    } else {
                        all_have_events = false;
                    }
                } else {
                    all_have_events = false;
                }
            }

            // Success if all metrics have at least one event
            if all_have_events && total_events > 0 {
                reporter.step_success(step, Some(&format!("{} events captured", total_events)));
                return Ok(total_events);
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}
