//! Conformance cases for [`AgendaStore`] backends (spec 3.2 §7.5, §11, §13 Q8).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`put_get_round_trip`] | An item reads back exactly as put, every field and every [`AgendaState`] and [`AgendaOrigin`] variant included; ids with `:`, `/` and non-ASCII characters survive |
//! | [`put_replaces`] | A second put with the same id replaces the item, and `list` finds it only under its new partition and state |
//! | [`list_filters_and_orders`] | `list` applies partition, states and limit, and orders by `created_at`, then id in byte order |
//! | [`transition_compares_the_state`] | `transition` writes only when the stored state is of the given kind; an unknown id answers `false` and creates nothing |
//! | [`concurrent_transitions_move_once`] | Concurrent transitions of one item from the same state, from two handles, succeed exactly once |
//! | [`delete`] | `delete` removes the item and its `list` entry and answers `true`, then `false` |
//! | [`records_survive_a_restart`] | Items read back through a new handle (durable backends only) |
//! | [`concurrent_writers`] | Concurrent puts from two handles over distinct ids lose no item |
//!
//! Every case writes under fresh ids in fresh partitions and deletes its items at the end,
//! so a harness may point every case at one shared database.

use std::sync::Arc;
use std::time::Duration;

use adk_core::RunBudget;
use adk_core::principal::SubjectRef;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::agenda::{
    AgendaFilter, AgendaItem, AgendaOrigin, AgendaState, AgendaStateKind, AgendaStore, PlanRef,
};
use crate::partition::PartitionId;
use crate::wake::{ExpiryAction, Wake, WakeSet};

/// Opens an [`AgendaStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::conformance::AgendaStoreHarness;
/// use adk_kernel::{AgendaStore, InMemoryAgendaStore};
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl AgendaStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn AgendaStore> {
///         Arc::new(InMemoryAgendaStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait AgendaStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn AgendaStore>;

    /// Whether data written through one handle is visible through the next. `false` skips
    /// the restart case and runs the concurrency cases on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

/// A fresh identifier with the characters a backend must escape: `:`, `/`, non-ASCII.
fn unique(prefix: &str) -> String {
    format!("conformance:{prefix}/ü-日本/{}", uuid::Uuid::new_v4())
}

fn partition() -> PartitionId {
    PartitionId::new(unique("partition"))
}

/// An instant with sub-microsecond precision, so a backend that rounds is caught.
fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, 123_456_789).single().expect("valid instant")
}

fn item(id: &str, partition: &PartitionId, created: i64) -> AgendaItem {
    AgendaItem::new(
        id,
        SubjectRef::human("alice@example.com"),
        "Re-run the tests",
        "The dependency bump lands tonight",
        AgendaOrigin::User,
        instant(created),
    )
    .with_partition(partition.clone())
}

/// An item with every field set.
fn full_item(
    id: &str,
    partition: &PartitionId,
    state: AgendaState,
    origin: AgendaOrigin,
) -> AgendaItem {
    AgendaItem::new(
        id,
        SubjectRef::service("scheduler:ü"),
        "Tidy \"the\" lockfile — 日本",
        "line one\nline two",
        origin,
        instant(10),
    )
    .with_partition(partition.clone())
    .with_state(state)
    .with_plan(PlanRef::new(
        "sha256:ab12",
        vec![serde_json::json!({"id": "1", "title": "Run", "instruction": "cargo test", "dependsOn": []})],
    ))
    .with_budget(
        RunBudget::new()
            .max_model_calls(12)
            .max_total_tokens(40_000)
            .max_cost_micro_usd(250_000)
            .max_wall_time(Duration::from_millis(90_500))
            .max_tool_calls(30)
            .allow_unpriced_models(),
    )
    .with_expiry(instant(86_400))
    .with_when(Wake::FileChange {
        root: "/srv/ü repo".into(),
        glob: "Cargo.lock".into(),
        debounce: Duration::from_millis(500),
    })
}

fn every_state() -> Vec<AgendaState> {
    vec![
        AgendaState::Proposed,
        AgendaState::Approved,
        AgendaState::Scheduled {
            wake: WakeSet::any_of([Wake::At(instant(60)), Wake::Manual])
                .with_expiry(instant(3_600), ExpiryAction::Run),
        },
        AgendaState::Running { pid: unique("run") },
        AgendaState::Done,
        AgendaState::Rejected,
        AgendaState::Expired,
    ]
}

fn every_origin() -> Vec<AgendaOrigin> {
    vec![
        AgendaOrigin::User,
        AgendaOrigin::Agent { pid: unique("run"), confidence: 0.75 },
        AgendaOrigin::Consolidation,
        AgendaOrigin::Observation { id: unique("observation") },
    ]
}

async fn list_ids(store: &dyn AgendaStore, filter: &AgendaFilter) -> Vec<String> {
    store.list(filter).await.expect("list").into_iter().map(|item| item.id).collect()
}

