//! The HTTP client for the CV endpoints: a client-side rate limit, one retry on a worker
//! error, and NVIDIA's statuses mapped to [`VisionError`].

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use serde_json::Value;
use tokio::sync::Mutex;

use super::config::NimCvConfig;
use crate::error::{Result, VisionError};

/// At most `per_minute` requests in any 60 s window.
#[derive(Debug)]
struct RateLimiter {
    per_minute: usize,
    sent: Mutex<VecDeque<Instant>>,
}

impl RateLimiter {
    fn new(per_minute: u32) -> Self {
        Self { per_minute: per_minute.max(1) as usize, sent: Mutex::new(VecDeque::new()) }
    }

    async fn acquire(&self) {
        const WINDOW: Duration = Duration::from_secs(60);
        loop {
            let wait = {
                let mut sent = self.sent.lock().await;
                let now = Instant::now();
                while sent.front().is_some_and(|t| now.duration_since(*t) >= WINDOW) {
                    sent.pop_front();
                }
                if sent.len() < self.per_minute {
                    sent.push_back(now);
                    return;
                }
                sent.front().map_or(WINDOW, |oldest| WINDOW - now.duration_since(*oldest))
            };
            tracing::debug!(
                nim.wait_ms = wait.as_millis() as u64,
                "client-side rate limit reached"
            );
            tokio::time::sleep(wait).await;
        }
    }
}

/// Calls CV models as one backend.
#[derive(Debug, Clone)]
pub(crate) struct CvClient {
    http: reqwest::Client,
    config: Arc<NimCvConfig>,
    limiter: Arc<RateLimiter>,
}

impl CvClient {
    pub(crate) fn new(config: NimCvConfig) -> Result<Self> {
        let http = match &config.http {
            Some(client) => client.clone(),
            None => reqwest::Client::builder()
                .timeout(config.timeout())
                .connect_timeout(Duration::from_secs(30))
                .user_agent(concat!("adk-vision/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| {
                    VisionError::InvalidInput(format!("the NIM HTTP client failed to start: {e}"))
                })?,
        };
        let limiter = Arc::new(RateLimiter::new(config.requests_per_minute()));
        Ok(Self { http, config: Arc::new(config), limiter })
    }

    pub(crate) fn config(&self) -> &NimCvConfig {
        &self.config
    }

