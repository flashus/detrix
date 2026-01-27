//! JWT Validator for External Authentication
//!
//! Validates JWTs using JWKS (JSON Web Key Set) from an external identity provider.
//! Supports RS256 algorithm commonly used by OAuth2/OIDC providers.
//!
//! Features:
//! - JWKS caching with configurable TTL
//! - Automatic key refresh on cache miss
//! - Issuer and audience validation
//! - Thread-safe for concurrent validation

use detrix_config::JwtConfig;
use jsonwebtoken::{
    decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, TokenData, Validation,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Errors that can occur during JWT validation
#[derive(Debug, Error)]
pub enum JwtError {
    #[error("JWKS URL not configured")]
    JwksUrlNotConfigured,

    #[error("Failed to fetch JWKS: {0}")]
    FetchError(String),

    #[error("Failed to parse JWKS: {0}")]
    ParseError(String),

    #[error("Key not found for kid: {0}")]
    KeyNotFound(String),

    #[error("Invalid token header: {0}")]
    InvalidHeader(String),

    #[error("Token validation failed: {0}")]
    ValidationFailed(String),

    #[error("Unsupported algorithm: {0:?}")]
    UnsupportedAlgorithm(Algorithm),
}

impl From<reqwest::Error> for JwtError {
    fn from(err: reqwest::Error) -> Self {
        JwtError::FetchError(err.to_string())
    }
}

impl From<serde_json::Error> for JwtError {
    fn from(err: serde_json::Error) -> Self {
        JwtError::ParseError(err.to_string())
    }
}

/// JWT claims extracted after validation
///
/// Standard claims plus common custom claims from identity providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    /// Subject (user ID)
    pub sub: Option<String>,
    /// Issuer
    pub iss: Option<String>,
    /// Audience
    pub aud: Option<Audience>,
    /// Expiration time (Unix timestamp)
    pub exp: Option<u64>,
    /// Issued at (Unix timestamp)
    pub iat: Option<u64>,
    /// Not before (Unix timestamp)
    pub nbf: Option<u64>,
    /// JWT ID
    pub jti: Option<String>,
    /// Email (common in OIDC)
    pub email: Option<String>,
    /// Name (common in OIDC)
    pub name: Option<String>,
    /// Roles (common in enterprise SSO)
    #[serde(default)]
    pub roles: Vec<String>,
    /// Scopes (common in OAuth2)
    #[serde(default)]
    pub scope: Option<String>,
}

/// Audience can be a single string or array of strings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Audience {
    Single(String),
    Multiple(Vec<String>),
}

impl Audience {
    /// Check if audience contains a specific value
    pub fn contains(&self, value: &str) -> bool {
        match self {
            Audience::Single(s) => s == value,
            Audience::Multiple(v) => v.iter().any(|s| s == value),
        }
    }
}

/// Cached JWKS keys with expiration
struct JwksCache {
    /// Decoded keys by key ID (kid)
    keys: HashMap<String, DecodingKey>,
    /// When the cache was last refreshed
    fetched_at: Instant,
}

impl JwksCache {
    fn new() -> Self {
        Self {
            keys: HashMap::new(),
            fetched_at: Instant::now() - Duration::from_secs(3600), // Start expired
        }
    }

    fn is_expired(&self, ttl: Duration) -> bool {
        self.fetched_at.elapsed() > ttl
    }
}

/// JWKS-based JWT validator
///
/// Validates JWTs by fetching public keys from a JWKS endpoint.
/// Keys are cached to reduce latency and load on the identity provider.
///
/// # Example
///
/// ```ignore
/// let config = JwtConfig {
///     jwks_url: Some("https://auth.example.com/.well-known/jwks.json".to_string()),
///     issuer: Some("https://auth.example.com".to_string()),
///     audience: Some("detrix".to_string()),
///     cache_ttl_seconds: 300,
/// };
///
/// let validator = JwksValidator::new(config).await?;
/// let claims = validator.validate_token("eyJ...").await?;
/// ```
pub struct JwksValidator {
    /// JWKS endpoint URL
    jwks_url: String,
    /// Expected issuer (optional)
    issuer: Option<String>,
    /// Expected audience (optional)
    audience: Option<String>,
    /// Cache TTL
    cache_ttl: Duration,
    /// Cached keys (thread-safe)
    cache: Arc<RwLock<JwksCache>>,
    /// HTTP client for JWKS fetching
    client: reqwest::Client,
}

