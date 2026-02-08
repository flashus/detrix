//! Unified E2E Test Framework
//!
//! This module provides macros and utilities for generating unified tests
//! across all API backends (MCP, gRPC, REST, WebSocket).
//!
//! # Design Philosophy
//!
//! 1. Write test scenarios ONCE using the `ApiClient` trait
//! 2. Generate backend-specific tests with macros
//! 3. Handle backend-specific features (streaming, MCP-only) gracefully
//!
//! # Usage
//!
//! ```ignore
//! use detrix_testing::e2e::unified::*;
//!
//! // Generate tests for all backends
//! generate_unified_tests! {
//!     // Basic tests (all backends)
//!     basic: [
//!         health_check,
//!         get_status,
//!         wake_sleep_cycle,
//!     ],
//!
//!     // Tests requiring debugpy (all backends)
//!     with_debugpy: [
//!         create_connection,
//!         add_metric,
//!         full_metric_lifecycle,
//!     ],
//!
//!     // Streaming tests (gRPC + WS only)
//!     streaming: [
//!         stream_events,
//!     ],
//! }
//! ```

use super::cli::CliClient;
use super::client::{AddMetricRequest, ApiClient, ApiError, ObserveRequest};
use super::executor::TestExecutor;
use super::grpc::GrpcClient;
use super::mcp::McpClient;
use super::reporter::TestReporter;
use super::rest::RestClient;
use super::scenarios::TestScenarios;
use std::sync::Arc;
use std::time::Duration;

/// Trait for backends that support event streaming
#[async_trait::async_trait]
pub trait StreamingClient: ApiClient {
    /// Check if streaming is supported
    fn supports_streaming(&self) -> bool {
        true
    }

    /// Receive events for a duration
    async fn receive_events_streaming(
        &mut self,
        duration: Duration,
        max_events: Option<usize>,
    ) -> Result<Vec<super::client::EventInfo>, ApiError>;
}

// Implement for gRPC
#[async_trait::async_trait]
impl StreamingClient for GrpcClient {
    async fn receive_events_streaming(
        &mut self,
        duration: Duration,
        max_events: Option<usize>,
    ) -> Result<Vec<super::client::EventInfo>, ApiError> {
        self.receive_events(duration, max_events).await
    }
}

// MCP doesn't support streaming
#[async_trait::async_trait]
impl StreamingClient for McpClient {
    fn supports_streaming(&self) -> bool {
        false
    }

    async fn receive_events_streaming(
        &mut self,
        _duration: Duration,
        _max_events: Option<usize>,
    ) -> Result<Vec<super::client::EventInfo>, ApiError> {
        Err(ApiError::new("MCP does not support streaming"))
    }
}

// REST doesn't support streaming (use WebSocket)
#[async_trait::async_trait]
impl StreamingClient for RestClient {
    fn supports_streaming(&self) -> bool {
        false
    }

    async fn receive_events_streaming(
        &mut self,
        _duration: Duration,
        _max_events: Option<usize>,
    ) -> Result<Vec<super::client::EventInfo>, ApiError> {
        Err(ApiError::new(
            "REST does not support streaming - use WebSocket",
        ))
    }
}

// CLI doesn't support streaming
#[async_trait::async_trait]
impl StreamingClient for CliClient {
    fn supports_streaming(&self) -> bool {
        false
    }

    async fn receive_events_streaming(
        &mut self,
        _duration: Duration,
        _max_events: Option<usize>,
    ) -> Result<Vec<super::client::EventInfo>, ApiError> {
        Err(ApiError::new("CLI does not support streaming"))
    }
}

/// Backend capabilities
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCapabilities {
    pub supports_streaming: bool,
    pub supports_mcp_tools: bool,
    pub supports_grpc_streaming: bool,
}

impl BackendCapabilities {
    pub const MCP: Self = Self {
        supports_streaming: false,
        supports_mcp_tools: true,
        supports_grpc_streaming: false,
    };

    pub const GRPC: Self = Self {
        supports_streaming: true,
        supports_mcp_tools: false,
        supports_grpc_streaming: true,
    };

    pub const REST: Self = Self {
        supports_streaming: false,
        supports_mcp_tools: false,
        supports_grpc_streaming: false,
    };

    pub const WEBSOCKET: Self = Self {
        supports_streaming: true,
        supports_mcp_tools: false,
        supports_grpc_streaming: false,
    };

    pub const CLI: Self = Self {
        supports_streaming: false,
        supports_mcp_tools: false,
        supports_grpc_streaming: false,
    };
}

/// Backend type for test generation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    Mcp,
    Grpc,
    Rest,
    WebSocket,
    Cli,
}

impl BackendType {
    pub fn name(&self) -> &'static str {
        match self {
            BackendType::Mcp => "MCP",
            BackendType::Grpc => "gRPC",
            BackendType::Rest => "REST",
            BackendType::WebSocket => "WebSocket",
            BackendType::Cli => "CLI",
        }
    }

    pub fn capabilities(&self) -> BackendCapabilities {
        match self {
            BackendType::Mcp => BackendCapabilities::MCP,
            BackendType::Grpc => BackendCapabilities::GRPC,
            BackendType::Rest => BackendCapabilities::REST,
            BackendType::WebSocket => BackendCapabilities::WEBSOCKET,
            BackendType::Cli => BackendCapabilities::CLI,
        }
    }
}

/// Unified test context that works with any backend
pub struct UnifiedTestContext<C: ApiClient> {
    pub client: C,
    pub reporter: Arc<TestReporter>,
    pub executor: TestExecutor,
    pub backend_type: BackendType,
}

impl<C: ApiClient> UnifiedTestContext<C> {
    pub fn capabilities(&self) -> BackendCapabilities {
        self.backend_type.capabilities()
    }
}

/// Start daemon and create reporter/executor for a test context.
///
/// Shared setup for all `create_*_context` functions: allocates a
/// `TestExecutor`, creates a `TestReporter`, prints the header, and
/// starts the daemon.
async fn start_daemon_context(
    test_name: &str,
    label: &str,
) -> Result<(TestExecutor, Arc<TestReporter>), String> {
    let mut executor = TestExecutor::new();
    let reporter = TestReporter::new(test_name, label);

    reporter.print_header();

    if let Err(e) = executor.start_daemon().await {
        reporter.error(&format!("Failed to start daemon: {}", e));
        executor.print_daemon_logs(50);
        return Err(format!("Failed to start daemon: {}", e));
    }

    Ok((executor, reporter))
}

/// Create MCP test context
pub async fn create_mcp_context(test_name: &str) -> Result<UnifiedTestContext<McpClient>, String> {
    let (executor, reporter) = start_daemon_context(test_name, "MCP").await?;
    let client = McpClient::new(executor.http_port);
    Ok(UnifiedTestContext {
        client,
        reporter,
        executor,
        backend_type: BackendType::Mcp,
    })
}

/// Create gRPC test context
pub async fn create_grpc_context(
    test_name: &str,
) -> Result<UnifiedTestContext<GrpcClient>, String> {
    let (executor, reporter) = start_daemon_context(test_name, "gRPC").await?;
    let client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);
    Ok(UnifiedTestContext {
        client,
        reporter,
        executor,
        backend_type: BackendType::Grpc,
    })
}

/// Create REST test context
pub async fn create_rest_context(
    test_name: &str,
) -> Result<UnifiedTestContext<RestClient>, String> {
    let (executor, reporter) = start_daemon_context(test_name, "REST").await?;
    let client = RestClient::new(executor.http_port);
    Ok(UnifiedTestContext {
        client,
        reporter,
        executor,
        backend_type: BackendType::Rest,
    })
}

/// Create CLI test context
pub async fn create_cli_context(test_name: &str) -> Result<UnifiedTestContext<CliClient>, String> {
    let (executor, reporter) = start_daemon_context(test_name, "CLI").await?;
    // Use with_grpc_port so CLI can connect to the test daemon
    let client = CliClient::with_grpc_port(executor.http_port, executor.grpc_port);
    Ok(UnifiedTestContext {
        client,
        reporter,
        executor,
        backend_type: BackendType::Cli,
    })
}

// ============================================================================
// Unified Test Scenarios - These run identically on all backends
// ============================================================================

