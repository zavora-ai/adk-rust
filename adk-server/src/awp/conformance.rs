//! The conformance suite of the edge's event log and beacon store (spec 3.5 §7.5, §7.9,
//! §7.12, §11).
//!
//! Every [`EventLog`] that keeps what event delivery needs across a restart, and every
//! [`BeaconStore`], proves its contract by running these cases through an
//! [`EdgeLogHarness`]: the in-memory stores, and the SQLite, PostgreSQL and Redis edge stores.
//! Each harness call returns a new handle over the same data, as a restarted or second
//! process would open; the generated tests build one harness per case.
//!
//! [`edge_log_conformance_tests!`](crate::edge_log_conformance_tests) expands to one
//! `#[tokio::test]` per case.
//!
//! # Example
//!
//! ```rust,ignore
//! adk_server::edge_log_conformance_tests! {
//!     harness = adk_server::awp::conformance::MemoryEdgeLog::default(),
//! }
//! ```

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use awp_types::{CatalogChangesPage, ContentId, EventEnvelope, EventType, RequesterId};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

use super::beacon::{BeaconStore, InMemoryBeaconStore};
use super::events::{EventLog, InMemoryEventLog, PendingDelivery, RetainedEvent};

/// Opens the event log and beacon store of one backend (spec 3.5 §7.9).
///
/// Each call returns a new handle over the same data.
#[async_trait]
pub trait EdgeLogHarness: Send + Sync {
    /// Opens the event log.
    async fn events(&self) -> Arc<dyn EventLog>;
    /// Opens the beacon store.
    async fn beacon(&self) -> Arc<dyn BeaconStore>;
}

/// The in-memory event log and beacon store, shared by every handle.
#[derive(Debug, Clone, Default)]
pub struct MemoryEdgeLog {
    /// The event log.
    pub events: InMemoryEventLog,
    /// The beacon store.
    pub beacon: InMemoryBeaconStore,
}

#[async_trait]
impl EdgeLogHarness for MemoryEdgeLog {
    async fn events(&self) -> Arc<dyn EventLog> {
        Arc::new(self.events.clone())
    }
    async fn beacon(&self) -> Arc<dyn BeaconStore> {
        Arc::new(self.beacon.clone())
    }
}

fn at(minute: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap() + Duration::minutes(minute)
}

fn day(n: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, n).unwrap()
}

fn retained(minute: i64, audience: Option<&[&str]>) -> RetainedEvent {
    RetainedEvent {
        event: EventEnvelope::new(
            format!("evt_{}", Uuid::now_v7().simple()),
            EventType::CatalogChanged,
            at(minute),
            json!({ "releaseSeq": minute, "items": ["sku-1"], "total": 1 }),
        ),
        audience: audience.map(|ids| ids.iter().map(|id| RequesterId::new(*id)).collect()),
    }
}

fn page(first: u32) -> CatalogChangesPage {
    CatalogChangesPage {
        items: (first..first + 3).map(|n| ContentId(format!("sku-{n}"))).collect(),
        next: Some(format!("https://agent.example.com/awp/v2/events/changes/9/{first}")),
    }
}

/// Events read back by window, oldest first, with their audiences, through any handle; one
/// is found by id (spec 3.5 §7.5).
pub async fn event_log_windows_and_lookup<H: EdgeLogHarness + ?Sized>(harness: &H) {
    let early = retained(0, None);
    let middle = retained(10, Some(&["wba:https://buyer.example#k1"]));
    let late = retained(20, None);
    for event in [&middle, &early, &late] {
        harness.events().await.append(event.clone()).await.unwrap();
    }
    let log = harness.events().await;
    assert_eq!(
        log.between(at(0), None).await.unwrap(),
        vec![early.clone(), middle.clone(), late.clone()]
    );
    assert_eq!(log.between(at(5), Some(at(20))).await.unwrap(), vec![middle.clone()]);
    assert_eq!(log.between(at(21), None).await.unwrap(), Vec::<RetainedEvent>::new());
    assert_eq!(log.event(&middle.event.id).await.unwrap(), Some(middle));
    assert_eq!(log.event("evt_unknown").await.unwrap(), None);
}

