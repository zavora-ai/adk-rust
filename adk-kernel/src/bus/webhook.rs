//! The outbound webhook sink (spec 3.2 §7.6).

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use adk_core::egress::EgressPolicy;
use adk_core::webhook::WebhookSecret;
use adk_core::webhook_delivery::{
    DeadLetterSink, DeliveryReport, RetryPolicy, WebhookDelivery, WebhookEndpoint, WebhookMessage,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::Semaphore;

use super::{EventFilter, EventRecord, EventSink, KernelEvent};
use crate::error::{KernelError, Result};

/// One delivery's body: the Standard Webhooks payload shape, whose `data` is the record the
/// control plane's SSE stream sends for the same event.
#[derive(Serialize)]
struct Payload<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    timestamp: DateTime<Utc>,
    data: &'a EventRecord,
}

/// Delivers the kernel's events to an HTTP endpoint as signed webhooks (spec 3.2 §7.6).
///
/// Each event the sink's [`EventFilter`] selects is `POST`ed as JSON, signed with
/// [Standard Webhooks](adk_core::webhook) signatures — the scheme the kernel's webhook
/// ingress verifies — through `adk-core`'s [`WebhookDelivery`]:
///
/// | Part | Value |
/// |---|---|
/// | `webhook-id` | `evt_<uuid>`, one per event and the same on every retry, so a receiver deduplicates on it |
/// | `webhook-timestamp`, `webhook-signature` | Signed afresh for each attempt with the sink's secret |
/// | Body | `{"type": "<event type>", "timestamp": "<RFC 3339>", "data": {"cursor": …, "at": …, "event": {…}}}`; `data` is the record `GET /api/kernel/events` streams |
///
/// | Concern | Behaviour |
/// |---|---|
/// | Selection | The [`EventFilter`]; the default leaves out `health` |
/// | Destination | `https`, or plain `http` to this machine (`localhost`, `*.localhost`, a loopback address) only; a URL with credentials in it is refused |
/// | Retry | The [`RetryPolicy`]: connection errors, timeouts, `408`, `425`, `429` and `5xx` are retried with exponential backoff and full jitter, honouring `Retry-After`; redirects are never followed |
/// | Undeliverable | Any other status, or the last failed attempt, ends the event at the [`DeadLetterSink`] when one is set, and drops it with a warning otherwise |
/// | Back-pressure | At most [`with_max_in_flight`](Self::with_max_in_flight) deliveries run at once, each on its own task; past that the bus queues the sink's events and, when its queue is full, drops `health` first and then the oldest (§7.6). The bus and the processes never wait on the receiver |
/// | Logs | The endpoint's origin only — never its path, query or the secret |
///
/// Deliveries run in order with one in flight; with more, a later event may arrive before
/// an earlier one that is being retried, and the receiver orders by `data.cursor`. A
/// `held` event carries the held call's arguments: leave [`EventKind::Held`] out of the
/// filter for a receiver that should not see them.
///
/// [`EventKind::Held`]: crate::EventKind::Held
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::webhook::WebhookSecret;
/// use adk_kernel::{EventBus, EventFilter, EventKind, WebhookSink};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Read the secret from the environment or a secret store, never from source.
/// let secret = WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?;
/// let sink = WebhookSink::new("https://hooks.example.com/kernel", secret.clone())?
///     .with_filter(EventFilter::only([EventKind::Held, EventKind::Decided, EventKind::Governance]));
/// let bus = EventBus::default();
/// bus.add_sink(Arc::new(sink));
/// // On a kernel: `Kernel::builder().event_sink(Arc::new(sink))`.
///
/// assert!(WebhookSink::new("http://hooks.example.com/kernel", secret.clone()).is_err());
/// assert!(WebhookSink::new("http://127.0.0.1:9000/kernel", secret).is_ok());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct WebhookSink {
    endpoint: WebhookEndpoint,
    /// `scheme://host[:port]`, what the logs name; the path may carry a token.
    origin: String,
    filter: EventFilter,
    delivery: WebhookDelivery,
    in_flight: Arc<Semaphore>,
    max_in_flight: usize,
}

impl fmt::Debug for WebhookSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhookSink")
            .field("origin", &self.origin)
            .field("filter", &self.filter)
            .field("retry", &self.delivery.retry_policy())
            .field("max_in_flight", &self.max_in_flight)
            .finish_non_exhaustive()
    }
}

impl WebhookSink {
    /// How many deliveries run at once by default: one, so the receiver gets events in
    /// order.
    pub const DEFAULT_MAX_IN_FLIGHT: usize = 1;

    /// A sink delivering to `url`, signed with `secret`, with the default [`EventFilter`],
    /// [`RetryPolicy`] and no dead-letter sink.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when `url` does not parse, carries credentials, or
    /// is neither `https` nor plain `http` to a loopback host. The error names the URL's
    /// origin only.
    pub fn new(url: &str, secret: WebhookSecret) -> Result<Self> {
        let parsed = check_url(url)?;
        let delivery = WebhookDelivery::new()
            // `check_url` admits only https and loopback http; an operator configures the URL,
            // so a receiver on the private network is allowed over https.
            .with_egress_policy(EgressPolicy::allow_private_networks());
        Ok(Self {
            endpoint: WebhookEndpoint::new(parsed.as_str(), secret),
            origin: parsed.origin().ascii_serialization(),
            filter: EventFilter::default(),
            delivery,
            in_flight: Arc::new(Semaphore::new(Self::DEFAULT_MAX_IN_FLIGHT)),
            max_in_flight: Self::DEFAULT_MAX_IN_FLIGHT,
        })
    }

