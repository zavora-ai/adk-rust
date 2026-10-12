//! AWP event subscription system with signed webhook delivery.
//!
//! [`InMemoryEventSubscriptionService`] delivers every matching event over HTTP through
//! [`WebhookDelivery`]: [Standard Webhooks](adk_core::webhook) signatures keyed with the
//! subscription's secret bytes, the event id as the stable `webhook-id`, retries with
//! exponential backoff and jitter, a dead-letter sink, and an egress guard that refuses
//! internal destinations.

use std::sync::Arc;

use adk_core::webhook::WebhookSecret;
use adk_core::webhook_delivery::{
    DeliveryReport, EgressPolicy, WebhookDelivery, WebhookEndpoint, WebhookMessage,
};
use async_trait::async_trait;
use awp_types::AwpError;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// An AWP event to be delivered to subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwpEvent {
    /// Unique event identifier.
    pub id: Uuid,
    /// Event type (e.g. `"health.changed"`, `"consent.captured"`).
    pub event_type: String,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Event payload.
    pub payload: serde_json::Value,
}

/// A webhook subscription for AWP events.
///
/// The HMAC secret never leaves this process: it is skipped on serialization and redacted in
/// `Debug`. Listing subscriptions previously returned every subscriber's signing secret in the
/// response body, which let anyone who could read the list forge signed deliveries to that
/// endpoint.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSubscription {
    /// Unique subscription identifier.
    pub id: Uuid,
    /// Human-readable subscriber name.
    pub subscriber: String,
    /// URL to POST events to.
    pub callback_url: String,
    /// Event types this subscription listens for.
    pub event_types: Vec<String>,
    /// Shared secret for HMAC-SHA256 signing.
    ///
    /// Skipped on serialization so it cannot reach an HTTP response. Deserialization still
    /// accepts it, defaulting to empty, so a stored subscription can be loaded.
    #[serde(skip_serializing, default)]
    pub secret: String,
}

impl std::fmt::Debug for EventSubscription {
    /// Redacts the signing secret.
    ///
    /// The derived implementation printed it, so any `tracing` call that captured a
    /// subscription would have written a live credential to the logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSubscription")
            .field("id", &self.id)
            .field("subscriber", &self.subscriber)
            .field("callback_url", &self.callback_url)
            .field("event_types", &self.event_types)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl EventSubscription {
    /// Validates the bounded subscription fields accepted by the built-in service.
    ///
    /// Callback URLs require HTTPS and must not name a loopback, private, or
    /// link-local address; see [`EgressPolicy`].
    ///
    /// # Errors
    ///
    /// Returns [`AwpError::InvalidRequest`] when a field is missing, oversized,
    /// insecure, or malformed.
    pub fn validate(&self) -> Result<(), AwpError> {
        self.validate_with(&EgressPolicy::default())
    }

    /// Validates the subscription against the destinations `egress` allows.
    ///
    /// # Errors
    ///
    /// Same as [`validate`](Self::validate).
    pub fn validate_with(&self, egress: &EgressPolicy) -> Result<(), AwpError> {
        if self.subscriber.trim().is_empty() || self.subscriber.len() > 256 {
            return Err(AwpError::InvalidRequest(
                "subscriber must contain 1 to 256 bytes".to_string(),
            ));
        }
        if self.callback_url.len() > 2_048 {
            return Err(AwpError::InvalidRequest(
                "callbackUrl must not exceed 2048 bytes".to_string(),
            ));
        }
        let callback = self.callback_url.parse::<http::Uri>().map_err(|_| {
            AwpError::InvalidRequest("callbackUrl must be a valid absolute HTTPS URL".to_string())
        })?;
        let scheme_allowed = callback.scheme_str() == Some("https")
            || (egress.allow_http && callback.scheme_str() == Some("http"));
        if !scheme_allowed || callback.authority().is_none() {
            return Err(AwpError::InvalidRequest(
                "callbackUrl must be a valid absolute HTTPS URL".to_string(),
            ));
        }
        egress
            .check_url(&self.callback_url)
            .map_err(|reason| AwpError::InvalidRequest(format!("callbackUrl refused: {reason}")))?;
        if self.event_types.len() > 64 {
            return Err(AwpError::InvalidRequest(
                "eventTypes must not contain more than 64 entries".to_string(),
            ));
        }
        if self
            .event_types
            .iter()
            .any(|event_type| event_type.trim().is_empty() || event_type.len() > 128)
        {
            return Err(AwpError::InvalidRequest(
                "each eventTypes entry must contain 1 to 128 bytes".to_string(),
            ));
        }
        if self.secret.len() < 32 || self.secret.len() > 4_096 {
            return Err(AwpError::InvalidRequest(
                "secret must contain 32 to 4096 bytes".to_string(),
            ));
        }

        Ok(())
    }
}

/// Trait for managing event subscriptions and delivering events.
#[async_trait]
pub trait EventSubscriptionService: Send + Sync {
    /// Create a new subscription, returning its ID.
    async fn create(&self, subscription: EventSubscription) -> Result<Uuid, AwpError>;

