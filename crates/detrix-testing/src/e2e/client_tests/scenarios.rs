//! Client conformance test scenarios
//!
//! Language-agnostic test scenarios that verify client implementations
//! conform to the specification in `clients/specs/conformance/test_cases.yaml`.

use super::{ClientStatus, ClientTester};
use crate::e2e::{ApiClient, TestReporter};
use std::sync::Arc;

/// Client conformance test scenarios
///
/// These scenarios implement the test cases defined in `clients/specs/conformance/test_cases.yaml`.
/// They are designed to be run against any client implementation that implements the
/// `ClientTester` trait.
pub struct ClientTestScenarios;

impl ClientTestScenarios {
    /// ST001: Initial state is SLEEPING
    ///
    /// After init(), client state should be 'sleeping'
    pub async fn initial_state_sleeping<T: ClientTester>(
        tester: &T,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        let step = reporter.step_start("ST001", "Initial state is SLEEPING");

        let status = tester.status().await?;

        if status.state != "sleeping" {
            reporter.step_failed(
                step,
                &format!("Expected 'sleeping', got '{}'", status.state),
            );
            return Err(format!("Expected 'sleeping', got '{}'", status.state));
        }

        if status.connection_id.is_some() {
            reporter.step_failed(step, "Expected connection_id to be null");
            return Err("Expected connection_id to be null".to_string());
        }

        reporter.step_success(step, Some("State is 'sleeping', connection_id is null"));
        Ok(())
    }

    /// ST002: Wake transitions to AWAKE
    ///
    /// Calling wake() should transition from SLEEPING to AWAKE
    pub async fn wake_transitions_to_awake<T: ClientTester, D: ApiClient>(
        tester: &T,
        daemon: &D,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        let step = reporter.step_start("ST002", "Wake transitions to AWAKE");

        let result = tester.wake(None).await?;

        if result.status != "awake" {
            reporter.step_failed(step, &format!("Expected 'awake', got '{}'", result.status));
            return Err(format!("Expected 'awake', got '{}'", result.status));
        }

        if result.debug_port == 0 {
            reporter.step_failed(step, "debug_port should be > 0");
            return Err("debug_port should be > 0".to_string());
        }

        // Verify state is now awake
        let status = tester.status().await?;
        if status.state != "awake" {
            reporter.step_failed(
                step,
                &format!("Status state should be 'awake', got '{}'", status.state),
            );
            return Err(format!(
                "Status state should be 'awake', got '{}'",
                status.state
            ));
        }

        if status.connection_id.is_none() {
            reporter.step_failed(step, "connection_id should be present after wake");
            return Err("connection_id should be present after wake".to_string());
        }

        // Verify daemon received the registration
        let connections = daemon.list_connections().await.map_err(|e| e.to_string())?;
        let has_connection = connections.data.iter().any(|c| {
            status
                .connection_id
                .as_ref()
                .is_some_and(|id| c.connection_id == *id)
        });

        if !has_connection {
            reporter.step_failed(step, "Daemon should have the registered connection");
            return Err("Daemon should have the registered connection".to_string());
        }

        reporter.step_success(
            step,
            Some(&format!(
                "State is 'awake', debug_port={}, connection registered",
                result.debug_port
            )),
        );
        Ok(())
    }

    /// ST003: Sleep transitions to SLEEPING
    ///
    /// Calling sleep() should transition from AWAKE to SLEEPING
    pub async fn sleep_transitions_to_sleeping<T: ClientTester, D: ApiClient>(
        tester: &T,
        daemon: &D,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        let step = reporter.step_start("ST003", "Sleep transitions to SLEEPING");

        // Get connection_id before sleep
        let status_before = tester.status().await?;
        let connection_id = status_before.connection_id.clone();

        let result = tester.sleep().await?;

        if result.status != "sleeping" {
            reporter.step_failed(
                step,
                &format!("Expected 'sleeping', got '{}'", result.status),
            );
            return Err(format!("Expected 'sleeping', got '{}'", result.status));
        }

        // Verify state is now sleeping
        let status = tester.status().await?;
        if status.state != "sleeping" {
            reporter.step_failed(
                step,
                &format!("Status state should be 'sleeping', got '{}'", status.state),
            );
            return Err(format!(
                "Status state should be 'sleeping', got '{}'",
                status.state
            ));
        }

        if status.connection_id.is_some() {
            reporter.step_failed(step, "connection_id should be null after sleep");
            return Err("connection_id should be null after sleep".to_string());
        }

        // Verify daemon has marked the connection as disconnected
        // Note: The daemon doesn't delete connections on close, it marks them as "Disconnected"
        if let Some(conn_id) = connection_id {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            let connections = daemon.list_connections().await.map_err(|e| e.to_string())?;

            // Check if connection is either deleted OR marked as disconnected
            let conn_entry = connections.data.iter().find(|c| c.connection_id == conn_id);

            match conn_entry {
                None => {
                    // Connection was deleted - that's fine
                }
                Some(conn) => {
                    // Connection exists but should be marked as disconnected
                    let status_lower = conn.status.to_lowercase();
                    if !status_lower.contains("disconnected") && !status_lower.contains("closed") {
                        reporter.step_failed(
                            step,
                            &format!(
                                "Connection '{}' should be disconnected but status is '{}'",
                                conn_id, conn.status
                            ),
                        );
                        return Err(format!(
                            "Connection '{}' should be disconnected but status is '{}'",
                            conn_id, conn.status
                        ));
                    }
                }
            }
        }

        reporter.step_success(step, Some("State is 'sleeping', connection unregistered"));
        Ok(())
    }

