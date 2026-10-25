//! What the scenarios share: the model source, the harness each scenario builds, the
//! transcript printer and the named checks.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, InMemoryAuditRecorder};
use adk_core::{
    ActionLedger, ApprovalStore, DurableRunStore, Event, EventStream, Llm, LlmRequest,
    LlmResponseStream, Part, PendingApproval, RunState, SchemaAdapter, SessionId, UserId,
};
use adk_harness::{
    GoalEngine, Harness, HarnessAgent, HarnessAgentBuilder, HarnessDecision, HarnessMode,
    InstructionsConfig, SqliteGoalStore, SqliteModeStore,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{
    InMemorySessionService, SessionService, SqliteActionLedger, SqliteApprovalStore,
    SqliteRunStore, SqliteSessionService,
};
use anyhow::{Result, bail};
use futures::StreamExt;
use serde_json::{Value, json};

/// The application name every scenario's sessions, rules and records are kept under.
pub const APP: &str = "harness-walkthrough";

/// How long one turn may take; live turns that build a crate and call a model several
/// times need minutes.
const TURN_TIMEOUT: Duration = Duration::from_secs(900);

// --- Models ------------------------------------------------------------------------------

/// Where a scenario's models come from: a `ScriptedLlm` per harness process, or the live
/// model of the walkthrough's provider.
#[derive(Clone)]
pub enum Models {
    /// Every harness process replays the script its scenario gives it.
    Scripted,
    /// Every harness process calls this model.
    Live {
        /// `provider/model`, for the report.
        label: String,
        /// The model.
        model: Arc<dyn Llm>,
    },
}

impl Models {
    /// Whether the scenarios call a real model.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live { .. })
    }

    /// `provider/model`, or `scripted`.
    pub fn label(&self) -> &str {
        match self {
            Self::Scripted => "scripted",
            Self::Live { label, .. } => label,
        }
    }

    /// The model of one harness process: `script` replayed in order, or the live model.
    /// Either way every request is recorded.
    pub fn model(&self, script: Vec<ScriptedResponse>) -> Arc<Recorder> {
        let inner: Arc<dyn Llm> = match self {
            Self::Scripted => Arc::new(ScriptedLlm::new("scripted", script)),
            Self::Live { model, .. } => model.clone(),
        };
        Arc::new(Recorder { inner, requests: Mutex::new(Vec::new()) })
    }
}

/// A model that records the requests it forwards, so a scenario can check what a member saw.
pub struct Recorder {
    inner: Arc<dyn Llm>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl Recorder {
    /// Every request so far, in call order.
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }
}

#[async_trait::async_trait]
impl Llm for Recorder {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.requests.lock().unwrap_or_else(|error| error.into_inner()).push(request.clone());
        self.inner.generate_content(request, stream).await
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }

    fn uses_interactions_api(&self) -> bool {
        self.inner.uses_interactions_api()
    }

    fn max_cost_micro_usd(&self, request: &LlmRequest) -> Option<u64> {
        self.inner.max_cost_micro_usd(request)
    }
}

/// A scripted reply that calls `name` with `args`.
pub fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

/// A scripted text reply.
pub fn text(text: &str) -> ScriptedResponse {
    ScriptedResponse::text(text)
}

// --- Checks ------------------------------------------------------------------------------

/// The named checks a scenario made, or the reason it did not run.
#[derive(Default)]
pub struct Checks {
    checks: Vec<(String, bool)>,
    skipped: Option<String>,
}

impl Checks {
    /// A scenario that did not run, and why.
    pub fn skipped(reason: impl Into<String>) -> Self {
        Self { checks: Vec::new(), skipped: Some(reason.into()) }
    }

    /// Records and prints one check.
    pub fn check(&mut self, name: impl Into<String>, ok: bool) {
        let name = name.into();
        println!("    {} {name}", if ok { "PASS" } else { "FAIL" });
        self.checks.push((name, ok));
    }

    /// Why the scenario did not run, if it did not.
    pub fn skip_reason(&self) -> Option<&str> {
        self.skipped.as_deref()
    }

    /// Whether every check passed (and there was at least one).
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|(_, ok)| *ok)
    }

    /// The names of the failed checks.
    pub fn failures(&self) -> Vec<&str> {
        self.checks.iter().filter(|(_, ok)| !ok).map(|(name, _)| name.as_str()).collect()
    }

    /// How many checks passed, of how many.
    pub fn tally(&self) -> (usize, usize) {
        (self.checks.iter().filter(|(_, ok)| *ok).count(), self.checks.len())
    }
}

