//! The edge store conformance suite (spec 3.5 §7.9, §11).
//!
//! Every backend — the in-memory stores in this crate, SQLite, PostgreSQL and Redis ones
//! elsewhere — proves its contract by running these cases through an [`EdgeStoreHarness`].
//! Each harness call returns a new handle over the same data, as a restart or a second
//! process would, so the cases check that records survive across handles. Each case writes
//! under fresh identifiers, so a harness may point every case at one database; status cases
//! assume their harness owns the status record.
//!
//! [`edge_store_conformance_tests!`](crate::edge_store_conformance_tests) expands to one
//! `#[tokio::test]` per case.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_awp::v2::store::conformance::EdgeStoreHarness;
//!
//! struct SqliteHarness { url: String }
//!
//! #[async_trait::async_trait]
//! impl EdgeStoreHarness for SqliteHarness {
//!     async fn subscriptions(&self) -> Arc<dyn SubscriptionStore> { /* open */ }
//!     // ... one method per store
//! }
//!
//! adk_awp::edge_store_conformance_tests! {
//!     #[ignore = "requires a database at MY_URL"]
//!     harness = SqliteHarness { url: std::env::var("MY_URL").unwrap() },
//! }
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use awp_types::{
    Bounded, Channel, ConsentReceipt, ConsentReceiptRef, ConsentRequirement, ConsentScope,
    ContactRef, Digest, ErrorBody, ErrorCode, EventSubscription, EventType, HeldCall,
    HeldCallState, LeadRef, LegalBasis, Proof, RequesterId, SiteState, SiteStatus,
    SubscriptionState, TrustTier,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

use super::{
    ConsentReceiptStore, HeldCallRecord, HeldCallStore, HeldCallTransition, IdempotencyBegin,
    IdempotencyStore, Lead, LeadStore, MemoryEdgeStores, RecordedOutcome, StatusStore, StoreError,
    SubscriptionRecord, SubscriptionStore, SubscriptionUpdate, has_live_receipt,
};

/// Opens the edge stores of one backend (spec 3.5 §7.9).
///
/// Each call returns a new handle over the same data.
#[async_trait]
pub trait EdgeStoreHarness: Send + Sync {
    /// Opens the subscription store.
    async fn subscriptions(&self) -> Arc<dyn SubscriptionStore>;
    /// Opens the consent receipt store.
    async fn consent(&self) -> Arc<dyn ConsentReceiptStore>;
    /// Opens the status store.
    async fn status(&self) -> Arc<dyn StatusStore>;
    /// Opens the held-call store.
    async fn held_calls(&self) -> Arc<dyn HeldCallStore>;
    /// Opens the lead store.
    async fn leads(&self) -> Arc<dyn LeadStore>;
    /// Opens the idempotency store.
    async fn idempotency(&self) -> Arc<dyn IdempotencyStore>;
}

#[async_trait]
impl EdgeStoreHarness for MemoryEdgeStores {
    async fn subscriptions(&self) -> Arc<dyn SubscriptionStore> {
        Arc::new(self.subscriptions.clone())
    }
    async fn consent(&self) -> Arc<dyn ConsentReceiptStore> {
        Arc::new(self.consent.clone())
    }
    async fn status(&self) -> Arc<dyn StatusStore> {
        Arc::new(self.status.clone())
    }
    async fn held_calls(&self) -> Arc<dyn HeldCallStore> {
        Arc::new(self.held_calls.clone())
    }
    async fn leads(&self) -> Arc<dyn LeadStore> {
        Arc::new(self.leads.clone())
    }
    async fn idempotency(&self) -> Arc<dyn IdempotencyStore> {
        Arc::new(self.idempotency.clone())
    }
}

fn fresh(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

fn at(minute: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap() + Duration::minutes(minute)
}

fn proof() -> Proof {
    Proof::eddsa("edge-2026-10", "e30..c2ln")
}

fn subscription(owner: &RequesterId, types: Vec<EventType>) -> SubscriptionRecord {
    SubscriptionRecord {
        owner: owner.clone(),
        subscription: EventSubscription {
            id: fresh("sub"),
            types,
            url: "https://agents.buyer.example/hooks/awp".to_owned(),
            state: SubscriptionState::Active,
            created_at: at(0),
            failures: 0,
            secret: Some("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw".to_owned()),
        },
    }
}

fn receipt(subject: &str, scope: ConsentScope) -> ConsentReceipt {
    ConsentReceipt {
        id: Uuid::now_v7(),
        subject_ref: subject.to_owned(),
        scope,
        policy_version: "2026-10".to_owned(),
        policy_digest: Digest::from_sha256_bytes([3; 32]),
        legal_basis: LegalBasis::ExplicitConsent,
        jurisdiction: "KE".to_owned(),
        granted_at: at(0),
        expires_at: Some(at(60 * 24 * 365)),
        channel: Channel::Web,
        proof: proof(),
    }
}

fn held(id: &str) -> HeldCallRecord {
    HeldCallRecord {
        call: HeldCall::pending(id, "book", at(0)),
        requester: Some(RequesterId::web_bot_auth("https://agents.buyer.example", "k1")),
        tier: TrustTier::Known,
        input: json!({ "date": "2026-11-02" }),
        idempotency_key: Some(fresh("key")),
        consent: vec![Uuid::now_v7()],
    }
}

/// A subscription is visible, changeable and deletable only by the requester that created
/// it, through any handle.
pub async fn subscription_owner_scoping<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let owner = RequesterId::web_bot_auth("https://agents.buyer.example", &fresh("k"));
    let stranger = RequesterId::web_bot_auth("https://other.example", &fresh("k"));
    let record = subscription(&owner, vec![EventType::CatalogChanged]);
    let id = record.subscription.id.clone();
    harness.subscriptions().await.create(record.clone()).await.unwrap();

    let store = harness.subscriptions().await;
    assert_eq!(store.get(&owner, &id).await.unwrap(), Some(record.clone()));
    assert_eq!(store.get(&stranger, &id).await.unwrap(), None);
    assert_eq!(store.list(&owner).await.unwrap(), vec![record.clone()]);
    assert!(store.list(&stranger).await.unwrap().is_empty());
    assert!(matches!(
        store.update(&stranger, &id, SubscriptionUpdate::default()).await,
        Err(StoreError::NotFound(_))
    ));
    assert!(!store.delete(&stranger, &id).await.unwrap());

    let updated = store
        .update(
            &owner,
            &id,
            SubscriptionUpdate {
                types: Some(vec![EventType::OrderUpdated]),
                url: Some("https://agents.buyer.example/hooks/v2".to_owned()),
                state: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.subscription.types, vec![EventType::OrderUpdated]);
    assert_eq!(updated.subscription.url, "https://agents.buyer.example/hooks/v2");
    assert!(harness.subscriptions().await.delete(&owner, &id).await.unwrap());
    assert_eq!(harness.subscriptions().await.get(&owner, &id).await.unwrap(), None);
}

/// Six consecutive failed deliveries suspend a subscription; a success clears the count;
/// reactivation clears it too (spec 3.5 §7.5).
pub async fn subscription_suspends_after_six_failures<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let owner = RequesterId::api_key(&fresh("k"));
    let record = subscription(&owner, vec![EventType::SiteReleasePublished]);
    let id = record.subscription.id.clone();
    let store = harness.subscriptions().await;
    store.create(record).await.unwrap();

    for _ in 0..3 {
        store.record_delivery(&id, false).await.unwrap();
    }
    let reset = store.record_delivery(&id, true).await.unwrap();
    assert_eq!(reset.subscription.failures, 0);
    for attempt in 1..=6 {
        let after = harness.subscriptions().await.record_delivery(&id, false).await.unwrap();
        assert_eq!(after.subscription.failures, attempt);
        let expected =
            if attempt == 6 { SubscriptionState::Suspended } else { SubscriptionState::Active };
        assert_eq!(after.subscription.state, expected);
    }
    assert!(
        !store
            .active_for(&EventType::SiteReleasePublished)
            .await
            .unwrap()
            .iter()
            .any(|active| active.subscription.id == id),
        "a suspended subscription receives nothing"
    );
    let reactivated = store
        .update(
            &owner,
            &id,
            SubscriptionUpdate { state: Some(SubscriptionState::Active), ..Default::default() },
        )
        .await
        .unwrap();
    assert_eq!(reactivated.subscription.state, SubscriptionState::Active);
    assert_eq!(reactivated.subscription.failures, 0);
    assert!(matches!(
        store.record_delivery(&fresh("missing"), true).await,
        Err(StoreError::NotFound(_))
    ));
}

/// Delivery lists only active subscriptions naming the type; ids are unique.
pub async fn subscription_delivery_targets<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let owner = RequesterId::api_key(&fresh("k"));
    let custom = EventType::from(fresh("custom.event").as_str());
    let named = subscription(&owner, vec![custom.clone(), EventType::OrderUpdated]);
    let other = subscription(&owner, vec![EventType::OrderUpdated]);
    let store = harness.subscriptions().await;
    store.create(named.clone()).await.unwrap();
    store.create(other).await.unwrap();
    let targets = harness.subscriptions().await.active_for(&custom).await.unwrap();
    assert_eq!(targets, vec![named.clone()]);
    assert!(matches!(store.create(named).await, Err(StoreError::Conflict(_))));
}

