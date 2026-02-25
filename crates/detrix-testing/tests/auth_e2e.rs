//! E2E tests for authentication
//!
//! Tests the simplified auth model:
//! - Simple mode: Static bearer token from config (like Prometheus)
//! - External mode: JWT validation via JWKS endpoint (for enterprise SSO)
//! - Disabled mode: No authentication required
//! - Auto-auth: Secure-by-default when no [api.auth] section in config
//!
//! Run with: `cargo test --package detrix-testing --test auth_e2e -- --test-threads=1`

use reqwest::{Client, StatusCode};
use serial_test::serial;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;

use detrix_api::generated::detrix::v1::{
    metrics_service_client::MetricsServiceClient, ListMetricsRequest,
};
use detrix_config::constants::{AUTHORIZATION_HEADER, AUTHORIZATION_METADATA_KEY, BEARER_PREFIX};
use detrix_testing::e2e::executor::{
    find_detrix_binary, get_grpc_port, get_http_port, wait_for_port, TestDaemonSetup,
};
use detrix_testing::e2e::jwt::{JwtBuilder, JwtKeyPair, MockJwksServer, TestClaims};
use tonic::transport::Channel;
use tonic::Request;

/// Test executor for simple bearer token auth (no user management)
struct SimpleBearerTestExecutor {
    temp_dir: TempDir,
    http_port: u16,
    grpc_port: u16,
    daemon_process: Option<Child>,
    daemon_log_path: PathBuf,
    workspace_root: PathBuf,
    client: Client,
    bearer_token: String,
}

