//! Snapshots and rewind (spec 3.1 §7.27, §9.2 I68, §11 `rewind_restores_shell_writes`):
//! a turn that writes through `write_file` and through `bash` is rewound, both writes are
//! undone, and `/redo` brings them back.

#![cfg(feature = "runner")]

use crate::common;

use std::sync::Arc;

use adk_core::{Tool, ToolContext, ToolEffect, async_trait};
use adk_harness::HarnessMode;
use adk_harness::snapshot::{
    ChangeKind, InMemorySnapshotObjects, InMemorySnapshotStore, RewindTarget, SnapshotConfig,
    SnapshotKind,
};
use adk_model::ScriptedResponse;
use adk_session::GetRequest;
use common::{APP, ScriptedPrompter, fixture};
use serde_json::{Value, json};

async fn event_count(f: &common::Fixture) -> usize {
    f.harness
        .runner()
        .session_service()
        .get(GetRequest {
            app_name: APP.into(),
            user_id: f.user.to_string(),
            session_id: f.session.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .len()
}

fn read(f: &common::Fixture, path: &str) -> Option<String> {
    std::fs::read_to_string(f.path(path)).ok()
}

/// The §11 regression test: what `bash` wrote is undone like what `write_file` wrote, and
/// a redo puts both back.
#[tokio::test]
async fn rewind_restores_shell_writes() {
    let prompter = ScriptedPrompter::new();
    for _ in 0..4 {
        prompter.approve_once();
    }
    let f = fixture(
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "notes.txt", "content": "from the tool\n" }),
            ),
            ScriptedResponse::function_call(
                "bash",
                json!({ "command": "echo 'from bash' > shell.txt && sed -i.bak 's/old/new/' kept.txt && rm kept.txt.bak" }),
            ),
            ScriptedResponse::text("done"),
        ],
        {
            let prompter = prompter.clone();
            move |b| {
                b.mode(HarnessMode::Auto).prompter(prompter).snapshots(SnapshotConfig::default())
            }
        },
    )
    .await;
    std::fs::write(f.path("kept.txt"), "old\n").unwrap();

    f.send("write the notes and run the script").await;
    assert_eq!(read(&f, "notes.txt").as_deref(), Some("from the tool\n"));
    assert_eq!(read(&f, "shell.txt").as_deref(), Some("from bash\n"));
    assert_eq!(read(&f, "kept.txt").as_deref(), Some("new\n"), "sed -i changed it");
    let events_after_turn = event_count(&f).await;
    assert!(events_after_turn > 0);

    let turns = f.harness.rewind_turns(&f.user, &f.session).await.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].changed_files, Some(3), "{turns:?}");

    let mut plan = f.harness.rewind_plan(&f.user, &f.session, RewindTarget::Turn(1)).await.unwrap();
    assert_eq!(plan.paths(ChangeKind::Delete), ["notes.txt", "shell.txt"]);
    assert_eq!(plan.paths(ChangeKind::Restore), ["kept.txt"]);
    // A file tool's write is the harness's own; a command's is a conflict, shown, unchosen.
    let conflicts: Vec<&str> = plan.conflicts().map(|e| e.path.as_str()).collect();
    assert_eq!(conflicts, ["kept.txt", "shell.txt"]);
    assert!(plan.not_undone.iter().any(|item| item.tool == "bash"), "{:?}", plan.not_undone);
    assert_eq!(plan.transcript.events_removed, events_after_turn);
    assert!(plan.render().contains("changed since the latest snapshot by a command or by you"));

    plan.choose_conflicts(true);
    let report = f.harness.rewind(&f.user, &f.session, &plan).await.unwrap();
    assert_eq!(report.deleted, ["notes.txt", "shell.txt"]);
    assert_eq!(report.restored, ["kept.txt"]);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(read(&f, "notes.txt"), None);
    assert_eq!(read(&f, "shell.txt"), None);
    assert_eq!(read(&f, "kept.txt").as_deref(), Some("old\n"));
    assert_eq!(event_count(&f).await, 0, "the transcript forked before turn 1");
    let rewinds = f.records("harness.rewind");
    assert_eq!(rewinds.len(), 1);
    assert_eq!(rewinds[0].ext["conflictsChosen"], json!(["kept.txt", "shell.txt"]));
    let kinds: Vec<String> = f
        .records("harness.snapshot.taken")
        .iter()
        .map(|record| record.ext["kind"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(kinds, ["turn", "rewind"]);

    // `/redo`: the rewind's own writes are not conflicts.
    let redo = f.harness.redo_plan(&f.user, &f.session).await.unwrap();
    assert_eq!(redo.snapshot_kind, SnapshotKind::Rewind);
    assert_eq!(redo.conflicts().count(), 0, "{}", redo.render());
    assert_eq!(redo.transcript.events_restored, events_after_turn);
    f.harness.rewind(&f.user, &f.session, &redo).await.unwrap();
    assert_eq!(read(&f, "notes.txt").as_deref(), Some("from the tool\n"));
    assert_eq!(read(&f, "shell.txt").as_deref(), Some("from bash\n"));
    assert_eq!(read(&f, "kept.txt").as_deref(), Some("new\n"));
    assert_eq!(event_count(&f).await, events_after_turn, "the transcript is back");
}

/// A protected path changes only under the rewind's confirmation; files the snapshot did
/// not cover are listed and never touched (I29, I68).
#[tokio::test]
async fn protected_paths_need_confirmation_and_uncovered_files_are_kept() {
    let config = SnapshotConfig { max_file_bytes: 16, ..SnapshotConfig::default() };
    let f = fixture(vec![ScriptedResponse::text("ok"), ScriptedResponse::text("ok")], move |b| {
        b.snapshots(config)
    })
    .await;
    std::fs::write(f.path("AGENTS.md"), "be careful\n").unwrap();
    std::fs::write(f.path(".env"), "TOKEN=one\n").unwrap();
    std::fs::write(f.path("big.bin"), vec![b'x'; 64]).unwrap();
    f.send("hello").await;

    // The person edits between turns.
    std::fs::write(f.path("AGENTS.md"), "be reckless\n").unwrap();
    std::fs::write(f.path(".env"), "TOKEN=two\n").unwrap();
    std::fs::write(f.path("big.bin"), vec![b'y'; 64]).unwrap();

    let mut plan = f.harness.rewind_plan(&f.user, &f.session, RewindTarget::Turn(1)).await.unwrap();
    assert_eq!(plan.paths(ChangeKind::Restore), ["AGENTS.md"]);
    let protected: Vec<&str> = plan.protected().map(|e| e.path.as_str()).collect();
    assert_eq!(protected, ["AGENTS.md"]);
    let uncovered: Vec<&str> = plan.not_covered.iter().map(|item| item.path.as_str()).collect();
    assert_eq!(uncovered, [".env", "big.bin"]);
    plan.choose_conflicts(true);

    let report = f.harness.rewind(&f.user, &f.session, &plan).await.unwrap();
    assert_eq!(report.skipped.len(), 1, "{report:?}");
    assert!(report.skipped[0].reason.contains("protected"));
    assert_eq!(read(&f, "AGENTS.md").as_deref(), Some("be reckless\n"));

    let mut plan = f.harness.rewind_plan(&f.user, &f.session, RewindTarget::Turn(1)).await;
    // The first rewind removed turn 1 from the transcript.
    assert_eq!(plan.as_ref().unwrap_err().code, "harness.rewind.no_turn");
    let redo = f.harness.redo_plan(&f.user, &f.session).await.unwrap();
    f.harness.rewind(&f.user, &f.session, &redo).await.unwrap();
    plan = f.harness.rewind_plan(&f.user, &f.session, RewindTarget::Turn(1)).await;
    let mut plan = plan.unwrap();
    plan.choose_conflicts(true);
    plan.confirm_protected();
    let report = f.harness.rewind(&f.user, &f.session, &plan).await.unwrap();
    assert_eq!(report.restored, ["AGENTS.md"]);
    assert_eq!(read(&f, "AGENTS.md").as_deref(), Some("be careful\n"));
    assert_eq!(read(&f, ".env").as_deref(), Some("TOKEN=two\n"), "excluded: never touched");
    assert_eq!(
        std::fs::read(f.path("big.bin")).unwrap(),
        vec![b'y'; 64],
        "too large: never touched"
    );
}

struct SendEmail;

#[async_trait]
impl Tool for SendEmail {
    fn name(&self) -> &str {
        "send_email"
    }

    fn description(&self) -> &str {
        "sends an email"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "sent": true }))
    }
}