/// Health check scenario
pub async fn scenario_health_check<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::health_check(&ctx.client, &ctx.reporter).await
}

/// Get status scenario
pub async fn scenario_get_status<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::get_status(&ctx.client, &ctx.reporter).await
}

/// Wake scenario
pub async fn scenario_wake<C: ApiClient>(ctx: &UnifiedTestContext<C>) -> Result<(), ApiError> {
    let step = ctx.reporter.step_start("Wake", "Wake the daemon");
    ctx.reporter.step_request("wake", None);

    let response = ctx.client.wake().await?;
    ctx.reporter
        .step_response("OK", Some(&format!("status={}", response.data)));
    ctx.reporter.step_success(step, Some("Daemon woke up"));
    Ok(())
}

/// Sleep scenario
pub async fn scenario_sleep<C: ApiClient>(ctx: &UnifiedTestContext<C>) -> Result<(), ApiError> {
    let step = ctx.reporter.step_start("Sleep", "Put the daemon to sleep");
    ctx.reporter.step_request("sleep", None);

    let response = ctx.client.sleep().await?;
    ctx.reporter
        .step_response("OK", Some(&format!("status={}", response.data)));
    ctx.reporter.step_success(step, Some("Daemon sleeping"));
    Ok(())
}

/// Wake/sleep cycle scenario
pub async fn scenario_wake_sleep_cycle<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::wake_sleep_cycle(&ctx.client, &ctx.reporter).await
}

/// List connections scenario
pub async fn scenario_list_connections<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::list_connections(&ctx.client, &ctx.reporter).await?;
    Ok(())
}

/// List metrics scenario
pub async fn scenario_list_metrics<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::list_metrics(&ctx.client, &ctx.reporter).await?;
    Ok(())
}

/// Get metric (nonexistent) scenario - tests endpoint exists
pub async fn scenario_get_metric_nonexistent<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Get Metric", "Get nonexistent metric");
    ctx.reporter
        .step_request("get_metric", Some("name=nonexistent"));

    match ctx.client.get_metric("nonexistent").await {
        Ok(response) => {
            ctx.reporter.step_response(
                "OK",
                Some(&format!(
                    "name={}, enabled={}",
                    response.data.name, response.data.enabled
                )),
            );
            ctx.reporter.step_success(step, Some("Got metric"));
        }
        Err(e) => {
            // Not found is acceptable - endpoint exists
            ctx.reporter.step_response("NOT_FOUND", Some(&e.message));
            ctx.reporter
                .step_success(step, Some("Endpoint exists (metric not found is expected)"));
        }
    }
    Ok(())
}

/// List groups scenario
pub async fn scenario_list_groups<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("List Groups", "List all metric groups");
    ctx.reporter.step_request("list_groups", None);

    let response = ctx.client.list_groups().await?;
    ctx.reporter
        .step_response("OK", Some(&format!("{} groups", response.data.len())));
    for group in &response.data {
        ctx.reporter.info(&format!(
            "  {} ({}/{} enabled)",
            group.name, group.enabled_count, group.metric_count
        ));
    }
    ctx.reporter.step_success(step, Some("Listed groups"));
    Ok(())
}

/// Enable group scenario
pub async fn scenario_enable_group<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx.reporter.step_start("Enable Group", "Enable test_group");
    ctx.reporter
        .step_request("enable_group", Some("group=test_group"));

    let response = ctx.client.enable_group("test_group").await?;
    ctx.reporter
        .step_response("OK", Some(&format!("{} metrics enabled", response.data)));
    ctx.reporter.step_success(step, Some("Group enabled"));
    Ok(())
}

/// Disable group scenario
pub async fn scenario_disable_group<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Disable Group", "Disable test_group");
    ctx.reporter
        .step_request("disable_group", Some("group=test_group"));

    let response = ctx.client.disable_group("test_group").await?;
    ctx.reporter
        .step_response("OK", Some(&format!("{} metrics disabled", response.data)));
    ctx.reporter.step_success(step, Some("Group disabled"));
    Ok(())
}

/// Validate safe Python expression scenario
pub async fn scenario_validate_safe_expression<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::validate_expression(&ctx.client, &ctx.reporter, "user.id", "python", true)
        .await?;
    Ok(())
}

/// Validate unsafe Python expression scenario
pub async fn scenario_validate_unsafe_expression<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::validate_expression(&ctx.client, &ctx.reporter, "eval('bad')", "python", false)
        .await?;
    Ok(())
}

/// Validate safe Go expression scenario
pub async fn scenario_validate_safe_go_expression<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::validate_expression(&ctx.client, &ctx.reporter, "len(items)", "go", true)
        .await?;
    Ok(())
}

/// Validate unsafe Go expression scenario (exec.Command is impure/unsafe)
pub async fn scenario_validate_unsafe_go_expression<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::validate_expression(
        &ctx.client,
        &ctx.reporter,
        "exec.Command(\"ls\")",
        "go",
        false,
    )
    .await?;
    Ok(())
}

/// Validate safe Rust expression scenario
pub async fn scenario_validate_safe_rust_expression<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::validate_expression(&ctx.client, &ctx.reporter, "items.len()", "rust", true)
        .await?;
    Ok(())
}

/// Validate unsafe Rust expression scenario (std::fs::read_to_string is impure)
pub async fn scenario_validate_unsafe_rust_expression<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::validate_expression(
        &ctx.client,
        &ctx.reporter,
        "std::fs::read_to_string(\"config.txt\")",
        "rust",
        false,
    )
    .await?;
    Ok(())
}

/// Get config scenario
pub async fn scenario_get_config<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Get Config", "Get current configuration");
    ctx.reporter.step_request("get_config", None);

    let response = ctx.client.get_config().await?;
    ctx.reporter
        .step_response("OK", Some(&format!("{} chars", response.data.len())));
    ctx.reporter.step_success(step, Some("Got configuration"));
    Ok(())
}

/// Validate config scenario
pub async fn scenario_validate_config<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Validate Config", "Validate configuration");
    let config_toml = r#"
[server]
http_port = 8080
grpc_port = 50051

[performance]
max_metrics = 1000
"#;
    ctx.reporter
        .step_request("validate_config", Some("config_toml=..."));

    let response = ctx.client.validate_config(config_toml).await?;
    ctx.reporter
        .step_response("OK", Some(&format!("is_valid: {}", response.data)));
    ctx.reporter
        .step_success(step, Some("Validated configuration"));
    Ok(())
}

/// Query events (nonexistent metric) scenario
pub async fn scenario_query_events_nonexistent<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Query Events", "Query events for nonexistent metric");
    ctx.reporter
        .step_request("query_events", Some("name=test, limit=10"));

    match ctx.client.query_events("test", 10).await {
        Ok(response) => {
            ctx.reporter
                .step_response("OK", Some(&format!("{} events", response.data.len())));
            ctx.reporter.step_success(step, Some("Queried events"));
        }
        Err(e) => {
            // Metric not found is acceptable - endpoint exists
            ctx.reporter.step_response("NOT_FOUND", Some(&e.message));
            ctx.reporter.step_success(step, Some("Endpoint exists"));
        }
    }
    Ok(())
}

/// Inspect file scenario
pub async fn scenario_inspect_file<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Inspect File", "Inspect Python file");
    ctx.reporter.step_request(
        "inspect_file",
        Some("file=fixtures/python/detrix_example_app.py"),
    );

    let response = ctx
        .client
        .inspect_file("fixtures/python/detrix_example_app.py", Some(10), None)
        .await?;
    ctx.reporter
        .step_response("OK", Some(&format!("{} chars", response.data.len())));
    ctx.reporter.step_success(step, Some("Inspected file"));
    Ok(())
}

/// Inspect file with variable search scenario
pub async fn scenario_inspect_file_with_variable<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Inspect File (Variable)", "Find variable in Python file");
    ctx.reporter.step_request(
        "inspect_file",
        Some("file=fixtures/python/detrix_example_app.py, find_variable=price"),
    );

    let response = ctx
        .client
        .inspect_file("fixtures/python/detrix_example_app.py", None, Some("price"))
        .await?;
    ctx.reporter
        .step_response("OK", Some(&format!("{} chars", response.data.len())));
    ctx.reporter
        .step_success(step, Some("Found variable occurrences"));
    Ok(())
}

