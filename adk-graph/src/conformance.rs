//! Conformance suite for [`Checkpointer`] backends.
//!
//! Every checkpointer — [`MemoryCheckpointer`](crate::checkpoint::MemoryCheckpointer),
//! the SQLite, PostgreSQL, Redis, MongoDB, Neo4j, and Firestore backends in this crate,
//! and any third-party one — proves the [contract](crate::checkpoint) by running the
//! same cases:
//!
//! | Case | Contract |
//! |------|----------|
//! | [`round_trip`] | `load` and `load_by_id` return exactly what `save` stored; unknown threads and IDs read as `None` |
//! | [`newest_version_wins`] | `load` returns the highest version and `list` ascends by version, whatever the timestamps say |
//! | [`list_pages`] | `list_page` pages newest first below a version, and stops at the limit |
//! | [`load_by_id_reads_any_version`] | An older checkpoint reads back by ID |
//! | [`delete_removes_one_thread`] | `delete` removes the thread's checkpoints and IDs and leaves other threads alone |
//! | [`prune_applies_policies`] | `prune` removes what a policy selects, never the newest, and only in its thread |
//! | [`threads_are_isolated`] | Threads keep separate version sequences |
//! | [`stale_writes_are_refused`] | An equal, lower, or zero version and a stored ID are refused, and leave the thread unchanged |
//! | [`concurrent_writers_never_interleave`] | Of concurrent saves of one version across two handles, exactly one succeeds |
//! | [`checkpoints_survive_a_restart`] | A new handle reads what an earlier one wrote |
//! | [`large_state_round_trips`] | A state of 512 KiB and a thousand channels round-trips |
//! | [`identifiers_are_escaped`] | Thread and checkpoint IDs holding delimiters, quotes, and non-ASCII text never collide |
//!
//! Timestamps must round-trip to the microsecond. The suite compares whole checkpoints.
//!
//! [`checkpointer_conformance_tests!`](crate::checkpointer_conformance_tests) expands to
//! one `#[tokio::test]` per case on a multi-threaded runtime, so the calling crate needs
//! `tokio` with the `macros` and `rt-multi-thread` features. Attributes written before
//! `harness =` apply to every generated test, which is how a live-database suite is
//! marked `#[ignore]`. Enable the `conformance` feature of `adk-graph` in
//! `[dev-dependencies]`.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_graph::checkpoint::Checkpointer;
//! use adk_graph::conformance::CheckpointerHarness;
//!
//! struct MyHarness { url: String }
//!
//! #[async_trait::async_trait]
//! impl CheckpointerHarness for MyHarness {
//!     async fn open(&self) -> Arc<dyn Checkpointer> {
//!         Arc::new(MyCheckpointer::connect(&self.url).await.unwrap())
//!     }
//! }
//!
//! adk_graph::checkpointer_conformance_tests! {
//!     #[ignore = "requires a server at MY_URL"]
//!     harness = MyHarness { url: std::env::var("MY_URL").unwrap() },
//! }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;

use crate::checkpoint::{Checkpointer, RetentionPolicy};
use crate::error::GraphError;
use crate::state::{Checkpoint, PendingWrite, State};

/// Opens a [`Checkpointer`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
/// use adk_graph::conformance::CheckpointerHarness;
///
/// #[derive(Default)]
/// struct InMemory(Arc<MemoryCheckpointer>);
///
/// #[async_trait::async_trait]
/// impl CheckpointerHarness for InMemory {
///     async fn open(&self) -> Arc<dyn Checkpointer> {
///         self.0.clone()
///     }
/// }
/// ```
#[async_trait]
pub trait CheckpointerHarness: Send + Sync {
    /// Opens the checkpointer.
    ///
    /// For a durable backend every call returns a new handle over the same stored
    /// data, as a restarted or second process would see it. A process-local backend
    /// returns the same checkpointer every time. Panics when the backend cannot be
    /// opened.
    async fn open(&self) -> Arc<dyn Checkpointer>;
}

/// An identifier no other case or run uses. It holds `:` and `/` so a backend that
/// builds keys or paths from identifiers is checked for escaping.
fn unique(prefix: &str) -> String {
    format!("conformance:{prefix}/{}", uuid::Uuid::new_v4())
}

fn whole_microsecond(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(at.timestamp_micros()).expect("in range")
}

