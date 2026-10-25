//! The harness under an in-process agent kernel (`--kernel`, spec 3.2 §5, §11): K2, K4
//! and K5 end to end on the walkthrough's provider.
//!
//! Each scenario boots a kernel the way `adk-rust up` builds one: `Harness::kernel()` over
//! the harness, with a durable worker per queue, the scheduler, SQLite stores in
//! `harness.db` and the control plane on a socket and loopback HTTP. As in the default
//! mode, a scripted run replays what a model that follows the prompt does and a live run
//! sends the same prompts to the provider. Every check is on stores, processes, files and
//! audit records, never on what the model says.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_core::{
    ApprovalAction, ApprovalPolicy, ApprovalService, ApproverSelector, Clock, DurableRunStore,
    Event, Llm, ManualClock, MissedFirePolicy, NewRun, RunFilter, RunRecord, RunState,
    StoreApprovalService, SystemClock,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::api::{AgendaQuery, AttachRequest, ProcessesQuery, SpawnRequest};
use adk_kernel::control::{ControlClient, ControlPlaneConfig};
use adk_kernel::{
    AgendaItem, AgendaState, ExpiryAction, Kernel, KernelConfig, PartitionId, Process,
    ProcessFilter, ProcessKind, ProcessSpec, QueueClass, Wake, WakeSet,
};
use adk_runner::Runner;
use adk_session::{
    GetRequest, SessionService, SqliteApprovalStore, SqliteRunStore, SqliteScheduleStore,
    SqliteSessionService,
};
use anyhow::{Context, Result, bail};
use chrono::{Timelike, Utc};
use serde_json::json;

use super::scenarios::Scenario;
use super::support::{APP, Checks, Models, call, show, text};

/// How long a scenario waits for a live turn to reach a state.
const TURN_LIMIT: Duration = Duration::from_secs(240);

/// Every kernel scenario, in walkthrough order.
pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "k2_sleep_is_free",
        spec: "K2",
        title: "ten sleeping processes cost nothing for an hour",
    },
    Scenario {
        name: "k4_it_proposes_its_own_work",
        spec: "K4",
        title: "a proposed follow-up waits for a person",
    },
    Scenario {
        name: "k5_one_job_many_windows",
        spec: "K5",
        title: "a held call approved from another surface",
    },
];

/// Runs kernel scenario `name`.
pub async fn run(name: &str, models: &Models) -> Result<Checks> {
    match name {
        "k2_sleep_is_free" => k2_sleep_is_free(models).await,
        "k4_it_proposes_its_own_work" => k4_it_proposes_its_own_work(models).await,
        "k5_one_job_many_windows" => k5_one_job_many_windows(models).await,
        other => bail!("unknown kernel scenario {other}"),
    }
}

// --- The kernel ------------------------------------------------------------------------------

/// One kernel home: `harness.db`, the workspace, and the kernel running over them.
struct KernelHome {
    /// Removed when the scenario ends.
    _dir: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
    runs: Arc<SqliteRunStore>,
    sessions: Arc<SqliteSessionService>,
    kernel: Kernel,
    audit: InMemoryAuditRecorder,
}

