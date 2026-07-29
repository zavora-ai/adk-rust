//! A subscription's HMAC secret must never leave this process.
//!
//! `EventSubscription` derived `Serialize` with a plain `pub secret: String`, and
//! `list_subscriptions` returned `Json(subs)`. Listing therefore handed every subscriber's
//! signing secret to the caller — and the endpoint carried no authentication, so anyone who
//! could reach it could collect the secrets and forge signed deliveries. The derived `Debug`
//! printed the secret too, so any `tracing` call capturing a subscription wrote a live
//! credential to the logs.

use adk_awp::{EventSubscription, InMemoryEventSubscriptionService};
use uuid::Uuid;

fn subscription() -> EventSubscription {
    EventSubscription {
        id: Uuid::now_v7(),
        subscriber: "partner".to_string(),
        callback_url: "https://partner.test/hook".to_string(),
        event_types: vec!["order.created".to_string()],
        secret: "super-secret-hmac-key-32-bytes!!".to_string(),
    }
}

#[test]
fn serializing_a_subscription_omits_the_secret() {
    let json = serde_json::to_string(&subscription()).expect("serialize");

    assert!(
        !json.contains("super-secret-hmac-key"),
        "the signing secret reached a serialized payload: {json}"
    );
    assert!(json.contains("partner"), "the rest of the record must still serialize: {json}");
    assert!(json.contains("https://partner.test/hook"));
}

#[test]
fn the_list_response_shape_contains_no_secret() {
    // This is the exact value `list_subscriptions` serializes.
    let subs = vec![subscription(), subscription()];
    let json = serde_json::to_string(&subs).expect("serialize");

    assert!(
        !json.contains("super-secret-hmac-key"),
        "listing subscriptions must not disclose signing secrets: {json}"
    );
}

#[test]
fn debug_output_redacts_the_secret() {
    let rendered = format!("{:?}", subscription());

    assert!(
        !rendered.contains("super-secret-hmac-key"),
        "Debug must not print the secret, or logging a subscription leaks it: {rendered}"
    );
    assert!(rendered.contains("<redacted>"), "the redaction must be visible: {rendered}");
    assert!(rendered.contains("partner"), "non-secret fields stay useful for diagnosis");
}

#[tokio::test]
async fn the_secret_is_still_usable_in_process_for_signing() {
    // Redaction must not break delivery signing: the value has to survive storage and retrieval
    // inside the process, it just must not be serialized outward.
    use adk_awp::EventSubscriptionService;

    let service = InMemoryEventSubscriptionService::new();
    let created = subscription();
    let id = service.create(created.clone()).await.expect("create");

    let listed = service.list().await.expect("list");
    let found = listed.iter().find(|s| s.id == id).expect("subscription must be stored");

    assert_eq!(
        found.secret, created.secret,
        "the secret must remain available in-process for HMAC signing"
    );
}

#[test]
fn a_stored_subscription_round_trips_without_its_secret() {
    // Deserialization accepts a payload with no `secret` field, so a serialized record can be
    // read back — with an empty secret, which a caller must treat as "not available" rather
    // than as a valid key.
    let json = serde_json::to_string(&subscription()).expect("serialize");
    let restored: EventSubscription = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(restored.subscriber, "partner");
    assert!(restored.secret.is_empty(), "a serialized record carries no secret to restore");
}