fn checkpoint(thread_id: &str, version: u64) -> Checkpoint {
    let mut state = State::new();
    state.insert("version".to_string(), json!(version));
    Checkpoint::new(thread_id, state, version as usize, vec!["next".to_string()])
        .with_version(version)
}

/// A checkpoint with every field set to a non-default value.
fn populated(thread_id: &str, version: u64) -> Checkpoint {
    let mut checkpoint = checkpoint(thread_id, version);
    checkpoint.state.insert("messages".to_string(), json!([{"role": "user", "content": "hi"}]));
    checkpoint.state.insert("nested".to_string(), json!({"a": [1, 2.5, null, true], "b": "ü"}));
    checkpoint.pending_nodes = vec!["left".to_string(), "right".to_string()];
    checkpoint.metadata.insert("source".to_string(), json!("conformance"));
    checkpoint.cleared_interrupt = Some("approve".to_string());
    checkpoint.attempts = HashMap::from([("flaky".to_string(), 2)]);
    checkpoint.child_ledger = HashMap::from([("child/1".to_string(), json!({"ok": true}))]);
    checkpoint.pending_writes = BTreeMap::from([(
        "left".to_string(),
        PendingWrite {
            updates: HashMap::from([("left_result".to_string(), json!(42))]),
            goto: Some(vec!["join".to_string()]),
            goto_parent: None,
        },
    )]);
    checkpoint.created_at = whole_microsecond(Utc::now());
    checkpoint
}

async fn save(store: &dyn Checkpointer, checkpoint: &Checkpoint) {
    let id = store.save(checkpoint).await.expect("the checkpointer accepts a newer version");
    assert_eq!(id, checkpoint.checkpoint_id, "save returns the checkpoint's id");
}

async fn versions(store: &dyn Checkpointer, thread_id: &str) -> Vec<u64> {
    store.list(thread_id).await.expect("list").iter().map(|c| c.version).collect()
}

fn is_conflict(error: &GraphError, expected_head: u64) -> bool {
    matches!(error, GraphError::CheckpointConflict { head, .. } if *head == expected_head)
}

/// `load` and `load_by_id` return exactly what `save` stored.
pub async fn round_trip<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let thread = unique("round-trip");
    assert_eq!(store.load(&thread).await.expect("load"), None, "an unknown thread is empty");
    assert_eq!(store.load_by_id(&unique("missing")).await.expect("load_by_id"), None);
    assert!(store.list(&thread).await.expect("list").is_empty());

    let saved = populated(&thread, 1);
    save(&*store, &saved).await;
    assert_eq!(store.load(&thread).await.expect("load"), Some(saved.clone()));
    assert_eq!(
        store.load_by_id(&saved.checkpoint_id).await.expect("load_by_id"),
        Some(saved.clone())
    );
    assert_eq!(store.list(&thread).await.expect("list"), vec![saved]);
}

/// The highest version is the newest, whatever the timestamps and steps say.
pub async fn newest_version_wins<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let thread = unique("ordering");
    let now = whole_microsecond(Utc::now());
    let mut saved = Vec::new();
    // Later versions carry earlier timestamps and repeat a step, so ordering by time
    // or by step would pick the wrong checkpoint.
    for (index, version) in [1u64, 2, 5].into_iter().enumerate() {
        let mut checkpoint = checkpoint(&thread, version);
        checkpoint.step = 3;
        checkpoint.created_at = now - chrono::Duration::seconds(index as i64 * 60);
        save(&*store, &checkpoint).await;
        saved.push(checkpoint);
    }
    assert_eq!(store.load(&thread).await.expect("load"), saved.last().cloned());
    assert_eq!(store.list(&thread).await.expect("list"), saved);
}

