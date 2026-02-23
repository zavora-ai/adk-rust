use adk_core::{ContextCacheConfig, Event};
use serde::{Deserialize, Serialize};

/// Internal cache lifecycle manager.
///
/// Tracks the active cache name, invocation count, and determines
/// when caching should be attempted or refreshed based on
/// [`ContextCacheConfig`] settings.
pub(crate) struct CacheManager {
    config: ContextCacheConfig,
    active_cache_name: Option<String>,
    invocation_count: u32,
}

impl CacheManager {
    pub(crate) fn new(config: ContextCacheConfig) -> Self {
        Self { config, active_cache_name: None, invocation_count: 0 }
    }

    /// Check if caching should be attempted based on config.
    ///
    /// Returns `false` when `min_tokens` or `ttl_seconds` is zero,
    /// effectively disabling the cache lifecycle.
    ///
    /// Note: The `min_tokens` threshold is enforced server-side by the
    /// provider (e.g., Gemini rejects cache creation for small contexts).
    /// A zero value here acts as a kill-switch for the entire lifecycle.
    pub(crate) fn is_enabled(&self) -> bool {
        self.config.min_tokens > 0 && self.config.ttl_seconds > 0
    }

    /// Return the active cache name, if any.
    pub(crate) fn active_cache_name(&self) -> Option<&str> {
        self.active_cache_name.as_deref()
    }

    /// Check if the cache needs refresh based on invocation count.
    ///
    /// Returns `true` when the number of recorded invocations has
    /// reached or exceeded `cache_intervals`.
    pub(crate) fn needs_refresh(&self) -> bool {
        self.invocation_count >= self.config.cache_intervals
    }

    /// Record an invocation and return the current cache name (if any).
    pub(crate) fn record_invocation(&mut self) -> Option<&str> {
        self.invocation_count += 1;
        self.active_cache_name.as_deref()
    }

    /// Set the active cache name after creation, resetting the
    /// invocation counter.
    pub(crate) fn set_active_cache(&mut self, name: String) {
        self.active_cache_name = Some(name);
        self.invocation_count = 0;
    }

    /// Clear the active cache (after deletion or on error),
    /// resetting the invocation counter.
    ///
    /// Returns the previously active cache name, if any.
    pub(crate) fn clear_active_cache(&mut self) -> Option<String> {
        self.invocation_count = 0;
        self.active_cache_name.take()
    }
}

/// Metrics computed from session event history.
///
/// All ratio fields are percentages in the range `[0.0, 100.0]`.
/// When there are no events with usage metadata, all fields are zero.
///
/// # Example
///
/// ```rust,ignore
/// use adk_runner::CachePerformanceAnalyzer;
///
/// let events = session.events();
/// let metrics = CachePerformanceAnalyzer::analyze(&events);
/// println!("Cache hit ratio: {:.1}%", metrics.cache_hit_ratio);
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheMetrics {
    /// Total requests with `UsageMetadata`.
    pub total_requests: u32,
    /// Requests where `cache_read_input_token_count > 0`.
    pub requests_with_cache_hits: u32,
    /// Sum of all `prompt_token_count` values.
    pub total_prompt_tokens: i64,
    /// Sum of all `cache_read_input_token_count` values.
    pub total_cache_read_tokens: i64,
    /// Sum of all `cache_creation_input_token_count` values.
    pub total_cache_creation_tokens: i64,
    /// `total_cache_read_tokens / total_prompt_tokens * 100`.
    pub cache_hit_ratio: f64,
    /// `requests_with_cache_hits / total_requests * 100`.
    pub cache_utilization_ratio: f64,
    /// `total_cache_read_tokens / total_requests`.
    pub avg_cached_tokens_per_request: f64,
}

/// Utility for computing cache effectiveness metrics from session events.
///
/// This is a stateless analyzer — call [`CachePerformanceAnalyzer::analyze`]
/// with any slice of events to get a [`CacheMetrics`] snapshot.
///
/// # Example
///
/// ```rust,ignore
/// use adk_runner::CachePerformanceAnalyzer;
///
/// let metrics = CachePerformanceAnalyzer::analyze(&events);
/// println!("Hit ratio: {:.1}%, Utilization: {:.1}%",
///     metrics.cache_hit_ratio, metrics.cache_utilization_ratio);
/// ```
pub struct CachePerformanceAnalyzer;

