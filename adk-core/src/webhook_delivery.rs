//! Signed outbound webhook delivery with retries, a dead-letter sink, and an egress guard.
//!
//! [`WebhookDelivery`] (feature `webhook-delivery`) sends one message to one endpoint:
//!
//! | Step | Behaviour |
//! |------|-----------|
//! | Egress check | The URL must pass the [`EgressPolicy`] (default [`EgressPolicy::public_only`]: HTTPS, and a host that is not, and does not resolve to, a loopback, private, link-local, cloud metadata or other non-public address). Requests go through a [`GuardedClient`], so the address checked is the address connected to; when that client cannot be built, every delivery is dead-lettered and none is sent |
//! | Signing | Every attempt is signed with [Standard Webhooks](crate::webhook) under the message's stable `webhook-id` and a fresh `webhook-timestamp` |
//! | Success | Any `2xx` response |
//! | Retry | Connection errors, timeouts, `408`, `425`, `429`, and `5xx`, with exponential backoff and full jitter, honouring `Retry-After` up to the backoff cap |
//! | Dead letter | Any other status, an egress refusal, or the last failed attempt; the message goes to the [`DeadLetterSink`] with its last error |
//!
//! Redirects are never followed: a public endpoint could otherwise bounce the request to
//! an internal one. Logs name the endpoint's origin, never its path or query, which may
//! carry a token.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use adk_core::webhook::WebhookSecret;
//! use adk_core::webhook_delivery::{
//!     InMemoryDeadLetterSink, RetryPolicy, WebhookDelivery, WebhookEndpoint, WebhookMessage,
//! };
//! use serde_json::json;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let dead_letters = Arc::new(InMemoryDeadLetterSink::new());
//! let delivery = WebhookDelivery::new()
//!     .with_retry_policy(RetryPolicy::default().with_max_attempts(5))
//!     .with_dead_letter_sink(dead_letters.clone());
//!
//! let endpoint = WebhookEndpoint::new(
//!     "https://hooks.example.com/runs",
//!     WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?,
//! );
//! let message = WebhookMessage::json("run_42.completed", &json!({"runId": "run_42", "state": "completed"}))?;
//! let report = delivery.deliver(&endpoint, &message).await;
//! assert!(report.is_delivered() || !dead_letters.letters().is_empty());
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::egress::{EgressPolicy, GuardedClient, find_denial};
use crate::webhook::{WebhookSecret, WebhookSigner};
use crate::{Clock, SystemClock};

/// How many attempts a delivery makes and how long it waits between them.
///
/// The wait before attempt `n + 1` is a uniformly random duration in
/// `[0, min(max_backoff, initial_backoff × 2ⁿ⁻¹)]` ("full jitter"), so many senders
/// retrying one recovering endpoint spread out instead of arriving together. A
/// `Retry-After` header raises the wait to the value it names, up to `max_backoff`.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::webhook_delivery::RetryPolicy;
///
/// let policy = RetryPolicy::default()
///     .with_max_attempts(8)
///     .with_initial_backoff(Duration::from_millis(500))
///     .with_max_backoff(Duration::from_secs(60));
/// assert_eq!(policy.backoff_cap(1), Duration::from_millis(500));
/// assert_eq!(policy.backoff_cap(4), Duration::from_secs(4));
/// assert_eq!(policy.backoff_cap(20), Duration::from_secs(60));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts before the message is dead-lettered; at least 1.
    pub max_attempts: u32,
    /// The backoff cap after the first failed attempt.
    pub initial_backoff: Duration,
    /// The largest wait between attempts.
    pub max_backoff: Duration,
    /// The bound on one attempt, from connect to response.
    pub attempt_timeout: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(300),
            attempt_timeout: Duration::from_secs(10),
        }
    }
}