impl SimpleBearerTestExecutor {
    fn new(bearer_token: &str) -> Self {
        let setup = TestDaemonSetup::new();
        let http_port = get_http_port();
        let grpc_port = get_grpc_port();

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            temp_dir: setup.temp_dir,
            http_port,
            grpc_port,
            daemon_process: None,
            daemon_log_path: setup.daemon_log_path,
            workspace_root: setup.workspace_root,
            client,
            bearer_token: bearer_token.to_string(),
        }
    }

    /// Start daemon with simple bearer token auth (no user management)
    async fn start_daemon(&mut self) -> Result<(), String> {
        let db_path = self.temp_dir.path().join("detrix.db");
        let config_content = format!(
            r#"
[metadata]
version = "1.0"

[project]
base_path = "{}"

[storage]
storage_type = "sqlite"
path = "{}"

[api]
port_fallback = false

[api.rest]
enabled = true
host = "127.0.0.1"
port = {}

[api.grpc]
enabled = true
host = "127.0.0.1"
port = {}

[api.auth]
mode = "simple"
bearer_token = "{}"

[safety]
enable_ast_analysis = false
"#,
            self.workspace_root.display(),
            db_path.display(),
            self.http_port,
            self.grpc_port,
            self.bearer_token,
        );

        let config_path = self.temp_dir.path().join("detrix.toml");
        std::fs::write(&config_path, config_content).map_err(|e| e.to_string())?;

        let binary_path =
            match find_detrix_binary(&self.workspace_root) {
                Some(p) => p,
                None => return Err(
                    "detrix binary not found. Set DETRIX_BIN or run `cargo build -p detrix-cli`"
                        .to_string(),
                ),
            };

        let daemon_log_file =
            std::fs::File::create(&self.daemon_log_path).map_err(|e| e.to_string())?;
        let daemon_log_stderr = daemon_log_file.try_clone().map_err(|e| e.to_string())?;

        let process = Command::new(&binary_path)
            .args(["serve", "--config", config_path.to_str().unwrap()])
            .current_dir(&self.workspace_root)
            .env("RUST_LOG", "debug")
            .stdin(Stdio::null())
            .stdout(Stdio::from(daemon_log_file))
            .stderr(Stdio::from(daemon_log_stderr))
            .spawn();

        match process {
            Ok(p) => {
                self.daemon_process = Some(p);
                if !wait_for_port(self.http_port, 30).await {
                    return Err(format!("Daemon not responding on port {}", self.http_port));
                }
                // Also wait for gRPC port
                if !wait_for_port(self.grpc_port, 10).await {
                    return Err(format!("gRPC not responding on port {}", self.grpc_port));
                }
                Ok(())
            }
            Err(e) => Err(format!("Could not spawn daemon: {}", e)),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    fn stop(&mut self) {
        if let Some(mut p) = self.daemon_process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }

    fn print_daemon_logs(&self, last_n_lines: usize) {
        println!("\n=== DAEMON LOG (last {} lines) ===", last_n_lines);
        if let Ok(content) = std::fs::read_to_string(&self.daemon_log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read daemon log)");
        }
        println!("=================================\n");
    }

    /// Request a protected endpoint without auth
    async fn request_without_auth(&self, path: &str) -> StatusCode {
        match self
            .client
            .get(format!("{}{}", self.base_url(), path))
            .send()
            .await
        {
            Ok(resp) => resp.status(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Request a protected endpoint with bearer token
    async fn request_with_token(&self, path: &str, token: &str) -> StatusCode {
        match self
            .client
            .get(format!("{}{}", self.base_url(), path))
            .header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token))
            .send()
            .await
        {
            Ok(resp) => resp.status(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl Drop for SimpleBearerTestExecutor {
    fn drop(&mut self) {
        self.stop();
    }
}

// ==================== REST API TESTS ====================

/// Test 1: Simple bearer token - protected endpoints require token
#[tokio::test]
async fn test_simple_bearer_protected_endpoint() {
    let mut executor = SimpleBearerTestExecutor::new("my-secret-token-123");

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Request without token should fail
    let status = executor.request_without_auth("/api/v1/metrics").await;
    if status != StatusCode::UNAUTHORIZED {
        executor.print_daemon_logs(100);
        panic!(
            "Protected endpoint should require auth. Got status: {}",
            status
        );
    }

    // Request with correct token should succeed
    let status = executor
        .request_with_token("/api/v1/metrics", "my-secret-token-123")
        .await;
    if status != StatusCode::OK {
        executor.print_daemon_logs(100);
        panic!("Valid token should grant access. Got status: {}", status);
    }

    println!("✓ Simple bearer token auth working!");
}

/// Test 2: Simple bearer token - invalid token rejected
#[tokio::test]
async fn test_simple_bearer_invalid_token() {
    let mut executor = SimpleBearerTestExecutor::new("correct-token");

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Request with wrong token should fail
    let status = executor
        .request_with_token("/api/v1/metrics", "wrong-token")
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "Invalid token should be rejected"
    );

    println!("✓ Invalid token correctly rejected!");
}

/// Test 3: Simple bearer token - health endpoint is public
#[tokio::test]
async fn test_simple_bearer_health_public() {
    let mut executor = SimpleBearerTestExecutor::new("test-token");

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Health endpoint should be public
    let status = executor.request_without_auth("/health").await;
    assert_eq!(status, StatusCode::OK, "Health endpoint should be public");

    println!("✓ Health endpoint is public!");
}

/// Test 4: Simple bearer token - prometheus metrics endpoint is public
#[tokio::test]
async fn test_simple_bearer_metrics_endpoint_public() {
    let mut executor = SimpleBearerTestExecutor::new("test-token");

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // /metrics endpoint (Prometheus format) should be public
    let status = executor.request_without_auth("/metrics").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Prometheus metrics endpoint should be public"
    );

    println!("✓ Prometheus metrics endpoint is public!");
}

// ==================== gRPC AUTH TESTS ====================

/// Test 5: gRPC - protected endpoints require auth
#[tokio::test]
async fn test_grpc_protected_endpoint_requires_auth() {
    let mut executor = SimpleBearerTestExecutor::new("grpc-test-token");

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Connect to gRPC without auth
    let addr = format!("http://127.0.0.1:{}", executor.grpc_port);
    let channel = Channel::from_shared(addr)
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to gRPC");

    let mut client = MetricsServiceClient::new(channel);

    // Request without auth should fail with Unauthenticated
    let result = client
        .list_metrics(ListMetricsRequest {
            group: None,
            enabled_only: None,
            name_pattern: None,
            metadata: None,
        })
        .await;

    assert!(result.is_err(), "Request without auth should fail");
    let status = result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::Unauthenticated,
        "Should return Unauthenticated status"
    );

    println!("✓ gRPC protected endpoints require auth!");
}

/// Test 6: gRPC - valid bearer token grants access
#[tokio::test]
async fn test_grpc_valid_token_grants_access() {
    let mut executor = SimpleBearerTestExecutor::new("grpc-valid-token");

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Connect to gRPC
    let addr = format!("http://127.0.0.1:{}", executor.grpc_port);
    let channel = Channel::from_shared(addr)
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to gRPC");

    let mut client = MetricsServiceClient::new(channel);

    // Create request with auth header
    let mut request = Request::new(ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    });
    request.metadata_mut().insert(
        AUTHORIZATION_METADATA_KEY,
        format!("{}grpc-valid-token", BEARER_PREFIX)
            .parse()
            .unwrap(),
    );

    // Request with valid token should succeed
    let result = client.list_metrics(request).await;
    assert!(result.is_ok(), "Request with valid token should succeed");

    println!("✓ gRPC valid token grants access!");
}

/// Test 7: gRPC - invalid bearer token rejected
#[tokio::test]
async fn test_grpc_invalid_token_rejected() {
    let mut executor = SimpleBearerTestExecutor::new("correct-grpc-token");

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Connect to gRPC
    let addr = format!("http://127.0.0.1:{}", executor.grpc_port);
    let channel = Channel::from_shared(addr)
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to gRPC");

    let mut client = MetricsServiceClient::new(channel);

    // Create request with wrong token
    let mut request = Request::new(ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    });
    request.metadata_mut().insert(
        AUTHORIZATION_METADATA_KEY,
        format!("{}wrong-token", BEARER_PREFIX).parse().unwrap(),
    );

    // Request with wrong token should fail
    let result = client.list_metrics(request).await;
    assert!(result.is_err(), "Request with wrong token should fail");

    let status = result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::Unauthenticated,
        "Should return Unauthenticated status"
    );

    println!("✓ gRPC invalid token correctly rejected!");
}

/// Test 8: Cross-protocol - same token works on REST and gRPC
#[tokio::test]
async fn test_cross_protocol_token_consistency() {
    let shared_token = "shared-cross-protocol-token";
    let mut executor = SimpleBearerTestExecutor::new(shared_token);

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Test REST with token
    let rest_status = executor
        .request_with_token("/api/v1/metrics", shared_token)
        .await;
    assert_eq!(rest_status, StatusCode::OK, "REST should accept the token");

    // Test gRPC with same token
    let addr = format!("http://127.0.0.1:{}", executor.grpc_port);
    let channel = Channel::from_shared(addr)
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to gRPC");

    let mut client = MetricsServiceClient::new(channel);
    let mut request = Request::new(ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    });
    request.metadata_mut().insert(
        AUTHORIZATION_METADATA_KEY,
        format!("{}{}", BEARER_PREFIX, shared_token)
            .parse()
            .unwrap(),
    );

    let grpc_result = client.list_metrics(request).await;
    assert!(
        grpc_result.is_ok(),
        "gRPC should accept the same token as REST"
    );

    println!("✓ Same token works across REST and gRPC!");
}