/// Purging drops the events and the catalogue pages kept before the cutoff, and only them.
pub async fn event_log_purge<H: EdgeLogHarness + ?Sized>(harness: &H) {
    let old = retained(0, None);
    let recent = retained(60, None);
    let log = harness.events().await;
    log.append(old.clone()).await.unwrap();
    log.append(recent.clone()).await.unwrap();
    log.put_catalog_pages(1, at(0), &[page(10)]).await.unwrap();
    log.put_catalog_pages(2, at(60), &[page(20)]).await.unwrap();
    assert_eq!(harness.events().await.purge_before(at(30)).await.unwrap(), 1);
    let log = harness.events().await;
    assert_eq!(log.between(at(0), None).await.unwrap(), vec![recent]);
    assert_eq!(log.event(&old.event.id).await.unwrap(), None);
    assert_eq!(log.catalog_page(1, 2).await.unwrap(), None);
    assert_eq!(log.catalog_page(2, 2).await.unwrap(), Some(page(20)));
}

/// A release's further catalogue pages are kept from page 2, and replaced when stored again
/// (spec 3.5 §7.8.3).
pub async fn event_log_catalog_pages<H: EdgeLogHarness + ?Sized>(harness: &H) {
    assert!(
        harness.events().await.put_catalog_pages(7, at(0), &[page(1), page(4)]).await.unwrap(),
        "a durable log keeps catalogue pages"
    );
    let log = harness.events().await;
    assert_eq!(log.catalog_page(7, 2).await.unwrap(), Some(page(1)));
    assert_eq!(log.catalog_page(7, 3).await.unwrap(), Some(page(4)));
    assert_eq!(log.catalog_page(7, 4).await.unwrap(), None);
    assert_eq!(log.catalog_page(8, 2).await.unwrap(), None);
    log.put_catalog_pages(7, at(1), &[page(9)]).await.unwrap();
    assert_eq!(harness.events().await.catalog_page(7, 2).await.unwrap(), Some(page(9)));
}

/// Who referenced a receipt is kept once per requester, through any handle (AWP 2.0 §9.2).
pub async fn event_log_consent_references<H: EdgeLogHarness + ?Sized>(harness: &H) {
    let receipt = Uuid::now_v7();
    let (a, b) = (RequesterId::new("wba:https://a.example#k"), RequesterId::new("key:b"));
    let log = harness.events().await;
    assert!(log.note_consent_reference(receipt, &a).await.unwrap(), "a durable log keeps notes");
    log.note_consent_reference(receipt, &b).await.unwrap();
    log.note_consent_reference(receipt, &a).await.unwrap();
    let log = harness.events().await;
    assert_eq!(log.consent_references(receipt).await.unwrap(), BTreeSet::from([a, b]));
    assert!(log.consent_references(Uuid::now_v7()).await.unwrap().is_empty());
}

/// A pending delivery is taken once it is due, by one caller, leased so it is not taken
/// again until the lease passes, replaced when saved again, and gone once finished (spec 3.5
/// §7.5).
pub async fn event_log_deliveries_resume_once<H: EdgeLogHarness + ?Sized>(harness: &H) {
    let lease = Duration::seconds(70);
    let due = PendingDelivery {
        subscription: format!("sub_{}", Uuid::now_v7().simple()),
        event_id: format!("evt_{}", Uuid::now_v7().simple()),
        failures: 2,
        due: at(0),
    };
    let later = PendingDelivery {
        subscription: due.subscription.clone(),
        event_id: format!("evt_{}", Uuid::now_v7().simple()),
        failures: 0,
        due: at(30),
    };
    harness.events().await.save_delivery(&due).await.unwrap();
    harness.events().await.save_delivery(&later).await.unwrap();

    let claimed = harness.events().await.claim_deliveries(at(1), lease).await.unwrap();
    assert_eq!(claimed, vec![PendingDelivery { due: at(1) + lease, ..due.clone() }]);
    assert!(
        harness.events().await.claim_deliveries(at(2), lease).await.unwrap().is_empty(),
        "a leased delivery is not taken again"
    );
    let mut retried = due.clone();
    retried.failures = 3;
    retried.due = at(3);
    harness.events().await.save_delivery(&retried).await.unwrap();
    assert_eq!(
        harness.events().await.claim_deliveries(at(4), lease).await.unwrap(),
        vec![PendingDelivery { due: at(4) + lease, ..retried.clone() }],
        "saving again replaces the delivery"
    );
    harness.events().await.finish_delivery(&due.subscription, &due.event_id).await.unwrap();
    let rest = harness.events().await.claim_deliveries(at(600), lease).await.unwrap();
    assert_eq!(rest, vec![PendingDelivery { due: at(600) + lease, ..later }]);
}