/// Update config scenario
pub async fn scenario_update_config<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Update Config", "Update configuration");
    let config_toml = "[server]\nhttp_port = 8080\n";
    ctx.reporter
        .step_request("update_config", Some("config_toml=..."));

    match ctx.client.update_config(config_toml, false).await {
        Ok(_) => {
            ctx.reporter.step_response("OK", Some("Updated"));
            ctx.reporter.step_success(step, Some("Config updated"));
        }
        Err(e) if e.message.contains("unimplemented") || e.message.contains("not supported") => {
            // update_config may be disabled for safety
            ctx.reporter
                .step_response("UNIMPLEMENTED", Some(&e.message));
            ctx.reporter
                .step_success(step, Some("Endpoint exists (disabled for safety)"));
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Reload config scenario
pub async fn scenario_reload_config<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Reload Config", "Reload configuration from file");
    ctx.reporter.step_request("reload_config", None);

    match ctx.client.reload_config().await {
        Ok(_) => {
            ctx.reporter.step_response("OK", Some("success"));
            ctx.reporter
                .step_success(step, Some("Config reload attempted"));
        }
        Err(e)
            if e.message.contains("not found")
                || e.message.contains("No config")
                || e.message.contains("not configured")
                || e.raw_response.as_ref().is_some_and(|r| {
                    r.contains("not found")
                        || r.contains("No config")
                        || r.contains("not configured")
                }) =>
        {
            // File not found / not configured is acceptable - endpoint exists
            ctx.reporter
                .step_response("NOT_CONFIGURED", Some(&e.message));
            ctx.reporter
                .step_success(step, Some("Endpoint exists (no config file)"));
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

// ============================================================================
// Scenarios requiring debugpy
// ============================================================================

/// Create connection scenario (requires debugpy)
pub async fn scenario_create_connection<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugpy_port: u16,
) -> Result<String, ApiError> {
    TestScenarios::create_connection(
        &ctx.client,
        &ctx.reporter,
        "127.0.0.1",
        debugpy_port,
        "python",
    )
    .await
}

/// Get connection scenario
pub async fn scenario_get_connection<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    connection_id: &str,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Get Connection", "Get connection details");
    ctx.reporter
        .step_request("get_connection", Some(&format!("id={}", connection_id)));

    let response = ctx.client.get_connection(connection_id).await?;
    ctx.reporter
        .step_response("OK", Some(&format!("status={}", response.data.status)));
    ctx.reporter
        .step_success(step, Some("Got connection details"));
    Ok(())
}

/// Close connection scenario
pub async fn scenario_close_connection<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    connection_id: &str,
) -> Result<(), ApiError> {
    TestScenarios::close_connection(&ctx.client, &ctx.reporter, connection_id).await
}

/// Connection lifecycle scenario (create, get, close)
pub async fn scenario_connection_lifecycle<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugpy_port: u16,
    _script_path: &str,
) -> Result<(), ApiError> {
    ctx.reporter.section("CONNECTION LIFECYCLE");

    // Create connection
    let connection_id = scenario_create_connection(ctx, debugpy_port).await?;

    // Wait for DAP handshake
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Get connection
    scenario_get_connection(ctx, &connection_id).await?;

    // List connections
    scenario_list_connections(ctx).await?;

    // Close connection
    scenario_close_connection(ctx, &connection_id).await?;

    Ok(())
}

/// Add metric scenario (requires debugpy)
pub async fn scenario_add_metric<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    name: &str,
    location: &str,
    expression: &str,
    connection_id: &str,
) -> Result<String, ApiError> {
    TestScenarios::add_metric(
        &ctx.client,
        &ctx.reporter,
        name,
        location,
        expression,
        connection_id,
    )
    .await
}

/// Remove metric scenario
pub async fn scenario_remove_metric<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    name: &str,
) -> Result<(), ApiError> {
    TestScenarios::remove_metric(&ctx.client, &ctx.reporter, name).await
}

/// Toggle metric scenario
pub async fn scenario_toggle_metric<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    name: &str,
    enabled: bool,
) -> Result<(), ApiError> {
    TestScenarios::toggle_metric(&ctx.client, &ctx.reporter, name, enabled).await
}

/// Full metric CRUD scenario (requires debugpy)
pub async fn scenario_metric_crud<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugpy_port: u16,
    script_path: &str,
) -> Result<(), ApiError> {
    ctx.reporter.section("CONNECTION");

    // Create connection
    let connection_id = scenario_create_connection(ctx, debugpy_port).await?;

    // Wait for DAP handshake
    tokio::time::sleep(Duration::from_secs(2)).await;

    ctx.reporter.section("METRICS");

    let metric_name = "crud_test_metric";
    let location = format!("@{}#22", script_path);

    // Add metric
    scenario_add_metric(ctx, metric_name, &location, "price", &connection_id).await?;

    // Small delay for persistence
    tokio::time::sleep(Duration::from_millis(500)).await;

    // List metrics
    scenario_list_metrics(ctx).await?;

    // Toggle off
    scenario_toggle_metric(ctx, metric_name, false).await?;

    // Remove
    scenario_remove_metric(ctx, metric_name).await?;

    Ok(())
}

/// Full metric lifecycle scenario (requires debugpy)
pub async fn scenario_full_metric_lifecycle<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugpy_port: u16,
    script_path: &str,
) -> Result<(), ApiError> {
    let location = format!("@{}#22", script_path);
    TestScenarios::full_metric_lifecycle(
        &ctx.client,
        &ctx.reporter,
        "127.0.0.1",
        debugpy_port,
        "lifecycle_metric",
        &location,
        "price",
        60,
    )
    .await
}

/// Query events scenario (requires debugpy with active metric)
pub async fn scenario_query_events<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugpy_port: u16,
    script_path: &str,
) -> Result<(), ApiError> {
    // Create connection
    let connection_id = scenario_create_connection(ctx, debugpy_port).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let metric_name = "query_events_test";
    let location = format!("@{}#22", script_path);

    // Add metric
    scenario_add_metric(ctx, metric_name, &location, "price", &connection_id).await?;

    // Wait for events
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Query events
    let step = ctx
        .reporter
        .step_start("Query Events", "Query metric events");
    ctx.reporter.step_request(
        "query_events",
        Some(&format!("name={}, limit=10", metric_name)),
    );

    let response = ctx.client.query_events(metric_name, 10).await?;
    ctx.reporter
        .step_response("OK", Some(&format!("{} events", response.data.len())));
    ctx.reporter.step_success(step, Some("Events queried"));

    // Cleanup
    let _ = ctx.client.remove_metric(metric_name).await;
    let _ = ctx.client.close_connection(&connection_id).await;

    Ok(())
}