    /// ST004: Wake is idempotent
    ///
    /// Calling wake() when already awake returns 'already_awake'
    pub async fn wake_is_idempotent<T: ClientTester>(
        tester: &T,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        let step = reporter.step_start("ST004", "Wake is idempotent");

        let result = tester.wake(None).await?;

        if result.status != "already_awake" {
            reporter.step_failed(
                step,
                &format!("Expected 'already_awake', got '{}'", result.status),
            );
            return Err(format!("Expected 'already_awake', got '{}'", result.status));
        }

        // State should still be awake
        let status = tester.status().await?;
        if status.state != "awake" {
            reporter.step_failed(
                step,
                &format!("State should remain 'awake', got '{}'", status.state),
            );
            return Err(format!(
                "State should remain 'awake', got '{}'",
                status.state
            ));
        }

        reporter.step_success(step, Some("Second wake returned 'already_awake'"));
        Ok(())
    }

    /// ST005: Sleep is idempotent
    ///
    /// Calling sleep() when already sleeping returns 'already_sleeping'
    pub async fn sleep_is_idempotent<T: ClientTester>(
        tester: &T,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        let step = reporter.step_start("ST005", "Sleep is idempotent");

        let result = tester.sleep().await?;

        if result.status != "already_sleeping" {
            reporter.step_failed(
                step,
                &format!("Expected 'already_sleeping', got '{}'", result.status),
            );
            return Err(format!(
                "Expected 'already_sleeping', got '{}'",
                result.status
            ));
        }

        // State should still be sleeping
        let status = tester.status().await?;
        if status.state != "sleeping" {
            reporter.step_failed(
                step,
                &format!("State should remain 'sleeping', got '{}'", status.state),
            );
            return Err(format!(
                "State should remain 'sleeping', got '{}'",
                status.state
            ));
        }

        reporter.step_success(step, Some("Second sleep returned 'already_sleeping'"));
        Ok(())
    }

    /// EP001: Health endpoint format
    ///
    /// /detrix/health returns correct format
    pub async fn health_endpoint_format<T: ClientTester>(
        tester: &T,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        let step = reporter.step_start("EP001", "Health endpoint format");

        let health = tester.health().await?;

        if health.status != "ok" {
            reporter.step_failed(
                step,
                &format!("Expected status 'ok', got '{}'", health.status),
            );
            return Err(format!("Expected status 'ok', got '{}'", health.status));
        }

        if health.service != "detrix-client" {
            reporter.step_failed(
                step,
                &format!("Expected service 'detrix-client', got '{}'", health.service),
            );
            return Err(format!(
                "Expected service 'detrix-client', got '{}'",
                health.service
            ));
        }

        reporter.step_success(step, Some("Health response format correct"));
        Ok(())
    }

    /// EP002: Status endpoint format
    ///
    /// /detrix/status returns all required fields
    pub async fn status_endpoint_format<T: ClientTester>(
        tester: &T,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        let step = reporter.step_start("EP002", "Status endpoint format");

        let status: ClientStatus = tester.status().await?;

        // Verify all required fields are present
        // (The struct definition enforces this, but we'll check values are sensible)
        if status.name.is_empty() {
            reporter.step_failed(step, "name should not be empty");
            return Err("name should not be empty".to_string());
        }

        if !["sleeping", "waking", "awake"].contains(&status.state.as_str()) {
            reporter.step_failed(step, &format!("Invalid state: '{}'", status.state));
            return Err(format!("Invalid state: '{}'", status.state));
        }

        if status.control_port == 0 {
            reporter.step_failed(step, "control_port should be > 0");
            return Err("control_port should be > 0".to_string());
        }

        reporter.step_success(
            step,
            Some(&format!(
                "Status has all fields: state={}, name={}, control_port={}",
                status.state, status.name, status.control_port
            )),
        );
        Ok(())
    }

