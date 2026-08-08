//! The checkpoint model must grow without invalidating stored checkpoints.
//!
//! Every field added for retry budgets, imperative child invocation, or the
//! interrupt cursor is optional on the wire. A checkpoint written before those
//! fields existed still has to load, or an upgrade would strand every resumable
//! run in flight.

use adk_graph::state::Checkpoint;
use serde_json::json;

/// A checkpoint serialized before the new fields existed still deserializes.
#[test]
fn a_checkpoint_without_the_new_fields_still_loads() {
    // Exactly the shape written by the released version: no `cleared_interrupt`,
    // no `attempts`, no `child_ledger`.
    let stored = json!({
        "thread_id": "thread-1",
        "checkpoint_id": "cp-1",
        "state": { "value": 7 },
        "step": 3,
        "pending_nodes": ["next"],
        "metadata": {},
        "created_at": "2026-07-01T12:00:00Z"
    });

    let checkpoint: Checkpoint =
        serde_json::from_value(stored).expect("an older checkpoint must still load");

    assert_eq!(checkpoint.thread_id, "thread-1");
    assert_eq!(checkpoint.step, 3);
    assert_eq!(checkpoint.pending_nodes, vec!["next".to_string()]);
    assert_eq!(checkpoint.cleared_interrupt, None);
    assert!(checkpoint.attempts.is_empty());
    assert!(checkpoint.child_ledger.is_empty());
}

/// The new fields are omitted when empty, so a checkpoint does not grow for a
/// graph that uses none of them.
#[test]
fn empty_bookkeeping_is_not_serialized() {
    let checkpoint = Checkpoint::new("thread-1", Default::default(), 0, vec!["first".to_string()]);
    let encoded = serde_json::to_value(&checkpoint).expect("serialize");

    assert!(encoded.get("cleared_interrupt").is_none());
    assert!(encoded.get("attempts").is_none());
    assert!(encoded.get("child_ledger").is_none());
}

/// All three survive a round trip when set.
#[test]
fn bookkeeping_survives_a_round_trip() {
    let mut checkpoint =
        Checkpoint::new("thread-1", Default::default(), 1, vec!["gated".to_string()]);
    checkpoint.cleared_interrupt = Some("gated".to_string());
    checkpoint.attempts.insert("flaky".to_string(), 2);
    checkpoint.child_ledger.insert("parent/child@1".to_string(), json!({ "ok": true }));

    let encoded = serde_json::to_string(&checkpoint).expect("serialize");
    let decoded: Checkpoint = serde_json::from_str(&encoded).expect("deserialize");

    assert_eq!(decoded.cleared_interrupt.as_deref(), Some("gated"));
    assert_eq!(decoded.attempts.get("flaky"), Some(&2));
    assert_eq!(
        decoded.child_ledger.get("parent/child@1").and_then(|v| v.get("ok")),
        Some(&json!(true))
    );
}

/// The durable backend persists all three.
///
/// They are struct fields, so an in-memory checkpointer keeps them whether or not
/// the SQL layer knows about them. Adding `cleared_interrupt` without a column
/// left the interrupt fix working in tests and broken in production; these
/// columns exist so the same trap does not recur for retry and child ledgers.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn the_sqlite_backend_persists_the_bookkeeping() {
    use adk_graph::checkpoint::{Checkpointer, SqliteCheckpointer};

    let checkpointer =
        SqliteCheckpointer::new("sqlite::memory:").await.expect("open the checkpointer");

    let mut checkpoint =
        Checkpoint::new("thread-1", Default::default(), 4, vec!["gated".to_string()]);
    checkpoint.cleared_interrupt = Some("gated".to_string());
    checkpoint.attempts.insert("flaky".to_string(), 3);
    checkpoint.child_ledger.insert("parent/child@1".to_string(), json!("done"));

    checkpointer.save(&checkpoint).await.expect("save");
    let loaded = checkpointer.load("thread-1").await.expect("load").expect("a checkpoint");

    assert_eq!(loaded.cleared_interrupt.as_deref(), Some("gated"));
    assert_eq!(loaded.attempts.get("flaky"), Some(&3));
    assert_eq!(loaded.child_ledger.get("parent/child@1"), Some(&json!("done")));
}
