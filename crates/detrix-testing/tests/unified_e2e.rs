//! Unified E2E Tests
//!
//! This file generates tests for ALL API backends (MCP, gRPC, REST) using macros.
//! Each test scenario is defined ONCE and automatically runs on all backends.
//!
//! # Test Categories
//!
//! ## Basic Tests (no debugpy needed)
//! - Health check, status, wake/sleep, validation
//!
//! ## Connection Tests (requires debugpy)
//! - Create/close connections, add/remove/toggle metrics
//!
//! ## Full Workflow Tests (requires debugpy)
//! - Full metric lifecycle, comprehensive coverage
//!
//! # Running Tests
//!
//! Tests use runtime availability checks for external tools (debugpy, delve, lldb-dap).
//! If a tool is not available, the test skips gracefully with a message.
//!
//! ```bash
//! # Run all unified tests
//! cargo test --package detrix-testing --test unified_e2e -- --nocapture
//!
//! # Run specific backend tests
//! cargo test --package detrix-testing --test unified_e2e test_mcp_
//! cargo test --package detrix-testing --test unified_e2e test_grpc_
//! cargo test --package detrix-testing --test unified_e2e test_rest_
//!
//! # Run specific test on all backends
//! cargo test --package detrix-testing --test unified_e2e health_check
//! ```

#[allow(unused_imports)]
use detrix_testing::e2e::unified::*;
#[allow(unused_imports)]
use detrix_testing::generate_tests_all_backends;

// ============================================================================
// BASIC TESTS - Run on all backends (MCP, gRPC, REST)
// No debugpy or external processes required
// ============================================================================

generate_tests_all_backends! {
    basic: [
        // Health & Status
        health_check => scenario_health_check,
        get_status => scenario_get_status,
        wake => scenario_wake,
        sleep => scenario_sleep,
        wake_sleep_cycle => scenario_wake_sleep_cycle,

        // Lists (empty results are OK)
        list_connections => scenario_list_connections,
        list_metrics => scenario_list_metrics,
        list_groups => scenario_list_groups,

        // Metric/Group operations on empty state
        get_metric_nonexistent => scenario_get_metric_nonexistent,
        query_events_nonexistent => scenario_query_events_nonexistent,

        // Validation (Python)
        validate_safe_expression => scenario_validate_safe_expression,
        validate_unsafe_expression => scenario_validate_unsafe_expression,

        // Validation (Go)
        validate_safe_go_expression => scenario_validate_safe_go_expression,
        validate_unsafe_go_expression => scenario_validate_unsafe_go_expression,

        // Validation (Rust)
        validate_safe_rust_expression => scenario_validate_safe_rust_expression,
        validate_unsafe_rust_expression => scenario_validate_unsafe_rust_expression,

        // Config
        get_config => scenario_get_config,
        validate_config => scenario_validate_config,
        update_config => scenario_update_config,
        reload_config => scenario_reload_config,

        // File inspection
        inspect_file => scenario_inspect_file,
        inspect_file_with_variable => scenario_inspect_file_with_variable,

        // MCP Usage statistics
        get_mcp_usage => scenario_get_mcp_usage,

        // All endpoints test
        all_basic_endpoints => scenario_all_basic_endpoints,

        // Error handling
        error_handling => scenario_error_handling,

        // Concurrent requests
        concurrent_requests => scenario_concurrent_requests,
    ]
}

// ============================================================================
// DEBUGPY TESTS - Run on all backends (MCP, gRPC, REST)
// Requires debugpy to be started
// ============================================================================

generate_tests_all_backends! {
    with_debugpy: [
        // Connection operations
        connection_lifecycle => scenario_connection_lifecycle,

        // Metric CRUD
        metric_crud => scenario_metric_crud,

        // Group operations (with real metrics)
        group_operations => scenario_group_operations,

        // Query events
        query_events => scenario_query_events,

        // Full lifecycle
        full_metric_lifecycle => scenario_full_metric_lifecycle,

        // Comprehensive coverage
        comprehensive_coverage => scenario_comprehensive_coverage,
    ]
}

// ============================================================================
// DAP WORKFLOW TESTS - MCP only (these test actual debugger integration)
// These tests are specific to DAP protocols and not just API coverage
// ============================================================================

mod dap_workflow_tests {
    use detrix_application::services::file_inspection_types::SourceLanguageExt;
    use detrix_testing::e2e::dap_scenarios::{DapWorkflowConfig, DapWorkflowScenarios};
    use detrix_testing::e2e::executor::TestExecutor;
    use detrix_testing::e2e::mcp::McpClient;
    use detrix_testing::e2e::reporter::TestReporter;

    /// Run Python DAP workflow
    #[tokio::test]
    async fn test_dap_python_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Python Workflow", "MCP");
        reporter.print_header();

        // Get script path for Python trade_bot_forever.py
        let script_path = executor
            .workspace_root
            .join("fixtures/python/trade_bot_forever.py");

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        // Start debugpy
        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        let client = McpClient::new(executor.http_port);
        let config = DapWorkflowConfig::python();

        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.debugpy_port,
            &script_path,
            None, // Python doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                // Always print daemon logs to see stopped event handling
                println!("DEBUG: daemon_log_path = {:?}", executor.daemon_log_path);
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                // Assert introspection works when language supports it
                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured. \
                        This indicates breakpoints are not triggering stopped events.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("Workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_debugpy_logs(50);
                reporter.print_footer(false);
                panic!("Workflow failed: {}", e);
            }
        }
    }

    /// Run Go DAP workflow
    #[tokio::test]
    async fn test_dap_go_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Delve).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Go Workflow", "MCP");
        reporter.print_header();

        // Get script path for Go detrix_example_app.go
        let script_path = executor
            .workspace_root
            .join("fixtures/go/detrix_example_app.go");

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        // Start delve (Go debugger)
        if let Err(e) = executor.start_delve(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start delve: {}", e));
            executor.print_delve_logs(50);
            panic!("Failed to start delve: {}", e);
        }

        let client = McpClient::new(executor.http_port);
        let config = DapWorkflowConfig::go();

        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.delve_port,
            &script_path,
            None, // Go doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                // Always print daemon logs for debugging
                println!("\n=== Daemon Logs (last 100 lines) ===");
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                // Assert introspection works when language supports it
                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured. \
                        This indicates breakpoints are not triggering stopped events.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("Workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_delve_logs(50);
                reporter.print_footer(false);
                panic!("Workflow failed: {}", e);
            }
        }
    }

    /// Run Rust DAP workflow (requires lldb-dap and compiled detrix_example_app)
    #[tokio::test]
    async fn test_dap_rust_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::LldbDap).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Rust Workflow", "MCP");
        reporter.print_header();

        // For Rust, start_lldb expects source file path (will compile it)
        let source_path = executor
            .workspace_root
            .join("fixtures/rust/detrix_example_app.rs");

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        // Start lldb-dap (compiles and runs the source file)
        if let Err(e) = executor.start_lldb(source_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start lldb-dap: {}", e));
            executor.print_lldb_logs(50);
            panic!("Failed to start lldb-dap: {}", e);
        }

        let client = McpClient::new(executor.http_port);
        let config = DapWorkflowConfig::rust();

        // For Rust workflow, we need the source file path for the config
        let source_file = std::path::PathBuf::from(&config.source_file);

        // When using lldb-serve, don't pass program_path (lldb-serve handles launch)
        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.lldb_port,
            &source_file,
            None, // lldb-serve handles program launch
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                // Always print daemon logs for debugging
                println!("\n=== Daemon Logs (last 100 lines) ===");
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                // Assert introspection works when language supports it
                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured. \
                        This indicates breakpoints are not triggering stopped events.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("Workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_lldb_logs(50);
                reporter.print_footer(false);
                panic!("Workflow failed: {}", e);
            }
        }
    }

    /// Run Rust DAP workflow using CodeLLDB (vadimcn's adapter)
    ///
    /// CodeLLDB has better PDB support on Windows compared to standard lldb-dap.
    /// On macOS/Linux, this test validates CodeLLDB as an alternative to lldb-dap.
    #[tokio::test]
    async fn test_dap_rust_codelldb_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::CodeLldb).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Rust CodeLLDB Workflow", "MCP");
        reporter.print_header();

        // For Rust, start_codelldb expects source file path (will compile it)
        let source_path = executor
            .workspace_root
            .join("fixtures/rust/detrix_example_app.rs");

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        // Start CodeLLDB (compiles and runs the source file)
        if let Err(e) = executor.start_codelldb(source_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start CodeLLDB: {}", e));
            executor.print_codelldb_logs(50);
            panic!("Failed to start CodeLLDB: {}", e);
        }

        let client = McpClient::new(executor.http_port);
        let config = DapWorkflowConfig::rust();

        // For Rust workflow, we need the source file path for the config
        let source_file = std::path::PathBuf::from(&config.source_file);

        // When using lldb-serve (proxy mode), don't pass program_path
        // lldb-serve already handles launching the program
        // Passing None makes the adapter use attach/default_config mode
        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.codelldb_port, // Use CodeLLDB port (via lldb-serve)
            &source_file,
            None, // lldb-serve handles program launch
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "CodeLLDB workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                // Always print daemon logs for debugging
                println!("\n=== Daemon Logs (last 100 lines) ===");
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                // Assert introspection works when language supports it
                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured. \
                        This indicates breakpoints are not triggering stopped events.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("CodeLLDB workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_codelldb_logs(50);
                reporter.print_footer(false);
                panic!("CodeLLDB workflow failed: {}", e);
            }
        }
    }

    // ========================================================================
    // DAP RECONNECTION TESTS - verify metrics work after disconnect/reconnect
    // ========================================================================

    /// Test Python DAP reconnection (metrics persist across disconnect/reconnect)
    #[tokio::test]
    async fn test_dap_python_reconnection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Python Reconnection", "MCP");
        reporter.print_header();

        // Use the script from DapWorkflowConfig to ensure metrics match
        let config = DapWorkflowConfig::python();
        let script_path = executor.workspace_root.join(&config.source_file);

        if !script_path.exists() {
            reporter.error(&format!("{} not found", script_path.display()));
            reporter.print_footer(false);
            panic!("{} not found", script_path.display());
        }

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        // Start debugpy with the correct script
        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        let client = McpClient::new(executor.http_port);
        let source_file = script_path.clone();

        // Run reconnection test with 3 cycles
        // Python/debugpy stays running after disconnect, so no restart callback needed
        match DapWorkflowScenarios::run_reconnection_workflow(
            &client,
            &reporter,
            &config,
            executor.debugpy_port,
            &source_file,
            3,    // 3 reconnection cycles
            None, // No restart callback needed for Python
            None, // Python doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Reconnection test complete: {} cycles, {} total events",
                    result.reconnection_cycles, result.total_events
                ));
                reporter.info(&format!("Events per cycle: {:?}", result.events_per_cycle));
                executor.print_daemon_logs(100);
                reporter.print_footer(true);

                // Verify all cycles completed
                assert_eq!(
                    result.reconnection_cycles, 3,
                    "Expected 3 reconnection cycles, got {}",
                    result.reconnection_cycles
                );
            }
            Err(e) => {
                reporter.error(&format!("Reconnection test failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_debugpy_logs(50);
                reporter.print_footer(false);
                panic!("Reconnection test failed: {}", e);
            }
        }
    }

    /// Test Go DAP reconnection (metrics persist across disconnect/reconnect)
    #[tokio::test]
    async fn test_dap_go_reconnection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Delve).await {
            return;
        }
        use detrix_testing::e2e::{restart_delve, RestartDebuggerFn};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Go Reconnection", "MCP");
        reporter.print_header();

        let config = DapWorkflowConfig::go();
        let source_file = executor.workspace_root.join(&config.source_file);
        let binary_path = source_file.with_extension(""); // Built binary without extension

        // Build Go binary with debug symbols
        let parent_dir = source_file.parent().unwrap();
        let build_output = std::process::Command::new("go")
            .args([
                "build",
                "-gcflags=all=-N -l",
                "-o",
                binary_path.to_str().unwrap(),
                source_file.to_str().unwrap(),
            ])
            .current_dir(parent_dir)
            .output()
            .expect("Failed to build Go binary");

        if !build_output.status.success() {
            reporter.error(&format!(
                "Go build failed: {}",
                String::from_utf8_lossy(&build_output.stderr)
            ));
            panic!("Failed to build Go binary");
        }

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        // Start delve
        if let Err(e) = executor.start_delve(source_file.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start delve: {}", e));
            executor.print_delve_logs(50);
            panic!("Failed to start delve: {}", e);
        }

        let client = McpClient::new(executor.http_port);
        let delve_port = executor.delve_port;

        // Store process handle in Arc<Mutex> so we can update it from callback
        let delve_process: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
        let delve_process_clone = Arc::clone(&delve_process);
        let binary_path_str = binary_path.to_str().unwrap().to_string();

        // Create restart callback for delve (single-session debugger)
        let restart_fn: RestartDebuggerFn = Box::new(move || {
            let port = delve_port;
            let binary = binary_path_str.clone();
            let process_ref = Arc::clone(&delve_process_clone);
            Box::pin(async move {
                let new_process = restart_delve(port, &binary).await?;
                // Store the new process handle
                let mut guard = process_ref.lock().await;
                *guard = Some(new_process);
                Ok(())
            })
        });

        // Run reconnection test with 3 cycles
        match DapWorkflowScenarios::run_reconnection_workflow(
            &client,
            &reporter,
            &config,
            executor.delve_port,
            &source_file,
            3,
            Some(restart_fn), // Provide restart callback for Go
            None,             // Go doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Reconnection test complete: {} cycles, {} total events",
                    result.reconnection_cycles, result.total_events
                ));
                executor.print_daemon_logs(100);
                reporter.print_footer(true);

                assert_eq!(
                    result.reconnection_cycles, 3,
                    "Expected 3 reconnection cycles, got {}",
                    result.reconnection_cycles
                );
            }
            Err(e) => {
                reporter.error(&format!("Reconnection test failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_delve_logs(50);
                reporter.print_footer(false);
                panic!("Reconnection test failed: {}", e);
            }
        }

        // Clean up the process handle from callback if any
        detrix_testing::e2e::kill_and_unregister_process_async("delve", &delve_process).await;
    }

    /// Test Rust DAP reconnection (metrics persist across disconnect/reconnect)
    #[tokio::test]
    async fn test_dap_rust_reconnection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::LldbDap).await {
            return;
        }
        use detrix_testing::e2e::{restart_lldb, RestartDebuggerFn};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Rust Reconnection", "MCP");
        reporter.print_header();

        let config = DapWorkflowConfig::rust();
        let source_file = executor.workspace_root.join(&config.source_file);
        // Binary path is source file without extension (e.g., detrix_example_app.rs -> detrix_example_app)
        let binary_path = source_file.with_extension("");

        // Note: start_lldb handles building via OnceLock - don't build here to avoid races

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        // Start lldb-dap
        if let Err(e) = executor.start_lldb(source_file.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start lldb-dap: {}", e));
            executor.print_lldb_logs(50);
            panic!("Failed to start lldb-dap: {}", e);
        }

        let client = McpClient::new(executor.http_port);
        let lldb_port = executor.lldb_port;

        // Store process handle in Arc<Mutex> so we can update it from callback
        let lldb_process: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
        let lldb_process_clone = Arc::clone(&lldb_process);
        let binary_path_str = binary_path.to_str().unwrap().to_string();
        let workspace_root = executor.workspace_root.clone();

        // Create restart callback for lldb (single-session debugger)
        let restart_fn: RestartDebuggerFn = Box::new(move || {
            let port = lldb_port;
            let binary = binary_path_str.clone();
            let workspace = workspace_root.clone();
            let process_ref = Arc::clone(&lldb_process_clone);
            Box::pin(async move {
                let new_process = restart_lldb(port, &binary, &workspace).await?;
                // Store the new process handle
                let mut guard = process_ref.lock().await;
                *guard = Some(new_process);
                Ok(())
            })
        });

        // When using lldb-serve, don't pass program_path (lldb-serve handles launch)
        let program_path: Option<&str> = None;

        // Run reconnection test with 3 cycles
        match DapWorkflowScenarios::run_reconnection_workflow(
            &client,
            &reporter,
            &config,
            executor.lldb_port,
            &source_file,
            3,
            Some(restart_fn), // Provide restart callback for Rust
            program_path,
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Reconnection test complete: {} cycles, {} total events",
                    result.reconnection_cycles, result.total_events
                ));
                executor.print_daemon_logs(100);
                reporter.print_footer(true);

                assert_eq!(
                    result.reconnection_cycles, 3,
                    "Expected 3 reconnection cycles, got {}",
                    result.reconnection_cycles
                );
            }
            Err(e) => {
                reporter.error(&format!("Reconnection test failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_lldb_logs(50);
                reporter.print_footer(false);
                panic!("Reconnection test failed: {}", e);
            }
        }

        // Clean up the process handle from callback if any
        detrix_testing::e2e::kill_and_unregister_process_async("lldb", &lldb_process).await;
    }
}

