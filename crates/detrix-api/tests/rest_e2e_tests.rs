//! REST API E2E tests for connection lifecycle endpoints

use detrix_testing::e2e::client::ApiClient;
use detrix_testing::e2e::executor::{
    get_debugpy_port, register_e2e_process, start_debugpy_setsid, wait_for_debugger_port,
};
use detrix_testing::e2e::rest::RestClient;
use detrix_testing::e2e::TestExecutor;
use serde_json::json;

/// Helper to create a connection with explicit identity fields via raw HTTP.
/// This lets us control name/workspace_root/hostname for UUID uniqueness.
async fn create_connection_raw(
    base_url: &str,
    host: &str,
    port: u16,
    language: &str,
    name: &str,
    workspace_root: &str,
    hostname: &str,
) -> Result<String, String> {
    let request = detrix_api::CreateConnectionRequest {
        host: host.to_string(),
        port: port.into(),
        language: language.to_string(),
        name: name.to_string(),
        workspace_root: workspace_root.to_string(),
        hostname: hostname.to_string(),
        metadata: None,
        program: None,
        safe_mode: false,
        pid: None,
        control_plane_url: None,
        build_commit: None,
        build_tag: None,
        created_by: None,
    };

    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/connections", base_url))
        .header(detrix_api::common::CLIENT_ID_HEADER, "test-client")
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("Create connection failed: {}", e))?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;

    if !status.is_success() {
        return Err(format!("HTTP {} - {}", status, text));
    }

    let resp: detrix_api::CreateConnectionResponse =
        serde_json::from_str(&text).map_err(|e| format!("Parse error: {}", e))?;

    Ok(resp.connection_id)
}

#[tokio::test]
async fn test_touch_connections_updates_last_active() {
    let mut executor = TestExecutor::new();
    executor
        .start_daemon()
        .await
        .expect("Failed to start daemon");

    // Start debugpy so the adapter can connect
    let script_path = executor
        .detrix_example_app_path()
        .expect("Python fixture not found");
    executor
        .start_debugpy(script_path.to_str().unwrap())
        .await
        .expect("Failed to start debugpy");

    let rest_client = RestClient::new(executor.http_port);
    let base_url = format!("http://127.0.0.1:{}", executor.http_port);
    let client = reqwest::Client::new();

    // Create a connection (using debugpy port)
    let conn_info = rest_client
        .create_connection("127.0.0.1", executor.debugpy_port, "python")
        .await
        .expect("Failed to create connection");

    let connection_id = &conn_info.data.connection_id;

    // Get initial last_active timestamp via direct HTTP GET
    let get_response = client
        .get(format!("{}/api/v1/connections/{}", base_url, connection_id))
        .send()
        .await
        .expect("Failed to get connection");

    let conn_before: serde_json::Value = get_response
        .json()
        .await
        .expect("Failed to parse connection");

    let last_active_before = conn_before["lastActiveAt"]
        .as_i64()
        .expect("Missing lastActiveAt");

    // Wait a bit to ensure timestamp changes
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Touch the connection using HTTP POST
    let touch_payload = json!({
        "connectionIds": [connection_id]
    });

    let touch_response = client
        .post(format!("{}/api/v1/connections/touch", base_url))
        .json(&touch_payload)
        .send()
        .await
        .expect("Failed to send touch request");

    assert!(
        touch_response.status().is_success(),
        "Touch request failed with status: {}",
        touch_response.status()
    );

    let touch_result: serde_json::Value = touch_response
        .json()
        .await
        .expect("Failed to parse touch response");

    let updated_count = touch_result["updated"]
        .as_u64()
        .expect("Missing updated count");
    assert_eq!(updated_count, 1, "Should update exactly 1 connection");

    // Get updated last_active timestamp via direct HTTP GET
    let get_response_after = client
        .get(format!("{}/api/v1/connections/{}", base_url, connection_id))
        .send()
        .await
        .expect("Failed to get connection after touch");

    let conn_after: serde_json::Value = get_response_after
        .json()
        .await
        .expect("Failed to parse connection after touch");

    let last_active_after = conn_after["lastActiveAt"]
        .as_i64()
        .expect("Missing lastActiveAt after touch");

    assert!(
        last_active_after > last_active_before,
        "last_active should be updated: before={}, after={}",
        last_active_before,
        last_active_after
    );
}