// ==================== EXTERNAL JWT AUTH TESTS ====================

/// Test executor for external JWT auth mode
struct ExternalJwtTestExecutor {
    temp_dir: TempDir,
    http_port: u16,
    grpc_port: u16,
    daemon_process: Option<Child>,
    daemon_log_path: PathBuf,
    workspace_root: PathBuf,
    client: Client,
    key_pair: JwtKeyPair,
    jwks_server: Option<MockJwksServer>,
    issuer: String,
    audience: String,
}

impl ExternalJwtTestExecutor {
    async fn new() -> Self {
        let setup = TestDaemonSetup::new();
        let http_port = get_http_port();
        let grpc_port = get_grpc_port();

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");

        // Generate key pair and start JWKS server
        let key_pair = JwtKeyPair::generate();
        let jwks_server = MockJwksServer::start(&key_pair).await;
        let issuer = jwks_server.issuer();
        let audience = "detrix".to_string();

        Self {
            temp_dir: setup.temp_dir,
            http_port,
            grpc_port,
            daemon_process: None,
            daemon_log_path: setup.daemon_log_path,
            workspace_root: setup.workspace_root,
            client,
            key_pair,
            jwks_server: Some(jwks_server),
            issuer,
            audience,
        }
    }

    /// Start daemon with external JWT auth mode
    async fn start_daemon(&mut self) -> Result<(), String> {
        let jwks_server = self.jwks_server.as_ref().ok_or("JWKS server not started")?;
        let db_path = self.temp_dir.path().join("detrix.db");
        let config_content = format!(
            r#"
[metadata]
version = "1.0"

[project]
base_path = "{}"

[storage]
storage_type = "sqlite"
path = "{}"

[api]
port_fallback = false

[api.rest]
enabled = true
host = "127.0.0.1"
port = {}

[api.grpc]
enabled = true
host = "127.0.0.1"
port = {}

[api.auth]
mode = "external"

[api.auth.jwt]
jwks_url = "{}"
issuer = "{}"
audience = "{}"
cache_ttl_seconds = 60

[safety]
enable_ast_analysis = false
"#,
            self.workspace_root.display(),
            db_path.display(),
            self.http_port,
            self.grpc_port,
            jwks_server.jwks_url(),
            self.issuer,
            self.audience,
        );

        let config_path = self.temp_dir.path().join("detrix.toml");
        std::fs::write(&config_path, config_content).map_err(|e| e.to_string())?;

        let binary_path =
            match find_detrix_binary(&self.workspace_root) {
                Some(p) => p,
                None => return Err(
                    "detrix binary not found. Set DETRIX_BIN or run `cargo build -p detrix-cli`"
                        .to_string(),
                ),
            };

        let daemon_log_file =
            std::fs::File::create(&self.daemon_log_path).map_err(|e| e.to_string())?;
        let daemon_log_stderr = daemon_log_file.try_clone().map_err(|e| e.to_string())?;

        let process = Command::new(&binary_path)
            .args(["serve", "--config", config_path.to_str().unwrap()])
            .current_dir(&self.workspace_root)
            .env("RUST_LOG", "debug")
            .stdin(Stdio::null())
            .stdout(Stdio::from(daemon_log_file))
            .stderr(Stdio::from(daemon_log_stderr))
            .spawn();

        match process {
            Ok(p) => {
                self.daemon_process = Some(p);
                if !wait_for_port(self.http_port, 30).await {
                    return Err(format!("Daemon not responding on port {}", self.http_port));
                }
                // Also wait for gRPC port
                if !wait_for_port(self.grpc_port, 10).await {
                    return Err(format!("gRPC not responding on port {}", self.grpc_port));
                }
                Ok(())
            }
            Err(e) => Err(format!("Could not spawn daemon: {}", e)),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    fn stop(&mut self) {
        if let Some(mut p) = self.daemon_process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }

    fn print_daemon_logs(&self, last_n_lines: usize) {
        println!("\n=== DAEMON LOG (last {} lines) ===", last_n_lines);
        if let Ok(content) = std::fs::read_to_string(&self.daemon_log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read daemon log)");
        }
        println!("=================================\n");
    }

    /// Create a valid JWT for testing
    fn create_valid_jwt(&self) -> String {
        let claims = TestClaims::new("user123", &self.issuer)
            .with_audience(&self.audience)
            .with_email("user@example.com")
            .with_name("Test User");

        JwtBuilder::new(&self.key_pair, claims).build()
    }

    /// Create a JWT with wrong issuer
    fn create_wrong_issuer_jwt(&self) -> String {
        let claims = TestClaims::new("user123", "https://wrong-issuer.example.com")
            .with_audience(&self.audience);

        JwtBuilder::new(&self.key_pair, claims).build()
    }

    /// Create a JWT with wrong audience
    fn create_wrong_audience_jwt(&self) -> String {
        let claims = TestClaims::new("user123", &self.issuer).with_audience("wrong-audience");

        JwtBuilder::new(&self.key_pair, claims).build()
    }

    /// Create an expired JWT
    fn create_expired_jwt(&self) -> String {
        let claims = TestClaims::expired("user123", &self.issuer).with_audience(&self.audience);

        JwtBuilder::new(&self.key_pair, claims).build()
    }

    /// Request a protected endpoint without auth
    async fn request_without_auth(&self, path: &str) -> StatusCode {
        match self
            .client
            .get(format!("{}{}", self.base_url(), path))
            .send()
            .await
        {
            Ok(resp) => resp.status(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Request a protected endpoint with JWT token
    async fn request_with_jwt(&self, path: &str, token: &str) -> StatusCode {
        match self
            .client
            .get(format!("{}{}", self.base_url(), path))
            .header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token))
            .send()
            .await
        {
            Ok(resp) => resp.status(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl Drop for ExternalJwtTestExecutor {
    fn drop(&mut self) {
        self.stop();
    }
}

// ==================== EXTERNAL JWT REST TESTS ====================

/// Test 9: External JWT - protected endpoints require valid JWT
#[tokio::test]
async fn test_external_jwt_protected_endpoint() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Request without token should fail
    let status = executor.request_without_auth("/api/v1/metrics").await;
    if status != StatusCode::UNAUTHORIZED {
        executor.print_daemon_logs(100);
        panic!(
            "Protected endpoint should require auth. Got status: {}",
            status
        );
    }

    // Request with valid JWT should succeed
    let token = executor.create_valid_jwt();
    let status = executor.request_with_jwt("/api/v1/metrics", &token).await;
    if status != StatusCode::OK {
        executor.print_daemon_logs(100);
        panic!("Valid JWT should grant access. Got status: {}", status);
    }

    println!("✓ External JWT auth working!");
}

/// Test 10: External JWT - expired token rejected
#[tokio::test]
async fn test_external_jwt_expired_token() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Request with expired JWT should fail
    let token = executor.create_expired_jwt();
    let status = executor.request_with_jwt("/api/v1/metrics", &token).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "Expired JWT should be rejected"
    );

    println!("✓ Expired JWT correctly rejected!");
}

/// Test 11: External JWT - wrong issuer rejected
#[tokio::test]
async fn test_external_jwt_wrong_issuer() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Request with wrong issuer should fail
    let token = executor.create_wrong_issuer_jwt();
    let status = executor.request_with_jwt("/api/v1/metrics", &token).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "JWT with wrong issuer should be rejected"
    );

    println!("✓ Wrong issuer JWT correctly rejected!");
}

/// Test 12: External JWT - wrong audience rejected
#[tokio::test]
async fn test_external_jwt_wrong_audience() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Request with wrong audience should fail
    let token = executor.create_wrong_audience_jwt();
    let status = executor.request_with_jwt("/api/v1/metrics", &token).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "JWT with wrong audience should be rejected"
    );

    println!("✓ Wrong audience JWT correctly rejected!");
}