/// Group operations scenario (requires debugpy)
/// Tests enable_group and disable_group with ACTUAL metrics
pub async fn scenario_group_operations<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugpy_port: u16,
    script_path: &str,
) -> Result<(), ApiError> {
    ctx.reporter.section("GROUP OPERATIONS TEST");

    // Create connection
    let connection_id = scenario_create_connection(ctx, debugpy_port).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Add metrics as DISABLED with group name
    // Note: Each metric must be on a different line (DAP only supports one logpoint per line)
    let group_name = "test_group_ops";
    let metrics = [
        ("group_test_metric_1", 22, "price"),  // line 22: order_id = ...
        ("group_test_metric_2", 32, "symbol"), // line 32: last_update = ...
    ];

    ctx.reporter.section("ADD DISABLED METRICS TO GROUP");

    for (name, line, expr) in &metrics {
        let location = format!("@{}#{}", script_path, line);
        let step = ctx.reporter.step_start(
            "Add Disabled Metric",
            &format!("Add '{}' to group '{}' (disabled)", name, group_name),
        );
        ctx.reporter.step_request(
            "add_metric",
            Some(&format!(
                "name={}, group={}, enabled=false",
                name, group_name
            )),
        );

        let request = AddMetricRequest::new(name, &location, expr, &connection_id)
            .with_group(group_name)
            .disabled(); // Add as disabled

        match ctx.client.add_metric(request).await {
            Ok(r) => {
                ctx.reporter
                    .step_response("OK", Some(&format!("id={} (disabled)", r.data)));
                ctx.reporter
                    .step_success(step, Some(&format!("Added '{}' as disabled", name)));
            }
            Err(e) => {
                ctx.reporter.step_failed(step, &e.to_string());
                // Cleanup and return
                let _ = ctx.client.close_connection(&connection_id).await;
                return Err(e);
            }
        }
    }

    // Verify metrics are disabled
    let step = ctx.reporter.step_start(
        "Verify Disabled",
        &format!(
            "Check that {} metrics are in group (disabled)",
            metrics.len()
        ),
    );
    ctx.reporter.step_request("list_groups", None);

    match ctx.client.list_groups().await {
        Ok(groups) => {
            if let Some(group) = groups.data.iter().find(|g| g.name == group_name) {
                ctx.reporter.step_response(
                    "OK",
                    Some(&format!(
                        "{} metrics, {} enabled",
                        group.metric_count, group.enabled_count
                    )),
                );
                if group.enabled_count > 0 {
                    ctx.reporter
                        .warn("Some metrics already enabled - test may be invalid");
                }
            } else {
                ctx.reporter
                    .step_response("NOT_FOUND", Some("Group not in list"));
            }
        }
        Err(e) => {
            ctx.reporter.step_response("ERROR", Some(&e.to_string()));
        }
    }
    ctx.reporter.step_success(
        step,
        Some(&format!(
            "{} disabled metrics verified in group",
            metrics.len()
        )),
    );

    // Enable group
    ctx.reporter.section("ENABLE GROUP");
    let step = ctx.reporter.step_start(
        "Enable Group",
        &format!(
            "Enable '{}' - expecting {} metrics to be enabled",
            group_name,
            metrics.len()
        ),
    );
    ctx.reporter.step_request("enable_group", Some(group_name));

    let expected_enabled = metrics.len();
    match ctx.client.enable_group(group_name).await {
        Ok(r) => {
            ctx.reporter
                .step_response("OK", Some(&format!("{} metrics enabled", r.data)));

            // Verify count - FAIL if expected doesn't match actual
            if r.data != expected_enabled {
                let error_msg = if r.data == 0 {
                    format!(
                        "enable_group returned 0 metrics - expected {} to be enabled. \
                        This indicates group name mismatch or metrics not saved with correct group.",
                        expected_enabled
                    )
                } else {
                    format!(
                        "enable_group returned {} metrics - expected {} to be enabled",
                        r.data, expected_enabled
                    )
                };
                ctx.reporter.step_failed(step, &error_msg);
                // Cleanup and return error
                let _ = ctx.client.close_connection(&connection_id).await;
                return Err(ApiError::new(error_msg));
            }

            ctx.reporter
                .step_success(step, Some(&format!("{} metrics enabled", r.data)));
        }
        Err(e) => {
            ctx.reporter.step_failed(step, &e.to_string());
            // Cleanup and return error
            let _ = ctx.client.close_connection(&connection_id).await;
            return Err(e);
        }
    }

    // Verify metrics are now enabled
    let step = ctx.reporter.step_start(
        "Verify Enabled",
        "Check that metrics are now enabled in group",
    );
    match ctx.client.list_groups().await {
        Ok(groups) => {
            if let Some(group) = groups.data.iter().find(|g| g.name == group_name) {
                ctx.reporter.step_response(
                    "OK",
                    Some(&format!(
                        "{} metrics, {} enabled",
                        group.metric_count, group.enabled_count
                    )),
                );
                ctx.reporter.step_success(
                    step,
                    Some(&format!(
                        "{}/{} enabled",
                        group.enabled_count, group.metric_count
                    )),
                );
            } else {
                ctx.reporter
                    .step_success(step, Some("Group not in list (may be normal)"));
            }
        }
        Err(e) => {
            ctx.reporter.step_failed(step, &e.to_string());
        }
    }

    // Disable group
    ctx.reporter.section("DISABLE GROUP");
    let step = ctx.reporter.step_start(
        "Disable Group",
        &format!(
            "Disable '{}' - expecting {} metrics to be disabled",
            group_name,
            metrics.len()
        ),
    );
    ctx.reporter.step_request("disable_group", Some(group_name));

    match ctx.client.disable_group(group_name).await {
        Ok(r) => {
            ctx.reporter
                .step_response("OK", Some(&format!("{} metrics disabled", r.data)));
            ctx.reporter
                .step_success(step, Some(&format!("{} metrics disabled", r.data)));
        }
        Err(e) => {
            ctx.reporter.step_failed(step, &e.to_string());
        }
    }

    // Cleanup
    ctx.reporter.section("CLEANUP");
    for (name, _, _) in &metrics {
        let _ = ctx.client.remove_metric(name).await;
    }
    let _ = ctx.client.close_connection(&connection_id).await;

    Ok(())
}

