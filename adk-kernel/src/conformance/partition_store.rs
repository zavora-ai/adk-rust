//! Conformance cases for [`PartitionStore`] backends (spec 3.2 §7.8, §11, §13 Q8).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`put_get_round_trip`] | A partition reads back exactly as put, every field set; ids with `:`, `/` and non-ASCII characters survive |
//! | [`put_replaces`] | A second put with the same id replaces the record |
//! | [`list_orders_by_id`] | `list` returns every partition, ordered by id in byte order |
//! | [`delete`] | `delete` removes the partition and answers `true`, then `false`; the partition's crossings stay |
//! | [`unknown_partition`] | `get` of an unknown id is `None` and `delete` answers `false` |
//! | [`crossings_filter_and_order`] | `crossings` applies source, target, run, fingerprint, outcomes, since and limit, and orders by time, then id |
//! | [`crossing_records_round_trip`] | A crossing record reads back exactly as recorded, every outcome, resource and access included |
//! | [`run_grants_stay_in_their_run`] | Through [`Partitions`], an approved read is granted for the rest of its run only; an approved write grants nothing |
//! | [`released_runs_hold_no_grants`] | `grant` keeps one grant per run and fingerprint, `grants` lists a run's, and `release_run` drops them all and answers how many, leaving other runs' |
//! | [`records_survive_a_restart`] | Partitions and crossings read back through a new handle (durable backends only) |
//! | [`concurrent_writers`] | Concurrent puts and crossing records from two handles lose nothing |
//!
//! Every case writes under fresh partition ids and runs, so a harness may point every case
//! at one shared database. Crossing records have no delete, so they stay under ids no other
//! case uses.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::partition::{
    CrossPartition, Crossing, CrossingAccess, CrossingFilter, CrossingOutcome, CrossingRecord,
    CrossingResource, CrossingRun, Partition, PartitionId, PartitionMode, PartitionStore,
    Partitions,
};

/// Opens a [`PartitionStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::conformance::PartitionStoreHarness;
/// use adk_kernel::{InMemoryPartitionStore, PartitionStore};
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl PartitionStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn PartitionStore> {
///         Arc::new(InMemoryPartitionStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait PartitionStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn PartitionStore>;

    /// Whether data written through one handle is visible through the next. `false` skips
    /// the restart case and runs the concurrency case on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

/// A fresh identifier with the characters a backend must escape: `:`, `/`, non-ASCII.
fn unique(prefix: &str) -> String {
    format!("conformance:{prefix}/ü-日本/{}", uuid::Uuid::new_v4())
}

fn partition_id() -> PartitionId {
    PartitionId::new(unique("partition"))
}

/// An instant on a whole second, so every backend's order agrees.
fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, 0).single().expect("valid instant")
}

/// A partition with every field set.
fn full(id: &PartitionId) -> Partition {
    Partition::new(id.clone(), "ü-class")
        .with_tenant("acme/日本")
        .with_mode(PartitionMode::Plan)
        .with_memory_scope(unique("scope"))
        .with_driver("calendar")
        .with_driver(unique("driver"))
        .with_cross_partition(CrossPartition::CoordinatorWithApproval)
        .with_blocked_crossing(partition_id())
}

fn record(
    from: &PartitionId,
    to: &PartitionId,
    run: &str,
    at: i64,
    outcome: CrossingOutcome,
) -> CrossingRecord {
    CrossingRecord {
        id: unique("crossing"),
        at: instant(at),
        run: run.to_string(),
        agent: "urn:adk:agent:app/harness".to_string(),
        from: from.clone(),
        to: to.clone(),
        resource: CrossingResource::Driver,
        name: "calendar".to_string(),
        access: CrossingAccess::Read,
        tool: "work__list_events".to_string(),
        fingerprint: format!("sha256:{}", uuid::Uuid::new_v4().simple()),
        outcome,
        reason: None,
    }
}

async fn ids(store: &dyn PartitionStore, filter: &CrossingFilter) -> Vec<String> {
    store.crossings(filter).await.expect("crossings").into_iter().map(|record| record.id).collect()
}

/// A partition reads back exactly as put, every field set.
pub async fn put_get_round_trip(harness: &dyn PartitionStoreHarness) {
    let store = harness.open().await;
    let id = partition_id();
    let partition = full(&id);
    store.put(partition.clone()).await.expect("put");
    assert_eq!(store.get(&id).await.expect("get"), Some(partition));
    let bare = partition_id();
    store.put(Partition::new(bare.clone(), "class")).await.expect("put");
    assert_eq!(store.get(&bare).await.expect("get"), Some(Partition::new(bare.clone(), "class")));
    for id in [id, bare] {
        store.delete(&id).await.expect("delete");
    }
}

