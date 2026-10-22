//! Conformance cases for [`ProcessStore`] backends (spec 3.2 §11, §13 Q8).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`put_get_round_trip`] | A row reads back exactly as put, every field and every [`Wake`](crate::Wake), [`ProcessKind`](crate::ProcessKind) and [`Suspension`](crate::Suspension) variant included; ids with `:`, `/` and non-ASCII characters survive |
//! | [`put_replaces`] | A second put with the same `pid` replaces the row, and `list` finds it only under its new partition, queue and parent |
//! | [`list_filters_and_orders`] | `list` applies partition, queue, parent and limit, orders by `pid` in byte order, and ignores `states` |
//! | [`set_wake_and_queue`] | `set_wake` sets and clears the wake set, `set_queue` the queue, leaving every other field |
//! | [`unknown_pid`] | `get` of an unknown `pid` is `None`; `set_wake` and `set_queue` fail with `kernel.not_found` and create no row; `delete` answers `false` |
//! | [`delete`] | `delete` removes the row and its `list` entries and answers `true`, then `false` |
//! | [`records_survive_a_restart`] | Rows read back through a new handle (durable backends only) |
//! | [`concurrent_writers`] | Concurrent puts and updates from two handles over distinct pids lose no row, and concurrent updates of different fields of one row lose neither |
//! | [`sustained_writes`] | A sustained stream of puts, updates and deletes from two handles, read concurrently, stays consistent and readable (SQLite's write lock and WAL included) |
//!
//! Every case writes under fresh process ids in fresh partitions and deletes its rows at
//! the end, so a harness may point every case at one shared database.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::MissedFirePolicy;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::error::{KernelError, NOT_FOUND_CODE};
use crate::partition::PartitionId;
use crate::process::{ProcessKind, ProcessMeta, Suspension};
use crate::sched::QueueClass;
use crate::stores::{ProcessFilter, ProcessStore};
use crate::wake::{ExpiryAction, Wake, WakeSet};

/// Opens a [`ProcessStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::conformance::ProcessStoreHarness;
/// use adk_kernel::{InMemoryProcessStore, ProcessStore};
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl ProcessStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn ProcessStore> {
///         Arc::new(InMemoryProcessStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait ProcessStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn ProcessStore>;

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

/// A fresh partition, so `list` in one case never sees another case's rows.
fn partition() -> PartitionId {
    PartitionId::new(unique("partition"))
}

/// An instant with nanoseconds, so a backend that rounds timestamps is caught.
fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, 123_456_789).single().expect("valid instant")
}

/// A wake set holding every [`Wake`] variant, with an expiry that runs the process.
fn every_wake(partition: &PartitionId) -> WakeSet {
    WakeSet::any_of([
        Wake::At(instant(60)),
        Wake::Cron { expr: "0 9 * * 1-5".into(), missed: MissedFirePolicy::CatchUp(3) },
        Wake::FileChange {
            root: "/srv/ü repo/日本".into(),
            glob: "**/{Cargo.lock,*.rs}".into(),
            debounce: Duration::from_millis(750),
        },
        Wake::Webhook { hook: unique("hook") },
        Wake::Queue { source: unique("queue-source") },
        Wake::Approval { fingerprint: unique("fingerprint") },
        Wake::RunFinished { pid: unique("run") },
        Wake::BudgetReplenished { partition: partition.clone() },
        Wake::Manual,
    ])
    .with_expiry(instant(3_600), ExpiryAction::Run)
}

/// A row with every field set.
fn full_meta(pid: &str, partition: &PartitionId) -> ProcessMeta {
    ProcessMeta::new(pid, QueueClass::Background, ProcessKind::Triggered { source: unique("src") })
        .with_parent(unique("parent"))
        .with_partition(partition.clone())
        .with_wake(every_wake(partition))
        .with_suspension(Suspension::Preempted { by: unique("run") })
        .with_agenda_item(unique("agenda"))
        .with_label("repo", "adk-rust")
        .with_label("ü-日本", "line one\nline \"two\"")
}

async fn list_pids(store: &dyn ProcessStore, filter: &ProcessFilter) -> Vec<String> {
    store.list(filter).await.expect("list").into_iter().map(|meta| meta.pid).collect()
}

async fn cleanup(store: &dyn ProcessStore, pids: &[String]) {
    for pid in pids {
        store.delete(pid).await.expect("delete");
    }
}

