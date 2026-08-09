//! Tests for the first managed state store that survives process loss.
//!
//! `InMemoryManagedStateStore` reports `ProcessLocal` and loses everything when the
//! process stops. These tests pin that a file-backed store reports `CrashDurable`
//! and that a second store, sharing only the directory, reads what the first wrote.

use adk_managed::checkpoint::RunState;
use adk_managed::state_store::{
    Durability, FileManagedStateStore, InMemoryManagedStateStore, ManagedSessionState,
    ManagedStateStore,
};

fn temp_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("adk-managed-{}-{name}", std::process::id()))
}

fn a_state(seq: u64) -> ManagedSessionState {
    let mut run_state = RunState::initial();
    run_state.seq = seq;
    ManagedSessionState { events: Vec::new(), run_state }
}

#[tokio::test]
async fn the_in_memory_store_says_it_is_not_durable() {
    // The contrast the file store exists to provide.
    let store = InMemoryManagedStateStore::default();
    assert_eq!(store.durability(), Durability::ProcessLocal);
    assert!(!store.durability().survives_process_loss());
}

#[tokio::test]
async fn the_file_store_declares_crash_durability() {
    let store = FileManagedStateStore::new(temp_root("declares"));
    assert_eq!(store.durability(), Durability::CrashDurable);
    assert!(store.durability().survives_process_loss());
}

#[tokio::test]
async fn a_second_store_reads_what_the_first_wrote() {
    // Two stores sharing only the directory, standing in for two process lifetimes.
    let root = temp_root("across");
    let _ = std::fs::remove_dir_all(&root);

    let first = FileManagedStateStore::new(&root);
    first.save("session-a", a_state(7)).await.unwrap();
    drop(first);

    let second = FileManagedStateStore::new(&root);
    let loaded = second.load("session-a").await.unwrap().expect("the snapshot survived");
    assert_eq!(loaded.run_state.seq, 7);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_save_replaces_the_previous_snapshot() {
    let root = temp_root("replace");
    let _ = std::fs::remove_dir_all(&root);
    let store = FileManagedStateStore::new(&root);

    store.save("session-a", a_state(1)).await.unwrap();
    store.save("session-a", a_state(2)).await.unwrap();

    assert_eq!(store.load("session-a").await.unwrap().unwrap().run_state.seq, 2);
    assert_eq!(store.session_ids().await.unwrap(), vec!["session-a".to_string()]);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_absent_session_loads_as_none() {
    let store = FileManagedStateStore::new(temp_root("absent"));
    assert!(store.load("never-written").await.unwrap().is_none());
    assert!(store.session_ids().await.unwrap().is_empty(), "a missing directory lists nothing");
}

#[tokio::test]
async fn sessions_are_listed_and_deleted_individually() {
    let root = temp_root("many");
    let _ = std::fs::remove_dir_all(&root);
    let store = FileManagedStateStore::new(&root);

    for id in ["alpha", "beta", "gamma"] {
        store.save(id, a_state(1)).await.unwrap();
    }
    assert_eq!(store.session_ids().await.unwrap(), vec!["alpha", "beta", "gamma"]);

    store.delete("beta").await.unwrap();
    assert_eq!(store.session_ids().await.unwrap(), vec!["alpha", "gamma"]);
    // Deleting one leaves the others readable, which one-file-per-session buys.
    assert!(store.load("alpha").await.unwrap().is_some());
    // And deleting what is not there is not an error.
    store.delete("beta").await.unwrap();

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_session_id_cannot_write_outside_the_root() {
    // An id is not a path. Without escaping, `../` would place a snapshot anywhere
    // the process can write.
    let root = temp_root("escape");
    let _ = std::fs::remove_dir_all(&root);
    let store = FileManagedStateStore::new(&root);

    store.save("../../etc/passwd", a_state(1)).await.unwrap();

    let escaped = root.parent().unwrap().parent().unwrap().join("etc/passwd.json");
    assert!(!escaped.exists(), "the id must not escape the root");
    let written: Vec<_> = std::fs::read_dir(&root).unwrap().filter_map(Result::ok).collect();
    assert_eq!(written.len(), 1, "exactly one file, inside the root");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_partial_write_cannot_be_read_as_a_snapshot() {
    // The rename is what makes this true: a reader sees either the old snapshot or
    // the new one, never a half-written file.
    let root = temp_root("atomic");
    let _ = std::fs::remove_dir_all(&root);
    let store = FileManagedStateStore::new(&root);

    store.save("session-a", a_state(1)).await.unwrap();
    // A leftover temporary must not be mistaken for the snapshot.
    std::fs::write(root.join("session-a.json.tmp"), b"{ truncated").unwrap();

    assert_eq!(store.load("session-a").await.unwrap().unwrap().run_state.seq, 1);
    assert_eq!(
        store.session_ids().await.unwrap(),
        vec!["session-a".to_string()],
        "a .tmp file is not a session"
    );

    let _ = std::fs::remove_dir_all(&root);
}
