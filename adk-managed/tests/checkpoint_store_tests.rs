//! Managed checkpoints are process-local, and now say so.
//!
//! `CheckpointManager` held events in a `Vec` and run state in a struct field, with doc comments
//! claiming that checkpointing "guarantees that replay will see a consistent view after any
//! crash" and that a load returns "everything needed to reconstruct a session after a restart".
//! Neither was true: a crash lost event history, parked-tool state, sequence position, and
//! lifecycle status even when the nested Runner had already written conversation events through
//! the `SessionService`, and a new process could not resume another's session.
//!
//! The in-memory implementation is a reasonable default. The problem was that nothing
//! distinguished it from a durable one — there was no seam to implement against and no way for a
//! caller to ask what guarantee it had.

use adk_managed::state_store::{Durability, InMemoryManagedStateStore, ManagedStateStore};
use adk_managed::types::SessionStatus;
use adk_managed::{CheckpointManager, RunState};
use std::sync::Arc;

/// A run state distinguishable from the initial one.
fn advanced_state() -> RunState {
    RunState {
        seq: 12,
        pending_tool_ids: vec!["call-7".to_string()],
        status: SessionStatus::Running,
    }
}

/// An event to checkpoint.
fn idle_event() -> adk_managed::types::SessionEvent {
    adk_managed::types::SessionEvent::StatusIdle { seq: 12, stop_reason: None, usage: None }
}

#[tokio::test]
async fn a_manager_without_a_store_flushes_without_error() {
    // Flushing is a no-op rather than a failure, so the store stays optional.
    let mut manager = CheckpointManager::new("session-1".to_string());
    manager.checkpoint(idle_event(), advanced_state());

    assert!(manager.store().is_none());
    assert!(manager.flush().await.is_ok());
}

#[tokio::test]
async fn a_flushed_snapshot_is_visible_in_the_store() {
    let store = Arc::new(InMemoryManagedStateStore::new());
    let mut manager =
        CheckpointManager::new("session-1".to_string()).with_store(store.clone() as Arc<_>);

    manager.checkpoint(idle_event(), advanced_state());
    manager.flush().await.expect("flush");

    let stored = store.load("session-1").await.unwrap().expect("snapshot must be stored");
    assert_eq!(stored.run_state.seq, 12);
    assert_eq!(stored.run_state.pending_tool_ids, vec!["call-7".to_string()]);
    assert_eq!(stored.events.len(), 1);
}

#[tokio::test]
async fn a_checkpoint_is_not_in_the_store_until_flushed() {
    // `checkpoint` writes local fields. Calling it "atomic persistence" implied otherwise.
    let store = Arc::new(InMemoryManagedStateStore::new());
    let mut manager =
        CheckpointManager::new("session-1".to_string()).with_store(store.clone() as Arc<_>);

    manager.checkpoint(idle_event(), advanced_state());

    assert!(
        store.load("session-1").await.unwrap().is_none(),
        "a local checkpoint is not a store write"
    );
}

#[tokio::test]
async fn restore_rebuilds_a_manager_from_the_store() {
    let store: Arc<dyn ManagedStateStore> = Arc::new(InMemoryManagedStateStore::new());

    let mut original =
        CheckpointManager::new("session-1".to_string()).with_store(Arc::clone(&store));
    original.checkpoint(idle_event(), advanced_state());
    original.flush().await.expect("flush");

    let restored = CheckpointManager::restore("session-1".to_string(), Arc::clone(&store))
        .await
        .expect("restore");

    assert_eq!(restored.run_state().seq, 12);
    assert_eq!(restored.events().len(), 1);
    assert_eq!(restored.session_id(), "session-1");
}

#[tokio::test]
async fn restoring_an_unknown_session_yields_an_empty_manager() {
    let store: Arc<dyn ManagedStateStore> = Arc::new(InMemoryManagedStateStore::new());
    let restored = CheckpointManager::restore("never-seen".to_string(), store)
        .await
        .expect("restore must not fail on a missing session");

    assert_eq!(restored.run_state().seq, 0);
    assert!(restored.events().is_empty());
}

#[tokio::test]
async fn the_shipped_store_reports_that_it_does_not_survive_process_loss() {
    // This is the finding, made checkable: the guarantee is now reportable rather than implied
    // by the presence of checkpointing.
    let store = InMemoryManagedStateStore::new();
    assert_eq!(store.durability(), Durability::ProcessLocal);
    assert!(!store.durability().survives_process_loss());
}

#[tokio::test]
async fn a_separate_store_instance_sees_nothing_which_is_the_restart_case() {
    let first: Arc<dyn ManagedStateStore> = Arc::new(InMemoryManagedStateStore::new());
    let mut manager =
        CheckpointManager::new("session-1".to_string()).with_store(Arc::clone(&first));
    manager.checkpoint(idle_event(), advanced_state());
    manager.flush().await.expect("flush");

    // A fresh store stands in for a fresh process. A `CrashDurable` implementation backed by
    // shared storage would find the session; the shipped one cannot.
    let second: Arc<dyn ManagedStateStore> = Arc::new(InMemoryManagedStateStore::new());
    let restored =
        CheckpointManager::restore("session-1".to_string(), second).await.expect("restore");

    assert_eq!(
        restored.run_state().seq,
        0,
        "process-local state does not cross a process boundary"
    );
    assert!(restored.events().is_empty());
}