// --- Transcript --------------------------------------------------------------------------

/// `text` on one line, cut at `max` characters.
pub fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max) {
        Some((at, _)) => format!("{}...", &flat[..at]),
        None => flat,
    }
}

/// One line for a tool's response: held, refused with its error, or what it returned.
fn summary(response: &Value) -> String {
    if adk_core::is_confirmation_pending(response) {
        return "held for approval".to_string();
    }
    if let Some(error) = response.get("error") {
        return format!(
            "refused: {}",
            clip(&error.as_str().map_or_else(|| error.to_string(), str::to_string), 200)
        );
    }
    if let Some(code) = response.get("exit_code") {
        let mut line = format!("exit_code {code}");
        if let Some(hint) = response.pointer("/sandbox/hint").and_then(Value::as_str) {
            line.push_str(&format!(", sandbox hint: {}", clip(hint, 160)));
        }
        return line;
    }
    clip(&response.to_string(), 160)
}

/// Prints one event of a turn: the model's text, its tool calls and the tools' responses.
pub fn show(event: &Event) {
    if event.llm_response.partial {
        return;
    }
    let author = &event.author;
    if let Some(content) = &event.llm_response.content {
        for part in &content.parts {
            match part {
                Part::Text { text } if !text.trim().is_empty() && author != "user" => {
                    println!("      {author}: {}", clip(text, 240));
                }
                Part::FunctionCall { name, args, .. } => {
                    println!("      {author} -> {name} {}", clip(&args.to_string(), 200));
                }
                Part::FunctionResponse { function_response, .. } => {
                    println!(
                        "      {author} <- {}: {}",
                        function_response.name,
                        summary(&function_response.response)
                    );
                }
                _ => {}
            }
        }
    }
    if let Some(code) = &event.llm_response.error_code {
        let message = event.llm_response.error_message.as_deref().unwrap_or_default();
        println!("      {author} stopped: {code}: {}", clip(message, 200));
    }
}

/// Collects a turn's events, printing each, and the errors that ended it.
pub async fn collect(stream: EventStream) -> Result<Turn> {
    let results = tokio::time::timeout(TURN_TIMEOUT, stream.collect::<Vec<_>>()).await?;
    let mut turn = Turn::default();
    for result in results {
        match result {
            Ok(event) => {
                show(&event);
                turn.events.push(event);
            }
            Err(error) => {
                println!("      run error: {}: {}", error.code, clip(&error.message, 200));
                turn.errors.push(error.code.to_string());
            }
        }
    }
    Ok(turn)
}

/// What one turn produced.
#[derive(Default)]
pub struct Turn {
    /// The turn's events.
    pub events: Vec<Event>,
    /// Codes of the errors that ended it.
    pub errors: Vec<String>,
}

/// One tool call of a turn and its response.
#[derive(Clone, Debug)]
pub struct CallResult {
    /// Who called it: the harness, or a member.
    pub author: String,
    /// The tool.
    pub name: String,
    /// Its arguments.
    pub args: Value,
    /// Its response, if it has one yet.
    pub response: Option<Value>,
}

/// How a call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It ran.
    Ran,
    /// It waits for a decision.
    Held,
    /// It returned an error: refused by the policy, a permission or the tool.
    Refused,
    /// No response.
    Missing,
}

impl CallResult {
    /// How the call ended.
    pub fn outcome(&self) -> Outcome {
        match &self.response {
            None => Outcome::Missing,
            Some(response) if adk_core::is_confirmation_pending(response) => Outcome::Held,
            Some(response) if response.get("error").is_some() => Outcome::Refused,
            Some(_) => Outcome::Ran,
        }
    }

    /// The error the call returned, or an empty string.
    pub fn error(&self) -> String {
        self.response
            .as_ref()
            .and_then(|response| response.get("error"))
            .map(|error| error.as_str().map_or_else(|| error.to_string(), str::to_string))
            .unwrap_or_default()
    }
}

