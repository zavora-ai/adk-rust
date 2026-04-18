//! Property tests for CachedSecretProvider TTL behavior.
//!
//! **Feature: competitive-parity-v070, Property 10: Secret Cache Respects TTL**
//! *For any* secret name and value, after caching with TTL `t`, calling `get_secret`
//! within `t` SHALL return the cached value without calling the inner provider, and
//! calling `get_secret` after `t` has elapsed SHALL call the inner provider again.
//! **Validates: Requirements 7.6**

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_auth::secrets::{CachedSecretProvider, SecretProvider};
use adk_core::AdkError;
use async_trait::async_trait;
use proptest::prelude::*;
use tokio::sync::Mutex;

/// A mock `SecretProvider` that tracks how many times `get_secret` is called
/// and returns a configurable value.
struct CountingSecretProvider {
    call_count: AtomicUsize,
    value: Mutex<String>,
}

impl CountingSecretProvider {
    fn new(value: String) -> Self {
        Self { call_count: AtomicUsize::new(0), value: Mutex::new(value) }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    async fn set_value(&self, new_value: String) {
        let mut v = self.value.lock().await;
        *v = new_value;
    }
}

#[async_trait]
impl SecretProvider for CountingSecretProvider {
    async fn get_secret(&self, _name: &str) -> Result<String, AdkError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let v = self.value.lock().await;
        Ok(v.clone())
    }
}

/// Wrapper to allow `Arc<CountingSecretProvider>` to be used as the inner provider
/// for `CachedSecretProvider`, which requires `P: SecretProvider`.
struct InnerRef(Arc<CountingSecretProvider>);

#[async_trait]
impl SecretProvider for InnerRef {
    async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
        self.0.get_secret(name).await
    }
}

/// Strategy for secret names.
fn arb_secret_name() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_-]{0,20}"
}

/// Strategy for secret values.
fn arb_secret_value() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9!@#$%^&*]{1,50}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: competitive-parity-v070, Property 10: Secret Cache Respects TTL**
    /// After caching with TTL t, get_secret within t returns cached value without
    /// calling inner provider.
    /// **Validates: Requirements 7.6**
    #[test]
    fn prop_cache_returns_cached_value_within_ttl(
        secret_name in arb_secret_name(),
        secret_value in arb_secret_value(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Use a long TTL so the cache never expires during this test
            let ttl = Duration::from_secs(60);
            let inner = Arc::new(CountingSecretProvider::new(secret_value.clone()));
            let cached = CachedSecretProvider::new(
                InnerRef(Arc::clone(&inner)),
                ttl,
            );

            // First call: should hit the inner provider
            let result1 = cached.get_secret(&secret_name).await.unwrap();
            prop_assert_eq!(&result1, &secret_value, "first call should return the secret value");
            prop_assert_eq!(inner.call_count(), 1, "first call should invoke inner provider once");

            // Second call within TTL: should return cached value without calling inner
            let result2 = cached.get_secret(&secret_name).await.unwrap();
            prop_assert_eq!(&result2, &secret_value, "second call within TTL should return cached value");
            prop_assert_eq!(inner.call_count(), 1, "second call within TTL should NOT invoke inner provider");

            // Third call: still cached
            let result3 = cached.get_secret(&secret_name).await.unwrap();
            prop_assert_eq!(&result3, &secret_value);
            prop_assert_eq!(inner.call_count(), 1, "third call within TTL should still use cache");

            Ok(())
        })?;
    }

    /// After TTL expires, the inner provider is called again and returns the new value.
    /// Uses a very short TTL (10ms) and a real sleep to test expiry.
    /// **Validates: Requirements 7.6**
    #[test]
    fn prop_cache_calls_inner_after_ttl_expires(
        secret_name in arb_secret_name(),
        initial_value in arb_secret_value(),
        updated_value in arb_secret_value(),
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ttl = Duration::from_millis(10);
            let inner = Arc::new(CountingSecretProvider::new(initial_value.clone()));
            let cached = CachedSecretProvider::new(
                InnerRef(Arc::clone(&inner)),
                ttl,
            );

            // First call: populates cache
            let result1 = cached.get_secret(&secret_name).await.unwrap();
            prop_assert_eq!(&result1, &initial_value);
            prop_assert_eq!(inner.call_count(), 1);

            // Update the inner provider's value
            inner.set_value(updated_value.clone()).await;

            // Sleep past TTL to ensure cache expires
            tokio::time::sleep(Duration::from_millis(20)).await;

            // Call after TTL: should hit inner provider again
            let result2 = cached.get_secret(&secret_name).await.unwrap();
            prop_assert_eq!(&result2, &updated_value, "after TTL expires, should get updated value");
            prop_assert_eq!(inner.call_count(), 2, "after TTL expires, inner provider should be called again");

            Ok(())
        })?;
    }

    /// Multiple different secret names each get their own cache entry.
    /// **Validates: Requirements 7.6**
    #[test]
    fn prop_cache_is_per_secret_name(
        name_a in arb_secret_name(),
        name_b in arb_secret_name(),
        value in arb_secret_value(),
    ) {
        prop_assume!(name_a != name_b);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ttl = Duration::from_secs(60);
            let inner = Arc::new(CountingSecretProvider::new(value.clone()));
            let cached = CachedSecretProvider::new(
                InnerRef(Arc::clone(&inner)),
                ttl,
            );

            // Fetch secret A
            cached.get_secret(&name_a).await.unwrap();
            prop_assert_eq!(inner.call_count(), 1);

            // Fetch secret B (different name, should call inner again)
            cached.get_secret(&name_b).await.unwrap();
            prop_assert_eq!(inner.call_count(), 2, "different secret names should each call inner provider");

            // Fetch secret A again within TTL (should be cached)
            cached.get_secret(&name_a).await.unwrap();
            prop_assert_eq!(inner.call_count(), 2, "re-fetching A within TTL should use cache");

            // Fetch secret B again within TTL (should be cached)
            cached.get_secret(&name_b).await.unwrap();
            prop_assert_eq!(inner.call_count(), 2, "re-fetching B within TTL should use cache");

            Ok(())
        })?;
    }
}