/// A ledgered non-idempotent call after the target is listed as not undone, with its
/// ledger key (spec 3.1 §7.27, 3.3 D-8).
#[tokio::test]
async fn ledgered_effects_after_the_target_are_listed_not_undone() {
    let prompter = ScriptedPrompter::new();
    prompter.approve_once();
    let f = fixture(
        vec![
            ScriptedResponse::text("hi"),
            ScriptedResponse::function_call("send_email", json!({ "to": "ops@example.com" })),
            ScriptedResponse::text("sent"),
        ],
        {
            let prompter = prompter.clone();
            move |b| {
                b.mode(HarnessMode::Auto)
                    .prompter(prompter)
                    .tool(Arc::new(SendEmail))
                    .snapshots(SnapshotConfig::default())
            }
        },
    )
    .await;
    f.send("hello").await;
    f.send("email ops").await;
    let plan = f.harness.rewind_plan(&f.user, &f.session, RewindTarget::Turn(2)).await.unwrap();
    assert_eq!(plan.not_undone.len(), 1, "{:?}", plan.not_undone);
    let item = &plan.not_undone[0];
    assert_eq!(item.tool, "send_email");
    let key = item.ledger_key.as_deref().expect("the ledger recorded the call");
    assert!(key.starts_with(&format!("{APP}/alice/s1/")), "{key}");
    assert!(plan.render().contains("not undone"));
}