/// A row reads back exactly as put: every field, every [`Wake`], [`ProcessKind`] and
/// [`Suspension`] variant, named queues, unicode labels, and ids with `:`, `/` and
/// non-ASCII characters.
pub async fn put_get_round_trip(harness: &dyn ProcessStoreHarness) {
    let store = harness.open().await;
    let partition = partition();
    let kinds = [
        ProcessKind::Interactive,
        ProcessKind::Goal,
        ProcessKind::Scheduled,
        ProcessKind::Triggered { source: unique("source") },
        ProcessKind::Proposed,
        ProcessKind::Maintenance,
    ];
    let suspensions = [
        None,
        Some(Suspension::Manual),
        Some(Suspension::Preempted { by: unique("run") }),
        Some(Suspension::Freeze),
        Some(Suspension::Drain),
    ];
    let queues = [
        QueueClass::Interactive,
        QueueClass::Background,
        QueueClass::Maintenance,
        QueueClass::Named(unique("queue")),
    ];
    let mut rows = Vec::new();
    for (index, kind) in kinds.into_iter().enumerate() {
        let mut meta = ProcessMeta::new(unique("pid"), queues[index % queues.len()].clone(), kind)
            .with_partition(partition.clone());
        if let Some(suspension) = suspensions[index % suspensions.len()].clone() {
            meta = meta.with_suspension(suspension);
        }
        if index % 2 == 0 {
            meta = meta.with_wake(every_wake(&partition)).with_parent(unique("parent"));
        }
        rows.push(meta);
    }
    rows.push(full_meta(&unique("pid"), &partition));
    rows.push(
        ProcessMeta::new(unique("pid"), QueueClass::Background, ProcessKind::Goal)
            .with_partition(partition.clone())
            .with_wake(WakeSet::any_of([Wake::Manual])),
    );
    // The default partition, as a phase-1 process has.
    rows.push(ProcessMeta::new(unique("pid"), QueueClass::Interactive, ProcessKind::Interactive));

    for meta in &rows {
        store.put(meta.clone()).await.expect("put");
    }
    for meta in &rows {
        assert_eq!(store.get(&meta.pid).await.expect("get").as_ref(), Some(meta), "{}", meta.pid);
    }
    let pids: Vec<String> = rows.iter().map(|meta| meta.pid.clone()).collect();
    cleanup(&*store, &pids).await;
}

/// A second put with the same `pid` replaces the whole row; `list` then finds it under its
/// new partition, queue and parent only.
pub async fn put_replaces(harness: &dyn ProcessStoreHarness) {
    let store = harness.open().await;
    let (before, after) = (partition(), partition());
    let pid = unique("pid");
    let first = full_meta(&pid, &before);
    store.put(first.clone()).await.expect("put");
    let old_parent = first.parent.clone().expect("parent");

    let second = ProcessMeta::new(&pid, QueueClass::Interactive, ProcessKind::Interactive)
        .with_partition(after.clone());
    store.put(second.clone()).await.expect("second put");
    assert_eq!(store.get(&pid).await.expect("get"), Some(second.clone()));

    let empty: Vec<String> = Vec::new();
    assert_eq!(
        list_pids(&*store, &ProcessFilter::default().partition(before.clone())).await,
        empty
    );
    assert_eq!(list_pids(&*store, &ProcessFilter::default().parent(old_parent)).await, empty);
    assert_eq!(
        list_pids(
            &*store,
            &ProcessFilter::default().partition(before).queue(QueueClass::Background)
        )
        .await,
        empty
    );
    assert_eq!(
        store.list(&ProcessFilter::default().partition(after.clone())).await.expect("list"),
        std::slice::from_ref(&second)
    );
    assert_eq!(
        list_pids(
            &*store,
            &ProcessFilter::default().partition(after).queue(QueueClass::Interactive)
        )
        .await,
        std::slice::from_ref(&pid)
    );
    cleanup(&*store, &[pid]).await;
}

