//! Signed outbound webhook delivery with retries, a dead-letter sink, and an egress guard.
//!
//! [`WebhookDelivery`] (feature `webhook-delivery`) sends one message to one endpoint:
//!
//! | Step | Behaviour |
//! |------|-----------|
//! | Egress check | The URL must pass the [`EgressPolicy`]: HTTPS, and a host that is not, and does not resolve to, a loopback, private, link-local, or other non-public address. Resolution happens inside the HTTP client, so the address checked is the address connected to |
//! | Signing | Every attempt is signed with [Standard Webhooks](crate::webhook) under the message's stable `webhook-id` and a fresh `webhook-timestamp` |
//! | Success | Any `2xx` response |
//! | Retry | Connection errors, timeouts, `408`, `425`, `429`, and `5xx`, with exponential backoff and full jitter, honouring `Retry-After` up to the backoff cap |
//! | Dead letter | Any other status, an egress refusal, or the last failed attempt; the message goes to the [`DeadLetterSink`] with its last error |
//!
//! Redirects are never followed: a public endpoint could otherwise bounce the request to
//! an internal one.
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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

/// Which destinations a delivery may reach.
///
/// The default allows HTTPS to public addresses only. Host names are resolved by the
/// HTTP client through a resolver that refuses non-public answers, so a name that
/// resolves to an internal address — including through DNS rebinding — is refused at
/// connect time, not only at registration.
///
/// | Range | Purpose |
/// |-------|---------|
/// | `0.0.0.0/8`, `::` | "this network" / unspecified |
/// | `127.0.0.0/8`, `::1`, `localhost` | loopback |
/// | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | private |
/// | `100.64.0.0/10` | carrier-grade NAT shared space |
/// | `169.254.0.0/16`, `fe80::/10` | link-local, including cloud metadata endpoints |
/// | `fc00::/7`, `fec0::/10` | IPv6 unique-local and deprecated site-local |
/// | `224.0.0.0/4`, `255.255.255.255`, `ff00::/8` | multicast and broadcast |
/// | `::ffff:0:0/96`, `::/96`, `64:ff9b::/96` | IPv6 forms embedding any IPv4 address above |
///
/// # Example
///
/// ```rust
/// use adk_core::webhook_delivery::EgressPolicy;
///
/// let policy = EgressPolicy::default();
/// assert!(policy.check_url("https://hooks.example.com/a").is_ok());
/// assert!(policy.check_url("http://hooks.example.com/a").is_err());
/// assert!(policy.check_url("https://169.254.169.254/latest/meta-data").is_err());
/// assert!(EgressPolicy::allow_private_networks().check_url("http://127.0.0.1:8080/hook").is_ok());
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    /// Allow loopback, private, link-local, and other non-public addresses.
    pub allow_private: bool,
    /// Allow plain `http` URLs.
    pub allow_http: bool,
}

impl EgressPolicy {
    /// Allows any address and plain HTTP, for in-cluster receivers and tests.
    ///
    /// Use only when every endpoint is configured by an operator, never when callers
    /// register their own URLs.
    pub fn allow_private_networks() -> Self {
        Self { allow_private: true, allow_http: true }
    }

    /// Checks a URL's scheme and, when its host is a literal address or `localhost`,
    /// the address. Other host names are checked when the client resolves them.
    ///
    /// # Errors
    ///
    /// Returns the reason the URL is refused.
    pub fn check_url(&self, url: &str) -> Result<reqwest::Url, String> {
        let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid webhook URL: {e}"))?;
        match parsed.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            other => return Err(format!("webhook URL scheme `{other}` is not allowed; use https")),
        }
        if self.allow_private {
            return Ok(parsed);
        }
        let host = parsed.host_str().ok_or_else(|| "webhook URL has no host".to_string())?;
        // IPv6 hosts are serialized in brackets, e.g. `[::1]`.
        let unbracketed = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
        if let Ok(ip) = unbracketed.parse::<IpAddr>() {
            if is_non_public(ip) {
                return Err(format!("webhook URL must not target a non-public address: {ip}"));
            }
        } else {
            let name = host.trim_end_matches('.').to_ascii_lowercase();
            if name == "localhost" || name.ends_with(".localhost") {
                return Err("webhook URL must not target localhost".to_string());
            }
        }
        Ok(parsed)
    }
}

/// Whether a delivery must not reach `ip` under the default [`EgressPolicy`].
///
/// # Example
///
/// ```rust
/// use adk_core::webhook_delivery::is_non_public;
///
/// assert!(is_non_public("10.1.2.3".parse().unwrap()));
/// assert!(is_non_public("::ffff:127.0.0.1".parse().unwrap()));
/// assert!(!is_non_public("93.184.216.34".parse().unwrap()));
/// ```
pub fn is_non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_non_public_v4(v4),
        IpAddr::V6(v6) => is_non_public_v6(v6),
    }
}

