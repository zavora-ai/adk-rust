//! Conformance cases for [`LedgerStore`] backends (spec 3.2 §7.10, §11, §13 Q8; features
//! `conformance` and `proc`).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`append_round_trips`] | A row reads back exactly as appended — every field, a non-ASCII partition, offsets east and west of UTC, millisecond instants — under a sequence number greater than every earlier one |
//! | [`list_filters_and_pages`] | `list` applies partition, kinds, agent, the `[from, to)` window, `after` and `limit`, in sequence order; paging with `after` returns every row once |
//! | [`records_survive_a_restart`] | Rows, observations and pass states read back through a new handle (durable backends only) |
//! | [`concurrent_appends`] | Appends from two handles at once all land, each under its own sequence number |
//! | [`sustained_appends`] | A sustained stream of appends from two handles, read concurrently, stays consistent: a reader never sees a row twice or out of order |
//! | [`observations_replace_and_order`] | `put_observation` replaces by id; `observations` filters by partition and day and orders newest day first, then by id |
//! | [`pass_states_round_trip`] | `put_pass_state` replaces a partition's state; `pass_state` reads it, `None` before the first; `pass_states` lists them by partition |
//!
//! Every case writes under fresh partitions, so a harness may point every case at one
//! shared database. The ledger is append-only, so the cases leave their rows.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone as _, Utc};
use serde_json::{Map, json};

use crate::partition::PartitionId;
use crate::proc::{
    Direction, LedgerEntry, LedgerQuery, LedgerStore, Observation, ObservationQuery,
    ObservedDimension, PassState, Phrasing,
};