impl RetryPolicy {
    /// Sets the attempts before dead-lettering; values below 1 become 1.
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts.max(1);
        self
    }

    /// Sets the backoff cap after the first failed attempt.
    pub fn with_initial_backoff(mut self, backoff: Duration) -> Self {
        self.initial_backoff = backoff;
        self
    }

    /// Sets the largest wait between attempts.
    pub fn with_max_backoff(mut self, backoff: Duration) -> Self {
        self.max_backoff = backoff;
        self
    }

    /// Sets the bound on one attempt.
    pub fn with_attempt_timeout(mut self, timeout: Duration) -> Self {
        self.attempt_timeout = timeout;
        self
    }

    /// The upper bound of the jittered wait after failed attempt `attempt` (1-based).
    pub fn backoff_cap(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1));
        self.initial_backoff.saturating_mul(factor).min(self.max_backoff)
    }
}

/// One receiver of webhook deliveries.
#[derive(Debug, Clone)]
pub struct WebhookEndpoint {
    /// The URL deliveries are POSTed to.
    pub url: String,
    signer: WebhookSigner,
}

impl WebhookEndpoint {
    /// Creates an endpoint whose deliveries are signed with `secret`.
    pub fn new(url: impl Into<String>, secret: WebhookSecret) -> Self {
        Self { url: url.into(), signer: WebhookSigner::new(secret) }
    }

    /// Creates an endpoint with a configured signer, for example one carrying two
    /// secrets during a rotation.
    pub fn with_signer(url: impl Into<String>, signer: WebhookSigner) -> Self {
        Self { url: url.into(), signer }
    }
}

/// One message to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookMessage {
    /// The `webhook-id`: stable across every retry, so receivers deduplicate on it.
    pub id: String,
    /// The raw body.
    pub body: Vec<u8>,
    /// The `content-type` header.
    pub content_type: String,
}

impl WebhookMessage {
    /// A message with a JSON body.
    ///
    /// # Errors
    ///
    /// Returns the serialization error.
    pub fn json(id: impl Into<String>, body: &impl Serialize) -> serde_json::Result<Self> {
        Ok(Self {
            id: id.into(),
            body: serde_json::to_vec(body)?,
            content_type: "application/json".to_string(),
        })
    }
}

/// A message that could not be delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeadLetter {
    /// The message's `webhook-id`.
    pub message_id: String,
    /// The endpoint URL.
    pub url: String,
    /// Attempts made.
    pub attempts: u32,
    /// The last HTTP status, when a response arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<u16>,
    /// Why the last attempt failed.
    pub last_error: String,
    /// The body, for a later redrive.
    pub body: Vec<u8>,
    /// When the message was dead-lettered.
    pub failed_at: DateTime<Utc>,
}

/// Where undeliverable messages go.
#[async_trait]
pub trait DeadLetterSink: Send + Sync {
    /// Stores `letter`.
    ///
    /// # Errors
    ///
    /// Returns an error when the sink cannot be written; the delivery logs it.
    async fn dead_letter(&self, letter: DeadLetter) -> crate::Result<()>;
}

/// Process-local [`DeadLetterSink`] for tests and single-process deployments.
#[derive(Debug, Default)]
pub struct InMemoryDeadLetterSink {
    letters: std::sync::Mutex<Vec<DeadLetter>>,
}