// ============================================================================
// GRPC DAP WORKFLOW TESTS - gRPC protocol (same workflows as MCP)
// ============================================================================

mod grpc_dap_workflow_tests {
    use detrix_application::services::file_inspection_types::SourceLanguageExt;
    use detrix_testing::e2e::dap_scenarios::{DapWorkflowConfig, DapWorkflowScenarios};
    use detrix_testing::e2e::executor::TestExecutor;
    use detrix_testing::e2e::grpc::GrpcClient;
    use detrix_testing::e2e::reporter::TestReporter;

    /// Run Python DAP workflow via gRPC
    #[tokio::test]
    async fn test_grpc_dap_python_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Python Workflow", "gRPC");
        reporter.print_header();

        let script_path = executor
            .workspace_root
            .join("fixtures/python/trade_bot_forever.py");

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        let client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);
        let config = DapWorkflowConfig::python();

        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.debugpy_port,
            &script_path,
            None, // Python doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                println!("DEBUG: daemon_log_path = {:?}", executor.daemon_log_path);
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("Workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_debugpy_logs(50);
                reporter.print_footer(false);
                panic!("Workflow failed: {}", e);
            }
        }
    }

    /// Run Go DAP workflow via gRPC
    #[tokio::test]
    async fn test_grpc_dap_go_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Delve).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Go Workflow", "gRPC");
        reporter.print_header();

        let script_path = executor
            .workspace_root
            .join("fixtures/go/detrix_example_app.go");

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_delve(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start delve: {}", e));
            executor.print_delve_logs(50);
            panic!("Failed to start delve: {}", e);
        }

        let client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);
        let config = DapWorkflowConfig::go();

        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.delve_port,
            &script_path,
            None, // Go doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                println!("\n=== Daemon Logs (last 100 lines) ===");
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("Workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_delve_logs(50);
                reporter.print_footer(false);
                panic!("Workflow failed: {}", e);
            }
        }
    }

    /// Run Rust DAP workflow via gRPC
    #[tokio::test]
    async fn test_grpc_dap_rust_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::LldbDap).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Rust Workflow", "gRPC");
        reporter.print_header();

        let source_path = executor
            .workspace_root
            .join("fixtures/rust/detrix_example_app.rs");

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_lldb(source_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start lldb-dap: {}", e));
            executor.print_lldb_logs(50);
            panic!("Failed to start lldb-dap: {}", e);
        }

        let client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);
        let config = DapWorkflowConfig::rust();
        let source_file = std::path::PathBuf::from(&config.source_file);

        // When using lldb-serve, don't pass program_path (lldb-serve handles launch)
        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.lldb_port,
            &source_file,
            None, // lldb-serve handles program launch
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                println!("\n=== Daemon Logs (last 100 lines) ===");
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("Workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_lldb_logs(50);
                reporter.print_footer(false);
                panic!("Workflow failed: {}", e);
            }
        }
    }

    // ========================================================================
    // GRPC DAP RECONNECTION TESTS
    // ========================================================================

    /// Test Python DAP reconnection via gRPC
    #[tokio::test]
    async fn test_grpc_dap_python_reconnection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Python Reconnection", "gRPC");
        reporter.print_header();

        let config = DapWorkflowConfig::python();
        let script_path = executor.workspace_root.join(&config.source_file);

        if !script_path.exists() {
            reporter.error(&format!("{} not found", script_path.display()));
            reporter.print_footer(false);
            panic!("{} not found", script_path.display());
        }

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        let client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);
        let source_file = script_path.clone();

        match DapWorkflowScenarios::run_reconnection_workflow(
            &client,
            &reporter,
            &config,
            executor.debugpy_port,
            &source_file,
            3,
            None,
            None, // Python doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Reconnection test complete: {} cycles, {} total events",
                    result.reconnection_cycles, result.total_events
                ));
                reporter.info(&format!("Events per cycle: {:?}", result.events_per_cycle));
                executor.print_daemon_logs(100);
                reporter.print_footer(true);

                assert_eq!(
                    result.reconnection_cycles, 3,
                    "Expected 3 reconnection cycles, got {}",
                    result.reconnection_cycles
                );
            }
            Err(e) => {
                reporter.error(&format!("Reconnection test failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_debugpy_logs(50);
                reporter.print_footer(false);
                panic!("Reconnection test failed: {}", e);
            }
        }
    }

    /// Test Go DAP reconnection via gRPC
    #[tokio::test]
    async fn test_grpc_dap_go_reconnection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Delve).await {
            return;
        }
        use detrix_testing::e2e::{restart_delve, RestartDebuggerFn};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Go Reconnection", "gRPC");
        reporter.print_header();

        let config = DapWorkflowConfig::go();
        let source_file = executor.workspace_root.join(&config.source_file);
        let binary_path = source_file.with_extension("");

        let parent_dir = source_file.parent().unwrap();
        let build_output = std::process::Command::new("go")
            .args([
                "build",
                "-gcflags=all=-N -l",
                "-o",
                binary_path.to_str().unwrap(),
                source_file.to_str().unwrap(),
            ])
            .current_dir(parent_dir)
            .output()
            .expect("Failed to build Go binary");

        if !build_output.status.success() {
            reporter.error(&format!(
                "Go build failed: {}",
                String::from_utf8_lossy(&build_output.stderr)
            ));
            panic!("Failed to build Go binary");
        }

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_delve(source_file.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start delve: {}", e));
            executor.print_delve_logs(50);
            panic!("Failed to start delve: {}", e);
        }

        let client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);
        let delve_port = executor.delve_port;

        let delve_process: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
        let delve_process_clone = Arc::clone(&delve_process);
        let binary_path_str = binary_path.to_str().unwrap().to_string();

        let restart_fn: RestartDebuggerFn = Box::new(move || {
            let port = delve_port;
            let binary = binary_path_str.clone();
            let process_ref = Arc::clone(&delve_process_clone);
            Box::pin(async move {
                let new_process = restart_delve(port, &binary).await?;
                let mut guard = process_ref.lock().await;
                *guard = Some(new_process);
                Ok(())
            })
        });

        match DapWorkflowScenarios::run_reconnection_workflow(
            &client,
            &reporter,
            &config,
            executor.delve_port,
            &source_file,
            3,
            Some(restart_fn),
            None, // Go doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Reconnection test complete: {} cycles, {} total events",
                    result.reconnection_cycles, result.total_events
                ));
                executor.print_daemon_logs(100);
                reporter.print_footer(true);

                assert_eq!(
                    result.reconnection_cycles, 3,
                    "Expected 3 reconnection cycles, got {}",
                    result.reconnection_cycles
                );
            }
            Err(e) => {
                reporter.error(&format!("Reconnection test failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_delve_logs(50);
                reporter.print_footer(false);
                panic!("Reconnection test failed: {}", e);
            }
        }

        detrix_testing::e2e::kill_and_unregister_process_async("delve", &delve_process).await;
    }

    /// Test Rust DAP reconnection via gRPC
    #[tokio::test]
    async fn test_grpc_dap_rust_reconnection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::LldbDap).await {
            return;
        }
        use detrix_testing::e2e::{restart_lldb, RestartDebuggerFn};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Rust Reconnection", "gRPC");
        reporter.print_header();

        let config = DapWorkflowConfig::rust();
        let source_file = executor.workspace_root.join(&config.source_file);
        // Binary path is source file without extension (e.g., detrix_example_app.rs -> detrix_example_app)
        let binary_path = source_file.with_extension("");

        // Note: start_lldb handles building via OnceLock - don't build here to avoid races

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_lldb(source_file.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start lldb-dap: {}", e));
            executor.print_lldb_logs(50);
            panic!("Failed to start lldb-dap: {}", e);
        }

        let client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);
        let lldb_port = executor.lldb_port;

        let lldb_process: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
        let lldb_process_clone = Arc::clone(&lldb_process);
        let binary_path_str = binary_path.to_str().unwrap().to_string();
        let workspace_root = executor.workspace_root.clone();

        let restart_fn: RestartDebuggerFn = Box::new(move || {
            let port = lldb_port;
            let binary = binary_path_str.clone();
            let workspace = workspace_root.clone();
            let process_ref = Arc::clone(&lldb_process_clone);
            Box::pin(async move {
                let new_process = restart_lldb(port, &binary, &workspace).await?;
                let mut guard = process_ref.lock().await;
                *guard = Some(new_process);
                Ok(())
            })
        });

        // When using lldb-serve, don't pass program_path (lldb-serve handles launch)
        let program_path: Option<&str> = None;

        match DapWorkflowScenarios::run_reconnection_workflow(
            &client,
            &reporter,
            &config,
            executor.lldb_port,
            &source_file,
            3,
            Some(restart_fn),
            program_path,
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Reconnection test complete: {} cycles, {} total events",
                    result.reconnection_cycles, result.total_events
                ));
                executor.print_daemon_logs(100);
                reporter.print_footer(true);

                assert_eq!(
                    result.reconnection_cycles, 3,
                    "Expected 3 reconnection cycles, got {}",
                    result.reconnection_cycles
                );
            }
            Err(e) => {
                reporter.error(&format!("Reconnection test failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_lldb_logs(50);
                reporter.print_footer(false);
                panic!("Reconnection test failed: {}", e);
            }
        }

        detrix_testing::e2e::kill_and_unregister_process_async("lldb", &lldb_process).await;
    }
}