/// Snapshots are off until the builder sets them; the block says so (spec 3.1 §13 Q11).
#[tokio::test]
async fn snapshots_are_off_in_the_library_by_default() {
    let f = fixture(vec![ScriptedResponse::text("ok")], |b| b).await;
    f.send("hello").await;
    let error =
        f.harness.rewind_plan(&f.user, &f.session, RewindTarget::Turn(1)).await.unwrap_err();
    assert_eq!(error.code, "harness.snapshot.off");
    let block = f.harness.agent().manifest().render();
    assert!(block.contains("no file is restored"), "{block}");

    let store = Arc::new(InMemorySnapshotStore::new());
    let objects = Arc::new(InMemorySnapshotObjects::new());
    let on = fixture(vec![ScriptedResponse::text("ok")], {
        let (store, objects) = (store.clone(), objects.clone());
        move |b| {
            b.snapshots(SnapshotConfig::default()).snapshot_store(store).snapshot_objects(objects)
        }
    })
    .await;
    on.send("hello").await;
    let block = on.harness.agent().manifest().render();
    assert!(block.contains("shell writes included"), "{block}");
    let info =
        on.harness.snapshot_now(&on.user, &on.session, SnapshotKind::Destructive).await.unwrap();
    assert_eq!(info.kind, SnapshotKind::Destructive);
    assert_eq!(
        adk_harness::snapshot::SnapshotStore::list_session(store.as_ref(), APP, "alice", "s1")
            .await
            .unwrap()
            .len(),
        2,
        "the turn snapshot and the one taken now"
    );
}

/// Records survive in SQLite, sealed under the store key (spec 3.1 §7.12, I68).
#[cfg(all(feature = "sqlite", feature = "encrypted"))]
#[tokio::test]
async fn sqlite_records_are_sealed_and_reopen() {
    use adk_harness::encrypted::StoreKeyring;
    use adk_harness::snapshot::{
        FileSnapshotObjects, SnapshotObjects, SnapshotRequest, SnapshotStore, Snapshots,
        SqliteSnapshotStore,
    };

    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("secret-plan.txt"), "launch at dawn").unwrap();
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let keyring = StoreKeyring::generate();
    let store = SqliteSnapshotStore::new(pool.clone()).with_keyring(keyring.clone());
    store.migrate().await.unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let objects = FileSnapshotObjects::for_workspace(&home.path().join("snapshots"), &root)
        .with_keyring(keyring.clone());
    let snapshots = Snapshots::new(
        SnapshotConfig::default(),
        &adk_devtools::Workspace::new(workspace.path()),
        Arc::new(store.clone()),
        Arc::new(objects.clone()),
    )
    .unwrap();
    let info = snapshots
        .take(SnapshotRequest::new(APP, "alice", "s1", "i1", SnapshotKind::Turn))
        .await
        .unwrap();
    let (record,): (String,) =
        sqlx::query_as("SELECT record FROM adk_harness_snapshots").fetch_one(&pool).await.unwrap();
    assert!(record.starts_with("adk-enc:v1:"), "the record is sealed");
    for object in objects.list().unwrap() {
        let path = home.path().join("snapshots").join(adk_harness::snapshot::root_digest(&root));
        assert!(path.exists());
        let bytes = objects.get(&object.id).unwrap();
        assert!(!bytes.is_empty());
    }
    let on_disk: Vec<u8> = walk_bytes(home.path());
    assert!(!String::from_utf8_lossy(&on_disk).contains("launch at dawn"), "no plaintext on disk");

    let reopened = SqliteSnapshotStore::new(pool).with_keyring(keyring);
    let records = reopened.list_session(APP, "alice", "s1").await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, info.id);
}

