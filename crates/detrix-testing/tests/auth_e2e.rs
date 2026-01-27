//! E2E tests for authentication
//!
//! Tests the simplified auth model:
//! - Simple mode: Static bearer token from config (like Prometheus)
//! - External mode: JWT validation via JWKS endpoint (for enterprise SSO)
//! - Disabled mode: No authentication required
//!
//! Run with: `cargo test --package detrix-testing --test auth_e2e -- --test-threads=1`

use reqwest::{Client, StatusCode};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tempfile::TempDir;

use detrix_api::generated::detrix::v1::{
    metrics_service_client::MetricsServiceClient, ListMetricsRequest,
};
use detrix_api::grpc::{AUTHORIZATION_METADATA_KEY, BEARER_PREFIX};
use detrix_testing::e2e::executor::find_detrix_binary;
use detrix_testing::e2e::jwt::{JwtBuilder, JwtKeyPair, MockJwksServer, TestClaims};
use tonic::transport::Channel;
use tonic::Request;

/// Global port counter for unique ports
static PORT_COUNTER: AtomicU16 = AtomicU16::new(0);

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
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| manifest_dir.clone());

        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Use unique ports for this test
        let counter = PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
        let base_port = 19000 + ((std::process::id() as u16 + counter) % 500);
        let http_port = base_port;
        let grpc_port = base_port + 1000;

        let daemon_log_path = temp_dir.path().join("daemon.log");

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            temp_dir,
            http_port,
            grpc_port,
            daemon_process: None,
            daemon_log_path,
            workspace_root,
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
port_fallback = true

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
            .header("Authorization", format!("Bearer {}", token))
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

/// Wait for TCP port
async fn wait_for_port(port: u16, timeout_secs: u64) -> bool {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout {
        let output = Command::new("lsof")
            .args(["-i", &format!(":{}", port), "-sTCP:LISTEN"])
            .output();

        if let Ok(out) = output {
            if out.status.success() && !out.stdout.is_empty() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
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
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| manifest_dir.clone());

        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Use unique ports for this test
        let counter = PORT_COUNTER.fetch_add(1, Ordering::SeqCst);
        let base_port = 19500 + ((std::process::id() as u16 + counter) % 500);
        let http_port = base_port;
        let grpc_port = base_port + 1000;

        let daemon_log_path = temp_dir.path().join("daemon.log");

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
            temp_dir,
            http_port,
            grpc_port,
            daemon_process: None,
            daemon_log_path,
            workspace_root,
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
port_fallback = true

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
            .header("Authorization", format!("Bearer {}", token))
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