/// Opens a [`LedgerStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::conformance::LedgerStoreHarness;
/// use adk_kernel::proc::{InMemoryLedgerStore, LedgerStore};
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl LedgerStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn LedgerStore> {
///         Arc::new(InMemoryLedgerStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait LedgerStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn LedgerStore>;

    /// Whether data written through one handle is visible through the next. `false` skips
    /// the restart case and runs the concurrency cases on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

/// A fresh partition with the characters a backend must keep: `:`, `/`, non-ASCII.
fn fresh_partition() -> PartitionId {
    PartitionId::new(format!("conformance:ledger/ü-日本/{}", uuid::Uuid::new_v4().simple()))
}

/// An instant `seconds` from a fixed base, to the millisecond, in `offset`.
fn at(seconds: i64, millis: u32, offset: FixedOffset) -> DateTime<FixedOffset> {
    let utc = Utc.timestamp_opt(1_800_000_000 + seconds, millis * 1_000_000).single().unwrap();
    utc.with_timezone(&offset)
}

fn east() -> FixedOffset {
    FixedOffset::east_opt(5 * 3600 + 45 * 60).unwrap()
}

fn west() -> FixedOffset {
    FixedOffset::west_opt(5 * 3600).unwrap()
}

/// A row with every field set.
fn full(partition: &PartitionId, seconds: i64, kind: &str) -> LedgerEntry {
    let mut meta = Map::new();
    meta.insert("attempt".into(), json!(2));
    meta.insert("ratio".into(), json!(0.25));
    meta.insert("retried".into(), json!(true));
    LedgerEntry {
        seq: 0,
        ts: at(seconds, 123, east()),
        partition: partition.clone(),
        agent: "urn:adk:agent:adk-rust/harness".into(),
        kind: kind.into(),
        effect: Some("read_only".into()),
        duration_ms: Some(1_500),
        subject_hash: Some(format!("hmac-sha256:{}", "ab".repeat(32))),
        meta,
        trace_id: Some("4bf92f3577b34da6a3ce929d0e0e4736".into()),
    }
}

/// A row with only the required fields.
fn bare(partition: &PartitionId, seconds: i64, kind: &str, agent: &str) -> LedgerEntry {
    LedgerEntry {
        seq: 0,
        ts: at(seconds, 0, west()),
        partition: partition.clone(),
        agent: agent.into(),
        kind: kind.into(),
        effect: None,
        duration_ms: None,
        subject_hash: None,
        meta: Map::new(),
        trace_id: None,
    }
}

async fn rows_of(store: &dyn LedgerStore, partition: &PartitionId) -> Vec<LedgerEntry> {
    store.list(&LedgerQuery::default().partition(partition.clone())).await.expect("list")
}

/// A row reads back exactly as appended, under an increasing sequence number.
pub async fn append_round_trips(harness: &dyn LedgerStoreHarness) {
    let store = harness.open().await;
    let partition = fresh_partition();
    let mut written = Vec::new();
    for (n, entry) in [
        full(&partition, 0, "tool_call"),
        bare(&partition, 1, "wake", "kernel"),
        full(&partition, 2, "turn"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut expected = entry.clone();
        let stored = store.append(entry).await.expect("append");
        assert!(stored.seq > 0, "row {n} has a sequence number");
        if let Some(previous) = written.last() {
            let previous: &LedgerEntry = previous;
            assert!(stored.seq > previous.seq, "sequence numbers increase");
        }
        expected.seq = stored.seq;
        assert_eq!(stored, expected, "append answers the row as stored");
        written.push(stored);
    }
    let read = rows_of(store.as_ref(), &partition).await;
    assert_eq!(read, written, "every field reads back, offsets and milliseconds included");
    assert_eq!(read[0].ts.offset(), &east());
    assert_eq!(read[1].ts.offset(), &west());
}

/// `list` applies every filter in sequence order, and paging returns each row once.
pub async fn list_filters_and_pages(harness: &dyn LedgerStoreHarness) {
    let store = harness.open().await;
    let partition = fresh_partition();
    let other = fresh_partition();
    let mut all = Vec::new();
    for n in 0..10_i64 {
        let kind = if n % 2 == 0 { "tool_call" } else { "intent" };
        let agent = if n % 3 == 0 { "kernel" } else { "harness" };
        all.push(store.append(bare(&partition, n * 60, kind, agent)).await.expect("append"));
        store.append(bare(&other, n * 60, kind, agent)).await.expect("append");
    }
    let seqs = |rows: &[LedgerEntry]| rows.iter().map(|row| row.seq).collect::<Vec<_>>();
    let base = LedgerQuery::default().partition(partition.clone());
    assert_eq!(seqs(&store.list(&base).await.unwrap()), seqs(&all), "partition, in order");

    let tools = store.list(&base.clone().kind("tool_call")).await.unwrap();
    assert_eq!(tools.len(), 5);
    assert!(tools.iter().all(|row| row.kind == "tool_call"));
    let both = store.list(&base.clone().kind("tool_call").kind("intent")).await.unwrap();
    assert_eq!(seqs(&both), seqs(&all), "several kinds");
    let kernel = store.list(&base.clone().agent("kernel")).await.unwrap();
    assert_eq!(kernel.len(), 4, "rows 0, 3, 6 and 9");

    let from = at(2 * 60, 0, west()).with_timezone(&Utc);
    let to = at(5 * 60, 0, west()).with_timezone(&Utc);
    let window = store.list(&base.clone().from(from).to(to)).await.unwrap();
    assert_eq!(seqs(&window), seqs(&all[2..5]), "from is inclusive, to exclusive");

    let after = store.list(&base.clone().after(all[6].seq)).await.unwrap();
    assert_eq!(seqs(&after), seqs(&all[7..]));
    let limited = store.list(&base.clone().limit(3)).await.unwrap();
    assert_eq!(seqs(&limited), seqs(&all[..3]));

    let mut paged = Vec::new();
    let mut query = base.clone().limit(4);
    loop {
        let page = store.list(&query).await.unwrap();
        let Some(last) = page.last().map(|row| row.seq) else { break };
        paged.extend(page);
        query = query.after(last);
    }
    assert_eq!(seqs(&paged), seqs(&all), "paging returns every row once");
}

/// Rows, observations and pass states read back through a new handle.
pub async fn records_survive_a_restart(harness: &dyn LedgerStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let partition = fresh_partition();
    let (row, observation, state) = {
        let store = harness.open().await;
        let row = store.append(full(&partition, 0, "tool_call")).await.unwrap();
        let observation = observation(&partition, 20, "a");
        store.put_observation(observation.clone()).await.unwrap();
        let state = pass_state(&partition, 20);
        store.put_pass_state(state.clone()).await.unwrap();
        (row, observation, state)
    };
    let store = harness.open().await;
    assert_eq!(rows_of(store.as_ref(), &partition).await, [row]);
    let query = ObservationQuery::default().partition(partition.clone());
    assert_eq!(store.observations(&query).await.unwrap(), [observation]);
    assert_eq!(store.pass_state(&partition).await.unwrap(), Some(state));
}

/// The second handle of a concurrency case: a new one on a durable backend.
async fn second(
    harness: &dyn LedgerStoreHarness,
    first: &Arc<dyn LedgerStore>,
) -> Arc<dyn LedgerStore> {
    if harness.is_durable() { harness.open().await } else { Arc::clone(first) }
}

/// Appends from two handles at once all land under their own sequence numbers.
pub async fn concurrent_appends(harness: &dyn LedgerStoreHarness) {
    let first = harness.open().await;
    let other = second(harness, &first).await;
    let partition = fresh_partition();
    let mut tasks = Vec::new();
    for (writer, store) in [(0_i64, first.clone()), (1, other)] {
        let partition = partition.clone();
        tasks.push(tokio::spawn(async move {
            for n in 0..25_i64 {
                store
                    .append(bare(&partition, writer * 1000 + n, "tool_call", "harness"))
                    .await
                    .unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let rows = rows_of(first.as_ref(), &partition).await;
    assert_eq!(rows.len(), 50, "no append is lost");
    assert!(rows.windows(2).all(|pair| pair[0].seq < pair[1].seq), "unique and in order");
}

/// A sustained stream of appends, read concurrently, never shows a row twice or out of
/// order.
pub async fn sustained_appends(harness: &dyn LedgerStoreHarness) {
    let first = harness.open().await;
    let other = second(harness, &first).await;
    let partition = fresh_partition();
    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let store = first.clone();
        let partition = partition.clone();
        let done = done.clone();
        tokio::spawn(async move {
            let mut cursor = 0;
            let mut seen = 0_usize;
            loop {
                let finished = done.load(Ordering::SeqCst);
                let page = store
                    .list(
                        &LedgerQuery::default()
                            .partition(partition.clone())
                            .after(cursor)
                            .limit(16),
                    )
                    .await
                    .unwrap();
                for row in &page {
                    assert!(row.seq > cursor, "rows arrive once and in order");
                    cursor = row.seq;
                    seen += 1;
                }
                if finished && page.is_empty() {
                    return seen;
                }
                tokio::task::yield_now().await;
            }
        })
    };
    let mut writers = Vec::new();
    for (writer, store) in [(0_i64, first.clone()), (1, other)] {
        let partition = partition.clone();
        writers.push(tokio::spawn(async move {
            for n in 0..100_i64 {
                store.append(full(&partition, writer * 1000 + n, "tool_call")).await.unwrap();
            }
        }));
    }
    for writer in writers {
        writer.await.unwrap();
    }
    done.store(true, Ordering::SeqCst);
    let seen = reader.await.unwrap();
    assert_eq!(rows_of(first.as_ref(), &partition).await.len(), 200);
    assert!(seen <= 200);
}

fn day(n: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, n).unwrap()
}

fn observation(partition: &PartitionId, as_of: u32, suffix: &str) -> Observation {
    Observation {
        id: format!("{}-{suffix}", Observation::id_for(partition, day(as_of))),
        partition: partition.clone(),
        as_of: day(as_of),
        dimensions: vec![ObservedDimension::new(
            "activity.last_minute",
            Direction::Higher,
            day(as_of.saturating_sub(9).max(1)),
            10,
        )],
        windows: vec![7, 14],
        question: "Your routine has changed over the last two weeks. Would you like to review \
                   what changed?"
            .into(),
        phrasing: Phrasing::Model { model: "scripted".into() },
        created_at: Utc.timestamp_opt(1_800_000_000, 123_000_000).single().unwrap(),
    }
}

fn pass_state(partition: &PartitionId, last_day: u32) -> PassState {
    let mut state = PassState::new(
        partition.clone(),
        day(last_day),
        Utc.timestamp_opt(1_800_000_000, 0).single().unwrap(),
    );
    state.drift =
        json!({ "deviatingSince": { "activity.last_minute": "2026-10-12" }, "lastObserved": {} });
    state.baselines = json!({ "partition": partition, "dimensions": [] });
    state
}

/// `put_observation` replaces by id; `observations` filters and orders.
pub async fn observations_replace_and_order(harness: &dyn LedgerStoreHarness) {
    let store = harness.open().await;
    let partition = fresh_partition();
    let other = fresh_partition();
    let early = observation(&partition, 12, "b");
    let late_b = observation(&partition, 20, "b");
    let late_a = observation(&partition, 20, "a");
    for observation in [&early, &late_b, &late_a, &observation(&other, 20, "a")] {
        store.put_observation(observation.clone()).await.unwrap();
    }
    let mut edited = early.clone();
    edited.question = "Your routine has changed over the last week. Would you like to review \
                       what changed?"
        .into();
    edited.phrasing = Phrasing::Template;
    store.put_observation(edited.clone()).await.unwrap();

    let query = ObservationQuery::default().partition(partition.clone());
    assert_eq!(
        store.observations(&query).await.unwrap(),
        [late_a.clone(), late_b.clone(), edited],
        "newest day first, then by id; a put replaces"
    );
    assert_eq!(
        store.observations(&query.clone().since(day(13))).await.unwrap(),
        [late_a.clone(), late_b]
    );
    assert_eq!(store.observations(&query.limit(1)).await.unwrap(), [late_a]);
}

/// Pass states replace per partition and list by partition.
pub async fn pass_states_round_trip(harness: &dyn LedgerStoreHarness) {
    let store = harness.open().await;
    let first = fresh_partition();
    let second = fresh_partition();
    assert_eq!(store.pass_state(&first).await.unwrap(), None);
    store.put_pass_state(pass_state(&first, 10)).await.unwrap();
    let replaced = pass_state(&first, 11);
    store.put_pass_state(replaced.clone()).await.unwrap();
    store.put_pass_state(pass_state(&second, 11)).await.unwrap();
    assert_eq!(store.pass_state(&first).await.unwrap(), Some(replaced.clone()));
    let ours: Vec<PassState> = store
        .pass_states()
        .await
        .unwrap()
        .into_iter()
        .filter(|state| state.partition == first || state.partition == second)
        .collect();
    let mut expected = vec![replaced, pass_state(&second, 11)];
    expected.sort_by(|a, b| a.partition.cmp(&b.partition));
    assert_eq!(ours, expected, "one state per partition, by partition");
    let all = store.pass_states().await.unwrap();
    assert!(all.windows(2).all(|pair| pair[0].partition <= pair[1].partition), "ordered");
}