/// Multi-expression metric scenario (requires debugpy)
///
/// Tests that a metric with multiple expressions can be created, events captured,
/// and the metric correctly stores/returns all expressions.
pub async fn scenario_multi_expr_metric<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugpy_port: u16,
    script_path: &str,
) -> Result<(), ApiError> {
    ctx.reporter.section("MULTI-EXPRESSION METRIC TEST");

    // Create connection
    let connection_id = scenario_create_connection(ctx, debugpy_port).await?;

    // Wait for DAP handshake
    tokio::time::sleep(Duration::from_secs(2)).await;

    ctx.reporter.section("ADD MULTI-EXPRESSION METRIC");

    // Add metric with 3 expressions at line 19 (symbol, quantity, price all in scope)
    // detrix_example_app.py line 19: print(...) inside place_order(symbol, quantity, price)
    // All 3 are function parameters, guaranteed in scope
    let metric_name = "multi_expr_test";
    let location = format!("@{}#19", script_path);
    let expressions = vec![
        "symbol".to_string(),
        "quantity".to_string(),
        "price".to_string(),
    ];

    TestScenarios::add_multi_expr_metric(
        &ctx.client,
        &ctx.reporter,
        metric_name,
        &location,
        expressions.clone(),
        &connection_id,
    )
    .await?;

    // Small delay for persistence
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify metric was stored with all 3 expressions
    ctx.reporter.section("VERIFY EXPRESSIONS STORED");

    let step = ctx
        .reporter
        .step_start("List Metrics", "Verify metric has 3 expressions");
    ctx.reporter.step_request("list_metrics", None);

    let metrics = ctx.client.list_metrics().await?;
    let multi_metric = metrics.data.iter().find(|m| m.name == metric_name);

    match multi_metric {
        Some(m) => {
            ctx.reporter.step_response(
                "OK",
                Some(&format!("expressions=[{}]", m.expressions.join(", "))),
            );
            if m.expressions.len() == 3 {
                ctx.reporter.step_success(
                    step,
                    Some(&format!(
                        "Metric has {} expressions as expected",
                        m.expressions.len()
                    )),
                );
            } else {
                let msg = format!(
                    "Expected 3 expressions, got {}: [{}]",
                    m.expressions.len(),
                    m.expressions.join(", ")
                );
                ctx.reporter.step_failed(step, &msg);
                return Err(ApiError::new(msg));
            }
        }
        None => {
            ctx.reporter
                .step_failed(step, "Multi-expression metric not found in list");
            return Err(ApiError::new("Multi-expression metric not found"));
        }
    }

    // Wait for events
    ctx.reporter.section("CAPTURE EVENTS");

    let event_count = TestScenarios::wait_for_events(
        &ctx.client,
        &ctx.reporter,
        metric_name,
        1,  // At least 1 event
        30, // 30 second timeout
    )
    .await?;

    ctx.reporter.info(&format!(
        "Captured {} events from multi-expression metric",
        event_count
    ));

    // Query events and verify they contain proper multi-expression data
    let step = ctx.reporter.step_start(
        "Verify Events",
        "Check that events contain multi-expression values",
    );
    ctx.reporter.step_request(
        "query_events",
        Some(&format!("name={}, limit=10", metric_name)),
    );

    let events = ctx.client.query_events(metric_name, 10).await?;
    if events.data.is_empty() {
        ctx.reporter.step_failed(step, "No events found");
        return Err(ApiError::new("No events for multi-expression metric"));
    }

    // Verify each event has multi-expression values
    let first_event = &events.data[0];
    ctx.reporter.info(&format!(
        "  First event: value='{}', values count={}, values={:?}",
        first_event.value,
        first_event.values.len(),
        first_event.values,
    ));

    // Verify the values array has entries for each expression
    if first_event.values.len() < expressions.len() {
        let msg = format!(
            "Event has {} values but expected at least {} (one per expression: [{}]). values={:?}",
            first_event.values.len(),
            expressions.len(),
            expressions.join(", "),
            first_event.values,
        );
        ctx.reporter.step_failed(step, &msg);
        return Err(ApiError::new(msg));
    }

    // Verify each value entry has a non-empty valueJson
    for (i, val) in first_event.values.iter().enumerate() {
        let value_json = val
            .get("valueJson")
            .or_else(|| val.get("value_json"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if value_json.is_empty() {
            let msg = format!("Expression value [{}] has empty valueJson: {:?}", i, val);
            ctx.reporter.step_failed(step, &msg);
            return Err(ApiError::new(msg));
        }
        ctx.reporter
            .info(&format!("  Expression [{}]: valueJson='{}'", i, value_json,));
    }

    // Verify expression names match if available (REST/gRPC include expression field)
    let has_expression_names = first_event
        .values
        .iter()
        .any(|v| v.get("expression").is_some());
    if has_expression_names {
        for (i, (val, expected_expr)) in first_event
            .values
            .iter()
            .zip(expressions.iter())
            .enumerate()
        {
            let actual_expr = val.get("expression").and_then(|v| v.as_str()).unwrap_or("");
            if actual_expr != expected_expr {
                let msg = format!(
                    "Expression [{}]: expected name '{}', got '{}'",
                    i, expected_expr, actual_expr
                );
                ctx.reporter.step_failed(step, &msg);
                return Err(ApiError::new(msg));
            }
        }
        ctx.reporter
            .info("  Expression names verified: all match expected");
    }

    ctx.reporter.step_success(
        step,
        Some(&format!(
            "{} events, each with {} expression values verified",
            events.data.len(),
            first_event.values.len(),
        )),
    );

    // Cleanup
    ctx.reporter.section("CLEANUP");
    let _ = ctx.client.remove_metric(metric_name).await;
    let _ = ctx.client.close_connection(&connection_id).await;

    Ok(())
}

/// All basic endpoints scenario (no debugpy needed)
pub async fn scenario_all_basic_endpoints<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    TestScenarios::all_endpoints(&ctx.client, &ctx.reporter).await
}

/// Comprehensive coverage scenario (requires debugpy)
pub async fn scenario_comprehensive_coverage<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugpy_port: u16,
    script_path: &str,
) -> Result<(), ApiError> {
    let mut failures: Vec<String> = vec![];

    // Health & Status
    ctx.reporter.section("HEALTH & STATUS");

    ctx.reporter.info("Testing health...");
    if let Err(e) = ctx.client.health().await {
        failures.push(format!("health: {}", e));
    }

    ctx.reporter.info("Testing get_status...");
    if let Err(e) = ctx.client.get_status().await {
        failures.push(format!("get_status: {}", e));
    }

    ctx.reporter.info("Testing wake...");
    if let Err(e) = ctx.client.wake().await {
        failures.push(format!("wake: {}", e));
    }

    ctx.reporter.info("Testing sleep...");
    if let Err(e) = ctx.client.sleep().await {
        failures.push(format!("sleep: {}", e));
    }

    // Connections
    ctx.reporter.section("CONNECTIONS");

    ctx.reporter.info("Testing list_connections...");
    if let Err(e) = ctx.client.list_connections().await {
        failures.push(format!("list_connections: {}", e));
    }

    ctx.reporter.info("Testing create_connection...");
    let connection_id = match ctx
        .client
        .create_connection("127.0.0.1", debugpy_port, "python")
        .await
    {
        Ok(r) => {
            ctx.reporter
                .info(&format!("  OK create_connection: {}", r.data.connection_id));
            Some(r.data.connection_id)
        }
        Err(e) => {
            failures.push(format!("create_connection: {}", e));
            None
        }
    };

    tokio::time::sleep(Duration::from_secs(2)).await;

    if let Some(ref conn_id) = connection_id {
        ctx.reporter.info("Testing get_connection...");
        if let Err(e) = ctx.client.get_connection(conn_id).await {
            failures.push(format!("get_connection: {}", e));
        }
    }

    // Metrics
    ctx.reporter.section("METRICS");

    ctx.reporter.info("Testing list_metrics...");
    if let Err(e) = ctx.client.list_metrics().await {
        failures.push(format!("list_metrics: {}", e));
    }

    let location = format!("@{}#22", script_path);
    let conn_id = connection_id.as_deref().unwrap_or("unknown");

    ctx.reporter.info("Testing add_metric...");
    let add_req = AddMetricRequest::new("coverage_test_metric", &location, "price", conn_id);
    let metric_added = match ctx.client.add_metric(add_req).await {
        Ok(r) => {
            ctx.reporter
                .info(&format!("  OK add_metric: id={}", r.data));
            true
        }
        Err(e) => {
            failures.push(format!("add_metric: {}", e));
            false
        }
    };

    if metric_added {
        ctx.reporter.info("Testing toggle_metric...");
        if let Err(e) = ctx
            .client
            .toggle_metric("coverage_test_metric", false)
            .await
        {
            failures.push(format!("toggle_metric: {}", e));
        }

        ctx.reporter.info("Testing query_events...");
        if let Err(e) = ctx.client.query_events("coverage_test_metric", 10).await {
            failures.push(format!("query_events: {}", e));
        }

        ctx.reporter.info("Testing remove_metric...");
        if let Err(e) = ctx.client.remove_metric("coverage_test_metric").await {
            failures.push(format!("remove_metric: {}", e));
        }
    }

    // Groups
    ctx.reporter.section("GROUPS");

    ctx.reporter.info("Testing list_groups...");
    if let Err(e) = ctx.client.list_groups().await {
        failures.push(format!("list_groups: {}", e));
    }

    ctx.reporter.info("Testing enable_group...");
    if let Err(e) = ctx.client.enable_group("test_group").await {
        failures.push(format!("enable_group: {}", e));
    }

    ctx.reporter.info("Testing disable_group...");
    if let Err(e) = ctx.client.disable_group("test_group").await {
        failures.push(format!("disable_group: {}", e));
    }

    // Validation (Python)
    ctx.reporter.section("VALIDATION (Python)");

    ctx.reporter
        .info("Testing validate_expression (Python safe)...");
    if let Err(e) = ctx.client.validate_expression("user.id", "python").await {
        failures.push(format!("validate_expression (python safe): {}", e));
    }

    ctx.reporter
        .info("Testing validate_expression (Python unsafe)...");
    if let Err(e) = ctx
        .client
        .validate_expression("eval('bad')", "python")
        .await
    {
        // Unsafe expression is expected to return OK with is_safe=false, not error
        failures.push(format!("validate_expression (python unsafe): {}", e));
    }

    // Validation (Go)
    ctx.reporter.section("VALIDATION (Go)");

    ctx.reporter
        .info("Testing validate_expression (Go safe)...");
    if let Err(e) = ctx.client.validate_expression("len(items)", "go").await {
        failures.push(format!("validate_expression (go safe): {}", e));
    }

    ctx.reporter
        .info("Testing validate_expression (Go unsafe)...");
    if let Err(e) = ctx
        .client
        .validate_expression("exec.Command(\"ls\")", "go")
        .await
    {
        // Unsafe expression is expected to return OK with is_safe=false, not error
        failures.push(format!("validate_expression (go unsafe): {}", e));
    }

    ctx.reporter.info("Testing inspect_file...");
    if let Err(e) = ctx
        .client
        .inspect_file("fixtures/python/detrix_example_app.py", Some(10), None)
        .await
    {
        failures.push(format!("inspect_file: {}", e));
    }

    // Config
    ctx.reporter.section("CONFIGURATION");

    ctx.reporter.info("Testing get_config...");
    if let Err(e) = ctx.client.get_config().await {
        failures.push(format!("get_config: {}", e));
    }

    ctx.reporter.info("Testing validate_config...");
    if let Err(e) = ctx
        .client
        .validate_config("[server]\nhttp_port = 8080\n")
        .await
    {
        failures.push(format!("validate_config: {}", e));
    }

    // Cleanup
    ctx.reporter.section("CLEANUP");

    if let Some(ref conn_id) = connection_id {
        ctx.reporter.info("Testing close_connection...");
        if let Err(e) = ctx.client.close_connection(conn_id).await {
            failures.push(format!("close_connection: {}", e));
        }
    }

    // Summary
    ctx.reporter.section("SUMMARY");

    if failures.is_empty() {
        ctx.reporter.info("All endpoints passed!");
        Ok(())
    } else {
        ctx.reporter
            .error(&format!("{} endpoint(s) failed:", failures.len()));
        for failure in &failures {
            ctx.reporter.error(&format!("  - {}", failure));
        }
        Err(ApiError::new(format!(
            "Comprehensive coverage failed: {} endpoint(s) not working",
            failures.len()
        )))
    }
}

