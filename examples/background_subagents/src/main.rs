//! Background sub-agents on the agent kernel, live on Anthropic.
//!
//! | Command | What it proves |
//! |---|---|
//! | `scenario` (default) | A lead agent starts two sub-agents and keeps working, is woken by their completion, sends one a follow-up and cancels the other; a third holds a call, shows `waitingForHuman`, and resumes after the approval |
//! | `crash --home DIR` | Starts a sub-agent whose tool call takes a minute, prints `READY_FOR_KILL`, and waits to be killed with `kill -9` |
//! | `resume --home DIR` | Reopens `DIR`: the kernel recovers the sub-agent, it finishes, and the lead is told once |

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_agent::subagents::SubagentTools;
use adk_core::principal::SubjectRef;
use adk_core::subagent::{SubagentScope, SubagentTarget};
use adk_core::{
    ApprovalDecision, ApprovalService, ApprovalStore, InMemoryApprovalStore, InboxQuery, Llm,
    NewRun, Part, Result as AdkResult, RunBudget, RunConfig, RunRecord, RunState,
    StoreApprovalService, SubagentHost, SubagentRecord, SubagentStatus, Tool, ToolContext,
    ToolEffect,
};
use adk_kernel::stores::KernelStores;
use adk_kernel::{
    Kernel, KernelConfig, Process, ProcessFilter, ProcessKind, ProcessSpec, QueueClass,
    SUBAGENT_NAME_LABEL, SubagentHandle,
};
use adk_model::AnthropicClient;
use adk_model::anthropic::AnthropicConfig;
use adk_runner::Runner;
use adk_server::background::{DurableWorkerConfig, SubagentConfig};
use adk_session::{GetRequest, SessionService, SqliteSessionService};
use anyhow::{Context, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

const APP: &str = "subagents-demo";
const USER: &str = "ana";
const SESSION: &str = "lead-1";

const LEAD: &str = "You are a lead agent. You delegate work to background sub-agents with \
    start_subagent and never wait for them: after starting them you carry on with the user's \
    other steps. Follow the user's numbered steps exactly and in order. Never call \
    fetch_archive or publish_summary yourself; they exist for your sub-agents. When a message \
    starts with '[sub-agent', it is a report from a sub-agent: reply with one line, \
    'NOTED <name> <status>', and do nothing else.";

/// What the tools record, for the proof table.
#[derive(Default)]
struct Shared {
    notes: Mutex<Vec<(String, DateTime<Utc>)>>,
    published: AtomicUsize,
    archive_entered: AtomicBool,
    archive_delay: Mutex<Duration>,
}

struct NoteProgress(Arc<Shared>);

#[async_trait]
impl Tool for NoteProgress {
    fn name(&self) -> &str {
        "note_progress"
    }

    fn description(&self) -> &str {
        "Records a progress note on the lead's own work."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(
            json!({ "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] }),
        )
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        let text = args["text"].as_str().unwrap_or_default().to_string();
        println!("  · note_progress: {text}");
        self.0.notes.lock().unwrap().push((text, Utc::now()));
        Ok(json!({ "recorded": true }))
    }
}

struct FetchArchive(Arc<Shared>);

#[async_trait]
impl Tool for FetchArchive {
    fn name(&self) -> &str {
        "fetch_archive"
    }

    fn description(&self) -> &str {
        "Fetches archived release notes on a topic. Slow."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(
            json!({ "type": "object", "properties": { "topic": { "type": "string" } }, "required": ["topic"] }),
        )
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        self.0.archive_entered.store(true, Ordering::SeqCst);
        let delay = *self.0.archive_delay.lock().unwrap();
        let until = tokio::time::Instant::now() + delay;
        while tokio::time::Instant::now() < until {
            if ctx.is_cancelled() {
                return Ok(json!({ "error": "cancelled" }));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Ok(json!({
            "topic": args["topic"],
            "notes": "Stable release with the ownership and borrowing model, Cargo, and crates.io; \
                      no breaking changes promised within the major version."
        }))
    }
}

struct PublishSummary(Arc<Shared>);

#[async_trait]
impl Tool for PublishSummary {
    fn name(&self) -> &str {
        "publish_summary"
    }

    fn description(&self) -> &str {
        "Publishes a one-line summary to the team channel. A person approves each call."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(
            json!({ "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] }),
        )
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        self.0.published.fetch_add(1, Ordering::SeqCst);
        println!("  · publish_summary ran: {}", args["text"]);
        Ok(json!({ "published": true }))
    }
}

struct Demo {
    kernel: Kernel,
    sessions: Arc<dyn SessionService>,
    approvals: Arc<StoreApprovalService>,
    shared: Arc<Shared>,
}

fn model() -> anyhow::Result<Arc<dyn Llm>> {
    let key = std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY is not set")?;
    let model = std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-5-5".into());
    Ok(Arc::new(AnthropicClient::new(AnthropicConfig::new(key, model))?))
}

fn targets() -> Vec<SubagentTarget> {
    vec![
        SubagentTarget::new("researcher", "answers a question from its own knowledge, briefly"),
        SubagentTarget::new(
            "archivist",
            "fetches archived release notes with fetch_archive and summarizes them",
        ),
        SubagentTarget::new(
            "publisher",
            "publishes a one-line summary with publish_summary, which a person approves",
        ),
    ]
}

async fn build(home: &Path, archive_delay: Duration) -> anyhow::Result<Demo> {
    let shared = Arc::new(Shared::default());
    *shared.archive_delay.lock().unwrap() = archive_delay;
    let stores =
        KernelStores::open(&format!("sqlite://{}?mode=rwc", home.join("kernel.db").display()))
            .await?;
    let sessions = SqliteSessionService::new(&format!(
        "sqlite://{}?mode=rwc",
        home.join("sessions.db").display()
    ))
    .await?;
    sessions.migrate().await?;
    let sessions: Arc<dyn SessionService> = Arc::new(sessions);
    let approvals = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>)
            .with_run_store(Arc::clone(&stores.runs)),
    );
    let config = |budget: Option<RunBudget>| {
        let mut config = RunConfig::builder().approval_service(approvals.clone()).build();
        config.budget = budget;
        config
    };
    let runner = |agent: LlmAgentBuilder, run_config: RunConfig| -> anyhow::Result<Arc<Runner>> {
        Ok(Arc::new(
            Runner::builder()
                .app_name(APP)
                .agent(Arc::new(agent.build()?))
                .session_service(Arc::clone(&sessions))
                .run_config(run_config)
                .build()?,
        ))
    };
    let handle = SubagentHandle::new().with_targets(targets());
    let mut lead = LlmAgentBuilder::new("lead")
        .instruction(LEAD)
        .model(model()?)
        .tool(Arc::new(NoteProgress(shared.clone())))
        .tool(Arc::new(FetchArchive(shared.clone())))
        .tool(Arc::new(PublishSummary(shared.clone())))
        .require_tool_confirmation("publish_summary");
    for tool in SubagentTools::new(Arc::new(handle.clone())).tools() {
        lead = lead.tool(tool);
    }
    let lead = runner(lead, config(Some(RunBudget::new().max_model_calls(40))))?;
    let researcher = runner(
        LlmAgentBuilder::new("researcher")
            .instruction("Answer the task from your own knowledge in at most five short lines.")
            .model(model()?),
        config(None),
    )?;
    let archivist = runner(
        LlmAgentBuilder::new("archivist")
            .instruction("Call fetch_archive once with the topic the task names, then summarize what it returned in two lines.")
            .model(model()?)
            .tool(Arc::new(FetchArchive(shared.clone()))),
        config(None),
    )?;
    let publisher = runner(
        LlmAgentBuilder::new("publisher")
            .instruction("Call publish_summary exactly once with the one-line summary the task gives, then reply 'published'.")
            .model(model()?)
            .tool(Arc::new(PublishSummary(shared.clone())))
            .require_tool_confirmation("publish_summary"),
        config(None),
    )?;
    let mut subagents = SubagentConfig::default().with_default_budget(
        RunBudget::new()
            .max_model_calls(8)
            .max_tool_calls(8)
            .max_wall_time(Duration::from_secs(600)),
    );
    for target in targets() {
        subagents = subagents.with_target(target);
    }
    let kernel = Kernel::builder()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_home(home)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_lease_ttl(Duration::from_secs(5))
                        .with_poll_interval(Duration::from_millis(100)),
                )
                .with_scheduler_tick(Duration::from_millis(200))
                // `adk-rust ps --home DIR` reaches it over `DIR/kernel.sock`.
                .with_control(adk_kernel::control::ControlPlaneConfig::default()),
        )
        .stores(&stores)
        .session_service(Arc::clone(&sessions))
        .approval_service(approvals.clone())
        .agent("lead", lead)
        .agent("researcher", researcher)
        .agent("archivist", archivist)
        .agent("publisher", publisher)
        .subagents(subagents)
        .subagent_handle(handle)
        .build()?;
    Ok(Demo { kernel, sessions, approvals, shared })
}

impl Demo {
    fn scope(&self) -> SubagentScope {
        SubagentScope::new(APP, USER, SESSION)
    }

    async fn subagents(&self) -> anyhow::Result<Vec<SubagentRecord>> {
        Ok(self.kernel.subagents().context("sub-agents on")?.list(&self.scope()).await?)
    }

    async fn subagent(&self, name: &str) -> anyhow::Result<Option<SubagentRecord>> {
        Ok(self.subagents().await?.into_iter().find(|record| record.name == name))
    }

    /// A user turn of the lead, as a process; waits for it to end.
    async fn turn(&self, text: &str) -> anyhow::Result<RunRecord> {
        println!("\n> {text}");
        let run = NewRun::new(QueueClass::INTERACTIVE, APP, USER, "lead", json!({ "text": text }))
            .with_session_id(SESSION);
        let process =
            self.kernel.processes().spawn(run, ProcessSpec::new(ProcessKind::Interactive)).await?;
        let pid = process.meta.pid;
        let run = self
            .wait(&format!("turn {pid}"), Duration::from_secs(240), async || {
                let process = self.kernel.processes().get(&pid).await?;
                Ok(process.filter(|p| p.run.state.is_terminal()).map(|p| p.run))
            })
            .await?;
        let answer = run.result.as_ref().and_then(|r| r["text"].as_str()).unwrap_or_default();
        println!("< [{}] {}", state(run.state), answer.trim());
        Ok(run)
    }

    async fn wait<T>(
        &self,
        what: &str,
        limit: Duration,
        mut probe: impl AsyncFnMut() -> anyhow::Result<Option<T>>,
    ) -> anyhow::Result<T> {
        let started = tokio::time::Instant::now();
        loop {
            if let Some(value) = probe().await? {
                return Ok(value);
            }
            if started.elapsed() > limit {
                bail!("timed out waiting for {what}");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// The lead session's sub-agent reports: (header, delivered as a steering message).
    async fn reports(&self) -> anyhow::Result<Vec<(String, bool)>> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: SESSION.into(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        Ok(session
            .events()
            .all()
            .iter()
            .filter(|event| event.author == "user")
            .filter_map(|event| {
                let text =
                    event.llm_response.content.as_ref()?.parts.iter().find_map(Part::text)?;
                text.starts_with("[sub-agent ").then(|| {
                    (
                        text.lines().next().unwrap_or_default().to_string(),
                        event.actions.steering.is_some(),
                    )
                })
            })
            .collect())
    }

    async fn report_for(
        &self,
        record: &SubagentRecord,
        status: &str,
    ) -> anyhow::Result<Option<(String, bool)>> {
        let needle = format!("({}) {status} · run {}", record.id, record.run_id);
        Ok(self.reports().await?.into_iter().find(|(header, _)| header.contains(&needle)))
    }

    async fn ps(&self) -> anyhow::Result<Vec<Process>> {
        Ok(self.kernel.processes().ps(ProcessFilter::default()).await?)
    }
}

fn state(state: RunState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn print_ps(processes: &[Process]) {
    println!("\n$ adk-rust ps");
    println!("{:<12}  {:<9}  {:<18}  {:<12}  NOTE", "PID", "STATE", "KIND", "PARENT");
    for process in processes {
        let mut note = String::new();
        if let Some(name) = process.meta.labels.get(SUBAGENT_NAME_LABEL) {
            note.push_str(name);
        }
        if process.run.state == RunState::Waiting
            && matches!(
                process.run.checkpoint,
                Some(adk_core::RunCheckpointRef::AwaitingApproval { .. })
            )
        {
            note.push_str(" · waiting for a human");
        }
        println!(
            "{:<12}  {:<9}  {:<18}  {:<12}  {}",
            process.meta.pid.chars().take(12).collect::<String>(),
            state(process.run.state),
            process.meta.kind.to_string(),
            process
                .meta
                .parent
                .as_deref()
                .map(|p| p.chars().take(12).collect::<String>())
                .unwrap_or_default(),
            note.trim_start_matches(" · ")
        );
    }
}

struct Proof(Vec<(&'static str, bool, String)>);

impl Proof {
    fn check(&mut self, what: &'static str, ok: bool, evidence: impl Into<String>) {
        self.0.push((what, ok, evidence.into()));
    }

    fn print(&self) -> bool {
        println!("\n| Requirement | Result | Evidence |\n|---|---|---|");
        for (what, ok, evidence) in &self.0 {
            println!("| {what} | {} | {evidence} |", if *ok { "PASS" } else { "FAIL" });
        }
        self.0.iter().all(|(_, ok, _)| *ok)
    }
}

/// With `HOLD_FOR_PS=<seconds>`, waits that long so `adk-rust ps --home DIR` can be run
/// from another terminal.
async fn hold_for_ps(home: &Path, moment: &str) {
    let Some(seconds) = std::env::var("HOLD_FOR_PS").ok().and_then(|s| s.parse::<u64>().ok())
    else {
        return;
    };
    println!("PS_NOW {moment} home={}", home.display());
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}

async fn scenario(home: Option<PathBuf>) -> anyhow::Result<bool> {
    let temp = tempfile::tempdir()?;
    let home = home.unwrap_or_else(|| temp.path().to_path_buf());
    let home = home.as_path();
    let demo = build(home, Duration::from_secs(90)).await?;
    demo.kernel.start().await?;
    let mut proof = Proof(Vec::new());

    // 1. Two sub-agents in the background; the lead keeps working.
    let first = demo
        .turn(
            "1. Start a sub-agent named alpha (agent researcher) with the task 'List three facts \
             about the Rust borrow checker.' 2. Start a sub-agent named beta (agent archivist) \
             with the task 'Use fetch_archive with topic \"rust 1.0\" and summarize the notes.' \
             3. Without waiting for them, call note_progress with 'alpha and beta started', then \
             call note_progress with 'drafting the plan while they work'. 4. Reply with the two \
             sub-agent ids.",
        )
        .await?;
    let alpha = demo.subagent("alpha").await?.context("alpha started")?;
    let beta = demo.subagent("beta").await?.context("beta started")?;
    hold_for_ps(home, "alpha-and-beta").await;
    let notes = demo.shared.notes.lock().unwrap().clone();
    let kept_working = notes.len() >= 2
        && notes.iter().all(|(_, at)| *at >= alpha.created_at && *at >= beta.created_at);
    let beta_now = demo.subagent("beta").await?.context("beta")?;
    proof.check(
        "starts two background sub-agents and keeps working",
        kept_working && first.state == RunState::Completed && !beta_now.status.is_terminal(),
        format!(
            "alpha {} and beta {} started; {} note_progress calls after both; the turn ended {} while beta was {}",
            alpha.id, beta.id, notes.len(), state(first.state), beta_now.status
        ),
    );

    // 2. alpha's finish wakes the lead.
    let alpha_done = demo
        .wait("alpha to finish", Duration::from_secs(180), async || {
            Ok(demo.subagent("alpha").await?.filter(|r| r.status.is_terminal()))
        })
        .await?;
    let report = demo
        .wait("the lead to be told about alpha", Duration::from_secs(180), async || {
            demo.report_for(&alpha_done, "completed").await
        })
        .await?;
    proof.check(
        "is woken by a sub-agent's completion",
        alpha_done.status == SubagentStatus::Completed,
        format!(
            "alpha completed; the lead received `{}` {}",
            report.0,
            if report.1 {
                "in its running turn (steered)"
            } else {
                "as a new turn (a triggered:subagent process)"
            }
        ),
    );
    // Let the woken turn finish before the next user turn.
    tokio::time::sleep(Duration::from_secs(3)).await;
    demo.wait("the lead to be idle", Duration::from_secs(120), async || {
        let busy =
            demo.ps().await?.iter().any(|p| p.run.target == "lead" && !p.run.state.is_terminal());
        Ok((!busy).then_some(()))
    })
    .await?;

    // 3. A follow-up to alpha, and beta cancelled.
    demo.turn(&format!(
        "1. Send sub-agent {} a follow-up with message_subagent: 'Add one more fact, about \
         lifetimes.' 2. Cancel sub-agent {} with cancel_subagent. 3. Reply with what you did.",
        alpha.id, beta.id
    ))
    .await?;
    let alpha_again = demo
        .wait("alpha's follow-up to finish", Duration::from_secs(180), async || {
            Ok(demo.subagent("alpha").await?.filter(|r| r.runs >= 2 && r.status.is_terminal()))
        })
        .await?;
    let followed = demo
        .wait("the lead to be told about the follow-up", Duration::from_secs(180), async || {
            demo.report_for(&alpha_again, "completed").await
        })
        .await?;
    proof.check(
        "sends a follow-up to a sub-agent",
        alpha_again.runs == 2 && alpha_again.status == SubagentStatus::Completed,
        format!(
            "alpha ran a second turn in {} ({}); the lead was told: `{}`",
            alpha_again.session_id,
            alpha_again
                .result
                .as_deref()
                .unwrap_or_default()
                .lines()
                .last()
                .unwrap_or_default()
                .trim(),
            followed.0
        ),
    );
    let beta_done = demo
        .wait("beta to be cancelled", Duration::from_secs(60), async || {
            Ok(demo.subagent("beta").await?.filter(|r| r.status.is_terminal()))
        })
        .await?;
    let beta_reports =
        demo.reports().await?.into_iter().filter(|(h, _)| h.contains(&beta.id)).count();
    proof.check(
        "cancels another",
        beta_done.status == SubagentStatus::Cancelled && beta_reports == 0,
        format!(
            "beta is {}; the lead received {beta_reports} report(s) about it",
            beta_done.status
        ),
    );
    demo.wait("the lead to be idle", Duration::from_secs(120), async || {
        let busy =
            demo.ps().await?.iter().any(|p| p.run.target == "lead" && !p.run.state.is_terminal());
        Ok((!busy).then_some(()))
    })
    .await?;

    // 4. A sub-agent that holds a call waits for a human, and resumes after the approval.
    demo.turn(
        "Start a sub-agent named gamma (agent publisher) with the task 'Publish this one-line \
         summary: The borrow checker rules out data races at compile time.' Then reply with its id.",
    )
    .await?;
    let waiting = demo
        .wait("gamma to wait for a person", Duration::from_secs(180), async || {
            Ok(demo
                .subagent("gamma")
                .await?
                .filter(|r| r.status == SubagentStatus::WaitingForHuman))
        })
        .await?;
    let processes = demo.ps().await?;
    print_ps(&processes);
    hold_for_ps(home, "gamma-waits").await;
    let listed = processes.iter().filter(|p| p.meta.kind == ProcessKind::Subagent).count();
    let parented = processes
        .iter()
        .filter(|p| p.meta.kind == ProcessKind::Subagent)
        .all(|p| p.meta.parent.is_some());
    let published_before = demo.shared.published.load(Ordering::SeqCst);
    let inbox = demo.approvals.inbox(InboxQuery::default()).await?;
    let request = inbox
        .items
        .iter()
        .find(|item| item.run_id.as_deref() == Some(waiting.run_id.as_str()))
        .context("gamma's held call is in the approvals inbox")?;
    println!("\n  · approving {} ({}) in the inbox", request.id, request.tool_name);
    demo.approvals
        .decide(
            &request.id,
            request.version,
            ApprovalDecision::approve(SubjectRef::human(USER)).with_groups(["operator"]),
        )
        .await?;
    let gamma_done = demo
        .wait("gamma to finish", Duration::from_secs(180), async || {
            Ok(demo.subagent("gamma").await?.filter(|r| r.status.is_terminal()))
        })
        .await?;
    let gamma_report = demo
        .wait("the lead to be told about gamma", Duration::from_secs(180), async || {
            demo.report_for(&gamma_done, "completed").await
        })
        .await?;
    let published = demo.shared.published.load(Ordering::SeqCst);
    proof.check(
        "a held call shows 'waiting for a human' and resumes after approval",
        published_before == 0 && published == 1 && gamma_done.status == SubagentStatus::Completed,
        format!(
            "gamma was {} with publish_summary run {published_before} time(s); approved; then {} with it run {published} time(s); the lead was told: `{}`",
            waiting.status, gamma_done.status, gamma_report.0
        ),
    );
    proof.check(
        "ps lists every sub-agent as a process of its parent",
        listed >= 4 && parented,
        format!("{listed} `subagent` processes, each with the lead's turn as its parent"),
    );
    proof.check(
        "each sub-agent runs on a budget slice",
        [&alpha, &beta].iter().all(|r| r.budget.max_model_calls.is_some_and(|n| n <= 8)),
        format!("alpha's slice: {}", serde_json::to_string(&alpha.budget)?),
    );
    print_ps(&demo.ps().await?);
    demo.kernel.stop().await?;
    Ok(proof.print())
}

const CRASH_TURN: &str = "Start a sub-agent named delta (agent archivist) with the task 'Use \
    fetch_archive with topic \"rust 2.0\" and summarize the notes in two lines.' Then reply with \
    its id only.";

async fn crash(home: PathBuf) -> anyhow::Result<bool> {
    let demo = build(&home, Duration::from_secs(120)).await?;
    demo.kernel.start().await?;
    demo.turn(CRASH_TURN).await?;
    let delta = demo
        .wait("delta's tool call to start", Duration::from_secs(120), async || {
            let delta = demo.subagent("delta").await?;
            Ok(delta.filter(|r| {
                r.status == SubagentStatus::Running
                    && demo.shared.archive_entered.load(Ordering::SeqCst)
            }))
        })
        .await?;
    print_ps(&demo.ps().await?);
    println!("PS_NOW delta-runs home={}", home.display());
    println!(
        "READY_FOR_KILL pid={} subagent={} run={}",
        std::process::id(),
        delta.id,
        delta.run_id
    );
    std::future::pending::<()>().await;
    Ok(true)
}

async fn resume(home: PathBuf) -> anyhow::Result<bool> {
    let demo = build(&home, Duration::from_secs(2)).await?;
    let before = demo.subagent("delta").await?.context("delta was started before the kill")?;
    println!(
        "before the restart: delta is {} (run {}, attempt {})",
        before.status,
        before.run_id,
        demo.kernel
            .processes()
            .get(&before.run_id)
            .await?
            .map(|p| p.run.attempt)
            .unwrap_or_default()
    );
    demo.kernel.start().await?;
    let done = demo
        .wait("delta to finish", Duration::from_secs(240), async || {
            Ok(demo.subagent("delta").await?.filter(|r| r.status.is_terminal()))
        })
        .await?;
    let report = demo
        .wait("the lead to be told", Duration::from_secs(180), async || {
            demo.report_for(&done, "completed").await
        })
        .await?;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let reports = demo.reports().await?.into_iter().filter(|(h, _)| h.contains(&done.id)).count();
    let attempt =
        demo.kernel.processes().get(&done.run_id).await?.map(|p| p.run.attempt).unwrap_or_default();
    print_ps(&demo.ps().await?);
    let mut proof = Proof(Vec::new());
    proof.check(
        "survives kill -9 of the parent and the kernel",
        before.status == SubagentStatus::Running && done.status == SubagentStatus::Completed && attempt >= 2,
        format!("delta was {} when the process was killed; the restarted kernel recovered run {} (attempt {attempt}) and it {}", before.status, done.run_id, done.status),
    );
    proof.check(
        "the parent is told once after the restart",
        reports == 1,
        format!("the lead received `{}` {reports} time(s)", report.0),
    );
    demo.kernel.stop().await?;
    Ok(proof.print())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,adk_server::background::subagents=info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let home = || -> anyhow::Result<PathBuf> {
        let at = args.iter().position(|a| a == "--home").context("--home DIR is required")?;
        let dir = PathBuf::from(args.get(at + 1).context("--home DIR is required")?);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    };
    let passed = match args.first().map(String::as_str) {
        None | Some("scenario") => scenario(home().ok()).await?,
        Some("crash") => crash(home()?).await?,
        Some("resume") => resume(home()?).await?,
        Some(other) => bail!("unknown command `{other}`; use scenario, crash or resume"),
    };
    if !passed {
        std::process::exit(1);
    }
    Ok(())
}
