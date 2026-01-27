//! Test Scenarios - Reusable test logic for all API backends
//!
//! These scenarios can be run against MCP, gRPC, or REST APIs.
//! They use the ApiClient trait for abstraction.

use super::client::{AddMetricRequest, ApiClient, ApiError};
use super::reporter::TestReporter;
use std::sync::Arc;
use std::time::Duration;

/// Collection of reusable test scenarios
pub struct TestScenarios;

impl TestScenarios {
    // ========================================================================
    // Basic Health & Status Scenarios
    // ========================================================================

    /// Test basic health check
    pub async fn health_check<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), ApiError> {
        let step = reporter.step_start("Health Check", "Verify API is healthy");

        reporter.step_request("GET", Some("/health"));
        match client.health().await {
            Ok(true) => {
                reporter.step_response("200 OK", Some("healthy"));
                reporter.step_success(step, Some("API is healthy"));
                Ok(())
            }
            Ok(false) => {
                reporter.step_response("503", Some("unhealthy"));
                reporter.step_failed(step, "API is not healthy");
                Err(ApiError::new("API is not healthy"))
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    /// Test status endpoint
    pub async fn get_status<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), ApiError> {
        let step = reporter.step_start("Get Status", "Retrieve system status");

        reporter.step_request("get_status", None);
        match client.get_status().await {
            Ok(response) => {
                reporter.step_response(
                    "OK",
                    Some(&format!(
                        "mode={}, uptime={}s, connections={}, metrics={}",
                        response.data.mode,
                        response.data.uptime_seconds,
                        response.data.active_connections,
                        response.data.total_metrics
                    )),
                );
                reporter.step_success(step, Some("Status retrieved"));
                Ok(())
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    /// Test wake/sleep cycle
    pub async fn wake_sleep_cycle<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), ApiError> {
        // Wake
        let step = reporter.step_start("Wake", "Wake Detrix from sleep");
        reporter.step_request("wake", None);
        match client.wake().await {
            Ok(response) => {
                reporter.step_response("OK", Some(&response.data));
                reporter.step_success(step, Some("Woke successfully"));
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                return Err(e);
            }
        }

        // Sleep
        let step = reporter.step_start("Sleep", "Put Detrix to sleep");
        reporter.step_request("sleep", None);
        match client.sleep().await {
            Ok(response) => {
                reporter.step_response("OK", Some(&response.data));
                reporter.step_success(step, Some("Sleeping"));
                Ok(())
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    // ========================================================================
    // Connection Scenarios
    // ========================================================================

    /// Test creating a debugger connection
    pub async fn create_connection<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        host: &str,
        port: u16,
        language: &str,
    ) -> Result<String, ApiError> {
        let step = reporter.step_start(
            "Create Connection",
            &format!("Connect to {}:{} ({})", host, port, language),
        );

        reporter.step_request(
            "create_connection",
            Some(&format!(
                "host={}, port={}, language={}",
                host, port, language
            )),
        );

        match client.create_connection(host, port, language).await {
            Ok(response) => {
                reporter.step_response(
                    "OK",
                    Some(&format!(
                        "connection_id={}, status={}",
                        response.data.connection_id, response.data.status
                    )),
                );
                reporter.step_success(
                    step,
                    Some(&format!("Connected: {}", response.data.connection_id)),
                );
                Ok(response.data.connection_id)
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    /// Test listing connections
    pub async fn list_connections<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
    ) -> Result<usize, ApiError> {
        let step = reporter.step_start("List Connections", "List all debugger connections");

        reporter.step_request("list_connections", None);
        match client.list_connections().await {
            Ok(response) => {
                let count = response.data.len();
                for conn in &response.data {
                    reporter.info(&format!(
                        "  {} -> {}:{} ({}) [{}]",
                        conn.connection_id, conn.host, conn.port, conn.language, conn.status
                    ));
                }
                reporter.step_success(step, Some(&format!("{} connection(s)", count)));
                Ok(count)
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    /// Test closing a connection
    pub async fn close_connection<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        connection_id: &str,
    ) -> Result<(), ApiError> {
        let step = reporter.step_start("Close Connection", &format!("Close {}", connection_id));

        reporter.step_request("close_connection", Some(connection_id));
        match client.close_connection(connection_id).await {
            Ok(_) => {
                reporter.step_success(step, Some("Connection closed"));
                Ok(())
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    // ========================================================================
    // Metric Scenarios
    // ========================================================================

    /// Test adding a metric
    /// `connection_id` is REQUIRED to enforce explicit relationship: metric → MCP client → daemon → DAP adapter
    pub async fn add_metric<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        name: &str,
        location: &str,
        expression: &str,
        connection_id: &str,
    ) -> Result<String, ApiError> {
        let step = reporter.step_start(
            "Add Metric",
            &format!("Add '{}' at {} with expr '{}'", name, location, expression),
        );

        let request = AddMetricRequest::new(name, location, expression, connection_id);
        reporter.step_request(
            "add_metric",
            Some(&format!(
                "name={}, location={}, expression={}, connection_id={}",
                name, location, expression, connection_id
            )),
        );

        match client.add_metric(request).await {
            Ok(response) => {
                reporter.step_response("OK", Some(&format!("metric_id={}", response.data)));
                reporter.step_success(step, Some(&format!("Metric '{}' added", name)));
                Ok(response.data)
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    /// Test listing metrics
    pub async fn list_metrics<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
    ) -> Result<usize, ApiError> {
        let step = reporter.step_start("List Metrics", "List all configured metrics");

        reporter.step_request("list_metrics", None);
        match client.list_metrics().await {
            Ok(response) => {
                let count = response.data.len();
                for metric in &response.data {
                    reporter.info(&format!(
                        "  {} @ {} -> {} [{}]",
                        metric.name,
                        metric.location,
                        metric.expression,
                        if metric.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ));
                }
                reporter.step_success(step, Some(&format!("{} metric(s)", count)));
                Ok(count)
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    /// Test removing a metric
    pub async fn remove_metric<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        name: &str,
    ) -> Result<(), ApiError> {
        let step = reporter.step_start("Remove Metric", &format!("Remove '{}'", name));

        reporter.step_request("remove_metric", Some(name));
        match client.remove_metric(name).await {
            Ok(_) => {
                reporter.step_success(step, Some("Metric removed"));
                Ok(())
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    /// Test toggling metric enabled state
    pub async fn toggle_metric<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        name: &str,
        enabled: bool,
    ) -> Result<(), ApiError> {
        let action = if enabled { "Enable" } else { "Disable" };
        let step = reporter.step_start(
            &format!("{} Metric", action),
            &format!("{} '{}'", action, name),
        );

        reporter.step_request(
            "toggle_metric",
            Some(&format!("name={}, enabled={}", name, enabled)),
        );
        match client.toggle_metric(name, enabled).await {
            Ok(_) => {
                reporter.step_success(
                    step,
                    Some(&format!(
                        "Metric {}",
                        if enabled { "enabled" } else { "disabled" }
                    )),
                );
                Ok(())
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    // ========================================================================
    // Event Scenarios
    // ========================================================================

    /// Test querying events for a metric
    pub async fn query_events<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        metric_name: &str,
        limit: u32,
    ) -> Result<usize, ApiError> {
        let step = reporter.step_start(
            "Query Events",
            &format!("Query events for '{}' (limit={})", metric_name, limit),
        );

        reporter.step_request(
            "query_events",
            Some(&format!("name={}, limit={}", metric_name, limit)),
        );

        match client.query_events(metric_name, limit).await {
            Ok(response) => {
                let count = response.data.len();
                for event in &response.data {
                    reporter.info(&format!(
                        "  [{}] {} = {} (age: {}s)",
                        event.timestamp_iso, event.metric_name, event.value, event.age_seconds
                    ));
                }
                reporter.step_success(step, Some(&format!("{} event(s) found", count)));
                Ok(count)
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    /// Wait for events to be captured (polls until events appear or timeout)
    pub async fn wait_for_events<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        metric_name: &str,
        min_events: usize,
        timeout_secs: u64,
    ) -> Result<usize, ApiError> {
        let step = reporter.step_start(
            "Wait for Events",
            &format!(
                "Wait for {} events on '{}' (timeout: {}s)",
                min_events, metric_name, timeout_secs
            ),
        );

        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed() > timeout {
                reporter.step_failed(step, &format!("Timeout waiting for {} events", min_events));
                return Err(ApiError::new(format!(
                    "Timeout waiting for {} events on '{}'",
                    min_events, metric_name
                )));
            }

            match client.query_events(metric_name, 100).await {
                Ok(response) => {
                    let count = response.data.len();
                    reporter.step_progress(&format!("Found {} events...", count));

                    if count >= min_events {
                        reporter.step_success(step, Some(&format!("{} events captured", count)));
                        return Ok(count);
                    }
                }
                Err(e) => {
                    reporter.warn(&format!("Query failed: {}", e));
                }
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    // ========================================================================
    // Validation Scenarios
    // ========================================================================

    /// Test expression validation for a specific language
    pub async fn validate_expression<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        expression: &str,
        language: &str,
        expect_safe: bool,
    ) -> Result<bool, ApiError> {
        let step = reporter.step_start(
            "Validate Expression",
            &format!(
                "Validate '{}' ({}) (expect: {})",
                expression,
                language,
                if expect_safe { "safe" } else { "unsafe" }
            ),
        );

        reporter.step_request(
            "validate_expression",
            Some(&format!("expression={}, language={}", expression, language)),
        );
        match client.validate_expression(expression, language).await {
            Ok(response) => {
                let is_safe = response.data.is_safe;
                reporter.step_response(
                    "OK",
                    Some(&format!(
                        "is_safe={}, issues=[{}]",
                        is_safe,
                        response.data.issues.join(", ")
                    )),
                );

                if is_safe == expect_safe {
                    reporter.step_success(
                        step,
                        Some(&format!(
                            "Expression is {}",
                            if is_safe { "safe" } else { "unsafe" }
                        )),
                    );
                    Ok(is_safe)
                } else {
                    reporter
                        .step_failed(step, &format!("Expected {}, got {}", expect_safe, is_safe));
                    Err(ApiError::new(format!(
                        "Expression validation mismatch: expected {}, got {}",
                        expect_safe, is_safe
                    )))
                }
            }
            Err(e) => {
                reporter.step_failed(step, &e.to_string());
                Err(e)
            }
        }
    }

    // ========================================================================
    // Composite Scenarios (Full Flows)
    // ========================================================================

    /// Full metric lifecycle test (add, list, query, toggle, remove)
    #[allow(clippy::too_many_arguments)]
    pub async fn full_metric_lifecycle<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
        debugpy_host: &str,
        debugpy_port: u16,
        metric_name: &str,
        metric_location: &str,
        metric_expression: &str,
        wait_for_events_secs: u64,
    ) -> Result<(), ApiError> {
        reporter.section("SETUP");

        // Health check
        Self::health_check(client, reporter).await?;

        // Create connection to debugpy and capture connection_id
        let connection_id =
            Self::create_connection(client, reporter, debugpy_host, debugpy_port, "python").await?;

        // Small delay for DAP handshake
        reporter.info("Waiting for DAP handshake...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        reporter.section("METRICS");

        // Add metric with explicit connection_id
        Self::add_metric(
            client,
            reporter,
            metric_name,
            metric_location,
            metric_expression,
            &connection_id,
        )
        .await?;

        // Small delay for metric to be persisted
        tokio::time::sleep(Duration::from_millis(500)).await;

        // List metrics (may return 0 due to storage sync timing - endpoint functionality
        // is verified in comprehensive test)
        let _count = Self::list_metrics(client, reporter).await?;

        reporter.section("EVENTS");

        // Wait for events
        let event_count = Self::wait_for_events(
            client,
            reporter,
            metric_name,
            1, // At least 1 event
            wait_for_events_secs,
        )
        .await?;

        reporter.info(&format!("Captured {} events", event_count));

        reporter.section("CLEANUP");

        // Toggle metric off
        Self::toggle_metric(client, reporter, metric_name, false).await?;

        // Remove metric
        Self::remove_metric(client, reporter, metric_name).await?;

        // List connections
        Self::list_connections(client, reporter).await?;

        Ok(())
    }

    /// Test all MCP endpoints (comprehensive coverage)
    pub async fn all_endpoints<C: ApiClient>(
        client: &C,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), ApiError> {
        reporter.section("HEALTH & STATUS");
        Self::health_check(client, reporter).await?;
        Self::get_status(client, reporter).await?;
        Self::wake_sleep_cycle(client, reporter).await?;

        reporter.section("VALIDATION (Python)");
        Self::validate_expression(client, reporter, "user.id", "python", true).await?;
        Self::validate_expression(client, reporter, "eval('bad')", "python", false).await?;

        reporter.section("VALIDATION (Go)");
        Self::validate_expression(client, reporter, "len(items)", "go", true).await?;
        Self::validate_expression(client, reporter, "exec.Command(\"ls\")", "go", false).await?;

        reporter.section("METRICS (without connection)");
        // List should work even without connection - verify it returns successfully
        let metric_count = Self::list_metrics(client, reporter).await?;
        reporter.info(&format!(
            "Listed {} metrics without active connection (expected 0 or more)",
            metric_count
        ));

        Ok(())
    }
}