impl Turn {
    /// The turn's tool calls with their responses, in call order.
    pub fn calls(&self) -> Vec<CallResult> {
        let mut calls: Vec<(Option<String>, CallResult)> = Vec::new();
        for event in &self.events {
            if event.llm_response.partial {
                continue;
            }
            let Some(content) = &event.llm_response.content else { continue };
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, args, id, .. } => calls.push((
                        id.clone(),
                        CallResult {
                            author: event.author.clone(),
                            name: name.clone(),
                            args: args.clone(),
                            response: None,
                        },
                    )),
                    Part::FunctionResponse { function_response, id, .. } => {
                        let open = calls.iter_mut().find(|(call_id, call)| {
                            call.response.is_none()
                                && call.name == function_response.name
                                && (id.is_none() || *call_id == *id)
                        });
                        match open {
                            Some((_, call)) => {
                                call.response = Some(function_response.response.clone())
                            }
                            // An answer to a call an earlier turn made: a resumed held call.
                            None => calls.push((
                                id.clone(),
                                CallResult {
                                    author: event.author.clone(),
                                    name: function_response.name.clone(),
                                    args: Value::Null,
                                    response: Some(function_response.response.clone()),
                                },
                            )),
                        }
                    }
                    _ => {}
                }
            }
        }
        calls.into_iter().map(|(_, call)| call).collect()
    }

    /// The calls of `tool`.
    pub fn calls_of(&self, tool: &str) -> Vec<CallResult> {
        self.calls().into_iter().filter(|call| call.name == tool).collect()
    }

    /// The outcome of the last call of `tool`.
    pub fn last_outcome(&self, tool: &str) -> Outcome {
        self.calls_of(tool).last().map_or(Outcome::Missing, CallResult::outcome)
    }

    /// Whether an event or an error of the turn carries `code`.
    pub fn stopped_with(&self, code: &str) -> bool {
        self.errors.iter().any(|error| error == code)
            || self
                .events
                .iter()
                .any(|event| event.llm_response.error_code.as_deref() == Some(code))
    }
}

// --- Workspaces ----------------------------------------------------------------------------

/// The `calc` crate the scenarios work on. With `bug`, `add` subtracts and its test fails.
pub fn write_crate(root: &Path, bug: bool) -> Result<()> {
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"calc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )?;
    let op = if bug { "a - b" } else { "a + b" };
    std::fs::write(
        root.join("src/lib.rs"),
        format!(
            "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {{\n    {op}\n}}\n\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn adds() {{\n        assert_eq!(super::add(2, 3), 5);\n    }}\n}}\n"
        ),
    )?;
    Ok(())
}

/// A digest of every file under `root` except build output: equal digests, equal trees.
pub fn tree_digest(root: &Path) -> String {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                walk(&path, root, out);
            } else {
                let name = path.strip_prefix(root).unwrap_or(&path).display().to_string();
                out.push((name, std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    let digest = files.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, (name, bytes)| {
        name.bytes()
            .chain([0])
            .chain(bytes.iter().copied())
            .fold(hash, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3))
    });
    format!("{digest:016x} ({} files)", files.len())
}

/// Runs `cargo test --quiet` in `root` with a clean environment for the build.
pub fn cargo_test_passes(root: &Path) -> bool {
    std::process::Command::new("cargo")
        .args(["test", "--quiet"])
        .current_dir(root)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTC_WRAPPER")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn ids(user: &str, session: &str) -> Result<(UserId, SessionId)> {
    Ok((UserId::new(user)?, SessionId::new(session)?))
}

/// The instruction files the walkthrough loads: the workspace's only, trusted, so a
/// developer's own user-level `AGENTS.md` does not change what the model is told.
fn workspace_instructions() -> InstructionsConfig {
    InstructionsConfig { user_file: None, require_trust: false, ..InstructionsConfig::default() }
}

/// The builder every scenario starts from.
fn base(model: Arc<dyn Llm>, workspace: &Path) -> HarnessAgentBuilder {
    HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(workspace).allow_bash(true))
        .project_instructions(workspace_instructions())
        .without_compaction()
        .max_iterations(16)
}

// --- One harness over in-memory sessions -------------------------------------------------

/// A harness over in-memory sessions with an in-memory audit log, in a temporary workspace.
pub struct Env {
    /// The workspace.
    pub dir: tempfile::TempDir,
    /// The harness.
    pub harness: Harness,
    /// Every audit record the harness wrote.
    pub audit: InMemoryAuditRecorder,
}