/// `list_page` returns newest first, below `before`, at most `limit`.
pub async fn list_pages<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let thread = unique("pages");
    for version in 1..=7 {
        save(&*store, &checkpoint(&thread, version)).await;
    }
    let page = |before: Option<u64>, limit: usize| {
        let store = Arc::clone(&store);
        let thread = thread.clone();
        async move {
            store
                .list_page(&thread, before, limit)
                .await
                .expect("list_page")
                .iter()
                .map(|c| c.version)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(page(None, 3).await, vec![7, 6, 5]);
    assert_eq!(page(Some(5), 3).await, vec![4, 3, 2]);
    assert_eq!(page(Some(2), 3).await, vec![1]);
    assert_eq!(page(Some(1), 3).await, Vec::<u64>::new());
    assert_eq!(page(None, 0).await, Vec::<u64>::new());
    assert_eq!(page(None, 100).await, vec![7, 6, 5, 4, 3, 2, 1]);
    let full = store.list_page(&thread, Some(4), 1).await.expect("list_page");
    assert_eq!(full, vec![checkpoint_with_id(&*store, &thread, 3).await]);
}

async fn checkpoint_with_id(store: &dyn Checkpointer, thread: &str, version: u64) -> Checkpoint {
    store
        .list(thread)
        .await
        .expect("list")
        .into_iter()
        .find(|c| c.version == version)
        .expect("version is stored")
}

/// Any stored version reads back by ID.
pub async fn load_by_id_reads_any_version<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let thread = unique("by-id");
    let first = populated(&thread, 1);
    let second = populated(&thread, 2);
    save(&*store, &first).await;
    save(&*store, &second).await;
    assert_eq!(store.load_by_id(&first.checkpoint_id).await.expect("load_by_id"), Some(first));
    assert_eq!(store.load_by_id(&second.checkpoint_id).await.expect("load_by_id"), Some(second));
}

/// `delete` removes one thread, its IDs included, and starts its versions afresh.
pub async fn delete_removes_one_thread<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let doomed = unique("doomed");
    let kept = unique("kept");
    let doomed_first = checkpoint(&doomed, 1);
    save(&*store, &doomed_first).await;
    save(&*store, &checkpoint(&doomed, 2)).await;
    let kept_first = checkpoint(&kept, 1);
    save(&*store, &kept_first).await;

    store.delete(&doomed).await.expect("delete");
    assert_eq!(store.load(&doomed).await.expect("load"), None);
    assert!(store.list(&doomed).await.expect("list").is_empty());
    assert_eq!(store.load_by_id(&doomed_first.checkpoint_id).await.expect("load_by_id"), None);
    assert_eq!(store.list(&kept).await.expect("list"), vec![kept_first]);

    store.delete(&doomed).await.expect("deleting an empty thread is a no-op");
    save(&*store, &checkpoint(&doomed, 1)).await;
    assert_eq!(versions(&*store, &doomed).await, vec![1]);
}

/// `prune` removes what the policy selects, keeps the newest, and stays in its thread.
pub async fn prune_applies_policies<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let policy_none = RetentionPolicy::default();
    assert_eq!(
        store.prune(&unique("unknown"), &RetentionPolicy::keep_last(1)).await.expect("prune"),
        0
    );

    // Count limit.
    let counted = unique("count");
    let neighbour = unique("neighbour");
    for version in 1..=5 {
        save(&*store, &checkpoint(&counted, version)).await;
        save(&*store, &checkpoint(&neighbour, version)).await;
    }
    assert_eq!(store.prune(&counted, &policy_none).await.expect("prune"), 0);
    assert_eq!(store.prune(&counted, &RetentionPolicy::keep_last(2)).await.expect("prune"), 3);
    assert_eq!(versions(&*store, &counted).await, vec![4, 5]);
    assert_eq!(versions(&*store, &neighbour).await, vec![1, 2, 3, 4, 5], "other threads untouched");
    assert_eq!(store.prune(&counted, &RetentionPolicy::keep_last(2)).await.expect("prune"), 0);

    // Age limit: every checkpoint is old, yet the newest stays.
    let aged = unique("age");
    let old = whole_microsecond(Utc::now() - chrono::Duration::days(30));
    for version in 1..=3 {
        let mut checkpoint = checkpoint(&aged, version);
        checkpoint.created_at = old;
        save(&*store, &checkpoint).await;
    }
    let mut fresh = checkpoint(&aged, 4);
    fresh.created_at = whole_microsecond(Utc::now());
    save(&*store, &fresh).await;
    let mut stale_newest = checkpoint(&aged, 5);
    stale_newest.created_at = old;
    save(&*store, &stale_newest).await;
    let max_age = RetentionPolicy::max_age(Duration::from_secs(3600));
    assert_eq!(store.prune(&aged, &max_age).await.expect("prune"), 3);
    assert_eq!(versions(&*store, &aged).await, vec![4, 5], "the newest is kept even when old");

    // Both limits.
    let both = unique("both");
    for version in 1..=4 {
        let mut checkpoint = checkpoint(&both, version);
        checkpoint.created_at = if version == 1 { old } else { whole_microsecond(Utc::now()) };
        save(&*store, &checkpoint).await;
    }
    let policy = RetentionPolicy::keep_last(3).with_max_age(Duration::from_secs(3600));
    assert_eq!(store.prune(&both, &policy).await.expect("prune"), 1);
    assert_eq!(versions(&*store, &both).await, vec![2, 3, 4]);

    // A pruned thread keeps its head: an old version is still refused.
    let error = store.save(&checkpoint(&counted, 3)).await.expect_err("version 3 is behind");
    assert!(is_conflict(&error, 5), "got {error}");
}

