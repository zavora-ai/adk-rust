use adk_core::{AdkError, ErrorCategory, Result};
use std::{future::Future, time::Duration};

/// Configuration for automatic retry with exponential backoff.
#[derive(Clone, Debug)]
pub struct RetryConfig {
    /// Whether retries are enabled.
    pub enabled: bool,
    /// Maximum number of retry attempts before giving up.
    pub max_retries: u32,
    /// Initial delay before the first retry.
    pub initial_delay: Duration,
    /// Maximum exponential backoff delay.
    pub max_delay: Duration,
    /// Largest server-provided delay this executor will wait for.
    /// A larger delay returns the original failure without another attempt.
    pub max_server_delay: Duration,
    /// Multiplier applied to the delay after each retry.
    pub backoff_multiplier: f32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
            max_server_delay: Duration::from_secs(60),
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryConfig {
    /// Create a disabled retry configuration (no retries).
    #[must_use]
    pub fn disabled() -> Self {
        Self { enabled: false, ..Self::default() }
    }

    /// Set the maximum number of retry attempts.
    #[must_use]
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Set the initial delay before the first retry.
    #[must_use]
    pub fn with_initial_delay(mut self, initial_delay: Duration) -> Self {
        self.initial_delay = initial_delay;
        self
    }

    /// Set the maximum exponential backoff delay.
    #[must_use]
    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }

    /// Set the backoff multiplier applied after each retry.
    #[must_use]
    pub fn with_backoff_multiplier(mut self, backoff_multiplier: f32) -> Self {
        self.backoff_multiplier = backoff_multiplier;
        self
    }

    /// Set the largest accepted server-provided retry delay.
    ///
    /// The default is 60 seconds. A hint above the limit returns the original
    /// error; an accepted hint is never shortened to [`Self::max_delay`].
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_model::retry::RetryConfig;
    /// use std::time::Duration;
    ///
    /// let retry = RetryConfig::default().with_max_server_delay(Duration::from_secs(30));
    /// assert_eq!(retry.max_server_delay, Duration::from_secs(30));
    /// ```
    #[must_use]
    pub fn with_max_server_delay(mut self, limit: Duration) -> Self {
        self.max_server_delay = limit;
        self
    }
}

/// Returns `true` if the HTTP status code indicates a transient error worth retrying.
#[must_use]
pub fn is_retryable_status_code(status_code: u16) -> bool {
    matches!(status_code, 408 | 429 | 500 | 502 | 503 | 504 | 520..=524 | 529)
}

/// Maps an upstream HTTP error status to the [`ErrorCategory`] provider clients report.
///
/// The transient server statuses (500, 502–504, 520–524, and Anthropic's 529) map
/// to [`ErrorCategory::Unavailable`], which [`execute_with_retry`] retries; every
/// supported provider documents them as safe to retry after a short wait.
///
/// # Example
///
/// ```rust
/// use adk_core::ErrorCategory;
/// use adk_model::retry::category_for_status_code;
///
/// assert_eq!(category_for_status_code(502), ErrorCategory::Unavailable);
/// assert_eq!(category_for_status_code(429), ErrorCategory::RateLimited);
/// assert_eq!(category_for_status_code(400), ErrorCategory::InvalidInput);
/// ```
#[must_use]
pub fn category_for_status_code(status_code: u16) -> ErrorCategory {
    match status_code {
        401 => ErrorCategory::Unauthorized,
        403 => ErrorCategory::Forbidden,
        404 => ErrorCategory::NotFound,
        408 => ErrorCategory::Timeout,
        429 => ErrorCategory::RateLimited,
        500 | 502 | 503 | 504 | 520..=524 | 529 => ErrorCategory::Unavailable,
        500..=599 => ErrorCategory::Internal,
        _ => ErrorCategory::InvalidInput,
    }
}

