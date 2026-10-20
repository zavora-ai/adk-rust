//! Team, plan-graph and durable-goal scenarios (spec 3.1 §5 H1, H3, H4; §7.10–§7.12).
//!
//! The 3.0 `CodingAgent` has no members, no plan graph and no goal engine, so each scripted
//! scenario's baseline column states the missing machinery rather than running a half-equivalent.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{ActionLedger, ApprovalStore, DurableRunStore, Llm, RunState, SessionId, UserId};
use adk_harness::{
    GoalSpec, GoalStatus, Harness, HarnessAgent, HarnessDecision, HarnessMode, MemberSpec, PlanGate, PlanRunStatus,
    PlanStep, SqliteGoalStore, SqliteModeStore,
};
use adk_model::ScriptedResponse;
use adk_runner::Runner;
use adk_session::{SessionService, SqliteActionLedger, SqliteApprovalStore, SqliteRunStore, SqliteSessionService};
use anyhow::{Result, bail};
use serde_json::json;

use crate::{APP, Checks, Env, Outcome, Subject, call, done, outcome, responses, scripted};

pub const SCRIPTED: &[&str] = &[
    "team_budget_stops_run",
    "acp_member_governed",
    "plan_graph_approve_gate",
    "plan_graph_rewind",
    "lost_result_answered_from_ledger",
    "goal_resumes_after_crash",
];

pub const LIVE: &[&str] = &["live_team_review", "live_plan_graph", "live_durable_goal"];

/// Runs scripted scenario `name`, if it is one of these.
pub async fn run_scripted(name: &str, subject: Subject) -> Option<Result<Checks>> {
    if name == "lost_result_answered_from_ledger" {
        return Some(lost_result_answered_from_ledger(subject).await);
    }
    if subject == Subject::Baseline {
        let missing = match name {
            "team_budget_stops_run" | "acp_member_governed" => "the 3.0 CodingAgent has no team members",
            "plan_graph_approve_gate" | "plan_graph_rewind" => "the 3.0 CodingAgent has no plan mode or plan graph",
            "goal_resumes_after_crash" => "the 3.0 CodingAgent has no goal engine",
            _ => return None,
        };
        let mut checks = Checks::default();
        checks.check(missing, false);
        return Some(Ok(checks));
    }
    Some(match name {
        "team_budget_stops_run" => team_budget_stops_run().await,
        "acp_member_governed" => acp_member_governed().await,
        "plan_graph_approve_gate" => plan_graph_approve_gate().await,
        "plan_graph_rewind" => plan_graph_rewind().await,
        "goal_resumes_after_crash" => goal_resumes_after_crash().await,
        _ => return None,
    })
}

fn text(text: &str) -> ScriptedResponse {
    ScriptedResponse::text(text)
}

fn records(env: &Env, action: &str) -> Vec<adk_core::audit_log::AuditRecordDraft> {
    env.audit.records().into_iter().filter(|record| record.action == action).collect()
}

fn ids() -> Result<(UserId, SessionId)> {
    Ok((UserId::new("alice")?, SessionId::new("s1")?))
}

// --- H3: team -------------------------------------------------------------------------------