fn is_non_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    a == 0
        || v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || (a == 100 && (64..=127).contains(&b))
        || v4.is_multicast()
        || v4.is_broadcast()
}

fn is_non_public_v6(v6: Ipv6Addr) -> bool {
    let segments = v6.segments();
    // NAT64 (64:ff9b::/96) carries an IPv4 address in its low 32 bits.
    let nat64 = segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0];
    // `to_ipv4` covers both IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d).
    let embedded = v6.to_ipv4().or_else(|| {
        nat64.then(|| {
            let [.., hi, lo] = segments;
            Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo))
        })
    });
    if let Some(v4) = embedded {
        return is_non_public_v4(v4);
    }
    v6.is_unspecified()
        || v6.is_loopback()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || v6.is_multicast()
}

/// Resolves host names and refuses any answer that contains a non-public address.
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            if let Some(blocked) = addrs.iter().find(|addr| is_non_public(addr.ip())) {
                let message = format!("{host} resolves to the non-public address {}", blocked.ip());
                return Err(
                    Box::new(EgressRefused(message)) as Box<dyn std::error::Error + Send + Sync>
                );
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[derive(Debug)]
struct EgressRefused(String);

impl std::fmt::Display for EgressRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "egress refused: {}", self.0)
    }
}

impl std::error::Error for EgressRefused {}

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
    client: reqwest::Client,
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
        let mut builder = reqwest::Client::builder()
            .timeout(retry.attempt_timeout)
            .connect_timeout(retry.attempt_timeout.min(Duration::from_secs(5)))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("adk-rust-webhooks/", env!("CARGO_PKG_VERSION")));
        if !egress.allow_private {
            // A proxy would resolve the host itself, out of the resolver's reach.
            builder = builder.no_proxy().dns_resolver(Arc::new(PublicOnlyResolver));
        }
        let client = builder.build().unwrap_or_else(|error| {
            tracing::error!(error = %error, "webhook client failed to build; using the default client");
            reqwest::Client::new()
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
    pub fn egress_policy(&self) -> EgressPolicy {
        self.egress
    }

    /// Delivers `message` to `endpoint`, retrying until it succeeds, fails permanently,
    /// or runs out of attempts, and dead-letters it in the last two cases.
    pub async fn deliver(
        &self,
        endpoint: &WebhookEndpoint,
        message: &WebhookMessage,
    ) -> DeliveryReport {
        let url = match self.egress.check_url(&endpoint.url) {
            Ok(url) => url,
            Err(reason) => return self.dead_letter(endpoint, message, 0, None, reason).await,
        };
        let mut attempt = 0;
        loop {
            attempt += 1;
            let (status, error, retry_after) = match self.attempt(&url, endpoint, message).await {
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
        url: &reqwest::Url,
        endpoint: &WebhookEndpoint,
        message: &WebhookMessage,
    ) -> Attempt {
        let signed = endpoint.signer.sign(&message.id, self.clock.now().timestamp(), &message.body);
        let mut request = self
            .client
            .post(url.clone())
            .header(reqwest::header::CONTENT_TYPE, &message.content_type)
            .body(message.body.clone());
        for (name, value) in signed.header_pairs() {
            request = request.header(name, value);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                let refused = std::error::Error::source(&error).is_some_and(|source| {
                    let mut current = Some(source);
                    while let Some(err) = current {
                        if err.is::<EgressRefused>() {
                            return true;
                        }
                        current = err.source();
                    }
                    false
                });
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
        tracing::warn!(
            webhook.id = %letter.message_id,
            webhook.url = %letter.url,
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

    #[test]
    fn egress_policy_refuses_non_public_literals_and_plain_http() {
        let policy = EgressPolicy::default();
        for url in [
            "http://example.com/hook",
            "https://127.0.0.1/hook",
            "https://10.0.0.1/hook",
            "https://169.254.169.254/latest",
            "https://[::1]/hook",
            "https://[::ffff:192.168.1.1]/hook",
            "https://[64:ff9b::a00:1]/hook",
            "https://localhost/hook",
            "https://api.localhost/hook",
            "https://0x7f.1/hook",
            "ftp://example.com/hook",
        ] {
            assert!(policy.check_url(url).is_err(), "{url}");
        }
        for url in [
            "https://example.com/hook",
            "https://93.184.216.34/hook",
            "https://[2606:4700::1111]/x",
        ] {
            assert!(policy.check_url(url).is_ok(), "{url}");
        }
    }

    #[tokio::test]
    async fn the_resolver_refuses_names_that_resolve_to_non_public_addresses() {
        use reqwest::dns::Resolve;
        use std::str::FromStr;
        for name in ["localhost", "127.0.0.1"] {
            let resolved =
                PublicOnlyResolver.resolve(reqwest::dns::Name::from_str(name).unwrap()).await;
            let error = resolved.err().unwrap_or_else(|| panic!("{name} must be refused"));
            assert!(error.to_string().contains("non-public"), "{name}: {error}");
        }
    }
}