    /// Get a subscription by ID.
    async fn get(&self, id: Uuid) -> Result<Option<EventSubscription>, AwpError>;

    /// List all subscriptions.
    async fn list(&self) -> Result<Vec<EventSubscription>, AwpError>;

    /// Delete a subscription by ID.
    async fn delete(&self, id: Uuid) -> Result<(), AwpError>;

    /// Deliver an event to all matching subscribers.
    async fn deliver(&self, event: AwpEvent) -> Result<(), AwpError>;
}

/// In-memory event subscription service backed by [`DashMap`] that delivers over HTTP.
///
/// [`deliver`](EventSubscriptionService::deliver) starts one background delivery per
/// matching subscription and returns at once, so a health transition never waits on a
/// slow subscriber. [`deliver_and_wait`](Self::deliver_and_wait) waits for the reports.
/// Receivers verify deliveries with Standard Webhooks, using the subscription secret's
/// bytes as the key (its `whsec_` form is `WebhookSecret::from_bytes(secret).to_whsec()`).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_awp::InMemoryEventSubscriptionService;
/// use adk_core::webhook_delivery::{InMemoryDeadLetterSink, RetryPolicy, WebhookDelivery};
///
/// let dead_letters = Arc::new(InMemoryDeadLetterSink::new());
/// let service = InMemoryEventSubscriptionService::new().with_delivery(
///     WebhookDelivery::new()
///         .with_retry_policy(RetryPolicy::default().with_max_attempts(8))
///         .with_dead_letter_sink(dead_letters),
/// );
/// # let _ = service;
/// ```
pub struct InMemoryEventSubscriptionService {
    subscriptions: DashMap<Uuid, EventSubscription>,
    delivery: Arc<WebhookDelivery>,
}

impl InMemoryEventSubscriptionService {
    /// Create a new empty event subscription service with the default [`WebhookDelivery`].
    pub fn new() -> Self {
        Self { subscriptions: DashMap::new(), delivery: Arc::new(WebhookDelivery::new()) }
    }

    /// Delivers through `delivery`, for a custom retry policy, dead-letter sink, or egress policy.
    pub fn with_delivery(mut self, delivery: WebhookDelivery) -> Self {
        self.delivery = Arc::new(delivery);
        self
    }

    /// The signed messages for `event`, one per matching subscription.
    fn deliveries(
        &self,
        event: &AwpEvent,
    ) -> Result<Vec<(Uuid, WebhookEndpoint, WebhookMessage)>, AwpError> {
        let message = WebhookMessage::json(event.id.to_string(), event)
            .map_err(|e| AwpError::InternalError(format!("event serialization failed: {e}")))?;
        let mut deliveries = Vec::new();
        for entry in self.subscriptions.iter() {
            let sub = entry.value();
            if !sub.event_types.is_empty()
                && !sub.event_types.iter().any(|t| t == &event.event_type || t == "*")
            {
                continue;
            }
            let Ok(secret) = WebhookSecret::from_bytes(sub.secret.as_bytes().to_vec()) else {
                tracing::warn!(subscription.id = %sub.id, "subscription has no signing secret; skipped");
                continue;
            };
            deliveries.push((
                sub.id,
                WebhookEndpoint::new(&sub.callback_url, secret),
                message.clone(),
            ));
        }
        Ok(deliveries)
    }