impl KernelHome {
    /// Starts a kernel over a harness on `model` in `mode`. The run store and the kernel
    /// read `clock`; with `http`, the control plane serves the socket in the home and
    /// loopback HTTP on that address, and without it neither.
    async fn boot(
        model: Arc<dyn Llm>,
        mode: HarnessMode,
        clock: Arc<dyn Clock>,
        http: Option<SocketAddr>,
    ) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let (home, workspace) = (dir.path().join("home"), dir.path().join("ws"));
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&workspace)?;
        let url = format!("sqlite://{}?mode=rwc", home.join("harness.db").display());
        let sessions = SqliteSessionService::new(&url).await?;
        sessions.migrate().await?;
        let pool = sessions.pool().clone();
        let runs = SqliteRunStore::from_pool(pool.clone()).with_clock(clock.clone());
        runs.migrate().await?;
        let approvals = SqliteApprovalStore::from_pool(pool.clone());
        approvals.migrate().await?;
        let schedules = SqliteScheduleStore::from_pool(pool.clone());
        schedules.migrate().await?;
        let processes = adk_kernel::SqliteProcessStore::new(pool.clone());
        processes.migrate().await?;
        let agenda = adk_kernel::SqliteAgendaStore::new(pool);
        agenda.migrate().await?;
        let (sessions, runs, approvals) = (Arc::new(sessions), Arc::new(runs), Arc::new(approvals));

        // Held calls go to the `operator` group, as `adk-rust up` routes them.
        let routing =
            ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval();
        let service: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(approvals.clone())
                .with_router(routing)
                .with_run_store(runs.clone()),
        );
        let audit = InMemoryAuditRecorder::new();
        let agent = HarnessAgent::builder("harness")
            .model(model)
            .workspace(adk_devtools::Workspace::new(&workspace).allow_bash(true))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_default_members()
            .without_compaction()
            .max_iterations(16)
            .mode(mode)
            .approval_store(approvals)
            .approval_service(service)
            .audit_log(Arc::new(audit.clone()))
            .build()?;
        let agent = Arc::new(agent);
        let runner = agent
            .configure_runner(
                Runner::builder()
                    .app_name(APP)
                    .agent(agent.clone())
                    .session_service(sessions.clone() as Arc<dyn SessionService>),
            )
            .build()?;
        let harness = Harness::with_runner(agent, runner)?
            .with_run_store(runs.clone() as Arc<dyn DurableRunStore>)
            .with_goal_queue(QueueClass::BACKGROUND);
        let control = match http {
            Some(addr) => ControlPlaneConfig::default().with_http(addr),
            None => ControlPlaneConfig::default().without_socket(),
        };
        let config = KernelConfig::default()
            .with_app_name(APP)
            .with_home(&home)
            .with_scheduler_tick(Duration::from_millis(250))
            .with_control(control)
            .with_clock(clock);
        let kernel = harness
            .kernel()
            .config(config)
            .process_store(Arc::new(processes))
            .agenda_store(Arc::new(agenda))
            .schedule_store(Arc::new(schedules))
            .build()?;
        kernel.start().await?;
        Ok(Self { _dir: dir, home, workspace, runs, sessions, kernel, audit })
    }

    /// A client on the kernel's socket, as a terminal attaches.
    async fn terminal(&self) -> Result<ControlClient> {
        ControlClient::connect_local(&self.home).await?.context("the kernel's socket")
    }

    /// The run of process `pid`.
    async fn run_of(&self, pid: &str) -> Result<Option<RunRecord>> {
        Ok(self.runs.get(pid).await?)
    }

    /// Every run on the kernel's queues.
    async fn all_runs(&self) -> Result<Vec<RunRecord>> {
        let mut all = Vec::new();
        for queue in QueueClass::defaults() {
            all.extend(self.runs.list(RunFilter::default().queue(queue.as_str())).await?);
        }
        Ok(all)
    }

    /// The events of `session`, which the kernel's worker runs for `user`.
    async fn transcript(&self, user: &str, session: &str) -> Result<Vec<Event>> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: APP.into(),
                user_id: user.into(),
                session_id: session.into(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        Ok(session.events().all())
    }

    /// The actions of the audit records the harness and the kernel wrote.
    fn actions(&self) -> Vec<String> {
        self.audit.records().into_iter().map(|record| record.action).collect()
    }
}

/// Waits until `check` holds, polling every 100 ms, for at most `limit`.
async fn until<F, Fut>(limit: Duration, mut check: F) -> Result<bool>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if check().await? {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(false)
}

/// A free loopback port for the kernel's HTTP control plane.
fn free_port() -> Result<SocketAddr> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?)
}

/// Prints a process's transcript, as the default mode prints a turn.
fn print_transcript(label: &str, events: &[Event]) {
    println!("    {label}:");
    for event in events {
        show(event);
    }
}

/// Whether `events` hold an answer of tool `tool`.
fn answered(events: &[Event], tool: &str) -> bool {
    events.iter().any(|event| {
        event.llm_response.content.as_ref().is_some_and(|content| {
            content.parts.iter().any(|part| {
                matches!(part, adk_core::Part::FunctionResponse { function_response, .. }
                    if function_response.name == tool)
            })
        })
    })
}

// --- K2 --------------------------------------------------------------------------------------