async fn team_budget_stops_run() -> Result<Checks> {
    let mut checks = Checks::default();
    let members: Arc<dyn Llm> = scripted(vec![text("one finding"), text("another")]);
    let model = scripted(vec![
        call("explore", json!({ "request": "find the entry point" })),
        call("explore", json!({ "request": "find the tests" })),
        done(),
    ]);
    let env = Env::new(Subject::Harness, model, move |b| {
        b.member_model(members).team_policy(adk_agent::TeamPolicy {
            budget: adk_agent::TeamBudget { max_delegations: Some(1), ..Default::default() },
            ..Default::default()
        })
    })
    .await?;
    env.session("alice", "s1").await?;
    let events = env.send("alice", "s1", "look around twice").await?;
    let spawned = records(&env, "harness.member.spawned");
    checks.check("one delegation ran, in an isolated member", spawned.len() == 1);
    checks.check("the delegation is audited with its parent and member", spawned.first().is_some_and(|r| {
        r.ext["parentAgentId"].as_str().is_some_and(|id| id.ends_with("/harness")) && r.ext["member"] == "explore"
    }));
    let budget_hit = env.audit.records().iter().any(|r| r.ext.values().any(|v| v.to_string().contains("budget")))
        || events.iter().any(|e| e.llm_response.error_code.as_deref().is_some_and(|code| code.contains("budget")));
    checks.check("the team budget stopped the second delegation", budget_hit || records(&env, "harness.member.spawned").len() == 1);
    let lowered = env.harness.as_ref().and_then(|h| h.agent().runtime().team().map(|t| t.spec().clone()));
    checks.check(
        "members lowered to one TeamSpec with isolated, timed edges",
        lowered.is_some_and(|spec| {
            spec.policy.max_delegation_depth == 1
                && spec.relationships.iter().all(|edge| {
                    edge.policy.context == Some(adk_agent::TeamContextPolicy::Isolated) && edge.policy.timeout_ms.is_some()
                })
        }),
    );
    Ok(checks)
}

/// A scripted ACP-compatible coding agent: asks to read, then to edit `REVIEW.md`, writing it only
/// when allowed.
const SCRIPTED_ACP_AGENT: &str = r#"
import json, os, sys
next_id = 900
def send(m):
    sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
def read():
    while True:
        line = sys.stdin.readline()
        if not line: sys.exit(0)
        if line.strip(): return json.loads(line)
def ask(kind, title, raw):
    global next_id
    next_id += 1; rid = next_id
    send({"jsonrpc": "2.0", "id": rid, "method": "session/request_permission", "params": {"sessionId": "s1",
          "toolCall": {"toolCallId": f"t{rid}", "title": title, "kind": kind, "rawInput": raw},
          "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                      {"optionId": "reject", "name": "Reject", "kind": "reject_once"}]}})
    while True:
        m = read()
        if m.get("id") == rid and "method" not in m:
            o = (m.get("result") or {}).get("outcome") or {}
            return o.get("outcome") == "selected" and o.get("optionId") == "allow"
        handle(m)
def handle(m):
    method, rid = m.get("method"), m.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}})
    elif method == "session/new":
        os.chdir((m.get("params") or {}).get("cwd") or ".")
        send({"jsonrpc": "2.0", "id": rid, "result": {"sessionId": "s1"}})
    elif method == "session/prompt":
        r = ask("read", "Read src/lib.rs", {"path": "src/lib.rs"})
        e = ask("edit", "Write REVIEW.md", {"path": "REVIEW.md"})
        if e: open("REVIEW.md", "w").write("reviewed\n")
        t = f"read: {'allowed' if r else 'denied'}; edit: {'written' if e else 'denied'}"
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s1", "update": {
              "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": t}}}})
        send({"jsonrpc": "2.0", "id": rid, "result": {"stopReason": "end_turn"}})
    elif method == "session/close":
        send({"jsonrpc": "2.0", "id": rid, "result": {}})
    elif rid is not None and method is not None:
        send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "method not found"}})
while True:
    handle(read())
"#;

async fn acp_member_governed() -> Result<Checks> {
    let mut checks = Checks::default();
    let scripts = tempfile::tempdir()?;
    let agent = scripts.path().join("acp_agent.py");
    std::fs::write(&agent, SCRIPTED_ACP_AGENT)?;
    let reviewer = MemberSpec::reviewer_acp(&format!("python3 {}", agent.display()));
    for (mode, read, edit) in [(HarnessMode::Plan, "allowed", "denied"), (HarnessMode::Ask, "allowed", "written")] {
        let reviewer = reviewer.clone();
        let env = Env::new(
            Subject::Harness,
            scripted(vec![call("reviewer", json!({ "request": "review the change" })), done()]),
            move |b| b.mode(mode).without_default_members().member(reviewer),
        )
        .await?;
        env.session("alice", "s1").await?;
        let events = env.send("alice", "s1", "review it").await?;
        let answer = responses(&events).into_iter().find(|(name, _)| name == "reviewer").map(|(_, r)| r.to_string()).unwrap_or_default();
        checks.check(format!("{mode}: the ACP member's read is {read}"), answer.contains(&format!("read: {read}")));
        checks.check(format!("{mode}: the ACP member's edit is {edit}"), answer.contains(&format!("edit: {edit}")));
        checks.check(format!("{mode}: REVIEW.md written only when allowed"), env.path("REVIEW.md").exists() == (edit == "written"));
        checks.check(format!("{mode}: the delegation is audited as an ACP member"), records(&env, "harness.member.spawned").first().is_some_and(|r| r.ext["kind"] == "acp"));
    }
    Ok(checks)
}

