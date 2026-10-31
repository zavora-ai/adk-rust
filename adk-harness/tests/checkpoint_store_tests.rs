//! Plan and goal checkpointers named by a store URL (spec 3.1 §7.11, §13 Q5), and the
//! checkpointer sealed under the store key (spec 3.1 §7.12, I57).
//!
//! The PostgreSQL and Redis legs run in `shared_store_postgres_tests` and
//! `shared_store_redis_tests`, which need a server.

#![cfg(feature = "graph")]

use adk_graph::{Checkpoint, State};
use adk_harness::checkpointer_from_url;
use serde_json::json;

fn checkpoint(thread: &str, version: u64, text: &str) -> Checkpoint {
    let mut state = State::new();
    state.insert("plan".into(), json!(text));
    let mut checkpoint = Checkpoint::new(thread, state, version as usize, vec!["step:1".into()]);
    checkpoint.version = version;
    checkpoint
}

#[tokio::test]
async fn a_memory_url_opens_a_checkpointer_in_memory() {
    let checkpointer = checkpointer_from_url("memory:").await.unwrap();
    checkpointer.save(&checkpoint("t", 1, "a")).await.unwrap();
    assert_eq!(checkpointer.load("t").await.unwrap().unwrap().state["plan"], "a");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_sqlite_url_keeps_checkpoints_across_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("plans.db").display());
    let first = checkpointer_from_url(&url).await.unwrap();
    first.save(&checkpoint("t", 1, "a")).await.unwrap();
    drop(first);
    let second = checkpointer_from_url(&url).await.unwrap();
    assert_eq!(second.load("t").await.unwrap().unwrap().state["plan"], "a");
}

/// A scheme whose backend this build lacks is refused with the feature that adds it, and
/// the URL's password never reaches the message.
#[tokio::test]
async fn a_scheme_this_build_lacks_names_its_feature() {
    let cases = [
        ("sqlite:///tmp/x.db", "sqlite", cfg!(feature = "sqlite")),
        ("postgres://app:hunter2@db/harness", "postgres", cfg!(feature = "postgres")),
        ("rediss://:hunter2@cache:6380", "redis", cfg!(feature = "redis")),
        ("mongodb://app:hunter2@mongo/harness", "mongodb", cfg!(feature = "mongodb")),
        ("bolt://neo4j:hunter2@graph:7687", "neo4j", cfg!(feature = "neo4j")),
        ("firestore://my-project", "firestore", cfg!(feature = "firestore")),
    ];
    for (url, feature, compiled) in cases {
        if compiled {
            continue;
        }
        let error = checkpointer_from_url(url).await.err().expect(url);
        assert_eq!(error.code, "harness.checkpointer.not_compiled", "{url}");
        assert!(error.message.contains(&format!("`{feature}` feature")), "{}", error.message);
        assert!(!error.message.contains("hunter2"), "{}", error.message);
    }
}

#[cfg(all(feature = "encrypted", feature = "sqlite"))]
mod sealed {
    use std::sync::Arc;

    use adk_graph::{Checkpointer, SqliteCheckpointer};
    use adk_harness::SealedCheckpointer;
    use adk_harness::encrypted::StoreKeyring;
    use adk_session::EncryptionKey;

    use super::checkpoint;

    const SENTINEL: &str = "plan-sentinel-7d1e";

    async fn pool(dir: &std::path::Path) -> sqlx::SqlitePool {
        let url = format!("sqlite://{}?mode=rwc", dir.join("harness.db").display());
        sqlx::SqlitePool::connect(&url).await.unwrap()
    }

    fn files_hold(dir: &std::path::Path, needle: &str) -> bool {
        std::fs::read_dir(dir).unwrap().any(|entry| {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap_or_default();
            bytes.windows(needle.len()).any(|window| window == needle.as_bytes())
        })
    }

    fn rich_checkpoint() -> adk_graph::Checkpoint {
        let mut checkpoint = checkpoint("plan-thread", 1, SENTINEL);
        checkpoint.metadata.insert("note".into(), SENTINEL.into());
        checkpoint.child_ledger.insert("child".into(), SENTINEL.into());
        checkpoint.cleared_interrupt = Some("step:1".into());
        checkpoint
    }