/// Concurrent claims of one due delivery have exactly one winner (spec 3.5 §7.5).
pub async fn event_log_concurrent_claims_have_one_winner<H: EdgeLogHarness + ?Sized>(harness: &H) {
    let delivery = PendingDelivery {
        subscription: format!("sub_{}", Uuid::now_v7().simple()),
        event_id: format!("evt_{}", Uuid::now_v7().simple()),
        failures: 1,
        due: at(0),
    };
    harness.events().await.save_delivery(&delivery).await.unwrap();
    let mut logs = Vec::new();
    for _ in 0..8 {
        logs.push(harness.events().await);
    }
    let attempts = logs.into_iter().map(|log| {
        tokio::spawn(async move { log.claim_deliveries(at(1), Duration::seconds(70)).await })
    });
    let mut won = 0;
    for attempt in attempts {
        won += attempt.await.unwrap().unwrap().len();
    }
    assert_eq!(won, 1);
}

/// Daily counts add up by metric over a window of whole days, through any handle (spec 3.5
/// §7.12).
pub async fn beacon_counts_and_totals<H: EdgeLogHarness + ?Sized>(harness: &H) {
    let store = harness.beacon().await;
    store.add(day(24), "pageviews", 2).await.unwrap();
    store.add(day(24), "pageviews", 3).await.unwrap();
    store.add(day(25), "pageviews", 1).await.unwrap();
    store.add(day(24), "events:signup", 1).await.unwrap();
    store.add(day(27), "pageviews", 10).await.unwrap();
    let store = harness.beacon().await;
    assert_eq!(store.total("pageviews", day(24), day(26)).await.unwrap(), 6);
    assert_eq!(store.total("pageviews", day(24), day(25)).await.unwrap(), 5);
    assert_eq!(store.total("pageviews", day(25), day(28)).await.unwrap(), 11);
    assert_eq!(store.total("events:signup", day(1), day(31)).await.unwrap(), 1);
    assert_eq!(store.total("pageviews:/unknown", day(1), day(31)).await.unwrap(), 0);
    assert_eq!(store.total("pageviews", day(26), day(26)).await.unwrap(), 0);
}

/// A visitor counts once a day, whatever its visits (spec 3.5 §7.12).
pub async fn beacon_visitors_are_distinct_per_day<H: EdgeLogHarness + ?Sized>(harness: &H) {
    let store = harness.beacon().await;
    store.add_visitor(day(24), "subj_a").await.unwrap();
    store.add_visitor(day(24), "subj_a").await.unwrap();
    store.add_visitor(day(24), "subj_b").await.unwrap();
    store.add_visitor(day(25), "subj_a").await.unwrap();
    let store = harness.beacon().await;
    assert_eq!(store.total("visitors", day(24), day(26)).await.unwrap(), 3);
    assert_eq!(store.total("visitors", day(25), day(26)).await.unwrap(), 1);
    assert_eq!(store.total("visitors", day(26), day(30)).await.unwrap(), 0);
}

/// Concurrent counts are never lost (spec 3.5 §7.12).
pub async fn beacon_concurrent_adds_are_not_lost<H: EdgeLogHarness + ?Sized>(harness: &H) {
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = harness.beacon().await;
        tasks.push(tokio::spawn(async move {
            for _ in 0..25 {
                store.add(day(24), "pageviews", 1).await.unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(harness.beacon().await.total("pageviews", day(24), day(25)).await.unwrap(), 200);
}

/// Generates one `#[tokio::test]` per event log and beacon store conformance case for an
/// [`EdgeLogHarness`](crate::awp::conformance::EdgeLogHarness) (spec 3.5 §7.9).
///
/// Attributes written before `harness =` apply to every generated test; each test builds its
/// own harness. The calling crate needs `tokio` with the `macros` and `rt-multi-thread`
/// features.
///
/// # Example
///
/// ```rust,ignore
/// adk_server::edge_log_conformance_tests! {
///     harness = adk_server::awp::conformance::MemoryEdgeLog::default(),
/// }
/// ```
#[macro_export]
macro_rules! edge_log_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $crate::edge_log_conformance_tests!(@cases [$(#[$attr])*] $harness;
            event_log_windows_and_lookup,
            event_log_purge,
            event_log_catalog_pages,
            event_log_consent_references,
            event_log_deliveries_resume_once,
            event_log_concurrent_claims_have_one_winner,
            beacon_counts_and_totals,
            beacon_visitors_are_distinct_per_day,
            beacon_concurrent_adds_are_not_lost,
        );
    };
    (@cases $attrs:tt $harness:expr; $($case:ident),* $(,)?) => {
        $( $crate::edge_log_conformance_tests!(@case $attrs $harness; $case); )*
    };
    (@case [$(#[$attr:meta])*] $harness:expr; $case:ident) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $case() {
            $crate::awp::conformance::$case(&$harness).await;
        }
    };
}