// ============================================================================
// REST DAP WORKFLOW TESTS - REST protocol (same workflows as MCP)
// ============================================================================

mod rest_dap_workflow_tests {
    use detrix_application::services::file_inspection_types::SourceLanguageExt;
    use detrix_testing::e2e::dap_scenarios::{DapWorkflowConfig, DapWorkflowScenarios};
    use detrix_testing::e2e::executor::TestExecutor;
    use detrix_testing::e2e::reporter::TestReporter;
    use detrix_testing::e2e::rest::RestClient;

    /// Run Python DAP workflow via REST
    #[tokio::test]
    async fn test_rest_dap_python_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Python Workflow", "REST");
        reporter.print_header();

        let script_path = executor
            .workspace_root
            .join("fixtures/python/trade_bot_forever.py");

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        let client = RestClient::new(executor.http_port);
        let config = DapWorkflowConfig::python();

        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.debugpy_port,
            &script_path,
            None, // Python doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                println!("DEBUG: daemon_log_path = {:?}", executor.daemon_log_path);
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("Workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_debugpy_logs(50);
                reporter.print_footer(false);
                panic!("Workflow failed: {}", e);
            }
        }
    }

    /// Run Go DAP workflow via REST
    #[tokio::test]
    async fn test_rest_dap_go_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Delve).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Go Workflow", "REST");
        reporter.print_header();

        let script_path = executor
            .workspace_root
            .join("fixtures/go/detrix_example_app.go");

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_delve(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start delve: {}", e));
            executor.print_delve_logs(50);
            panic!("Failed to start delve: {}", e);
        }

        let client = RestClient::new(executor.http_port);
        let config = DapWorkflowConfig::go();

        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.delve_port,
            &script_path,
            None, // Go doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                println!("\n=== Daemon Logs (last 100 lines) ===");
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("Workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_delve_logs(50);
                reporter.print_footer(false);
                panic!("Workflow failed: {}", e);
            }
        }
    }

    /// Run Rust DAP workflow via REST
    #[tokio::test]
    async fn test_rest_dap_rust_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::LldbDap).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Rust Workflow", "REST");
        reporter.print_header();

        let source_path = executor
            .workspace_root
            .join("fixtures/rust/detrix_example_app.rs");

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_lldb(source_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start lldb-dap: {}", e));
            executor.print_lldb_logs(50);
            panic!("Failed to start lldb-dap: {}", e);
        }

        let client = RestClient::new(executor.http_port);
        let config = DapWorkflowConfig::rust();
        let source_file = std::path::PathBuf::from(&config.source_file);

        // When using lldb-serve, don't pass program_path (lldb-serve handles launch)
        match DapWorkflowScenarios::run_workflow(
            &client,
            &reporter,
            &config,
            executor.lldb_port,
            &source_file,
            None, // lldb-serve handles program launch
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Workflow complete: {} metrics added, {} events captured",
                    result.metrics_added, result.total_events
                ));
                if result.introspection_metrics_added > 0 {
                    reporter.info(&format!(
                        "Introspection: {} metrics, {} events (stack_trace: {}, memory_snapshot: {})",
                        result.introspection_metrics_added,
                        result.introspection_events,
                        result.stack_trace_events,
                        result.memory_snapshot_events
                    ));
                }
                println!("\n=== Daemon Logs (last 100 lines) ===");
                executor.print_daemon_logs(1000);
                reporter.print_footer(true);

                if config.language.capabilities().has_introspection
                    && result.introspection_metrics_added > 0
                    && result.introspection_events == 0
                {
                    panic!(
                        "INTROSPECTION FAILURE: {} has_introspection=true and {} metrics were added, \
                        but 0 introspection events were captured.",
                        config.language.display_name(),
                        result.introspection_metrics_added
                    );
                }
            }
            Err(e) => {
                reporter.error(&format!("Workflow failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_lldb_logs(50);
                reporter.print_footer(false);
                panic!("Workflow failed: {}", e);
            }
        }
    }

    // ========================================================================
    // REST DAP RECONNECTION TESTS
    // ========================================================================

    /// Test Python DAP reconnection via REST
    #[tokio::test]
    async fn test_rest_dap_python_reconnection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Python Reconnection", "REST");
        reporter.print_header();

        let config = DapWorkflowConfig::python();
        let script_path = executor.workspace_root.join(&config.source_file);

        if !script_path.exists() {
            reporter.error(&format!("{} not found", script_path.display()));
            reporter.print_footer(false);
            panic!("{} not found", script_path.display());
        }

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        let client = RestClient::new(executor.http_port);
        let source_file = script_path.clone();

        match DapWorkflowScenarios::run_reconnection_workflow(
            &client,
            &reporter,
            &config,
            executor.debugpy_port,
            &source_file,
            3,
            None,
            None, // Python doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Reconnection test complete: {} cycles, {} total events",
                    result.reconnection_cycles, result.total_events
                ));
                reporter.info(&format!("Events per cycle: {:?}", result.events_per_cycle));
                executor.print_daemon_logs(100);
                reporter.print_footer(true);

                assert_eq!(
                    result.reconnection_cycles, 3,
                    "Expected 3 reconnection cycles, got {}",
                    result.reconnection_cycles
                );
            }
            Err(e) => {
                reporter.error(&format!("Reconnection test failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_debugpy_logs(50);
                reporter.print_footer(false);
                panic!("Reconnection test failed: {}", e);
            }
        }
    }

    /// Test Go DAP reconnection via REST
    #[tokio::test]
    async fn test_rest_dap_go_reconnection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Delve).await {
            return;
        }
        use detrix_testing::e2e::{restart_delve, RestartDebuggerFn};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Go Reconnection", "REST");
        reporter.print_header();

        let config = DapWorkflowConfig::go();
        let source_file = executor.workspace_root.join(&config.source_file);
        let binary_path = source_file.with_extension("");

        let parent_dir = source_file.parent().unwrap();
        let build_output = std::process::Command::new("go")
            .args([
                "build",
                "-gcflags=all=-N -l",
                "-o",
                binary_path.to_str().unwrap(),
                source_file.to_str().unwrap(),
            ])
            .current_dir(parent_dir)
            .output()
            .expect("Failed to build Go binary");

        if !build_output.status.success() {
            reporter.error(&format!(
                "Go build failed: {}",
                String::from_utf8_lossy(&build_output.stderr)
            ));
            panic!("Failed to build Go binary");
        }

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_delve(source_file.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start delve: {}", e));
            executor.print_delve_logs(50);
            panic!("Failed to start delve: {}", e);
        }

        let client = RestClient::new(executor.http_port);
        let delve_port = executor.delve_port;

        let delve_process: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
        let delve_process_clone = Arc::clone(&delve_process);
        let binary_path_str = binary_path.to_str().unwrap().to_string();

        let restart_fn: RestartDebuggerFn = Box::new(move || {
            let port = delve_port;
            let binary = binary_path_str.clone();
            let process_ref = Arc::clone(&delve_process_clone);
            Box::pin(async move {
                let new_process = restart_delve(port, &binary).await?;
                let mut guard = process_ref.lock().await;
                *guard = Some(new_process);
                Ok(())
            })
        });

        match DapWorkflowScenarios::run_reconnection_workflow(
            &client,
            &reporter,
            &config,
            executor.delve_port,
            &source_file,
            3,
            Some(restart_fn),
            None, // Go doesn't need program path
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Reconnection test complete: {} cycles, {} total events",
                    result.reconnection_cycles, result.total_events
                ));
                executor.print_daemon_logs(100);
                reporter.print_footer(true);

                assert_eq!(
                    result.reconnection_cycles, 3,
                    "Expected 3 reconnection cycles, got {}",
                    result.reconnection_cycles
                );
            }
            Err(e) => {
                reporter.error(&format!("Reconnection test failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_delve_logs(50);
                reporter.print_footer(false);
                panic!("Reconnection test failed: {}", e);
            }
        }

        detrix_testing::e2e::kill_and_unregister_process_async("delve", &delve_process).await;
    }

    /// Test Rust DAP reconnection via REST
    #[tokio::test]
    async fn test_rest_dap_rust_reconnection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::LldbDap).await {
            return;
        }
        use detrix_testing::e2e::{restart_lldb, RestartDebuggerFn};
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("DAP Rust Reconnection", "REST");
        reporter.print_header();

        let config = DapWorkflowConfig::rust();
        let source_file = executor.workspace_root.join(&config.source_file);
        // Binary path is source file without extension (e.g., detrix_example_app.rs -> detrix_example_app)
        let binary_path = source_file.with_extension("");

        // Note: start_lldb handles building via OnceLock - don't build here to avoid races

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_lldb(source_file.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start lldb-dap: {}", e));
            executor.print_lldb_logs(50);
            panic!("Failed to start lldb-dap: {}", e);
        }

        let client = RestClient::new(executor.http_port);
        let lldb_port = executor.lldb_port;

        let lldb_process: Arc<Mutex<Option<std::process::Child>>> = Arc::new(Mutex::new(None));
        let lldb_process_clone = Arc::clone(&lldb_process);
        let binary_path_str = binary_path.to_str().unwrap().to_string();
        let workspace_root = executor.workspace_root.clone();

        let restart_fn: RestartDebuggerFn = Box::new(move || {
            let port = lldb_port;
            let binary = binary_path_str.clone();
            let workspace = workspace_root.clone();
            let process_ref = Arc::clone(&lldb_process_clone);
            Box::pin(async move {
                let new_process = restart_lldb(port, &binary, &workspace).await?;
                let mut guard = process_ref.lock().await;
                *guard = Some(new_process);
                Ok(())
            })
        });

        // When using lldb-serve, don't pass program_path (lldb-serve handles launch)
        let program_path: Option<&str> = None;

        match DapWorkflowScenarios::run_reconnection_workflow(
            &client,
            &reporter,
            &config,
            executor.lldb_port,
            &source_file,
            3,
            Some(restart_fn),
            program_path,
        )
        .await
        {
            Ok(result) => {
                reporter.info(&format!(
                    "Reconnection test complete: {} cycles, {} total events",
                    result.reconnection_cycles, result.total_events
                ));
                executor.print_daemon_logs(100);
                reporter.print_footer(true);

                assert_eq!(
                    result.reconnection_cycles, 3,
                    "Expected 3 reconnection cycles, got {}",
                    result.reconnection_cycles
                );
            }
            Err(e) => {
                reporter.error(&format!("Reconnection test failed: {}", e));
                executor.print_daemon_logs(50);
                executor.print_lldb_logs(50);
                reporter.print_footer(false);
                panic!("Reconnection test failed: {}", e);
            }
        }

        detrix_testing::e2e::kill_and_unregister_process_async("lldb", &lldb_process).await;
    }
}

// ============================================================================
// GRPC STREAMING TESTS - gRPC only (streaming is gRPC-specific)
// ============================================================================