// --- H4: plan graph ---------------------------------------------------------------------------

fn step(id: &str, deps: &[&str], gate: PlanGate) -> PlanStep {
    PlanStep {
        id: id.into(),
        title: format!("Step {id}"),
        instruction: format!("Write {id}.txt"),
        depends_on: deps.iter().map(|dep| (*dep).to_string()).collect(),
        gate,
    }
}

fn write(path: &str) -> ScriptedResponse {
    call("write_file", json!({ "path": path, "content": path }))
}

fn plan_call(steps: &[PlanStep]) -> ScriptedResponse {
    call("exit_plan_mode", json!({ "plan": "Write three files.", "next_mode": "auto", "steps": steps }))
}

async fn plan_graph_approve_gate() -> Result<Checks> {
    let mut checks = Checks::default();
    let steps = vec![step("1", &[], PlanGate::None), step("2", &["1"], PlanGate::None), step("3", &["2"], PlanGate::Approve)];
    let model = scripted(vec![plan_call(&steps), write("1.txt"), text("1"), write("2.txt"), text("2"), write("3.txt"), text("3")]);
    let env = Env::new(Subject::Harness, model, |b| b.mode(HarnessMode::Plan).without_default_members()).await?;
    env.session("alice", "s1").await?;
    env.send("alice", "s1", "plan three files").await?;
    checks.check("the approved plan compiled to a graph", records(&env, "harness.plan.compiled").len() == 1);
    let events = env.send("alice", "s1", "go").await?;
    checks.check("steps 1 and 2 ran; step 3 waits", env.path("2.txt").exists() && !env.path("3.txt").exists());
    checks.check("the gate is a held call", outcome(&events, adk_harness::PLAN_GATE_TOOL) == Outcome::Held);
    let harness = env.harness.as_ref().unwrap();
    let (user, session) = ids()?;
    let pending = harness.pending_approvals(&user, &session).await?;
    checks.check("one gate pending, bound to the plan hash and step", pending.len() == 1 && pending[0].request.args["stepId"] == "3");
    if let Some(gate) = pending.first() {
        harness.decide(&user, &session, &gate.fingerprint, HarnessDecision::Approve).await?;
    }
    env.send("alice", "s1", "").await?;
    checks.check("after Harness::decide, step 3 ran", env.path("3.txt").exists());
    let plan = harness.agent().runtime().plan_graph().unwrap().active(APP, "alice", "s1").await?;
    checks.check("the plan is done", plan.is_some_and(|plan| plan.status == PlanRunStatus::Done));
    Ok(checks)
}

async fn plan_graph_rewind() -> Result<Checks> {
    let mut checks = Checks::default();
    let steps = vec![step("1", &[], PlanGate::None), step("2", &["1"], PlanGate::None), step("3", &["2"], PlanGate::None)];
    let model = Arc::new(adk_model::ScriptedLlm::new(
        "scripted",
        vec![
            plan_call(&steps),
            write("1.txt"),
            text("1"),
            write("2.txt"),
            text("2"),
            ScriptedResponse::error(adk_core::AdkError::model("the provider is down")),
        ],
    ));
    let env = Env::new(Subject::Harness, model.clone(), |b| b.mode(HarnessMode::Plan).without_default_members()).await?;
    env.session("alice", "s1").await?;
    env.send("alice", "s1", "plan three files").await?;
    env.send("alice", "s1", "go").await?;
    checks.check("step 3 failed after steps 1 and 2", env.path("2.txt").exists() && !env.path("3.txt").exists());
    let graphs = env.harness.as_ref().unwrap().agent().runtime().plan_graph().unwrap().clone();
    let thread = graphs.rewind(APP, "alice", "s1", "2").await?;
    checks.check("rewound to a fork after step 2", thread.ends_with(":rewind:1"));
    std::fs::remove_file(env.path("1.txt"))?;
    std::fs::remove_file(env.path("2.txt"))?;
    model.push(write("3.txt"));
    model.push(text("3"));
    env.send("alice", "s1", "again").await?;
    checks.check("step 3 ran on the fork", env.path("3.txt").exists());
    checks.check("steps 1 and 2 did not run again", !env.path("1.txt").exists() && !env.path("2.txt").exists());
    Ok(checks)
}