impl InMemoryDeadLetterSink {
    /// Creates an empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// The letters received so far, oldest first.
    pub fn letters(&self) -> Vec<DeadLetter> {
        self.letters.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl DeadLetterSink for InMemoryDeadLetterSink {
    async fn dead_letter(&self, letter: DeadLetter) -> crate::Result<()> {
        self.letters.lock().unwrap_or_else(|e| e.into_inner()).push(letter);
        Ok(())
    }
}

/// The result of one [`WebhookDelivery::deliver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryReport {
    /// The endpoint answered `2xx`.
    Delivered {
        /// Attempts made, including the successful one.
        attempts: u32,
        /// The success status.
        status: u16,
    },
    /// The message went to the dead-letter sink.
    DeadLettered(DeadLetter),
}

impl DeliveryReport {
    /// Whether the endpoint accepted the message.
    pub fn is_delivered(&self) -> bool {
        matches!(self, Self::Delivered { .. })
    }
}

enum Attempt {
    Delivered(u16),
    Retry { status: Option<u16>, error: String, retry_after: Option<Duration> },
    Permanent { status: Option<u16>, error: String },
}

/// Sends signed webhooks with retries and dead-lettering.
///
/// Clones share the HTTP client and the dead-letter sink.
#[derive(Clone)]
pub struct WebhookDelivery {
    /// The client that enforces the egress policy, or why it could not be built.
    client: Result<GuardedClient, String>,
    retry: RetryPolicy,
    egress: EgressPolicy,
    dead_letters: Option<Arc<dyn DeadLetterSink>>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for WebhookDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookDelivery")
            .field("retry", &self.retry)
            .field("egress", &self.egress)
            .field("dead_letters", &self.dead_letters.is_some())
            .finish()
    }
}

impl Default for WebhookDelivery {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookDelivery {
    /// Creates a sender with the default [`RetryPolicy`] and [`EgressPolicy`] and no
    /// dead-letter sink (undeliverable messages are logged).
    pub fn new() -> Self {
        Self::configured(RetryPolicy::default(), EgressPolicy::default())
    }

    fn configured(retry: RetryPolicy, egress: EgressPolicy) -> Self {
        // Never a default client in its place: it would skip the egress checks, use the
        // system proxies and follow redirects.
        let client = GuardedClient::builder(egress.clone())
            .timeout(retry.attempt_timeout)
            .connect_timeout(retry.attempt_timeout.min(Duration::from_secs(5)))
            .redirects(0)
            .user_agent(concat!("adk-rust-webhooks/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                tracing::error!(error = %error, "webhook client failed to build; every delivery is refused");
                error.to_string()
            });
        Self { client, retry, egress, dead_letters: None, clock: Arc::new(SystemClock) }
    }

    /// Sets the retry policy.
    pub fn with_retry_policy(self, retry: RetryPolicy) -> Self {
        let Self { dead_letters, clock, egress, .. } = self;
        Self { dead_letters, clock, ..Self::configured(retry, egress) }
    }

    /// Sets the egress policy.
    pub fn with_egress_policy(self, egress: EgressPolicy) -> Self {
        let Self { dead_letters, clock, retry, .. } = self;
        Self { dead_letters, clock, ..Self::configured(retry, egress) }
    }

    /// Sets where undeliverable messages go.
    pub fn with_dead_letter_sink(mut self, sink: Arc<dyn DeadLetterSink>) -> Self {
        self.dead_letters = Some(sink);
        self
    }

    /// Sets the clock that timestamps signatures and dead letters.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The retry policy.
    pub fn retry_policy(&self) -> RetryPolicy {
        self.retry
    }

    /// The egress policy.
    pub fn egress_policy(&self) -> &EgressPolicy {
        &self.egress
    }