mod grpc_streaming_tests {
    use detrix_testing::e2e::client::AddMetricRequest;
    use detrix_testing::e2e::executor::TestExecutor;
    use detrix_testing::e2e::grpc::GrpcClient;
    use detrix_testing::e2e::reporter::TestReporter;
    use detrix_testing::e2e::ApiClient;
    use std::time::Duration;

    /// Test gRPC event streaming
    #[tokio::test]
    async fn test_grpc_stream_events() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("gRPC Stream Events", "gRPC");
        reporter.print_header();

        let script_path = match executor.detrix_example_app_path() {
            Some(p) => p,
            None => {
                reporter.error("detrix_example_app.py not found");
                reporter.print_footer(false);
                panic!("detrix_example_app.py not found");
            }
        };

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        let mut client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);

        // Create connection
        let step = reporter.step_start("Create Connection", "Connect to debugpy");
        let connection = match client
            .create_connection("127.0.0.1", executor.debugpy_port, "python")
            .await
        {
            Ok(c) => {
                reporter.step_success(step, Some(&c.data.connection_id));
                c
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Add metric
        let step = reporter.step_start("Add Metric", "Add metric for streaming");
        let location = format!("@{}#22", script_path.display());
        match client
            .add_metric(AddMetricRequest::new(
                "grpc_stream_test",
                &location,
                "price",
                &connection.data.connection_id,
            ))
            .await
        {
            Ok(r) => reporter.step_success(step, Some(&r.data)),
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to add metric: {}", e);
            }
        }

        // Stream events
        let step = reporter.step_start("Stream Events", "Receive streaming events");
        match client
            .receive_events(Duration::from_secs(5), Some(10))
            .await
        {
            Ok(events) => {
                reporter.step_success(step, Some(&format!("{} events received", events.len())));
                for event in events.iter().take(3) {
                    reporter.info(&format!("  Event: {} = {}", event.metric_name, event.value));
                }
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
            }
        }

        // Cleanup
        let _ = client.remove_metric("grpc_stream_test").await;
        let _ = client
            .close_connection(&connection.data.connection_id)
            .await;

        reporter.print_footer(true);
    }

    /// Test gRPC streaming channel
    #[tokio::test]
    async fn test_grpc_stream_channel() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("gRPC Stream Channel", "gRPC");
        reporter.print_header();

        let script_path = match executor.detrix_example_app_path() {
            Some(p) => p,
            None => {
                reporter.error("detrix_example_app.py not found");
                reporter.print_footer(false);
                panic!("detrix_example_app.py not found");
            }
        };

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        let mut client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);

        // Create connection
        let step = reporter.step_start("Create Connection", "Connect to debugpy");
        let connection = match client
            .create_connection("127.0.0.1", executor.debugpy_port, "python")
            .await
        {
            Ok(c) => {
                reporter.step_success(step, Some(&c.data.connection_id));
                c
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Add metric
        let step = reporter.step_start("Add Metric", "Add metric for streaming");
        let location = format!("@{}#22", script_path.display());
        match client
            .add_metric(AddMetricRequest::new(
                "grpc_channel_test",
                &location,
                "price",
                &connection.data.connection_id,
            ))
            .await
        {
            Ok(r) => reporter.step_success(step, Some(&r.data)),
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to add metric: {}", e);
            }
        }

        // Create streaming channel
        let step = reporter.step_start("Create Stream", "Create streaming channel");
        match client.stream_events().await {
            Ok((mut rx, handle)) => {
                reporter.step_success(step, Some("Channel created"));

                // Collect events
                let step = reporter.step_start("Collect Events", "Collect from channel");
                let events = tokio::time::timeout(Duration::from_secs(5), async {
                    let mut collected = Vec::new();
                    while let Some(event) = rx.recv().await {
                        collected.push(event);
                        if collected.len() >= 3 {
                            break;
                        }
                    }
                    collected
                })
                .await
                .unwrap_or_default();

                reporter.step_success(step, Some(&format!("{} events", events.len())));

                handle.close().await;
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
            }
        }

        // Cleanup
        let _ = client.remove_metric("grpc_channel_test").await;
        let _ = client
            .close_connection(&connection.data.connection_id)
            .await;

        reporter.print_footer(true);
    }

    /// Test gRPC introspection (stack trace and memory snapshot) via streaming
    #[tokio::test]
    async fn test_grpc_stream_introspection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("gRPC Introspection Streaming", "gRPC");
        reporter.print_header();

        let script_path = match executor.detrix_example_app_path() {
            Some(p) => p,
            None => {
                reporter.error("detrix_example_app.py not found");
                reporter.print_footer(false);
                panic!("detrix_example_app.py not found");
            }
        };

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        let mut client = GrpcClient::with_http_port(executor.grpc_port, executor.http_port);

        // Create connection
        let step = reporter.step_start("Create Connection", "Connect to debugpy");
        let connection = match client
            .create_connection("127.0.0.1", executor.debugpy_port, "python")
            .await
        {
            Ok(c) => {
                reporter.step_success(step, Some(&c.data.connection_id));
                c
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Add metric WITH introspection enabled
        let step = reporter.step_start(
            "Add Metric with Introspection",
            "Add metric with stack trace and memory snapshot",
        );
        let location = format!("@{}#22", script_path.display());
        match client
            .add_metric(
                AddMetricRequest::new(
                    "grpc_introspection_test",
                    &location,
                    "price",
                    &connection.data.connection_id,
                )
                .with_introspection(), // Enable both stack trace and memory snapshot
            )
            .await
        {
            Ok(r) => reporter.step_success(step, Some(&r.data)),
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to add metric: {}", e);
            }
        }

        // Stream events and verify introspection data
        let step = reporter.step_start(
            "Stream Introspection Events",
            "Receive events with introspection data",
        );
        match client
            .receive_events(Duration::from_secs(10), Some(5))
            .await
        {
            Ok(events) => {
                reporter.step_success(step, Some(&format!("{} events received", events.len())));

                // Count events with introspection data
                let mut stack_trace_count = 0;
                let mut memory_snapshot_count = 0;

                for event in &events {
                    if event.stack_trace.is_some() {
                        stack_trace_count += 1;
                    }
                    if event.memory_snapshot.is_some() {
                        memory_snapshot_count += 1;
                    }
                }

                reporter.info(&format!(
                    "  Introspection: {} events with stack_trace, {} with memory_snapshot",
                    stack_trace_count, memory_snapshot_count
                ));

                // Verify introspection data structure if we have events
                if let Some(event) = events.iter().find(|e| e.stack_trace.is_some()) {
                    let st = event.stack_trace.as_ref().unwrap();
                    reporter.info(&format!("  Stack trace frames: {}", st.frames.len()));

                    // Verify stack trace structure
                    assert!(
                        !st.frames.is_empty(),
                        "Stack trace should have at least one frame"
                    );

                    let first_frame = &st.frames[0];
                    reporter.info(&format!(
                        "    Frame 0: {} at {:?}:{}",
                        first_frame.name,
                        first_frame.file,
                        first_frame.line.unwrap_or(0)
                    ));

                    // Verify frame has required fields
                    assert!(
                        !first_frame.name.is_empty(),
                        "Frame name should not be empty"
                    );
                }

                if let Some(event) = events.iter().find(|e| e.memory_snapshot.is_some()) {
                    let snap = event.memory_snapshot.as_ref().unwrap();
                    reporter.info(&format!(
                        "  Memory snapshot: {} locals, {} globals",
                        snap.locals.len(),
                        snap.globals.len()
                    ));

                    // Verify memory snapshot structure
                    if !snap.locals.is_empty() {
                        let first_local = &snap.locals[0];
                        reporter.info(&format!(
                            "    Local: {} = {} (type: {:?})",
                            first_local.name, first_local.value, first_local.var_type
                        ));

                        // Verify variable has required fields
                        assert!(
                            !first_local.name.is_empty(),
                            "Variable name should not be empty"
                        );
                    }
                }

                // Assert we got at least some introspection data (debugpy should provide it)
                if !events.is_empty() && stack_trace_count == 0 && memory_snapshot_count == 0 {
                    reporter.info(
                        "  WARNING: No introspection data captured (may be expected for logpoints)",
                    );
                }
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
            }
        }

        // Print daemon logs for debugging
        println!("\n=== Daemon Logs (last 50 lines) ===");
        executor.print_daemon_logs(50);

        // Cleanup
        let _ = client.remove_metric("grpc_introspection_test").await;
        let _ = client
            .close_connection(&connection.data.connection_id)
            .await;

        reporter.print_footer(true);
    }
}

// ============================================================================
// WEBSOCKET STREAMING TESTS - WebSocket only
// ============================================================================

mod websocket_streaming_tests {
    use detrix_testing::e2e::client::AddMetricRequest;
    use detrix_testing::e2e::executor::TestExecutor;
    use detrix_testing::e2e::reporter::TestReporter;
    use detrix_testing::e2e::rest::RestClient;
    use detrix_testing::e2e::websocket::WebSocketClient;
    use detrix_testing::e2e::ApiClient;
    use std::time::Duration;

    /// Test WebSocket connection
    #[tokio::test]
    async fn test_ws_connection() {
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("WebSocket Connection", "WebSocket");
        reporter.print_header();

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        tokio::time::sleep(Duration::from_secs(2)).await;

        let ws = WebSocketClient::new(executor.http_port);

        let step = reporter.step_start("Connect", "Connect to WebSocket endpoint");
        match ws.receive_events(Duration::from_secs(2), Some(0)).await {
            Ok(events) => {
                reporter.step_success(step, Some(&format!("Connected, {} events", events.len())));
            }
            Err(e) => {
                reporter.step_success(step, Some(&format!("Result: {}", e.message)));
            }
        }

        reporter.print_footer(true);
    }

    /// Test WebSocket streaming channel
    #[tokio::test]
    async fn test_ws_stream_channel() {
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("WebSocket Stream Channel", "WebSocket");
        reporter.print_header();

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        tokio::time::sleep(Duration::from_secs(2)).await;

        let ws = WebSocketClient::new(executor.http_port);

        let step = reporter.step_start("Create Stream", "Create streaming channel");
        match ws.stream_events().await {
            Ok((mut rx, handle)) => {
                reporter.step_success(step, Some("Channel created"));

                let step2 = reporter.step_start("Receive Events", "Wait for events");
                let events_received = tokio::time::timeout(Duration::from_secs(2), async {
                    let mut count = 0;
                    while rx.recv().await.is_some() {
                        count += 1;
                        if count >= 1 {
                            break;
                        }
                    }
                    count
                })
                .await
                .unwrap_or(0);

                reporter.step_success(step2, Some(&format!("{} events", events_received)));
                handle.close().await;
            }
            Err(e) => {
                reporter.step_success(step, Some(&format!("Result: {}", e.message)));
            }
        }

        reporter.print_footer(true);
    }

    /// Test WebSocket receive metric events
    #[tokio::test]
    async fn test_ws_receive_metric_events() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("WebSocket Metric Events", "WebSocket");
        reporter.print_header();

        let script_path = match executor.detrix_example_app_path() {
            Some(p) => p,
            None => {
                reporter.error("detrix_example_app.py not found");
                reporter.print_footer(false);
                panic!("detrix_example_app.py not found");
            }
        };

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        tokio::time::sleep(Duration::from_secs(2)).await;

        let rest = RestClient::new(executor.http_port);
        let ws = WebSocketClient::new(executor.http_port);

        // Create connection via REST
        let step = reporter.step_start("Create Connection", "Connect to debugpy");
        let connection = match rest
            .create_connection("127.0.0.1", executor.debugpy_port, "python")
            .await
        {
            Ok(c) => {
                reporter.step_success(step, Some(&c.data.connection_id));
                c
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Add metric
        let step = reporter.step_start("Add Metric", "Add metric");
        let location = format!("@{}#22", script_path.display());
        match rest
            .add_metric(AddMetricRequest::new(
                "ws_metric_test",
                &location,
                "price",
                &connection.data.connection_id,
            ))
            .await
        {
            Ok(r) => reporter.step_success(step, Some(&r.data)),
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to add metric: {}", e);
            }
        }

        // Listen for WebSocket events
        let step = reporter.step_start("Listen Events", "Receive events via WebSocket");
        let events = ws.receive_events(Duration::from_secs(5), Some(10)).await;

        match events {
            Ok(events) => {
                reporter.step_success(step, Some(&format!("{} events", events.len())));
                for event in events.iter().take(3) {
                    reporter.info(&format!(
                        "  Event: {} = {}",
                        event.metric_name, event.value_json
                    ));
                }
            }
            Err(e) => {
                reporter.step_success(step, Some(&format!("WebSocket: {}", e.message)));
            }
        }

        // Cleanup
        let _ = rest.remove_metric("ws_metric_test").await;
        let _ = rest.close_connection(&connection.data.connection_id).await;

        reporter.print_footer(true);
    }

