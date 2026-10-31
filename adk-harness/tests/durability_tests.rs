//! Durability invariants (spec 3.1 §7.12, §9.2 I36–I37, H1) over one SQLite file.
//!
//! A crash is simulated by dropping the harness — runner, stores and connections — in the
//! middle of a run and opening a new one over the same file, as a restarted process would.

#![cfg(all(feature = "sqlite", feature = "graph", feature = "runner"))]

use crate::common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_core::{ActionLedger, ApprovalStore, DurableRunStore, Event, Part, RunState};
use adk_harness::{
    GoalSpec, GoalStatus, Harness, HarnessAgent, HarnessDecision, HarnessMode, PlanGate, PlanStep,
    SqliteGoalStore, SqliteModeStore,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, ListRequest, Session,
    SessionService, SqliteActionLedger, SqliteApprovalStore, SqliteRunStore, SqliteSessionService,
};
use common::*;
use serde_json::{Value, json};

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn text(text: &str) -> ScriptedResponse {
    ScriptedResponse::text(text)
}

/// Fails the first append of an event carrying a `bash` response, as a process that stops
/// after the command ran and before its result was recorded.
struct CrashAfterBash {
    inner: Arc<dyn SessionService>,
    armed: AtomicBool,
}

impl CrashAfterBash {
    fn crashes(&self, event: &Event) -> bool {
        let bash_response = event.llm_response.content.iter().flat_map(|c| c.parts.iter()).any(
            |part| matches!(part, Part::FunctionResponse { function_response, .. } if function_response.name == "bash"),
        );
        bash_response && self.armed.swap(false, Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl SessionService for CrashAfterBash {
    async fn create(&self, req: CreateRequest) -> adk_core::Result<Box<dyn Session>> {
        self.inner.create(req).await
    }
    async fn get(&self, req: GetRequest) -> adk_core::Result<Box<dyn Session>> {
        self.inner.get(req).await
    }
    async fn list(&self, req: ListRequest) -> adk_core::Result<Vec<Box<dyn Session>>> {
        self.inner.list(req).await
    }
    async fn delete(&self, req: DeleteRequest) -> adk_core::Result<()> {
        self.inner.delete(req).await
    }
    async fn append_event(&self, session_id: &str, event: Event) -> adk_core::Result<()> {
        if self.crashes(&event) {
            return Err(adk_core::AdkError::session("the process stopped"));
        }
        self.inner.append_event(session_id, event).await
    }
    async fn get_for_identity(
        &self,
        identity: &adk_core::AdkIdentity,
    ) -> adk_core::Result<Box<dyn Session>> {
        self.inner.get_for_identity(identity).await
    }
    async fn append_event_for_identity(&self, req: AppendEventRequest) -> adk_core::Result<()> {
        if self.crashes(&req.event) {
            return Err(adk_core::AdkError::session("the process stopped"));
        }
        self.inner.append_event_for_identity(req).await
    }
}

/// Everything a harness process keeps, on one SQLite file.
struct Process {
    harness: Harness,
    model: Arc<ScriptedLlm>,
    runs: Arc<SqliteRunStore>,
}

async fn open(
    dir: &Path,
    script: Vec<ScriptedResponse>,
    mode: HarnessMode,
    crash_after_bash: bool,
    configure: impl FnOnce(adk_harness::HarnessAgentBuilder) -> adk_harness::HarnessAgentBuilder,
) -> Process {
    let url = format!("sqlite://{}?mode=rwc", dir.join("harness.db").display());
    let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
    let sqlite_sessions = SqliteSessionService::from_pool(pool.clone());
    sqlite_sessions.migrate().await.unwrap();
    let sessions: Arc<dyn SessionService> = if crash_after_bash {
        Arc::new(CrashAfterBash { inner: Arc::new(sqlite_sessions), armed: AtomicBool::new(true) })
    } else {
        Arc::new(sqlite_sessions)
    };
    let approvals = SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await.unwrap();
    let ledger = SqliteActionLedger::from_pool(pool.clone());
    ledger.migrate().await.unwrap();
    let modes = SqliteModeStore::new(pool.clone());
    modes.migrate().await.unwrap();
    let goals = SqliteGoalStore::new(pool.clone());
    goals.migrate().await.unwrap();
    let runs = SqliteRunStore::from_pool(pool.clone());
    runs.migrate().await.unwrap();
    let checkpointer = adk_graph::SqliteCheckpointer::from_pool(pool.clone()).await.unwrap();
    let reads = adk_harness::SqliteReadRecords::new(pool.clone());
    reads.migrate().await.unwrap();
    let model = Arc::new(ScriptedLlm::new("scripted", script));
    let builder = HarnessAgent::builder("harness")
        .model(model.clone())
        .workspace(adk_devtools::Workspace::new(dir.join("ws")).allow_bash(true))
        .project_instructions(no_user_file())
        .without_compaction()
        .without_default_members()
        .mode(mode)
        .mode_store(Arc::new(modes))
        .goal_store(Arc::new(goals))
        .goal_engine(adk_harness::GoalEngine::Loop)
        .approval_store(Arc::new(approvals) as Arc<dyn ApprovalStore>)
        .plan_checkpointer(Arc::new(checkpointer))
        .read_records(Arc::new(reads));
    let agent = Arc::new(configure(builder).build().unwrap());
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions),
        )
        .action_ledger(Arc::new(ledger) as Arc<dyn ActionLedger>)
        .build()
        .unwrap();
    let runs = Arc::new(runs);
    let harness = Harness::with_runner(agent, runner)
        .unwrap()
        .with_run_store(runs.clone() as Arc<dyn DurableRunStore>);
    Process { harness, model, runs }
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("ws")).unwrap();
    dir
}