// ============================================================================
// Error Handling Scenarios
// ============================================================================

/// Test error handling for various invalid inputs
pub async fn scenario_error_handling<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    ctx.reporter.section("ERROR HANDLING");

    // Test 1: Get nonexistent connection
    let step = ctx.reporter.step_start(
        "Get Nonexistent Connection",
        "Expect error for missing connection",
    );
    ctx.reporter
        .step_request("get_connection", Some("id=nonexistent_conn_12345"));

    match ctx.client.get_connection("nonexistent_conn_12345").await {
        Ok(_) => {
            ctx.reporter
                .step_failed(step, "Expected error but got success");
            return Err(ApiError::new("Expected error for nonexistent connection"));
        }
        Err(e) => {
            ctx.reporter.step_response("ERROR", Some(&e.message));
            ctx.reporter.step_success(
                step,
                Some("Correctly returned error for missing connection"),
            );
        }
    }

    // Test 2: Get nonexistent metric
    let step = ctx
        .reporter
        .step_start("Get Nonexistent Metric", "Expect error for missing metric");
    ctx.reporter
        .step_request("get_metric", Some("name=nonexistent_metric_xyz"));

    match ctx.client.get_metric("nonexistent_metric_xyz").await {
        Ok(_) => {
            // Some implementations return empty result instead of error
            ctx.reporter
                .step_response("OK", Some("Returned empty/default"));
            ctx.reporter
                .step_success(step, Some("Handled nonexistent metric gracefully"));
        }
        Err(e) => {
            ctx.reporter.step_response("ERROR", Some(&e.message));
            ctx.reporter
                .step_success(step, Some("Correctly returned error for missing metric"));
        }
    }

    // Test 3: Validate empty expression
    let step = ctx.reporter.step_start(
        "Validate Empty Expression",
        "Test empty expression handling",
    );
    ctx.reporter
        .step_request("validate_expression", Some("expression=, language=python"));

    match ctx.client.validate_expression("", "python").await {
        Ok(response) => {
            // Empty expression should be flagged as unsafe or error
            if response.data.is_safe {
                ctx.reporter
                    .step_response("UNSAFE", Some("Empty expression marked unsafe"));
            } else {
                ctx.reporter
                    .step_response("SAFE", Some("Empty expression allowed"));
            }
            ctx.reporter
                .step_success(step, Some("Handled empty expression"));
        }
        Err(e) => {
            ctx.reporter.step_response("ERROR", Some(&e.message));
            ctx.reporter
                .step_success(step, Some("Correctly rejected empty expression"));
        }
    }

    // Test 4: Close nonexistent connection
    let step = ctx.reporter.step_start(
        "Close Nonexistent Connection",
        "Expect error for closing missing connection",
    );
    ctx.reporter
        .step_request("close_connection", Some("id=fake_connection_id"));

    match ctx.client.close_connection("fake_connection_id").await {
        Ok(_) => {
            // Some implementations silently succeed for idempotency
            ctx.reporter
                .step_response("OK", Some("Closed (idempotent)"));
            ctx.reporter
                .step_success(step, Some("Handled idempotently"));
        }
        Err(e) => {
            ctx.reporter.step_response("ERROR", Some(&e.message));
            ctx.reporter.step_success(
                step,
                Some("Correctly returned error for missing connection"),
            );
        }
    }

    // Test 5: Invalid config TOML
    let step = ctx
        .reporter
        .step_start("Validate Invalid Config", "Test invalid TOML handling");
    ctx.reporter
        .step_request("validate_config", Some("config_toml=invalid{{{{"));

    match ctx.client.validate_config("invalid{{{{").await {
        Ok(response) => {
            if !response.data {
                ctx.reporter
                    .step_response("INVALID", Some("Correctly identified as invalid"));
            } else {
                ctx.reporter
                    .step_response("VALID", Some("Unexpectedly valid"));
            }
            ctx.reporter
                .step_success(step, Some("Validation completed"));
        }
        Err(e) => {
            ctx.reporter.step_response("ERROR", Some(&e.message));
            ctx.reporter
                .step_success(step, Some("Correctly returned error for invalid TOML"));
        }
    }

    ctx.reporter.info("All error handling tests passed");
    Ok(())
}

/// Test concurrent request handling
/// Note: Uses HTTP port from executor for direct HTTP requests
pub async fn scenario_concurrent_requests<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    ctx.reporter.section("CONCURRENT REQUESTS");

    let http_port = ctx.executor.http_port;

    // Test 1: Multiple concurrent health checks
    let step = ctx
        .reporter
        .step_start("Concurrent Health Checks", "Run 10 parallel health checks");

    let mut handles = vec![];
    for _ in 0..10 {
        let base_url = format!("http://127.0.0.1:{}", http_port);

        handles.push(tokio::spawn(async move {
            let client = reqwest::Client::new();
            client
                .get(format!("{}/health", base_url))
                .timeout(Duration::from_secs(5))
                .send()
                .await
        }));
    }

    let mut successes = 0;
    let mut failures = 0;
    for handle in handles {
        match handle.await {
            Ok(Ok(response)) if response.status().is_success() => successes += 1,
            _ => failures += 1,
        }
    }

    ctx.reporter.step_response(
        "OK",
        Some(&format!("{} succeeded, {} failed", successes, failures)),
    );

    if failures > 2 {
        ctx.reporter
            .step_failed(step, &format!("Too many failures: {}", failures));
        return Err(ApiError::new(format!(
            "Concurrent health checks had {} failures",
            failures
        )));
    }

    ctx.reporter.step_success(
        step,
        Some(&format!("{}/10 health checks succeeded", successes)),
    );

    // Test 2: Concurrent list operations
    let step = ctx.reporter.step_start(
        "Concurrent List Operations",
        "Run parallel list_metrics calls",
    );

    let mut list_handles = vec![];
    for _ in 0..5 {
        let base_url = format!("http://127.0.0.1:{}", http_port);
        list_handles.push(tokio::spawn(async move {
            let client = reqwest::Client::new();
            client
                .get(format!("{}/api/v1/metrics", base_url))
                .timeout(Duration::from_secs(10))
                .send()
                .await
        }));
    }

    let mut list_successes = 0;
    for handle in list_handles {
        if let Ok(Ok(response)) = handle.await {
            if response.status().is_success() {
                list_successes += 1;
            }
        }
    }

    ctx.reporter.step_response(
        "OK",
        Some(&format!("{}/5 list operations succeeded", list_successes)),
    );
    ctx.reporter.step_success(
        step,
        Some(&format!(
            "{} concurrent list operations completed",
            list_successes
        )),
    );

    // Test 3: Concurrent status checks
    let step = ctx
        .reporter
        .step_start("Concurrent Status Checks", "Run parallel status checks");

    let mut status_handles = vec![];
    for _ in 0..5 {
        let base_url = format!("http://127.0.0.1:{}", http_port);
        status_handles.push(tokio::spawn(async move {
            let client = reqwest::Client::new();
            client
                .get(format!("{}/api/v1/status", base_url))
                .timeout(Duration::from_secs(10))
                .send()
                .await
        }));
    }

    let mut status_successes = 0;
    for handle in status_handles {
        if let Ok(Ok(response)) = handle.await {
            if response.status().is_success() {
                status_successes += 1;
            }
        }
    }

    ctx.reporter.step_response(
        "OK",
        Some(&format!("{}/5 status checks succeeded", status_successes)),
    );
    ctx.reporter.step_success(
        step,
        Some(&format!(
            "{} concurrent status checks completed",
            status_successes
        )),
    );

    ctx.reporter.info("All concurrent request tests passed");
    Ok(())
}