impl JwksValidator {
    /// Create a new JWKS validator from config
    ///
    /// Does NOT fetch keys immediately - keys are fetched lazily on first validation.
    pub fn new(config: &JwtConfig) -> Result<Self, JwtError> {
        let jwks_url = config
            .jwks_url
            .clone()
            .ok_or(JwtError::JwksUrlNotConfigured)?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        info!(
            jwks_url = %jwks_url,
            issuer = ?config.issuer,
            audience = ?config.audience,
            cache_ttl_seconds = config.cache_ttl_seconds,
            "Creating JWKS validator"
        );

        Ok(Self {
            jwks_url,
            issuer: config.issuer.clone(),
            audience: config.audience.clone(),
            cache_ttl: Duration::from_secs(config.cache_ttl_seconds),
            cache: Arc::new(RwLock::new(JwksCache::new())),
            client,
        })
    }

    /// Validate a JWT token and return claims
    ///
    /// This will:
    /// 1. Extract the key ID (kid) from the token header
    /// 2. Fetch/refresh JWKS keys if cache is stale or key not found
    /// 3. Validate the token signature, expiration, issuer, and audience
    /// 4. Return the decoded claims
    pub async fn validate_token(&self, token: &str) -> Result<JwtClaims, JwtError> {
        // Extract header to get kid
        let header = decode_header(token).map_err(|e| JwtError::InvalidHeader(e.to_string()))?;

        let kid = header
            .kid
            .ok_or_else(|| JwtError::InvalidHeader("Missing kid in token header".to_string()))?;

        // Check algorithm - we only support RS256 for security
        if header.alg != Algorithm::RS256 {
            return Err(JwtError::UnsupportedAlgorithm(header.alg));
        }

        // Get decoding key (with cache refresh if needed)
        let decoding_key = self.get_decoding_key(&kid).await?;

        // Build validation parameters
        let mut validation = Validation::new(Algorithm::RS256);

        // Configure issuer validation
        if let Some(ref issuer) = self.issuer {
            validation.set_issuer(&[issuer]);
        }
        // Note: If issuer is not set, validation.iss remains None and issuer is not validated

        // Configure audience validation
        if let Some(ref audience) = self.audience {
            validation.set_audience(&[audience]);
        }
        // Note: If audience is not set, validation.aud remains None and audience is not validated

        // Validate and decode token
        let token_data: TokenData<JwtClaims> = decode(token, &decoding_key, &validation)
            .map_err(|e| JwtError::ValidationFailed(e.to_string()))?;

        debug!(
            sub = ?token_data.claims.sub,
            email = ?token_data.claims.email,
            "JWT validation successful"
        );

        Ok(token_data.claims)
    }

    /// Get decoding key for a kid, refreshing cache if needed
    async fn get_decoding_key(&self, kid: &str) -> Result<DecodingKey, JwtError> {
        // Try to get from cache first
        {
            let cache = self.cache.read().await;
            if !cache.is_expired(self.cache_ttl) {
                if let Some(key) = cache.keys.get(kid) {
                    debug!(kid = %kid, "Using cached decoding key");
                    return Ok(key.clone());
                }
            }
        }

        // Cache miss or expired - refresh keys
        self.refresh_keys().await?;

        // Try again after refresh
        let cache = self.cache.read().await;
        cache
            .keys
            .get(kid)
            .cloned()
            .ok_or_else(|| JwtError::KeyNotFound(kid.to_string()))
    }

    /// Fetch and cache JWKS keys
    async fn refresh_keys(&self) -> Result<(), JwtError> {
        info!(jwks_url = %self.jwks_url, "Refreshing JWKS keys");

        // Fetch JWKS
        let response = self.client.get(&self.jwks_url).send().await?;

        if !response.status().is_success() {
            return Err(JwtError::FetchError(format!(
                "JWKS endpoint returned status: {}",
                response.status()
            )));
        }

        let jwks_text = response.text().await?;

        let jwks: JwkSet = serde_json::from_str(&jwks_text)?;

        // Parse keys into DecodingKey
        let mut keys = HashMap::new();
        for jwk in &jwks.keys {
            if let Some(kid) = &jwk.common.key_id {
                match DecodingKey::from_jwk(jwk) {
                    Ok(key) => {
                        debug!(kid = %kid, "Loaded JWK");
                        keys.insert(kid.clone(), key);
                    }
                    Err(e) => {
                        warn!(kid = %kid, error = %e, "Failed to parse JWK, skipping");
                    }
                }
            }
        }

        if keys.is_empty() {
            return Err(JwtError::ParseError(
                "No valid keys found in JWKS".to_string(),
            ));
        }

        info!(key_count = keys.len(), "JWKS keys refreshed");

        // Update cache
        let mut cache = self.cache.write().await;
        cache.keys = keys;
        cache.fetched_at = Instant::now();

        Ok(())
    }