/// A second put with the same id replaces the record.
pub async fn put_replaces(harness: &dyn PartitionStoreHarness) {
    let store = harness.open().await;
    let id = partition_id();
    store.put(full(&id)).await.expect("put");
    let replaced = Partition::new(id.clone(), "other").with_mode(PartitionMode::Auto);
    store.put(replaced.clone()).await.expect("put again");
    assert_eq!(store.get(&id).await.expect("get"), Some(replaced));
    let listed: Vec<_> =
        store.list().await.expect("list").into_iter().filter(|p| p.id == id).collect();
    assert_eq!(listed.len(), 1);
    store.delete(&id).await.expect("delete");
}

/// `list` returns every partition, ordered by id in byte order.
pub async fn list_orders_by_id(harness: &dyn PartitionStoreHarness) {
    let store = harness.open().await;
    let base = unique("list");
    // Byte order: uppercase before lowercase, ASCII before non-ASCII.
    let mine: Vec<PartitionId> = ["b", "B", "a", "é", "a0"]
        .into_iter()
        .map(|suffix| PartitionId::new(format!("{base}:{suffix}")))
        .collect();
    for id in &mine {
        store.put(Partition::new(id.clone(), "class")).await.expect("put");
    }
    let listed: Vec<PartitionId> = store
        .list()
        .await
        .expect("list")
        .into_iter()
        .map(|partition| partition.id)
        .filter(|id| mine.contains(id))
        .collect();
    let mut expected = mine.clone();
    expected.sort_by(|a, b| a.as_str().as_bytes().cmp(b.as_str().as_bytes()));
    assert_eq!(listed, expected);
    let all: Vec<PartitionId> =
        store.list().await.expect("list").into_iter().map(|partition| partition.id).collect();
    let mut sorted = all.clone();
    sorted.sort_by(|a, b| a.as_str().as_bytes().cmp(b.as_str().as_bytes()));
    assert_eq!(all, sorted, "the whole list is in byte order");
    for id in &mine {
        store.delete(id).await.expect("delete");
    }
}

/// `delete` removes the partition, then answers `false`; the partition's crossings stay.
pub async fn delete(harness: &dyn PartitionStoreHarness) {
    let store = harness.open().await;
    let (id, to) = (partition_id(), partition_id());
    store.put(full(&id)).await.expect("put");
    let run = unique("run");
    let crossing = record(&id, &to, &run, 1, CrossingOutcome::Held);
    store.record_crossing(crossing.clone()).await.expect("record");
    assert!(store.delete(&id).await.expect("delete"));
    assert_eq!(store.get(&id).await.expect("get"), None);
    assert!(!store.list().await.expect("list").iter().any(|partition| partition.id == id));
    assert!(!store.delete(&id).await.expect("delete again"));
    assert_eq!(ids(store.as_ref(), &CrossingFilter::default().run(&run)).await, [crossing.id]);
}

/// `get` of an unknown id is `None` and `delete` answers `false`.
pub async fn unknown_partition(harness: &dyn PartitionStoreHarness) {
    let store = harness.open().await;
    let id = partition_id();
    assert_eq!(store.get(&id).await.expect("get"), None);
    assert!(!store.delete(&id).await.expect("delete"));
    assert!(
        store.crossings(&CrossingFilter::default().run(unique("run"))).await.unwrap().is_empty()
    );
}

