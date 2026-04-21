//! AWP event subscription system with HMAC-SHA256 webhook delivery.

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub secret: String,
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

/// In-memory event subscription service backed by [`DashMap`].
///
/// Webhook delivery is logged but not actually performed (no HTTP client).
/// Enable the `webhook-delivery` feature for real HTTP delivery.
pub struct InMemoryEventSubscriptionService {
    subscriptions: DashMap<Uuid, EventSubscription>,
}

impl InMemoryEventSubscriptionService {
    /// Create a new empty event subscription service.
    pub fn new() -> Self {
        Self { subscriptions: DashMap::new() }
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
        let payload = serde_json::to_vec(&event)
            .map_err(|e| AwpError::InternalError(format!("event serialization failed: {e}")))?;

        for entry in self.subscriptions.iter() {
            let sub = entry.value();
            // Check if subscription matches this event type
            if !sub.event_types.is_empty()
                && !sub.event_types.iter().any(|t| t == &event.event_type || t == "*")
            {
                continue;
            }

            let signature = sign_payload(&payload, &sub.secret);
            tracing::info!(
                subscriber = %sub.subscriber,
                callback_url = %sub.callback_url,
                event_type = %event.event_type,
                signature = %signature,
                "would deliver webhook (in-memory mode)"
            );
        }

        Ok(())
    }
}

/// Compute HMAC-SHA256 of a payload with the given secret.
///
/// Returns the signature in `sha256={hex_digest}` format.
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

    if let Some(hex_sig) = signature.strip_prefix("sha256=") {
        if let Ok(sig_bytes) = hex::decode(hex_sig) {
            return mac.verify_slice(&sig_bytes).is_ok();
        }
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
            secret: "test-secret-key".to_string(),
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
}