    /// Force refresh of JWKS keys
    ///
    /// Useful for key rotation scenarios where you want to immediately
    /// fetch new keys without waiting for cache expiration.
    pub async fn force_refresh(&self) -> Result<(), JwtError> {
        self.refresh_keys().await
    }

    /// Check if the validator has any cached keys
    pub async fn has_cached_keys(&self) -> bool {
        let cache = self.cache.read().await;
        !cache.keys.is_empty()
    }

    /// Get the number of cached keys
    pub async fn cached_key_count(&self) -> usize {
        let cache = self.cache.read().await;
        cache.keys.len()
    }
}

impl std::fmt::Debug for JwksValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwksValidator")
            .field("jwks_url", &self.jwks_url)
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("cache_ttl", &self.cache_ttl)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwt_error_display() {
        let err = JwtError::JwksUrlNotConfigured;
        assert_eq!(err.to_string(), "JWKS URL not configured");

        let err = JwtError::KeyNotFound("abc123".to_string());
        assert_eq!(err.to_string(), "Key not found for kid: abc123");
    }

    #[test]
    fn test_audience_contains() {
        let single = Audience::Single("detrix".to_string());
        assert!(single.contains("detrix"));
        assert!(!single.contains("other"));

        let multiple = Audience::Multiple(vec!["detrix".to_string(), "api".to_string()]);
        assert!(multiple.contains("detrix"));
        assert!(multiple.contains("api"));
        assert!(!multiple.contains("other"));
    }

    #[test]
    fn test_jwks_cache_expiration() {
        let cache = JwksCache::new();
        // New cache should be expired (we set it to 1 hour ago)
        assert!(cache.is_expired(Duration::from_secs(60)));
    }

    #[test]
    fn test_validator_creation_missing_url() {
        let config = JwtConfig::default();
        let result = JwksValidator::new(&config);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            JwtError::JwksUrlNotConfigured
        ));
    }

    #[test]
    fn test_validator_creation_with_url() {
        let config = JwtConfig {
            jwks_url: Some("https://example.com/.well-known/jwks.json".to_string()),
            issuer: Some("https://example.com".to_string()),
            audience: Some("detrix".to_string()),
            cache_ttl_seconds: 300,
        };
        let result = JwksValidator::new(&config);
        assert!(result.is_ok());

        let validator = result.unwrap();
        assert_eq!(
            validator.jwks_url,
            "https://example.com/.well-known/jwks.json"
        );
        assert_eq!(validator.issuer, Some("https://example.com".to_string()));
        assert_eq!(validator.audience, Some("detrix".to_string()));
    }

    #[test]
    fn test_jwt_claims_deserialization() {
        let json = r#"{
            "sub": "user123",
            "iss": "https://auth.example.com",
            "aud": "detrix",
            "exp": 1735560000,
            "email": "user@example.com",
            "roles": ["admin", "user"]
        }"#;

        let claims: JwtClaims = serde_json::from_str(json).unwrap();
        assert_eq!(claims.sub, Some("user123".to_string()));
        assert_eq!(claims.email, Some("user@example.com".to_string()));
        assert_eq!(claims.roles, vec!["admin", "user"]);
    }

    #[test]
    fn test_jwt_claims_audience_single() {
        let json = r#"{"aud": "detrix"}"#;
        let claims: JwtClaims = serde_json::from_str(json).unwrap();
        assert!(claims.aud.is_some());
        assert!(claims.aud.unwrap().contains("detrix"));
    }

    #[test]
    fn test_jwt_claims_audience_multiple() {
        let json = r#"{"aud": ["detrix", "api"]}"#;
        let claims: JwtClaims = serde_json::from_str(json).unwrap();
        assert!(claims.aud.is_some());
        let aud = claims.aud.unwrap();
        assert!(aud.contains("detrix"));
        assert!(aud.contains("api"));
    }
}