    /// Delivers `message` to `endpoint`, retrying until it succeeds, fails permanently,
    /// or runs out of attempts, and dead-letters it in the last two cases.
    ///
    /// A sender whose HTTP client could not be built dead-letters every message without an
    /// attempt.
    pub async fn deliver(
        &self,
        endpoint: &WebhookEndpoint,
        message: &WebhookMessage,
    ) -> DeliveryReport {
        if let Err(denial) = self.egress.check_url(&endpoint.url) {
            return self.dead_letter(endpoint, message, 0, None, denial.to_string()).await;
        }
        let client = match &self.client {
            Ok(client) => client,
            Err(reason) => {
                return self.dead_letter(endpoint, message, 0, None, reason.clone()).await;
            }
        };
        let mut attempt = 0;
        loop {
            attempt += 1;
            let (status, error, retry_after) = match self.attempt(client, endpoint, message).await {
                Attempt::Delivered(status) => {
                    tracing::debug!(webhook.id = %message.id, http.status = status, attempt, "webhook delivered");
                    return DeliveryReport::Delivered { attempts: attempt, status };
                }
                Attempt::Permanent { status, error } => {
                    return self.dead_letter(endpoint, message, attempt, status, error).await;
                }
                Attempt::Retry { status, error, retry_after } => (status, error, retry_after),
            };
            if attempt >= self.retry.max_attempts {
                return self.dead_letter(endpoint, message, attempt, status, error).await;
            }
            let wait = jittered(self.retry.backoff_cap(attempt))
                .max(retry_after.unwrap_or_default())
                .min(self.retry.max_backoff);
            tracing::debug!(
                webhook.id = %message.id,
                attempt,
                retry.wait_ms = wait.as_millis() as u64,
                error = %error,
                "webhook attempt failed; retrying"
            );
            tokio::time::sleep(wait).await;
        }
    }

    async fn attempt(
        &self,
        client: &GuardedClient,
        endpoint: &WebhookEndpoint,
        message: &WebhookMessage,
    ) -> Attempt {
        let signed = endpoint.signer.sign(&message.id, self.clock.now().timestamp(), &message.body);
        let mut request = match client.post(&endpoint.url) {
            Ok(request) => request,
            Err(denial) => return Attempt::Permanent { status: None, error: denial.to_string() },
        };
        request = request
            .header(reqwest::header::CONTENT_TYPE, &message.content_type)
            .body(message.body.clone());
        for (name, value) in signed.header_pairs() {
            request = request.header(name, value);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                // The URL's path or query may carry a token; the error is logged and kept.
                let error = error.without_url();
                let refused = find_denial(&error).is_some();
                let text = full_error(&error);
                return if refused {
                    Attempt::Permanent { status: None, error: text }
                } else {
                    Attempt::Retry { status: None, error: text, retry_after: None }
                };
            }
        };
        let status = response.status().as_u16();
        if response.status().is_success() {
            return Attempt::Delivered(status);
        }
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let error = format!("endpoint answered HTTP {status}");
        match status {
            408 | 425 | 429 | 500..=599 => {
                Attempt::Retry { status: Some(status), error, retry_after }
            }
            _ => Attempt::Permanent { status: Some(status), error },
        }
    }

    async fn dead_letter(
        &self,
        endpoint: &WebhookEndpoint,
        message: &WebhookMessage,
        attempts: u32,
        last_status: Option<u16>,
        last_error: String,
    ) -> DeliveryReport {
        let letter = DeadLetter {
            message_id: message.id.clone(),
            url: endpoint.url.clone(),
            attempts,
            last_status,
            last_error,
            body: message.body.clone(),
            failed_at: self.clock.now(),
        };
        // The origin only: the URL's path or query may carry a token.
        let origin = reqwest::Url::parse(&letter.url)
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_else(|_| "an invalid URL".to_string());
        tracing::warn!(
            webhook.id = %letter.message_id,
            webhook.origin = %origin,
            attempts,
            error = %letter.last_error,
            "webhook dead-lettered"
        );
        if let Some(sink) = &self.dead_letters
            && let Err(error) = sink.dead_letter(letter.clone()).await
        {
            tracing::error!(webhook.id = %letter.message_id, error = %error, "dead-letter sink write failed");
        }
        DeliveryReport::DeadLettered(letter)
    }
}