impl CachePerformanceAnalyzer {
    /// Analyze cache performance from a slice of events.
    ///
    /// Iterates over all events, extracts `usage_metadata` from LLM responses,
    /// and computes aggregate cache metrics. Events without `usage_metadata`
    /// are skipped. An empty slice returns zeroed metrics.
    pub fn analyze(events: &[Event]) -> CacheMetrics {
        let mut metrics = CacheMetrics::default();

        for event in events {
            let Some(ref usage) = event.llm_response.usage_metadata else {
                continue;
            };

            metrics.total_requests += 1;
            metrics.total_prompt_tokens += i64::from(usage.prompt_token_count);

            let cache_read = usage.cache_read_input_token_count.unwrap_or(0);
            metrics.total_cache_read_tokens += i64::from(cache_read);

            if cache_read > 0 {
                metrics.requests_with_cache_hits += 1;
            }

            let cache_creation = usage.cache_creation_input_token_count.unwrap_or(0);
            metrics.total_cache_creation_tokens += i64::from(cache_creation);
        }

        if metrics.total_prompt_tokens > 0 {
            metrics.cache_hit_ratio =
                metrics.total_cache_read_tokens as f64 / metrics.total_prompt_tokens as f64 * 100.0;
        }
        if metrics.total_requests > 0 {
            metrics.cache_utilization_ratio =
                metrics.requests_with_cache_hits as f64 / metrics.total_requests as f64 * 100.0;
            metrics.avg_cached_tokens_per_request =
                metrics.total_cache_read_tokens as f64 / metrics.total_requests as f64;
        }

        metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ContextCacheConfig {
        ContextCacheConfig { min_tokens: 4096, ttl_seconds: 600, cache_intervals: 3 }
    }

    #[test]
    fn test_new_manager_has_no_active_cache() {
        let cm = CacheManager::new(default_config());
        assert!(cm.active_cache_name.is_none());
        assert_eq!(cm.invocation_count, 0);
    }

    #[test]
    fn test_is_enabled_with_valid_config() {
        let cm = CacheManager::new(default_config());
        assert!(cm.is_enabled());
    }

    #[test]
    fn test_is_enabled_false_when_min_tokens_zero() {
        let config = ContextCacheConfig { min_tokens: 0, ttl_seconds: 600, cache_intervals: 3 };
        let cm = CacheManager::new(config);
        assert!(!cm.is_enabled());
    }

    #[test]
    fn test_is_enabled_false_when_ttl_zero() {
        let config = ContextCacheConfig { min_tokens: 4096, ttl_seconds: 0, cache_intervals: 3 };
        let cm = CacheManager::new(config);
        assert!(!cm.is_enabled());
    }

    #[test]
    fn test_is_enabled_false_when_both_zero() {
        let config = ContextCacheConfig { min_tokens: 0, ttl_seconds: 0, cache_intervals: 3 };
        let cm = CacheManager::new(config);
        assert!(!cm.is_enabled());
    }

    #[test]
    fn test_needs_refresh_false_initially() {
        let cm = CacheManager::new(default_config());
        assert!(!cm.needs_refresh());
    }

    #[test]
    fn test_needs_refresh_true_after_n_invocations() {
        let mut cm = CacheManager::new(default_config());
        // cache_intervals = 3, so after 3 invocations needs_refresh should be true
        cm.record_invocation();
        assert!(!cm.needs_refresh());
        cm.record_invocation();
        assert!(!cm.needs_refresh());
        cm.record_invocation();
        assert!(cm.needs_refresh());
    }

    #[test]
    fn test_record_invocation_returns_none_without_active_cache() {
        let mut cm = CacheManager::new(default_config());
        assert!(cm.record_invocation().is_none());
    }

    #[test]
    fn test_record_invocation_returns_cache_name() {
        let mut cm = CacheManager::new(default_config());
        cm.set_active_cache("cachedContents/abc123".to_string());
        let name = cm.record_invocation();
        assert_eq!(name, Some("cachedContents/abc123"));
    }

    #[test]
    fn test_set_active_cache_resets_invocation_count() {
        let mut cm = CacheManager::new(default_config());
        cm.record_invocation();
        cm.record_invocation();
        assert_eq!(cm.invocation_count, 2);

        cm.set_active_cache("cachedContents/new".to_string());
        assert_eq!(cm.invocation_count, 0);
        assert_eq!(cm.active_cache_name.as_deref(), Some("cachedContents/new"));
    }

    #[test]
    fn test_clear_active_cache_returns_old_name() {
        let mut cm = CacheManager::new(default_config());
        cm.set_active_cache("cachedContents/old".to_string());
        cm.record_invocation();

        let old = cm.clear_active_cache();
        assert_eq!(old.as_deref(), Some("cachedContents/old"));
        assert!(cm.active_cache_name.is_none());
        assert_eq!(cm.invocation_count, 0);
    }

    #[test]
    fn test_clear_active_cache_returns_none_when_empty() {
        let mut cm = CacheManager::new(default_config());
        let old = cm.clear_active_cache();
        assert!(old.is_none());
    }

    #[test]
    fn test_full_lifecycle() {
        let mut cm = CacheManager::new(ContextCacheConfig {
            min_tokens: 1024,
            ttl_seconds: 300,
            cache_intervals: 2,
        });

        assert!(cm.is_enabled());
        assert!(!cm.needs_refresh());

        // No cache yet
        assert!(cm.record_invocation().is_none());

        // Set a cache
        cm.set_active_cache("cachedContents/v1".to_string());
        assert_eq!(cm.invocation_count, 0);

        // First invocation returns cache name
        assert_eq!(cm.record_invocation(), Some("cachedContents/v1"));
        assert!(!cm.needs_refresh());

        // Second invocation triggers refresh
        assert_eq!(cm.record_invocation(), Some("cachedContents/v1"));
        assert!(cm.needs_refresh());

        // Refresh: clear old, set new
        let old = cm.clear_active_cache();
        assert_eq!(old.as_deref(), Some("cachedContents/v1"));
        cm.set_active_cache("cachedContents/v2".to_string());
        assert!(!cm.needs_refresh());
        assert_eq!(cm.record_invocation(), Some("cachedContents/v2"));
    }

    // --- CachePerformanceAnalyzer tests ---

    use adk_core::{LlmResponse, UsageMetadata};

    fn event_with_usage(
        prompt: i32,
        candidates: i32,
        cache_read: Option<i32>,
        cache_creation: Option<i32>,
    ) -> Event {
        let mut event = Event::new("test-invocation");
        event.llm_response = LlmResponse {
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: prompt,
                candidates_token_count: candidates,
                total_token_count: prompt + candidates,
                cache_read_input_token_count: cache_read,
                cache_creation_input_token_count: cache_creation,
                ..Default::default()
            }),
            ..Default::default()
        };
        event
    }

    fn event_without_usage() -> Event {
        Event::new("test-invocation")
    }

    #[test]
    fn test_analyze_empty_events() {
        let metrics = CachePerformanceAnalyzer::analyze(&[]);
        assert_eq!(metrics.total_requests, 0);
        assert_eq!(metrics.requests_with_cache_hits, 0);
        assert_eq!(metrics.total_prompt_tokens, 0);
        assert_eq!(metrics.total_cache_read_tokens, 0);
        assert_eq!(metrics.total_cache_creation_tokens, 0);
        assert_eq!(metrics.cache_hit_ratio, 0.0);
        assert_eq!(metrics.cache_utilization_ratio, 0.0);
        assert_eq!(metrics.avg_cached_tokens_per_request, 0.0);
    }

    #[test]
    fn test_analyze_events_without_usage_metadata() {
        let events = vec![event_without_usage(), event_without_usage()];
        let metrics = CachePerformanceAnalyzer::analyze(&events);
        assert_eq!(metrics.total_requests, 0);
        assert_eq!(metrics.cache_hit_ratio, 0.0);
    }

    #[test]
    fn test_analyze_single_event_no_cache() {
        let events = vec![event_with_usage(1000, 200, None, None)];
        let metrics = CachePerformanceAnalyzer::analyze(&events);
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.requests_with_cache_hits, 0);
        assert_eq!(metrics.total_prompt_tokens, 1000);
        assert_eq!(metrics.total_cache_read_tokens, 0);
        assert_eq!(metrics.total_cache_creation_tokens, 0);
        assert_eq!(metrics.cache_hit_ratio, 0.0);
        assert_eq!(metrics.cache_utilization_ratio, 0.0);
        assert_eq!(metrics.avg_cached_tokens_per_request, 0.0);
    }

    #[test]
    fn test_analyze_single_event_with_cache_hit() {
        let events = vec![event_with_usage(1000, 200, Some(500), None)];
        let metrics = CachePerformanceAnalyzer::analyze(&events);
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.requests_with_cache_hits, 1);
        assert_eq!(metrics.total_prompt_tokens, 1000);
        assert_eq!(metrics.total_cache_read_tokens, 500);
        assert_eq!(metrics.cache_hit_ratio, 50.0);
        assert_eq!(metrics.cache_utilization_ratio, 100.0);
        assert_eq!(metrics.avg_cached_tokens_per_request, 500.0);
    }

    #[test]
    fn test_analyze_mixed_events() {
        let events = vec![
            event_with_usage(1000, 200, Some(800), Some(200)),
            event_with_usage(1000, 300, None, None),
            event_with_usage(1000, 100, Some(600), None),
            event_without_usage(), // skipped
        ];
        let metrics = CachePerformanceAnalyzer::analyze(&events);
        assert_eq!(metrics.total_requests, 3);
        assert_eq!(metrics.requests_with_cache_hits, 2);
        assert_eq!(metrics.total_prompt_tokens, 3000);
        assert_eq!(metrics.total_cache_read_tokens, 1400);
        assert_eq!(metrics.total_cache_creation_tokens, 200);
        // cache_hit_ratio = 1400 / 3000 * 100 ≈ 46.67
        assert!((metrics.cache_hit_ratio - 46.666_666_666_666_664).abs() < 1e-10);
        // cache_utilization_ratio = 2 / 3 * 100 ≈ 66.67
        assert!((metrics.cache_utilization_ratio - 66.666_666_666_666_66).abs() < 1e-10);
        // avg_cached_tokens_per_request = 1400 / 3 ≈ 466.67
        assert!((metrics.avg_cached_tokens_per_request - 466.666_666_666_666_7).abs() < 1e-10);
    }

    #[test]
    fn test_analyze_all_cache_hits() {
        let events = vec![
            event_with_usage(500, 100, Some(500), None),
            event_with_usage(500, 100, Some(500), None),
        ];
        let metrics = CachePerformanceAnalyzer::analyze(&events);
        assert_eq!(metrics.total_requests, 2);
        assert_eq!(metrics.requests_with_cache_hits, 2);
        assert_eq!(metrics.cache_hit_ratio, 100.0);
        assert_eq!(metrics.cache_utilization_ratio, 100.0);
        assert_eq!(metrics.avg_cached_tokens_per_request, 500.0);
    }

    #[test]
    fn test_analyze_zero_prompt_tokens() {
        // Edge case: usage_metadata present but prompt_token_count is 0
        let events = vec![event_with_usage(0, 100, None, None)];
        let metrics = CachePerformanceAnalyzer::analyze(&events);
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.total_prompt_tokens, 0);
        // cache_hit_ratio stays 0.0 (no division by zero)
        assert_eq!(metrics.cache_hit_ratio, 0.0);
        assert_eq!(metrics.cache_utilization_ratio, 0.0);
    }

    #[test]
    fn test_analyze_cache_creation_only() {
        let events = vec![event_with_usage(2000, 500, None, Some(1500))];
        let metrics = CachePerformanceAnalyzer::analyze(&events);
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.requests_with_cache_hits, 0);
        assert_eq!(metrics.total_cache_creation_tokens, 1500);
        assert_eq!(metrics.cache_hit_ratio, 0.0);
        assert_eq!(metrics.cache_utilization_ratio, 0.0);
    }
}
