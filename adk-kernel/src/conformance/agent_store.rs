//! Conformance cases for [`AgentStore`] backends (spec 3.3 §5.7, §10 K7, S-I14).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`put_get_round_trip`] | A record reads back exactly as written, every state, origin and use included; names, lines and instructions with quotes, newlines and non-ASCII characters survive |
//! | [`put_if_version_compares`] | A write succeeds only from the stored version, or from none with 0; a refused write changes nothing and an unknown id with a version creates nothing |
//! | [`concurrent_puts_succeed_once`] | Concurrent writes of one record from the same version, from two handles, succeed exactly once; concurrent creates of one id succeed once |
//! | [`list_orders_by_id`] | `list` returns every record, tombstones included, ordered by id in byte order |
//! | [`delete_removes_only_suggestions`] | `delete` removes a `Proposed` record and answers `true`, then `false`; it leaves any other record and answers `false` |
//! | [`tombstones_stay`] | A `Removed` record stays readable and listed, and a write from an older version is refused |
//! | [`records_survive_a_restart`] | Records read back through a new handle (durable backends only) |
//! | [`concurrent_writers`] | Concurrent creates from two handles over distinct ids lose no record |
//!
//! Every case writes under fresh ids, so a harness may point every case at one shared
//! database. Stores never delete a record other than a suggestion, so the cases leave their
//! tombstones and active records behind.

use std::sync::Arc;

use adk_core::AgentId;
use adk_core::principal::SubjectRef;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::agents::{
    AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState, AgentStore, AgentUses,
    AvatarColor, FileAccess, ModelChoice, ShellAccess, WebAccess,
};
use crate::partition::PartitionMode;

/// Opens an [`AgentStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::agents::AgentStore;
/// use adk_kernel::conformance::AgentStoreHarness;
/// use adk_kernel::stores::InMemoryAgentStore;
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl AgentStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn AgentStore> {
///         Arc::new(InMemoryAgentStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait AgentStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn AgentStore>;

    /// Whether data written through one handle is visible through the next. `false` skips
    /// the restart case and runs the concurrency cases on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

/// A fresh agent id with `prefix`: the prefix, then 20 hex digits of a UUID.
fn unique(prefix: &str) -> String {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &uuid[..20])
}

/// An instant with sub-microsecond precision, so a backend that rounds is caught.
fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, 123_456_789).single().expect("valid instant")
}

fn record(id: &str, state: AgentRecordState) -> AgentRecord {
    AgentRecord::new(
        id,
        AgentDefinition::new("Finance", "default"),
        state,
        AgentOrigin::ByHand,
        SubjectRef::human("alice@example.com"),
        instant(0),
    )
}

/// A record with every field set, odd characters included.
fn full_record(id: &str, state: AgentRecordState, origin: AgentOrigin) -> AgentRecord {
    let definition = AgentDefinition::new("Bills \"& Taxes\" — 日本", "home ü")
        .with_color(AvatarColor::Teal)
        .with_description("Reads invoices; drafts \"payment\" reminders")
        .with_instructions("line one\nline two\t— never send.\n{\"json\": true}")
        .with_mode(PartitionMode::Plan)
        .with_model(ModelChoice::new("anthropic", "claude-sonnet-5-5"))
        .with_uses(
            AgentUses::default()
                .with_files(FileAccess::ReadWrite)
                .with_folder("Invoices/2026 ü")
                .with_shell(ShellAccess::Sandboxed)
                .with_web(WebAccess::listed(["example.com", "docs.rs"]))
                .with_pack("search")
                .with_driver("calendar"),
        )
        .with_daily_limit(250_000);
    let mut record = AgentRecord::new(
        id,
        definition,
        state,
        origin,
        SubjectRef::agent(&AgentId::new("urn:adk:agent:adk-rust/adk-rust").expect("agent id")),
        instant(10),
    )
    .with_approved_by(SubjectRef::human("alice@example.com"));
    record.updated_at = instant(20);
    record.version = 3;
    record
}

fn every_state() -> [AgentRecordState; 6] {
    [
        AgentRecordState::Proposed,
        AgentRecordState::Active,
        AgentRecordState::Paused,
        AgentRecordState::Removing,
        AgentRecordState::Removed,
        AgentRecordState::Refused,
    ]
}

fn every_origin() -> [AgentOrigin; 3] {
    [
        AgentOrigin::ByHand,
        AgentOrigin::Suggested {
            agent: AgentId::new("urn:adk:agent:adk-rust/adk-rust").expect("agent id"),
            session: "session ü/1".into(),
            run: Some("run-1".into()),
            unattended: true,
        },
        AgentOrigin::Suggested {
            agent: AgentId::new("urn:adk:agent:adk-rust/adk-rust%40work").expect("agent id"),
            session: "s".into(),
            run: None,
            unattended: false,
        },
    ]
}

