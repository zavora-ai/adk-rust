use adk_core::{AdkError, Result};
use std::{future::Future, time::Duration};

#[derive(Clone, Debug)]
pub struct RetryConfig {
    pub enabled: bool,
    pub max_retries: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub backoff_multiplier: f32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryConfig {
    #[must_use]
    pub fn disabled() -> Self {
        Self { enabled: false, ..Self::default() }
    }

    #[must_use]
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    #[must_use]
    pub fn with_initial_delay(mut self, initial_delay: Duration) -> Self {
        self.initial_delay = initial_delay;
        self
    }

    #[must_use]
    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }

    #[must_use]
    pub fn with_backoff_multiplier(mut self, backoff_multiplier: f32) -> Self {
        self.backoff_multiplier = backoff_multiplier;
        self
    }
}

#[must_use]
pub fn is_retryable_status_code(status_code: u16) -> bool {
    matches!(status_code, 408 | 429 | 500 | 502 | 503 | 504 | 529)
}

#[must_use]
pub fn is_retryable_error_message(message: &str) -> bool {
    let normalized = message.to_ascii_uppercase();
    normalized.contains("429")
        || normalized.contains("408")
        || normalized.contains("500")
        || normalized.contains("502")
        || normalized.contains("503")
        || normalized.contains("504")
        || normalized.contains("529")
        || normalized.contains("RATE LIMIT")
        || normalized.contains("TOO MANY REQUESTS")
        || normalized.contains("RESOURCE_EXHAUSTED")
        || normalized.contains("UNAVAILABLE")
        || normalized.contains("DEADLINE_EXCEEDED")
        || normalized.contains("TIMEOUT")
        || normalized.contains("TIMED OUT")
        || normalized.contains("CONNECTION RESET")
        || normalized.contains("OVERLOADED")
}

#[must_use]
pub fn is_retryable_model_error(error: &AdkError) -> bool {
    match error {
        AdkError::Model(message) => is_retryable_error_message(message),
        _ => false,
    }
}

fn next_retry_delay(current: Duration, retry_config: &RetryConfig) -> Duration {
    if current >= retry_config.max_delay {
        return retry_config.max_delay;
    }

    let multiplier = retry_config.backoff_multiplier.max(1.0) as f64;
    let scaled = Duration::from_secs_f64(current.as_secs_f64() * multiplier);
    scaled.min(retry_config.max_delay)
}

/// Hint from the server about when to retry.
///
/// When the server provides a `retry-after` header, this hint overrides the
/// exponential backoff calculation for the next retry attempt.
///
/// # Example
///
/// ```rust
/// use adk_model::retry::ServerRetryHint;
/// use std::time::Duration;
///
/// let hint = ServerRetryHint { retry_after: Some(Duration::from_secs(30)) };
/// assert_eq!(hint.retry_after, Some(Duration::from_secs(30)));
/// ```
#[derive(Debug, Clone, Default)]
pub struct ServerRetryHint {
    /// Server-suggested delay before retrying.
    pub retry_after: Option<Duration>,
}

pub async fn execute_with_retry<T, Op, Fut, Classify>(
    retry_config: &RetryConfig,
    classify_error: Classify,
    mut operation: Op,
) -> Result<T>
where
    Op: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
    Classify: Fn(&AdkError) -> bool,
{
    execute_with_retry_hint(retry_config, classify_error, None, &mut operation).await
}

