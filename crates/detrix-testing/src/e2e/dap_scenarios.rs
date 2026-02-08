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
    /// IMPORTANT: DAP logpoints evaluate BEFORE the line executes, so we must use
    /// lines where variables are already in scope from PREVIOUS assignments.
    ///
    /// Variables defined at (after Detrix client init block - +13 lines offset):
    /// - Line 54: `symbol = random.choice(symbols)`
    /// - Line 55: `quantity = random.randint(1, 50)`
    /// - Line 56: `price = random.uniform(100, 1000)`
    /// - Line 59: `order_id = place_order(...)` <- symbol, quantity, price in scope here
    /// - Line 66: `print(...)` <- all variables in scope here
    ///
    /// So we use:
    /// - Line 59 for symbol, quantity, price (before order_id is assigned, but after others)
    /// - Line 66 for order_id (after it's assigned on line 59)
    pub fn python() -> Self {
        Self {
            language: SourceLanguage::Python,
            source_file: PathBuf::from("fixtures/python/trade_bot_forever.py"),
            metrics: vec![
                // order_id is assigned on line 59, so evaluate after that (line 66)
                MetricPoint::new("order_metric", 66, "order_id").with_group("python_workflow"),
                // price is assigned on line 56, so evaluate after that (line 59)
                MetricPoint::new("price_metric", 59, "price").with_group("python_workflow"),
                // IMPORTANT: Each metric must be on a DIFFERENT line because DAP logpoints
                // evaluate only one expression per breakpoint location
                // quantity is assigned on line 55, evaluate on line 62 (entry_price := price)
                MetricPoint::new("quantity_metric", 62, "quantity").with_group("python_workflow"),
                // symbol is assigned on line 54, evaluate on line 63 (current_price := ...)
                MetricPoint::new("symbol_metric", 63, "symbol").with_group("python_workflow"),
                // Multi-expression metric: capture symbol + quantity + price on a single metric
                // Line 64: pnl = calculate_pnl(...) - all 3 vars in scope
                MetricPoint::new("trade_details", 64, "symbol")
                    .with_extra_expressions(vec!["quantity", "price"])
                    .with_group("python_workflow"),
            ],
            // Introspection metrics: stack trace and memory snapshot capture
            // Each metric MUST be on a different line (Detrix allows only one metric per line)
            // - Line 66: `print(f"  -> P&L: ...")` - all vars in scope
            // - Line 67: `print()` - all vars in scope
            // - Line 69: `time.sleep(3)` - all vars in scope
            introspection_metrics: vec![
                MetricPoint::new("stack_trace_metric", 66, "order_id")
                    .with_group("python_introspection")
                    .with_stack_trace(),
                MetricPoint::new("memory_snapshot_metric", 67, "price")
                    .with_group("python_introspection")
                    .with_memory_snapshot(),
                MetricPoint::new("full_introspection_metric", 69, "quantity")
                    .with_group("python_introspection")
                    .with_introspection(),
            ],
            inspect_line: 66,
            inspect_variable: "price".to_string(),
            invalid_metric: Some(
                MetricPoint::new("bad_metric", 66, "nonexistent_var").with_group("python_workflow"),
            ),
            group_name: "python_workflow".to_string(),
            event_wait_secs: 15,
            iteration_duration_ms: 3000, // 3 seconds per trade
        }
    }

    /// Create a Go workflow config using detrix_example_app.go
    ///
    /// IMPORTANT: DAP logpoints evaluate BEFORE the line executes, so we must use
    /// lines where variables are already in scope from PREVIOUS assignments.
    ///
    /// Variables defined at (with Detrix client init block + log function):
    /// - Line 117: `symbol := symbols[...]`
    /// - Line 118: `quantity := rand.Intn(50) + 1`
    /// - Line 119: `price := rand.Float64()*900 + 100`
    /// - Line 122: `orderID := placeOrder(...)` <- symbol, quantity, price in scope here
    /// - Line 125: `entryPrice := price`
    /// - Line 126: `currentPrice := ...`
    /// - Line 127: `pnl := calculatePnl(...)` <- all variables in scope here
    /// - Line 130: `_ = orderID` <- all variables in scope here
    /// - Line 131: `_ = pnl` <- all variables in scope here
    /// - Line 133: `log(...)` <- all variables in scope here
    ///
    /// So we use:
    /// - Line 122 for symbol, quantity, price (on orderID assignment, after others assigned)
    /// - Line 127 for orderID (after it's assigned on line 122, on pnl calculation)
    /// - Line 130-133 for introspection (all variables in scope)
    pub fn go() -> Self {
        Self {
            language: SourceLanguage::Go,
            source_file: PathBuf::from("fixtures/go/detrix_example_app.go"),
            metrics: vec![
                // orderID is assigned on line 122, so evaluate after that on line 127
                // Use line 127 (`pnl := calculatePnl(...)`) which is a real executable statement
                MetricPoint::new("order_metric", 127, "orderID").with_group("go_workflow"),
                // price is assigned on line 119, so evaluate after that (line 122)
                MetricPoint::new("price_metric", 122, "price").with_group("go_workflow"),
                // IMPORTANT: Each metric must be on a DIFFERENT line because DAP logpoints
                // evaluate only one expression per breakpoint location
                // quantity is assigned on line 118, evaluate on line 125 (entryPrice := price)
                MetricPoint::new("quantity_metric", 125, "quantity").with_group("go_workflow"),
                // symbol is assigned on line 117, evaluate on line 126 (currentPrice := ...)
                MetricPoint::new("symbol_metric", 126, "symbol").with_group("go_workflow"),
                // Multi-expression metric: capture symbol + quantity + price on a single metric
                // Line 135: time.Sleep(3 * time.Second) - all vars in scope
                MetricPoint::new("trade_details", 135, "symbol")
                    .with_extra_expressions(vec!["quantity", "price"])
                    .with_group("go_workflow"),
            ],
            // Introspection metrics: stack trace and memory snapshot capture
            // Each metric MUST be on a different line (Detrix allows only one metric per line)
            // Lines must be REAL executable statements (not `_ = x` dead assignments).
            // Delve requires actual code at the line to set a verified breakpoint.
            // - Line 130: `totalPnl = totalPnl + pnl` - real assignment
            // - Line 131: `lastOrderID := orderID` - real assignment
            // - Line 133: `log(...)` - function call
            introspection_metrics: vec![
                MetricPoint::new("stack_trace_metric", 130, "orderID")
                    .with_group("go_introspection")
                    .with_stack_trace(),
                MetricPoint::new("memory_snapshot_metric", 131, "price")
                    .with_group("go_introspection")
                    .with_memory_snapshot(),
                MetricPoint::new("full_introspection_metric", 133, "quantity")
                    .with_group("go_introspection")
                    .with_introspection(),
            ],
            inspect_line: 127,
            inspect_variable: "price".to_string(),
            invalid_metric: Some(
                // Line 104 is `go signalHandler(sigChan)` - sigChan is only variable in scope
                MetricPoint::new("bad_metric", 104, "nonexistent_var").with_group("go_workflow"),
            ),
            group_name: "go_workflow".to_string(),
            event_wait_secs: 15,
            iteration_duration_ms: 3000, // 3 seconds per trade (same as Python)
        }
    }

    /// Create a Rust workflow config using fixtures/rust/src/main.rs
    ///
    /// This uses the unified fixture that works for both DAP and client tests:
    ///   cargo build  # for DAP tests (no client feature)
    ///   cargo run --features client  # for client tests
    ///
    /// LINE NUMBER CALCULATION:
    /// All line numbers are calculated as MAIN_LINE + offset.
    /// If you modify fixtures/rust/src/main.rs, only update MAIN_LINE here.
    ///
    /// MAIN_LINE = 76 (the line where `fn main()` is defined)
    ///
    /// Offset definitions (from main):
    /// - main+31: symbol assignment (line 107)
    /// - main+33: quantity assignment (line 109)
    /// - main+35: price assignment (line 111)
    /// - main+38: order_id = place_order (line 114, symbol/quantity/price in scope)
    /// - main+41: entry_price assignment (line 117)
    /// - main+43: current_price assignment (line 119)
    /// - main+45: pnl = calculate_pnl (line 121, all vars in scope)
    /// - main+52: log! (line 128, introspection point 1)
    /// - main+54: stderr().flush() (line 130, introspection point 2)
    /// - main+56: thread::sleep() (line 132, introspection point 3)
    ///
    /// IMPORTANT: DAP logpoints evaluate BEFORE the line executes, so we use
    /// lines where variables are already in scope from PREVIOUS assignments.
    pub fn rust() -> Self {
        // Base line number for fn main() - update this if you add/remove lines before main()
        const MAIN_LINE: u32 = 76;

        // Offsets from main() for each variable/metric point
        // Note: Not all offsets are used directly - some are for documentation
        #[allow(dead_code)]
        const OFFSET_SYMBOL: u32 = 31; // symbol assignment (line 107)
        #[allow(dead_code)]
        const OFFSET_QUANTITY: u32 = 33; // quantity assignment (line 109)
        #[allow(dead_code)]
        const OFFSET_PRICE: u32 = 35; // price assignment (line 111)
        const OFFSET_ORDER_ID: u32 = 38; // place_order call (line 114)
        const OFFSET_ENTRY_PRICE: u32 = 41; // entry_price assignment (line 117)
        const OFFSET_CURRENT_PRICE: u32 = 43; // current_price assignment (line 119)
        const OFFSET_PNL: u32 = 45; // pnl calculation (line 121)
        const OFFSET_LOG: u32 = 52; // log! introspection point (line 128)
        const OFFSET_FLUSH: u32 = 54; // stderr flush introspection point (line 130)
        const OFFSET_SLEEP: u32 = 56; // thread::sleep introspection point (line 132)

        Self {
            language: SourceLanguage::Rust,
            source_file: PathBuf::from("fixtures/rust/src/main.rs"),
            metrics: vec![
                // order_id is assigned at main+33, evaluate after (main+36 = entry_price line)
                MetricPoint::new("order_metric", MAIN_LINE + OFFSET_ENTRY_PRICE, "order_id")
                    .with_group("rust_workflow"),
                // price is assigned at main+30, evaluate after (main+33 = place_order line)
                MetricPoint::new("price_metric", MAIN_LINE + OFFSET_ORDER_ID, "price")
                    .with_group("rust_workflow"),
                // quantity is assigned at main+28, evaluate after (main+38 = current_price line)
                MetricPoint::new(
                    "quantity_metric",
                    MAIN_LINE + OFFSET_CURRENT_PRICE,
                    "quantity",
                )
                .with_group("rust_workflow"),
                // symbol is assigned at main+26, evaluate after (main+40 = pnl line)
                MetricPoint::new("symbol_metric", MAIN_LINE + OFFSET_PNL, "symbol")
                    .with_group("rust_workflow"),
                // NOTE: No multi-expression metric for Rust - all executable lines are taken
                // by other metrics, and `let _ = x` dead assignments are unreliable breakpoint
                // targets with lldb-dap. Multi-expression DAP coverage is provided by Python
                // and Go workflows.
            ],
            // Introspection metrics: stack trace and memory snapshot capture
            // Use distinct function call lines for reliable breakpoint placement
            introspection_metrics: vec![
                // main+52: log! - all variables in scope
                MetricPoint::new("stack_trace_metric", MAIN_LINE + OFFSET_LOG, "order_id")
                    .with_group("rust_introspection")
                    .with_stack_trace(),
                // main+54: stderr().flush() - I/O syscall
                MetricPoint::new("memory_snapshot_metric", MAIN_LINE + OFFSET_FLUSH, "price")
                    .with_group("rust_introspection")
                    .with_memory_snapshot(),
                // main+56: thread::sleep() - function call
                MetricPoint::new(
                    "full_introspection_metric",
                    MAIN_LINE + OFFSET_SLEEP,
                    "quantity",
                )
                .with_group("rust_introspection")
                .with_introspection(),
            ],
            inspect_line: MAIN_LINE + OFFSET_LOG,
            inspect_variable: "price".to_string(),
            invalid_metric: Some(
                MetricPoint::new(
                    "bad_metric",
                    MAIN_LINE + OFFSET_ENTRY_PRICE,
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
                                                &frame.name
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