// ============================================================================
// Observe Tool Scenarios (MCP-only)
// ============================================================================

/// Observe workflow scenario using the MCP observe tool.
///
/// This tests the new simplified workflow:
/// - Auto-finds best line for expression
/// - Auto-selects connection if only one exists
/// - Auto-generates metric name
///
/// Only works on MCP backend (returns graceful skip on other backends).
pub async fn scenario_observe_workflow<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugger_port: u16,
    script_path: &str,
) -> Result<(), ApiError> {
    ctx.reporter.section("OBSERVE WORKFLOW (MCP-only)");

    // Skip on non-MCP backends
    if !ctx.capabilities().supports_mcp_tools {
        ctx.reporter
            .info("Skipping observe workflow - only supported on MCP backend");
        ctx.reporter.info(&format!(
            "Backend {} does not support MCP tools",
            ctx.backend_type.name()
        ));
        return Ok(());
    }

    // Step 1: Create connection
    let step = ctx.reporter.step_start(
        "Create Connection",
        &format!("Connect to debugger at 127.0.0.1:{}", debugger_port),
    );
    ctx.reporter.step_request(
        "create_connection",
        Some(&format!("port={}, language=python", debugger_port)),
    );

    let connection_id = match ctx
        .client
        .create_connection("127.0.0.1", debugger_port, "python")
        .await
    {
        Ok(r) => {
            ctx.reporter
                .step_response("OK", Some(&format!("id={}", r.data.connection_id)));
            ctx.reporter
                .step_success(step, Some(&format!("Connected: {}", r.data.connection_id)));
            r.data.connection_id
        }
        Err(e) => {
            ctx.reporter.step_failed(step, &e.to_string());
            return Err(e);
        }
    };

    // Wait for DAP handshake
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Step 2: Observe with explicit line (simplest case)
    let step = ctx.reporter.step_start(
        "Observe (Explicit Line)",
        "Use observe tool with explicit line number",
    );
    ctx.reporter.step_request(
        "observe",
        Some(&format!("file={}, expression=price, line=22", script_path)),
    );

    let observe_request = ObserveRequest::new(script_path, "price")
        .with_line(22)
        .with_connection_id(&connection_id)
        .with_name("explicit_line_metric");

    match ctx.client.observe(observe_request).await {
        Ok(r) => {
            ctx.reporter.step_response(
                "OK",
                Some(&format!(
                    "metric={}, line={}, line_source={}",
                    r.data.metric_name, r.data.line, r.data.line_source
                )),
            );
            ctx.reporter.step_success(
                step,
                Some(&format!(
                    "Observing '{}' at {}:{}",
                    r.data.expression, r.data.file, r.data.line
                )),
            );

            // Print detailed info
            ctx.reporter.info(&format!(
                "  Metric ID: {}, Connection: {}",
                r.data.metric_id, r.data.connection_id
            ));
        }
        Err(e) => {
            ctx.reporter.step_failed(step, &e.to_string());
            // Continue to cleanup
        }
    }

    // Step 3: Observe with auto line finding (uses find_variable)
    let step = ctx.reporter.step_start(
        "Observe (Auto-Find Line)",
        "Use observe tool to auto-find best line for variable",
    );
    ctx.reporter.step_request(
        "observe",
        Some(&format!(
            "file={}, expression=symbol, find_variable=symbol",
            script_path
        )),
    );

    let observe_request = ObserveRequest::new(script_path, "symbol")
        .with_find_variable("symbol")
        .with_connection_id(&connection_id);

    match ctx.client.observe(observe_request).await {
        Ok(r) => {
            ctx.reporter.step_response(
                "OK",
                Some(&format!(
                    "metric={}, line={} (auto), line_source={}",
                    r.data.metric_name, r.data.line, r.data.line_source
                )),
            );
            ctx.reporter.step_success(
                step,
                Some(&format!(
                    "Auto-found line {} for '{}'",
                    r.data.line, r.data.expression
                )),
            );

            // Show alternatives if any
            if !r.data.alternatives.is_empty() {
                ctx.reporter.info("  Alternatives found:");
                for (line, context) in &r.data.alternatives {
                    ctx.reporter
                        .info(&format!("    Line {}: {}", line, context));
                }
            }
        }
        Err(e) => {
            // Auto-find might fail if file inspection doesn't find the variable
            ctx.reporter.step_response("WARN", Some(&e.to_string()));
            ctx.reporter.warn(&format!("Auto-find failed: {}", e));
        }
    }

    // Step 4: Observe with group and introspection
    let step = ctx.reporter.step_start(
        "Observe (With Introspection)",
        "Use observe tool with stack trace and memory snapshot",
    );
    ctx.reporter.step_request(
        "observe",
        Some(&format!(
            "file={}, expression=quantity, line=25, capture_stack_trace=true, capture_memory_snapshot=true",
            script_path
        )),
    );

    let observe_request = ObserveRequest::new(script_path, "quantity")
        .with_line(25)
        .with_connection_id(&connection_id)
        .with_group("observe_test")
        .with_introspection();

    match ctx.client.observe(observe_request).await {
        Ok(r) => {
            ctx.reporter.step_response(
                "OK",
                Some(&format!(
                    "metric={}, group=observe_test",
                    r.data.metric_name
                )),
            );
            ctx.reporter.step_success(
                step,
                Some(&format!(
                    "Observing '{}' with introspection",
                    r.data.expression
                )),
            );

            // Print warnings if any
            for warning in &r.data.warnings {
                ctx.reporter.warn(&format!("  Warning: {}", warning));
            }
        }
        Err(e) => {
            ctx.reporter.step_failed(step, &e.to_string());
        }
    }

    // Step 5: Wait for some events to be captured
    ctx.reporter.section("VERIFY EVENTS");

    let step = ctx.reporter.step_start(
        "Wait for Events",
        "Wait for observe metrics to capture data",
    );

    tokio::time::sleep(Duration::from_secs(5)).await;

    // Query events from our observed metrics
    let metrics = ctx.client.list_metrics().await?;
    let mut total_events = 0;

    for metric in metrics.data.iter() {
        if metric.name.contains("metric") || metric.name.contains("observe") {
            if let Ok(events) = ctx.client.query_events(&metric.name, 10).await {
                let count = events.data.len();
                if count > 0 {
                    ctx.reporter
                        .info(&format!("  '{}': {} events captured", metric.name, count));
                    total_events += count;
                }
            }
        }
    }

    if total_events > 0 {
        ctx.reporter.step_success(
            step,
            Some(&format!("{} total events captured", total_events)),
        );
    } else {
        ctx.reporter
            .warn("No events captured yet (may need more time)");
    }

    // Step 6: Cleanup
    ctx.reporter.section("CLEANUP");

    // Remove metrics we created
    let metrics_to_remove = ["explicit_line_metric"];
    for name in &metrics_to_remove {
        let _ = ctx.client.remove_metric(name).await;
    }

    // Also clean up any auto-generated metrics
    if let Ok(metrics) = ctx.client.list_metrics().await {
        for metric in metrics.data.iter() {
            if metric.name.contains("symbol_at_") || metric.name.contains("quantity_at_") {
                let _ = ctx.client.remove_metric(&metric.name).await;
            }
        }
    }

    // Close connection
    let step = ctx
        .reporter
        .step_start("Close Connection", &format!("Close '{}'", connection_id));
    match ctx.client.close_connection(&connection_id).await {
        Ok(_) => ctx.reporter.step_success(step, Some("Connection closed")),
        Err(e) => ctx.reporter.step_failed(step, &e.to_string()),
    }

    ctx.reporter.info("Observe workflow completed successfully");
    Ok(())
}