    #[tokio::test]
    async fn no_payload_reaches_the_file_and_every_field_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        let inner = Arc::new(SqliteCheckpointer::from_pool(pool.clone()).await.unwrap());
        let sealed = SealedCheckpointer::new(inner, StoreKeyring::generate());
        let original = rich_checkpoint();
        sealed.save(&original).await.unwrap();
        assert_eq!(sealed.load("plan-thread").await.unwrap(), Some(original.clone()));
        assert_eq!(
            sealed.load_by_id(&original.checkpoint_id).await.unwrap(),
            Some(original.clone())
        );
        assert_eq!(sealed.list("plan-thread").await.unwrap(), vec![original.clone()]);
        assert_eq!(sealed.list_page("plan-thread", None, 5).await.unwrap(), vec![original]);
        pool.close().await;
        assert!(!files_hold(dir.path(), SENTINEL), "a payload reached the database in plaintext");
    }

    #[tokio::test]
    async fn another_key_or_a_plaintext_row_fails_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        let inner = Arc::new(SqliteCheckpointer::from_pool(pool.clone()).await.unwrap());
        SealedCheckpointer::new(inner.clone(), StoreKeyring::generate())
            .save(&rich_checkpoint())
            .await
            .unwrap();
        let stranger = SealedCheckpointer::new(inner.clone(), StoreKeyring::generate());
        assert!(stranger.load("plan-thread").await.is_err(), "another key opens nothing");
        inner.save(&checkpoint("plain-thread", 1, "plaintext")).await.unwrap();
        let error = stranger.load("plain-thread").await.unwrap_err().to_string();
        assert!(error.contains("rotate-key"), "{error}");
    }

    #[tokio::test]
    async fn a_rotation_seals_plaintext_rows_and_reseals_the_rest_under_the_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        let inner = Arc::new(SqliteCheckpointer::from_pool(pool.clone()).await.unwrap());
        // Written before a store key was configured.
        let plaintext = rich_checkpoint();
        inner.save(&plaintext).await.unwrap();

        let first = StoreKeyring::generate();
        let sealed = SealedCheckpointer::new(inner.clone(), first.clone());
        assert_eq!(sealed.rewrap_sqlite(&pool).await.unwrap(), 1);
        assert_eq!(sealed.rewrap_sqlite(&pool).await.unwrap(), 0, "nothing left to seal");
        assert_eq!(sealed.load("plan-thread").await.unwrap(), Some(plaintext.clone()));

        let rotated = first.rotated(EncryptionKey::generate());
        let resealed = SealedCheckpointer::new(inner.clone(), rotated.clone());
        assert_eq!(resealed.rewrap_sqlite(&pool).await.unwrap(), 1);
        assert_eq!(resealed.load("plan-thread").await.unwrap(), Some(plaintext));
        let retired =
            SealedCheckpointer::new(inner, StoreKeyring::new(first.current_key(), vec![]));
        assert!(retired.load("plan-thread").await.is_err(), "the retired key opens nothing");
        pool.close().await;
        assert!(!files_hold(dir.path(), SENTINEL));
    }
}

/// Restarts over one SQLite file: the stores a restarted process reads, with plans and goal
/// graphs checkpointed through [`HarnessAgentBuilder::plan_checkpoint_store`].
#[cfg(all(feature = "sqlite", feature = "runner"))]
mod restart {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use adk_core::ApprovalStore;
    use adk_harness::{
        GoalEngine, GoalSpec, GoalStatus, Harness, HarnessAgent, HarnessDecision, HarnessMode,
        PlanStep, SqliteGoalStore, SqliteModeStore,
    };
    use adk_model::{ScriptedLlm, ScriptedResponse};
    use adk_runner::Runner;
    use adk_session::{SqliteApprovalStore, SqliteSessionService};
    use serde_json::json;

    use crate::common::{self, APP, collect, no_user_file, session_id, user};

    fn database(dir: &Path) -> String {
        format!("sqlite://{}?mode=rwc", dir.join("harness.db").display())
    }