impl Env {
    /// A harness in an empty workspace.
    pub async fn new(
        model: Arc<dyn Llm>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> Result<Self> {
        Self::in_dir(tempfile::tempdir()?, model, configure).await
    }

    /// A harness in `dir`.
    pub async fn in_dir(
        dir: tempfile::TempDir,
        model: Arc<dyn Llm>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> Result<Self> {
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let audit = InMemoryAuditRecorder::new();
        let agent = Arc::new(configure(base(model, dir.path())).build()?);
        let runner = agent
            .configure_runner(
                Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions),
            )
            .audit_log(Arc::new(audit.clone()))
            .build()?;
        let harness = Harness::with_runner(agent, runner)?;
        Ok(Self { dir, harness, audit })
    }

    /// A path in the workspace.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    /// The audit records with `action`.
    pub fn records(&self, action: &str) -> Vec<AuditRecordDraft> {
        self.audit.records().into_iter().filter(|record| record.action == action).collect()
    }

    /// Sends `text` as `user` in `session`, opening the session first.
    pub async fn send(&self, user: &str, session: &str, text: &str) -> Result<Turn> {
        println!("    [{user}/{session}] user: {}", clip(text, 200));
        let (user, session) = ids(user, session)?;
        self.harness.open_session(&user, Some(&session)).await?;
        collect(self.harness.send(&user, &session, text).await?).await
    }

    /// Runs an empty turn, which resumes the session's decided held calls.
    pub async fn resume(&self, user: &str, session: &str) -> Result<Turn> {
        println!("    [{user}/{session}] resume");
        let (user, session) = ids(user, session)?;
        collect(self.harness.resume(&user, &session).await?).await
    }

    /// The session's calls waiting for a decision.
    pub async fn pending(&self, user: &str, session: &str) -> Result<Vec<PendingApproval>> {
        let (user, session) = ids(user, session)?;
        Ok(self.harness.pending_approvals(&user, &session).await?)
    }

    /// Records a person's decision on the pending call with `fingerprint`.
    pub async fn decide(
        &self,
        user: &str,
        session: &str,
        pending: &PendingApproval,
        decision: HarnessDecision,
    ) -> Result<()> {
        let verdict = match &decision {
            HarnessDecision::Approve => "approve once".to_string(),
            HarnessDecision::ApproveWithArgs(_) => "approve with edited arguments".to_string(),
            HarnessDecision::ApproveAndRemember(rule) => {
                format!("approve and always allow \"{}\"", rule.label)
            }
            HarnessDecision::Deny { .. } => "deny".to_string(),
            HarnessDecision::DenyAndRemember(rule) => {
                format!("deny and always deny \"{}\"", rule.label)
            }
        };
        println!(
            "    [{user}/{session}] {verdict}: {} {}",
            pending.request.tool_name,
            clip(&pending.request.args.to_string(), 120)
        );
        let (user, session) = ids(user, session)?;
        self.harness.decide(&user, &session, &pending.fingerprint, decision).await?;
        Ok(())
    }
}

// --- One harness process over a SQLite file -----------------------------------------------

/// A harness process whose sessions, approvals, action ledger, modes, goals, durable runs
/// and plan checkpoints are in `dir/harness.db`, working in `dir/ws`. Dropping it is the
/// process stopping; opening it again on the same `dir` is the restart.
pub struct Durable {
    /// The harness.
    pub harness: Harness,
    /// The durable runs.
    pub runs: Arc<SqliteRunStore>,
    /// The database.
    pub pool: sqlx::SqlitePool,
    /// This process's audit records.
    pub audit: InMemoryAuditRecorder,
}

impl Durable {
    /// Opens the process over `dir`.
    pub async fn open(
        dir: &Path,
        model: Arc<dyn Llm>,
        mode: HarnessMode,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> Result<Self> {
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
        let builder = base(model, &dir.join("ws"))
            .mode(mode)
            .mode_store(Arc::new(modes))
            .goal_store(Arc::new(goals))
            .goal_engine(GoalEngine::Loop)
            .approval_store(Arc::new(approvals) as Arc<dyn ApprovalStore>)
            .plan_checkpointer(Arc::new(checkpointer));
        let agent = Arc::new(configure(builder).build()?);
        let audit = InMemoryAuditRecorder::new();
        let sessions: Arc<dyn SessionService> = Arc::new(sessions);
        let runner = agent
            .configure_runner(
                Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions),
            )
            .action_ledger(Arc::new(ledger) as Arc<dyn ActionLedger>)
            .audit_log(Arc::new(audit.clone()))
            .build()?;
        let runs = Arc::new(runs);
        let harness = Harness::with_runner(agent, runner)?
            .with_run_store(runs.clone() as Arc<dyn DurableRunStore>);
        Ok(Self { harness, runs, pool, audit })
    }