fn lines(dir: &Path, file: &str) -> usize {
    std::fs::read_to_string(dir.join("ws").join(file)).map(|text| text.lines().count()).unwrap_or(0)
}

#[tokio::test]
async fn a_completed_bash_never_runs_twice_after_a_crash() {
    // spec 3.1 §7.12, I37: the command ran and the process stopped before its result was
    // recorded. The next turn answers the call from the action ledger.
    let dir = workspace();
    let (user, session) = (user("alice"), session_id("s1"));
    {
        let p = open(
            dir.path(),
            vec![call("bash", json!({ "command": "echo ran >> ran.log" }))],
            HarnessMode::Auto,
            true,
            |b| b.allow_host_shell_in_auto(),
        )
        .await;
        p.harness.open_session(&user, Some(&session)).await.unwrap();
        let stream = p.harness.send(&user, &session, "run it").await.unwrap();
        let results: Vec<_> = futures::StreamExt::collect(stream).await;
        assert!(results.iter().any(Result::is_err), "the run stops with the process");
    }
    assert_eq!(lines(dir.path(), "ran.log"), 1);

    let p = open(dir.path(), vec![text("done")], HarnessMode::Auto, false, |b| {
        b.allow_host_shell_in_auto()
    })
    .await;
    let events = collect(p.harness.send(&user, &session, "continue").await.unwrap()).await;
    assert!(events.iter().any(|event| event.author == "harness"));
    assert_eq!(lines(dir.path(), "ran.log"), 1, "the completed command did not run again");
    let seen = serde_json::to_string(&p.model.requests()[0].contents).unwrap();
    assert!(seen.contains("already_succeeded"), "the model reads the ledger's answer: {seen}");
}

#[tokio::test]
async fn a_held_approval_survives_process_loss_and_is_answered_from_another_harness() {
    let dir = workspace();
    let (user, session) = (user("alice"), session_id("s1"));
    {
        let p = open(
            dir.path(),
            vec![call("write_file", json!({ "path": "notes.txt", "content": "x" })), text("held")],
            HarnessMode::Ask,
            false,
            |b| b,
        )
        .await;
        p.harness.open_session(&user, Some(&session)).await.unwrap();
        collect(p.harness.send(&user, &session, "write it").await.unwrap()).await;
    }
    assert!(!dir.path().join("ws/notes.txt").exists());

    let p = open(dir.path(), vec![text("written")], HarnessMode::Ask, false, |b| b).await;
    let pending = p.harness.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1);
    p.harness
        .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    collect(p.harness.resume(&user, &session).await.unwrap()).await;
    assert_eq!(std::fs::read_to_string(dir.path().join("ws/notes.txt")).unwrap(), "x");
}

#[tokio::test]
async fn a_resumed_run_is_governed_by_the_current_mode_not_the_one_it_parked_under() {
    let dir = workspace();
    let (user, session) = (user("alice"), session_id("s1"));
    {
        let p = open(
            dir.path(),
            vec![call("bash", json!({ "command": "echo ran >> ran.log" })), text("held")],
            HarnessMode::Ask,
            false,
            |b| b,
        )
        .await;
        p.harness.open_session(&user, Some(&session)).await.unwrap();
        collect(p.harness.send(&user, &session, "run it").await.unwrap()).await;
    }
    let p = open(dir.path(), vec![text("refused")], HarnessMode::Ask, false, |b| b).await;
    p.harness.set_mode(&user, &session, HarnessMode::ReadOnly).await.unwrap();
    let pending = p.harness.pending_approvals(&user, &session).await.unwrap();
    p.harness
        .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    let events = collect(p.harness.resume(&user, &session).await.unwrap()).await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied);
    assert_eq!(lines(dir.path(), "ran.log"), 0);
}