/// An item reads back exactly as put, every field and variant included.
pub async fn put_get_round_trip(harness: &dyn AgendaStoreHarness) {
    let store = harness.open().await;
    let partition = partition();
    let mut ids = Vec::new();
    for (index, state) in every_state().into_iter().enumerate() {
        let origin = every_origin()[index % 4].clone();
        let id = unique("round-trip");
        let item = full_item(&id, &partition, state, origin);
        store.put(item.clone()).await.expect("put");
        assert_eq!(store.get(&id).await.expect("get"), Some(item), "item reads back as put");
        ids.push(id);
    }
    let minimal = item(&unique("minimal"), &partition, 0);
    store.put(minimal.clone()).await.expect("put");
    assert_eq!(store.get(&minimal.id).await.expect("get"), Some(minimal.clone()));
    ids.push(minimal.id);
    for id in ids {
        assert!(store.delete(&id).await.expect("delete"));
    }
}

/// A second put with the same id replaces the item.
pub async fn put_replaces(harness: &dyn AgendaStoreHarness) {
    let store = harness.open().await;
    let (first, second) = (partition(), partition());
    let id = unique("replace");
    store.put(item(&id, &first, 0)).await.expect("put");
    let replaced = item(&id, &second, 5).with_state(AgendaState::Approved);
    store.put(replaced.clone()).await.expect("put again");
    assert_eq!(store.get(&id).await.expect("get"), Some(replaced));
    assert!(
        list_ids(store.as_ref(), &AgendaFilter::default().partition(first.clone()))
            .await
            .is_empty()
    );
    assert_eq!(
        list_ids(store.as_ref(), &AgendaFilter::default().partition(second.clone())).await,
        std::slice::from_ref(&id)
    );
    assert!(
        list_ids(
            store.as_ref(),
            &AgendaFilter::default().partition(second.clone()).states([AgendaStateKind::Proposed])
        )
        .await
        .is_empty(),
        "the old state is no longer listed"
    );
    assert!(store.delete(&id).await.expect("delete"));
}

/// `list` filters and orders by `created_at`, then id.
pub async fn list_filters_and_orders(harness: &dyn AgendaStoreHarness) {
    let store = harness.open().await;
    let (work, home) = (partition(), partition());
    let base = unique("order");
    let ids: Vec<String> =
        ["c", "a", "b", "d"].iter().map(|suffix| format!("{base}-{suffix}")).collect();
    // `-c` is oldest; `-a` and `-b` tie on `created_at` and order by id; `-d` is newest.
    store.put(item(&ids[0], &work, 1)).await.expect("put");
    store.put(item(&ids[1], &work, 2).with_state(AgendaState::Approved)).await.expect("put");
    store.put(item(&ids[2], &work, 2)).await.expect("put");
    store.put(item(&ids[3], &work, 3).with_state(AgendaState::Done)).await.expect("put");
    let other = unique("other");
    store.put(item(&other, &home, 0)).await.expect("put");

    let all = list_ids(store.as_ref(), &AgendaFilter::default().partition(work.clone())).await;
    assert_eq!(all, [ids[0].clone(), ids[1].clone(), ids[2].clone(), ids[3].clone()]);
    let proposed = list_ids(
        store.as_ref(),
        &AgendaFilter::default().partition(work.clone()).states([AgendaStateKind::Proposed]),
    )
    .await;
    assert_eq!(proposed, [ids[0].clone(), ids[2].clone()]);
    let open =
        list_ids(store.as_ref(), &AgendaFilter::default().partition(work.clone()).open()).await;
    assert_eq!(open, [ids[0].clone(), ids[1].clone(), ids[2].clone()]);
    let limited =
        list_ids(store.as_ref(), &AgendaFilter::default().partition(work.clone()).limit(2)).await;
    assert_eq!(limited, [ids[0].clone(), ids[1].clone()]);
    assert_eq!(
        list_ids(store.as_ref(), &AgendaFilter::default().partition(home.clone())).await,
        std::slice::from_ref(&other)
    );
    let everywhere = list_ids(store.as_ref(), &AgendaFilter::default()).await;
    assert!(
        ids.iter().chain([&other]).all(|id| everywhere.contains(id)),
        "an empty filter lists every item"
    );

    for id in ids.iter().chain([&other]) {
        assert!(store.delete(id).await.expect("delete"));
    }
}