    /// Delivers `event` to every matching subscription and waits for each report.
    ///
    /// # Errors
    ///
    /// Returns [`AwpError::InternalError`] when the event cannot be serialized.
    pub async fn deliver_and_wait(
        &self,
        event: AwpEvent,
    ) -> Result<Vec<(Uuid, DeliveryReport)>, AwpError> {
        let deliveries = self.deliveries(&event)?;
        let mut reports = Vec::with_capacity(deliveries.len());
        for (id, endpoint, message) in deliveries {
            reports.push((id, self.delivery.deliver(&endpoint, &message).await));
        }
        Ok(reports)
    }
}

impl Default for InMemoryEventSubscriptionService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventSubscriptionService for InMemoryEventSubscriptionService {
    async fn create(&self, subscription: EventSubscription) -> Result<Uuid, AwpError> {
        subscription.validate_with(&self.delivery.egress_policy())?;
        let id = subscription.id;
        self.subscriptions.insert(id, subscription);
        Ok(id)
    }

    async fn get(&self, id: Uuid) -> Result<Option<EventSubscription>, AwpError> {
        Ok(self.subscriptions.get(&id).map(|e| e.value().clone()))
    }

    async fn list(&self) -> Result<Vec<EventSubscription>, AwpError> {
        Ok(self.subscriptions.iter().map(|e| e.value().clone()).collect())
    }

    async fn delete(&self, id: Uuid) -> Result<(), AwpError> {
        self.subscriptions.remove(&id);
        Ok(())
    }