#[tokio::test]
async fn test_touch_connections_handles_nonexistent() {
    let mut executor = TestExecutor::new();
    executor
        .start_daemon()
        .await
        .expect("Failed to start daemon");

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Touch a non-existent connection
    let touch_payload = json!({
        "connectionIds": ["00000000000000000000000000000000"]
    });

    let client = reqwest::Client::new();
    let touch_response = client
        .post(format!("{}/api/v1/connections/touch", base_url))
        .json(&touch_payload)
        .send()
        .await
        .expect("Failed to send touch request");

    assert!(
        touch_response.status().is_success(),
        "Touch request should succeed even for nonexistent ID"
    );

    let touch_result: serde_json::Value = touch_response
        .json()
        .await
        .expect("Failed to parse touch response");

    let updated_count = touch_result["updated"]
        .as_u64()
        .expect("Missing updated count");
    assert_eq!(
        updated_count, 0,
        "Should update 0 connections when touching non-existent ID"
    );
}

#[tokio::test]
async fn test_touch_connections_batch() {
    let mut executor = TestExecutor::new();
    executor
        .start_daemon()
        .await
        .expect("Failed to start daemon");

    let script_path = executor
        .detrix_example_app_path()
        .expect("Python fixture not found");
    let script = script_path.to_str().unwrap();

    // Start first debugpy (uses executor's built-in port)
    executor
        .start_debugpy(script)
        .await
        .expect("Failed to start debugpy #1");
    let port1 = executor.debugpy_port;

    // Start second debugpy (manually managed)
    let port2 = get_debugpy_port();
    let mut debugpy2 = start_debugpy_setsid(port2, script).expect("Failed to spawn debugpy #2");
    register_e2e_process("debugpy", debugpy2.id());

    if !wait_for_debugger_port(port2, 60).await {
        let _ = debugpy2.kill();
        panic!("debugpy #2 not listening on port {}", port2);
    }

    let base_url = format!("http://127.0.0.1:{}", executor.http_port);

    // Create two connections with different debugpy instances (different UUIDs)
    let connection_id1 = create_connection_raw(
        &base_url,
        "127.0.0.1",
        port1,
        "python",
        "batch-test-conn-1",
        "/e2e-batch-test",
        "batch-host",
    )
    .await
    .expect("Failed to create connection 1");

    let connection_id2 = create_connection_raw(
        &base_url,
        "127.0.0.1",
        port2,
        "python",
        "batch-test-conn-2",
        "/e2e-batch-test",
        "batch-host",
    )
    .await
    .expect("Failed to create connection 2");

    // Kill debugpy2 cleanup handle (will be cleaned up by e2e process tracker)
    let _ = debugpy2.kill();

    // Touch both connections at once
    let touch_payload = json!({
        "connectionIds": [connection_id1, connection_id2]
    });

    let client = reqwest::Client::new();
    let touch_response = client
        .post(format!("{}/api/v1/connections/touch", base_url))
        .json(&touch_payload)
        .send()
        .await
        .expect("Failed to send touch request");

    assert!(
        touch_response.status().is_success(),
        "Touch request failed with status: {}",
        touch_response.status()
    );

    let touch_result: serde_json::Value = touch_response
        .json()
        .await
        .expect("Failed to parse touch response");

    let updated_count = touch_result["updated"]
        .as_u64()
        .expect("Missing updated count");
    assert_eq!(updated_count, 2, "Should update exactly 2 connections");
}