/// The next version of `record`.
fn next(mut record: AgentRecord, state: AgentRecordState) -> AgentRecord {
    record.version += 1;
    record.state = state;
    record
}

async fn listed_ids(store: &dyn AgentStore, ids: &[String]) -> Vec<String> {
    store
        .list()
        .await
        .expect("list")
        .into_iter()
        .map(|record| record.id)
        .filter(|id| ids.contains(id))
        .collect()
}

/// A record reads back exactly as written.
pub async fn put_get_round_trip(harness: &dyn AgentStoreHarness) {
    let store = harness.open().await;
    for (index, state) in every_state().into_iter().enumerate() {
        let origin = every_origin()[index % 3].clone();
        let id = unique("round-trip");
        let written = full_record(&id, state, origin);
        assert!(store.put_if_version(written.clone(), 0).await.expect("put"));
        assert_eq!(
            store.get(&id).await.expect("get"),
            Some(written),
            "record reads back as written"
        );
    }
    let minimal = record(&unique("minimal"), AgentRecordState::Active);
    assert!(store.put_if_version(minimal.clone(), 0).await.expect("put"));
    assert_eq!(store.get(&minimal.id).await.expect("get"), Some(minimal));
    assert_eq!(store.get(&unique("unknown")).await.expect("get"), None);
}

/// Writes compare the stored version.
pub async fn put_if_version_compares(harness: &dyn AgentStoreHarness) {
    let store = harness.open().await;
    let id = unique("version");
    let first = record(&id, AgentRecordState::Active);
    assert!(store.put_if_version(first.clone(), 0).await.expect("create"));
    assert!(
        !store.put_if_version(first.clone(), 0).await.expect("create again"),
        "the id is taken"
    );

    let paused = next(first.clone(), AgentRecordState::Paused);
    assert!(!store.put_if_version(paused.clone(), 2).await.expect("put"), "not the stored version");
    assert_eq!(
        store.get(&id).await.expect("get"),
        Some(first.clone()),
        "a refused write changes nothing"
    );
    assert!(store.put_if_version(paused.clone(), 1).await.expect("put"));
    assert_eq!(store.get(&id).await.expect("get"), Some(paused.clone()));

    let stale = next(first, AgentRecordState::Removed);
    assert!(!store.put_if_version(stale, 1).await.expect("put"), "a write from an older version");
    assert_eq!(store.get(&id).await.expect("get").map(|r| r.version), Some(2));

    let unknown =
        next(record(&unique("unknown"), AgentRecordState::Active), AgentRecordState::Paused);
    assert!(!store.put_if_version(unknown.clone(), 1).await.expect("put"));
    assert_eq!(
        store.get(&unknown.id).await.expect("get"),
        None,
        "a versioned write creates nothing"
    );
}

/// Concurrent writes from one version succeed exactly once.
pub async fn concurrent_puts_succeed_once(harness: &dyn AgentStoreHarness) {
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { Arc::clone(&first) };
    for round in 0..4 {
        let id = unique(&format!("race-{round}"));
        let mut creates = Vec::new();
        for attempt in 0..6 {
            let store = if attempt % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
            let mut candidate = record(&id, AgentRecordState::Active);
            candidate.description = format!("create {attempt}");
            creates.push(tokio::spawn(async move {
                store.put_if_version(candidate, 0).await.expect("create")
            }));
        }
        let mut created = 0;
        for task in creates {
            created += usize::from(task.await.expect("task"));
        }
        assert_eq!(created, 1, "exactly one create of an id succeeds");

        let stored = first.get(&id).await.expect("get").expect("created");
        let mut changes = Vec::new();
        for attempt in 0..8 {
            let store = if attempt % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
            let mut candidate = next(stored.clone(), AgentRecordState::Paused);
            candidate.description = format!("change {attempt}");
            changes.push(tokio::spawn(async move {
                store.put_if_version(candidate, 1).await.expect("change")
            }));
        }
        let mut changed = 0;
        for task in changes {
            changed += usize::from(task.await.expect("task"));
        }
        assert_eq!(changed, 1, "exactly one change from version 1 succeeds");
        assert_eq!(first.get(&id).await.expect("get").map(|r| r.version), Some(2));
    }
}

/// `list` returns every record in id order, tombstones included.
pub async fn list_orders_by_id(harness: &dyn AgentStoreHarness) {
    let store = harness.open().await;
    let base = unique("order");
    let ids: Vec<String> =
        ["c", "a", "b", "0", "a-2"].iter().map(|s| format!("{base}-{s}")).collect();
    for (index, id) in ids.iter().enumerate() {
        let state = every_state()[index % 6];
        assert!(store.put_if_version(record(id, state), 0).await.expect("put"));
    }
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(listed_ids(store.as_ref(), &ids).await, expected, "byte order, every state listed");
}