// --- H1: durability over one SQLite file ------------------------------------------------------

/// A harness process over `dir/harness.db`.
pub struct Process {
    pub harness: Harness,
    pub runs: Arc<SqliteRunStore>,
}

/// Fails the first append of a `bash` result, as a process that stops after the command ran and
/// before its result was recorded.
struct CrashAfterBash {
    inner: Arc<dyn SessionService>,
    armed: std::sync::atomic::AtomicBool,
}

impl CrashAfterBash {
    fn crashes(&self, event: &adk_core::Event) -> bool {
        let result = event.llm_response.content.iter().flat_map(|c| c.parts.iter()).any(
            |part| matches!(part, adk_core::Part::FunctionResponse { function_response, .. } if function_response.name == "bash"),
        );
        result && self.armed.swap(false, std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl SessionService for CrashAfterBash {
    async fn create(&self, req: adk_session::CreateRequest) -> adk_core::Result<Box<dyn adk_session::Session>> {
        self.inner.create(req).await
    }
    async fn get(&self, req: adk_session::GetRequest) -> adk_core::Result<Box<dyn adk_session::Session>> {
        self.inner.get(req).await
    }
    async fn list(&self, req: adk_session::ListRequest) -> adk_core::Result<Vec<Box<dyn adk_session::Session>>> {
        self.inner.list(req).await
    }
    async fn delete(&self, req: adk_session::DeleteRequest) -> adk_core::Result<()> {
        self.inner.delete(req).await
    }
    async fn append_event(&self, session_id: &str, event: adk_core::Event) -> adk_core::Result<()> {
        if self.crashes(&event) {
            return Err(adk_core::AdkError::session("the process stopped"));
        }
        self.inner.append_event(session_id, event).await
    }
    async fn get_for_identity(&self, identity: &adk_core::AdkIdentity) -> adk_core::Result<Box<dyn adk_session::Session>> {
        self.inner.get_for_identity(identity).await
    }
    async fn append_event_for_identity(&self, req: adk_session::AppendEventRequest) -> adk_core::Result<()> {
        if self.crashes(&req.event) {
            return Err(adk_core::AdkError::session("the process stopped"));
        }
        self.inner.append_event_for_identity(req).await
    }
}

pub async fn open(
    dir: &Path,
    model: Arc<dyn Llm>,
    mode: HarnessMode,
    configure: impl FnOnce(adk_harness::HarnessAgentBuilder) -> adk_harness::HarnessAgentBuilder,
) -> Result<Process> {
    open_with(dir, model, mode, false, configure).await
}

pub async fn open_with(
    dir: &Path,
    model: Arc<dyn Llm>,
    mode: HarnessMode,
    crash_after_bash: bool,
    configure: impl FnOnce(adk_harness::HarnessAgentBuilder) -> adk_harness::HarnessAgentBuilder,
) -> Result<Process> {
    let url = format!("sqlite://{}?mode=rwc", dir.join("harness.db").display());
    let pool = sqlx::SqlitePool::connect(&url).await?;
    let sessions = SqliteSessionService::from_pool(pool.clone());
    sessions.migrate().await?;
    let approvals = SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await?;
    let ledger = SqliteActionLedger::from_pool(pool.clone());
    ledger.migrate().await?;
    let modes = SqliteModeStore::new(pool.clone());
    modes.migrate().await?;
    let goals = SqliteGoalStore::new(pool.clone());
    goals.migrate().await?;
    let runs = SqliteRunStore::from_pool(pool.clone());
    runs.migrate().await?;
    let checkpointer = adk_graph::SqliteCheckpointer::from_pool(pool.clone()).await?;
    let builder = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.join("ws")).allow_bash(true))
        .project_instructions(crate::no_user_file())
        .without_compaction()
        .mode(mode)
        .mode_store(Arc::new(modes))
        .goal_store(Arc::new(goals))
        .goal_engine(adk_harness::GoalEngine::Loop)
        .approval_store(Arc::new(approvals) as Arc<dyn ApprovalStore>)
        .plan_checkpointer(Arc::new(checkpointer));
    let agent = Arc::new(configure(builder).build()?);
    let sessions: Arc<dyn SessionService> = if crash_after_bash {
        Arc::new(CrashAfterBash { inner: Arc::new(sessions), armed: std::sync::atomic::AtomicBool::new(true) })
    } else {
        Arc::new(sessions)
    };
    let runner = agent
        .configure_runner(Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions))
        .action_ledger(Arc::new(ledger) as Arc<dyn ActionLedger>)
        .build()?;
    let runs = Arc::new(runs);
    let harness = Harness::with_runner(agent, runner)?.with_run_store(runs.clone() as Arc<dyn DurableRunStore>);
    Ok(Process { harness, runs })
}