    /// EH001: Wake without daemon fails
    ///
    /// Wake fails gracefully when daemon is not reachable
    pub async fn wake_without_daemon_fails<T: ClientTester>(
        tester: &T,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        let step = reporter.step_start("EH001", "Wake without daemon fails");

        // Wake with invalid daemon URL
        let result = tester.wake(Some("http://127.0.0.1:1")).await;

        if result.is_ok() {
            reporter.step_failed(step, "Wake should fail with unreachable daemon");
            return Err("Wake should fail with unreachable daemon".to_string());
        }

        // State should revert to sleeping
        let status = tester.status().await?;
        if status.state != "sleeping" {
            reporter.step_failed(
                step,
                &format!("State should revert to 'sleeping', got '{}'", status.state),
            );
            return Err(format!(
                "State should revert to 'sleeping', got '{}'",
                status.state
            ));
        }

        reporter.step_success(
            step,
            Some("Wake failed as expected, state reverted to sleeping"),
        );
        Ok(())
    }

    /// CC001: Concurrent wake is safe
    ///
    /// Multiple concurrent wake() calls result in exactly one registration
    pub async fn concurrent_wake_is_safe<T: ClientTester + 'static, D: ApiClient>(
        tester: &Arc<T>,
        daemon: &D,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        let step = reporter.step_start("CC001", "Concurrent wake is safe");

        // Spawn multiple concurrent wake calls
        let mut handles = Vec::new();
        for _ in 0..5 {
            let tester_clone = Arc::clone(tester);
            handles.push(tokio::spawn(async move { tester_clone.wake(None).await }));
        }

        // Wait for all to complete
        let results: Vec<_> = futures::future::join_all(handles).await;

        // Count successful wakes (should be exactly one "awake", rest "already_awake")
        let mut awake_count = 0;
        let mut already_awake_count = 0;
        for result in results {
            if let Ok(Ok(wake_resp)) = result {
                if wake_resp.status == "awake" {
                    awake_count += 1;
                } else if wake_resp.status == "already_awake" {
                    already_awake_count += 1;
                }
            }
        }

        if awake_count != 1 {
            reporter.step_failed(
                step,
                &format!("Expected exactly 1 'awake', got {}", awake_count),
            );
            return Err(format!("Expected exactly 1 'awake', got {}", awake_count));
        }

        // Verify only one connected connection in daemon
        // Note: Disconnected connections may still exist in the list
        let connections = daemon.list_connections().await.map_err(|e| e.to_string())?;
        let connected_count = connections
            .data
            .iter()
            .filter(|c| {
                c.status.to_lowercase().contains("connected")
                    && !c.status.to_lowercase().contains("disconnected")
            })
            .count();
        if connected_count != 1 {
            reporter.step_failed(
                step,
                &format!(
                    "Expected exactly 1 connected connection in daemon, got {}",
                    connected_count
                ),
            );
            return Err(format!(
                "Expected exactly 1 connected connection in daemon, got {}",
                connected_count
            ));
        }

        reporter.step_success(
            step,
            Some(&format!(
                "1 'awake' + {} 'already_awake', 1 daemon connection",
                already_awake_count
            )),
        );
        Ok(())
    }

    /// Run the full conformance suite for a client that hasn't been woken yet
    ///
    /// This runs tests in the proper order:
    /// 1. Initial state tests (client is sleeping)
    /// 2. Wake and verify (transitions to awake)
    /// 3. Wake idempotency (already awake)
    /// 4. Sleep and verify (transitions back to sleeping)
    /// 5. Sleep idempotency (already sleeping)
    pub async fn run_full_suite<T: ClientTester + 'static, D: ApiClient>(
        tester: &Arc<T>,
        daemon: &D,
        reporter: &Arc<TestReporter>,
    ) -> Result<(), String> {
        reporter.print_header();

        // Phase 1: Initial state tests (sleeping)
        Self::initial_state_sleeping(tester.as_ref(), reporter).await?;
        Self::health_endpoint_format(tester.as_ref(), reporter).await?;
        Self::status_endpoint_format(tester.as_ref(), reporter).await?;
        Self::sleep_is_idempotent(tester.as_ref(), reporter).await?;

        // Phase 2: Wake tests
        Self::wake_transitions_to_awake(tester.as_ref(), daemon, reporter).await?;
        Self::wake_is_idempotent(tester.as_ref(), reporter).await?;

        // Phase 3: Sleep tests (from awake)
        Self::sleep_transitions_to_sleeping(tester.as_ref(), daemon, reporter).await?;
        Self::sleep_is_idempotent(tester.as_ref(), reporter).await?;

        // Phase 4: Error handling tests
        Self::wake_without_daemon_fails(tester.as_ref(), reporter).await?;

        // Phase 5: Concurrency tests (need to be sleeping first, then wake concurrently)
        // Don't wake before - let the concurrent test handle multiple wake calls from sleep
        Self::concurrent_wake_is_safe(tester, daemon, reporter).await?;

        reporter.print_footer(true);
        Ok(())
    }
}