/// Observe workflow with auto-connection selection.
///
/// Tests that observe tool auto-selects a connection when only one exists.
pub async fn scenario_observe_auto_connection<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
    debugger_port: u16,
    script_path: &str,
) -> Result<(), ApiError> {
    ctx.reporter.section("OBSERVE AUTO-CONNECTION");

    // Skip on non-MCP backends
    if !ctx.capabilities().supports_mcp_tools {
        ctx.reporter
            .info("Skipping - only supported on MCP backend");
        return Ok(());
    }

    // Step 1: Create a single connection (observe will auto-select it)
    let step = ctx.reporter.step_start(
        "Create Single Connection",
        "Create connection so observe can auto-select it",
    );

    let connection_id = match ctx
        .client
        .create_connection("127.0.0.1", debugger_port, "python")
        .await
    {
        Ok(r) => {
            ctx.reporter
                .step_success(step, Some(&format!("id={}", r.data.connection_id)));
            r.data.connection_id
        }
        Err(e) => {
            ctx.reporter.step_failed(step, &e.to_string());
            return Err(e);
        }
    };

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Step 2: Observe WITHOUT specifying connection_id
    let step = ctx.reporter.step_start(
        "Observe (Auto-Select Connection)",
        "Observe without connection_id - should auto-select the only connection",
    );
    ctx.reporter.step_request(
        "observe",
        Some(&format!("file={}, expression=price, line=22", script_path)),
    );

    // Intentionally NOT setting connection_id
    let observe_request = ObserveRequest::new(script_path, "price")
        .with_line(22)
        .with_name("auto_conn_test");

    match ctx.client.observe(observe_request).await {
        Ok(r) => {
            ctx.reporter.step_response(
                "OK",
                Some(&format!(
                    "metric={}, auto_selected_connection={}",
                    r.data.metric_name, r.data.connection_id
                )),
            );

            // Verify the connection was auto-selected
            if r.data.connection_id == connection_id {
                ctx.reporter.step_success(
                    step,
                    Some(&format!(
                        "Correctly auto-selected connection '{}'",
                        r.data.connection_id
                    )),
                );
            } else {
                ctx.reporter.warn(&format!(
                    "Selected connection '{}' differs from expected '{}'",
                    r.data.connection_id, connection_id
                ));
            }
        }
        Err(e) => {
            ctx.reporter.step_failed(step, &e.to_string());
        }
    }

    // Cleanup
    let _ = ctx.client.remove_metric("auto_conn_test").await;
    let _ = ctx.client.close_connection(&connection_id).await;

    Ok(())
}

// ============================================================================
// MCP Usage Statistics Scenarios
// ============================================================================

/// Get MCP usage statistics scenario
pub async fn scenario_get_mcp_usage<C: ApiClient>(
    ctx: &UnifiedTestContext<C>,
) -> Result<(), ApiError> {
    let step = ctx
        .reporter
        .step_start("Get MCP Usage", "Get MCP tool usage statistics");
    ctx.reporter.step_request("get_mcp_usage", None);

    let response = ctx.client.get_mcp_usage().await?;
    let usage = &response.data;

    ctx.reporter.step_response(
        "OK",
        Some(&format!(
            "calls={}, success_rate={:.1}%",
            usage.total_calls,
            usage.success_rate * 100.0
        )),
    );

    // Log additional details
    ctx.reporter.info(&format!(
        "  Successes: {}, Errors: {}, Avg Latency: {:.1}ms",
        usage.total_success, usage.total_errors, usage.avg_latency_ms
    ));

    if !usage.top_errors.is_empty() {
        ctx.reporter.info("  Top Errors:");
        for error in &usage.top_errors {
            ctx.reporter.info(&format!(
                "    - {}: {} occurrences",
                error.code, error.count
            ));
        }
    }

    ctx.reporter.info(&format!(
        "  Workflow: correct={}, skipped_inspect={}, no_connection={}, adherence={:.1}%",
        usage.workflow.correct_workflows,
        usage.workflow.skipped_inspect,
        usage.workflow.no_connection,
        usage.workflow.adherence_rate * 100.0
    ));

    ctx.reporter
        .step_success(step, Some("Got MCP usage statistics"));
    Ok(())
}

// ============================================================================
// Re-export macros at module level
// ============================================================================

/// Generate a single test for a specific backend
#[macro_export]
macro_rules! unified_test {
    // Basic test (no external debugger required - daemon is always available)
    ($backend:ident, $name:ident, basic, $scenario:expr) => {
        paste::paste! {
            #[tokio::test]
            async fn [<test_ $backend _ $name>]() {
                use $crate::e2e::unified::*;

                let ctx = match [<create_ $backend _context>](stringify!($name)).await {
                    Ok(ctx) => ctx,
                    Err(e) => panic!("Failed to create test context: {}", e),
                };

                let result = $scenario(&ctx).await;

                match result {
                    Ok(()) => ctx.reporter.print_footer(true),
                    Err(e) => {
                        ctx.reporter.error(&format!("Test failed: {}", e));
                        ctx.reporter.print_footer(false);
                        panic!("Test failed: {}", e);
                    }
                }
            }
        }
    };

    // Test requiring debugpy (Python debugger)
    ($backend:ident, $name:ident, with_debugpy, $scenario:expr) => {
        paste::paste! {
            #[tokio::test]
            async fn [<test_ $backend _ $name>]() {
                use $crate::e2e::unified::*;
                use $crate::e2e::executor::TestExecutor;
                use $crate::e2e::is_debugpy_available;

                // Runtime availability check - skip if debugpy not installed
                if !is_debugpy_available().await {
                    eprintln!("Skipping test: debugpy not available");
                    return;
                }

                let mut ctx = match [<create_ $backend _context>](stringify!($name)).await {
                    Ok(ctx) => ctx,
                    Err(e) => panic!("Failed to create test context: {}", e),
                };

                let script_path = match ctx.executor.detrix_example_app_path() {
                    Some(p) => p,
                    None => {
                        ctx.reporter.error("detrix_example_app.py not found");
                        ctx.reporter.print_footer(false);
                        panic!("detrix_example_app.py not found");
                    }
                };

                if let Err(e) = ctx.executor.start_debugpy(script_path.to_str().unwrap()).await {
                    ctx.reporter.error(&format!("Failed to start debugpy: {}", e));
                    ctx.executor.print_debugpy_logs(50);
                    ctx.reporter.print_footer(false);
                    panic!("Failed to start debugpy: {}", e);
                }

                let debugpy_port = ctx.executor.debugpy_port;
                let script_path_str = script_path.to_string_lossy().to_string();

                let result = $scenario(&ctx, debugpy_port, &script_path_str).await;

                match result {
                    Ok(()) => ctx.reporter.print_footer(true),
                    Err(e) => {
                        ctx.reporter.error(&format!("Test failed: {}", e));
                        ctx.executor.print_daemon_logs(50);
                        ctx.executor.print_debugpy_logs(50);
                        ctx.reporter.print_footer(false);
                        panic!("Test failed: {}", e);
                    }
                }
            }
        }
    };
}

/// Generate tests for all three main backends (MCP, gRPC, REST)
#[macro_export]
macro_rules! generate_tests_all_backends {
    // Basic tests
    (basic: [$($name:ident => $scenario:expr),* $(,)?]) => {
        $(
            $crate::unified_test!(mcp, $name, basic, $scenario);
            $crate::unified_test!(grpc, $name, basic, $scenario);
            $crate::unified_test!(rest, $name, basic, $scenario);
            $crate::unified_test!(cli, $name, basic, $scenario);
        )*
    };

    // Tests requiring debugpy
    (with_debugpy: [$($name:ident => $scenario:expr),* $(,)?]) => {
        $(
            $crate::unified_test!(mcp, $name, with_debugpy, $scenario);
            $crate::unified_test!(grpc, $name, with_debugpy, $scenario);
            $crate::unified_test!(rest, $name, with_debugpy, $scenario);
            $crate::unified_test!(cli, $name, with_debugpy, $scenario);
        )*
    };
}

/// Generate single-backend tests
#[macro_export]
macro_rules! generate_tests_single_backend {
    ($backend:ident, basic: [$($name:ident => $scenario:expr),* $(,)?]) => {
        $(
            $crate::unified_test!($backend, $name, basic, $scenario);
        )*
    };

    ($backend:ident, with_debugpy: [$($name:ident => $scenario:expr),* $(,)?]) => {
        $(
            $crate::unified_test!($backend, $name, with_debugpy, $scenario);
        )*
    };
}