/// Execute an operation with retry logic, optionally using a server-provided
/// retry hint to override the backoff delay.
///
/// When `server_hint` contains a `retry_after` duration, that duration is used
/// instead of the exponential backoff calculation. This respects server-provided
/// timing from `retry-after` headers (Requirement 5.1).
pub async fn execute_with_retry_hint<T, Op, Fut, Classify>(
    retry_config: &RetryConfig,
    classify_error: Classify,
    server_hint: Option<&ServerRetryHint>,
    operation: &mut Op,
) -> Result<T>
where
    Op: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
    Classify: Fn(&AdkError) -> bool,
{
    if !retry_config.enabled {
        return operation().await;
    }

    let mut attempt: u32 = 0;
    let mut delay = retry_config.initial_delay;

    // If the server provided a retry-after hint, use it for the first retry delay.
    let server_delay = server_hint.and_then(|h| h.retry_after);

    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) if attempt < retry_config.max_retries && classify_error(&error) => {
                attempt += 1;

                // Requirement 5.1: Use server-provided retry-after when present,
                // Requirement 5.4: Fall back to exponential backoff otherwise.
                let effective_delay =
                    if attempt == 1 { server_delay.unwrap_or(delay) } else { delay };

                adk_telemetry::warn!(
                    attempt = attempt,
                    max_retries = retry_config.max_retries,
                    delay_ms = effective_delay.as_millis(),
                    error = %error,
                    "Provider request failed with retryable error; retrying"
                );
                tokio::time::sleep(effective_delay).await;
                delay = next_retry_delay(delay, retry_config);
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    };

    #[tokio::test]
    async fn execute_with_retry_retries_when_classified_retryable() {
        let retry_config = RetryConfig::default()
            .with_max_retries(2)
            .with_initial_delay(Duration::ZERO)
            .with_max_delay(Duration::ZERO);
        let attempts = Arc::new(AtomicU32::new(0));

        let result = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let attempts = Arc::clone(&attempts);
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    return Err(AdkError::Model("HTTP 429 rate limit".to_string()));
                }
                Ok("ok")
            }
        })
        .await
        .expect("operation should succeed after retries");

        assert_eq!(result, "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn execute_with_retry_stops_on_non_retryable_error() {
        let retry_config = RetryConfig::default()
            .with_max_retries(3)
            .with_initial_delay(Duration::ZERO)
            .with_max_delay(Duration::ZERO);
        let attempts = Arc::new(AtomicU32::new(0));

        let error = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(AdkError::Model("HTTP 400 bad request".to_string()))
            }
        })
        .await
        .expect_err("operation should fail without retries");

        assert!(matches!(error, AdkError::Model(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn execute_with_retry_respects_disabled_config() {
        let retry_config = RetryConfig::disabled().with_max_retries(10);
        let attempts = Arc::new(AtomicU32::new(0));

        let error = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let attempts = Arc::clone(&attempts);
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(AdkError::Model("HTTP 429 too many requests".to_string()))
            }
        })
        .await
        .expect_err("disabled retries should return first error");

        assert!(matches!(error, AdkError::Model(_)));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retryable_status_code_matches_transient_errors() {
        assert!(is_retryable_status_code(429));
        assert!(is_retryable_status_code(503));
        assert!(is_retryable_status_code(529));
        assert!(!is_retryable_status_code(400));
        assert!(!is_retryable_status_code(401));
    }

    #[test]
    fn retryable_error_message_matches_529_and_overloaded() {
        assert!(is_retryable_error_message("HTTP 529 overloaded"));
        assert!(is_retryable_error_message("Server OVERLOADED, try again"));
    }

    #[tokio::test]
    async fn execute_with_retry_hint_uses_server_delay() {
        let retry_config = RetryConfig::default()
            .with_max_retries(2)
            .with_initial_delay(Duration::ZERO)
            .with_max_delay(Duration::ZERO);
        let attempts = Arc::new(AtomicU32::new(0));
        let hint = ServerRetryHint { retry_after: Some(Duration::ZERO) };

        let result = execute_with_retry_hint(
            &retry_config,
            is_retryable_model_error,
            Some(&hint),
            &mut || {
                let attempts = Arc::clone(&attempts);
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt < 1 {
                        return Err(AdkError::Model("HTTP 429 rate limit".to_string()));
                    }
                    Ok("ok")
                }
            },
        )
        .await
        .expect("operation should succeed after retry with hint");

        assert_eq!(result, "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// Requirement 5.2: HTTP 529 (overloaded) is retried end-to-end.
    #[tokio::test]
    async fn status_529_is_retried_end_to_end() {
        let retry_config = RetryConfig::default()
            .with_max_retries(2)
            .with_initial_delay(Duration::ZERO)
            .with_max_delay(Duration::ZERO);
        let attempts = Arc::new(AtomicU32::new(0));

        let result = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let attempts = Arc::clone(&attempts);
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    return Err(AdkError::Model("HTTP 529 overloaded".to_string()));
                }
                Ok("recovered")
            }
        })
        .await
        .expect("529 should be retried and succeed on second attempt");

        assert_eq!(result, "recovered");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    /// Requirement 5.4: Exponential backoff when retry-after is absent.
    /// With initial_delay=20ms and multiplier=2.0, the delays should be
    /// ~20ms (attempt 1) then ~40ms (attempt 2). We verify each gap is
    /// at least the expected delay.
    #[tokio::test]
    async fn exponential_backoff_without_retry_after() {
        let retry_config = RetryConfig::default()
            .with_max_retries(3)
            .with_initial_delay(Duration::from_millis(20))
            .with_max_delay(Duration::from_millis(200))
            .with_backoff_multiplier(2.0);

        let timestamps: Arc<std::sync::Mutex<Vec<std::time::Instant>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let result = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let timestamps = Arc::clone(&timestamps);
            async move {
                let now = std::time::Instant::now();
                let mut ts = timestamps.lock().unwrap();
                let attempt = ts.len();
                ts.push(now);
                if attempt < 3 {
                    return Err(AdkError::Model("HTTP 429 rate limit".to_string()));
                }
                Ok("done")
            }
        })
        .await
        .expect("should succeed after backoff retries");

        assert_eq!(result, "done");

        let ts = timestamps.lock().unwrap();
        assert_eq!(ts.len(), 4); // initial + 3 retries

        // Gap between attempt 0 and 1 should be >= initial_delay (20ms).
        let gap1 = ts[1].duration_since(ts[0]);
        assert!(gap1 >= Duration::from_millis(18), "first backoff gap {gap1:?} should be >= ~20ms");

        // Gap between attempt 1 and 2 should be >= 2 * initial_delay (40ms).
        let gap2 = ts[2].duration_since(ts[1]);
        assert!(
            gap2 >= Duration::from_millis(36),
            "second backoff gap {gap2:?} should be >= ~40ms"
        );

        // Gap 2 should be roughly double gap 1 (with tolerance for scheduling).
        assert!(gap2 >= gap1, "backoff should increase: gap2={gap2:?} should be >= gap1={gap1:?}");
    }
}
