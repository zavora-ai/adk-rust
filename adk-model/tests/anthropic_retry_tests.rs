//! Property tests for Anthropic retry and rate-limit handling.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use adk_core::AdkError;
use adk_model::anthropic::RateLimitInfo;
use adk_model::retry::{
    RetryConfig, ServerRetryHint, execute_with_retry_hint, is_retryable_model_error,
    is_retryable_status_code,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generate retry-after durations in a small range to keep tests fast.
/// We use 0..50 milliseconds so the property test completes quickly while
/// still exercising the timing logic.
fn arb_retry_after_ms() -> impl Strategy<Value = u64> {
    10u64..50
}

/// Generate arbitrary u32 values for rate-limit header fields.
fn arb_rate_limit_u32() -> impl Strategy<Value = u32> {
    0u32..1_000_000
}

/// Generate optional u32 header values (present or absent).
fn arb_optional_u32() -> impl Strategy<Value = Option<u32>> {
    prop_oneof![Just(None), arb_rate_limit_u32().prop_map(Some),]
}

/// Generate optional ISO 8601 reset timestamps.
fn arb_optional_reset() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        "[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z".prop_map(|s| Some(s)),
    ]
}

/// Generate optional retry-after header values (seconds as string).
fn arb_optional_retry_after_secs() -> impl Strategy<Value = Option<u64>> {
    prop_oneof![Just(None), (0u64..3600).prop_map(Some),]
}

// ---------------------------------------------------------------------------
// Property 9: Retry-after header respected
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: anthropic-deep-integration, Property 9: Retry-after header respected**
    /// *For any* HTTP 429 response with a `retry-after` header specifying D seconds,
    /// the retry mechanism SHALL wait at least D seconds before the next attempt,
    /// regardless of the configured exponential backoff parameters.
    /// **Validates: Requirements 5.1**
    #[test]
    fn prop_retry_after_header_respected(delay_ms in arb_retry_after_ms()) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        let retry_config = RetryConfig::default()
            .with_max_retries(1)
            // Set backoff to zero so any observed delay comes from the hint.
            .with_initial_delay(Duration::ZERO)
            .with_max_delay(Duration::ZERO);

        let hint = ServerRetryHint {
            retry_after: Some(Duration::from_millis(delay_ms)),
        };

        let attempts = Arc::new(AtomicU32::new(0));
        let first_attempt_time: Arc<std::sync::Mutex<Option<Instant>>> =
            Arc::new(std::sync::Mutex::new(None));
        let second_attempt_time: Arc<std::sync::Mutex<Option<Instant>>> =
            Arc::new(std::sync::Mutex::new(None));

        let attempts_c = Arc::clone(&attempts);
        let t1 = Arc::clone(&first_attempt_time);
        let t2 = Arc::clone(&second_attempt_time);

        let result = rt.block_on(execute_with_retry_hint(
            &retry_config,
            is_retryable_model_error,
            Some(&hint),
            &mut || {
                let attempts_c = Arc::clone(&attempts_c);
                let t1 = Arc::clone(&t1);
                let t2 = Arc::clone(&t2);
                async move {
                    let attempt = attempts_c.fetch_add(1, Ordering::SeqCst);
                    let now = Instant::now();
                    if attempt == 0 {
                        *t1.lock().unwrap() = Some(now);
                        Err(AdkError::Model("HTTP 429 rate limit".to_string()))
                    } else {
                        *t2.lock().unwrap() = Some(now);
                        Ok("success")
                    }
                }
            },
        ));

        prop_assert!(result.is_ok(), "retry should succeed on second attempt");
        prop_assert_eq!(attempts.load(Ordering::SeqCst), 2);

        let t1_val = first_attempt_time.lock().unwrap().unwrap();
        let t2_val = second_attempt_time.lock().unwrap().unwrap();
        let elapsed = t2_val.duration_since(t1_val);

        // The retry mechanism should wait at least the server-provided delay.
        // Allow a small tolerance (1ms) for scheduling jitter.
        let min_expected = Duration::from_millis(delay_ms.saturating_sub(1));
        prop_assert!(
            elapsed >= min_expected,
            "Elapsed {:?} should be >= {:?} (retry-after: {}ms)",
            elapsed,
            min_expected,
            delay_ms,
        );
    }
}

// ---------------------------------------------------------------------------
// Property 10: Rate-limit header parsing
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: anthropic-deep-integration, Property 10: Rate-limit header parsing**
    /// *For any* set of Anthropic rate-limit response headers with arbitrary numeric
    /// values, the parsed `RateLimitInfo` SHALL contain the exact values from each
    /// header, with missing headers represented as None.
    /// **Validates: Requirements 5.3**
    #[test]
    fn prop_rate_limit_header_parsing(
        requests_limit in arb_optional_u32(),
        requests_remaining in arb_optional_u32(),
        requests_reset in arb_optional_reset(),
        tokens_limit in arb_optional_u32(),
        tokens_remaining in arb_optional_u32(),
        tokens_reset in arb_optional_reset(),
        retry_after_secs in arb_optional_retry_after_secs(),
    ) {
        let mut headers = HashMap::new();

        if let Some(v) = requests_limit {
            headers.insert(
                "anthropic-ratelimit-requests-limit".to_string(),
                v.to_string(),
            );
        }
        if let Some(v) = requests_remaining {
            headers.insert(
                "anthropic-ratelimit-requests-remaining".to_string(),
                v.to_string(),
            );
        }
        if let Some(ref v) = requests_reset {
            headers.insert(
                "anthropic-ratelimit-requests-reset".to_string(),
                v.clone(),
            );
        }
        if let Some(v) = tokens_limit {
            headers.insert(
                "anthropic-ratelimit-tokens-limit".to_string(),
                v.to_string(),
            );
        }
        if let Some(v) = tokens_remaining {
            headers.insert(
                "anthropic-ratelimit-tokens-remaining".to_string(),
                v.to_string(),
            );
        }
        if let Some(ref v) = tokens_reset {
            headers.insert(
                "anthropic-ratelimit-tokens-reset".to_string(),
                v.clone(),
            );
        }
        if let Some(v) = retry_after_secs {
            headers.insert("retry-after".to_string(), v.to_string());
        }

        let info = RateLimitInfo::from_headers(&headers);

        // Each field must match the input exactly.
        prop_assert_eq!(info.requests_limit, requests_limit);
        prop_assert_eq!(info.requests_remaining, requests_remaining);
        prop_assert_eq!(&info.requests_reset, &requests_reset);
        prop_assert_eq!(info.tokens_limit, tokens_limit);
        prop_assert_eq!(info.tokens_remaining, tokens_remaining);
        prop_assert_eq!(&info.tokens_reset, &tokens_reset);
        prop_assert_eq!(
            info.retry_after,
            retry_after_secs.map(Duration::from_secs),
        );
    }
}

// ---------------------------------------------------------------------------
// Supplementary: 529 is retryable (supports Property 9 context)
// ---------------------------------------------------------------------------

#[test]
fn status_529_is_retryable() {
    assert!(is_retryable_status_code(529));
}