#[cfg(all(feature = "sqlite", feature = "encrypted"))]
fn walk_bytes(dir: &std::path::Path) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_bytes(&path));
        } else {
            out.extend(std::fs::read(&path).unwrap());
        }
    }
    out
}

/// A snapshot is taken before each plan step, and a rewind to a step restores its files
/// and rewinds the plan with them, so only that step and the ones after it run again
/// (spec 3.1 §7.27 "Plan").
#[cfg(feature = "graph")]
#[tokio::test]
async fn a_plan_step_rewind_restores_its_files_and_the_plan_together() {
    use adk_harness::snapshot::PlanRewind;
    use adk_harness::{PlanGate, PlanStep};

    let step = |id: &str, deps: &[&str]| PlanStep {
        id: id.into(),
        title: format!("Step {id}"),
        instruction: format!("Write {id}.txt"),
        depends_on: deps.iter().map(|dep| (*dep).to_string()).collect(),
        gate: PlanGate::None,
    };
    let steps = vec![step("1", &[]), step("2", &["1"]), step("3", &["2"])];
    let write = |path: &str| {
        ScriptedResponse::function_call("write_file", json!({ "path": path, "content": path }))
    };
    let prompter = ScriptedPrompter::new();
    prompter.approve_once();
    let f = fixture(
        vec![
            ScriptedResponse::function_call(
                "exit_plan_mode",
                json!({ "plan": "Write three files.", "next_mode": "auto", "steps": steps }),
            ),
            write("1.txt"),
            ScriptedResponse::text("1"),
            write("2.txt"),
            ScriptedResponse::text("2"),
            write("3.txt"),
            ScriptedResponse::text("3"),
        ],
        move |b| {
            b.mode(HarnessMode::Plan)
                .without_default_members()
                .prompter(prompter)
                .snapshots(SnapshotConfig::default())
        },
    )
    .await;
    f.send("plan the change").await;
    f.send("go").await;
    assert!(f.path("1.txt").exists() && f.path("2.txt").exists() && f.path("3.txt").exists());
    let steps_taken: Vec<String> = f
        .records("harness.snapshot.taken")
        .iter()
        .filter(|record| record.ext["kind"] == "step")
        .map(|record| record.ext["snapshotId"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(steps_taken.len(), 3, "one snapshot before each step");

    let plan = f
        .harness
        .rewind_plan(&f.user, &f.session, RewindTarget::PlanStep("3".into()))
        .await
        .unwrap();
    assert_eq!(plan.paths(ChangeKind::Delete), ["3.txt"], "{}", plan.render());
    assert_eq!(plan.plan, Some(PlanRewind::AfterStep { step_id: "2".into() }));
    assert!(!plan.transcript.unchanged);
    f.harness.rewind(&f.user, &f.session, &plan).await.unwrap();
    assert!(!f.path("3.txt").exists());
    assert!(f.path("1.txt").exists() && f.path("2.txt").exists());

    std::fs::remove_file(f.path("1.txt")).unwrap();
    f.model.push(write("3.txt"));
    f.model.push(ScriptedResponse::text("3 again"));
    f.send("again").await;
    assert!(f.path("3.txt").exists(), "step 3 ran again");
    assert!(!f.path("1.txt").exists(), "step 1 did not run again");
}