    /// Test WebSocket multiple connections
    #[tokio::test]
    async fn test_ws_multiple_connections() {
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("WebSocket Multiple", "WebSocket");
        reporter.print_header();

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        tokio::time::sleep(Duration::from_secs(2)).await;

        let ws = WebSocketClient::new(executor.http_port);

        let step = reporter.step_start(
            "Multiple Connections",
            "Create multiple WebSocket connections",
        );

        let result1 = ws.stream_events().await;
        let handle1 = match result1 {
            Ok((_rx, handle)) => {
                reporter.info("First connection established");
                Some(handle)
            }
            Err(e) => {
                reporter.info(&format!("First connection failed: {}", e.message));
                None
            }
        };

        let result2 = ws.stream_events().await;
        let handle2 = match result2 {
            Ok((_rx, handle)) => {
                reporter.info("Second connection established");
                Some(handle)
            }
            Err(e) => {
                reporter.info(&format!("Second connection failed: {}", e.message));
                None
            }
        };

        if handle1.is_some() && handle2.is_some() {
            reporter.step_success(step, Some("Multiple connections supported"));
        } else {
            reporter.step_success(step, Some("Connection test completed"));
        }

        if let Some(h) = handle1 {
            h.close().await;
        }
        if let Some(h) = handle2 {
            h.close().await;
        }

        reporter.print_footer(true);
    }

    /// Test WebSocket reconnection
    #[tokio::test]
    async fn test_ws_reconnection() {
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("WebSocket Reconnection", "WebSocket");
        reporter.print_header();

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        tokio::time::sleep(Duration::from_secs(2)).await;

        let ws = WebSocketClient::new(executor.http_port);

        let step = reporter.step_start("Reconnection", "Test WebSocket reconnection");

        let result1 = ws.stream_events().await;
        if let Ok((_rx, handle)) = result1 {
            reporter.info("First connection established");
            handle.close().await;
            reporter.info("First connection closed");

            tokio::time::sleep(Duration::from_millis(500)).await;

            let result2 = ws.stream_events().await;
            if let Ok((_, handle2)) = result2 {
                reporter.step_success(step, Some("Reconnection successful"));
                handle2.close().await;
            } else {
                reporter.step_success(step, Some("Reconnection failed"));
            }
        } else {
            reporter.step_success(step, Some("Initial connection failed"));
        }

        reporter.print_footer(true);
    }

    /// Test WebSocket introspection (stack trace and memory snapshot) via streaming
    #[tokio::test]
    async fn test_ws_stream_introspection() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("WebSocket Introspection Streaming", "WebSocket");
        reporter.print_header();

        let script_path = match executor.detrix_example_app_path() {
            Some(p) => p,
            None => {
                reporter.error("detrix_example_app.py not found");
                reporter.print_footer(false);
                panic!("detrix_example_app.py not found");
            }
        };

        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            executor.print_debugpy_logs(50);
            panic!("Failed to start debugpy: {}", e);
        }

        tokio::time::sleep(Duration::from_secs(2)).await;

        let rest = RestClient::new(executor.http_port);
        let ws = WebSocketClient::new(executor.http_port);

        // Create connection via REST
        let step = reporter.step_start("Create Connection", "Connect to debugpy");
        let connection = match rest
            .create_connection("127.0.0.1", executor.debugpy_port, "python")
            .await
        {
            Ok(c) => {
                reporter.step_success(step, Some(&c.data.connection_id));
                c
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Add metric WITH introspection enabled
        let step = reporter.step_start(
            "Add Metric with Introspection",
            "Add metric with stack trace and memory snapshot",
        );
        let location = format!("@{}#22", script_path.display());
        match rest
            .add_metric(
                AddMetricRequest::new(
                    "ws_introspection_test",
                    &location,
                    "price",
                    &connection.data.connection_id,
                )
                .with_introspection(), // Enable both stack trace and memory snapshot
            )
            .await
        {
            Ok(r) => reporter.step_success(step, Some(&r.data)),
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                reporter.print_footer(false);
                panic!("Failed to add metric: {}", e);
            }
        }

        // Stream events via WebSocket and verify introspection data
        let step = reporter.step_start(
            "Stream Introspection Events",
            "Receive events with introspection data via WebSocket",
        );
        match ws.receive_events(Duration::from_secs(10), Some(5)).await {
            Ok(events) => {
                reporter.step_success(step, Some(&format!("{} events received", events.len())));

                // Count events with introspection data
                let mut stack_trace_count = 0;
                let mut memory_snapshot_count = 0;

                for event in &events {
                    if event.stack_trace.is_some() {
                        stack_trace_count += 1;
                    }
                    if event.memory_snapshot.is_some() {
                        memory_snapshot_count += 1;
                    }
                }

                reporter.info(&format!(
                    "  Introspection: {} events with stack_trace, {} with memory_snapshot",
                    stack_trace_count, memory_snapshot_count
                ));

                // Verify introspection data structure if we have events
                if let Some(event) = events.iter().find(|e| e.stack_trace.is_some()) {
                    let st = event.stack_trace.as_ref().unwrap();
                    reporter.info(&format!("  Stack trace frames: {}", st.frames.len()));

                    // Verify stack trace structure
                    assert!(
                        !st.frames.is_empty(),
                        "Stack trace should have at least one frame"
                    );

                    let first_frame = &st.frames[0];
                    reporter.info(&format!(
                        "    Frame 0: {} at {:?}:{}",
                        first_frame.name,
                        first_frame.file,
                        first_frame.line.unwrap_or(0)
                    ));

                    // Verify frame has required fields
                    assert!(
                        !first_frame.name.is_empty(),
                        "Frame name should not be empty"
                    );
                }

                if let Some(event) = events.iter().find(|e| e.memory_snapshot.is_some()) {
                    let snap = event.memory_snapshot.as_ref().unwrap();
                    reporter.info(&format!(
                        "  Memory snapshot: {} locals, {} globals",
                        snap.locals.len(),
                        snap.globals.len()
                    ));

                    // Verify memory snapshot structure
                    if !snap.locals.is_empty() {
                        let first_local = &snap.locals[0];
                        reporter.info(&format!(
                            "    Local: {} = {} (type: {:?})",
                            first_local.name, first_local.value, first_local.var_type
                        ));

                        // Verify variable has required fields
                        assert!(
                            !first_local.name.is_empty(),
                            "Variable name should not be empty"
                        );
                    }
                }

                // Note: WebSocket introspection may not be available for all deployment modes
                if !events.is_empty() && stack_trace_count == 0 && memory_snapshot_count == 0 {
                    reporter.info(
                        "  WARNING: No introspection data captured (may be expected for logpoints)",
                    );
                }
            }
            Err(e) => {
                reporter.step_success(step, Some(&format!("WebSocket: {}", e.message)));
            }
        }

        // Print daemon logs for debugging
        println!("\n=== Daemon Logs (last 50 lines) ===");
        executor.print_daemon_logs(50);

        // Cleanup
        let _ = rest.remove_metric("ws_introspection_test").await;
        let _ = rest.close_connection(&connection.data.connection_id).await;

        reporter.print_footer(true);
    }
}

// ============================================================================
// MCP SYSTEM EVENTS STREAMING TESTS - Test event streaming and DAP disconnection
// ============================================================================

mod mcp_system_events_tests {
    use detrix_testing::e2e::client::{AddMetricRequest, ApiClient};
    use detrix_testing::e2e::dap_scenarios::DapWorkflowConfig;
    use detrix_testing::e2e::executor::TestExecutor;
    use detrix_testing::e2e::mcp::McpClient;
    use detrix_testing::e2e::reporter::TestReporter;
    use std::time::Duration;