/// `crossings` applies every filter field and orders by time, then id.
pub async fn crossings_filter_and_order(harness: &dyn PartitionStoreHarness) {
    let store = harness.open().await;
    let (home, work, team) = (partition_id(), partition_id(), partition_id());
    let (run_a, run_b) = (unique("run"), unique("run"));
    let mut records = vec![
        record(&home, &work, &run_a, 30, CrossingOutcome::Held),
        record(&home, &work, &run_a, 10, CrossingOutcome::Denied),
        record(&home, &team, &run_b, 20, CrossingOutcome::Approved),
        record(&work, &home, &run_b, 40, CrossingOutcome::RunGrant),
        // Two at one instant order by id.
        record(&home, &work, &run_a, 50, CrossingOutcome::Approved),
        record(&home, &work, &run_a, 50, CrossingOutcome::Approved),
    ];
    for record in &records {
        store.record_crossing(record.clone()).await.expect("record");
    }
    records.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    let expect = |keep: &dyn Fn(&CrossingRecord) -> bool| -> Vec<String> {
        records.iter().filter(|record| keep(record)).map(|record| record.id.clone()).collect()
    };
    let in_runs = |record: &CrossingRecord| record.run == run_a || record.run == run_b;

    let from_home = ids(store.as_ref(), &CrossingFilter::default().from(home.clone())).await;
    assert_eq!(from_home, expect(&|r| r.from == home && in_runs(r)));
    let into_work = ids(store.as_ref(), &CrossingFilter::default().to(work.clone())).await;
    assert_eq!(into_work, expect(&|r| r.to == work && in_runs(r)));
    let run = ids(store.as_ref(), &CrossingFilter::default().run(&run_a)).await;
    assert_eq!(run, expect(&|r| r.run == run_a));
    let fingerprint = records[0].fingerprint.clone();
    let by_fingerprint =
        ids(store.as_ref(), &CrossingFilter::default().fingerprint(fingerprint.clone())).await;
    assert_eq!(by_fingerprint, [records[0].id.clone()]);
    let outcomes = CrossingFilter::default()
        .from(home.clone())
        .outcomes([CrossingOutcome::Approved, CrossingOutcome::Held]);
    assert_eq!(
        ids(store.as_ref(), &outcomes).await,
        expect(&|r| r.from == home
            && in_runs(r)
            && matches!(r.outcome, CrossingOutcome::Approved | CrossingOutcome::Held))
    );
    let since = CrossingFilter::default().run(&run_a).since(instant(30));
    assert_eq!(
        ids(store.as_ref(), &since).await,
        expect(&|r| r.run == run_a && r.at >= instant(30))
    );
    let limited = CrossingFilter::default().run(&run_a).limit(2);
    assert_eq!(ids(store.as_ref(), &limited).await, expect(&|r| r.run == run_a)[..2]);
    let combined = CrossingFilter::default()
        .from(home.clone())
        .to(work.clone())
        .run(&run_a)
        .outcomes([CrossingOutcome::Approved])
        .since(instant(50))
        .limit(1);
    assert_eq!(
        ids(store.as_ref(), &combined).await,
        expect(&|r| r.run == run_a && r.outcome == CrossingOutcome::Approved)[..1]
    );
}

/// A crossing record reads back exactly as recorded.
pub async fn crossing_records_round_trip(harness: &dyn PartitionStoreHarness) {
    let store = harness.open().await;
    let (from, to) = (partition_id(), partition_id());
    let run = unique("run");
    let outcomes = [
        CrossingOutcome::Denied,
        CrossingOutcome::Held,
        CrossingOutcome::Approved,
        CrossingOutcome::RunGrant,
    ];
    let resources = [CrossingResource::Memory, CrossingResource::Driver, CrossingResource::Session];
    let mut recorded = Vec::new();
    for (index, outcome) in outcomes.into_iter().enumerate() {
        let mut crossing = record(&from, &to, &run, i64::try_from(index).unwrap_or(0), outcome);
        crossing.resource = resources[index % resources.len()];
        crossing.access = if index % 2 == 0 { CrossingAccess::Read } else { CrossingAccess::Write };
        crossing.reason =
            (outcome == CrossingOutcome::Denied).then(|| "line one\nline \"two\" ü".to_string());
        crossing.name = unique("name");
        store.record_crossing(crossing.clone()).await.expect("record");
        recorded.push(crossing);
    }
    let read = store.crossings(&CrossingFilter::default().run(&run)).await.expect("crossings");
    assert_eq!(read, recorded);
}