/// `list` applies partition, queue, parent and limit together, orders by `pid` in byte
/// order (so `B` < `Z` < `a` < `ä` < `日`), and ignores `states`, which the process table
/// applies after joining the run store.
pub async fn list_filters_and_orders(harness: &dyn ProcessStoreHarness) {
    let store = harness.open().await;
    let (here, there) = (partition(), partition());
    let prefix = unique("pid");
    let parent = unique("parent");
    let named = QueueClass::Named(unique("queue"));
    let pid = |suffix: &str| format!("{prefix}/{suffix}");
    // Put out of order; `with_parent` marks the rows `parent` caused.
    let rows = [
        ProcessMeta::new(pid("日"), QueueClass::Background, ProcessKind::Goal)
            .with_partition(here.clone())
            .with_parent(parent.clone()),
        ProcessMeta::new(pid("a"), QueueClass::Interactive, ProcessKind::Interactive)
            .with_partition(here.clone()),
        ProcessMeta::new(pid("Z"), QueueClass::Background, ProcessKind::Scheduled)
            .with_partition(here.clone())
            .with_parent(parent.clone()),
        ProcessMeta::new(pid("ä"), QueueClass::Interactive, ProcessKind::Proposed)
            .with_partition(here.clone()),
        ProcessMeta::new(pid("B"), named.clone(), ProcessKind::Maintenance)
            .with_partition(here.clone()),
        ProcessMeta::new(pid("c"), QueueClass::Background, ProcessKind::Goal)
            .with_partition(there.clone())
            .with_parent(parent.clone()),
        ProcessMeta::new(pid("A"), QueueClass::Interactive, ProcessKind::Goal)
            .with_partition(there.clone()),
    ];
    for meta in &rows {
        store.put(meta.clone()).await.expect("put");
    }
    let ours: Vec<String> = rows.iter().map(|meta| meta.pid.clone()).collect();
    let pids = |suffixes: &[&str]| suffixes.iter().map(|suffix| pid(suffix)).collect::<Vec<_>>();

    let in_here = ProcessFilter::default().partition(here.clone());
    assert_eq!(list_pids(&*store, &in_here).await, pids(&["B", "Z", "a", "ä", "日"]));
    let full: Vec<ProcessMeta> = store.list(&in_here).await.expect("list");
    let mut expected: Vec<ProcessMeta> =
        rows.iter().filter(|meta| meta.partition == here).cloned().collect();
    expected.sort_by(|a, b| a.pid.cmp(&b.pid));
    assert_eq!(full, expected, "list returns whole rows");
    assert_eq!(
        list_pids(&*store, &in_here.clone().queue(QueueClass::Interactive)).await,
        pids(&["a", "ä"])
    );
    assert_eq!(
        list_pids(&*store, &in_here.clone().queue(QueueClass::Background)).await,
        pids(&["Z", "日"])
    );
    assert_eq!(list_pids(&*store, &ProcessFilter::default().queue(named)).await, pids(&["B"]));
    assert_eq!(
        list_pids(&*store, &ProcessFilter::default().parent(parent.clone())).await,
        pids(&["Z", "c", "日"])
    );
    assert_eq!(
        list_pids(&*store, &in_here.clone().parent(parent.clone())).await,
        pids(&["Z", "日"])
    );
    assert_eq!(
        list_pids(
            &*store,
            &ProcessFilter::default().partition(there.clone()).parent(parent.clone())
        )
        .await,
        pids(&["c"])
    );
    assert_eq!(
        list_pids(&*store, &in_here.clone().parent(parent).queue(QueueClass::Interactive)).await,
        Vec::<String>::new()
    );
    assert_eq!(list_pids(&*store, &in_here.clone().limit(2)).await, pids(&["B", "Z"]));
    assert_eq!(
        list_pids(&*store, &in_here.clone().queue(QueueClass::Background).limit(1)).await,
        pids(&["Z"])
    );
    assert_eq!(list_pids(&*store, &in_here.clone().limit(0)).await, Vec::<String>::new());
    assert_eq!(
        list_pids(
            &*store,
            &in_here.clone().states([adk_core::RunState::Completed, adk_core::RunState::Failed])
        )
        .await,
        pids(&["B", "Z", "a", "ä", "日"]),
        "states is not a side-table field"
    );

    let everything = list_pids(&*store, &ProcessFilter::default()).await;
    assert!(everything.windows(2).all(|pair| pair[0] < pair[1]), "list orders by pid");
    let found: Vec<String> = everything.into_iter().filter(|pid| ours.contains(pid)).collect();
    let mut sorted = ours.clone();
    sorted.sort();
    assert_eq!(found, sorted, "an unfiltered list holds every row");
    cleanup(&*store, &ours).await;
}