pub fn worker(process: &Process, id: &str) -> adk_server::WorkerHandle {
    let (target, runner) = process.harness.goal_target();
    adk_server::DurableWorker::new(process.runs.clone() as Arc<dyn DurableRunStore>)
        .with_agent(target, runner)
        .with_config(
            adk_server::DurableWorkerConfig::default()
                .with_worker_id(id)
                .with_poll_interval(Duration::from_millis(50))
                .with_lease_ttl(Duration::from_secs(5)),
        )
        .spawn()
}

pub async fn wait_for(runs: &SqliteRunStore, run_id: &str, states: &[RunState], timeout: Duration) -> Result<RunState> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(run) = runs.get(run_id).await?
            && states.contains(&run.state)
        {
            return Ok(run.state);
        }
        if tokio::time::Instant::now() > deadline {
            bail!("run {run_id} did not reach {states:?}: {:?}", runs.get(run_id).await?.map(|run| run.state));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn lines(dir: &Path, file: &str) -> usize {
    std::fs::read_to_string(dir.join("ws").join(file)).map(|text| text.lines().count()).unwrap_or(0)
}

async fn collect(stream: adk_core::EventStream) -> Vec<adk_core::Event> {
    use futures::StreamExt;
    stream.filter_map(|result| async move { result.ok() }).collect().await
}

/// A 3.0 `CodingAgent` runner over the same SQLite stores, for the baseline column.
async fn baseline_runner(dir: &Path, model: Arc<dyn Llm>, crash_after_bash: bool) -> Result<Runner> {
    let url = format!("sqlite://{}?mode=rwc", dir.join("harness.db").display());
    let pool = sqlx::SqlitePool::connect(&url).await?;
    let sessions = SqliteSessionService::from_pool(pool.clone());
    sessions.migrate().await?;
    let ledger = SqliteActionLedger::from_pool(pool);
    ledger.migrate().await?;
    let sessions: Arc<dyn SessionService> = if crash_after_bash {
        Arc::new(CrashAfterBash { inner: Arc::new(sessions), armed: std::sync::atomic::AtomicBool::new(true) })
    } else {
        Arc::new(sessions)
    };
    let coding = adk_agent::coding::CodingAgent::builder()
        .name("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.join("ws")).allow_bash(true))
        .build()?;
    Ok(Runner::builder()
        .app_name(APP)
        .agent(coding.agent())
        .session_service(sessions)
        .action_ledger(Arc::new(ledger) as Arc<dyn ActionLedger>)
        .build()?)
}

async fn lost_result_answered_from_ledger(subject: Subject) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("ws"))?;
    let (user, session) = ids()?;
    let first = scripted(vec![call("bash", json!({ "command": "echo ran >> ran.log" })), done()]);
    let after = Arc::new(adk_model::ScriptedLlm::new("scripted", vec![done()]));
    match subject {
        Subject::Harness => {
            {
                // The command runs; the process stops before its result is recorded.
                let process = open_with(dir.path(), first, HarnessMode::Auto, true, |b| b.allow_host_shell_in_auto().without_default_members()).await?;
                process.harness.open_session(&user, Some(&session)).await?;
                collect(process.harness.send(&user, &session, "run it").await?).await;
            }
            checks.check("the command ran once before the crash", lines(dir.path(), "ran.log") == 1);
            let process = open(dir.path(), after.clone(), HarnessMode::Auto, |b| b.allow_host_shell_in_auto().without_default_members()).await?;
            collect(process.harness.send(&user, &session, "continue").await?).await;
        }
        Subject::Baseline => {
            {
                let runner = baseline_runner(dir.path(), first, true).await?;
                runner
                    .session_service()
                    .create(adk_session::CreateRequest { app_name: APP.into(), user_id: user.to_string(), session_id: Some(session.to_string()), state: Default::default() })
                    .await?;
                collect(runner.run(user.clone(), session.clone(), adk_core::Content::new("user").with_text("run it")).await?).await;
            }
            checks.check("the command ran once before the crash", lines(dir.path(), "ran.log") == 1);
            let runner = baseline_runner(dir.path(), after.clone(), false).await?;
            collect(runner.run(user.clone(), session.clone(), adk_core::Content::new("user").with_text("continue")).await?).await;
        }
    }
    checks.check("after the restart the command did not run again", lines(dir.path(), "ran.log") == 1);
    let seen = after.requests().first().map(|request| serde_json::to_string(&request.contents).unwrap_or_default()).unwrap_or_default();
    checks.check("the next turn's model reads the ledger's answer for the interrupted call", seen.contains("already_succeeded"));
    Ok(checks)
}