/// The error and its sources, so a refusal's reason is not hidden behind "error sending request".
fn full_error(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// A uniformly random duration in `[0, cap]`.
fn jittered(cap: Duration) -> Duration {
    // uuid v4 draws from the OS random source; 64 of its random bits are plenty here.
    let random = uuid::Uuid::new_v4().as_u64_pair().0;
    let fraction = (random >> 11) as f64 / (1u64 << 53) as f64;
    cap.mul_f64(fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let policy = RetryPolicy::default()
            .with_initial_backoff(Duration::from_millis(100))
            .with_max_backoff(Duration::from_millis(500));
        let caps: Vec<_> = (1..=5).map(|n| policy.backoff_cap(n).as_millis()).collect();
        assert_eq!(caps, vec![100, 200, 400, 500, 500]);
        for _ in 0..100 {
            assert!(jittered(Duration::from_millis(100)) <= Duration::from_millis(100));
        }
    }

    /// A name that resolves to an address the policy refuses is dead-lettered after one
    /// attempt: the refusal is permanent, not retried.
    #[tokio::test]
    async fn a_resolution_refusal_is_not_retried() {
        use crate::egress::StaticResolver;
        let resolver =
            StaticResolver::new().with_host("hooks.internal.test", ["10.0.0.9".parse().unwrap()]);
        let client = GuardedClient::builder(EgressPolicy::public_only())
            .resolver(Arc::new(resolver))
            .redirects(0)
            .build()
            .unwrap();
        let sink = Arc::new(InMemoryDeadLetterSink::new());
        let delivery = WebhookDelivery {
            client: Ok(client),
            ..WebhookDelivery::new()
                .with_retry_policy(RetryPolicy::default().with_max_attempts(5))
                .with_dead_letter_sink(sink.clone())
        };
        let endpoint = WebhookEndpoint::new(
            "https://hooks.internal.test/runs",
            WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap(),
        );
        let message = WebhookMessage::json("msg_2", &serde_json::json!({ "ok": true })).unwrap();

        let DeliveryReport::DeadLettered(letter) = delivery.deliver(&endpoint, &message).await
        else {
            panic!("a refused destination must not be delivered");
        };
        assert_eq!(letter.attempts, 1);
        assert!(letter.last_error.contains("10.0.0.9"), "{}", letter.last_error);
        assert_eq!(sink.letters().len(), 1);
    }

    /// A sender whose guarded client did not build sends nothing: no fallback client reaches
    /// the endpoint, and the message is dead-lettered with the reason.
    #[tokio::test]
    async fn a_client_that_did_not_build_refuses_every_delivery() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let sink = Arc::new(InMemoryDeadLetterSink::new());
        let delivery = WebhookDelivery {
            client: Err("the webhook HTTP client could not be built (no TLS)".to_string()),
            ..WebhookDelivery::new()
                .with_egress_policy(EgressPolicy::allow_private_networks())
                .with_dead_letter_sink(sink.clone())
        };
        let endpoint = WebhookEndpoint::new(
            format!("http://{address}/hook"),
            WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap(),
        );
        let message = WebhookMessage::json("msg_1", &serde_json::json!({ "ok": true })).unwrap();

        let report = delivery.deliver(&endpoint, &message).await;

        let DeliveryReport::DeadLettered(letter) = report else {
            panic!("a delivery without its guarded client must not succeed: {report:?}");
        };
        assert_eq!(letter.attempts, 0);
        assert_eq!(letter.last_error, "the webhook HTTP client could not be built (no TLS)");
        assert_eq!(sink.letters(), vec![letter]);
        let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
        assert!(accepted.is_err(), "no request reached the endpoint");

        // Under the default, public-only policy too: a public URL gets no attempt.
        let guarded = WebhookDelivery {
            client: Err("the webhook HTTP client could not be built (no TLS)".to_string()),
            ..WebhookDelivery::new()
        };
        let public = WebhookEndpoint::new(
            "https://hooks.example.com/runs",
            WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap(),
        );
        match guarded.deliver(&public, &message).await {
            DeliveryReport::DeadLettered(letter) => assert_eq!(letter.attempts, 0),
            other => panic!("a delivery without its guarded client must not succeed: {other:?}"),
        }
    }
}