/// Threads keep separate version sequences.
pub async fn threads_are_isolated<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let left = unique("left");
    let right = unique("right");
    let left_first = checkpoint(&left, 1);
    let right_first = checkpoint(&right, 1);
    save(&*store, &left_first).await;
    save(&*store, &right_first).await;
    let left_second = checkpoint(&left, 2);
    save(&*store, &left_second).await;
    assert_eq!(store.load(&left).await.expect("load"), Some(left_second));
    assert_eq!(store.load(&right).await.expect("load"), Some(right_first));
}

/// Equal, lower, and zero versions and a stored ID are refused without effect.
pub async fn stale_writes_are_refused<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let thread = unique("stale");
    let first = checkpoint(&thread, 1);
    save(&*store, &first).await;
    save(&*store, &checkpoint(&thread, 2)).await;

    let error = store.save(&checkpoint(&thread, 2)).await.expect_err("version 2 is stored");
    assert!(is_conflict(&error, 2), "an equal version conflicts, got {error}");
    let error = store.save(&checkpoint(&thread, 1)).await.expect_err("version 1 is behind");
    assert!(is_conflict(&error, 2), "a lower version conflicts, got {error}");
    store.save(&checkpoint(&thread, 0)).await.expect_err("version 0 is invalid");

    let mut reused = checkpoint(&thread, 3);
    reused.checkpoint_id = first.checkpoint_id.clone();
    store.save(&reused).await.expect_err("a stored id is refused");
    let mut reused_elsewhere = checkpoint(&unique("other"), 1);
    reused_elsewhere.checkpoint_id = first.checkpoint_id.clone();
    store.save(&reused_elsewhere).await.expect_err("a stored id is refused in any thread");

    assert_eq!(versions(&*store, &thread).await, vec![1, 2], "refused saves change nothing");
    assert_eq!(store.load_by_id(&first.checkpoint_id).await.expect("load_by_id"), Some(first));
    save(&*store, &checkpoint(&thread, 3)).await;
    // Versions may skip.
    save(&*store, &checkpoint(&thread, 10)).await;
    assert_eq!(versions(&*store, &thread).await, vec![1, 2, 3, 10]);
}

/// Concurrent saves of one version, across two handles, admit exactly one.
pub async fn concurrent_writers_never_interleave<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let first = harness.open().await;
    let second = harness.open().await;
    let thread = unique("race");
    save(&*first, &checkpoint(&thread, 1)).await;

    const ROUNDS: u64 = 5;
    const WRITERS: usize = 8;
    let mut winners = Vec::new();
    for round in 0..ROUNDS {
        let version = round + 2;
        let attempts: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let store = if writer % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
                let candidate = checkpoint(&thread, version);
                tokio::spawn(async move {
                    let result = store.save(&candidate).await;
                    (candidate, result)
                })
            })
            .collect();
        let mut won = Vec::new();
        for attempt in attempts {
            let (candidate, result) = attempt.await.expect("writer task");
            match result {
                Ok(_) => won.push(candidate),
                Err(GraphError::CheckpointConflict { .. }) => {}
                Err(error) => panic!("a losing writer gets a conflict, got {error}"),
            }
        }
        assert_eq!(won.len(), 1, "round {round}: exactly one writer stores version {version}");
        winners.extend(won);
    }
    let stored = second.list(&thread).await.expect("list");
    assert_eq!(
        stored.iter().map(|c| c.version).collect::<Vec<_>>(),
        (1..=ROUNDS + 1).collect::<Vec<_>>()
    );
    assert_eq!(&stored[1..], winners.as_slice(), "the stored checkpoints are the winners'");
    assert_eq!(first.load(&thread).await.expect("load").as_ref(), winners.last());
}