/// What each sleeper is asked when it wakes.
const SLEEPER_PROMPT: &str = "You were asleep on a wake set and it fired. Do not use any tool. \
    Reply with the single word: awake";

/// K2 (spec 3.2 §5): ten processes asleep on ten kinds of wake set make no model call,
/// spend nothing and hold no lease for an hour; `ps` lists all ten `Waiting` with their wake
/// sets. Past the hour, the time wake and the expiry wake their sleepers, which complete on
/// the model. The hour passes on a manual clock the run store and the kernel share.
async fn k2_sleep_is_free(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    let start = Utc::now();
    let clock = ManualClock::new(start);
    let model = models.model(vec![text("awake"), text("awake")]);
    let env = KernelHome::boot(
        model.clone() as Arc<dyn Llm>,
        HarnessMode::Ask,
        Arc::new(clock.clone()),
        None,
    )
    .await?;
    std::fs::write(env.workspace.join("Cargo.lock"), "# lockfile\n")?;

    // The daily cron sleeper fires twelve hours from now, outside the scenario's two hours.
    let cron_hour = (start + chrono::Duration::hours(12)).hour();
    let two_hours = start + chrono::Duration::hours(2);
    let sets = [
        WakeSet::any_of([Wake::Webhook { hook: "github".into() }]),
        WakeSet::any_of([Wake::Queue { source: "orders".into() }]),
        WakeSet::any_of([Wake::FileChange {
            root: env.workspace.clone(),
            glob: "Cargo.lock".into(),
            debounce: Duration::from_millis(100),
        }]),
        WakeSet::any_of([Wake::Cron {
            expr: format!("0 0 {cron_hour} * * *"),
            missed: MissedFirePolicy::Skip,
        }]),
        WakeSet::any_of([Wake::Approval { fingerprint: "sha256:held-call".into() }]),
        WakeSet::any_of([Wake::RunFinished { pid: "run-elsewhere".into() }]),
        WakeSet::any_of([Wake::BudgetReplenished { partition: PartitionId::new("default") }]),
        WakeSet::any_of([Wake::Manual]),
        WakeSet::any_of([Wake::At(two_hours)]),
        WakeSet::any_of([Wake::Webhook { hook: "linear".into() }])
            .with_expiry(two_hours, ExpiryAction::Run),
    ];
    let mut pids = Vec::new();
    for (n, set) in sets.iter().enumerate() {
        let run =
            NewRun::new(QueueClass::BACKGROUND, APP, "ana", APP, json!({ "text": SLEEPER_PROMPT }))
                .with_session_id(format!("k2-sleeper-{n}"));
        let process = env
            .kernel
            .processes()
            .spawn(run, ProcessSpec::new(ProcessKind::Goal).with_wake(set.clone()))
            .await?;
        pids.push(process.meta.pid);
    }
    let asleep = until(Duration::from_secs(20), || async {
        for pid in &pids {
            if env.run_of(pid).await?.is_none_or(|run| run.state != RunState::Waiting) {
                return Ok(false);
            }
        }
        Ok(true)
    })
    .await?;
    checks.check("ten processes sleep on ten kinds of wake set", asleep);

    // An hour in one-minute steps, each long enough for the kernel's tick to sweep.
    let mut leased = Vec::new();
    for minute in 1..=60 {
        clock.advance(Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(25)).await;
        if minute % 10 == 0 {
            leased.extend(
                env.all_runs()
                    .await?
                    .into_iter()
                    .filter(|run| run.lease_owner.is_some())
                    .map(|run| format!("{} at minute {minute}", run.run_id)),
            );
        }
    }
    println!(
        "    an hour passed on the kernel's clock; model requests: {}",
        model.requests().len()
    );
    checks.check("an hour asleep makes no model call", model.requests().is_empty());
    checks.check("no lease is held at any ten-minute mark", leased.is_empty());
    let runs = env.all_runs().await?;
    checks.check(
        "nothing ran but the ten sleepers, all `Waiting` without a lease",
        runs.len() == sets.len()
            && runs.iter().all(|run| run.state == RunState::Waiting && run.lease_owner.is_none()),
    );
    let waiting =
        env.kernel.processes().ps(ProcessFilter::default().states([RunState::Waiting])).await?;
    let shown = pids.iter().zip(&sets).all(|(pid, set)| {
        waiting
            .iter()
            .any(|process| &process.meta.pid == pid && process.meta.wake.as_ref() == Some(set))
    });
    checks.check("`ps` lists all ten `Waiting` with their wake sets", waiting.len() == 10 && shown);
    let spend: u64 = waiting.iter().map(|process| process.view.spend_today).sum();
    let calls: u64 = waiting.iter().map(|process| process.view.model_calls).sum();
    println!(
        "    ps: {} waiting, spend today {spend} micro-USD, model calls {calls}",
        waiting.len()
    );
    checks.check("no process spent anything or called the model", spend == 0 && calls == 0);
    checks.check(
        "each has slept the hour",
        waiting.iter().all(|process| process.view.age >= Duration::from_secs(59 * 60)),
    );

    // Past two hours on the same clock: the time wake and the expiry fire.
    clock.advance(Duration::from_secs(61 * 60));
    let woken = &pids[8..];
    let completed = until(TURN_LIMIT, || async {
        for pid in woken {
            if env.run_of(pid).await?.is_none_or(|run| run.state != RunState::Completed) {
                return Ok(false);
            }
        }
        Ok(true)
    })
    .await?;
    for n in 8..10 {
        print_transcript(
            &format!("sleeper {n}, woken"),
            &env.transcript("ana", &format!("k2-sleeper-{n}")).await?,
        );
    }
    checks.check(
        "the time wake and the expiry woke their sleepers, which completed on the model",
        completed,
    );
    let all = env.kernel.processes().ps(ProcessFilter::default()).await?;
    let calls_of = |pid: &str| {
        all.iter()
            .find(|process| process.meta.pid == pid)
            .map_or(0, |process| process.view.model_calls)
    };
    checks.check(
        "the woken processes called the model; the others did not",
        !model.requests().is_empty()
            && woken.iter().all(|pid| calls_of(pid) > 0)
            && pids[..8].iter().all(|pid| calls_of(pid) == 0),
    );
    let mut still = true;
    for pid in &pids[..8] {
        still &= env
            .run_of(pid)
            .await?
            .is_some_and(|run| run.state == RunState::Waiting && run.lease_owner.is_none());
    }
    checks.check("the other eight still sleep without a lease", still);
    env.kernel.stop().await?;
    Ok(checks)
}