    /// The audit records with `action`.
    pub fn records(&self, action: &str) -> Vec<AuditRecordDraft> {
        self.audit.records().into_iter().filter(|record| record.action == action).collect()
    }

    /// Sends `text` as `user` in `session`, opening the session first.
    pub async fn send(&self, user: &str, session: &str, text: &str) -> Result<Turn> {
        println!("    [{user}/{session}] user: {}", clip(text, 200));
        let (user, session) = ids(user, session)?;
        self.harness.open_session(&user, Some(&session)).await?;
        collect(self.harness.send(&user, &session, text).await?).await
    }

    /// Runs an empty turn, which resumes the session's decided held calls.
    pub async fn resume(&self, user: &str, session: &str) -> Result<Turn> {
        println!("    [{user}/{session}] resume");
        let (user, session) = ids(user, session)?;
        collect(self.harness.resume(&user, &session).await?).await
    }

    /// The session's calls waiting for a decision.
    pub async fn pending(&self, user: &str, session: &str) -> Result<Vec<PendingApproval>> {
        let (user, session) = ids(user, session)?;
        Ok(self.harness.pending_approvals(&user, &session).await?)
    }

    /// Approves every pending call of the session, returning how many of each tool.
    pub async fn approve_all(&self, user: &str, session: &str) -> Result<Vec<String>> {
        let mut approved = Vec::new();
        for pending in self.pending(user, session).await? {
            println!(
                "    [{user}/{session}] approve {} {}",
                pending.request.tool_name,
                clip(&pending.request.args.to_string(), 120)
            );
            let (u, s) = ids(user, session)?;
            self.harness.decide(&u, &s, &pending.fingerprint, HarnessDecision::Approve).await?;
            approved.push(pending.request.tool_name.clone());
        }
        Ok(approved)
    }

    /// A durable worker that runs this process's goals.
    pub fn worker(&self, id: &str) -> adk_server::WorkerHandle {
        let (target, runner) = self.harness.goal_target();
        adk_server::DurableWorker::new(self.runs.clone() as Arc<dyn DurableRunStore>)
            .with_agent(target, runner)
            .with_config(
                adk_server::DurableWorkerConfig::default()
                    .with_worker_id(id)
                    .with_poll_interval(Duration::from_millis(50))
                    .with_lease_ttl(Duration::from_secs(5)),
            )
            .spawn()
    }

    /// Waits until run `run_id` is in one of `states`.
    pub async fn wait_for(&self, run_id: &str, states: &[RunState]) -> Result<RunState> {
        let deadline = tokio::time::Instant::now() + TURN_TIMEOUT;
        loop {
            if let Some(run) = self.runs.get(run_id).await?
                && states.contains(&run.state)
            {
                return Ok(run.state);
            }
            if tokio::time::Instant::now() > deadline {
                bail!("run {run_id} did not reach {states:?}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Prints the session's events after event `from`, as the transcript of turns a
    /// durable worker ran; returns how many events the session has.
    pub async fn print_session(&self, user: &str, session: &str, from: usize) -> Result<usize> {
        let events = self
            .harness
            .runner()
            .session_service()
            .get(adk_session::GetRequest {
                app_name: APP.into(),
                user_id: user.into(),
                session_id: session.into(),
                num_recent_events: None,
                after: None,
            })
            .await?
            .events()
            .all();
        for event in events.iter().skip(from) {
            show(event);
        }
        Ok(events.len())
    }

    /// The action ledger's records of `tool`: (idempotency key, recorded outcome).
    pub async fn ledger_records(&self, tool: &str) -> Result<Vec<(String, Option<String>)>> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT idempotency_key, outcome FROM adk_action_ledger WHERE tool_name = ? ORDER BY started_at",
        )
        .bind(tool)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

/// The JSON of a pending call, for the report.
pub fn describe(pending: &[PendingApproval]) -> Value {
    json!(
        pending
            .iter()
            .map(|entry| json!({ "tool": entry.request.tool_name, "args": entry.request.args }))
            .collect::<Vec<_>>()
    )
}