/// A new handle reads what an earlier handle wrote.
pub async fn checkpoints_survive_a_restart<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let thread = unique("restart");
    let saved = populated(&thread, 1);
    {
        let before = harness.open().await;
        save(&*before, &saved).await;
    }
    let after = harness.open().await;
    assert_eq!(after.load(&thread).await.expect("load"), Some(saved.clone()));
    assert_eq!(
        after.load_by_id(&saved.checkpoint_id).await.expect("load_by_id"),
        Some(saved.clone())
    );
    let error = after.save(&checkpoint(&thread, 1)).await.expect_err("the head survives too");
    assert!(is_conflict(&error, 1), "got {error}");
}

/// A large state round-trips intact.
pub async fn large_state_round_trips<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let thread = unique("large");
    let mut large = checkpoint(&thread, 1);
    large.state.insert("blob".to_string(), json!("x".repeat(512 * 1024)));
    for index in 0..1000 {
        large.state.insert(format!("channel_{index}"), json!({ "index": index, "text": "ü✓" }));
    }
    save(&*store, &large).await;
    assert_eq!(store.load(&thread).await.expect("load"), Some(large));
}

/// Identifiers holding delimiters, quotes, and non-ASCII text never collide.
pub async fn identifiers_are_escaped<H: CheckpointerHarness + ?Sized>(harness: &H) {
    let store = harness.open().await;
    let base = unique("escape");
    let suffixes = [
        "a:b",
        "a%3Ab",
        "a/b",
        "a%2Fb",
        "{a}",
        "a b",
        "ü-ñ-✓",
        "a'b\"c",
        "a\\b",
        ".",
        "..",
        "*",
        "a?b#c",
        "a\nb",
        "$where",
    ];
    let mut saved = Vec::new();
    for suffix in suffixes {
        let mut checkpoint = checkpoint(&format!("{base}{suffix}"), 1);
        checkpoint.checkpoint_id = format!("{base}{suffix}");
        save(&*store, &checkpoint).await;
        saved.push(checkpoint);
    }
    for checkpoint in &saved {
        assert_eq!(
            store.load(&checkpoint.thread_id).await.expect("load").as_ref(),
            Some(checkpoint)
        );
        assert_eq!(
            store.load_by_id(&checkpoint.checkpoint_id).await.expect("load_by_id").as_ref(),
            Some(checkpoint)
        );
        assert_eq!(store.list(&checkpoint.thread_id).await.expect("list").len(), 1);
    }
    store.delete(&saved[0].thread_id).await.expect("delete");
    for checkpoint in &saved[1..] {
        assert_eq!(
            store.load(&checkpoint.thread_id).await.expect("load").as_ref(),
            Some(checkpoint)
        );
    }
}

/// Generates one test per [`conformance`](crate::conformance) case for a
/// [`CheckpointerHarness`].
///
/// See the [module documentation](crate::conformance) for an example.
#[macro_export]
macro_rules! checkpointer_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_round_trip() {
            $crate::conformance::round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_newest_version_wins() {
            $crate::conformance::newest_version_wins(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_list_pages() {
            $crate::conformance::list_pages(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_load_by_id_reads_any_version() {
            $crate::conformance::load_by_id_reads_any_version(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_delete_removes_one_thread() {
            $crate::conformance::delete_removes_one_thread(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_prune_applies_policies() {
            $crate::conformance::prune_applies_policies(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_threads_are_isolated() {
            $crate::conformance::threads_are_isolated(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_stale_writes_are_refused() {
            $crate::conformance::stale_writes_are_refused(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_concurrent_writers_never_interleave() {
            $crate::conformance::concurrent_writers_never_interleave(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_checkpoints_survive_a_restart() {
            $crate::conformance::checkpoints_survive_a_restart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_large_state_round_trips() {
            $crate::conformance::large_state_round_trips(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn checkpointer_identifiers_are_escaped() {
            $crate::conformance::identifiers_are_escaped(&$harness).await;
        }
    };
}