/// Parses a `Retry-After` header value given in seconds or as an HTTP-date.
///
/// Delay-seconds follow RFC 9110's non-negative integer grammar. An HTTP-date
/// in any of its three formats yields the time remaining, or zero when it has
/// passed. Invalid or overflowing values are ignored so normal backoff applies.
/// Returns `None` for any other value.
///
/// # Example
///
/// ```rust
/// use adk_model::retry::parse_retry_after;
/// use std::time::Duration;
///
/// assert_eq!(parse_retry_after("7"), Some(Duration::from_secs(7)));
/// assert_eq!(parse_retry_after("1.5"), None);
/// assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), Some(Duration::ZERO));
/// assert_eq!(parse_retry_after("soon"), None);
/// ```
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    // `RetryHint` stores whole milliseconds as `u64`.
    let value = value.trim_matches([' ', '\t']);
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        let delay = Duration::from_secs(value.parse().ok()?);
        u64::try_from(delay.as_millis()).ok()?;
        return Some(delay);
    }
    let date = httpdate::parse_http_date(value).ok()?;
    let delay = date.duration_since(std::time::SystemTime::now()).unwrap_or(Duration::ZERO);
    // Round up so a retry never starts before the requested date.
    let millis = delay.as_nanos().div_ceil(1_000_000);
    u64::try_from(millis).ok().map(Duration::from_millis)
}

/// Attaches a server-provided `Retry-After` delay to a retryable error.
///
/// Errors in a non-retryable category are returned unchanged.
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, ErrorCategory, ErrorComponent};
/// use adk_model::retry::with_retry_after;
/// use std::time::Duration;
///
/// let error = AdkError::new(ErrorComponent::Model, ErrorCategory::Unavailable, "m.e", "down");
/// let error = with_retry_after(error, Some(Duration::from_secs(2)));
/// assert_eq!(error.retry.retry_after(), Some(Duration::from_secs(2)));
/// ```
#[must_use]
pub fn with_retry_after(error: AdkError, retry_after: Option<Duration>) -> AdkError {
    match retry_after {
        Some(delay) if error.retry.should_retry => {
            let retry = error.retry.clone().with_retry_after(delay);
            error.with_retry(retry)
        }
        _ => error,
    }
}

/// Returns `true` if the error message contains patterns indicating a transient failure.
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

/// Returns `true` if the `AdkError` should be retried based on its retry hint or message.
#[must_use]
pub fn is_retryable_model_error(error: &AdkError) -> bool {
    // Primary: use structured retry hint (single source of truth)
    if error.retry.should_retry {
        return true;
    }
    // Fallback: for backward-compat `.legacy` errors during transition,
    // check the error message for retryable patterns
    if error.code.ends_with(".legacy") && error.is_model() {
        return is_retryable_error_message(&error.message);
    }
    false
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

/// Execute an operation with automatic retry on transient errors.
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
///
/// A server-provided delay is honored without shortening it, up to
/// [`RetryConfig::max_server_delay`]; a larger hint returns the original error.
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
    execute_with_retry_observer(retry_config, classify_error, server_hint, operation, |_, _, _| {
        std::future::ready(Ok(()))
    })
    .await
}