async fn goal_resumes_after_crash() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("ws"))?;
    let (user, session) = ids()?;
    let spec = GoalSpec { goal: "create done.txt".into(), until: "test -f done.txt".into(), max_iterations: 3, check_timeout: Duration::from_secs(30) };
    let run_id = {
        let process = open(dir.path(), scripted(vec![call("write_file", json!({ "path": "done.txt", "content": "done" })), done()]), HarnessMode::Ask, |b| b.without_default_members()).await?;
        process.harness.open_session(&user, Some(&session)).await?;
        let events = collect(process.harness.start_goal(&user, &session, spec).await?).await;
        let run_id = events.first().and_then(|event| event.provider_metadata.get("harness.goal.run_id").cloned()).unwrap_or_default();
        let handle = worker(&process, "first");
        let parked = wait_for(&process.runs, &run_id, &[RunState::Waiting], Duration::from_secs(20)).await;
        checks.check("the goal's write is held and its run parked", parked.is_ok() && !dir.path().join("ws/done.txt").exists());
        handle.abort();
        run_id
    };
    let process = open(dir.path(), scripted(vec![done()]), HarnessMode::Ask, |b| b.without_default_members()).await?;
    let second = worker(&process, "second");
    let pending = process.harness.pending_approvals(&user, &session).await?;
    checks.check("the held call survived the crash", pending.len() == 1);
    if let Some(call) = pending.first() {
        process.harness.decide(&user, &session, &call.fingerprint, HarnessDecision::Approve).await?;
    }
    let finished = wait_for(&process.runs, &run_id, &[RunState::Completed, RunState::Failed], Duration::from_secs(20)).await;
    second.shutdown().await;
    checks.check("the run completed in the new process", matches!(finished, Ok(RunState::Completed)));
    let state = process.harness.goal(&user, &session).await?;
    checks.check("the goal is done at iteration 1", state.is_some_and(|state| state.status == GoalStatus::Done && state.iteration == 1));
    checks.check("the write happened once", std::fs::read_to_string(dir.path().join("ws/done.txt")).unwrap_or_default() == "done");
    Ok(checks)
}

// --- Live -------------------------------------------------------------------------------------

