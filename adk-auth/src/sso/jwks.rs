//! JWKS (JSON Web Key Set) caching.

#[cfg(feature = "sso")]
use dashmap::DashMap;
#[cfg(feature = "sso")]
use jsonwebtoken::DecodingKey;
#[cfg(feature = "sso")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "sso")]
use std::time::Duration;

use super::TokenError;

/// Cached JWKS keys for token validation.
///
/// Automatically refreshes keys when:
/// - Cache is empty
/// - Refresh interval has passed
/// - A key ID is requested but not found (key rotation)
#[cfg(feature = "sso")]
pub struct JwksCache {
    /// Cached decoding keys by key ID (kid).
    keys: DashMap<String, DecodingKey>,
    /// JWKS endpoint URL.
    jwks_uri: String,
    /// Last refresh timestamp (Unix seconds).
    last_refresh: AtomicU64,
    /// Refresh interval.
    refresh_interval: Duration,
    /// HTTP client.
    client: reqwest::Client,
}

#[cfg(feature = "sso")]
impl JwksCache {
    /// Create a new JWKS cache.
    pub fn new(jwks_uri: impl Into<String>) -> Self {
        Self {
            keys: DashMap::new(),
            jwks_uri: jwks_uri.into(),
            last_refresh: AtomicU64::new(0),
            refresh_interval: Duration::from_secs(3600), // 1 hour
            client: reqwest::Client::new(),
        }
    }

    /// Create with custom refresh interval.
    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.refresh_interval = interval;
        self
    }

    /// Get a decoding key by key ID.
    ///
    /// Refreshes the cache if needed.
    pub async fn get_key(&self, kid: &str) -> Result<DecodingKey, TokenError> {
        // Check if we have the key
        if let Some(key) = self.keys.get(kid) {
            return Ok(key.clone());
        }

        // Key not found, try refreshing
        self.refresh().await?;

        // Check again after refresh
        self.keys
            .get(kid)
            .map(|k| k.clone())
            .ok_or_else(|| TokenError::KeyNotFound(kid.to_string()))
    }

    /// Refresh the JWKS cache.
    pub async fn refresh(&self) -> Result<(), TokenError> {
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

        let last = self.last_refresh.load(Ordering::Relaxed);
        let interval_secs = self.refresh_interval.as_secs();

        // Skip if refreshed recently (unless forced)
        if last > 0 && now - last < interval_secs / 2 {
            return Ok(());
        }

        tracing::debug!("Fetching JWKS from {}", self.jwks_uri);

        let response = self
            .client
            .get(&self.jwks_uri)
            .send()
            .await?
            .error_for_status()
            .map_err(|e| TokenError::JwksFetchError(e.to_string()))?;

        let jwks: Jwks =
            response.json().await.map_err(|e| TokenError::JwksParseError(e.to_string()))?;

        // Clear old keys and add new ones
        self.keys.clear();
        for key in jwks.keys {
            if let Some(kid) = &key.kid {
                if let Ok(decoding_key) = key.to_decoding_key() {
                    self.keys.insert(kid.clone(), decoding_key);
                }
            }
        }

        self.last_refresh.store(now, Ordering::Relaxed);
        tracing::debug!("JWKS cache refreshed with {} keys", self.keys.len());

        Ok(())
    }

    /// Get the number of cached keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Check if cache is empty.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// JWKS response structure.
#[cfg(feature = "sso")]
#[derive(Debug, serde::Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

/// Individual JWK (JSON Web Key).
#[cfg(feature = "sso")]
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct Jwk {
    /// Key type (RSA, EC, etc.)
    kty: String,
    /// Key ID
    kid: Option<String>,
    /// Algorithm
    alg: Option<String>,
    /// Key use (sig, enc)
    #[serde(rename = "use")]
    use_: Option<String>,
    /// RSA modulus
    n: Option<String>,
    /// RSA exponent
    e: Option<String>,
    /// EC x coordinate
    x: Option<String>,
    /// EC y coordinate
    y: Option<String>,
    /// EC curve
    crv: Option<String>,
}

#[cfg(feature = "sso")]
impl Jwk {
    fn to_decoding_key(&self) -> Result<DecodingKey, TokenError> {
        match self.kty.as_str() {
            "RSA" => {
                let n = self.n.as_ref().ok_or_else(|| {
                    TokenError::JwksParseError("Missing 'n' in RSA key".to_string())
                })?;
                let e = self.e.as_ref().ok_or_else(|| {
                    TokenError::JwksParseError("Missing 'e' in RSA key".to_string())
                })?;
                DecodingKey::from_rsa_components(n, e)
                    .map_err(|e| TokenError::JwksParseError(e.to_string()))
            }
            "EC" => {
                let x = self.x.as_ref().ok_or_else(|| {
                    TokenError::JwksParseError("Missing 'x' in EC key".to_string())
                })?;
                let y = self.y.as_ref().ok_or_else(|| {
                    TokenError::JwksParseError("Missing 'y' in EC key".to_string())
                })?;
                DecodingKey::from_ec_components(x, y)
                    .map_err(|e| TokenError::JwksParseError(e.to_string()))
            }
            _ => Err(TokenError::UnsupportedAlgorithm(self.kty.clone())),
        }
    }
}

// Stub for when SSO is not enabled
#[cfg(not(feature = "sso"))]
pub struct JwksCache;

#[cfg(not(feature = "sso"))]
impl JwksCache {
    pub fn new(_jwks_uri: impl Into<String>) -> Self {
        Self
    }
}