/// Test 13: External JWT - health endpoint is public
#[tokio::test]
async fn test_external_jwt_health_public() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Health endpoint should be public
    let status = executor.request_without_auth("/health").await;
    assert_eq!(status, StatusCode::OK, "Health endpoint should be public");

    println!("✓ Health endpoint is public with external JWT mode!");
}

/// Test 14: External JWT - malformed token rejected
#[tokio::test]
async fn test_external_jwt_malformed_token() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Request with malformed JWT should fail
    let status = executor
        .request_with_jwt("/api/v1/metrics", "not-a-valid-jwt")
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "Malformed JWT should be rejected"
    );

    println!("✓ Malformed JWT correctly rejected!");
}

// ==================== EXTERNAL JWT gRPC TESTS ====================

/// Test 15: gRPC External JWT - protected endpoints require valid JWT
#[tokio::test]
async fn test_grpc_external_jwt_protected_endpoint() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Connect to gRPC without auth
    let addr = format!("http://127.0.0.1:{}", executor.grpc_port);
    let channel = Channel::from_shared(addr)
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to gRPC");

    let mut client = MetricsServiceClient::new(channel);

    // Request without auth should fail
    let result = client
        .list_metrics(ListMetricsRequest {
            group: None,
            enabled_only: None,
            name_pattern: None,
            metadata: None,
        })
        .await;

    assert!(result.is_err(), "Request without auth should fail");
    let status = result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::Unauthenticated,
        "Should return Unauthenticated status"
    );

    println!("✓ gRPC external JWT protected endpoints require auth!");
}

/// Test 16: gRPC External JWT - valid JWT grants access
#[tokio::test]
async fn test_grpc_external_jwt_valid_token() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Connect to gRPC
    let addr = format!("http://127.0.0.1:{}", executor.grpc_port);
    let channel = Channel::from_shared(addr)
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to gRPC");

    let mut client = MetricsServiceClient::new(channel);

    // Create request with valid JWT
    let token = executor.create_valid_jwt();
    let mut request = Request::new(ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    });
    request.metadata_mut().insert(
        AUTHORIZATION_METADATA_KEY,
        format!("{}{}", BEARER_PREFIX, token).parse().unwrap(),
    );

    // Request with valid JWT should succeed
    let result = client.list_metrics(request).await;
    if result.is_err() {
        executor.print_daemon_logs(100);
        panic!(
            "Request with valid JWT should succeed: {:?}",
            result.unwrap_err()
        );
    }

    println!("✓ gRPC valid JWT grants access!");
}

/// Test 17: gRPC External JWT - expired token rejected
#[tokio::test]
async fn test_grpc_external_jwt_expired_token() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Connect to gRPC
    let addr = format!("http://127.0.0.1:{}", executor.grpc_port);
    let channel = Channel::from_shared(addr)
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to gRPC");

    let mut client = MetricsServiceClient::new(channel);

    // Create request with expired JWT
    let token = executor.create_expired_jwt();
    let mut request = Request::new(ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    });
    request.metadata_mut().insert(
        AUTHORIZATION_METADATA_KEY,
        format!("{}{}", BEARER_PREFIX, token).parse().unwrap(),
    );

    // Request with expired JWT should fail
    let result = client.list_metrics(request).await;
    assert!(result.is_err(), "Request with expired JWT should fail");
    assert_eq!(
        result.unwrap_err().code(),
        tonic::Code::Unauthenticated,
        "Should return Unauthenticated status"
    );

    println!("✓ gRPC expired JWT correctly rejected!");
}

