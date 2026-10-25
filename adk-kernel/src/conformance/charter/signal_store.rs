//! Conformance cases for [`SignalStore`] backends (spec 3.5 §7.10, §11).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`first_opens_then_counts`] | The first signal of a key opens a record with count 1; each repeat raises the count and replaces the latest signal, keeping the first arrival |
//! | [`keys_are_apart`] | Another partition, kind or topic opens its own record |
//! | [`close_then_reopen`] | A closed record keeps its count; the key's next signal opens a new record |
//! | [`list_filters_and_orders`] | `list` filters by partition, kind and openness, oldest first, up to the limit |
//! | [`hits_count_per_source_and_window`] | Each source and window counts on its own |
//! | [`concurrent_records_count_every_signal`] | Concurrent records of one key from two handles open one record and count every signal once |
//! | [`records_survive_a_restart`] | Records and counts read back through a new handle (durable backends only) |

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};

use super::fixtures::{content_gap, review};
use crate::charter::{SignalFilter, SignalKey, SignalStore, signal_key};
use crate::partition::PartitionId;

/// Opens a [`SignalStore`] backend for the conformance suite (spec 3.5 §11).
#[async_trait]
pub trait SignalStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn SignalStore>;

    /// Whether data written through one handle is visible through the next.
    fn is_durable(&self) -> bool {
        true
    }
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 20, 8, 0, 0).single().expect("valid")
        + Duration::seconds(seconds)
        + Duration::microseconds(11)
}

/// A fresh back-office partition.
fn partition() -> PartitionId {
    PartitionId::new(format!("back-office/ü {}", uuid::Uuid::new_v4().simple()))
}

fn id() -> String {
    format!("signal-{}", uuid::Uuid::new_v4().simple())
}

/// The first signal opens a record; each repeat raises its count.
pub async fn first_opens_then_counts(harness: &impl SignalStoreHarness) {
    let store = harness.open().await;
    let partition = partition();
    let first = content_gap("Opening hours", 1);
    let key = signal_key(&partition, &first);
    let new_id = id();
    let (opened, new) =
        store.record(&key, &first, "concierge", at(0), &new_id).await.expect("record");
    assert!(new);
    assert_eq!((opened.id.as_str(), opened.count, opened.open), (new_id.as_str(), 1, true));
    assert_eq!((opened.first_at, opened.last_at), (at(0), at(0)));
    assert_eq!(
        (opened.kind.as_str(), opened.topic.as_str()),
        (key.kind.as_str(), key.topic.as_str())
    );
    let repeat = content_gap("opening hours", 3);
    let (counted, new) = store.record(&key, &repeat, "inbox", at(5), &id()).await.expect("record");
    assert!(!new);
    assert_eq!((counted.id.as_str(), counted.count), (new_id.as_str(), 2));
    assert_eq!((counted.first_at, counted.last_at), (at(0), at(5)));
    assert_eq!((counted.signal.clone(), counted.source.as_str()), (repeat, "inbox"));
    assert_eq!(store.get(&new_id).await.expect("get"), Some(counted));
    assert!(store.get(&id()).await.expect("get").is_none());
}

/// Another partition, kind or topic opens its own record.
pub async fn keys_are_apart(harness: &impl SignalStoreHarness) {
    let store = harness.open().await;
    let (here, there) = (partition(), partition());
    let signals = [
        (here.clone(), content_gap("parking", 1)),
        (here.clone(), content_gap("delivery", 1)),
        (here.clone(), review("maps", 5, "parking was easy")),
        (there.clone(), content_gap("parking", 1)),
    ];
    let mut ids = BTreeSet::new();
    for (partition, signal) in &signals {
        let key = signal_key(partition, signal);
        let (record, new) =
            store.record(&key, signal, "concierge", at(0), &id()).await.expect("record");
        assert!(new, "{key:?}");
        ids.insert(record.id);
    }
    assert_eq!(ids.len(), 4);
}

/// A closed record keeps its count; the key's next signal opens a new record.
pub async fn close_then_reopen(harness: &impl SignalStoreHarness) {
    let store = harness.open().await;
    let partition = partition();
    let signal = content_gap("returns", 1);
    let key = signal_key(&partition, &signal);
    let (first, _) = store.record(&key, &signal, "concierge", at(0), &id()).await.expect("record");
    store.record(&key, &signal, "concierge", at(1), &id()).await.expect("record");
    assert!(store.close(&first.id).await.expect("close"));
    assert!(!store.close(&first.id).await.expect("close"), "already closed");
    assert!(!store.close(&id()).await.expect("close"), "no such record");
    let closed = store.get(&first.id).await.expect("get").expect("kept");
    assert_eq!((closed.open, closed.count), (false, 2));
    let (reopened, new) =
        store.record(&key, &signal, "concierge", at(2), &id()).await.expect("record");
    assert!(new);
    assert_ne!(reopened.id, first.id);
    assert_eq!(reopened.count, 1);
    assert_eq!(store.get(&first.id).await.expect("get").map(|r| r.count), Some(2), "unchanged");
}