/// Receipts round-trip, ids are unique, and a receipt is live only for its scope and policy
/// version until it expires or is revoked (spec 3.5 A-I8).
pub async fn consent_receipts_and_liveness<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let subject = fresh("subj");
    let granted = receipt(&subject, ConsentScope::Newsletter);
    harness.consent().await.insert(granted.clone()).await.unwrap();
    let store = harness.consent().await;
    let record = store.get(granted.id).await.unwrap().unwrap();
    assert_eq!(record.receipt, granted);
    assert_eq!(record.revoked_at, None);
    assert!(matches!(store.insert(granted.clone()).await, Err(StoreError::Conflict(_))));

    let newsletter = ConsentRequirement {
        scope: ConsentScope::Newsletter,
        policy_version: "2026-10".to_owned(),
    };
    let ids = [Uuid::now_v7(), granted.id];
    assert!(has_live_receipt(store.as_ref(), &ids, &newsletter, at(5)).await.unwrap());
    let outreach =
        ConsentRequirement { scope: ConsentScope::ProactiveOutreach, ..newsletter.clone() };
    assert!(!has_live_receipt(store.as_ref(), &ids, &outreach, at(5)).await.unwrap());
    let later_policy =
        ConsentRequirement { policy_version: "2026-11".to_owned(), ..newsletter.clone() };
    assert!(!has_live_receipt(store.as_ref(), &ids, &later_policy, at(5)).await.unwrap());

    store.revoke(granted.id, at(10)).await.unwrap();
    assert!(has_live_receipt(store.as_ref(), &ids, &newsletter, at(9)).await.unwrap());
    assert!(!has_live_receipt(store.as_ref(), &ids, &newsletter, at(10)).await.unwrap());
}