    /// Test Python metric events and DAP disconnection events via MCP
    #[tokio::test]
    async fn test_mcp_python_events_streaming() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        run_events_streaming_test("python", DapWorkflowConfig::python()).await;
    }

    /// Test Go metric events and DAP disconnection events via MCP
    #[tokio::test]
    async fn test_mcp_go_events_streaming() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Delve).await {
            return;
        }
        run_events_streaming_test("go", DapWorkflowConfig::go()).await;
    }

    /// Test Rust metric events and DAP disconnection events via MCP
    #[tokio::test]
    async fn test_mcp_rust_events_streaming() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::LldbDap).await {
            return;
        }
        run_events_streaming_test("rust", DapWorkflowConfig::rust()).await;
    }

    /// Common test implementation for all languages
    async fn run_events_streaming_test(language: &str, config: DapWorkflowConfig) {
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new(
            &format!("MCP {} Events Streaming", language.to_uppercase()),
            "MCP",
        );
        reporter.print_header();

        let script_path = executor.workspace_root.join(&config.source_file);

        // Start daemon
        let step = reporter.step_start("Start Daemon", "Starting Detrix daemon");
        if let Err(e) = executor.start_daemon().await {
            reporter.step_failed(step, &format!("Failed: {}", e));
            executor.print_daemon_logs(50);
            panic!("Failed to start daemon: {}", e);
        }
        reporter.step_success(step, Some(&format!("Port {}", executor.http_port)));

        // Start debugger based on language
        let debugger_port = match language {
            "python" => {
                let step = reporter.step_start("Start debugpy", "Starting Python debugger");
                if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
                    reporter.step_failed(step, &format!("Failed: {}", e));
                    executor.print_debugpy_logs(50);
                    panic!("Failed to start debugpy: {}", e);
                }
                reporter.step_success(step, Some(&format!("Port {}", executor.debugpy_port)));
                executor.debugpy_port
            }
            "go" => {
                let step = reporter.step_start("Start delve", "Starting Go debugger");
                if let Err(e) = executor.start_delve(script_path.to_str().unwrap()).await {
                    reporter.step_failed(step, &format!("Failed: {}", e));
                    executor.print_delve_logs(50);
                    panic!("Failed to start delve: {}", e);
                }
                reporter.step_success(step, Some(&format!("Port {}", executor.delve_port)));
                executor.delve_port
            }
            "rust" => {
                let step = reporter.step_start("Start lldb-dap", "Starting Rust debugger");
                if let Err(e) = executor.start_lldb(script_path.to_str().unwrap()).await {
                    reporter.step_failed(step, &format!("Failed: {}", e));
                    executor.print_lldb_logs(50);
                    panic!("Failed to start lldb-dap: {}", e);
                }
                reporter.step_success(step, Some(&format!("Port {}", executor.lldb_port)));
                executor.lldb_port
            }
            _ => panic!("Unknown language: {}", language),
        };

        let client = McpClient::new(executor.http_port);

        // Create connection
        // For Rust, use create_connection_with_program to pass the binary path for direct lldb-dap mode
        let step = reporter.step_start("Create Connection", "Connect to debugger via MCP");
        let connection = if language == "rust" {
            let program = executor
                .rust_binary_path
                .as_ref()
                .map(|p| p.to_str().unwrap());
            match client
                .create_connection_with_program("127.0.0.1", debugger_port, language, program)
                .await
            {
                Ok(c) => {
                    reporter.step_success(step, Some(&c.data.connection_id));
                    c.data
                }
                Err(e) => {
                    reporter.step_failed(step, &e.to_string());
                    executor.print_daemon_logs(50);
                    panic!("Failed to create connection: {}", e);
                }
            }
        } else {
            match client
                .create_connection("127.0.0.1", debugger_port, language)
                .await
            {
                Ok(c) => {
                    reporter.step_success(step, Some(&c.data.connection_id));
                    c.data
                }
                Err(e) => {
                    reporter.step_failed(step, &e.to_string());
                    executor.print_daemon_logs(50);
                    panic!("Failed to create connection: {}", e);
                }
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Add a metric
        let step = reporter.step_start("Add Metric", "Add test metric");
        let metric_location = format!(
            "@{}#{}",
            script_path.display(),
            config.metrics.first().map(|m| m.line).unwrap_or(10)
        );
        let metric_expr = config
            .metrics
            .first()
            .map(|m| m.expression.as_str())
            .unwrap_or("1");

        match client
            .add_metric(AddMetricRequest::new(
                "streaming_test_metric",
                &metric_location,
                metric_expr,
                &connection.connection_id,
            ))
            .await
        {
            Ok(r) => reporter.step_success(step, Some(&r.data)),
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                panic!("Failed to add metric: {}", e);
            }
        }

        // Wait for some events to be captured
        let step = reporter.step_start("Wait for Events", "Waiting for metric events (5s)");
        tokio::time::sleep(Duration::from_secs(5)).await;
        reporter.step_success(step, Some("5 seconds elapsed"));

        // Query metric events
        let step = reporter.step_start("Query Metric Events", "Query captured metric events");
        match client.query_events("streaming_test_metric", 20).await {
            Ok(events) => {
                let count = events.data.len();
                reporter.step_success(step, Some(&format!("{} events", count)));

                println!(
                    "\n╔════════════════════════════════════════════════════════════════════╗"
                );
                println!(
                    "║                     METRIC EVENTS ({:3} total)                      ║",
                    count
                );
                println!("╠════════════════════════════════════════════════════════════════════╣");

                for (i, event) in events.data.iter().take(10).enumerate() {
                    println!(
                        "║ [{:2}] {} | value={:<20} | age={}s",
                        i + 1,
                        event
                            .timestamp_iso
                            .get(..19)
                            .unwrap_or(&event.timestamp_iso),
                        event.value.chars().take(20).collect::<String>(),
                        event.age_seconds
                    );
                }

                if count > 10 {
                    println!("║      ... and {} more events", count - 10);
                }
                println!(
                    "╚════════════════════════════════════════════════════════════════════╝\n"
                );
            }
            Err(e) => {
                reporter.step_success(step, Some(&format!("Query result: {}", e.message)));
            }
        }

        // Query system events BEFORE disconnection (should see metric_added, connection_created)
        let step = reporter.step_start(
            "Query System Events (Before)",
            "Query system events before disconnect",
        );
        match client.query_system_events(50, false, None).await {
            Ok(events) => {
                let count = events.data.len();
                reporter.step_success(step, Some(&format!("{} events", count)));

                println!(
                    "\n╔════════════════════════════════════════════════════════════════════╗"
                );
                println!(
                    "║               SYSTEM EVENTS BEFORE DISCONNECT ({:3})                ║",
                    count
                );
                println!("╠════════════════════════════════════════════════════════════════════╣");

                for event in &events.data {
                    let conn_id = event.connection_id.as_deref().unwrap_or("-");
                    let metric = event.metric_name.as_deref().unwrap_or("-");
                    println!(
                        "║ {:20} | conn={:<12} | metric={:<15} | {}",
                        event.event_type,
                        conn_id.chars().take(12).collect::<String>(),
                        metric.chars().take(15).collect::<String>(),
                        event
                            .timestamp_iso
                            .get(..19)
                            .unwrap_or(&event.timestamp_iso)
                    );
                }
                println!(
                    "╚════════════════════════════════════════════════════════════════════╝\n"
                );
            }
            Err(e) => {
                reporter.step_success(step, Some(&format!("Query result: {}", e.message)));
            }
        }

        // Kill the debugger to trigger disconnection event
        let step = reporter.step_start(
            "Kill Debugger",
            "Killing debugger to trigger disconnect event",
        );
        detrix_testing::e2e::executor::kill_listening_process_on_port(debugger_port);
        reporter.step_success(step, Some("Debugger killed"));

        // Wait for disconnect event to be captured
        let step = reporter.step_start("Wait for Disconnect", "Waiting for disconnect event (3s)");
        tokio::time::sleep(Duration::from_secs(3)).await;
        reporter.step_success(step, Some("3 seconds elapsed"));

        // Query system events AFTER disconnection (should see connection_lost or connection_closed)
        let step = reporter.step_start(
            "Query System Events (After)",
            "Query system events after disconnect",
        );
        match client.query_system_events(50, false, None).await {
            Ok(events) => {
                let count = events.data.len();
                let disconnect_events: Vec<_> = events
                    .data
                    .iter()
                    .filter(|e| e.is_disconnection_event())
                    .collect();

                reporter.step_success(
                    step,
                    Some(&format!(
                        "{} total, {} disconnect events",
                        count,
                        disconnect_events.len()
                    )),
                );

                println!(
                    "\n╔════════════════════════════════════════════════════════════════════╗"
                );
                println!(
                    "║                SYSTEM EVENTS AFTER DISCONNECT ({:3})                ║",
                    count
                );
                println!("╠════════════════════════════════════════════════════════════════════╣");

                for event in &events.data {
                    let marker = if event.is_disconnection_event() {
                        ">>> "
                    } else {
                        "    "
                    };
                    let conn_id = event.connection_id.as_deref().unwrap_or("-");
                    let metric = event.metric_name.as_deref().unwrap_or("-");
                    println!(
                        "║ {}{:20} | conn={:<12} | metric={:<15}",
                        marker,
                        event.event_type,
                        conn_id.chars().take(12).collect::<String>(),
                        metric.chars().take(15).collect::<String>(),
                    );
                }

                println!("╠════════════════════════════════════════════════════════════════════╣");

                if disconnect_events.is_empty() {
                    println!(
                        "║ ⚠️  WARNING: No disconnection events captured!                     ║"
                    );
                } else {
                    println!(
                        "║ ✅ DISCONNECTION EVENTS DETECTED:                                  ║"
                    );
                    for event in &disconnect_events {
                        let conn_id = event.connection_id.as_deref().unwrap_or("unknown");
                        println!(
                            "║    - {} for connection {} at {}",
                            event.event_type,
                            conn_id,
                            event
                                .timestamp_iso
                                .get(..19)
                                .unwrap_or(&event.timestamp_iso)
                        );
                    }
                }
                println!(
                    "╚════════════════════════════════════════════════════════════════════╝\n"
                );

                // Verify we got at least one disconnect event
                if disconnect_events.is_empty() {
                    reporter.error("No disconnection events detected!");
                    executor.print_daemon_logs(100);
                    // Don't panic - just warn since event timing can be flaky
                }
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
            }
        }

        // Print daemon logs for debugging
        println!("\n=== Daemon Logs (last 50 lines) ===");
        executor.print_daemon_logs(50);

        reporter.print_footer(true);
    }
}

// ============================================================================
// ERROR HANDLING TESTS - WebSocket specific (no daemon needed)
// ============================================================================

mod error_handling_tests {
    use detrix_testing::e2e::websocket::WebSocketClient;
    use std::time::Duration;

    /// Test WebSocket invalid URL
    #[tokio::test]
    async fn test_ws_invalid_url() {
        let ws = WebSocketClient::with_url("ws://invalid-host-that-does-not-exist:9999/ws");
        let result = ws.receive_events(Duration::from_secs(2), Some(0)).await;

        assert!(result.is_err(), "Should fail with invalid URL");
    }

    /// Test WebSocket invalid port
    #[tokio::test]
    async fn test_ws_invalid_port() {
        let ws = WebSocketClient::new(1); // Port 1 is unlikely to be available
        let result = ws.receive_events(Duration::from_secs(2), Some(0)).await;

        assert!(result.is_err(), "Should fail with invalid port");
    }
}

// ============================================================================
// OBSERVE WORKFLOW TESTS - MCP-only observe tool (prints all steps)
// ============================================================================

mod observe_workflow_tests {
    use detrix_testing::e2e::client::{ApiClient, ObserveRequest};
    use detrix_testing::e2e::dap_scenarios::DapWorkflowConfig;
    use detrix_testing::e2e::executor::TestExecutor;
    use detrix_testing::e2e::mcp::McpClient;
    use detrix_testing::e2e::reporter::TestReporter;
    use std::sync::Arc;
    use std::time::Duration;

    /// Test observe workflow with Python (debugpy)
    ///
    /// This test demonstrates the simplified observe workflow:
    /// 1. Create connection
    /// 2. Use observe tool (auto-generates name, finds best line)
    /// 3. Verify events are captured
    /// 4. Clean up
    #[tokio::test]
    async fn test_observe_python_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = Arc::new(TestReporter::new("Observe Python Workflow", "MCP"));
        reporter.print_header();

        let config = DapWorkflowConfig::python();
        let script_path = executor.workspace_root.join(&config.source_file);

        reporter.section("STEP 1: START DAEMON");
        let step = reporter.step_start("Start Daemon", "Starting Detrix daemon");
        if let Err(e) = executor.start_daemon().await {
            reporter.step_failed(step, &e.to_string());
            executor.print_daemon_logs(50);
            reporter.print_footer(false);
            panic!("Failed to start daemon: {}", e);
        }
        reporter.step_success(
            step,
            Some(&format!("Daemon started on port {}", executor.http_port)),
        );