/// `transition` writes only from the given state kind.
pub async fn transition_compares_the_state(harness: &dyn AgendaStoreHarness) {
    let store = harness.open().await;
    let partition = partition();
    let id = unique("transition");
    let proposed = item(&id, &partition, 0);
    store.put(proposed.clone()).await.expect("put");

    let approved = proposed.clone().with_state(AgendaState::Approved);
    assert!(
        store.transition(approved.clone(), AgendaStateKind::Proposed).await.expect("transition")
    );
    assert_eq!(store.get(&id).await.expect("get"), Some(approved.clone()));

    let rejected = proposed.clone().with_state(AgendaState::Rejected);
    assert!(
        !store.transition(rejected, AgendaStateKind::Proposed).await.expect("transition"),
        "the item is no longer proposed"
    );
    assert_eq!(
        store.get(&id).await.expect("get"),
        Some(approved.clone()),
        "a refused transition writes nothing"
    );

    let scheduled =
        approved.with_state(AgendaState::Scheduled { wake: WakeSet::any_of([Wake::Manual]) });
    assert!(
        store.transition(scheduled.clone(), AgendaStateKind::Approved).await.expect("transition")
    );
    let running = scheduled.with_state(AgendaState::Running { pid: unique("run") });
    assert!(
        store.transition(running.clone(), AgendaStateKind::Scheduled).await.expect("transition")
    );
    assert_eq!(
        list_ids(
            store.as_ref(),
            &AgendaFilter::default()
                .partition(partition.clone())
                .states([AgendaStateKind::Running])
        )
        .await,
        std::slice::from_ref(&id)
    );

    let unknown = item(&unique("unknown"), &partition, 0).with_state(AgendaState::Approved);
    assert!(
        !store.transition(unknown.clone(), AgendaStateKind::Proposed).await.expect("transition")
    );
    assert_eq!(store.get(&unknown.id).await.expect("get"), None, "a transition creates nothing");
    assert!(store.delete(&id).await.expect("delete"));
}

/// Concurrent transitions of one item from one state succeed exactly once.
pub async fn concurrent_transitions_move_once(harness: &dyn AgendaStoreHarness) {
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { Arc::clone(&first) };
    let partition = partition();
    for round in 0..5 {
        let id = unique(&format!("race-{round}"));
        let proposed = item(&id, &partition, round);
        first.put(proposed.clone()).await.expect("put");
        let mut tasks = Vec::new();
        for attempt in 0..8 {
            let store = if attempt % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
            let next =
                proposed.clone().with_state(AgendaState::Running { pid: format!("run-{attempt}") });
            tasks.push(tokio::spawn(async move {
                store.transition(next, AgendaStateKind::Proposed).await.expect("transition")
            }));
        }
        let mut won = 0;
        for task in tasks {
            if task.await.expect("task") {
                won += 1;
            }
        }
        assert_eq!(won, 1, "exactly one transition from `proposed` succeeds");
        assert_eq!(
            first.get(&id).await.expect("get").map(|item| item.state.kind()),
            Some(AgendaStateKind::Running)
        );
        assert!(first.delete(&id).await.expect("delete"));
    }
}

/// `delete` removes the item and its list entry.
pub async fn delete(harness: &dyn AgendaStoreHarness) {
    let store = harness.open().await;
    let partition = partition();
    let id = unique("delete");
    store.put(item(&id, &partition, 0)).await.expect("put");
    assert!(store.delete(&id).await.expect("delete"));
    assert_eq!(store.get(&id).await.expect("get"), None);
    assert!(
        list_ids(store.as_ref(), &AgendaFilter::default().partition(partition)).await.is_empty()
    );
    assert!(!store.delete(&id).await.expect("delete again"));
}

/// Items read back through a new handle.
pub async fn records_survive_a_restart(harness: &dyn AgendaStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let partition = partition();
    let id = unique("restart");
    let stored = full_item(&id, &partition, AgendaState::Approved, AgendaOrigin::Consolidation);
    harness.open().await.put(stored.clone()).await.expect("put");
    let reopened = harness.open().await;
    assert_eq!(reopened.get(&id).await.expect("get"), Some(stored));
    assert_eq!(
        list_ids(reopened.as_ref(), &AgendaFilter::default().partition(partition)).await,
        std::slice::from_ref(&id)
    );
    assert!(reopened.delete(&id).await.expect("delete"));
}

/// Concurrent puts from two handles lose no item.
pub async fn concurrent_writers(harness: &dyn AgendaStoreHarness) {
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { Arc::clone(&first) };
    let partition = partition();
    let mut tasks = Vec::new();
    for writer in 0..2 {
        let store = if writer == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
        let partition = partition.clone();
        tasks.push(tokio::spawn(async move {
            let mut ids = Vec::new();
            for index in 0..25 {
                let id = unique(&format!("writer-{writer}-{index}"));
                store.put(item(&id, &partition, index)).await.expect("put");
                ids.push(id);
            }
            ids
        }));
    }
    let mut written = Vec::new();
    for task in tasks {
        written.extend(task.await.expect("task"));
    }
    let listed = list_ids(first.as_ref(), &AgendaFilter::default().partition(partition)).await;
    assert_eq!(listed.len(), written.len(), "no item is lost");
    for id in written {
        assert!(listed.contains(&id));
        assert!(first.delete(&id).await.expect("delete"));
    }
}