/// The command that starts an installed ACP-compatible coding agent, from `HARNESS_ACP_CMD`.
fn acp_command() -> Option<String> {
    std::env::var("HARNESS_ACP_CMD").ok().filter(|command| !command.trim().is_empty())
}

pub async fn run_live(name: &str, model: Arc<dyn Llm>) -> Option<Result<Checks>> {
    Some(match name {
        "live_team_review" => live_team_review(model).await,
        "live_plan_graph" => live_plan_graph(model).await,
        "live_durable_goal" => live_durable_goal(model).await,
        _ => return None,
    })
}

async fn live_team_review(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    crate::write_crate(dir.path())?;
    let Some(command) = acp_command() else {
        bail!("set HARNESS_ACP_CMD to the command of an installed ACP-compatible coding agent");
    };
    let reviewer = MemberSpec::reviewer_acp(&command);
    let env = crate::Env::in_dir(
        Subject::Harness,
        dir,
        model,
        move |b| b.mode(HarnessMode::Auto).allow_host_shell_in_auto().max_iterations(30).member(reviewer),
        Vec::new(),
        true,
    )
    .await?;
    env.session("alice", "s1").await?;
    let (user, session) = ids()?;
    let stream = env
        .harness
        .as_ref()
        .unwrap()
        .send(
            &user,
            &session,
            "The test in src/lib.rs fails. First delegate to the `explore` member to find the bug. Then fix it and run \
             `cargo test --quiet` with bash. Then delegate to the `reviewer` member to review the change in src/lib.rs. \
             Finish with a two-line summary.",
        )
        .await?;
    // A team turn makes several delegations; slow providers need more than one turn's deadline.
    let events = tokio::time::timeout(Duration::from_secs(1200), collect(stream)).await?;
    let delegations: Vec<String> = records(&env, "harness.member.spawned").iter().map(|r| format!("{}:{}", r.ext["member"], r.ext["kind"])).collect();
    println!("      delegations: {delegations:?}");
    let calls: Vec<String> = responses(&events).into_iter().map(|(name, _)| name).collect();
    println!("      tool calls: {calls:?}");
    checks.check("the explore member ran as an isolated local delegation", delegations.iter().any(|d| d == "\"explore\":\"local\""));
    checks.check("the reviewer ran as an ACP member", delegations.iter().any(|d| d == "\"reviewer\":\"acp\""));
    let review = responses(&events).into_iter().find(|(name, _)| name == "reviewer").map(|(_, response)| response.to_string()).unwrap_or_default();
    println!("      review: {}", &review[..review.len().min(300)]);
    checks.check(
        "the ACP reviewer's delegation completed",
        records(&env, "harness.member.finished").iter().any(|r| r.ext["member"] == "reviewer" && r.ext["outcome"] == "completed"),
    );
    let fixed = std::fs::read_to_string(env.path("src/lib.rs")).unwrap_or_default().contains("a + b");
    checks.check("the bug is fixed", fixed);
    checks.check("one audit trail records each delegation's end", records(&env, "harness.member.finished").len() == delegations.len());
    Ok(checks)
}

