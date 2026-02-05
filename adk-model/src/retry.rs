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
    matches!(status_code, 408 | 429 | 500 | 502 | 503 | 504)
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
        || normalized.contains("RATE LIMIT")
        || normalized.contains("TOO MANY REQUESTS")
        || normalized.contains("RESOURCE_EXHAUSTED")
        || normalized.contains("UNAVAILABLE")
        || normalized.contains("DEADLINE_EXCEEDED")
        || normalized.contains("TIMEOUT")
        || normalized.contains("TIMED OUT")
        || normalized.contains("CONNECTION RESET")
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
    if !retry_config.enabled {
        return operation().await;
    }

    let mut attempt: u32 = 0;
    let mut delay = retry_config.initial_delay;

    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) if attempt < retry_config.max_retries && classify_error(&error) => {
                attempt += 1;
                adk_telemetry::warn!(
                    attempt = attempt,
                    max_retries = retry_config.max_retries,
                    delay_ms = delay.as_millis(),
                    error = %error,
                    "Provider request failed with retryable error; retrying"
                );
                tokio::time::sleep(delay).await;
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
        assert!(!is_retryable_status_code(400));
    }
}