/// Execute with one retry owner and an asynchronous observation before each backoff.
///
/// The observer receives the next retry number, retry limit and actual delay.
/// An observer error stops recovery before sleeping or invoking the operation again.
/// Disabled retries and rejected server delays do not call the observer.
///
/// # Example
///
/// ```rust
/// use adk_core::AdkError;
/// use adk_model::retry::{RetryConfig, execute_with_retry_observer};
///
/// # tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap().block_on(async {
/// let mut calls = 0;
/// let result = execute_with_retry_observer(
///     &RetryConfig::default(),
///     AdkError::is_retryable,
///     None,
///     &mut || {
///         calls += 1;
///         std::future::ready(Err::<(), _>(AdkError::unavailable(adk_core::ErrorComponent::Model, "model.busy", "busy")))
///     },
///     |_, _, _| std::future::ready(Err(AdkError::model("retry cancelled"))),
/// ).await;
/// assert!(result.is_err());
/// assert_eq!(calls, 1);
/// # });
/// ```
pub async fn execute_with_retry_observer<T, Op, Fut, Classify, Observe, ObserveFut>(
    retry_config: &RetryConfig,
    classify_error: Classify,
    server_hint: Option<&ServerRetryHint>,
    operation: &mut Op,
    mut observe: Observe,
) -> Result<T>
where
    Op: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
    Classify: Fn(&AdkError) -> bool,
    Observe: FnMut(u32, u32, Duration) -> ObserveFut,
    ObserveFut: Future<Output = Result<()>>,
{
    if !retry_config.enabled {
        return operation().await;
    }

    let mut attempt: u32 = 0;
    let mut delay = retry_config.initial_delay.min(retry_config.max_delay);

    // If the server provided a retry-after hint, use it for the first retry delay.
    let server_delay = server_hint.and_then(|h| h.retry_after);

    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) if attempt < retry_config.max_retries && classify_error(&error) => {
                // Priority: error hint, initial server hint, then bounded backoff.
                let requested = error
                    .retry
                    .retry_after()
                    .or_else(|| (attempt == 0).then_some(server_delay).flatten());
                if requested.is_some_and(|delay| delay > retry_config.max_server_delay) {
                    return Err(error);
                }
                let effective_delay = requested.unwrap_or(delay);
                if tokio::time::Instant::now().checked_add(effective_delay).is_none() {
                    return Err(error);
                }
                attempt += 1;
                observe(attempt, retry_config.max_retries, effective_delay).await?;

                // The message can echo the request, and the client sees no `RunConfig`, so
                // only the content-free part is logged.
                adk_telemetry::warn!(
                    attempt = attempt,
                    max_retries = retry_config.max_retries,
                    delay_ms = effective_delay.as_millis(),
                    error = %error.diagnostics(),
                    "provider request failed with a retryable error; retrying"
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
                    return Err(AdkError::model("HTTP 429 rate limit"));
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
                Err::<(), _>(AdkError::model("HTTP 400 bad request"))
            }
        })
        .await
        .expect_err("operation should fail without retries");

        assert!(error.is_model());
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
                Err::<(), _>(AdkError::model("HTTP 429 too many requests"))
            }
        })
        .await
        .expect_err("disabled retries should return first error");

        assert!(error.is_model());
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

    #[tokio::test]
    async fn refuses_excessive_server_delays() {
        let retry_config = RetryConfig::default()
            .with_max_retries(1)
            .with_initial_delay(Duration::ZERO)
            .with_max_delay(Duration::from_millis(20));
        let attempts = Arc::new(AtomicU32::new(0));
        let started = std::time::Instant::now();

        let result = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let attempts = Arc::clone(&attempts);
            async move {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    let error = AdkError::new(
                        adk_core::ErrorComponent::Model,
                        ErrorCategory::Unavailable,
                        "model.test.unavailable",
                        "overloaded",
                    );
                    return Err(with_retry_after(error, Some(Duration::from_secs(3600))));
                }
                Ok("ok")
            }
        })
        .await;

        let error = result.expect_err("excessive server delay must not be shortened");
        assert_eq!(error.category, ErrorCategory::Unavailable);
        assert_eq!(error.retry.retry_after(), Some(Duration::from_secs(3600)));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn refuses_excessive_initial_hints() {
        let retry_config = RetryConfig::default()
            .with_max_retries(1)
            .with_initial_delay(Duration::ZERO)
            .with_max_delay(Duration::from_millis(20));
        let hint = ServerRetryHint { retry_after: Some(Duration::from_secs(3600)) };
        let attempts = Arc::new(AtomicU32::new(0));
        let started = std::time::Instant::now();

        let result = execute_with_retry_hint(
            &retry_config,
            is_retryable_model_error,
            Some(&hint),
            &mut || {
                let attempts = Arc::clone(&attempts);
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err(AdkError::model("HTTP 503 unavailable"));
                    }
                    Ok("ok")
                }
            },
        )
        .await;

        let error = result.expect_err("excessive initial hint must not be shortened");
        assert_eq!(error.message, "HTTP 503 unavailable");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn transient_server_statuses_map_to_retryable_unavailable() {
        for status in [500, 502, 503, 504, 520, 521, 522, 523, 524, 529] {
            assert_eq!(category_for_status_code(status), ErrorCategory::Unavailable, "{status}");
        }
        assert_eq!(category_for_status_code(501), ErrorCategory::Internal);
        assert_eq!(category_for_status_code(429), ErrorCategory::RateLimited);
        assert_eq!(category_for_status_code(408), ErrorCategory::Timeout);
        assert_eq!(category_for_status_code(422), ErrorCategory::InvalidInput);
    }

    #[test]
    fn retry_after_is_attached_only_to_retryable_errors() {
        let delay = Some(Duration::from_secs(4));
        let unavailable = AdkError::new(
            adk_core::ErrorComponent::Model,
            ErrorCategory::Unavailable,
            "model.test",
            "down",
        );
        let invalid = AdkError::new(
            adk_core::ErrorComponent::Model,
            ErrorCategory::InvalidInput,
            "model.test",
            "bad",
        );

        assert_eq!(
            with_retry_after(unavailable, delay).retry.retry_after(),
            Some(Duration::from_secs(4))
        );
        assert_eq!(with_retry_after(invalid, delay).retry.retry_after(), None);
        assert_eq!(parse_retry_after("-1"), None);
        assert_eq!(parse_retry_after(" 30 "), Some(Duration::from_secs(30)));
    }

    #[test]
    fn parses_dates_and_rejects_invalid_seconds() {
        let future = std::time::SystemTime::now() + Duration::from_secs(30);
        for date in [
            httpdate::fmt_http_date(future),
            "Sunday, 06-Nov-94 08:49:37 GMT".to_string(),
            "Sun Nov  6 08:49:37 1994".to_string(),
        ] {
            let delay = parse_retry_after(&date).expect("HTTP-date should parse");
            assert!(delay <= Duration::from_secs(30), "{date}");
        }
        assert!(parse_retry_after(&httpdate::fmt_http_date(future)).unwrap() > Duration::ZERO);

        assert_eq!(parse_retry_after("61"), Some(Duration::from_secs(61)));
        assert_eq!(parse_retry_after("3600"), Some(Duration::from_secs(3600)));
        for invalid in [
            "",
            "soon",
            "inf",
            "NaN",
            "-0.5",
            "-1",
            "+1",
            "1.5",
            "1e300",
            "18446744073709551616",
            "18446744073709552",
            "1\n",
        ] {
            assert_eq!(parse_retry_after(invalid), None, "{invalid:?}");
        }
    }

    #[tokio::test]
    async fn observer_receives_accepted_delays() {
        let config = RetryConfig::default()
            .with_max_retries(1)
            .with_max_delay(Duration::ZERO)
            .with_max_server_delay(Duration::from_millis(30));
        let mut calls = 0;
        let mut observations = Vec::new();
        let started = std::time::Instant::now();
        let result = execute_with_retry_observer(
            &config,
            AdkError::is_retryable,
            None,
            &mut || {
                calls += 1;
                std::future::ready(if calls == 1 {
                    Err(with_retry_after(
                        AdkError::unavailable(
                            adk_core::ErrorComponent::Model,
                            "model.busy",
                            "busy",
                        ),
                        Some(Duration::from_millis(30)),
                    ))
                } else {
                    Ok("done")
                })
            },
            |attempt, limit, delay| {
                observations.push((attempt, limit, delay));
                std::future::ready(Ok(()))
            },
        )
        .await
        .unwrap();
        assert_eq!(result, "done");
        assert_eq!(calls, 2);
        assert_eq!(observations, [(1, 1, Duration::from_millis(30))]);
        assert!(started.elapsed() >= Duration::from_millis(30));
    }

    #[tokio::test]
    async fn observer_can_cancel_before_replay() {
        let mut calls = 0;
        let mut observations = 0;
        let error = execute_with_retry_observer(
            &RetryConfig::default(),
            AdkError::is_retryable,
            None,
            &mut || {
                calls += 1;
                std::future::ready(Err::<(), _>(AdkError::unavailable(
                    adk_core::ErrorComponent::Model,
                    "model.busy",
                    "busy",
                )))
            },
            |attempt, limit, delay| {
                observations += 1;
                assert_eq!((attempt, limit, delay), (1, 3, Duration::from_millis(250)));
                std::future::ready(Err(AdkError::model("cancelled")))
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.message, "cancelled");
        assert_eq!((calls, observations), (1, 1));
    }

    #[tokio::test]
    async fn rejected_hints_skip_the_observer() {
        for seconds in [61, 3600] {
            let mut calls = 0;
            let mut observations = 0;
            let error = execute_with_retry_observer(
                &RetryConfig::default(),
                AdkError::is_retryable,
                None,
                &mut || {
                    calls += 1;
                    std::future::ready(Err::<(), _>(with_retry_after(
                        AdkError::unavailable(
                            adk_core::ErrorComponent::Model,
                            "model.busy",
                            "original",
                        ),
                        Some(Duration::from_secs(seconds)),
                    )))
                },
                |_, _, _| {
                    observations += 1;
                    std::future::ready(Ok(()))
                },
            )
            .await
            .unwrap_err();
            assert_eq!(error.message, "original");
            assert_eq!(error.retry.retry_after(), Some(Duration::from_secs(seconds)));
            assert_eq!((calls, observations), (1, 0));
        }
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
                        return Err(AdkError::model("HTTP 429 rate limit"));
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
                    return Err(AdkError::model("HTTP 529 overloaded"));
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
                    return Err(AdkError::model("HTTP 429 rate limit"));
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
