//! JWT Testing Utilities for External Auth Mode E2E Tests
//!
//! Provides utilities for testing external JWT authentication:
//! - RSA key pair generation
//! - JWT token creation with customizable claims
//! - Mock JWKS server for testing

use axum::{routing::get, Json, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rand::rngs::OsRng;
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// RSA key pair for JWT signing and verification
pub struct JwtKeyPair {
    /// Private key for signing JWTs
    private_key: RsaPrivateKey,
    /// Key ID used in JWT header and JWKS
    pub kid: String,
}

impl JwtKeyPair {
    /// Generate a new RSA key pair
    pub fn generate() -> Self {
        let mut rng = OsRng;
        let bits = 2048;
        let private_key = RsaPrivateKey::new(&mut rng, bits).expect("Failed to generate RSA key");
        let kid = format!("test-key-{}", rand::random::<u32>());

        Self { private_key, kid }
    }

    /// Get the private key in PEM format for JWT encoding
    pub fn encoding_key(&self) -> EncodingKey {
        let pem = self
            .private_key
            .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .expect("Failed to encode private key");
        EncodingKey::from_rsa_pem(pem.as_bytes()).expect("Failed to create encoding key")
    }

    /// Get the public key components for JWKS
    pub fn jwk(&self) -> Jwk {
        let public_key = self.private_key.to_public_key();

        // Get the modulus and exponent from the public key
        let n = public_key.n();
        let e = public_key.e();

        // Encode as base64url (no padding)
        let n_bytes = n.to_bytes_be();
        let e_bytes = e.to_bytes_be();

        Jwk {
            kty: "RSA".to_string(),
            alg: "RS256".to_string(),
            use_: "sig".to_string(),
            kid: self.kid.clone(),
            n: URL_SAFE_NO_PAD.encode(&n_bytes),
            e: URL_SAFE_NO_PAD.encode(&e_bytes),
        }
    }
}

/// JSON Web Key for JWKS response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub alg: String,
    #[serde(rename = "use")]
    pub use_: String,
    pub kid: String,
    pub n: String,
    pub e: String,
}

/// JSON Web Key Set response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

/// JWT claims for test tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestClaims {
    /// Subject (user ID)
    pub sub: String,
    /// Issuer
    pub iss: String,
    /// Audience
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
    /// Expiration time (Unix timestamp)
    pub exp: u64,
    /// Issued at (Unix timestamp)
    pub iat: u64,
    /// Email (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Name (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl TestClaims {
    /// Create new claims with default expiration (1 hour)
    pub fn new(sub: &str, iss: &str) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            sub: sub.to_string(),
            iss: iss.to_string(),
            aud: None,
            exp: now + 3600, // 1 hour
            iat: now,
            email: None,
            name: None,
        }
    }

    /// Set audience
    pub fn with_audience(mut self, aud: &str) -> Self {
        self.aud = Some(aud.to_string());
        self
    }

    /// Set email
    pub fn with_email(mut self, email: &str) -> Self {
        self.email = Some(email.to_string());
        self
    }

    /// Set name
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Create expired claims (for testing token rejection)
    pub fn expired(sub: &str, iss: &str) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            sub: sub.to_string(),
            iss: iss.to_string(),
            aud: None,
            exp: now - 3600, // 1 hour ago (expired)
            iat: now - 7200, // 2 hours ago
            email: None,
            name: None,
        }
    }
}

/// Builder for creating test JWTs
pub struct JwtBuilder<'a> {
    key_pair: &'a JwtKeyPair,
    claims: TestClaims,
}

impl<'a> JwtBuilder<'a> {
    /// Create a new JWT builder
    pub fn new(key_pair: &'a JwtKeyPair, claims: TestClaims) -> Self {
        Self { key_pair, claims }
    }

    /// Build and sign the JWT
    pub fn build(self) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.key_pair.kid.clone());

        encode(&header, &self.claims, &self.key_pair.encoding_key()).expect("Failed to encode JWT")
    }
}

/// Mock JWKS server for testing
pub struct MockJwksServer {
    /// Server address
    pub addr: SocketAddr,
    /// Shutdown signal sender
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Server task handle
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl MockJwksServer {
    /// Start a new mock JWKS server
    pub async fn start(key_pair: &JwtKeyPair) -> Self {
        Self::start_with_keys(vec![key_pair.jwk()]).await
    }

    /// Start with multiple keys (for key rotation testing)
    pub async fn start_with_keys(keys: Vec<Jwk>) -> Self {
        let jwks = JwkSet { keys };
        let jwks = Arc::new(jwks);

        let app = Router::new().route(
            "/.well-known/jwks.json",
            get({
                let jwks = Arc::clone(&jwks);
                move || {
                    let jwks = Arc::clone(&jwks);
                    async move { Json((*jwks).clone()) }
                }
            }),
        );

        // Find an available port
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind");
        let addr = listener.local_addr().expect("Failed to get local addr");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("Server error");
        });

        // Give the server a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            handle: Some(handle),
        }
    }

    /// Get the JWKS URL for this server
    pub fn jwks_url(&self) -> String {
        format!("http://{}/.well-known/jwks.json", self.addr)
    }

    /// Get the issuer URL (same as base URL for simplicity)
    pub fn issuer(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stop the server
    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for MockJwksServer {
    fn drop(&mut self) {
        // Send shutdown signal if not already sent
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_pair_generation() {
        let key_pair = JwtKeyPair::generate();
        assert!(!key_pair.kid.is_empty());

        let jwk = key_pair.jwk();
        assert_eq!(jwk.kty, "RSA");
        assert_eq!(jwk.alg, "RS256");
        assert_eq!(jwk.kid, key_pair.kid);
    }

    #[test]
    fn test_jwt_creation() {
        let key_pair = JwtKeyPair::generate();
        let claims = TestClaims::new("user123", "https://auth.example.com")
            .with_audience("detrix")
            .with_email("user@example.com");

        let token = JwtBuilder::new(&key_pair, claims).build();
        assert!(!token.is_empty());

        // Token should have 3 parts
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn test_expired_claims() {
        let claims = TestClaims::expired("user123", "https://auth.example.com");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(claims.exp < now);
    }

    #[tokio::test]
    async fn test_mock_jwks_server() {
        let key_pair = JwtKeyPair::generate();
        let server = MockJwksServer::start(&key_pair).await;

        // Fetch JWKS
        let client = reqwest::Client::new();
        let resp = client
            .get(server.jwks_url())
            .send()
            .await
            .expect("Failed to fetch JWKS");

        assert!(resp.status().is_success());

        let jwks: JwkSet = resp.json().await.expect("Failed to parse JWKS");
        assert_eq!(jwks.keys.len(), 1);
        assert_eq!(jwks.keys[0].kid, key_pair.kid);

        server.stop().await;
    }
}