/// A revocation keeps its first time; revoking a scope touches only that subject's live
/// receipts for it; erasure removes only that subject.
pub async fn consent_revocation_and_erasure<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let subject = fresh("subj");
    let neighbour = fresh("subj");
    let newsletter = receipt(&subject, ConsentScope::Newsletter);
    let second = receipt(&subject, ConsentScope::Newsletter);
    let outreach = receipt(&subject, ConsentScope::ProactiveOutreach);
    let theirs = receipt(&neighbour, ConsentScope::Newsletter);
    let store = harness.consent().await;
    for receipt in [&newsletter, &second, &outreach, &theirs] {
        store.insert(receipt.clone()).await.unwrap();
    }

    let first = store.revoke(newsletter.id, at(10)).await.unwrap().unwrap();
    assert_eq!(first.revoked_at, Some(at(10)));
    let again = store.revoke(newsletter.id, at(20)).await.unwrap().unwrap();
    assert_eq!(again.revoked_at, Some(at(10)), "the first revocation time stands");
    assert_eq!(store.revoke(Uuid::now_v7(), at(20)).await.unwrap(), None);

    let revoked = harness
        .consent()
        .await
        .revoke_scope(&subject, &ConsentScope::Newsletter, at(30))
        .await
        .unwrap();
    assert_eq!(revoked.iter().map(|record| record.receipt.id).collect::<Vec<_>>(), vec![second.id]);
    assert_eq!(store.get(outreach.id).await.unwrap().unwrap().revoked_at, None);
    assert_eq!(store.get(theirs.id).await.unwrap().unwrap().revoked_at, None);
    assert_eq!(store.for_subject(&subject).await.unwrap().len(), 3);

    assert_eq!(harness.consent().await.erase(&subject).await.unwrap(), 3);
    assert!(store.for_subject(&subject).await.unwrap().is_empty());
    assert_eq!(store.for_subject(&neighbour).await.unwrap().len(), 1);
}

/// Setting the status returns the previous one, for `site.status.changed`.
pub async fn status_replaces_and_returns_previous<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let operational = SiteStatus::new(SiteState::Operational, at(0), proof());
    let mut incident = SiteStatus::new(SiteState::Incident, at(5), proof());
    incident.reason_code = Some("drift_detected".into());
    incident.release_seq = Some(42);
    harness.status().await.set(operational.clone()).await.unwrap();
    let previous = harness.status().await.set(incident.clone()).await.unwrap();
    assert_eq!(previous, Some(operational));
    assert_eq!(harness.status().await.current().await.unwrap(), Some(incident));
}