/// Test 18: Cross-protocol - same JWT works on REST and gRPC
#[tokio::test]
async fn test_external_jwt_cross_protocol() {
    let mut executor = ExternalJwtTestExecutor::new().await;

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Create one JWT to use on both protocols
    let token = executor.create_valid_jwt();

    // Test REST with JWT
    let rest_status = executor.request_with_jwt("/api/v1/metrics", &token).await;
    assert_eq!(rest_status, StatusCode::OK, "REST should accept the JWT");

    // Test gRPC with same JWT
    let addr = format!("http://127.0.0.1:{}", executor.grpc_port);
    let channel = Channel::from_shared(addr)
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to gRPC");

    let mut client = MetricsServiceClient::new(channel);
    let mut request = Request::new(ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    });
    request.metadata_mut().insert(
        AUTHORIZATION_METADATA_KEY,
        format!("{}{}", BEARER_PREFIX, token).parse().unwrap(),
    );

    let grpc_result = client.list_metrics(request).await;
    assert!(
        grpc_result.is_ok(),
        "gRPC should accept the same JWT as REST"
    );

    println!("✓ Same JWT works across REST and gRPC!");
}

// ==================== AUTO-AUTH (SECURE-BY-DEFAULT) TESTS ====================

/// Test executor for auto-auth mode (no [api.auth] section in config).
///
/// When no auth config is present, the daemon auto-generates a bearer token,
/// writes it to ~/detrix/auth-token, and enables simple auth mode.
///
/// Supports two modes:
/// - Default: daemon auto-generates token, writes to file
/// - With env token: daemon uses DETRIX_TOKEN env var, no file written
struct AutoAuthTestExecutor {
    temp_dir: TempDir,
    http_port: u16,
    grpc_port: u16,
    enable_grpc: bool,
    env_token: Option<String>,
    /// Cached auto-generated token, captured immediately after daemon starts.
    /// This avoids reading the global token file later (which may be overwritten
    /// by daemons from other test binaries running in parallel).
    auto_token: Option<String>,
    daemon_process: Option<Child>,
    daemon_log_path: PathBuf,
    workspace_root: PathBuf,
    client: Client,
}

impl AutoAuthTestExecutor {
    /// Create executor with auto-generated token (default auto-auth mode).
    fn new() -> Self {
        Self::with_options(true, None)
    }

    /// Create executor that sets DETRIX_TOKEN env var instead of auto-generating.
    fn with_env_token(token: &str) -> Self {
        Self::with_options(false, Some(token.to_string()))
    }

    /// Isolated auth token path inside this executor's temp dir.
    ///
    /// When DETRIX_HOME is pointed at `temp_dir`, the daemon writes its
    /// auto-generated token here instead of the global `~/detrix/auth-token`.
    fn auth_token_path(&self) -> PathBuf {
        self.temp_dir.path().join("auth-token")
    }