/// `delete` removes suggestions only.
pub async fn delete_removes_only_suggestions(harness: &dyn AgentStoreHarness) {
    let store = harness.open().await;
    let proposed = unique("proposed");
    assert!(
        store.put_if_version(record(&proposed, AgentRecordState::Proposed), 0).await.expect("put")
    );
    assert!(store.delete(&proposed).await.expect("delete"));
    assert_eq!(store.get(&proposed).await.expect("get"), None);
    assert!(listed_ids(store.as_ref(), std::slice::from_ref(&proposed)).await.is_empty());
    assert!(!store.delete(&proposed).await.expect("delete again"));
    assert!(
        store.put_if_version(record(&proposed, AgentRecordState::Proposed), 0).await.expect("put"),
        "a deleted suggestion's id was never used"
    );

    for state in every_state().into_iter().filter(|state| *state != AgentRecordState::Proposed) {
        let id = unique("kept");
        let kept = record(&id, state);
        assert!(store.put_if_version(kept.clone(), 0).await.expect("put"));
        assert!(!store.delete(&id).await.expect("delete"), "{state} is never deleted");
        assert_eq!(store.get(&id).await.expect("get"), Some(kept));
    }
    assert!(!store.delete(&unique("unknown")).await.expect("delete"));
}

/// A tombstone stays.
pub async fn tombstones_stay(harness: &dyn AgentStoreHarness) {
    let store = harness.open().await;
    let id = unique("tombstone");
    let active = record(&id, AgentRecordState::Active);
    assert!(store.put_if_version(active.clone(), 0).await.expect("put"));
    let removing = next(active.clone(), AgentRecordState::Removing);
    assert!(store.put_if_version(removing.clone(), 1).await.expect("put"));
    let removed = next(removing, AgentRecordState::Removed);
    assert!(store.put_if_version(removed.clone(), 2).await.expect("put"));
    assert_eq!(store.get(&id).await.expect("get"), Some(removed.clone()));
    assert_eq!(
        listed_ids(store.as_ref(), std::slice::from_ref(&id)).await,
        std::slice::from_ref(&id)
    );
    assert!(!store.put_if_version(next(active, AgentRecordState::Active), 1).await.expect("put"));
    assert!(!store.put_if_version(record(&id, AgentRecordState::Active), 0).await.expect("put"));
    assert_eq!(store.get(&id).await.expect("get"), Some(removed));
}

/// Records read back through a new handle.
pub async fn records_survive_a_restart(harness: &dyn AgentStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let id = unique("restart");
    let written = full_record(&id, AgentRecordState::Paused, every_origin()[1].clone());
    assert!(harness.open().await.put_if_version(written.clone(), 0).await.expect("put"));
    let reopened = harness.open().await;
    assert_eq!(reopened.get(&id).await.expect("get"), Some(written));
    assert_eq!(
        listed_ids(reopened.as_ref(), std::slice::from_ref(&id)).await,
        std::slice::from_ref(&id)
    );
}

/// Concurrent creates from two handles lose no record.
pub async fn concurrent_writers(harness: &dyn AgentStoreHarness) {
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { Arc::clone(&first) };
    let mut tasks = Vec::new();
    for writer in 0..2 {
        let store = if writer == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
        tasks.push(tokio::spawn(async move {
            let mut ids = Vec::new();
            for index in 0..20 {
                let id = unique(&format!("w{writer}-{index}"));
                assert!(
                    store
                        .put_if_version(record(&id, AgentRecordState::Active), 0)
                        .await
                        .expect("put")
                );
                ids.push(id);
            }
            ids
        }));
    }
    let mut written = Vec::new();
    for task in tasks {
        written.extend(task.await.expect("task"));
    }
    assert_eq!(
        listed_ids(first.as_ref(), &written).await.len(),
        written.len(),
        "no record is lost"
    );
}

/// Generates one test per [`agent_store`](crate::conformance::agent_store) case for an
/// [`AgentStoreHarness`] (spec 3.3 §5.7, §10 K7).
///
/// Used as [`process_store_conformance_tests!`](crate::process_store_conformance_tests) is;
/// see the [module documentation](crate::conformance).
#[macro_export]
macro_rules! agent_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agent_store_put_get_round_trip() {
            $crate::conformance::agent_store::put_get_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agent_store_put_if_version_compares() {
            $crate::conformance::agent_store::put_if_version_compares(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agent_store_concurrent_puts_succeed_once() {
            $crate::conformance::agent_store::concurrent_puts_succeed_once(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agent_store_list_orders_by_id() {
            $crate::conformance::agent_store::list_orders_by_id(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agent_store_delete_removes_only_suggestions() {
            $crate::conformance::agent_store::delete_removes_only_suggestions(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agent_store_tombstones_stay() {
            $crate::conformance::agent_store::tombstones_stay(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agent_store_records_survive_a_restart() {
            $crate::conformance::agent_store::records_survive_a_restart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn agent_store_concurrent_writers() {
            $crate::conformance::agent_store::concurrent_writers(&$harness).await;
        }
    };
}