#[tokio::test]
async fn a_held_rm_in_ask_runs_zero_times_after_switching_to_auto() {
    // spec 3.1 §11: loosening cancels held calls, also across a restart.
    let dir = workspace();
    std::fs::write(dir.path().join("ws/victim.txt"), "keep").unwrap();
    let (user, session) = (user("alice"), session_id("s1"));
    {
        let p = open(
            dir.path(),
            vec![call("bash", json!({ "command": "rm victim.txt" })), text("held")],
            HarnessMode::Ask,
            false,
            |b| b,
        )
        .await;
        p.harness.open_session(&user, Some(&session)).await.unwrap();
        collect(p.harness.send(&user, &session, "clean up").await.unwrap()).await;
    }
    let p = open(dir.path(), vec![text("ok")], HarnessMode::Ask, false, |b| b).await;
    p.harness.set_mode(&user, &session, HarnessMode::Auto).await.unwrap();
    collect(p.harness.resume(&user, &session).await.unwrap()).await;
    assert!(dir.path().join("ws/victim.txt").exists());
}

fn step(id: &str, deps: &[&str], gate: PlanGate) -> PlanStep {
    PlanStep {
        id: id.into(),
        title: format!("Step {id}"),
        instruction: format!("Write {id}.txt"),
        depends_on: deps.iter().map(|dep| (*dep).to_string()).collect(),
        gate,
    }
}

#[tokio::test]
async fn a_plan_step_survives_process_loss() {
    let dir = workspace();
    let (user, session) = (user("alice"), session_id("s1"));
    let steps = vec![step("1", &[], PlanGate::None), step("2", &["1"], PlanGate::Approve)];
    {
        let prompter = ScriptedPrompter::new();
        prompter.approve_once();
        let p = open(
            dir.path(),
            vec![
                call(
                    "exit_plan_mode",
                    json!({ "plan": "two files", "next_mode": "auto", "steps": steps }),
                ),
                call("write_file", json!({ "path": "1.txt", "content": "1" })),
                text("step 1 done"),
            ],
            HarnessMode::Plan,
            false,
            move |b| b.prompter(prompter),
        )
        .await;
        p.harness.open_session(&user, Some(&session)).await.unwrap();
        collect(p.harness.send(&user, &session, "plan").await.unwrap()).await;
        collect(p.harness.send(&user, &session, "go").await.unwrap()).await;
    }
    assert!(dir.path().join("ws/1.txt").exists());
    std::fs::remove_file(dir.path().join("ws/1.txt")).unwrap();

    let p = open(
        dir.path(),
        vec![call("write_file", json!({ "path": "2.txt", "content": "2" })), text("step 2 done")],
        HarnessMode::Plan,
        false,
        |b| b,
    )
    .await;
    let pending = p.harness.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.tool_name, adk_harness::PLAN_GATE_TOOL);
    p.harness
        .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    collect(p.harness.resume(&user, &session).await.unwrap()).await;
    assert!(dir.path().join("ws/2.txt").exists());
    assert!(!dir.path().join("ws/1.txt").exists(), "step 1 did not run again");
}

