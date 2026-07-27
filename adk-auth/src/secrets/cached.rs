//! Cached secret provider wrapper.
//!
//! [`CachedSecretProvider`] wraps any [`SecretProvider`] with an in-memory cache
//! that respects a configurable TTL, a capacity bound, and explicit revocation.
//!
//! # Threat model
//!
//! A TTL controls what the cache *returns*, not how long a value stays in process
//! memory. This cache drops and zeroizes an entry when it expires, is evicted, or is
//! invalidated, which shortens residency to roughly the TTL rather than the process
//! lifetime. It cannot guarantee erasure: a `String` may have been reallocated,
//! copied by the allocator, swapped to disk, or captured in a core dump before the
//! zeroization runs. Treat it as reducing the window, not closing it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::AdkError;
use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio::time::Instant;
use zeroize::Zeroize;

use super::provider::SecretProvider;

/// Default number of distinct secret names held at once.
pub const DEFAULT_MAX_ENTRIES: usize = 128;

/// A cached secret value that is zeroized when dropped.
struct CachedEntry {
    value: String,
    expires_at: Instant,
    /// Monotonic read sequence, used to choose an eviction victim.
    ///
    /// A counter rather than a timestamp: two reads can share an `Instant`, which
    /// would leave the victim to be decided by hash order.
    last_access: u64,
}

impl CachedEntry {
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at <= now
    }
}

impl Drop for CachedEntry {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

/// Wraps a [`SecretProvider`] with a bounded in-memory cache.
///
/// Cached values are returned within the configured TTL. After expiry the inner
/// provider is called again and the cache is refreshed. Expired entries are removed
/// on the next write rather than lingering until their name is requested again, and
/// the cache never holds more than its capacity.
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::secrets::{CachedSecretProvider, SecretProvider};
/// use std::time::Duration;
///
/// let cached = CachedSecretProvider::new(inner_provider, Duration::from_secs(300))
///     .with_max_entries(32);
/// let secret = cached.get_secret("my-key").await?;
///
/// // A rotated secret can be dropped before its TTL elapses.
/// cached.invalidate("my-key").await;
/// ```
pub struct CachedSecretProvider<P: SecretProvider> {
    inner: P,
    cache: Arc<RwLock<HashMap<String, CachedEntry>>>,
    ttl: Duration,
    max_entries: usize,
    /// Hands out the read sequence numbers used for eviction ordering.
    access_counter: std::sync::atomic::AtomicU64,
}

impl<P: SecretProvider> CachedSecretProvider<P> {
    /// Create a new cached provider wrapping `inner` with the given TTL.
    pub fn new(inner: P, ttl: Duration) -> Self {
        Self {
            inner,
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl,
            max_entries: DEFAULT_MAX_ENTRIES,
            access_counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Set how many distinct secret names may be cached at once.
    ///
    /// When the cache is full the least recently used entry is dropped. A capacity of
    /// zero disables caching. Without a bound, code that derives secret names from
    /// input can grow the cache for the lifetime of the process.
    #[must_use]
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }

    /// Drop a single cached secret, zeroizing its value.
    ///
    /// Call this when a secret is rotated or revoked so the old value is not served
    /// for the remainder of its TTL.
    pub async fn invalidate(&self, name: &str) {
        self.cache.write().await.remove(name);
    }

    /// Drop every cached secret, zeroizing the values.
    pub async fn invalidate_all(&self) {
        self.cache.write().await.clear();
    }

    /// Drop every expired entry and return how many were removed.
    ///
    /// Expiry is otherwise noticed only when the same name is read again, so this is
    /// what a caller uses to bound residency without waiting for traffic.
    pub async fn purge_expired(&self) -> usize {
        let now = Instant::now();
        let mut cache = self.cache.write().await;
        let before = cache.len();
        cache.retain(|_, entry| !entry.is_expired(now));
        before - cache.len()
    }

    /// Number of entries currently held, expired or not.
    pub async fn len(&self) -> usize {
        self.cache.read().await.len()
    }

    /// Whether the cache holds no entries.
    pub async fn is_empty(&self) -> bool {
        self.cache.read().await.is_empty()
    }

    /// The next read sequence number.
    fn next_access(&self) -> u64 {
        self.access_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Insert a freshly fetched value, purging expired entries and enforcing capacity.
    async fn store(&self, name: &str, value: &str) {
        if self.max_entries == 0 {
            return;
        }
        let now = Instant::now();
        let mut cache = self.cache.write().await;
        cache.retain(|_, entry| !entry.is_expired(now));

        while cache.len() >= self.max_entries {
            // Least recently used victim. The cache is small by construction, so a
            // scan costs less than maintaining a separate ordering structure.
            let victim = cache
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(name, _)| name.clone());
            match victim {
                Some(victim) => {
                    cache.remove(&victim);
                }
                None => break,
            }
        }

        cache.insert(
            name.to_string(),
            CachedEntry {
                value: value.to_string(),
                expires_at: now + self.ttl,
                last_access: self.next_access(),
            },
        );
    }
}

/// Redacts cached values so a debug print cannot leak a secret.
impl<P: SecretProvider> std::fmt::Debug for CachedSecretProvider<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedSecretProvider")
            .field("ttl", &self.ttl)
            .field("max_entries", &self.max_entries)
            .field("cache", &"<redacted>")
            .finish()
    }
}

#[async_trait]
impl<P: SecretProvider> SecretProvider for CachedSecretProvider<P> {
    async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
        // Check the cache first, recording the read so eviction can pick a victim.
        {
            let mut cache = self.cache.write().await;
            let now = Instant::now();
            if let Some(entry) = cache.get_mut(name) {
                if entry.is_expired(now) {
                    cache.remove(name);
                } else {
                    entry.last_access = self.next_access();
                    return Ok(entry.value.clone());
                }
            }
        }

        let value = self.inner.get_secret(name).await?;
        self.store(name, &value).await;
        Ok(value)
    }
}