// --- K4 --------------------------------------------------------------------------------------

/// The turn that proposes a follow-up.
const K4_PROMPT: &str = "The dependency bump lands tonight. Do not do anything about it now. \
    Call the propose_followup tool once, with the title \"Write a status note about the \
    dependency bump\" and a one-sentence rationale, asking for a reply-only note that needs \
    no tools. Then reply with the single word: proposed";

/// The agenda item `pid` proposed, and the process it became, if any.
async fn followup_of(
    client: &ControlClient,
    pid: &str,
) -> Result<Option<(AgendaItem, Option<Process>)>> {
    let items = client.agenda(&AgendaQuery::default()).await?;
    let Some(item) = items.into_iter().find(|item| item.proposer() == Some(pid)) else {
        return Ok(None);
    };
    let processes = client.processes(&ProcessesQuery::default()).await?;
    let process = processes
        .into_iter()
        .find(|process| process.meta.agenda_item.as_deref() == Some(item.id.as_str()));
    Ok(Some((item, process)))
}

/// K4 (spec 3.2 §5): in Ask, the model's `propose_followup` puts a `proposed` item on the
/// agenda and in the inbox, and nothing runs; a person approves it from the inbox over
/// loopback HTTP; it runs as a `Proposed` process whose parent is the proposing turn.
async fn k4_it_proposes_its_own_work(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    let model = models.model(vec![
        call(
            "propose_followup",
            json!({
                "title": "Write a status note about the dependency bump",
                "rationale": "The bump lands tonight; a reply-only note that needs no tools records it.",
            }),
        ),
        text("proposed"),
        text("Status note: the dependency bump lands tonight."),
    ]);
    let addr = free_port()?;
    let env = KernelHome::boot(
        model as Arc<dyn Llm>,
        HarnessMode::Ask,
        Arc::new(SystemClock),
        Some(addr),
    )
    .await?;
    let terminal = env.terminal().await?;
    let request = SpawnRequest::new(APP, json!({ "text": K4_PROMPT })).with_session_id("k4");
    println!("    [terminal] spawn: {K4_PROMPT}");
    let pid = terminal.spawn(&request).await?.meta.pid;
    let proposed = until(TURN_LIMIT, || {
        let (terminal, pid) = (terminal.clone(), pid.clone());
        async move { Ok(followup_of(&terminal, &pid).await?.is_some()) }
    })
    .await?;
    let attached = terminal.attach("k4", &AttachRequest::default()).await?;
    print_transcript("the proposing turn", &attached.transcript);
    checks.check("the turn's proposal is on the agenda", proposed);
    let Some((item, _)) = followup_of(&terminal, &pid).await? else { return Ok(checks) };
    println!("    agenda: {} \"{}\" ({:?})", &item.id[..8], item.title, item.state);

    tokio::time::sleep(Duration::from_secs(2)).await;
    let waiting = followup_of(&terminal, &pid).await?;
    checks.check(
        "in Ask it stays proposed and nothing runs before a decision",
        waiting.as_ref().is_some_and(|(item, process)| {
            item.state == AgendaState::Proposed && process.is_none()
        }),
    );

    // A phone on loopback HTTP: another client, holding nothing.
    let phone = ControlClient::http(format!("http://{addr}"));
    let inbox = phone.approvals().await?;
    let request = inbox.iter().find(|entry| entry.tool_name == adk_kernel::AGENDA_TOOL_NAME);
    checks.check(
        "the inbox asks for it with approve, edit and reject",
        request.is_some_and(|entry| {
            entry.allowed_actions
                == [ApprovalAction::Approve, ApprovalAction::Edit, ApprovalAction::Reject]
        }),
    );
    if let Some(entry) = request {
        println!("    [phone] approve the follow-up from the inbox");
        phone.decide(&entry.id, entry.version, ApprovalAction::Approve, None).await?;
    }

    // The follow-up runs in Ask too: a call it holds waits for a person, who rejects it.
    let follow_session = adk_kernel::agenda::item_session(&item.id);
    let rejected = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let done = until(TURN_LIMIT, || {
        let (phone, pid) = (phone.clone(), pid.clone());
        let (follow_session, rejected) = (follow_session.clone(), rejected.clone());
        async move {
            for held in phone.approvals().await? {
                if held.scope.session_id == follow_session {
                    phone
                        .decide(&held.id, held.version, ApprovalAction::Reject, Some("not now"))
                        .await?;
                    rejected.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Ok(followup_of(&phone, &pid)
                .await?
                .is_some_and(|(item, _)| item.state == AgendaState::Done))
        }
    })
    .await?;
    let rejected = rejected.load(std::sync::atomic::Ordering::SeqCst);
    if rejected > 0 {
        println!("    [phone] rejected {rejected} call(s) the follow-up held");
    }
    checks.check("approved from the inbox, the follow-up ran", done);
    let followup = followup_of(&phone, &pid).await?;
    let process = followup.and_then(|(_, process)| process);
    if let Some(process) = &process {
        let session = process.run.session_id.clone().unwrap_or_default();
        let attached = phone.attach(&session, &AttachRequest::default()).await?;
        print_transcript("the follow-up", &attached.transcript);
    }
    checks.check(
        "it ran as a `Proposed` process whose parent is the proposing turn",
        process.as_ref().is_some_and(|process| {
            process.meta.kind == ProcessKind::Proposed
                && process.meta.parent.as_deref() == Some(pid.as_str())
                && process.run.state == RunState::Completed
        }),
    );
    let actions = env.actions();
    checks.check(
        "the proposal and the approval are audited",
        [adk_kernel::audit::AGENDA_PROPOSED, adk_kernel::audit::AGENDA_APPROVED]
            .iter()
            .all(|action| actions.iter().any(|recorded| recorded == action)),
    );
    env.kernel.stop().await?;
    Ok(checks)
}

// --- K5 --------------------------------------------------------------------------------------

/// The turn whose `bash` call is held.
const K5_PROMPT: &str = "Use the bash tool to run exactly this command, once: \
    echo kernel >> marker.txt\nThen reply with the single word: done";

/// The calls `session` holds, or none while the kernel's worker has not created it yet.
async fn pending_in(
    client: &ControlClient,
    session: &str,
) -> Result<Vec<adk_core::ToolConfirmationRequest>> {
    match client.attach(session, &AttachRequest::default()).await {
        Ok(attached) => Ok(attached.pending),
        Err(error) if error.category() == adk_core::ErrorCategory::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

/// K5 (spec 3.2 §5): a turn spawned from a terminal on the socket holds its `bash` call;
/// the terminal goes away; a phone on loopback HTTP approves the call from the inbox; the
/// run resumes and the command runs once with no terminal attached; a new shell attaches
/// and sees the same process, completed, and the call's answer in the transcript.
async fn k5_one_job_many_windows(models: &Models) -> Result<Checks> {
    let mut checks = Checks::default();
    let model = models
        .model(vec![call("bash", json!({ "command": "echo kernel >> marker.txt" })), text("done")]);
    let addr = free_port()?;
    let env = KernelHome::boot(
        model as Arc<dyn Llm>,
        HarnessMode::Ask,
        Arc::new(SystemClock),
        Some(addr),
    )
    .await?;
    let marker = env.workspace.join("marker.txt");

    // The terminal, on the socket.
    let terminal = env.terminal().await?;
    let request = SpawnRequest::new(APP, json!({ "text": K5_PROMPT })).with_session_id("k5");
    println!("    [terminal] spawn: {}", K5_PROMPT.replace('\n', " "));
    let pid = terminal.spawn(&request).await?.meta.pid;
    let held = until(TURN_LIMIT, || {
        let terminal = terminal.clone();
        async move { Ok(!pending_in(&terminal, "k5").await?.is_empty()) }
    })
    .await?;
    checks.check("the bash call is held for a decision", held);
    checks.check("nothing ran before the decision", !marker.exists());
    // The laptop lid closes.
    drop(terminal);
    println!("    [terminal] gone");

    // The phone, on HTTP: another client, holding nothing.
    let phone = ControlClient::http(format!("http://{addr}"));
    let inbox = phone.approvals().await?;
    let item = inbox.iter().find(|item| item.scope.session_id == "k5");
    checks.check("the phone sees the held call in the inbox", item.is_some());
    if let Some(item) = item {
        println!("    [phone] approve {} from the inbox", item.tool_name);
        phone.decide(&item.id, item.version, ApprovalAction::Approve, None).await?;
    }
    let finished = until(TURN_LIMIT, || async {
        Ok(env.run_of(&pid).await?.is_some_and(|run| run.state.is_terminal()))
    })
    .await?;
    checks.check(
        "the run resumed and completed with no terminal attached",
        finished && env.run_of(&pid).await?.is_some_and(|run| run.state == RunState::Completed),
    );
    checks.check(
        "the command ran once",
        std::fs::read_to_string(&marker).unwrap_or_default() == "kernel\n",
    );

    // A new shell attaches and sees the same process and transcript.
    let shell = env.terminal().await?;
    let attached = shell.attach("k5", &AttachRequest::default()).await?;
    print_transcript("the transcript a new shell attaches to", &attached.transcript);
    checks.check(
        "the new shell sees the call's answer in the transcript",
        answered(&attached.transcript, "bash"),
    );
    checks.check("nothing is held any more", attached.pending.is_empty());
    let listed = shell.processes(&ProcessesQuery::default()).await?;
    checks.check(
        "the new shell sees the same process, completed",
        listed
            .iter()
            .any(|process| process.meta.pid == pid && process.run.state == RunState::Completed),
    );
    let controls: Vec<(String, String)> = env
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == adk_kernel::audit::CONTROL)
        .map(|record| {
            let text = |key: &str| {
                record.ext.get(key).and_then(|value| value.as_str()).unwrap_or_default().to_string()
            };
            (text("action"), text("transport"))
        })
        .collect();
    checks.check(
        "the spawn and the attaches are recorded with their transport",
        controls.contains(&("spawn".into(), "socket".into()))
            && controls.contains(&("attach".into(), "socket".into())),
    );
    env.kernel.stop().await?;
    Ok(checks)
}