/// Through [`Partitions`], an approved read is granted for the rest of its run only, and an
/// approved write grants nothing.
pub async fn run_grants_stay_in_their_run(harness: &dyn PartitionStoreHarness) {
    let store = harness.open().await;
    let partitions = Partitions::new(store);
    let (home, work) = (partition_id(), partition_id());
    let (run, next_run) =
        (CrossingRun::Durable(unique("run")), CrossingRun::Durable(unique("run")));
    let read = Crossing::new(
        home.clone(),
        work.clone(),
        CrossingResource::Memory,
        "memory",
        CrossingAccess::Read,
    );
    let write =
        Crossing::new(home, work, CrossingResource::Memory, "memory", CrossingAccess::Write);
    let (read_print, write_print) = (unique("read"), unique("write"));
    assert!(!partitions.granted(&run, &read_print).await);
    partitions.executed(&read, &read_print, &run, "agent", "tool").await.expect("executed");
    partitions.executed(&write, &write_print, &run, "agent", "tool").await.expect("executed");
    assert!(partitions.granted(&run, &read_print).await, "the read holds for the run");
    assert!(!partitions.granted(&next_run, &read_print).await, "and nothing beyond it");
    assert!(!partitions.granted(&run, &write_print).await, "a write stays one-shot");
    // A read the run already holds is not recorded again.
    partitions.executed(&read, &read_print, &run, "agent", "tool").await.expect("executed");
    let approved =
        CrossingFilter::default().run(run.as_str()).outcomes([CrossingOutcome::Approved]);
    assert_eq!(partitions.crossings(&approved).await.expect("crossings").len(), 2);
    assert_eq!(partitions.grants(&run).await.expect("grants").len(), 1, "the write is no grant");
    // A finished run holds none.
    assert_eq!(partitions.release_run(&run).await.expect("release"), 1);
    assert!(!partitions.granted(&run, &read_print).await);
}

/// `grant` keeps one grant per run and fingerprint, `grants` lists a run's, and
/// `release_run` drops them all and answers how many, leaving other runs'.
pub async fn released_runs_hold_no_grants(harness: &dyn PartitionStoreHarness) {
    let store = harness.open().await;
    let (run, other) = (unique("run"), unique("run"));
    for fingerprint in ["sha256:b", "sha256:a", "sha256:a"] {
        store.grant(&run, fingerprint).await.expect("grant");
    }
    store.grant(&other, "sha256:a").await.expect("grant");
    let reopened = if harness.is_durable() { harness.open().await } else { store.clone() };
    assert_eq!(reopened.grants(&run).await.expect("grants"), ["sha256:a", "sha256:b"]);
    assert_eq!(reopened.release_run(&run).await.expect("release"), 2);
    assert!(reopened.grants(&run).await.expect("grants").is_empty());
    assert_eq!(reopened.release_run(&run).await.expect("release again"), 0);
    assert_eq!(reopened.grants(&other).await.expect("grants"), ["sha256:a"]);
    reopened.release_run(&other).await.expect("release");
}

/// Partitions and crossings read back through a new handle (durable backends only).
pub async fn records_survive_a_restart(harness: &dyn PartitionStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let id = partition_id();
    let partition = full(&id);
    let run = unique("run");
    let crossing = record(&id, &partition_id(), &run, 5, CrossingOutcome::Approved);
    {
        let store = harness.open().await;
        store.put(partition.clone()).await.expect("put");
        store.record_crossing(crossing.clone()).await.expect("record");
    }
    let store = harness.open().await;
    assert_eq!(store.get(&id).await.expect("get"), Some(partition));
    assert_eq!(
        store.crossings(&CrossingFilter::default().run(&run)).await.expect("crossings"),
        [crossing]
    );
    store.delete(&id).await.expect("delete");
}

/// Concurrent puts and crossing records from two handles lose nothing.
pub async fn concurrent_writers(harness: &dyn PartitionStoreHarness) {
    let (first, second) = (harness.open().await, harness.open().await);
    let second = if harness.is_durable() { second } else { first.clone() };
    let run = unique("run");
    let (from, to) = (partition_id(), partition_id());
    let mut tasks = Vec::new();
    for index in 0..16_i64 {
        let store = if index % 2 == 0 { first.clone() } else { second.clone() };
        let id = PartitionId::new(format!("{}:{index}", from.as_str()));
        let crossing = record(&from, &to, &run, index, CrossingOutcome::Held);
        tasks.push(tokio::spawn(async move {
            store.put(Partition::new(id.clone(), "class")).await.expect("put");
            store.record_crossing(crossing.clone()).await.expect("record");
            (id, crossing.id)
        }));
    }
    let mut written = Vec::new();
    for task in tasks {
        written.push(task.await.expect("task"));
    }
    let listed: BTreeSet<PartitionId> =
        first.list().await.expect("list").into_iter().map(|partition| partition.id).collect();
    for (id, _) in &written {
        assert!(listed.contains(id), "partition {id} was lost");
    }
    let crossings: BTreeSet<String> =
        ids(first.as_ref(), &CrossingFilter::default().run(&run)).await.into_iter().collect();
    let expected: BTreeSet<String> = written.iter().map(|(_, crossing)| crossing.clone()).collect();
    assert_eq!(crossings, expected);
    for (id, _) in written {
        first.delete(&id).await.expect("delete");
    }
}