/// `set_wake` sets and clears the wake set and `set_queue` sets the queue, each leaving
/// every other field as it was; `list` finds the row under its new queue.
pub async fn set_wake_and_queue(harness: &dyn ProcessStoreHarness) {
    let store = harness.open().await;
    let partition = partition();
    let pid = unique("pid");
    let meta = full_meta(&pid, &partition);
    store.put(meta.clone()).await.expect("put");

    let replacement = WakeSet::any_of([Wake::RunFinished { pid: unique("run") }]);
    store.set_wake(&pid, Some(replacement.clone())).await.expect("set wake");
    let mut expected = meta.clone();
    expected.wake = Some(replacement);
    assert_eq!(store.get(&pid).await.expect("get"), Some(expected.clone()));

    store.set_wake(&pid, None).await.expect("clear wake");
    expected.wake = None;
    assert_eq!(store.get(&pid).await.expect("get"), Some(expected.clone()));
    store.set_wake(&pid, None).await.expect("clearing twice is not an error");

    store.set_queue(&pid, QueueClass::Interactive).await.expect("set queue");
    expected.queue = QueueClass::Interactive;
    assert_eq!(store.get(&pid).await.expect("get"), Some(expected.clone()));
    let in_partition = ProcessFilter::default().partition(partition.clone());
    assert_eq!(
        list_pids(&*store, &in_partition.clone().queue(QueueClass::Interactive)).await,
        std::slice::from_ref(&pid)
    );
    assert_eq!(
        list_pids(&*store, &in_partition.clone().queue(QueueClass::Background)).await,
        Vec::<String>::new()
    );

    let named = QueueClass::Named(unique("queue"));
    store.set_queue(&pid, named.clone()).await.expect("set a named queue");
    store.set_queue(&pid, named.clone()).await.expect("setting the same queue again");
    expected.queue = named.clone();
    assert_eq!(store.get(&pid).await.expect("get"), Some(expected.clone()));
    assert_eq!(list_pids(&*store, &in_partition.queue(named)).await, std::slice::from_ref(&pid));
    assert!(
        !list_pids(
            &*store,
            &ProcessFilter::default().queue(QueueClass::Interactive).limit(usize::MAX)
        )
        .await
        .contains(&pid),
        "the old queue's entry is gone"
    );
    cleanup(&*store, &[pid]).await;
}

/// An unknown `pid` reads as `None`; `set_wake` and `set_queue` fail with
/// `kernel.not_found` naming it and create no row; `delete` answers `false`.
pub async fn unknown_pid(harness: &dyn ProcessStoreHarness) {
    let store = harness.open().await;
    let pid = unique("missing");
    assert_eq!(store.get(&pid).await.expect("get"), None);

    let not_found = |result: crate::Result<()>, call: &str| {
        let error = result.expect_err(call);
        assert_eq!(error.code(), NOT_FOUND_CODE, "{call}: {error}");
        assert!(
            matches!(&error, KernelError::NotFound { pid: named } if *named == pid),
            "{call}: {error}"
        );
    };
    not_found(
        store.set_wake(&pid, Some(WakeSet::any_of([Wake::Manual]))).await,
        "set_wake of an unknown pid",
    );
    not_found(store.set_wake(&pid, None).await, "clearing the wake set of an unknown pid");
    not_found(store.set_queue(&pid, QueueClass::Interactive).await, "set_queue of an unknown pid");
    assert!(!store.delete(&pid).await.expect("delete"), "nothing to delete");
    assert_eq!(store.get(&pid).await.expect("get"), None, "a failed update creates no row");
    assert!(!list_pids(&*store, &ProcessFilter::default()).await.contains(&pid));
}

/// `delete` removes the row and its `list` entries and answers `true`, then `false`; the
/// pid can be put again.
pub async fn delete(harness: &dyn ProcessStoreHarness) {
    let store = harness.open().await;
    let partition = partition();
    let pid = unique("pid");
    let meta = full_meta(&pid, &partition);
    let parent = meta.parent.clone().expect("parent");
    store.put(meta.clone()).await.expect("put");

    assert!(store.delete(&pid).await.expect("delete"), "the row existed");
    assert_eq!(store.get(&pid).await.expect("get"), None);
    assert!(!store.delete(&pid).await.expect("second delete"), "the row is gone");
    let empty: Vec<String> = Vec::new();
    assert_eq!(
        list_pids(&*store, &ProcessFilter::default().partition(partition.clone())).await,
        empty
    );
    assert_eq!(list_pids(&*store, &ProcessFilter::default().parent(parent)).await, empty);
    assert!(!list_pids(&*store, &ProcessFilter::default()).await.contains(&pid));

    store.put(meta.clone()).await.expect("put again");
    assert_eq!(store.get(&pid).await.expect("get"), Some(meta));
    cleanup(&*store, &[pid]).await;
}