    fn with_options(enable_grpc: bool, env_token: Option<String>) -> Self {
        let setup = TestDaemonSetup::new();
        let http_port = get_http_port();
        let grpc_port = get_grpc_port();

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            temp_dir: setup.temp_dir,
            http_port,
            grpc_port,
            enable_grpc,
            env_token,
            auto_token: None,
            daemon_process: None,
            daemon_log_path: setup.daemon_log_path,
            workspace_root: setup.workspace_root,
            client,
        }
    }

    /// Start daemon WITHOUT any [api.auth] section — triggers auto-auth.
    async fn start_daemon(&mut self) -> Result<(), String> {
        let db_path = self.temp_dir.path().join("detrix.db");
        // NOTE: No [api.auth] section at all — this triggers auto-auth
        let grpc_section = if self.enable_grpc {
            format!(
                "[api.grpc]\nenabled = true\nhost = \"127.0.0.1\"\nport = {}\n",
                self.grpc_port
            )
        } else {
            "[api.grpc]\nenabled = false\n".to_string()
        };

        let config_content = format!(
            r#"
[metadata]
version = "1.0"

[project]
base_path = "{}"

[storage]
storage_type = "sqlite"
path = "{}"

[api]
port_fallback = false

[api.rest]
enabled = true
host = "127.0.0.1"
port = {}

{}

[safety]
enable_ast_analysis = false
"#,
            self.workspace_root.display(),
            db_path.display(),
            self.http_port,
            grpc_section,
        );

        let config_path = self.temp_dir.path().join("detrix.toml");
        std::fs::write(&config_path, config_content).map_err(|e| e.to_string())?;

        let binary_path =
            match find_detrix_binary(&self.workspace_root) {
                Some(p) => p,
                None => return Err(
                    "detrix binary not found. Set DETRIX_BIN or run `cargo build -p detrix-cli`"
                        .to_string(),
                ),
            };

        let daemon_log_file =
            std::fs::File::create(&self.daemon_log_path).map_err(|e| e.to_string())?;
        let daemon_log_stderr = daemon_log_file.try_clone().map_err(|e| e.to_string())?;

        let mut cmd = Command::new(&binary_path);
        cmd.args(["serve", "--config", config_path.to_str().unwrap()])
            .current_dir(&self.workspace_root)
            .env("RUST_LOG", "debug")
            // Isolate DETRIX_HOME so the daemon writes auth-token to our temp dir
            // instead of the global ~/detrix/auth-token. This prevents cross-binary
            // races between concurrent test processes (e.g. mcp_bridge_e2e).
            .env("DETRIX_HOME", self.temp_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(daemon_log_file))
            .stderr(Stdio::from(daemon_log_stderr));

        // Either set DETRIX_TOKEN env var or remove it for file-based auto-auth
        if let Some(ref token) = self.env_token {
            cmd.env("DETRIX_TOKEN", token);
        } else {
            cmd.env_remove("DETRIX_TOKEN");
        }

        let process = cmd.spawn();

        match process {
            Ok(p) => {
                self.daemon_process = Some(p);
                if !wait_for_port(self.http_port, 30).await {
                    return Err(format!("Daemon not responding on port {}", self.http_port));
                }
                if self.enable_grpc && !wait_for_port(self.grpc_port, 10).await {
                    return Err(format!("gRPC not responding on port {}", self.grpc_port));
                }
                // Capture the auto-generated token from the isolated temp dir.
                // Because DETRIX_HOME points to temp_dir, the daemon writes here
                // instead of the global ~/detrix/auth-token.
                if self.env_token.is_none() {
                    let token_path = self.auth_token_path();
                    if let Ok(token) = std::fs::read_to_string(&token_path) {
                        let token = token.trim().to_string();
                        if !token.is_empty() {
                            self.auto_token = Some(token);
                        }
                    }
                }
                Ok(())
            }
            Err(e) => Err(format!("Could not spawn daemon: {}", e)),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    fn stop(&mut self) {
        if let Some(mut p) = self.daemon_process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }

    fn print_daemon_logs(&self, last_n_lines: usize) {
        println!("\n=== DAEMON LOG (last {} lines) ===", last_n_lines);
        if let Ok(content) = std::fs::read_to_string(&self.daemon_log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(last_n_lines);
            for line in &lines[start..] {
                println!("{}", line);
            }
        } else {
            println!("   (could not read daemon log)");
        }
        println!("=================================\n");
    }

    async fn request_without_auth(&self, path: &str) -> reqwest::Result<StatusCode> {
        self.client
            .get(format!("{}{}", self.base_url(), path))
            .send()
            .await
            .map(|resp| resp.status())
    }

    async fn request_with_token(&self, path: &str, token: &str) -> reqwest::Result<StatusCode> {
        self.client
            .get(format!("{}{}", self.base_url(), path))
            .header(AUTHORIZATION_HEADER, format!("{}{}", BEARER_PREFIX, token))
            .send()
            .await
            .map(|resp| resp.status())
    }
}

impl Drop for AutoAuthTestExecutor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Test: Auto-auth creates token file and enforces auth on protected endpoints.
///
/// When no [api.auth] section is in the config, the daemon should:
/// 1. Auto-generate a bearer token
/// 2. Write it to $DETRIX_HOME/auth-token (isolated per executor)
/// 3. Reject unauthenticated requests to protected endpoints
/// 4. Accept requests with the auto-generated token
/// 5. Keep health endpoint public
#[tokio::test]
async fn test_auto_auth_enforces_auth_on_protected_endpoints() {
    let mut executor = AutoAuthTestExecutor::new();

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Use the token captured immediately after daemon started (from isolated temp dir)
    let token = executor
        .auto_token
        .as_ref()
        .expect("Auto-generated token should have been captured by start_daemon");

    // Verify token file permissions on Unix (daemon writes to isolated temp dir)
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let token_path = executor.auth_token_path();
        if let Ok(meta) = std::fs::metadata(&token_path) {
            let mode = meta.mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "Token file should have 0600 permissions, got {:o}",
                mode
            );
        }
    }

    // Protected endpoint should reject unauthenticated requests
    let status = executor
        .request_without_auth("/api/v1/metrics")
        .await
        .expect("Request failed");
    if status != StatusCode::UNAUTHORIZED {
        executor.print_daemon_logs(100);
        panic!(
            "Auto-auth should reject unauthenticated requests. Got: {}",
            status
        );
    }

    // Protected endpoint should accept requests with auto-generated token
    let status = executor
        .request_with_token("/api/v1/metrics", token)
        .await
        .expect("Request failed");
    if status != StatusCode::OK {
        executor.print_daemon_logs(100);
        panic!(
            "Auto-auth should accept the auto-generated token. Got: {}",
            status
        );
    }

    // Health endpoint should still be public
    let status = executor
        .request_without_auth("/health")
        .await
        .expect("Request failed");
    assert_eq!(
        status,
        StatusCode::OK,
        "Health endpoint should remain public with auto-auth"
    );

    // Wrong token should be rejected
    let status = executor
        .request_with_token("/api/v1/metrics", "wrong-token")
        .await
        .expect("Request failed");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "Wrong token should be rejected even with auto-auth"
    );

    println!("✓ Auto-auth enforces authentication on protected endpoints!");
}

/// Test: Auto-auth with DETRIX_TOKEN env var uses the env token.
///
/// When DETRIX_TOKEN is set and no [api.auth] section exists, the daemon should:
/// 1. Use the env var token (not generate a new one)
/// 2. Enforce auth using the env var token
///
/// Note: When DETRIX_TOKEN is set, no token file is written, so there is
/// no cross-binary race to guard against.
#[tokio::test]
async fn test_auto_auth_uses_detrix_token_env_var() {
    let env_token = "my-env-token-for-auto-auth-test";

    let mut executor = AutoAuthTestExecutor::with_env_token(env_token);

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Unauthenticated request should fail
    let status = executor
        .request_without_auth("/api/v1/metrics")
        .await
        .expect("Request failed");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "Should reject unauthenticated request"
    );

    // Request with env token should succeed
    let status = executor
        .request_with_token("/api/v1/metrics", env_token)
        .await
        .expect("Request failed");
    if status != StatusCode::OK {
        executor.print_daemon_logs(100);
        panic!("DETRIX_TOKEN env var should grant access. Got: {}", status);
    }

    // Wrong token should be rejected
    let status = executor
        .request_with_token("/api/v1/metrics", "wrong-token")
        .await
        .expect("Request failed");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "Wrong token should be rejected"
    );

    println!("✓ Auto-auth with DETRIX_TOKEN env var works correctly!");
}