    /// A harness process over `dir`'s database, checkpointing plans and goals in it.
    async fn process(
        dir: &Path,
        mode: HarnessMode,
        engine: GoalEngine,
        script: Vec<ScriptedResponse>,
        prompter: Option<Arc<common::ScriptedPrompter>>,
    ) -> Harness {
        let pool = sqlx::SqlitePool::connect(&database(dir)).await.unwrap();
        let sessions = SqliteSessionService::from_pool(pool.clone());
        sessions.migrate().await.unwrap();
        let approvals = SqliteApprovalStore::from_pool(pool.clone());
        approvals.migrate().await.unwrap();
        let modes = SqliteModeStore::new(pool.clone());
        modes.migrate().await.unwrap();
        let goals = SqliteGoalStore::new(pool.clone());
        goals.migrate().await.unwrap();
        let mut builder = HarnessAgent::builder("harness")
            .model(Arc::new(ScriptedLlm::new("scripted", script)))
            .workspace(adk_devtools::Workspace::new(dir.join("ws")).allow_bash(true))
            .project_instructions(no_user_file())
            .without_compaction()
            .without_default_members()
            .mode(mode)
            .mode_store(Arc::new(modes))
            .goal_store(Arc::new(goals))
            .goal_engine(engine)
            .approval_store(Arc::new(approvals) as Arc<dyn ApprovalStore>)
            .plan_checkpoint_store(&database(dir))
            .await
            .unwrap();
        if let Some(prompter) = prompter {
            builder = builder.prompter(prompter);
        }
        let agent = Arc::new(builder.build().unwrap());
        let runner = agent
            .configure_runner(
                Runner::builder()
                    .app_name(APP)
                    .agent(agent.clone())
                    .session_service(Arc::new(sessions)),
            )
            .build()
            .unwrap();
        Harness::with_runner(agent, runner).unwrap()
    }

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ws")).unwrap();
        dir
    }

    /// An approved plan runs after a restart: the second process finds it in the database
    /// and runs both steps in one turn. With plans in memory the restarted process runs a
    /// free-form turn and writes only the first file.
    #[tokio::test]
    async fn an_approved_plan_runs_after_a_restart() {
        let dir = workspace();
        let (user, session) = (user("alice"), session_id("s1"));
        let steps: Vec<PlanStep> = ["1", "2"]
            .iter()
            .enumerate()
            .map(|(index, id)| PlanStep {
                id: (*id).into(),
                title: format!("Step {id}"),
                instruction: format!("Write {id}.txt"),
                depends_on: if index == 0 { vec![] } else { vec!["1".into()] },
                gate: adk_harness::PlanGate::None,
            })
            .collect();
        {
            let prompter = common::ScriptedPrompter::new();
            prompter.approve_once();
            let planner = process(
                dir.path(),
                HarnessMode::Plan,
                GoalEngine::Loop,
                vec![
                    ScriptedResponse::function_call(
                        "exit_plan_mode",
                        json!({ "plan": "two files", "next_mode": "auto", "steps": steps }),
                    ),
                    ScriptedResponse::text("planned"),
                ],
                Some(prompter),
            )
            .await;
            planner.open_session(&user, Some(&session)).await.unwrap();
            collect(planner.send(&user, &session, "plan").await.unwrap()).await;
        }
        let write = |path: &str| {
            ScriptedResponse::function_call("write_file", json!({ "path": path, "content": path }))
        };
        let restarted = process(
            dir.path(),
            HarnessMode::Plan,
            GoalEngine::Loop,
            vec![
                write("1.txt"),
                ScriptedResponse::text("one"),
                write("2.txt"),
                ScriptedResponse::text("two"),
            ],
            None,
        )
        .await;
        collect(restarted.send(&user, &session, "go").await.unwrap()).await;
        assert!(dir.path().join("ws/1.txt").exists());
        assert!(dir.path().join("ws/2.txt").exists(), "the restarted process ran the plan graph");
        let plan = restarted
            .agent()
            .runtime()
            .plan_graph()
            .unwrap()
            .active(APP, user.as_ref(), session.as_ref())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(plan.status, adk_harness::PlanRunStatus::Done);
    }

    /// The `Graph` goal engine continues its checkpointed thread after a restart: the first
    /// process's checkpoints are in the database, and the goal finishes on the same thread.
    #[tokio::test]
    async fn the_goal_graph_continues_its_thread_after_a_restart() {
        let dir = workspace();
        let (user, session) = (user("alice"), session_id("s1"));
        let spec = GoalSpec {
            goal: "create done.txt".into(),
            until: "test -f done.txt".into(),
            max_iterations: 3,
            check_timeout: Duration::from_secs(30),
        };
        {
            // Ask with no prompter: the write is held and the graph stops at `iterate`.
            let first = process(
                dir.path(),
                HarnessMode::Ask,
                GoalEngine::Graph,
                vec![ScriptedResponse::function_call(
                    "write_file",
                    json!({ "path": "done.txt", "content": "ok" }),
                )],
                None,
            )
            .await;
            first.open_session(&user, Some(&session)).await.unwrap();
            collect(first.start_goal(&user, &session, spec).await.unwrap()).await;
            assert!(!dir.path().join("ws/done.txt").exists(), "the held write did not run");
        }
        let restarted = process(
            dir.path(),
            HarnessMode::Ask,
            GoalEngine::Graph,
            vec![ScriptedResponse::text("written")],
            None,
        )
        .await;
        let record = restarted
            .agent()
            .runtime()
            .goal_store()
            .get(APP, user.as_ref(), session.as_ref())
            .await
            .unwrap()
            .unwrap();
        let thread = format!(
            "{}:goal:{}",
            adk_graph::agent::session_thread_id(APP, user.as_ref(), session.as_ref()),
            record.run_id
        );
        let probe = adk_harness::checkpointer_from_url(&database(dir.path())).await.unwrap();
        let before = probe.list(&thread).await.unwrap();
        assert!(!before.is_empty(), "the first process checkpointed its goal graph");

        let pending = restarted.pending_approvals(&user, &session).await.unwrap();
        assert_eq!(pending.len(), 1, "{pending:#?}");
        restarted
            .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
            .await
            .unwrap();
        collect(restarted.resume(&user, &session).await.unwrap()).await;
        assert!(dir.path().join("ws/done.txt").exists());
        let state = restarted.goal(&user, &session).await.unwrap().unwrap();
        assert_eq!(state.status, GoalStatus::Done);
        let after = probe.list(&thread).await.unwrap();
        assert!(after.len() > before.len(), "the restarted process continued the same thread");
        assert!(
            after.iter().zip(&before).all(|(now, then)| now.checkpoint_id == then.checkpoint_id),
            "the first process's checkpoints are kept"
        );
    }
}