/// Rows, with their updates, read back through a new handle as a restarted kernel opens
/// one. Skipped for a backend that is not durable.
pub async fn records_survive_a_restart(harness: &dyn ProcessStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let partition = partition();
    let rows: Vec<ProcessMeta> = (0..3).map(|_| full_meta(&unique("pid"), &partition)).collect();
    let mut expected = rows.clone();
    {
        let before = harness.open().await;
        for meta in &rows {
            before.put(meta.clone()).await.expect("put");
        }
        before.set_wake(&rows[0].pid, None).await.expect("set wake");
        expected[0].wake = None;
        before.set_queue(&rows[1].pid, QueueClass::Maintenance).await.expect("set queue");
        expected[1].queue = QueueClass::Maintenance;
    }
    expected.sort_by(|a, b| a.pid.cmp(&b.pid));

    let after = harness.open().await;
    for meta in &expected {
        assert_eq!(after.get(&meta.pid).await.expect("get").as_ref(), Some(meta));
    }
    assert_eq!(
        after.list(&ProcessFilter::default().partition(partition)).await.expect("list"),
        expected
    );
    let pids: Vec<String> = expected.iter().map(|meta| meta.pid.clone()).collect();
    cleanup(&*after, &pids).await;
}

/// Sixteen writers over two handles put and update distinct pids at once and lose no row;
/// two writers updating different fields of one row lose neither update.
pub async fn concurrent_writers(harness: &dyn ProcessStoreHarness) {
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { first.clone() };
    let partition = partition();
    let prefix = unique("pid");
    let wake = WakeSet::any_of([Wake::Manual]);

    let writers: Vec<_> = (0..16)
        .map(|writer| {
            let store = if writer % 2 == 0 { first.clone() } else { second.clone() };
            let (partition, prefix, wake) = (partition.clone(), prefix.clone(), wake.clone());
            tokio::spawn(async move {
                for row in 0..8 {
                    let pid = format!("{prefix}/{writer:02}-{row}");
                    let meta = ProcessMeta::new(&pid, QueueClass::Background, ProcessKind::Goal)
                        .with_partition(partition.clone());
                    store.put(meta).await.expect("put");
                    store.set_wake(&pid, Some(wake.clone())).await.expect("set wake");
                    if row % 2 == 0 {
                        store.set_queue(&pid, QueueClass::Interactive).await.expect("set queue");
                    }
                }
            })
        })
        .collect();
    for writer in writers {
        writer.await.expect("writer completes");
    }
    let rows =
        first.list(&ProcessFilter::default().partition(partition.clone())).await.expect("list");
    assert_eq!(rows.len(), 128, "no row is lost");
    for meta in &rows {
        let row: u32 = meta.pid.rsplit('-').next().and_then(|row| row.parse().ok()).expect("row");
        let queue =
            if row.is_multiple_of(2) { QueueClass::Interactive } else { QueueClass::Background };
        let expected = ProcessMeta::new(&meta.pid, queue, ProcessKind::Goal)
            .with_partition(partition.clone())
            .with_wake(wake.clone());
        assert_eq!(meta, &expected);
    }

    // Different fields of one row, from two handles at once.
    let pid = unique("shared");
    let meta = ProcessMeta::new(&pid, QueueClass::Background, ProcessKind::Goal)
        .with_partition(partition.clone());
    first.put(meta.clone()).await.expect("put");
    let last_wake = WakeSet::any_of([Wake::Webhook { hook: unique("hook") }]);
    let wakes = {
        let (store, pid, last_wake) = (first.clone(), pid.clone(), last_wake.clone());
        tokio::spawn(async move {
            for round in 0..20 {
                let wake = WakeSet::any_of([Wake::At(instant(round))]);
                store.set_wake(&pid, Some(wake)).await.expect("set wake");
            }
            store.set_wake(&pid, Some(last_wake)).await.expect("set wake");
        })
    };
    let queues = {
        let (store, pid) = (second.clone(), pid.clone());
        tokio::spawn(async move {
            for round in 0..20 {
                let queue =
                    if round % 2 == 0 { QueueClass::Maintenance } else { QueueClass::Background };
                store.set_queue(&pid, queue).await.expect("set queue");
            }
            store.set_queue(&pid, QueueClass::Interactive).await.expect("set queue");
        })
    };
    wakes.await.expect("wake writer completes");
    queues.await.expect("queue writer completes");
    let mut expected = meta;
    expected.wake = Some(last_wake);
    expected.queue = QueueClass::Interactive;
    assert_eq!(first.get(&pid).await.expect("get"), Some(expected));

    let mut pids: Vec<String> = rows.into_iter().map(|meta| meta.pid).collect();
    pids.push(pid);
    cleanup(&*first, &pids).await;
}