/// Test: Auto-auth also works on gRPC (not just REST).
#[tokio::test]
async fn test_auto_auth_grpc_enforcement() {
    let mut executor = AutoAuthTestExecutor::new();

    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start daemon: {}", e);
    }

    // Use the token captured immediately after daemon started (from isolated temp dir)
    let token = executor
        .auto_token
        .as_ref()
        .expect("Auto-generated token should have been captured by start_daemon");

    let addr = format!("http://127.0.0.1:{}", executor.grpc_port);
    let channel = Channel::from_shared(addr)
        .unwrap()
        .connect()
        .await
        .expect("Failed to connect to gRPC");

    let mut client = MetricsServiceClient::new(channel);

    // gRPC without auth should fail
    let request = Request::new(ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    });
    let result = client.list_metrics(request).await;
    assert!(result.is_err(), "gRPC without auth should be rejected");
    assert_eq!(
        result.unwrap_err().code(),
        tonic::Code::Unauthenticated,
        "Should return Unauthenticated"
    );

    // gRPC with auto-generated token should succeed
    let mut request = Request::new(ListMetricsRequest {
        group: None,
        enabled_only: None,
        name_pattern: None,
        metadata: None,
    });
    request.metadata_mut().insert(
        AUTHORIZATION_METADATA_KEY,
        format!("{}{}", BEARER_PREFIX, token).parse().unwrap(),
    );
    let result = client.list_metrics(request).await;
    assert!(
        result.is_ok(),
        "gRPC with auto-generated token should succeed: {:?}",
        result.err()
    );

    println!("✓ Auto-auth works on gRPC!");
}

/// Regression test: auth-token file must survive a concurrent daemon's lifecycle.
///
/// Scenario that caused a real bug:
/// 1. Production daemon A writes `token_A` to `~/detrix/auth-token`
/// 2. Integration test daemon B starts, overwrites the file with `token_B`
/// 3. Daemon B shuts down — OLD behaviour: deletes the file → daemon A's clients get 401
///                          FIXED behaviour: leaves the file → daemon A's clients get connection-refused (daemon down) not 401
///
/// This test simulates step 3 in isolation by writing a "foreign" token, starting
/// a fresh daemon (which overwrites with its own token), stopping it, then verifying
/// the token file was NOT deleted on shutdown.
///
/// NOTE: the test only checks the post-shutdown file presence; it does NOT start
/// daemon A because we'd need to keep it running across the whole scenario.
#[tokio::test]
async fn test_auto_auth_token_file_not_deleted_on_shutdown() {
    // Create the executor first so we know its isolated DETRIX_HOME path
    let mut executor = AutoAuthTestExecutor::new();
    let token_path = executor.auth_token_path();

    // Pre-condition: write a "production daemon" token to the isolated file.
    // The executor's temp dir already exists; create the parent just in case.
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let production_token = "production-daemon-token-do-not-delete";
    std::fs::write(&token_path, production_token).expect("Failed to write production token");
    assert!(
        token_path.exists(),
        "Token file should exist before test starts"
    );

    // Start a "test" daemon — it overwrites the isolated auth-token with its own token
    if let Err(e) = executor.start_daemon().await {
        executor.print_daemon_logs(100);
        panic!("Failed to start test daemon: {}", e);
    }

    // The file now contains the test daemon's token (overwrote the production token)
    let test_token = executor
        .auto_token
        .as_ref()
        .expect("Test daemon should have written a token");
    let current = std::fs::read_to_string(&token_path).unwrap_or_default();
    assert_eq!(
        current.trim(),
        test_token.as_str(),
        "Test daemon should have overwritten token file with its own token"
    );
    assert_ne!(
        current.trim(),
        production_token,
        "Sanity: production token should have been overwritten"
    );

    // Stop the test daemon — this is where the old code deleted the file
    executor.stop();

    // The fix: file must still exist after daemon shutdown.
    // If this assertion fails, it means the daemon deleted ~/detrix/auth-token on shutdown,
    // which would leave any concurrently-running daemon's clients unable to authenticate.
    assert!(
        token_path.exists(),
        "Auth-token file must NOT be deleted on daemon shutdown — \
         doing so leaves concurrent daemons' clients unable to authenticate (401 instead of connection-refused). \
         Fix: remove the std::fs::remove_file call in serve.rs cleanup section."
    );

    // Clean up
    let _ = std::fs::remove_file(&token_path);
    println!("✓ Auth-token file correctly survived daemon shutdown!");
}

