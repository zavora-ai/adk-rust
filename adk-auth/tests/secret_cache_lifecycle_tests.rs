//! The secret cache must bound what it holds and let a caller drop it early.
//!
//! `CachedSecretProvider` stored values in an unbounded `HashMap` and checked the TTL
//! only when the same name was read again. Expired entries were never removed, there
//! was no way to invalidate a rotated secret before its TTL elapsed, there was no
//! capacity limit, and values were not cleared on drop. TTL therefore controlled what
//! the cache *returned* while residency in process memory lasted for the lifetime of
//! the process.

use adk_auth::secrets::cached::CachedSecretProvider;
use adk_auth::secrets::provider::SecretProvider;
use adk_core::AdkError;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Counts fetches so cache hits and misses are observable.
struct CountingProvider {
    fetches: Arc<AtomicUsize>,
    value: String,
}

impl CountingProvider {
    fn new(value: &str) -> Self {
        Self { fetches: Arc::new(AtomicUsize::new(0)), value: value.to_string() }
    }
}

#[async_trait]
impl SecretProvider for CountingProvider {
    async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        Ok(format!("{}-{name}", self.value))
    }
}

#[tokio::test(start_paused = true)]
async fn a_cached_value_is_served_without_a_second_fetch() {
    let inner = CountingProvider::new("secret");
    let fetches = inner.fetches.clone();
    let cache = CachedSecretProvider::new(inner, Duration::from_secs(300));

    let first = cache.get_secret("api-key").await.unwrap();
    let second = cache.get_secret("api-key").await.unwrap();

    assert_eq!(first, second);
    assert_eq!(fetches.load(Ordering::SeqCst), 1, "the second read must come from the cache");
}

#[tokio::test(start_paused = true)]
async fn an_expired_entry_is_purged_rather_than_lingering() {
    let cache = CachedSecretProvider::new(CountingProvider::new("secret"), Duration::from_secs(60));
    cache.get_secret("api-key").await.unwrap();
    assert_eq!(cache.len().await, 1);

    tokio::time::advance(Duration::from_secs(61)).await;

    // Expiry previously became visible only when the same name was read again, so the
    // value stayed allocated indefinitely for a name that was never requested twice.
    assert_eq!(cache.purge_expired().await, 1, "the expired entry must be removable");
    assert!(cache.is_empty().await, "nothing must remain after a purge");
}

#[tokio::test(start_paused = true)]
async fn an_expired_entry_is_refetched_not_served() {
    let inner = CountingProvider::new("secret");
    let fetches = inner.fetches.clone();
    let cache = CachedSecretProvider::new(inner, Duration::from_secs(60));

    cache.get_secret("api-key").await.unwrap();
    tokio::time::advance(Duration::from_secs(61)).await;
    cache.get_secret("api-key").await.unwrap();

    assert_eq!(fetches.load(Ordering::SeqCst), 2, "an expired value must not be served");
}

#[tokio::test(start_paused = true)]
async fn a_rotated_secret_can_be_dropped_before_its_ttl() {
    let inner = CountingProvider::new("secret");
    let fetches = inner.fetches.clone();
    let cache = CachedSecretProvider::new(inner, Duration::from_secs(3600));

    cache.get_secret("api-key").await.unwrap();
    cache.invalidate("api-key").await;
    cache.get_secret("api-key").await.unwrap();

    assert_eq!(
        fetches.load(Ordering::SeqCst),
        2,
        "an invalidated secret must be fetched again rather than served for the rest of its TTL"
    );
    assert_eq!(cache.len().await, 1);
}

#[tokio::test(start_paused = true)]
async fn invalidate_all_clears_every_entry() {
    let cache =
        CachedSecretProvider::new(CountingProvider::new("secret"), Duration::from_secs(3600));
    for name in ["a", "b", "c"] {
        cache.get_secret(name).await.unwrap();
    }
    assert_eq!(cache.len().await, 3);

    cache.invalidate_all().await;
    assert!(cache.is_empty().await);
}

#[tokio::test(start_paused = true)]
async fn the_cache_is_bounded() {
    let cache =
        CachedSecretProvider::new(CountingProvider::new("secret"), Duration::from_secs(3600))
            .with_max_entries(4);

    for index in 0..50 {
        cache.get_secret(&format!("secret-{index}")).await.unwrap();
    }

    let held = cache.len().await;
    assert!(held <= 4, "the cache grew to {held} entries against a capacity of 4");
}

#[tokio::test(start_paused = true)]
async fn eviction_keeps_the_recently_used_entry() {
    let inner = CountingProvider::new("secret");
    let fetches = inner.fetches.clone();
    let cache = CachedSecretProvider::new(inner, Duration::from_secs(3600)).with_max_entries(2);

    cache.get_secret("keep").await.unwrap();
    cache.get_secret("evict").await.unwrap();
    // Touch "keep" so it is the more recently used of the two.
    cache.get_secret("keep").await.unwrap();
    let fetches_before = fetches.load(Ordering::SeqCst);

    cache.get_secret("newcomer").await.unwrap();

    // "keep" must still be cached; reading it costs no fetch.
    cache.get_secret("keep").await.unwrap();
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        fetches_before + 1,
        "the recently used entry was evicted instead of the stale one"
    );
}

#[tokio::test(start_paused = true)]
async fn a_zero_capacity_cache_never_caches() {
    let inner = CountingProvider::new("secret");
    let fetches = inner.fetches.clone();
    let cache = CachedSecretProvider::new(inner, Duration::from_secs(3600)).with_max_entries(0);

    cache.get_secret("api-key").await.unwrap();
    cache.get_secret("api-key").await.unwrap();

    assert_eq!(fetches.load(Ordering::SeqCst), 2);
    assert!(cache.is_empty().await);
}

#[tokio::test(start_paused = true)]
async fn debug_output_carries_no_secret_value() {
    let cache =
        CachedSecretProvider::new(CountingProvider::new("top-secret"), Duration::from_secs(3600));
    let value = cache.get_secret("api-key").await.unwrap();
    assert!(value.contains("top-secret"), "the fixture must actually hold a secret");

    let rendered = format!("{cache:?}");
    assert!(
        !rendered.contains("top-secret"),
        "a debug print exposed a cached secret value: {rendered}"
    );
}
