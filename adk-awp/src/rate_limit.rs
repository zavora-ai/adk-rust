//! Per-trust-level sliding window rate limiter.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use awp_types::TrustLevel;
use dashmap::DashMap;

/// Trait for checking whether a request should be rate-limited.
#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// Check if a request is allowed.
    ///
    /// Returns `Ok(())` if allowed, or `Err(retry_after_secs)` if the caller
    /// should wait before retrying.
    async fn check(&self, key: &str, trust_level: TrustLevel) -> Result<(), u64>;
}

/// Per-trust-level rate limit configuration.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    /// Maximum number of requests allowed within the window.
    pub max_requests: u64,
    /// Window duration in seconds.
    pub window_secs: u64,
}

/// In-memory sliding window rate limiter backed by [`DashMap`].
///
/// Each unique `"{trust_level}:{client_id}"` key gets its own sliding window.
/// Timestamps older than the window are pruned on every check.
///
/// Default limits:
/// - Anonymous: 30 requests/minute
/// - Known: 120 requests/minute
/// - Partner: 600 requests/minute
/// - Internal: unlimited (not tracked)
pub struct InMemoryRateLimiter {
    windows: DashMap<String, VecDeque<Instant>>,
    limits: HashMap<TrustLevel, RateLimitConfig>,
    window_size: Duration,
}

impl InMemoryRateLimiter {
    /// Create a rate limiter with default limits.
    pub fn new() -> Self {
        let mut limits = HashMap::new();
        limits.insert(TrustLevel::Anonymous, RateLimitConfig { max_requests: 30, window_secs: 60 });
        limits.insert(TrustLevel::Known, RateLimitConfig { max_requests: 120, window_secs: 60 });
        limits.insert(TrustLevel::Partner, RateLimitConfig { max_requests: 600, window_secs: 60 });
        // Internal is unlimited — no entry in the map

        Self { windows: DashMap::new(), limits, window_size: Duration::from_secs(60) }
    }

    /// Create a rate limiter with custom limits and window size.
    pub fn with_config(
        limits: HashMap<TrustLevel, RateLimitConfig>,
        window_size: Duration,
    ) -> Self {
        Self { windows: DashMap::new(), limits, window_size }
    }
}

impl Default for InMemoryRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RateLimiter for InMemoryRateLimiter {
    async fn check(&self, key: &str, trust_level: TrustLevel) -> Result<(), u64> {
        // Internal trust level is unlimited
        let config = match self.limits.get(&trust_level) {
            Some(c) => *c,
            None => return Ok(()),
        };

        let composite_key = format!("{trust_level}:{key}");
        let now = Instant::now();
        let window_start = now - self.window_size;

        let mut entry = self.windows.entry(composite_key).or_default();
        let deque = entry.value_mut();

        // Remove expired timestamps
        while deque.front().is_some_and(|t| *t < window_start) {
            deque.pop_front();
        }

        if deque.len() as u64 >= config.max_requests {
            // Calculate retry-after from the oldest entry in the window
            let oldest = deque.front().copied().unwrap_or(now);
            let expires_at = oldest + self.window_size;
            let retry_after = expires_at.duration_since(now).as_secs().max(1);
            return Err(retry_after);
        }

        deque.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_anonymous_under_limit() {
        let limiter = InMemoryRateLimiter::new();
        for _ in 0..30 {
            assert!(limiter.check("client1", TrustLevel::Anonymous).await.is_ok());
        }
    }

    #[tokio::test]
    async fn test_anonymous_over_limit() {
        let limiter = InMemoryRateLimiter::new();
        for _ in 0..30 {
            limiter.check("client1", TrustLevel::Anonymous).await.unwrap();
        }
        let result = limiter.check("client1", TrustLevel::Anonymous).await;
        assert!(result.is_err());
        let retry_after = result.unwrap_err();
        assert!(retry_after > 0);
    }

    #[tokio::test]
    async fn test_internal_unlimited() {
        let limiter = InMemoryRateLimiter::new();
        for _ in 0..1000 {
            assert!(limiter.check("client1", TrustLevel::Internal).await.is_ok());
        }
    }

    #[tokio::test]
    async fn test_different_keys_independent() {
        let limiter = InMemoryRateLimiter::new();
        for _ in 0..30 {
            limiter.check("client1", TrustLevel::Anonymous).await.unwrap();
        }
        // client2 should still be allowed
        assert!(limiter.check("client2", TrustLevel::Anonymous).await.is_ok());
    }

    #[tokio::test]
    async fn test_different_trust_levels_independent() {
        let limiter = InMemoryRateLimiter::new();
        for _ in 0..30 {
            limiter.check("client1", TrustLevel::Anonymous).await.unwrap();
        }
        // Same client at Known level should still be allowed
        assert!(limiter.check("client1", TrustLevel::Known).await.is_ok());
    }

    #[tokio::test]
    async fn test_custom_config() {
        let mut limits = HashMap::new();
        limits.insert(TrustLevel::Anonymous, RateLimitConfig { max_requests: 2, window_secs: 1 });
        let limiter = InMemoryRateLimiter::with_config(limits, Duration::from_secs(1));

        assert!(limiter.check("c", TrustLevel::Anonymous).await.is_ok());
        assert!(limiter.check("c", TrustLevel::Anonymous).await.is_ok());
        assert!(limiter.check("c", TrustLevel::Anonymous).await.is_err());
    }

    #[tokio::test]
    async fn test_known_higher_limit() {
        let limiter = InMemoryRateLimiter::new();
        for _ in 0..120 {
            assert!(limiter.check("client1", TrustLevel::Known).await.is_ok());
        }
        assert!(limiter.check("client1", TrustLevel::Known).await.is_err());
    }

    #[tokio::test]
    async fn test_partner_higher_limit() {
        let limiter = InMemoryRateLimiter::new();
        for _ in 0..600 {
            assert!(limiter.check("client1", TrustLevel::Partner).await.is_ok());
        }
        assert!(limiter.check("client1", TrustLevel::Partner).await.is_err());
    }
}