async fn wait_for_state(runs: &SqliteRunStore, run_id: &str, state: RunState) {
    for _ in 0..400 {
        if runs.get(run_id).await.unwrap().is_some_and(|run| run.state == state) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("run {run_id} never reached {state:?}: {:?}", runs.get(run_id).await.unwrap());
}

fn worker(p: &Process, id: &str) -> adk_server::WorkerHandle {
    let (target, runner) = p.harness.goal_target();
    adk_server::DurableWorker::new(p.runs.clone() as Arc<dyn DurableRunStore>)
        .with_agent(target, runner)
        .with_config(
            adk_server::DurableWorkerConfig::default()
                .with_worker_id(id)
                .with_poll_interval(Duration::from_millis(25))
                .with_lease_ttl(Duration::from_secs(5)),
        )
        .spawn()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_durable_goal_survives_killing_the_process() {
    // spec 3.1 §5 H1: the goal parks on a held call; the process dies; a new process answers
    // the call, and the run resumes at the same iteration with the write executed once.
    let dir = workspace();
    let (user, session) = (user("alice"), session_id("s1"));
    let spec = GoalSpec {
        goal: "create done.txt".into(),
        until: "test -f done.txt".into(),
        max_iterations: 3,
        check_timeout: Duration::from_secs(30),
    };
    let run_id = {
        let p = open(
            dir.path(),
            vec![
                call("write_file", json!({ "path": "done.txt", "content": "done" })),
                text("held"),
            ],
            HarnessMode::Ask,
            false,
            |b| b,
        )
        .await;
        p.harness.open_session(&user, Some(&session)).await.unwrap();
        let events =
            collect(p.harness.start_goal(&user, &session, spec.clone()).await.unwrap()).await;
        let run_id = events[0].provider_metadata["harness.goal.run_id"].clone();
        let handle = worker(&p, "first");
        wait_for_state(&p.runs, &run_id, RunState::Waiting).await;
        handle.abort();
        run_id
    };
    assert!(!dir.path().join("ws/done.txt").exists());

    let p = open(dir.path(), vec![text("written")], HarnessMode::Ask, false, |b| b).await;
    // Two processes now share the store; fenced leases let only one work the run.
    let second = worker(&p, "second");
    let third = worker(&p, "third");
    let pending = p.harness.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1);
    p.harness
        .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    wait_for_state(&p.runs, &run_id, RunState::Completed).await;
    second.shutdown().await;
    third.shutdown().await;

    assert_eq!(std::fs::read_to_string(dir.path().join("ws/done.txt")).unwrap(), "done");
    let state = p.harness.goal(&user, &session).await.unwrap().unwrap();
    assert_eq!((state.status, state.iteration), (GoalStatus::Done, 1));
    // The same goal on the same session is the same durable run.
    let again = p.harness.start_goal(&user, &session, spec).await.err().unwrap();
    assert_eq!(again.code, "harness.goal.duplicate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_edit_parked_by_a_goal_runs_once_when_approved_from_a_new_process() {
    // spec 3.1 §7.33 fix 5: the goal reads a file and parks on the held edit; the process
    // dies; a new process approves it. The read record persisted with the session, so the
    // edit runs, once, instead of failing as never read.
    let dir = workspace();
    std::fs::write(dir.path().join("ws/notes.txt"), "status: draft\n").unwrap();
    let (user, session) = (user("alice"), session_id("s1"));
    let spec = GoalSpec {
        goal: "mark the notes done".into(),
        until: "grep -q 'status: done' notes.txt".into(),
        max_iterations: 3,
        check_timeout: Duration::from_secs(30),
    };
    let run_id = {
        let p = open(
            dir.path(),
            vec![
                call("read_file", json!({ "path": "notes.txt" })),
                call(
                    "edit_file",
                    json!({ "path": "notes.txt", "old_string": "draft", "new_string": "done" }),
                ),
                text("held"),
            ],
            HarnessMode::Ask,
            false,
            |b| b,
        )
        .await;
        p.harness.open_session(&user, Some(&session)).await.unwrap();
        let events =
            collect(p.harness.start_goal(&user, &session, spec.clone()).await.unwrap()).await;
        let run_id = events[0].provider_metadata["harness.goal.run_id"].clone();
        let handle = worker(&p, "first");
        wait_for_state(&p.runs, &run_id, RunState::Waiting).await;
        handle.abort();
        run_id
    };
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ws/notes.txt")).unwrap(),
        "status: draft\n"
    );

    let p = open(dir.path(), vec![text("edited")], HarnessMode::Ask, false, |b| b).await;
    let second = worker(&p, "second");
    let pending = p.harness.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.tool_name, "edit_file");
    p.harness
        .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    wait_for_state(&p.runs, &run_id, RunState::Completed).await;
    second.shutdown().await;

    assert_eq!(std::fs::read_to_string(dir.path().join("ws/notes.txt")).unwrap(), "status: done\n");
    let seen = serde_json::to_string(&p.model.requests()[0].contents).unwrap();
    assert!(seen.contains("Replaced 1 occurrence(s) in notes.txt"), "the edit ran: {seen}");
    assert!(!seen.contains("devtools.not_read"), "{seen}");
    let state = p.harness.goal(&user, &session).await.unwrap().unwrap();
    assert_eq!((state.status, state.iteration), (GoalStatus::Done, 1));
}