/// A held call moves pending → approved → completed, and only pending calls are listed.
pub async fn held_call_lifecycle<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let id = fresh("call");
    let record = held(&id);
    harness.held_calls().await.create(record.clone()).await.unwrap();
    let store = harness.held_calls().await;
    assert_eq!(store.get(&id).await.unwrap(), Some(record.clone()));
    assert!(store.pending().await.unwrap().iter().any(|pending| pending.call.id == id));

    let approved = store
        .transition(
            &id,
            HeldCallTransition { to: HeldCallState::Approved, result: None, error: None },
        )
        .await
        .unwrap();
    assert_eq!(approved.call.state, HeldCallState::Approved);
    assert!(!store.pending().await.unwrap().iter().any(|pending| pending.call.id == id));
    let completed = harness
        .held_calls()
        .await
        .transition(
            &id,
            HeldCallTransition {
                to: HeldCallState::Completed,
                result: Some(json!({ "bookingId": "b-9" })),
                error: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(completed.call.result, Some(json!({ "bookingId": "b-9" })));
    assert_eq!(store.get(&id).await.unwrap().unwrap().call.state, HeldCallState::Completed);
    assert!(matches!(store.create(record).await, Err(StoreError::Conflict(_))));
}

/// Terminal calls stay terminal; a completed call needs a result or an error; a new call is
/// pending (spec 3.5 §7.4).
pub async fn held_call_refuses_invalid_transitions<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let store = harness.held_calls().await;
    let id = fresh("call");
    store.create(held(&id)).await.unwrap();
    let to = |to| HeldCallTransition { to, result: None, error: None };
    assert!(matches!(
        store.transition(&id, to(HeldCallState::Completed)).await,
        Err(StoreError::Conflict(_))
    ));
    let rejected = store
        .transition(
            &id,
            HeldCallTransition {
                to: HeldCallState::Rejected,
                result: None,
                error: Some(ErrorBody::new(ErrorCode::PaymentPolicyRejected, "declined")),
            },
        )
        .await
        .unwrap();
    assert_eq!(rejected.call.error.unwrap().code, ErrorCode::PaymentPolicyRejected);
    for next in [HeldCallState::Approved, HeldCallState::Expired, HeldCallState::Pending] {
        assert!(matches!(store.transition(&id, to(next)).await, Err(StoreError::Conflict(_))));
    }

    let approved = fresh("call");
    store.create(held(&approved)).await.unwrap();
    store.transition(&approved, to(HeldCallState::Approved)).await.unwrap();
    assert!(
        matches!(
            store.transition(&approved, to(HeldCallState::Completed)).await,
            Err(StoreError::Conflict(_))
        ),
        "a completed call carries a result or an error"
    );
    assert!(matches!(
        store.transition(&fresh("missing"), to(HeldCallState::Approved)).await,
        Err(StoreError::NotFound(_))
    ));

    let mut not_pending = held(&fresh("call"));
    not_pending.call.state = HeldCallState::Approved;
    assert!(matches!(store.create(not_pending).await, Err(StoreError::Conflict(_))));
}

/// Leads round-trip, list from a time, take a score, and have unique ids.
pub async fn leads_round_trip<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let lead = Lead {
        id: LeadRef(fresh("lead")),
        captured_at: at(30),
        source: Channel::Chat,
        contact_ref: ContactRef(fresh("contact")),
        consent: vec![ConsentReceiptRef(Uuid::now_v7())],
        answers: BTreeMap::from([("roof".to_owned(), Bounded::new("tin", 40).unwrap())]),
        thread_ref: None,
        score: None,
    };
    let older = Lead { id: LeadRef(fresh("lead")), captured_at: at(-30), ..lead.clone() };
    harness.leads().await.insert(lead.clone()).await.unwrap();
    harness.leads().await.insert(older.clone()).await.unwrap();
    let store = harness.leads().await;
    assert_eq!(store.get(&lead.id).await.unwrap(), Some(lead.clone()));
    let recent = store.since(at(0)).await.unwrap();
    assert!(recent.iter().any(|found| found.id == lead.id));
    assert!(!recent.iter().any(|found| found.id == older.id));
    store.set_score(&lead.id, 0.75).await.unwrap();
    assert_eq!(harness.leads().await.get(&lead.id).await.unwrap().unwrap().score, Some(0.75));
    assert!(matches!(store.insert(lead).await, Err(StoreError::Conflict(_))));
    assert!(matches!(
        store.set_score(&LeadRef(fresh("lead")), 1.0).await,
        Err(StoreError::NotFound(_))
    ));
}

