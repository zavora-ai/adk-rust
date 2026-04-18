//! Cached secret provider wrapper.
//!
//! [`CachedSecretProvider`] wraps any [`SecretProvider`] with an in-memory
//! cache that respects a configurable TTL.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::AdkError;
use async_trait::async_trait;
use tokio::sync::RwLock;

use super::provider::SecretProvider;

/// A cached entry with expiration tracking.
struct CachedEntry {
    value: String,
    expires_at: Instant,
}

/// Wraps a [`SecretProvider`] with an in-memory cache.
///
/// Cached values are returned within the configured TTL. After expiry,
/// the inner provider is called again and the cache is refreshed.
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::secrets::{CachedSecretProvider, SecretProvider};
/// use std::time::Duration;
///
/// let cached = CachedSecretProvider::new(inner_provider, Duration::from_secs(300));
/// let secret = cached.get_secret("my-key").await?;
/// ```
pub struct CachedSecretProvider<P: SecretProvider> {
    inner: P,
    cache: Arc<RwLock<HashMap<String, CachedEntry>>>,
    ttl: Duration,
}

impl<P: SecretProvider> CachedSecretProvider<P> {
    /// Create a new cached provider wrapping `inner` with the given TTL.
    pub fn new(inner: P, ttl: Duration) -> Self {
        Self { inner, cache: Arc::new(RwLock::new(HashMap::new())), ttl }
    }
}

#[async_trait]
impl<P: SecretProvider> SecretProvider for CachedSecretProvider<P> {
    async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(name) {
                if entry.expires_at > Instant::now() {
                    return Ok(entry.value.clone());
                }
            }
        }

        // Cache miss or expired — fetch from inner provider
        let value = self.inner.get_secret(name).await?;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(
                name.to_string(),
                CachedEntry { value: value.clone(), expires_at: Instant::now() + self.ttl },
            );
        }

        Ok(value)
    }
}