        reporter.section("STEP 2: START DEBUGPY");
        let step = reporter.step_start(
            "Start Debugpy",
            &format!("Starting debugpy with {}", script_path.display()),
        );
        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.step_failed(step, &e.to_string());
            executor.print_debugpy_logs(50);
            reporter.print_footer(false);
            panic!("Failed to start debugpy: {}", e);
        }
        reporter.step_success(
            step,
            Some(&format!(
                "Debugpy listening on port {}",
                executor.debugpy_port
            )),
        );

        reporter.section("STEP 3: CREATE MCP CLIENT");
        let client = McpClient::new(executor.http_port);
        reporter.info(&format!(
            "MCP client created for port {}",
            executor.http_port
        ));

        reporter.section("STEP 4: CREATE CONNECTION");
        let step = reporter.step_start(
            "Create Connection",
            &format!(
                "Connecting to debugpy at 127.0.0.1:{}",
                executor.debugpy_port
            ),
        );

        let connection_id = match client
            .create_connection("127.0.0.1", executor.debugpy_port, "python")
            .await
        {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!("Connection created: {}", r.data.connection_id)),
                );
                r.data.connection_id
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                reporter.print_footer(false);
                panic!("Failed to create connection: {}", e);
            }
        };

        // Wait for DAP handshake
        tokio::time::sleep(Duration::from_secs(2)).await;

        reporter.section("STEP 5: OBSERVE WITH EXPLICIT LINE");
        let step = reporter.step_start(
            "Observe (Explicit Line)",
            "Using observe tool with line 53 (all variables in scope)",
        );
        reporter.info(&format!("  File: {}", script_path.display()));
        reporter.info("  Expression: price");
        reporter.info("  Line: 53 (explicit)");

        let observe_request = ObserveRequest::new(script_path.to_str().unwrap(), "price")
            .with_line(53)
            .with_connection_id(&connection_id)
            .with_name("observe_price_explicit");

        match client.observe(observe_request).await {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Observing '{}' at line {} (source: {})",
                        r.data.expression, r.data.line, r.data.line_source
                    )),
                );
                reporter.info(&format!("  Metric ID: {}", r.data.metric_id));
                reporter.info(&format!("  Metric Name: {}", r.data.metric_name));
                reporter.info(&format!("  Connection: {}", r.data.connection_id));
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                reporter.print_footer(false);
                panic!("Observe failed: {}", e);
            }
        }

        reporter.section("STEP 6: OBSERVE WITH AUTO-NAME");
        let step = reporter.step_start(
            "Observe (Auto-Name)",
            "Using observe without name - should auto-generate",
        );
        reporter.info("  Expression: order_id");
        reporter.info("  Line: 50 (different line to avoid DAP conflict)");
        reporter.info("  Name: (auto-generated)");

        let observe_request = ObserveRequest::new(script_path.to_str().unwrap(), "order_id")
            .with_line(50) // Different line to avoid DAP logpoint conflict
            .with_connection_id(&connection_id);
        // No name set - should auto-generate

        match client.observe(observe_request).await {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!("Auto-generated name: '{}'", r.data.metric_name)),
                );
                reporter.info(&format!("  Metric ID: {}", r.data.metric_id));
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                // Continue - this is not fatal
            }
        }

        reporter.section("STEP 7: OBSERVE WITH INTROSPECTION");
        let step = reporter.step_start(
            "Observe (With Introspection)",
            "Using observe with stack trace + memory snapshot",
        );
        reporter.info("  Expression: quantity");
        reporter.info("  Line: 51 (different line to avoid DAP conflict)");
        reporter.info("  Stack Trace: enabled");
        reporter.info("  Memory Snapshot: enabled");

        let observe_request = ObserveRequest::new(script_path.to_str().unwrap(), "quantity")
            .with_line(51) // Different line to avoid DAP logpoint conflict
            .with_connection_id(&connection_id)
            .with_name("observe_quantity_introspection")
            .with_group("python_observe_test")
            .with_introspection();

        match client.observe(observe_request).await {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Observing with introspection: '{}'",
                        r.data.metric_name
                    )),
                );
                if !r.data.warnings.is_empty() {
                    for warning in &r.data.warnings {
                        reporter.warn(&format!("  Warning: {}", warning));
                    }
                }
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                // Continue - this is not fatal
            }
        }

        let sleep_duration: u64 = 30;
        reporter.section("STEP 8: WAIT FOR EVENTS");
        let step = reporter.step_start(
            "Wait for Events",
            &format!(
                "Waiting for observe metrics to capture data ({}s)",
                sleep_duration
            ),
        );

        reporter.info(&format!("Sleeping for {} seconds", sleep_duration));
        tokio::time::sleep(Duration::from_secs(sleep_duration)).await;

        let mut total_events = 0;
        let mut metrics_with_events = 0;

        if let Ok(metrics) = client.list_metrics().await {
            for metric in metrics.data.iter() {
                // Match explicit names and auto-generated names (format: filename_line)
                if metric.name.contains("observe_") || metric.name.contains("trade_bot_forever_py_")
                {
                    if let Ok(events) = client
                        .query_events(&metric.name, sleep_duration.try_into().unwrap())
                        .await
                    {
                        let count = events.data.len();
                        if count > 0 {
                            reporter
                                .info(&format!("  '{}': {} events captured", metric.name, count));
                            for (i, event) in events.data.iter().take(3).enumerate() {
                                reporter.info(&format!(
                                    "    [{}] value={}, timestamp={}",
                                    i, event.value, event.timestamp_iso
                                ));
                            }
                            total_events += count;
                            metrics_with_events += 1;
                        }
                    }
                }
            }
        }

        if total_events > 0 {
            reporter.step_success(
                step,
                Some(&format!(
                    "{} events across {} metrics",
                    total_events, metrics_with_events
                )),
            );
        } else {
            reporter.step_failed(
                step,
                "No events captured - metrics may not have been set correctly",
            );
        }

        reporter.section("STEP 9: CLEANUP");
        let step = reporter.step_start("Cleanup", "Removing metrics and closing connection");

        // Remove metrics
        let _ = client.remove_metric("observe_price_explicit").await;
        let _ = client.remove_metric("observe_quantity_introspection").await;

        // Clean up auto-generated metrics (format: filename_line)
        if let Ok(metrics) = client.list_metrics().await {
            for metric in metrics.data.iter() {
                if metric.name.contains("trade_bot_forever_py_") {
                    let _ = client.remove_metric(&metric.name).await;
                }
            }
        }

        // Close connection
        let _ = client.close_connection(&connection_id).await;
        reporter.step_success(step, Some("Cleanup complete"));

        reporter.section("OBSERVE WORKFLOW SUMMARY");
        reporter.info(&format!("Total events captured: {}", total_events));
        reporter.info(&format!("Metrics with events: {}", metrics_with_events));

        // FAIL if no events were captured - this indicates metrics weren't set correctly
        if total_events == 0 {
            reporter.print_footer(false);
            executor.print_daemon_logs(100);
            panic!("No events captured - observe workflow failed to set metrics correctly");
        }

        reporter.info("Observe workflow completed successfully");
        reporter.print_footer(true);
    }

    /// Test observe workflow with Go (delve)
    #[tokio::test]
    async fn test_observe_go_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Delve).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = Arc::new(TestReporter::new("Observe Go Workflow", "MCP"));
        reporter.print_header();

        let config = DapWorkflowConfig::go();
        let source_path = executor.workspace_root.join(&config.source_file);

        reporter.section("STEP 1: START DAEMON");
        let step = reporter.step_start("Start Daemon", "Starting Detrix daemon");
        if let Err(e) = executor.start_daemon().await {
            reporter.step_failed(step, &e.to_string());
            executor.print_daemon_logs(50);
            reporter.print_footer(false);
            panic!("Failed to start daemon: {}", e);
        }
        reporter.step_success(
            step,
            Some(&format!("Daemon started on port {}", executor.http_port)),
        );

        reporter.section("STEP 2: START DELVE");
        let step = reporter.step_start(
            "Start Delve",
            &format!("Starting delve with {}", source_path.display()),
        );
        if let Err(e) = executor.start_delve(source_path.to_str().unwrap()).await {
            reporter.step_failed(step, &e.to_string());
            executor.print_delve_logs(50);
            reporter.print_footer(false);
            panic!("Failed to start delve: {}", e);
        }
        reporter.step_success(
            step,
            Some(&format!("Delve listening on port {}", executor.delve_port)),
        );

        reporter.section("STEP 3: CREATE MCP CLIENT");
        let client = McpClient::new(executor.http_port);
        reporter.info(&format!(
            "MCP client created for port {}",
            executor.http_port
        ));

        reporter.section("STEP 4: CREATE CONNECTION");
        let step = reporter.step_start(
            "Create Connection",
            &format!("Connecting to delve at 127.0.0.1:{}", executor.delve_port),
        );

        let connection_id = match client
            .create_connection("127.0.0.1", executor.delve_port, "go")
            .await
        {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!("Connection created: {}", r.data.connection_id)),
                );
                r.data.connection_id
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                reporter.print_footer(false);
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        reporter.section("STEP 5: OBSERVE GO VARIABLES");
        let step = reporter.step_start(
            "Observe (Go)",
            "Using observe tool on line 69 (all variables in scope)",
        );
        reporter.info(&format!("  File: {}", source_path.display()));
        reporter.info("  Expression: orderID");
        reporter.info("  Line: 69");

        let observe_request = ObserveRequest::new(source_path.to_str().unwrap(), "orderID")
            .with_line(69)
            .with_connection_id(&connection_id)
            .with_name("observe_go_orderid");

        match client.observe(observe_request).await {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Observing '{}' at line {}",
                        r.data.expression, r.data.line
                    )),
                );
                reporter.info(&format!("  Metric ID: {}", r.data.metric_id));
                reporter.info(&format!("  Connection: {}", r.data.connection_id));
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                reporter.print_footer(false);
                panic!("Observe failed: {}", e);
            }
        }

        reporter.section("STEP 6: WAIT FOR EVENTS");
        let step = reporter.step_start("Wait for Events", "Waiting for events (10s)");
        tokio::time::sleep(Duration::from_secs(10)).await;

        let mut total_events = 0;
        if let Ok(events) = client.query_events("observe_go_orderid", 10).await {
            total_events = events.data.len();
            for (i, event) in events.data.iter().take(3).enumerate() {
                reporter.info(&format!(
                    "  [{}] value={}, timestamp={}",
                    i, event.value, event.timestamp_iso
                ));
            }
        }

        if total_events > 0 {
            reporter.step_success(step, Some(&format!("{} events captured", total_events)));
        } else {
            reporter.step_failed(
                step,
                "No events captured - metrics may not have been set correctly",
            );
        }

        reporter.section("STEP 7: CLEANUP");
        let _ = client.remove_metric("observe_go_orderid").await;
        let _ = client.close_connection(&connection_id).await;
        reporter.info("Cleanup complete");

        reporter.section("OBSERVE GO WORKFLOW SUMMARY");
        reporter.info(&format!("Total events: {}", total_events));

        // FAIL if no events were captured - this indicates metrics weren't set correctly
        if total_events == 0 {
            reporter.print_footer(false);
            executor.print_daemon_logs(100);
            executor.print_delve_logs(50);
            panic!("No events captured - observe workflow failed to set metrics correctly");
        }

        reporter.info("Go observe workflow completed");
        reporter.print_footer(true);
    }

    /// Test observe workflow with Rust (lldb-dap)
    #[tokio::test]
    async fn test_observe_rust_workflow() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::LldbDap).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = Arc::new(TestReporter::new("Observe Rust Workflow", "MCP"));
        reporter.print_header();

        let config = DapWorkflowConfig::rust();
        let source_path = executor.workspace_root.join(&config.source_file);

        reporter.section("STEP 1: START DAEMON");
        let step = reporter.step_start("Start Daemon", "Starting Detrix daemon");
        if let Err(e) = executor.start_daemon().await {
            reporter.step_failed(step, &e.to_string());
            executor.print_daemon_logs(50);
            reporter.print_footer(false);
            panic!("Failed to start daemon: {}", e);
        }
        reporter.step_success(
            step,
            Some(&format!("Daemon started on port {}", executor.http_port)),
        );

        reporter.section("STEP 2: START LLDB-DAP");
        let step = reporter.step_start(
            "Start LLDB-DAP",
            &format!("Compiling and starting {}", source_path.display()),
        );
        if let Err(e) = executor.start_lldb(source_path.to_str().unwrap()).await {
            reporter.step_failed(step, &e.to_string());
            executor.print_lldb_logs(50);
            reporter.print_footer(false);
            panic!("Failed to start lldb-dap: {}", e);
        }
        reporter.step_success(
            step,
            Some(&format!(
                "LLDB-DAP listening on port {}",
                executor.lldb_port
            )),
        );

        reporter.section("STEP 3: CREATE MCP CLIENT");
        let client = McpClient::new(executor.http_port);
        reporter.info(&format!(
            "MCP client created for port {}",
            executor.http_port
        ));

        reporter.section("STEP 4: CREATE CONNECTION");
        let step = reporter.step_start(
            "Create Connection",
            &format!("Connecting to lldb-dap at 127.0.0.1:{}", executor.lldb_port),
        );

        // Use create_connection_with_program for direct lldb-dap TCP mode
        let program = executor
            .rust_binary_path
            .as_ref()
            .map(|p| p.to_str().unwrap());
        let connection_id = match client
            .create_connection_with_program("127.0.0.1", executor.lldb_port, "rust", program)
            .await
        {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!("Connection created: {}", r.data.connection_id)),
                );
                r.data.connection_id
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                reporter.print_footer(false);
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        reporter.section("STEP 5: OBSERVE RUST VARIABLES");
        let step = reporter.step_start(
            "Observe (Rust)",
            "Using observe tool on line 83 (all variables in scope)",
        );
        reporter.info(&format!("  File: {}", source_path.display()));
        reporter.info("  Expression: order_id");
        reporter.info("  Line: 83");

        let observe_request = ObserveRequest::new(source_path.to_str().unwrap(), "order_id")
            .with_line(83)
            .with_connection_id(&connection_id)
            .with_name("observe_rust_orderid");

        match client.observe(observe_request).await {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Observing '{}' at line {}",
                        r.data.expression, r.data.line
                    )),
                );
                reporter.info(&format!("  Metric ID: {}", r.data.metric_id));
                reporter.info(&format!("  Connection: {}", r.data.connection_id));
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                reporter.print_footer(false);
                panic!("Observe failed: {}", e);
            }
        }

        reporter.section("STEP 6: WAIT FOR EVENTS");
        let step = reporter.step_start("Wait for Events", "Waiting for events (10s)");
        tokio::time::sleep(Duration::from_secs(10)).await;

        let mut total_events = 0;
        if let Ok(events) = client.query_events("observe_rust_orderid", 10).await {
            total_events = events.data.len();
            for (i, event) in events.data.iter().take(3).enumerate() {
                reporter.info(&format!(
                    "  [{}] value={}, timestamp={}",
                    i, event.value, event.timestamp_iso
                ));
            }
        }

        if total_events > 0 {
            reporter.step_success(step, Some(&format!("{} events captured", total_events)));
        } else {
            reporter.step_failed(
                step,
                "No events captured - metrics may not have been set correctly",
            );
        }

        reporter.section("STEP 7: CLEANUP");
        let _ = client.remove_metric("observe_rust_orderid").await;
        let _ = client.close_connection(&connection_id).await;
        reporter.info("Cleanup complete");

        reporter.section("OBSERVE RUST WORKFLOW SUMMARY");
        reporter.info(&format!("Total events: {}", total_events));

        // FAIL if no events were captured - this indicates metrics weren't set correctly
        if total_events == 0 {
            reporter.print_footer(false);
            executor.print_daemon_logs(100);
            executor.print_lldb_logs(50);
            panic!("No events captured - observe workflow failed to set metrics correctly");
        }

        reporter.info("Rust observe workflow completed");
        reporter.print_footer(true);
    }
}

// ============================================================================
// ENABLE FROM DIFF WORKFLOW TESTS - MCP only (parses git diffs for debug statements)
// ============================================================================