    async fn deliver(&self, event: AwpEvent) -> Result<(), AwpError> {
        for (subscription_id, endpoint, message) in self.deliveries(&event)? {
            let delivery = Arc::clone(&self.delivery);
            let event_type = event.event_type.clone();
            tokio::spawn(async move {
                let report = delivery.deliver(&endpoint, &message).await;
                tracing::debug!(
                    subscription.id = %subscription_id,
                    event.r#type = %event_type,
                    delivered = report.is_delivered(),
                    "awp event delivery finished"
                );
            });
        }
        Ok(())
    }
}

/// Compute HMAC-SHA256 of a payload with the given secret.
///
/// Returns the signature in `sha256={hex_digest}` format. Deliveries from
/// [`InMemoryEventSubscriptionService`] carry Standard Webhooks signatures instead;
/// this helper remains for receivers of the older format.
pub fn sign_payload(payload: &[u8], secret: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(payload);
    let result = mac.finalize();
    format!("sha256={}", hex::encode(result.into_bytes()))
}

/// Verify an HMAC-SHA256 signature against a payload and secret.
///
/// The `signature` should be in `sha256={hex_digest}` format.
pub fn verify_signature(payload: &[u8], secret: &str, signature: &str) -> bool {
    // Constant-time comparison via the hmac crate
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(payload);

    if let Some(hex_sig) = signature.strip_prefix("sha256=")
        && let Ok(sig_bytes) = hex::decode(hex_sig)
    {
        return mac.verify_slice(&sig_bytes).is_ok();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_subscription(event_types: Vec<String>) -> EventSubscription {
        EventSubscription {
            id: Uuid::now_v7(),
            subscriber: "test-subscriber".to_string(),
            callback_url: "https://example.com/webhook".to_string(),
            event_types,
            secret: "test-secret-key-32-bytes-minimum!!".to_string(),
        }
    }

    fn sample_event() -> AwpEvent {
        AwpEvent {
            id: Uuid::now_v7(),
            event_type: "health.changed".to_string(),
            timestamp: Utc::now(),
            payload: serde_json::json!({"state": "degrading"}),
        }
    }

    #[test]
    fn test_subscription_validation_accepts_bounded_https_destination() {
        assert!(sample_subscription(vec!["health.changed".to_string()]).validate().is_ok());
    }

    #[test]
    fn test_subscription_validation_rejects_insecure_destination() {
        let mut subscription = sample_subscription(vec![]);
        subscription.callback_url = "http://example.com/webhook".to_string();
        assert!(subscription.validate().is_err());
    }

    #[test]
    fn test_subscription_validation_rejects_short_signing_secret() {
        let mut subscription = sample_subscription(vec![]);
        subscription.secret = "short".to_string();
        assert!(subscription.validate().is_err());
    }

    #[tokio::test]
    async fn test_create_and_get() {
        let svc = InMemoryEventSubscriptionService::new();
        let sub = sample_subscription(vec!["health.changed".to_string()]);
        let id = sub.id;
        svc.create(sub).await.unwrap();

        let retrieved = svc.get(id).await.unwrap().unwrap();
        assert_eq!(retrieved.subscriber, "test-subscriber");
    }

    #[tokio::test]
    async fn test_get_nonexistent() {
        let svc = InMemoryEventSubscriptionService::new();
        assert!(svc.get(Uuid::now_v7()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_list() {
        let svc = InMemoryEventSubscriptionService::new();
        svc.create(sample_subscription(vec![])).await.unwrap();
        svc.create(sample_subscription(vec![])).await.unwrap();

        let list = svc.list().await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn test_delete() {
        let svc = InMemoryEventSubscriptionService::new();
        let sub = sample_subscription(vec![]);
        let id = sub.id;
        svc.create(sub).await.unwrap();
        assert!(svc.get(id).await.unwrap().is_some());

        svc.delete(id).await.unwrap();
        assert!(svc.get(id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_deliver_matching_event() {
        let svc = InMemoryEventSubscriptionService::new();
        svc.create(sample_subscription(vec!["health.changed".to_string()])).await.unwrap();
        // Should not error — just logs
        svc.deliver(sample_event()).await.unwrap();
    }

    #[tokio::test]
    async fn test_deliver_wildcard_subscription() {
        let svc = InMemoryEventSubscriptionService::new();
        svc.create(sample_subscription(vec!["*".to_string()])).await.unwrap();
        svc.deliver(sample_event()).await.unwrap();
    }

    #[tokio::test]
    async fn test_deliver_non_matching_event() {
        let svc = InMemoryEventSubscriptionService::new();
        svc.create(sample_subscription(vec!["consent.captured".to_string()])).await.unwrap();
        // health.changed doesn't match consent.captured — should still succeed (no delivery)
        svc.deliver(sample_event()).await.unwrap();
    }

    #[test]
    fn test_sign_payload() {
        let payload = b"hello world";
        let sig = sign_payload(payload, "secret");
        assert!(sig.starts_with("sha256="));
        assert_eq!(sig.len(), 7 + 64); // "sha256=" + 64 hex chars
    }

    #[test]
    fn test_verify_signature_valid() {
        let payload = b"test payload";
        let secret = "my-secret";
        let sig = sign_payload(payload, secret);
        assert!(verify_signature(payload, secret, &sig));
    }

    #[test]
    fn test_verify_signature_wrong_payload() {
        let secret = "my-secret";
        let sig = sign_payload(b"original", secret);
        assert!(!verify_signature(b"tampered", secret, &sig));
    }

    #[test]
    fn test_verify_signature_wrong_secret() {
        let payload = b"test payload";
        let sig = sign_payload(payload, "secret1");
        assert!(!verify_signature(payload, "secret2", &sig));
    }

    #[test]
    fn test_verify_signature_invalid_format() {
        assert!(!verify_signature(b"payload", "secret", "not-a-signature"));
    }

    #[test]
    fn test_verify_signature_invalid_hex() {
        assert!(!verify_signature(b"payload", "secret", "sha256=not-hex"));
    }

    #[test]
    fn test_event_serialization_round_trip() {
        let event = sample_event();
        let json = serde_json::to_string(&event).unwrap();
        let parsed: AwpEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.id, parsed.id);
        assert_eq!(event.event_type, parsed.event_type);
    }

    #[test]
    fn test_subscription_serialization_round_trip() {
        let sub = sample_subscription(vec!["health.changed".to_string()]);
        let json = serde_json::to_string(&sub).unwrap();
        let parsed: EventSubscription = serde_json::from_str(&json).unwrap();
        assert_eq!(sub.id, parsed.id);
        assert_eq!(sub.subscriber, parsed.subscriber);
    }

    #[test]
    fn test_subscription_validation_rejects_internal_destinations() {
        for url in
            ["https://169.254.169.254/latest", "https://127.0.0.1/hook", "https://localhost/hook"]
        {
            let mut subscription = sample_subscription(vec![]);
            subscription.callback_url = url.to_string();
            assert!(subscription.validate().is_err(), "{url}");
        }
    }

    type Captured = Arc<std::sync::Mutex<Vec<(axum::http::HeaderMap, bytes::Bytes)>>>;

    /// A local receiver that answers every delivery with `status`.
    async fn receiver(status: u16) -> (String, Captured) {
        let captured: Captured = Arc::default();
        let log = captured.clone();
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |headers: axum::http::HeaderMap, body: bytes::Bytes| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push((headers, body));
                    axum::http::StatusCode::from_u16(status).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, captured)
    }

    fn local_service(max_attempts: u32) -> InMemoryEventSubscriptionService {
        use adk_core::webhook_delivery::RetryPolicy;
        InMemoryEventSubscriptionService::new().with_delivery(
            WebhookDelivery::new()
                .with_retry_policy(
                    RetryPolicy::default()
                        .with_max_attempts(max_attempts)
                        .with_initial_backoff(std::time::Duration::from_millis(5))
                        .with_max_backoff(std::time::Duration::from_millis(20)),
                )
                .with_egress_policy(EgressPolicy::allow_private_networks()),
        )
    }

    #[tokio::test]
    async fn test_events_are_delivered_with_standard_webhooks_signatures() {
        use adk_core::webhook::{WebhookHeaders, WebhookVerifier};
        let (url, captured) = receiver(200).await;
        let svc = local_service(3);
        let mut sub = sample_subscription(vec!["health.changed".to_string()]);
        sub.callback_url = url;
        svc.create(sub.clone()).await.unwrap();
        let event = sample_event();

        let reports = svc.deliver_and_wait(event.clone()).await.unwrap();
        assert_eq!(reports, vec![(sub.id, DeliveryReport::Delivered { attempts: 1, status: 200 })]);

        let (headers, body) = captured.lock().unwrap()[0].clone();
        assert_eq!(headers["webhook-id"], event.id.to_string());
        let parsed =
            WebhookHeaders::from_lookup(|name| headers.get(name).and_then(|v| v.to_str().ok()))
                .unwrap();
        let verifier =
            WebhookVerifier::new(WebhookSecret::from_bytes(sub.secret.into_bytes()).unwrap());
        verifier.verify(&parsed, &body).expect("signed with the subscription secret");
        let delivered: AwpEvent = serde_json::from_slice(&body).unwrap();
        assert_eq!(delivered.id, event.id);
    }

    #[tokio::test]
    async fn test_failing_subscribers_are_retried_then_dead_lettered() {
        let (url, captured) = receiver(503).await;
        let svc = local_service(2);
        let mut sub = sample_subscription(vec![]);
        sub.callback_url = url;
        svc.create(sub).await.unwrap();
        let reports = svc.deliver_and_wait(sample_event()).await.unwrap();
        assert!(
            matches!(&reports[0].1, DeliveryReport::DeadLettered(letter) if letter.attempts == 2)
        );
        assert_eq!(captured.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_deliver_returns_before_the_delivery_finishes() {
        let (url, captured) = receiver(200).await;
        let svc = local_service(1);
        let mut sub = sample_subscription(vec![]);
        sub.callback_url = url;
        svc.create(sub).await.unwrap();
        svc.deliver(sample_event()).await.unwrap();
        for _ in 0..100 {
            if !captured.lock().unwrap().is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the background delivery never arrived");
    }
}
