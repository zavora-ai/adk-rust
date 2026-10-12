//! Tests for the managed state stores that survive process loss.
//!
//! `InMemoryManagedStateStore` reports `ProcessLocal` and loses everything when the
//! process stops. These tests pin that the file store and the session-service store report
//! what they guarantee, and that a second store, sharing only the backing storage, reads
//! what the first wrote.

use adk_managed::checkpoint::RunState;
use std::sync::Arc;

use adk_managed::state_store::{
    Durability, FileManagedStateStore, InMemoryManagedStateStore, ManagedSessionState,
    ManagedStateStore, PendingAction, PendingKind, SessionRecord, SessionServiceStateStore,
};
use adk_managed::types::{ContentBlock, ManagedAgentDef, ModelRef, SessionEvent};
use adk_session::SqliteSessionService;

fn temp_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("adk-managed-{}-{name}", std::process::id()))
}

fn a_state(seq: u64) -> ManagedSessionState {
    let mut run_state = RunState::initial();
    run_state.seq = seq;
    ManagedSessionState::new(Vec::new(), run_state)
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

/// A snapshot with events, a record, and a pending action, so every field round-trips.
fn a_full_state(events: u64) -> ManagedSessionState {
    let mut state = a_state(events);
    state.events = (0..events)
        .map(|seq| SessionEvent::Message {
            content: vec![ContentBlock::Text { text: format!("event {seq}") }],
            seq,
        })
        .collect();
    state.record = Some(SessionRecord::new(
        "app",
        "user",
        ManagedAgentDef::new("agent", ModelRef::Shorthand("gpt-5.4-mini".into())),
    ));
    state.pending = vec![PendingAction {
        id: "call-1".into(),
        kind: PendingKind::CustomTool,
        tool_name: "deploy".into(),
        function_call_id: Some("call-1".into()),
        decision_key: "custom_tool_result:call-1".into(),
        input: serde_json::json!({ "target": "prod" }),
        expires_at: None,
        resolution: None,
    }];
    state.durable_run_id = Some("run-1".into());
    state
}

/// Every field that matters for rebuilding a session, comparable.
fn summary(
    state: &ManagedSessionState,
) -> (u64, usize, Option<String>, Vec<PendingAction>, Option<String>) {
    (
        state.run_state.seq,
        state.events.len(),
        state.record.as_ref().map(|record| record.agent.name.clone()),
        state.pending.clone(),
        state.durable_run_id.clone(),
    )
}

async fn sqlite_sessions(path: &std::path::Path) -> Arc<dyn adk_session::SessionService> {
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let service = SqliteSessionService::new(&url).await.expect("open sqlite sessions");
    service.migrate().await.expect("migrate sqlite sessions");
    Arc::new(service)
}

#[tokio::test]
async fn the_session_service_store_reports_the_guarantee_it_was_given() {
    let service = Arc::new(adk_session::InMemorySessionService::new());
    let store = SessionServiceStateStore::new(service.clone(), Durability::ProcessLocal);
    assert_eq!(store.durability(), Durability::ProcessLocal);
    let store = SessionServiceStateStore::new(service, Durability::CrashDurable);
    assert_eq!(store.durability(), Durability::CrashDurable);
}

#[tokio::test]
async fn the_session_service_store_survives_reopening_its_database() {
    let root = temp_root("session-service");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("sessions.db");

    let first = SessionServiceStateStore::new(sqlite_sessions(&db).await, Durability::CrashDurable);
    first.save("session-a", a_full_state(3)).await.unwrap();
    // A later save writes only the events added since; the snapshot still reads whole.
    first.save("session-a", a_full_state(5)).await.unwrap();
    first.save("session-b", a_full_state(1)).await.unwrap();
    drop(first);

    let second =
        SessionServiceStateStore::new(sqlite_sessions(&db).await, Durability::CrashDurable);
    let loaded = second.load("session-a").await.unwrap().expect("the snapshot survived");
    assert_eq!(summary(&loaded), summary(&a_full_state(5)));
    match &loaded.events[4] {
        SessionEvent::Message { seq, .. } => assert_eq!(*seq, 4, "events load in order"),
        other => panic!("unexpected event {other:?}"),
    }
    assert_eq!(second.session_ids().await.unwrap(), vec!["session-a", "session-b"]);

    second.delete("session-a").await.unwrap();
    assert!(second.load("session-a").await.unwrap().is_none());
    second.delete("session-a").await.unwrap();
    assert_eq!(second.session_ids().await.unwrap(), vec!["session-b"]);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn every_store_round_trips_the_whole_snapshot() {
    let root = temp_root("round-trip");
    let _ = std::fs::remove_dir_all(&root);
    let stores: Vec<Arc<dyn ManagedStateStore>> = vec![
        Arc::new(InMemoryManagedStateStore::new()),
        Arc::new(FileManagedStateStore::new(root.join("files"))),
        Arc::new(SessionServiceStateStore::new(
            Arc::new(adk_session::InMemorySessionService::new()),
            Durability::ProcessLocal,
        )),
    ];
    for store in stores {
        store.save("session-a", a_full_state(2)).await.unwrap();
        let loaded = store.load("session-a").await.unwrap().expect("saved state loads");
        assert_eq!(summary(&loaded), summary(&a_full_state(2)), "{store:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_stored_record_drops_the_compatible_api_key() {
    use adk_managed::types::{ModelConfig, Provider};
    let agent = ManagedAgentDef::new(
        "agent",
        ModelRef::Structured {
            provider: Provider::OpenaiCompatible,
            model: ModelConfig::Compatible {
                model: "m".into(),
                base_url: "https://llm.example/v1".into(),
                api_key: "sk-do-not-store".into(),
            },
            speed: None,
        },
    );
    let mut state = a_state(0);
    state.record = Some(SessionRecord::new("app", "user", agent));
    let root = temp_root("secret");
    let _ = std::fs::remove_dir_all(&root);
    let store = FileManagedStateStore::new(&root);
    store.save("session-a", state).await.unwrap();
    let written = std::fs::read_to_string(root.join("session-a.json")).unwrap();
    assert!(!written.contains("sk-do-not-store"));
    let _ = std::fs::remove_dir_all(&root);
}