/// Four writers over two handles each run 150 rounds of put, set_wake and set_queue, and
/// delete every third row, while two readers list and get without pause. No call fails,
/// every read decodes to a state some write produced, and the final table holds exactly
/// the rows not deleted, each with its last update.
pub async fn sustained_writes(harness: &dyn ProcessStoreHarness) {
    const WRITERS: usize = 4;
    const ROUNDS: usize = 150;
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { first.clone() };
    let partition = partition();
    let prefix = unique("pid");
    let wake = WakeSet::any_of([Wake::Queue { source: unique("source") }]);
    let writing = Arc::new(AtomicBool::new(true));
    let reads = Arc::new(AtomicUsize::new(0));

    let readers: Vec<_> = (0..2)
        .map(|reader| {
            let store = if reader == 0 { first.clone() } else { second.clone() };
            let (partition, prefix, wake) = (partition.clone(), prefix.clone(), wake.clone());
            let (writing, reads) = (writing.clone(), reads.clone());
            tokio::spawn(async move {
                let filter = ProcessFilter::default().partition(partition.clone());
                while writing.load(Ordering::Acquire) {
                    for meta in store.list(&filter).await.expect("list while writing") {
                        assert!(meta.pid.starts_with(&prefix), "{}", meta.pid);
                        assert!(
                            matches!(meta.queue, QueueClass::Background | QueueClass::Interactive),
                            "{:?}",
                            meta.queue
                        );
                        assert!(meta.wake.is_none() || meta.wake.as_ref() == Some(&wake));
                    }
                    let probe = format!("{prefix}/{reader}-0");
                    store.get(&probe).await.expect("get while writing");
                    reads.fetch_add(1, Ordering::Relaxed);
                    tokio::task::yield_now().await;
                }
            })
        })
        .collect();

    let writers: Vec<_> = (0..WRITERS)
        .map(|writer| {
            let store = if writer % 2 == 0 { first.clone() } else { second.clone() };
            let (partition, prefix, wake) = (partition.clone(), prefix.clone(), wake.clone());
            tokio::spawn(async move {
                for round in 0..ROUNDS {
                    let pid = format!("{prefix}/{writer}-{round}");
                    let meta = ProcessMeta::new(&pid, QueueClass::Background, ProcessKind::Goal)
                        .with_partition(partition.clone());
                    store.put(meta).await.expect("put");
                    store.set_wake(&pid, Some(wake.clone())).await.expect("set wake");
                    store.set_queue(&pid, QueueClass::Interactive).await.expect("set queue");
                    if round % 3 == 0 {
                        assert!(store.delete(&pid).await.expect("delete"), "{pid}");
                    }
                }
            })
        })
        .collect();
    for writer in writers {
        writer.await.expect("writer completes");
    }
    writing.store(false, Ordering::Release);
    for reader in readers {
        reader.await.expect("reader completes");
    }
    assert!(reads.load(Ordering::Relaxed) > 0, "the readers read while the writers wrote");

    let rows =
        first.list(&ProcessFilter::default().partition(partition.clone())).await.expect("list");
    let mut expected: BTreeMap<String, ProcessMeta> = BTreeMap::new();
    for writer in 0..WRITERS {
        for round in (0..ROUNDS).filter(|round| round % 3 != 0) {
            let pid = format!("{prefix}/{writer}-{round}");
            let meta = ProcessMeta::new(&pid, QueueClass::Interactive, ProcessKind::Goal)
                .with_partition(partition.clone())
                .with_wake(wake.clone());
            expected.insert(pid, meta);
        }
    }
    assert_eq!(rows, expected.into_values().collect::<Vec<_>>());
    let pids: Vec<String> = rows.into_iter().map(|meta| meta.pid).collect();
    cleanup(&*first, &pids).await;
}