/// `list` filters by partition, kind and openness, oldest first, up to the limit.
pub async fn list_filters_and_orders(harness: &impl SignalStoreHarness) {
    let store = harness.open().await;
    let partition = partition();
    let mut expected = Vec::new();
    for (seconds, signal) in
        [(30, content_gap("b", 1)), (10, content_gap("a", 1)), (20, review("maps", 4, "good"))]
    {
        let key = signal_key(&partition, &signal);
        let (record, _) =
            store.record(&key, &signal, "concierge", at(seconds), &id()).await.expect("record");
        expected.push((seconds, record.id));
    }
    expected.sort();
    let all =
        store.list(&SignalFilter::default().partition(partition.clone())).await.expect("list");
    let ids: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, expected.iter().map(|(_, id)| id.as_str()).collect::<Vec<_>>());
    let gaps = store
        .list(&SignalFilter::default().partition(partition.clone()).kind("content_gap"))
        .await
        .expect("list");
    assert_eq!(gaps.len(), 2);
    store.close(&expected[0].1).await.expect("close");
    let open = store
        .list(&SignalFilter::default().partition(partition.clone()).open(true))
        .await
        .expect("list");
    assert_eq!(open.len(), 2);
    let closed = store
        .list(&SignalFilter::default().partition(partition.clone()).open(false))
        .await
        .expect("list");
    assert_eq!(
        closed.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        vec![expected[0].1.clone()]
    );
    let first =
        store.list(&SignalFilter::default().partition(partition).limit(1)).await.expect("list");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, expected[0].1);
}

/// Each source and window counts on its own.
pub async fn hits_count_per_source_and_window(harness: &impl SignalStoreHarness) {
    let store = harness.open().await;
    let source = format!("concierge/{}", uuid::Uuid::new_v4().simple());
    let other = format!("inbox/{}", uuid::Uuid::new_v4().simple());
    let window = Utc::now() - Duration::seconds(30);
    for expected in 1..=3 {
        assert_eq!(store.hit(&source, window).await.expect("hit"), expected);
    }
    assert_eq!(store.hit(&other, window).await.expect("hit"), 1);
    assert_eq!(store.hit(&source, window + Duration::minutes(1)).await.expect("hit"), 1);
}

/// Concurrent records of one key from two handles open one record and count every signal.
pub async fn concurrent_records_count_every_signal(harness: &impl SignalStoreHarness) {
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { Arc::clone(&first) };
    let partition = partition();
    let signal = content_gap("same topic", 1);
    let key: SignalKey = signal_key(&partition, &signal);
    let mut tasks = Vec::new();
    for index in 0..40_i64 {
        let store = if index % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
        let (key, signal) = (key.clone(), signal.clone());
        tasks.push(tokio::spawn(async move {
            store.record(&key, &signal, "concierge", at(index), &id()).await
        }));
    }
    let mut opened = 0;
    let mut counts = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for task in tasks {
        let (record, new) = task.await.expect("task").expect("record");
        opened += usize::from(new);
        counts.insert(record.count);
        ids.insert(record.id);
    }
    assert_eq!((opened, ids.len()), (1, 1), "one record opened");
    assert_eq!(counts, (1..=40).collect::<BTreeSet<u32>>(), "every signal counted once");
    let id = ids.into_iter().next().expect("an id");
    assert_eq!(second.get(&id).await.expect("get").map(|r| r.count), Some(40));
}

/// Records and counts read back through a new handle.
pub async fn records_survive_a_restart(harness: &impl SignalStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let partition = partition();
    let signal = content_gap("restart", 1);
    let key = signal_key(&partition, &signal);
    let writer = harness.open().await;
    let (record, _) =
        writer.record(&key, &signal, "concierge", at(0), &id()).await.expect("record");
    let reader = harness.open().await;
    let (again, new) =
        reader.record(&key, &signal, "concierge", at(1), &id()).await.expect("record");
    assert!(!new);
    assert_eq!((again.id, again.count), (record.id, 2));
}