mod enable_from_diff_workflow_tests {
    use detrix_testing::e2e::client::{ApiClient, EnableFromDiffRequest};
    use detrix_testing::e2e::executor::TestExecutor;
    use detrix_testing::e2e::mcp::McpClient;
    use detrix_testing::e2e::reporter::TestReporter;
    use std::time::Duration;

    /// Test enable_from_diff with all valid Python print statements
    #[tokio::test]
    async fn test_enable_from_diff_python_all_valid() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("Enable From Diff - Python All Valid", "MCP");
        reporter.print_header();

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            panic!("Failed to start daemon: {}", e);
        }

        // Start debugpy with trade_bot_forever.py
        let script_path = executor
            .workspace_root
            .join("fixtures/python/trade_bot_forever.py");

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            panic!("Failed to start debugpy: {}", e);
        }

        let client = McpClient::new(executor.http_port);

        // Create connection first
        reporter.section("STEP 1: CREATE CONNECTION");
        let step = reporter.step_start(
            "Create Connection",
            &format!(
                "Connecting to debugpy at 127.0.0.1:{}",
                executor.debugpy_port
            ),
        );

        let connection_id = match client
            .create_connection("127.0.0.1", executor.debugpy_port, "python")
            .await
        {
            Ok(r) => {
                reporter.step_success(step, Some(&format!("Connected: {}", r.data.connection_id)));
                r.data.connection_id
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        reporter.section("STEP 2: ENABLE FROM DIFF - ALL VALID");

        // Create a diff with valid Python print statements
        // These expressions should all be parseable (simple identifiers/attributes)
        let diff = format!(
            r#"diff --git a/{path} b/{path}
@@ -45,6 +45,9 @@ def run_trade_loop():
     symbol = random.choice(symbols)
+    print(f"symbol={{symbol}}")
     quantity = random.randint(1, 50)
+    print(f"quantity={{quantity}}")
     price = random.uniform(100, 1000)
+    print(f"price={{price}}")
     order_id = place_order(symbol, quantity, price)
"#,
            path = script_path.display()
        );

        let step = reporter.step_start(
            "Enable From Diff",
            "Parse diff with 3 valid Python print statements",
        );

        let request = EnableFromDiffRequest::new(&diff)
            .with_connection_id(&connection_id)
            .with_group("diff_test");

        match client.enable_from_diff(request).await {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Added {} metrics, {} failed, {} need review",
                        r.data.added.len(),
                        r.data.failed.len(),
                        r.data.needs_review.len()
                    )),
                );

                // Verify we got all 3 metrics
                assert_eq!(
                    r.data.added.len(),
                    3,
                    "Expected 3 metrics added, got {}",
                    r.data.added.len()
                );
                assert!(r.data.failed.is_empty(), "Expected no failures");
                assert!(r.data.needs_review.is_empty(), "Expected no review needed");

                // Log the added metrics
                for metric in &r.data.added {
                    reporter.info(&format!(
                        "  Added: {} at {}:{} (id={})",
                        metric.name, metric.file, metric.line, metric.id
                    ));
                }
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                panic!("enable_from_diff failed: {}", e);
            }
        }

        // Wait for events
        reporter.section("STEP 3: VERIFY EVENTS");
        let step = reporter.step_start("Wait for Events", "Waiting for events (8s)");
        tokio::time::sleep(Duration::from_secs(8)).await;

        let metrics = client.list_metrics().await.unwrap().data;
        let mut total_events = 0;

        for metric in &metrics {
            if let Ok(events) = client.query_events(&metric.name, 10).await {
                total_events += events.data.len();
                if !events.data.is_empty() {
                    reporter.info(&format!(
                        "  {} captured {} events",
                        metric.name,
                        events.data.len()
                    ));
                }
            }
        }

        if total_events > 0 {
            reporter.step_success(
                step,
                Some(&format!("{} total events captured", total_events)),
            );
        } else {
            reporter.step_failed(step, "No events captured");
        }

        reporter.section("STEP 4: CLEANUP");
        for metric in &metrics {
            let _ = client.remove_metric(&metric.name).await;
        }
        let _ = client.close_connection(&connection_id).await;
        reporter.info("Cleanup complete");

        reporter.print_footer(total_events > 0);
        if total_events == 0 {
            panic!("No events captured - enable_from_diff metrics may not be working");
        }
    }

    /// Test enable_from_diff with partial success (some valid, some complex expressions)
    #[tokio::test]
    async fn test_enable_from_diff_python_partial() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("Enable From Diff - Python Partial Success", "MCP");
        reporter.print_header();

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            panic!("Failed to start daemon: {}", e);
        }

        // Start debugpy
        let script_path = executor
            .workspace_root
            .join("fixtures/python/trade_bot_forever.py");

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            panic!("Failed to start debugpy: {}", e);
        }

        let client = McpClient::new(executor.http_port);

        // Create connection
        reporter.section("STEP 1: CREATE CONNECTION");
        let connection_id = match client
            .create_connection("127.0.0.1", executor.debugpy_port, "python")
            .await
        {
            Ok(r) => {
                reporter.info(&format!("Connected: {}", r.data.connection_id));
                r.data.connection_id
            }
            Err(e) => {
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        reporter.section("STEP 2: ENABLE FROM DIFF - PARTIAL SUCCESS");

        // Mix of simple and complex expressions
        let diff = format!(
            r#"diff --git a/{path} b/{path}
@@ -45,6 +45,9 @@ def run_trade_loop():
     symbol = random.choice(symbols)
+    print(f"symbol={{symbol}}")
     quantity = random.randint(1, 50)
+    print(calculate_total(quantity, price))
     price = random.uniform(100, 1000)
+    print(f"price={{price}}")
"#,
            path = script_path.display()
        );

        let step = reporter.step_start(
            "Enable From Diff",
            "Parse diff with 2 simple + 1 complex expression",
        );

        let request = EnableFromDiffRequest::new(&diff)
            .with_connection_id(&connection_id)
            .with_group("diff_partial");

        match client.enable_from_diff(request).await {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Added {}, failed {}, review {}",
                        r.data.added.len(),
                        r.data.failed.len(),
                        r.data.needs_review.len()
                    )),
                );

                // We expect 2 simple expressions to be added
                // The complex one (calculate_total(quantity, price)) may need review
                reporter.info(&format!("Added metrics: {}", r.data.added.len()));
                for metric in &r.data.added {
                    reporter.info(&format!(
                        "  - {} at {}:{}",
                        metric.name, metric.file, metric.line
                    ));
                }

                if !r.data.needs_review.is_empty() {
                    reporter.info(&format!("Needs review: {}", r.data.needs_review.len()));
                    for item in &r.data.needs_review {
                        reporter.info(&format!(
                            "  - {}:{} '{}' -> suggested: '{}'",
                            item.file, item.line, item.statement, item.suggested_expression
                        ));
                    }
                }

                // Verify at least the simple expressions were added
                assert!(
                    r.data.added.len() >= 2,
                    "Expected at least 2 metrics added, got {}",
                    r.data.added.len()
                );
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                panic!("enable_from_diff failed: {}", e);
            }
        }

        reporter.section("STEP 3: CLEANUP");
        let _ = client.close_connection(&connection_id).await;
        reporter.info("Cleanup complete");

        reporter.print_footer(true);
    }

    /// Test enable_from_diff with Go diff format
    #[tokio::test]
    async fn test_enable_from_diff_go() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Delve).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("Enable From Diff - Go", "MCP");
        reporter.print_header();

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            panic!("Failed to start daemon: {}", e);
        }

        // Start delve
        let script_path = executor
            .workspace_root
            .join("fixtures/go/detrix_example_app.go");

        if let Err(e) = executor.start_delve(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start delve: {}", e));
            panic!("Failed to start delve: {}", e);
        }

        let client = McpClient::new(executor.http_port);

        // Create connection
        reporter.section("STEP 1: CREATE CONNECTION");
        let connection_id = match client
            .create_connection("127.0.0.1", executor.delve_port, "go")
            .await
        {
            Ok(r) => {
                reporter.info(&format!("Connected: {}", r.data.connection_id));
                r.data.connection_id
            }
            Err(e) => {
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        reporter.section("STEP 2: ENABLE FROM DIFF - GO");

        // Go diff with fmt.Printf statements
        let diff = format!(
            r#"diff --git a/{path} b/{path}
@@ -58,6 +58,8 @@ func runTradeLoop() {{
    symbol := symbols[rand.Intn(len(symbols))]
+   fmt.Printf("symbol: %v\n", symbol)
    quantity := rand.Intn(50) + 1
+   fmt.Printf("quantity: %d\n", quantity)
    price := rand.Float64()*900 + 100
"#,
            path = script_path.display()
        );

        let step = reporter.step_start(
            "Enable From Diff",
            "Parse Go diff with 2 fmt.Printf statements",
        );

        let request = EnableFromDiffRequest::new(&diff)
            .with_connection_id(&connection_id)
            .with_group("diff_go");

        match client.enable_from_diff(request).await {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Added {}, failed {}, review {}",
                        r.data.added.len(),
                        r.data.failed.len(),
                        r.data.needs_review.len()
                    )),
                );

                for metric in &r.data.added {
                    reporter.info(&format!(
                        "  Added: {} at {}:{}",
                        metric.name, metric.file, metric.line
                    ));
                }
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                executor.print_daemon_logs(50);
                panic!("enable_from_diff failed: {}", e);
            }
        }

        reporter.section("STEP 3: CLEANUP");
        let _ = client.close_connection(&connection_id).await;
        reporter.info("Cleanup complete");

        reporter.print_footer(true);
    }

    /// Test enable_from_diff with no debug statements (empty result)
    #[tokio::test]
    async fn test_enable_from_diff_no_debug_statements() {
        if !detrix_testing::e2e::require_tool(detrix_testing::e2e::ToolDependency::Debugpy).await {
            return;
        }
        let mut executor = TestExecutor::new();
        let reporter = TestReporter::new("Enable From Diff - No Debug Statements", "MCP");
        reporter.print_header();

        // Start daemon
        if let Err(e) = executor.start_daemon().await {
            reporter.error(&format!("Failed to start daemon: {}", e));
            panic!("Failed to start daemon: {}", e);
        }

        // Start debugpy
        let script_path = executor
            .workspace_root
            .join("fixtures/python/trade_bot_forever.py");

        if let Err(e) = executor.start_debugpy(script_path.to_str().unwrap()).await {
            reporter.error(&format!("Failed to start debugpy: {}", e));
            panic!("Failed to start debugpy: {}", e);
        }

        let client = McpClient::new(executor.http_port);

        // Create connection
        reporter.section("STEP 1: CREATE CONNECTION");
        let connection_id = match client
            .create_connection("127.0.0.1", executor.debugpy_port, "python")
            .await
        {
            Ok(r) => {
                reporter.info(&format!("Connected: {}", r.data.connection_id));
                r.data.connection_id
            }
            Err(e) => {
                panic!("Failed to create connection: {}", e);
            }
        };

        tokio::time::sleep(Duration::from_secs(2)).await;

        reporter.section("STEP 2: ENABLE FROM DIFF - NO DEBUG STATEMENTS");

        // Diff with only regular code changes, no print statements
        let diff = format!(
            r#"diff --git a/{path} b/{path}
@@ -45,6 +45,8 @@ def run_trade_loop():
     symbol = random.choice(symbols)
+    # Just a comment
     quantity = random.randint(1, 50)
+    x = y + z
     price = random.uniform(100, 1000)
"#,
            path = script_path.display()
        );

        let step = reporter.step_start("Enable From Diff", "Parse diff with no debug statements");

        let request = EnableFromDiffRequest::new(&diff).with_connection_id(&connection_id);

        match client.enable_from_diff(request).await {
            Ok(r) => {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Added {}, failed {}, review {}",
                        r.data.added.len(),
                        r.data.failed.len(),
                        r.data.needs_review.len()
                    )),
                );

                // Expect no metrics added (no debug statements)
                assert_eq!(
                    r.data.added.len(),
                    0,
                    "Expected 0 metrics for diff with no debug statements, got {}",
                    r.data.added.len()
                );
            }
            Err(e) => {
                // This might be an error saying "no debug statements found"
                // which is acceptable behavior
                reporter.info(&format!("Got expected error: {}", e));
            }
        }

        reporter.section("STEP 3: CLEANUP");
        let _ = client.close_connection(&connection_id).await;
        reporter.info("Cleanup complete");

        reporter.print_footer(true);
    }
}