    /// Posts `body` to `model` and returns the answer's JSON. A worker error (500, 502,
    /// 503) is retried once after a second.
    pub(crate) async fn infer(&self, backend: &str, model: &str, body: &Value) -> Result<Value> {
        let url = self.config.url_for(model);
        for attempt in 0..2 {
            self.limiter.acquire().await;
            let mut request =
                self.http.post(&url).header(reqwest::header::ACCEPT, "application/json").json(body);
            if let Some(key) = self.config.api_key() {
                request = request.bearer_auth(key);
            }
            let started = Instant::now();
            let response = request.send().await.map_err(|e| transport(backend, model, &e))?;
            let status = response.status().as_u16();
            tracing::debug!(
                nim.model = model,
                http.status = status,
                http.duration_ms = started.elapsed().as_millis() as u64,
                "nim request finished"
            );
            if (200..300).contains(&status) {
                let bytes = response.bytes().await.map_err(|e| transport(backend, model, &e))?;
                return serde_json::from_slice(&bytes).map_err(|e| VisionError::Parse {
                    backend: backend.to_string(),
                    message: format!("`{model}` answered with invalid JSON: {e}"),
                });
            }
            let headers = response.headers().clone();
            let content_type = headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let retry_after = headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let text = response.text().await.unwrap_or_default();
            let err = error_for(backend, model, &url, status, &content_type, &text, retry_after);
            if attempt == 0 && matches!(status, 500 | 502 | 503) {
                tracing::warn!(nim.model = model, http.status = status, error = %err, "retrying once");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            return Err(err);
        }
        unreachable!("the loop returns on its second attempt")
    }
}

fn transport(backend: &str, model: &str, err: &reqwest::Error) -> VisionError {
    if err.is_timeout() {
        VisionError::Timeout { backend: backend.to_string() }
    } else {
        // The URL is the endpoint's, never the key's; the message names it for the operator.
        VisionError::Unavailable {
            backend: backend.to_string(),
            message: format!("`{model}` could not be reached: {err}"),
        }
    }
}

fn core(category: ErrorCategory, code: &'static str, message: String) -> VisionError {
    VisionError::Core(
        AdkError::new(ErrorComponent::Tool, category, code, message).with_provider("nvidia"),
    )
}

/// The human part of an error body: a problem's `detail`, an `error.message`, an `error`
/// string, or a `message`; the raw text otherwise. Bounded, so a large body cannot flood a
/// log line.
fn detail(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let text = parsed
        .as_ref()
        .and_then(|v| {
            v.get("detail")
                .and_then(Value::as_str)
                .or_else(|| v.pointer("/error/message").and_then(Value::as_str))
                .or_else(|| v.get("error").and_then(Value::as_str))
                .or_else(|| v.get("message").and_then(Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string());
    let mut text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > 300 {
        text = text.chars().take(300).collect::<String>() + "…";
    }
    text
}

/// Maps an NVIDIA error answer to a [`VisionError`] (the recorded taxonomy in the NIM
/// probe's `errors/` fixtures).
pub(crate) fn error_for(
    backend: &str,
    model: &str,
    url: &str,
    status: u16,
    content_type: &str,
    body: &str,
    retry_after: Option<Duration>,
) -> VisionError {
    match status {
        429 => VisionError::RateLimited { backend: backend.to_string(), retry_after },
        401 | 403 => core(
            ErrorCategory::Unauthorized,
            "vision.nim.unauthorized",
            format!(
                "NVIDIA refused the API key for `{model}` (HTTP {status}); set NVIDIA_API_KEY to a key from build.nvidia.com"
            ),
        ),
        404 if content_type.starts_with("text/plain") => core(
            ErrorCategory::NotFound,
            "vision.nim.no_route",
            format!(
                "no NIM endpoint answers at {url} for `{model}`; check the base URL and the model name"
            ),
        ),
        // An NVCF problem body names the caller's account, so it is not repeated.
        404 => core(
            ErrorCategory::Forbidden,
            "vision.nim.not_entitled",
            format!(
                "model `{model}` is not available to this API key (NVCF: function not found for the account); request access on build.nvidia.com or configure another model"
            ),
        ),
        410 => core(
            ErrorCategory::NotFound,
            "vision.nim.retired",
            format!("`{model}` is retired: {}; configure its successor", detail(body)),
        ),
        422 => VisionError::InvalidInput(format!(
            "`{model}` rejected the request body: {}",
            detail(body)
        )),
        400 => VisionError::Unsupported(format!("`{model}` rejected the input: {}", detail(body))),
        500 | 502 | 503 => VisionError::Unavailable {
            backend: backend.to_string(),
            message: format!("`{model}` failed (HTTP {status}): {}", detail(body)),
        },
        504 => VisionError::Timeout { backend: backend.to_string() },
        _ => VisionError::Backend {
            backend: backend.to_string(),
            message: format!("`{model}` answered HTTP {status}: {}", detail(body)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_limiter_holds_a_request_past_the_rate() {
        let limiter = RateLimiter::new(2);
        let started = Instant::now();
        limiter.acquire().await;
        limiter.acquire().await;
        assert!(started.elapsed() < Duration::from_secs(1));
        // The third request in the minute waits for the first to age out.
        let third = tokio::time::timeout(Duration::from_millis(50), limiter.acquire()).await;
        assert!(third.is_err(), "a third request went out inside the window");
    }

    #[test]
    fn details_are_bounded_and_single_line() {
        let long = format!("{{\"detail\": \"{}\\nmore\"}}", "x".repeat(400));
        let text = detail(&long);
        assert!(text.chars().count() <= 301);
        assert!(!text.contains('\n'));
        assert_eq!(detail("404 page not found\n"), "404 page not found");
        assert_eq!(
            detail(r#"{"error": {"message": "Invalid PaLiGemma media request"}}"#),
            "Invalid PaLiGemma media request"
        );
    }
}