/// Test that credential resolution for a remote daemon returns the configured
/// credential, NOT the local daemon's auto-generated auth-token.
///
/// Regression test for the bug where `is_local_daemon=true` was passed to
/// `resolve_token_for_target` for ALL daemons in the MCP bridge, causing the
/// local daemon's auth-token to be forwarded to remote daemons → 401.
///
/// Fixed by: always pass `is_local_daemon=false` in `switch_daemon`, so the
/// auth-token file fallback is never used for remote/discovered daemons.
#[tokio::test]
#[serial(auto_auth_token_file)]
async fn test_credential_resolution_does_not_leak_local_auth_token_to_remote_daemon() {
    let token_path = detrix_config::paths::auth_token_path();
    let creds_path = detrix_config::paths::credentials_path();

    // Write a local daemon token to the global auth-token file
    let local_token = "local-daemon-auto-token-should-not-leak";
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&token_path, local_token).expect("Failed to write local auth-token");

    // Add a credential specifically for the "remote" daemon
    let remote_hp = "127.0.0.1:18777";
    let remote_token = "remote-daemon-explicit-credential";
    if let Some(parent) = creds_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut creds =
        detrix_config::credentials::CredentialsFile::load_from(&creds_path).unwrap_or_default();
    creds.add(remote_hp, remote_token);
    creds.save_to(&creds_path).unwrap();

    // Case 1: remote daemon with a configured credential → returns the credential
    let resolved = detrix_config::credentials::resolve_token_for_target(remote_hp, false);
    assert_eq!(
        resolved.as_deref(),
        Some(remote_token),
        "Should return the configured credential for the remote daemon"
    );

    // Case 2: remote daemon with NO credential → returns None
    // (NOT the local auth-token file content)
    let unconfigured_hp = "127.0.0.1:18778";
    let resolved_none =
        detrix_config::credentials::resolve_token_for_target(unconfigured_hp, false);
    assert_eq!(
        resolved_none, None,
        "Should return None for unconfigured remote host when is_local_daemon=false"
    );
    assert_ne!(
        resolved_none.as_deref(),
        Some(local_token),
        "Must NOT return local auth-token for unconfigured remote daemon"
    );

    // Cleanup
    creds.remove(remote_hp);
    creds.save_to(&creds_path).unwrap();
    let _ = std::fs::remove_file(&token_path);
    println!(
        "✓ Credential resolution correctly scoped: remote daemon gets credential, not auth-token"
    );
}

/// E2E test: bridge switches to a remote daemon and uses the configured credential.
///
/// Starts two daemons:
/// - Local daemon: auto-auth (bridge connects here initially)
/// - Remote daemon: explicit token `remote-e2e-test-token`
///
/// Verifies that:
/// 1. Using the local token on the remote daemon is rejected (401) — sanity check
/// 2. Using the remote token on the remote daemon succeeds (200)
/// 3. `resolve_token_for_target(remote_hp, false)` returns the credential, not local token
///
/// This is the credential chain that the MCP bridge follows after `switch_daemon`.
#[tokio::test]
#[serial(auto_auth_token_file)]
async fn test_e2e_bridge_uses_credential_after_discovery_switch() {
    let token_path = detrix_config::paths::auth_token_path();
    let creds_path = detrix_config::paths::credentials_path();

    const REMOTE_TOKEN: &str = "remote-e2e-test-token-abcdef123";
    const LOCAL_TOKEN: &str = "local-daemon-auto-token-xyz";

    // Write local daemon token to global auth-token file
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&token_path, LOCAL_TOKEN).expect("Failed to write local auth-token");

    // Start remote daemon with an explicit token
    let mut remote = AutoAuthTestExecutor::with_env_token(REMOTE_TOKEN);
    if let Err(e) = remote.start_daemon().await {
        remote.print_daemon_logs(50);
        let _ = std::fs::remove_file(&token_path);
        panic!("Failed to start remote daemon: {}", e);
    }

    let remote_hp = format!("127.0.0.1:{}", remote.http_port);
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client");

    // Sanity: local token rejected by remote daemon → 401
    let res = http
        .get(format!("http://{}/api/v1/metrics", remote_hp))
        .header(
            AUTHORIZATION_HEADER,
            format!("{}{}", BEARER_PREFIX, LOCAL_TOKEN),
        )
        .send()
        .await
        .expect("request failed");
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "Local auth-token must be rejected by remote daemon (different token)"
    );

    // Remote token accepted → 200
    let res = http
        .get(format!("http://{}/api/v1/metrics", remote_hp))
        .header(
            AUTHORIZATION_HEADER,
            format!("{}{}", BEARER_PREFIX, REMOTE_TOKEN),
        )
        .send()
        .await
        .expect("request failed");
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "Remote token must be accepted by remote daemon"
    );

    // Now simulate what switch_daemon does: resolve_token_for_target(remote_hp, false)
    // Before adding credential → None
    let before = detrix_config::credentials::resolve_token_for_target(&remote_hp, false);
    assert_eq!(
        before, None,
        "Without credential, should return None (not local auth-token)"
    );

    // Add credential for remote daemon
    if let Some(parent) = creds_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut creds =
        detrix_config::credentials::CredentialsFile::load_from(&creds_path).unwrap_or_default();
    creds.add(&remote_hp, REMOTE_TOKEN);
    creds.save_to(&creds_path).unwrap();

    // After adding credential → resolves to remote token
    let after = detrix_config::credentials::resolve_token_for_target(&remote_hp, false);
    assert_eq!(
        after.as_deref(),
        Some(REMOTE_TOKEN),
        "With credential configured, should return the remote token"
    );

    // Confirm: make request using the resolved token → 200
    let resolved_token = after.unwrap();
    let res = http
        .get(format!("http://{}/api/v1/metrics", remote_hp))
        .header(
            AUTHORIZATION_HEADER,
            format!("{}{}", BEARER_PREFIX, resolved_token),
        )
        .send()
        .await
        .expect("request failed");
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "Resolved credential must grant access to remote daemon"
    );

    // Cleanup
    creds.remove(&remote_hp);
    creds.save_to(&creds_path).unwrap();
    let _ = std::fs::remove_file(&token_path);
    remote.stop();

    println!("✓ Bridge credential resolution for remote daemon verified end-to-end!");
}