/// A key begins once; a repeat with the same input is in flight, then answered with the
/// recorded outcome; a different input conflicts; outcomes are write-once (spec 3.5 A-I7).
pub async fn idempotency_records_and_replays<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let key = fresh("key");
    let store = harness.idempotency().await;
    assert_eq!(store.begin(&key, "sha256:a", at(0)).await.unwrap(), IdempotencyBegin::Started);
    assert_eq!(store.begin(&key, "sha256:a", at(1)).await.unwrap(), IdempotencyBegin::InFlight);
    assert_eq!(store.begin(&key, "sha256:b", at(1)).await.unwrap(), IdempotencyBegin::Conflict);

    let outcome = RecordedOutcome::Completed { output: json!({ "orderId": "o-1" }) };
    harness.idempotency().await.complete(&key, outcome.clone()).await.unwrap();
    assert_eq!(
        harness.idempotency().await.begin(&key, "sha256:a", at(2)).await.unwrap(),
        IdempotencyBegin::Recorded(outcome)
    );
    assert_eq!(store.begin(&key, "sha256:b", at(2)).await.unwrap(), IdempotencyBegin::Conflict);
    assert!(matches!(
        store.complete(&key, RecordedOutcome::Held { call_id: "c".to_owned() }).await,
        Err(StoreError::Conflict(_))
    ));
    assert!(matches!(
        store.complete(&fresh("key"), RecordedOutcome::Completed { output: json!({}) }).await,
        Err(StoreError::NotFound(_))
    ));
}

/// Concurrent begins under one key have exactly one winner (spec 3.5 A-I7).
pub async fn idempotency_concurrent_begin_has_one_winner<H: EdgeStoreHarness + ?Sized>(
    harness: &H,
) {
    let key = fresh("key");
    let mut stores = Vec::new();
    for _ in 0..8 {
        stores.push(harness.idempotency().await);
    }
    let attempts = stores.into_iter().map(|store| {
        let key = key.clone();
        tokio::spawn(async move { store.begin(&key, "sha256:same", at(0)).await.unwrap() })
    });
    let mut started = 0;
    for attempt in attempts {
        if attempt.await.unwrap() == IdempotencyBegin::Started {
            started += 1;
        }
    }
    assert_eq!(started, 1);
}

/// Purging removes only records that began before the cutoff.
pub async fn idempotency_purge<H: EdgeStoreHarness + ?Sized>(harness: &H) {
    let old = fresh("key");
    let recent = fresh("key");
    let store = harness.idempotency().await;
    store.begin(&old, "sha256:a", at(-60 * 48)).await.unwrap();
    store.begin(&recent, "sha256:a", at(0)).await.unwrap();
    assert!(store.purge_before(at(-60 * 24)).await.unwrap() >= 1);
    assert_eq!(store.begin(&old, "sha256:b", at(1)).await.unwrap(), IdempotencyBegin::Started);
    assert_eq!(store.begin(&recent, "sha256:b", at(1)).await.unwrap(), IdempotencyBegin::Conflict);
}

/// Generates one `#[tokio::test]` per edge store conformance case for an
/// [`EdgeStoreHarness`](crate::v2::store::conformance::EdgeStoreHarness) (spec 3.5 §7.9).
///
/// Attributes written before `harness =` apply to every generated test. The calling crate
/// needs `tokio` with the `macros` and `rt-multi-thread` features.
///
/// # Example
///
/// ```rust,ignore
/// adk_awp::edge_store_conformance_tests! {
///     harness = adk_awp::v2::store::MemoryEdgeStores::default(),
/// }
/// ```
#[macro_export]
macro_rules! edge_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $crate::edge_store_conformance_tests!(@cases [$(#[$attr])*] $harness;
            subscription_owner_scoping,
            subscription_suspends_after_six_failures,
            subscription_delivery_targets,
            consent_receipts_and_liveness,
            consent_revocation_and_erasure,
            status_replaces_and_returns_previous,
            held_call_lifecycle,
            held_call_refuses_invalid_transitions,
            leads_round_trip,
            idempotency_records_and_replays,
            idempotency_concurrent_begin_has_one_winner,
            idempotency_purge,
        );
    };
    (@cases $attrs:tt $harness:expr; $($case:ident),* $(,)?) => {
        $( $crate::edge_store_conformance_tests!(@case $attrs $harness; $case); )*
    };
    (@case [$(#[$attr:meta])*] $harness:expr; $case:ident) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $case() {
            $crate::v2::store::conformance::$case(&$harness).await;
        }
    };
}