    /// Delivers only the events `filter` selects.
    #[must_use]
    pub fn with_filter(mut self, filter: EventFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Retries each delivery under `retry`.
    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.delivery = self.delivery.with_retry_policy(retry);
        self
    }

    /// Keeps each undeliverable event, with its last error, in `sink`.
    #[must_use]
    pub fn with_dead_letter_sink(mut self, sink: Arc<dyn DeadLetterSink>) -> Self {
        self.delivery = self.delivery.with_dead_letter_sink(sink);
        self
    }

    /// Runs at most `max` deliveries at once (at least one).
    #[must_use]
    pub fn with_max_in_flight(mut self, max: usize) -> Self {
        self.max_in_flight = max.max(1);
        self.in_flight = Arc::new(Semaphore::new(self.max_in_flight));
        self
    }

    /// The events the sink delivers.
    pub fn filter(&self) -> &EventFilter {
        &self.filter
    }

    /// The endpoint's origin, `scheme://host[:port]`.
    pub fn origin(&self) -> &str {
        &self.origin
    }
}

#[async_trait]
impl EventSink for WebhookSink {
    /// Delivers `event` with cursor 0, which the bus never issues; the bus itself calls
    /// [`emit_record`](EventSink::emit_record).
    async fn emit(&self, event: &KernelEvent) -> Result<()> {
        self.emit_record(&EventRecord { cursor: 0, at: Utc::now(), event: event.clone() }).await
    }

    async fn emit_record(&self, record: &EventRecord) -> Result<()> {
        if !self.filter.matches(&record.event) {
            return Ok(());
        }
        let kind = record.event.name();
        let id = format!("evt_{}", uuid::Uuid::new_v4().simple());
        let payload = Payload { kind, timestamp: record.at, data: record };
        let message = WebhookMessage::json(id, &payload)?;
        // Waiting here, not in `publish`, is what fills the bus's queue for this sink.
        let permit = Arc::clone(&self.in_flight).acquire_owned().await.map_err(|_| {
            KernelError::Config { reason: "the webhook sink's delivery limit is closed".into() }
        })?;
        let (delivery, endpoint, origin) =
            (self.delivery.clone(), self.endpoint.clone(), self.origin.clone());
        let cursor = record.cursor;
        tokio::spawn(async move {
            let report = delivery.deliver(&endpoint, &message).await;
            drop(permit);
            match report {
                DeliveryReport::Delivered { attempts, status } => tracing::debug!(
                    webhook.id = %message.id,
                    webhook.origin = %origin,
                    event.kind = kind,
                    event.cursor = cursor,
                    attempts,
                    http.status = status,
                    "kernel event delivered"
                ),
                DeliveryReport::DeadLettered(letter) => tracing::warn!(
                    webhook.id = %message.id,
                    webhook.origin = %origin,
                    event.kind = kind,
                    event.cursor = cursor,
                    attempts = letter.attempts,
                    http.status = letter.last_status,
                    "kernel event not delivered"
                ),
            }
        });
        Ok(())
    }
}

/// `url` when deliveries may go to it: `https`, or plain `http` to this machine, with no
/// credentials in it. Errors name the origin at most, as the path may carry a token.
fn check_url(url: &str) -> Result<reqwest::Url> {
    let invalid =
        |reason: String| KernelError::Config { reason: format!("webhook sink: {reason}") };
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| invalid(format!("the URL does not parse: {error}")))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid(
            "the URL carries credentials; remove them, as the signature authenticates each \
             delivery"
                .into(),
        ));
    }
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if is_loopback(&parsed) => Ok(parsed),
        "http" => Err(invalid(format!(
            "{} must use https; plain http is accepted on loopback only",
            parsed.origin().ascii_serialization()
        ))),
        other => Err(invalid(format!("the scheme `{other}` is not allowed; use https"))),
    }
}

/// Whether `url` names this machine.
fn is_loopback(url: &reqwest::Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> WebhookSecret {
        WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap()
    }

    #[test]
    fn urls_are_https_or_loopback_http_without_credentials() {
        for url in [
            "https://hooks.example.com/kernel",
            "https://10.0.0.7/kernel",
            "http://127.0.0.1:9000/kernel",
            "http://[::1]:9000/kernel",
            "http://localhost:9000/kernel",
            "http://kernel.localhost/hooks",
        ] {
            assert!(WebhookSink::new(url, secret()).is_ok(), "{url}");
        }
        for (url, reason) in [
            ("http://hooks.example.com/kernel", "must use https"),
            ("http://10.0.0.7/kernel", "must use https"),
            ("ftp://hooks.example.com/kernel", "scheme `ftp`"),
            ("https://user:pass@hooks.example.com/kernel", "carries credentials"),
            ("https://token@hooks.example.com/kernel", "carries credentials"),
            ("not a url", "does not parse"),
        ] {
            let error = WebhookSink::new(url, secret()).unwrap_err().to_string();
            assert!(error.contains(reason), "{url}: {error}");
            assert!(!error.contains("pass") && !error.contains("/kernel"), "{url}: {error}");
        }
    }

    #[test]
    fn the_debug_form_names_the_origin_only() {
        let sink = WebhookSink::new("https://hooks.example.com/T0K3N?key=s3cr3t", secret())
            .unwrap()
            .with_max_in_flight(0);
        let debug = format!("{sink:?}");
        assert!(debug.contains("https://hooks.example.com"), "{debug}");
        for hidden in ["T0K3N", "s3cr3t", "MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw"] {
            assert!(!debug.contains(hidden), "{debug}");
        }
        assert_eq!(sink.max_in_flight, 1, "at least one delivery runs");
        assert_eq!(sink.origin(), "https://hooks.example.com");
    }
}