async fn live_plan_graph(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let env = Env::new(Subject::Harness, model, |b| b.mode(HarnessMode::Plan).without_default_members().max_iterations(15)).await?;
    env.session("alice", "s1").await?;
    env.send(
        "alice",
        "s1",
        "Plan creating three files in the workspace: a.txt containing `alpha`, b.txt containing `beta`, c.txt \
         containing `gamma`, one file per step. Then call exit_plan_mode with next_mode \"auto\" and exactly three \
         steps: {\"id\":\"1\",\"title\":\"a.txt\",\"instruction\":\"Create a.txt containing alpha\"}, \
         {\"id\":\"2\",\"title\":\"b.txt\",\"instruction\":\"Create b.txt containing beta\",\"depends_on\":[\"1\"]}, \
         {\"id\":\"3\",\"title\":\"c.txt\",\"instruction\":\"Create c.txt containing gamma\",\"depends_on\":[\"2\"],\"gate\":\"approve\"}.",
    )
    .await?;
    let compiled = records(&env, "harness.plan.compiled");
    checks.check("the approved plan compiled with an Approve gate on step 3", compiled.first().is_some_and(|r| r.ext["interruptBefore"] == json!(["step:3"])));
    env.send("alice", "s1", "Go ahead.").await?;
    checks.check("steps 1 and 2 ran; step 3 waits at its gate", env.path("a.txt").exists() && env.path("b.txt").exists() && !env.path("c.txt").exists());
    let harness = env.harness.as_ref().unwrap();
    let (user, session) = ids()?;
    let pending = harness.pending_approvals(&user, &session).await?;
    checks.check("the gate is pending", pending.len() == 1 && pending[0].request.tool_name == adk_harness::PLAN_GATE_TOOL);
    if let Some(gate) = pending.first() {
        harness.decide(&user, &session, &gate.fingerprint, HarnessDecision::Approve).await?;
    }
    env.send("alice", "s1", "").await?;
    checks.check("after Harness::decide, step 3 ran", env.path("c.txt").exists());
    let plan = harness.agent().runtime().plan_graph().unwrap().active(APP, "alice", "s1").await?;
    checks.check("the plan is done", plan.is_some_and(|plan| plan.status == PlanRunStatus::Done));
    Ok(checks)
}

async fn live_durable_goal(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    crate::write_crate(&dir.path().join("ws"))?;
    let (user, session) = ids()?;
    let spec = GoalSpec {
        goal: "Make the failing test in src/lib.rs pass. Fix the code with edit_file; do not change the test.".into(),
        until: "cargo test --quiet".into(),
        max_iterations: 3,
        check_timeout: Duration::from_secs(300),
    };
    let run_id = {
        let process = open(dir.path(), model.clone(), HarnessMode::Ask, |b| b.without_default_members().max_iterations(15)).await?;
        process.harness.open_session(&user, Some(&session)).await?;
        let events = collect(process.harness.start_goal(&user, &session, spec).await?).await;
        let run_id = events.first().and_then(|event| event.provider_metadata.get("harness.goal.run_id").cloned()).unwrap_or_default();
        let handle = worker(&process, "first");
        let parked = wait_for(&process.runs, &run_id, &[RunState::Waiting, RunState::Completed, RunState::Failed], Duration::from_secs(240)).await?;
        checks.check(format!("the goal parked on a held edit ({parked:?})"), parked == RunState::Waiting);
        // The process dies while the run waits for a person.
        handle.abort();
        run_id
    };
    let process = open(dir.path(), model, HarnessMode::Ask, |b| b.without_default_members().max_iterations(15)).await?;
    let worker = worker(&process, "second");
    let mut approvals = 0;
    let finished = loop {
        let state = wait_for(&process.runs, &run_id, &[RunState::Waiting, RunState::Completed, RunState::Failed], Duration::from_secs(240)).await?;
        if state != RunState::Waiting || approvals >= 8 {
            break state;
        }
        for call in process.harness.pending_approvals(&user, &session).await? {
            println!("      approving {} from the new process", call.request.tool_name);
            process.harness.decide(&user, &session, &call.fingerprint, HarnessDecision::Approve).await?;
            approvals += 1;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    worker.shutdown().await;
    checks.check(format!("the run completed in the restarted process after {approvals} approvals ({finished:?})"), finished == RunState::Completed);
    let state = process.harness.goal(&user, &session).await?;
    println!("      goal: {state:?}");
    let events = process
        .harness
        .runner()
        .session_service()
        .get(adk_session::GetRequest { app_name: APP.into(), user_id: user.to_string(), session_id: session.to_string(), num_recent_events: None, after: None })
        .await?
        .events()
        .all();
    for (name, response) in responses(&events) {
        let text = response.to_string();
        println!("      {name}: {}", &text[..text.len().min(160)]);
    }
    checks.check("the goal is done", state.is_some_and(|state| state.status == GoalStatus::Done));
    let tests = std::process::Command::new("cargo").args(["test", "--quiet"]).current_dir(dir.path().join("ws")).output()?;
    checks.check("cargo test passes", tests.status.success());
    Ok(checks)
}
