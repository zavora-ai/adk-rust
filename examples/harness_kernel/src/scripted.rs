//! K1, K4, K5, K6, K7 and K8 on `ScriptedLlm` with a real kernel (spec 3.2 §5, §11): the
//! kernel's durable workers, its scheduler, SQLite stores in `harness.db`, and its control
//! plane on a socket and loopback HTTP, composed as `adk-rust up` composes them, through
//! `Harness::kernel`.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_auth::signed_audit::{AuditExport, AuditLog, AuditSigner, Ed25519Signer, NoteVerifier};
use adk_core::audit_log::{AuditRecorder, InMemoryAuditRecorder};
use adk_core::{
    ApprovalAction, ApprovalPolicy, ApprovalService, ApproverSelector, Clock, DurableRunStore,
    ManualClock, MissedFirePolicy, NewRun, RunFilter, RunState, Schedule, ScheduleSpec,
    ScheduleStore, StoreApprovalService, SystemClock,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::api::{AttachRequest, ProcessesQuery, SpawnRequest};
use adk_kernel::control::{ControlClient, ControlPlaneConfig};
use adk_kernel::drivers::{DriverManifest, DriverState, DriverSupervisor};
use adk_kernel::{Kernel, KernelConfig, KernelEvent, PartitionId, ProcessFilter, QueueClass};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::{
    SqliteApprovalStore, SqliteAuditLogStore, SqliteRunStore, SqliteScheduleStore,
    SqliteSessionService,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::json;

use crate::Checks;

/// The app and target the harness runs under, as `adk-rust up` registers it.
const APP: &str = "adk-rust";

pub async fn run(name: &str) -> Result<Checks> {
    match name {
        "k1_leave_it_running" => k1_leave_it_running().await,
        "k4_it_proposes_its_own_work" => k4_it_proposes_its_own_work().await,
        "k5_one_job_many_windows" => k5_one_job_many_windows().await,
        "k8_a_message_preempts_background_work" => k8_a_message_preempts_background_work().await,
        "k6_drivers_you_can_trust" => k6_drivers_you_can_trust().await,
        "k7_partitions" => k7_partitions().await,
        "k7_driver_crossings" => k7_driver_crossings().await,
        other => bail!("unknown scenario {other}"),
    }
}

/// The stores of one kernel home, in `harness.db` as `adk-rust up` keeps them.
pub(crate) struct Stores {
    pub(crate) sessions: Arc<SqliteSessionService>,
    pub(crate) runs: Arc<SqliteRunStore>,
    pub(crate) approvals: Arc<SqliteApprovalStore>,
    pub(crate) schedules: Arc<SqliteScheduleStore>,
    pub(crate) processes: Arc<adk_kernel::SqliteProcessStore>,
    pub(crate) agenda: Arc<adk_kernel::SqliteAgendaStore>,
    logs: Arc<SqliteAuditLogStore>,
}

/// The stores of `home`, the run store on `clock`, the clock the kernel reads too: the run
/// store decides leases and `not_before`, the kernel wake times and due schedules.
pub(crate) async fn stores(home: &Path, clock: Arc<dyn Clock>) -> Result<Stores> {
    std::fs::create_dir_all(home)?;
    let url = format!("sqlite://{}?mode=rwc", home.join("harness.db").display());
    let sessions = SqliteSessionService::new(&url).await?;
    sessions.migrate().await?;
    let pool = sessions.pool().clone();
    let runs = SqliteRunStore::from_pool(pool.clone()).with_clock(clock);
    runs.migrate().await?;
    let approvals = SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await?;
    let schedules = SqliteScheduleStore::from_pool(pool.clone());
    schedules.migrate().await?;
    let processes = adk_kernel::SqliteProcessStore::new(pool.clone());
    processes.migrate().await?;
    let agenda = adk_kernel::SqliteAgendaStore::new(pool.clone());
    agenda.migrate().await?;
    let logs = SqliteAuditLogStore::from_pool(pool);
    logs.migrate().await?;
    Ok(Stores {
        sessions: Arc::new(sessions),
        runs: Arc::new(runs),
        approvals: Arc::new(approvals),
        schedules: Arc::new(schedules),
        processes: Arc::new(processes),
        agenda: Arc::new(agenda),
        logs: Arc::new(logs),
    })
}

fn scripted(replies: Vec<ScriptedResponse>) -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm::new("scripted", replies))
}

/// How one boot of the kernel runs.
pub(crate) struct Boot {
    pub(crate) model: Arc<dyn adk_core::Llm>,
    pub(crate) mode: HarnessMode,
    pub(crate) control: ControlPlaneConfig,
    pub(crate) audit: Arc<dyn AuditRecorder>,
    pub(crate) clock: Arc<dyn Clock>,
    /// The kernel's proactivity policy (spec 3.2 §7.5); `None` keeps the default.
    pub(crate) proactivity: Option<adk_kernel::ProactivityPolicy>,
    /// Every partition's quotas (spec 3.2 §7.9); `None` sets none.
    pub(crate) quotas: Option<adk_kernel::Quotas>,
    /// The kernel's drivers, served to the default partition (K6).
    pub(crate) drivers: Option<DriverSupervisor>,
}

/// One boot of the kernel over `stores`: the harness, held calls routed to the `operator`
/// group, built with `Harness::kernel` (spec 3.2 §10 HK1) as `adk-rust up` builds it.
pub(crate) fn boot(stores: &Stores, home: &Path, workspace: &Path, boot: Boot) -> Result<Kernel> {
    let routing = ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(stores.approvals.clone())
            .with_router(routing)
            .with_run_store(stores.runs.clone()),
    );
    let agent = HarnessAgent::builder(APP)
        .model(boot.model)
        .workspace(adk_devtools::Workspace::new(workspace).allow_bash(true))
        .project_instructions(InstructionsConfig {
            user_file: None,
            ..InstructionsConfig::default()
        })
        .without_default_members()
        .mode(boot.mode)
        .approval_store(stores.approvals.clone())
        .approval_service(service)
        .audit_log(boot.audit);
    let agent = match &boot.drivers {
        Some(drivers) => {
            agent.kernel_drivers(drivers, &PartitionId::default(), &["notes".to_string()])
        }
        None => agent,
    };
    let agent = Arc::new(agent.build()?);
    let runner = adk_runner::Runner::builder()
        .app_name(APP)
        .agent(agent.agent())
        .session_service(stores.sessions.clone())
        .approval_store(agent.runtime().approval_store().clone())
        .build()?;
    let harness = Harness::with_runner(agent, runner)?
        .with_run_store(stores.runs.clone())
        .with_goal_queue(QueueClass::BACKGROUND);
    if let Some(drivers) = &boot.drivers {
        drivers.set_rule_pins(harness.driver_rule_pins());
    }
    let config = KernelConfig::default()
        .with_app_name(APP)
        .with_home(home)
        .with_scheduler_tick(Duration::from_millis(250))
        .with_control(boot.control)
        .with_clock(boot.clock)
        .with_proactivity(boot.proactivity.unwrap_or_default())
        .with_quotas(boot.quotas.unwrap_or_default());
    let mut builder = harness
        .kernel()
        .config(config)
        .process_store(stores.processes.clone())
        .agenda_store(stores.agenda.clone())
        .schedule_store(stores.schedules.clone());
    if let Some(drivers) = boot.drivers {
        builder = builder.drivers(drivers);
    }
    Ok(builder.build()?)
}

/// Waits until `check` holds, polling every 100 ms, for at most `limit`.
pub(crate) async fn until<F, Fut>(limit: Duration, mut check: F) -> Result<bool>
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

/// The runs of the nightly schedule.
async fn nightly(stores: &Stores) -> Result<Vec<adk_core::RunRecord>> {
    Ok(stores.runs.list(RunFilter::default().session_id("nightly")).await?)
}

/// K1 without service installation (spec 3.2 §5): a nightly audit is scheduled, the host
/// goes down before its time and comes back the next morning; the next boot runs it once,
/// the process table shows it `Completed`, and the signed audit log verifies. The night
/// passes on a manual clock the run store and the kernel share.
async fn k1_leave_it_running() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace) = (dir.path().join("home"), dir.path().join("ws"));
    std::fs::create_dir_all(&workspace)?;
    let manual = Arc::new(ManualClock::new(Utc::now()));
    let clock: Arc<dyn Clock> = manual.clone();
    let stores = stores(&home, clock.clone()).await?;
    let signer = Arc::new(Ed25519Signer::generate("kernel-k1")?);
    let control = ControlPlaneConfig::default().without_socket();
    let fire_at = clock.now() + chrono::Duration::hours(2);

    // First boot, in the evening: the schedule is recorded, then the host goes down.
    {
        let log =
            AuditLog::builder(stores.logs.clone(), "kernel-k1", signer.clone()).open().await?;
        let first = Boot {
            model: scripted(Vec::new()),
            mode: HarnessMode::Auto,
            control: control.clone(),
            audit: Arc::new(log),
            clock: clock.clone(),
            proactivity: None,
            quotas: None,
            drivers: None,
        };
        let kernel = boot(&stores, &home, &workspace, first)?;
        kernel.start().await?;
        let run =
            NewRun::new("background", APP, "ana", APP, json!({ "text": "audit the repository" }))
                .with_session_id("nightly");
        let schedule = Schedule::new(
            "nightly-audit",
            ScheduleSpec::At(fire_at),
            run,
            MissedFirePolicy::FireOnce,
        )
        .with_next_fire_at(fire_at);
        stores.schedules.upsert(schedule).await?;
        kernel.stop().await?;
    }
    // The host is down overnight, through the schedule's time.
    manual.advance(Duration::from_secs(8 * 60 * 60));
    checks.check("nothing ran while the host was down", nightly(&stores).await?.is_empty());

    // Second boot, the next morning: the missed firing runs once.
    let log =
        Arc::new(AuditLog::builder(stores.logs.clone(), "kernel-k1", signer.clone()).open().await?);
    let second = Boot {
        model: scripted(vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "audit.txt", "content": "audited" }),
            ),
            ScriptedResponse::text("audited"),
        ]),
        mode: HarnessMode::Auto,
        control,
        audit: log.clone(),
        clock,
        proactivity: None,
        quotas: None,
        drivers: None,
    };
    let kernel = boot(&stores, &home, &workspace, second)?;
    kernel.start().await?;
    let completed = until(Duration::from_secs(30), || async {
        Ok(nightly(&stores).await?.iter().any(|run| run.state.is_terminal()))
    })
    .await?;
    // A second firing would show up within a few scheduler ticks.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let runs = nightly(&stores).await?;
    checks.check("the next boot ran the missed firing", completed);
    checks.check("it ran exactly once", runs.len() == 1);
    checks.check(
        "the run completed",
        runs.first().is_some_and(|run| run.state == RunState::Completed),
    );
    checks.check(
        "the audit was written once",
        std::fs::read_to_string(workspace.join("audit.txt")).unwrap_or_default() == "audited",
    );
    let processes = kernel.processes().ps(ProcessFilter::default()).await?;
    checks.check(
        "the process table shows it completed",
        runs.first().is_some_and(|run| {
            processes.iter().any(|process| {
                process.meta.pid == run.run_id && process.run.state == RunState::Completed
            })
        }),
    );
    kernel.stop().await?;
    // The replica signs a checkpoint over what it recorded, as it does every 256 records or
    // every minute.
    log.checkpoint().await?;
    let verifier: NoteVerifier = signer.verifier_key().parse().context("verifier key")?;
    let export = AuditExport::from_store(stores.logs.as_ref(), "kernel-k1").await?;
    let report = export.verify(&[verifier]);
    let records: Vec<serde_json::Value> = export
        .entries
        .iter()
        .filter_map(|entry| entry.record.as_deref())
        .filter_map(|record| serde_json::from_str(record).ok())
        .collect();
    checks.check("the signed audit log verifies across the restart", report.is_valid());
    checks.check(
        "the log records the write",
        records.iter().any(|record| record["action"] == "write_file"),
    );
    Ok(checks)
}

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

/// A free loopback port for the kernel's HTTP control plane.
fn free_port() -> Result<SocketAddr> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?)
}

/// K5 (spec 3.2 §5): a turn starts from a terminal on the socket and holds a `bash` call;
/// the terminal goes away; a phone on HTTP approves the call; the run resumes and the
/// command runs once without the terminal; a new shell attaches and sees the same process
/// and transcript.
async fn k5_one_job_many_windows() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace) = (dir.path().join("home"), dir.path().join("ws"));
    std::fs::create_dir_all(&workspace)?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let stores = stores(&home, clock.clone()).await?;
    let addr = free_port()?;
    let recorder = InMemoryAuditRecorder::new();
    let setup = Boot {
        model: scripted(vec![
            ScriptedResponse::function_call(
                "bash",
                json!({ "command": "echo once >> marker.txt" }),
            ),
            ScriptedResponse::text("done"),
        ]),
        mode: HarnessMode::Ask,
        control: ControlPlaneConfig::default().with_http(addr),
        audit: Arc::new(recorder.clone()),
        clock,
        proactivity: None,
        quotas: None,
        drivers: None,
    };
    let kernel = boot(&stores, &home, &workspace, setup)?;
    kernel.start().await?;

    // The terminal, on the socket.
    let terminal = ControlClient::connect_local(&home).await?.context("the kernel's socket")?;
    let request =
        SpawnRequest::new(APP, json!({ "text": "run the marker command" })).with_session_id("k5");
    let pid = terminal.spawn(&request).await?.meta.pid;
    let held = until(Duration::from_secs(20), || {
        let terminal = terminal.clone();
        async move { Ok(!pending_in(&terminal, "k5").await?.is_empty()) }
    })
    .await?;
    checks.check("the bash call is held for a decision", held);
    checks.check("nothing ran before the decision", !workspace.join("marker.txt").exists());
    // The laptop lid closes.
    drop(terminal);

    // The phone, on HTTP: another client, holding nothing.
    let phone = ControlClient::http(format!("http://{addr}"));
    let pending = phone.approvals().await?;
    let item = pending.iter().find(|item| item.scope.session_id == "k5");
    checks.check("the phone sees the held call in the inbox", item.is_some());
    if let Some(item) = item {
        phone.decide(&item.id, item.version, ApprovalAction::Approve, None).await?;
    }
    let finished = until(Duration::from_secs(20), || {
        let runs = stores.runs.clone();
        let pid = pid.clone();
        async move { Ok(runs.get(&pid).await?.is_some_and(|run| run.state.is_terminal())) }
    })
    .await?;
    let run = stores.runs.get(&pid).await?;
    checks.check(
        "the run resumed and completed with no terminal attached",
        finished && run.is_some_and(|run| run.state == RunState::Completed),
    );
    checks.check(
        "the command ran once",
        std::fs::read_to_string(workspace.join("marker.txt")).unwrap_or_default() == "once\n",
    );

    // A new shell attaches and sees the same process and transcript.
    let shell = ControlClient::connect_local(&home).await?.context("the kernel's socket")?;
    let attached = shell.attach("k5", &AttachRequest::default()).await?;
    let answered = attached.transcript.iter().any(|event| {
        event.llm_response.content.as_ref().is_some_and(|content| {
            content.parts.iter().any(|part| {
                matches!(part, adk_core::Part::FunctionResponse { function_response, .. } if function_response.name == "bash")
            })
        })
    });
    checks.check("the new shell sees the call's answer in the transcript", answered);
    checks.check("nothing is held any more", attached.pending.is_empty());
    let listed = shell.processes(&ProcessesQuery::default()).await?;
    checks.check(
        "the new shell sees the same process, completed",
        listed
            .iter()
            .any(|process| process.meta.pid == pid && process.run.state == RunState::Completed),
    );
    let controls: Vec<(String, String)> = recorder
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
    kernel.stop().await?;
    Ok(checks)
}

/// The `propose_followup` call a turn makes, then its answer.
fn proposing(title: &str) -> Vec<ScriptedResponse> {
    vec![
        ScriptedResponse::function_call(
            "propose_followup",
            json!({ "title": title, "rationale": "the dependency bump lands tonight" }),
        ),
        ScriptedResponse::text("I proposed a follow-up."),
        ScriptedResponse::text("Re-ran the tests: they pass."),
    ]
}

/// The agenda item `pid` proposed, and the process it became, if any.
async fn followup_of(
    client: &ControlClient,
    pid: &str,
) -> Result<Option<(adk_kernel::AgendaItem, Option<adk_kernel::Process>)>> {
    let items = client.agenda(&adk_kernel::control::api::AgendaQuery::default()).await?;
    let Some(item) = items.into_iter().find(|item| item.proposer() == Some(pid)) else {
        return Ok(None);
    };
    let processes = client.processes(&ProcessesQuery::default()).await?;
    let process = processes
        .into_iter()
        .find(|process| process.meta.agenda_item.as_deref() == Some(item.id.as_str()));
    Ok(Some((item, process)))
}

/// K4 (spec 3.2 §5): the agent proposes "re-run the tests after the dependency bump
/// lands". In Ask the proposal is `Proposed` in the agenda and the inbox, and nothing runs
/// until a person approves it from another surface; then it runs as a `Proposed` process
/// whose parent is the proposing turn. In Auto it is scheduled by the policy and runs only
/// once quiet hours end, on a clock the kernel and the run store share.
async fn k4_it_proposes_its_own_work() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace) = (dir.path().join("home"), dir.path().join("ws"));
    std::fs::create_dir_all(&workspace)?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let ask_stores = stores(&home, clock.clone()).await?;
    let addr = free_port()?;
    let recorder = InMemoryAuditRecorder::new();
    let ask = Boot {
        model: scripted(proposing("Re-run the tests after the dependency bump lands")),
        mode: HarnessMode::Ask,
        control: ControlPlaneConfig::default().with_http(addr),
        audit: Arc::new(recorder.clone()),
        clock,
        proactivity: None,
        quotas: None,
        drivers: None,
    };
    let kernel = boot(&ask_stores, &home, &workspace, ask)?;
    kernel.start().await?;
    let terminal = ControlClient::connect_local(&home).await?.context("the kernel's socket")?;
    let request =
        SpawnRequest::new(APP, json!({ "text": "bump the dependency" })).with_session_id("k4-ask");
    let pid = terminal.spawn(&request).await?.meta.pid;
    let proposed = until(Duration::from_secs(20), || {
        let terminal = terminal.clone();
        let pid = pid.clone();
        async move { Ok(followup_of(&terminal, &pid).await?.is_some()) }
    })
    .await?;
    checks.check("the turn's proposal is on the agenda", proposed);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let followup = followup_of(&terminal, &pid).await?;
    checks.check(
        "in Ask it stays proposed and nothing runs before a decision",
        followup.as_ref().is_some_and(|(item, process)| {
            item.state == adk_kernel::AgendaState::Proposed && process.is_none()
        }),
    );
    let phone = ControlClient::http(format!("http://{addr}"));
    let inbox = phone.approvals().await?;
    let request = inbox.iter().find(|item| item.tool_name == adk_kernel::AGENDA_TOOL_NAME);
    checks.check(
        "the inbox asks for it with approve, edit and reject",
        request.is_some_and(|item| {
            item.allowed_actions
                == [ApprovalAction::Approve, ApprovalAction::Edit, ApprovalAction::Reject]
        }),
    );
    if let Some(item) = request {
        phone.decide(&item.id, item.version, ApprovalAction::Approve, None).await?;
    }
    let done = until(Duration::from_secs(20), || {
        let phone = phone.clone();
        let pid = pid.clone();
        async move {
            Ok(followup_of(&phone, &pid)
                .await?
                .is_some_and(|(item, _)| item.state == adk_kernel::AgendaState::Done))
        }
    })
    .await?;
    let followup = followup_of(&phone, &pid).await?;
    checks.check("approved from the phone, the follow-up ran", done);
    checks.check(
        "it ran as a `Proposed` process whose parent is the proposing turn",
        followup.as_ref().and_then(|(_, process)| process.as_ref()).is_some_and(|process| {
            process.meta.kind == adk_kernel::ProcessKind::Proposed
                && process.meta.parent.as_deref() == Some(pid.as_str())
                && process.run.state == RunState::Completed
        }),
    );
    let actions: Vec<String> = recorder.records().into_iter().map(|record| record.action).collect();
    checks.check(
        "the proposal and the approval are audited",
        [adk_kernel::audit::AGENDA_PROPOSED, adk_kernel::audit::AGENDA_APPROVED]
            .iter()
            .all(|action| actions.iter().any(|recorded| recorded == action)),
    );
    kernel.stop().await?;

    // Auto, at night: the policy schedules the follow-up for when quiet hours end.
    let night = Utc::now().date_naive().and_hms_opt(23, 0, 0).context("23:00")?.and_utc();
    let manual = Arc::new(ManualClock::new(night));
    let clock: Arc<dyn Clock> = manual.clone();
    let home = dir.path().join("home-auto");
    let auto_stores = stores(&home, clock.clone()).await?;
    let quiet = "22:00-07:00".parse::<adk_kernel::QuietHours>()?.with_utc_offset_minutes(0);
    let auto = Boot {
        model: scripted(proposing("Re-run the tests")),
        mode: HarnessMode::Auto,
        control: ControlPlaneConfig::default().without_socket(),
        audit: Arc::new(InMemoryAuditRecorder::new()),
        clock,
        proactivity: Some(adk_kernel::ProactivityPolicy::default().with_quiet_hours(quiet)),
        quotas: None,
        drivers: None,
    };
    let kernel = boot(&auto_stores, &home, &workspace, auto)?;
    kernel.start().await?;
    let turn = kernel
        .processes()
        .spawn(
            NewRun::new("interactive", APP, "ana", APP, json!({ "text": "bump the dependency" }))
                .with_session_id("k4-auto"),
            adk_kernel::ProcessSpec::new(adk_kernel::ProcessKind::Interactive),
        )
        .await?;
    let scheduled = until(Duration::from_secs(20), || async {
        let items = kernel.agenda().list(&adk_kernel::AgendaFilter::default()).await?;
        Ok(items.iter().any(|item| matches!(item.state, adk_kernel::AgendaState::Scheduled { .. })))
    })
    .await?;
    checks.check("in Auto the policy schedules the proposal", scheduled);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let item = kernel.agenda().list(&adk_kernel::AgendaFilter::default()).await?.pop();
    checks.check(
        "nothing starts inside quiet hours",
        item.as_ref()
            .is_some_and(|item| matches!(item.state, adk_kernel::AgendaState::Scheduled { .. })),
    );
    manual.advance(Duration::from_secs(8 * 60 * 60 + 60)); // 07:01
    let done = until(Duration::from_secs(20), || async {
        let items = kernel.agenda().list(&adk_kernel::AgendaFilter::default()).await?;
        Ok(items.iter().any(|item| item.state == adk_kernel::AgendaState::Done))
    })
    .await?;
    checks.check("it runs once quiet hours end", done);
    let parent = kernel
        .processes()
        .ps(ProcessFilter::default())
        .await?
        .into_iter()
        .find(|process| process.meta.kind == adk_kernel::ProcessKind::Proposed);
    checks.check(
        "its parent is the proposing turn",
        parent
            .is_some_and(|process| process.meta.parent.as_deref() == Some(turn.meta.pid.as_str())),
    );
    kernel.stop().await?;
    Ok(checks)
}

/// A model whose answers follow the turn's first line: each prompt key has its own script,
/// and the first call of a held key waits until the scenario releases it.
struct PromptModel {
    scripts: Vec<(&'static str, Arc<ScriptedLlm>, Arc<tokio::sync::Semaphore>, bool)>,
    waiting: std::sync::atomic::AtomicUsize,
}

impl PromptModel {
    fn release(&self, key: &str) {
        if let Some((_, _, gate, _)) = self.scripts.iter().find(|(name, ..)| *name == key) {
            gate.add_permits(1);
        }
    }

    fn calls(&self, key: &str) -> usize {
        self.scripts
            .iter()
            .find(|(name, ..)| *name == key)
            .map_or(0, |(_, model, ..)| model.requests().len())
    }
}

#[adk_core::async_trait]
impl adk_core::Llm for PromptModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(
        &self,
        request: adk_core::LlmRequest,
        stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        let prompt = request
            .contents
            .iter()
            .filter(|content| content.role == "user")
            .flat_map(|content| content.parts.iter())
            .find_map(|part| match part {
                adk_core::Part::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let Some((_, model, gate, held)) =
            self.scripts.iter().find(|(key, ..)| prompt.contains(key))
        else {
            return Err(adk_core::AdkError::model(format!("no script for {prompt:?}")));
        };
        if *held && model.requests().is_empty() {
            self.waiting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            gate.acquire()
                .await
                .map_err(|error| adk_core::AdkError::model(error.to_string()))?
                .forget();
            self.waiting.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
        model.generate_content(request, stream).await
    }
}

/// Samples the admitted processes every 20 ms into `peak` while `sampling` holds: running,
/// claimed — the kernel publishes a claim after admission decided it — and not on their way
/// to park. A run is `Running` from its claim, before admission preempts or defers anything,
/// so a run whose claim is not yet published is not counted. The claims are read before the
/// process table: a claim seen there was published after admission wrote the preemption it
/// decided, so the table read that follows already shows the preempted process parking.
pub(crate) fn admitted_sampler(
    kernel: &Kernel,
    sampling: Arc<std::sync::atomic::AtomicBool>,
    peak: Arc<std::sync::atomic::AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    use std::sync::atomic::Ordering;
    let claimed = Arc::new(std::sync::Mutex::new(std::collections::HashSet::<String>::new()));
    let listener = {
        let (mut events, claimed) = (kernel.events(), claimed.clone());
        tokio::spawn(async move {
            while let Some(record) = events.recv().await {
                if let KernelEvent::Process { pid, to, .. } = record.event {
                    let mut claimed = claimed.lock().unwrap_or_else(|e| e.into_inner());
                    if to == RunState::Running {
                        claimed.insert(pid);
                    } else {
                        claimed.remove(&pid);
                    }
                }
            }
        })
    };
    let kernel = kernel.clone();
    tokio::spawn(async move {
        while sampling.load(Ordering::SeqCst) {
            let claimed = claimed.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Ok(processes) = kernel.processes().ps(ProcessFilter::default()).await {
                let admitted = processes
                    .iter()
                    .filter(|process| {
                        process.run.state == RunState::Running
                            && process.run.target != adk_kernel::KERNEL_WAKE_TARGET
                            && claimed.contains(&process.meta.pid)
                            && process.meta.wake.is_none()
                            && !process.meta.suspend_requested
                            && process.meta.suspended_by.is_none()
                    })
                    .count();
                peak.fetch_max(admitted, Ordering::SeqCst);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        listener.abort();
    })
}

/// K8 (spec 3.2 §5): three background jobs run at the partition's quota of three; a typed
/// message starts at once, the youngest job parks at its next checkpoint and is requeued
/// when the message's run finishes, its read runs once, and the partition never runs more
/// than three admitted processes.
async fn k8_a_message_preempts_background_work() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace) = (dir.path().join("home"), dir.path().join("ws"));
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(workspace.join("README.md"), "a project\n")?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let stores = stores(&home, clock.clone()).await?;
    let job = |answer: &str| {
        scripted(vec![
            ScriptedResponse::function_call("read_file", json!({ "path": "README.md" })),
            ScriptedResponse::text(answer),
        ])
    };
    let gate = || Arc::new(tokio::sync::Semaphore::new(0));
    let model = Arc::new(PromptModel {
        scripts: vec![
            ("job-0", job("job 0 done"), gate(), true),
            ("job-1", job("job 1 done"), gate(), true),
            ("job-2", job("job 2 done"), gate(), true),
            (
                "question",
                scripted(vec![ScriptedResponse::text("Here is the answer.")]),
                gate(),
                false,
            ),
        ],
        waiting: std::sync::atomic::AtomicUsize::new(0),
    });
    let recorder = InMemoryAuditRecorder::new();
    let setup = Boot {
        model: model.clone(),
        mode: HarnessMode::Ask,
        control: ControlPlaneConfig::default().without_socket(),
        audit: Arc::new(recorder.clone()),
        clock,
        proactivity: None,
        quotas: Some(adk_kernel::Quotas::new().with_max_running(3)),
        drivers: None,
    };
    let kernel = boot(&stores, &home, &workspace, setup)?;
    kernel.start().await?;

    let sampling = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sampler = admitted_sampler(&kernel, sampling.clone(), peak.clone());

    let mut jobs = Vec::new();
    for n in 0..3 {
        let run = NewRun::new(
            "background",
            APP,
            "ana",
            APP,
            json!({ "text": format!("job-{n}: tidy the docs") }),
        )
        .with_session_id(format!("k8-job-{n}"));
        let process = kernel
            .processes()
            .spawn(run, adk_kernel::ProcessSpec::new(adk_kernel::ProcessKind::Goal))
            .await?;
        jobs.push(process.meta.pid);
        let started = until(Duration::from_secs(20), || async {
            Ok(model.waiting.load(std::sync::atomic::Ordering::SeqCst) == n + 1)
        })
        .await?;
        if !started {
            bail!("background job {n} did not reach its first model call");
        }
    }
    checks.check("three background jobs run at the quota of three", true);

    let typed = std::time::Instant::now();
    let message = kernel
        .processes()
        .spawn(
            NewRun::new(
                "interactive",
                APP,
                "ana",
                APP,
                json!({ "text": "question: what changed?" }),
            )
            .with_session_id("k8-message"),
            adk_kernel::ProcessSpec::new(adk_kernel::ProcessKind::Interactive),
        )
        .await?
        .meta
        .pid;
    let answered = until(Duration::from_secs(10), || async {
        Ok(stores.runs.get(&message).await?.is_some_and(|run| run.state == RunState::Completed))
    })
    .await?;
    checks.check(
        "the message's run starts at once and completes while the jobs still run",
        answered && typed.elapsed() < Duration::from_secs(3),
    );
    let youngest = jobs[2].clone();
    let preempted = kernel.processes().get(&youngest).await?;
    checks.check(
        "the youngest job is preempted by the message",
        preempted.is_some_and(|process| {
            process.meta.suspended_by
                == Some(adk_kernel::Suspension::Preempted { by: message.clone() })
        }),
    );

    // The youngest job reaches its checkpoint, parks, and is requeued: the message finished.
    model.release("job-2");
    let requeued = until(Duration::from_secs(20), || async {
        Ok(stores.runs.get(&youngest).await?.is_some_and(|run| run.state == RunState::Completed))
    })
    .await?;
    for key in ["job-0", "job-1"] {
        model.release(key);
    }
    let finished = until(Duration::from_secs(20), || async {
        let mut all = true;
        for pid in &jobs[..2] {
            all &= stores.runs.get(pid).await?.is_some_and(|run| run.state == RunState::Completed);
        }
        Ok(all)
    })
    .await?;
    sampling.store(false, std::sync::atomic::Ordering::SeqCst);
    sampler.await?;
    let records: Vec<_> = recorder.records();
    let parked = records.iter().any(|record| {
        record.action == adk_kernel::audit::PROCESS_SUSPENDED
            && record.ext.get("pid").and_then(|pid| pid.as_str()) == Some(youngest.as_str())
    });
    let requeue_record = records.iter().any(|record| {
        record.action == adk_kernel::audit::PROCESS_REQUEUED
            && record.ext.get("pid").and_then(|pid| pid.as_str()) == Some(youngest.as_str())
    });
    checks.check(
        "the youngest job parked at its checkpoint and was requeued",
        parked && requeue_record && requeued,
    );
    checks.check("every job completed", finished);
    checks.check(
        "the preempted job's read ran once (two model calls, one read)",
        model.calls("job-2") == 2,
    );
    checks.check(
        "the partition never ran more than three admitted processes",
        peak.load(std::sync::atomic::Ordering::SeqCst) <= 3,
    );
    kernel.stop().await?;
    Ok(checks)
}

/// Waits for the next `Driver` event of the notes driver in `state`.
async fn driver_event(events: &mut adk_kernel::EventSubscriber, state: DriverState) -> bool {
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(record) = events.recv().await {
            if matches!(record.event, KernelEvent::Driver { ref id, state: seen, .. } if id == "notes" && seen == state)
            {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// The answer the transcript of `session` holds for its `read_note` calls.
async fn read_note_answers(stores: &Stores, session: &str) -> Result<Vec<String>> {
    use adk_session::SessionService as _;
    let session = stores
        .sessions
        .get(adk_session::GetRequest {
            app_name: APP.into(),
            user_id: "ana".into(),
            session_id: session.into(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    Ok(session
        .events()
        .all()
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            adk_core::Part::FunctionResponse { function_response, .. }
                if function_response.name == "read_note" =>
            {
                Some(function_response.response.to_string())
            }
            _ => None,
        })
        .collect())
}

/// One turn of the scripted agent in session `session`, run as a process.
async fn turn(kernel: &Kernel, session: &str) -> Result<String> {
    let run =
        NewRun::new(QueueClass::INTERACTIVE, APP, "ana", APP, json!({ "text": "read the note" }))
            .with_session_id(session);
    Ok(kernel
        .processes()
        .spawn(run, adk_kernel::ProcessSpec::new(adk_kernel::ProcessKind::Interactive))
        .await?
        .meta
        .pid)
}

async fn run_state(stores: &Stores, pid: &str, state: RunState) -> Result<bool> {
    let pid = pid.to_string();
    until(Duration::from_secs(30), || {
        let runs = stores.runs.clone();
        let pid = pid.clone();
        async move { Ok(runs.get(&pid).await?.is_some_and(|run| run.state == state)) }
    })
    .await
}

async fn k6_drivers_you_can_trust() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace, notes) =
        (dir.path().join("home"), dir.path().join("ws"), dir.path().join("notes"));
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&notes)?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let stores = stores(&home, clock.clone()).await?;
    let driver_store = adk_kernel::stores::SqliteDriverStore::new(stores.sessions.pool().clone());
    driver_store.migrate().await?;
    let drivers = DriverSupervisor::builder(Arc::new(driver_store))
        .app_name(APP)
        .home(&home)
        .health_check_interval(Duration::from_millis(200))
        .build();
    let read = || ScriptedResponse::function_call("read_note", json!({}));
    let recorder = InMemoryAuditRecorder::new();
    let setup = Boot {
        model: scripted(vec![
            read(),
            ScriptedResponse::text("first"),
            read(),
            ScriptedResponse::text("second"),
            read(),
            ScriptedResponse::text("third"),
        ]),
        mode: HarnessMode::Ask,
        control: ControlPlaneConfig::default().without_socket(),
        audit: Arc::new(recorder.clone()),
        clock,
        proactivity: None,
        quotas: None,
        drivers: Some(drivers.clone()),
    };
    let kernel = boot(&stores, &home, &workspace, setup)?;
    kernel.start().await?;
    let mut events = kernel.events();

    // Mount: the manifest the person approves, then a read in ask mode needs no approval.
    let instance = format!("k6-{}", uuid_like());
    let manifest = crate::notes_driver::manifest(&std::env::current_exe()?, &notes, &instance);
    drivers.mount(DriverManifest::from_toml(&manifest)?, "ana").await?;
    let first = turn(&kernel, "k6-first").await?;
    checks.check(
        "a `read` driver tool runs without approval",
        run_state(&stores, &first, RunState::Completed).await?,
    );
    checks.check(
        "the agent got the driver's answer",
        read_note_answers(&stores, "k6-first")
            .await?
            .iter()
            .any(|answer| answer.contains(crate::notes_driver::CODEWORD)),
    );
    let pinned = drivers
        .record("notes")
        .map(|record| record.manifest.declaration_digests.len())
        .unwrap_or(0);
    checks.check("its tools' declarations are pinned", pinned == 2);

    // Kill: the driver restarts with backoff.
    crate::notes_driver::kill(&instance)?;
    checks.check("the crash is reported", driver_event(&mut events, DriverState::Crashed).await);
    checks.check(
        "it restarts with backoff",
        driver_event(&mut events, DriverState::Restarting).await,
    );
    checks.check("and runs again", driver_event(&mut events, DriverState::Running).await);

    // Redeclare: the changed tool is stale, and a call of it is held for a person.
    std::fs::write(notes.join("declaration.txt"), "it now also forwards the note")?;
    drivers.restart("notes").await?;
    checks.check(
        "the redeclared tool needs re-approval",
        driver_event(&mut events, DriverState::NeedsReapproval).await,
    );
    let second = turn(&kernel, "k6-second").await?;
    checks.check(
        "a call of the stale tool is held",
        run_state(&stores, &second, RunState::Waiting).await?,
    );
    let service = StoreApprovalService::new(stores.approvals.clone());
    let held = service.inbox(adk_core::InboxQuery::default()).await?;
    let item = held.items.iter().find(|item| item.tool_name == "read_note").cloned();
    checks.check("the inbox shows it", item.is_some());

    // Re-approve: the new declaration is pinned; the held call runs once approved, and the
    // next call runs without asking.
    drivers.reapprove("notes", "ana").await?;
    checks.check("re-approval restores it", drivers.state("notes") == Some(DriverState::Running));
    if let Some(item) = item {
        let decision = adk_core::ApprovalDecision::approve(adk_core::SubjectRef::human("ana"))
            .with_groups(["operator"]);
        let routed =
            StoreApprovalService::new(stores.approvals.clone()).with_run_store(stores.runs.clone());
        routed.decide(&item.id, item.version, decision).await?;
    }
    checks
        .check("the held call completes", run_state(&stores, &second, RunState::Completed).await?);
    let third = turn(&kernel, "k6-third").await?;
    checks.check(
        "the next call runs without asking",
        run_state(&stores, &third, RunState::Completed).await?,
    );
    let actions: Vec<String> = recorder.records().into_iter().map(|record| record.action).collect();
    checks.check(
        "mount, start, crash, restart, stale and re-approval are audited",
        ["mounted", "started", "crashed", "restarted", "stale", "reapproved"]
            .iter()
            .all(|step| actions.iter().any(|action| *action == format!("kernel.driver.{step}"))),
    );
    kernel.stop().await?;
    Ok(checks)
}

/// A short unique id without a dependency.
fn uuid_like() -> String {
    format!("{:x}{:x}", std::process::id(), Utc::now().timestamp_nanos_opt().unwrap_or_default())
}

/// What every partition's harness in K7 shares: the stores, the workspace, the partition
/// table, the fact store and the approval service.
struct Shared<'a> {
    stores: &'a Stores,
    workspace: &'a Path,
    table: &'a adk_kernel::Partitions,
    facts: Arc<adk_harness::SqliteFactStore>,
    service: Arc<dyn ApprovalService>,
}

/// A partition's harness for K7, over the shared stores and fact store: memory scoped to the
/// partition, its class on every principal, its crossings under the partition's policy.
fn partition_harness(
    shared: &Shared<'_>,
    partition: adk_kernel::Partition,
    coordinator: bool,
    replies: Vec<ScriptedResponse>,
    extra: impl FnOnce(adk_harness::HarnessAgentBuilder) -> adk_harness::HarnessAgentBuilder,
) -> Result<Harness> {
    let Shared { stores, workspace, table, facts, service } = shared;
    let mut builder = HarnessAgent::builder(APP)
        .model(scripted(replies))
        .workspace(adk_devtools::Workspace::new(*workspace))
        .project_instructions(InstructionsConfig {
            user_file: None,
            ..InstructionsConfig::default()
        })
        .without_default_members()
        .approval_store(stores.approvals.clone())
        .approval_service(service.clone())
        .long_term_memory(adk_harness::LongTermMemoryConfig {
            distill: false,
            ..adk_harness::LongTermMemoryConfig::default()
        })
        .fact_store(facts.clone())
        .partition(partition.clone(), (*table).clone());
    if coordinator {
        builder = builder.partition_coordinator();
    }
    let agent = Arc::new(extra(builder).build()?);
    let runner = adk_runner::Runner::builder()
        .app_name(APP)
        .agent(agent.agent())
        .session_service(stores.sessions.clone())
        .approval_store(agent.runtime().approval_store().clone())
        .identity_source(partition.identity_source(APP, APP))
        .build()?;
    Ok(Harness::with_runner(agent, runner)?.with_run_store(stores.runs.clone()))
}

/// K7 (spec 3.2 §5, §7.8): home and work, each run by its own harness on one kernel, over
/// one `harness.db`. A work agent asks for a home fact and is denied at the policy; the
/// coordinator in home asks for a work fact, its process parks until a person approves the
/// crossing in the inbox, and its identical read after the decision runs without asking;
/// the next process asks again; every crossing is audited, without the query.
async fn k7_partitions() -> Result<Checks> {
    use adk_core::audit_log::AuditTrail;
    use adk_harness::{
        FactKind, FactScope, FactSource, FactStatus, FactStore, MemoryFact, Provenance,
    };
    use adk_kernel::{
        CrossPartition, CrossingFilter, CrossingOutcome, CrossingRun, Partition, PartitionId,
        ProcessKind, ProcessSpec,
    };

    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace) = (dir.path().join("home"), dir.path().join("ws"));
    std::fs::create_dir_all(&workspace)?;
    let stores = stores(&home, Arc::new(SystemClock)).await?;
    let pool = stores.sessions.pool().clone();
    let partition_store = adk_kernel::SqlitePartitionStore::new(pool.clone());
    partition_store.migrate().await?;
    let facts = adk_harness::SqliteFactStore::new(pool);
    facts.migrate().await?;
    let facts = Arc::new(facts);
    let recorder = InMemoryAuditRecorder::new();
    let table = adk_kernel::Partitions::new(Arc::new(partition_store))
        .with_audit(AuditTrail::new(Arc::new(recorder.clone())));
    for id in ["home", "work"] {
        let partition = Partition::new(id, format!("{id}-agents"))
            .with_cross_partition(CrossPartition::CoordinatorWithApproval);
        table.put(partition, adk_core::audit_log::AuditActor::default()).await?;
    }
    for (project, text) in
        [("home", "dentist appointment on tuesday"), ("work", "standup is at nine")]
    {
        let scope = FactScope::new(APP, "alice", Some(project.into()));
        let provenance = Provenance::new(FactSource::UserStated, "seed", "seed");
        facts
            .put(MemoryFact::new(scope, FactKind::ProjectFact, text, FactStatus::Known, provenance))
            .await?;
    }
    let routing = ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(stores.approvals.clone())
            .with_router(routing)
            .with_run_store(stores.runs.clone()),
    );
    let search = |partition: &str, query: &str| {
        ScriptedResponse::function_call(
            adk_harness::PARTITION_MEMORY_SEARCH,
            json!({ "partition": partition, "query": query }),
        )
    };
    let home_partition = table.get(&PartitionId::new("home")).await?.context("home")?;
    let work_partition = table.get(&PartitionId::new("work")).await?.context("work")?;
    let shared = Shared { stores: &stores, workspace: &workspace, table: &table, facts, service };
    let coordinator = partition_harness(
        &shared,
        home_partition,
        true,
        vec![
            search("work", "standup"),
            search("work", "standup"),
            ScriptedResponse::text("Standup is at nine."),
            search("work", "standup"),
            ScriptedResponse::text("asked again"),
        ],
        |builder| builder,
    )?;
    let work = partition_harness(
        &shared,
        work_partition,
        false,
        vec![search("home", "dentist"), ScriptedResponse::text("not mine to see")],
        |builder| builder,
    )?;
    let kernel = work
        .host_partition(coordinator.kernel())
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_home(&home)
                .with_scheduler_tick(Duration::from_millis(250))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .process_store(stores.processes.clone())
        .build()?;
    kernel.start().await?;
    let spawn = |target: &str, session: &str| {
        let run =
            NewRun::new(QueueClass::INTERACTIVE, APP, "alice", target, json!({ "text": "?" }))
                .with_session_id(session)
                .with_authenticated_user();
        kernel.processes().spawn(run, ProcessSpec::new(ProcessKind::Interactive))
    };
    let state = |pid: String| {
        let runs = stores.runs.clone();
        async move { Ok::<_, anyhow::Error>(runs.get(&pid).await?.map(|run| run.state)) }
    };
    let reaches = |pid: String, wanted: RunState| async move {
        until(Duration::from_secs(20), || {
            let pid = pid.clone();
            async move { Ok(state(pid).await? == Some(wanted)) }
        })
        .await
    };

    let asked = spawn("adk-rust@work", "w1").await?;
    checks.check("the work agent's process runs in work", asked.meta.partition.as_str() == "work");
    checks.check("it completes", reaches(asked.meta.pid.clone(), RunState::Completed).await?);
    let denied = table.crossings(&CrossingFilter::default().from("work")).await?;
    checks.check(
        "its read of a home fact was denied at the policy",
        denied.len() == 1 && denied[0].outcome == CrossingOutcome::Denied,
    );

    let parked = spawn("adk-rust@home", "h1").await?;
    let pid = parked.meta.pid.clone();
    checks.check(
        "the coordinator's crossing parks its process",
        reaches(pid.clone(), RunState::Waiting).await?,
    );
    let alice = adk_core::UserId::new("alice")?;
    let h1 = adk_core::SessionId::new("h1")?;
    let pending = coordinator.pending_approvals(&alice, &h1).await?;
    checks.check("one crossing waits in the inbox", pending.len() == 1);
    if let Some(item) = pending.first() {
        coordinator
            .decide(&alice, &h1, &item.fingerprint, adk_harness::HarnessDecision::Approve)
            .await?;
    }
    checks
        .check("the approved process completes", reaches(pid.clone(), RunState::Completed).await?);
    let ledger: Vec<CrossingOutcome> = table
        .crossings(&CrossingFilter::default().run(&pid))
        .await?
        .into_iter()
        .map(|record| record.outcome)
        .collect();
    checks.check(
        "its identical second read ran without asking",
        ledger == [CrossingOutcome::Held, CrossingOutcome::Approved, CrossingOutcome::RunGrant],
    );
    let released = until(Duration::from_secs(5), || {
        let table = table.clone();
        let run = CrossingRun::Durable(pid.clone());
        async move { Ok(table.grants(&run).await?.is_empty()) }
    })
    .await?;
    checks.check("the finished run holds no grant", released);

    let next = spawn("adk-rust@home", "h2").await?;
    checks.check(
        "the next process asks again",
        reaches(next.meta.pid.clone(), RunState::Waiting).await?,
    );
    let records: Vec<_> = recorder
        .records()
        .into_iter()
        .filter(|draft| draft.action == adk_kernel::audit::PARTITION_CROSSING)
        .collect();
    checks.check("every crossing is audited", records.len() == 5);
    checks.check(
        "without the query",
        records.iter().all(|draft| {
            !serde_json::to_string(&draft.ext).unwrap_or_default().contains("standup")
        }),
    );
    kernel.processes().kill(&next.meta.pid, "the scenario is over").await?;
    kernel.stop().await?;
    Ok(checks)
}

/// K7 with drivers (spec 3.2 §7.7, §7.8, K-I15): the notes driver serves work only. Work's
/// harness calls it as a plain tool; the coordinator in home reaches it only as the crossing
/// `work__read_note`, which parks its process until a person approves, and whose identical
/// read later in the run is not asked again.
async fn k7_driver_crossings() -> Result<Checks> {
    use adk_core::audit_log::AuditTrail;
    use adk_kernel::{
        CrossPartition, CrossingFilter, CrossingOutcome, CrossingResource, Partition, ProcessKind,
        ProcessSpec,
    };

    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace, notes) =
        (dir.path().join("home"), dir.path().join("ws"), dir.path().join("notes"));
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&notes)?;
    let stores = stores(&home, Arc::new(SystemClock)).await?;
    let pool = stores.sessions.pool().clone();
    let partition_store = adk_kernel::SqlitePartitionStore::new(pool.clone());
    partition_store.migrate().await?;
    let driver_store = adk_kernel::stores::SqliteDriverStore::new(pool.clone());
    driver_store.migrate().await?;
    let facts = adk_harness::SqliteFactStore::new(pool);
    facts.migrate().await?;
    let recorder = InMemoryAuditRecorder::new();
    let table = adk_kernel::Partitions::new(Arc::new(partition_store))
        .with_audit(AuditTrail::new(Arc::new(recorder.clone())));
    for partition in [
        Partition::new("home", "home-agents"),
        Partition::new("work", "work-agents").with_driver("notes"),
    ] {
        let partition = partition.with_cross_partition(CrossPartition::CoordinatorWithApproval);
        table.put(partition, adk_core::audit_log::AuditActor::default()).await?;
    }
    let drivers = DriverSupervisor::builder(Arc::new(driver_store))
        .app_name(APP)
        .home(&home)
        .health_check_interval(Duration::from_millis(200))
        .build();
    let manifest = crate::notes_driver::manifest(
        &std::env::current_exe()?,
        &notes,
        &format!("k7d-{}", uuid_like()),
    )
    .replace("sandbox = \"os\"\n", "sandbox = \"os\"\npartitions = [\"work\"]\n");
    drivers.mount(DriverManifest::from_toml(&manifest)?, "ana").await?;
    let routing = ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(stores.approvals.clone())
            .with_router(routing)
            .with_run_store(stores.runs.clone()),
    );
    let shared = Shared {
        stores: &stores,
        workspace: &workspace,
        table: &table,
        facts: Arc::new(facts),
        service,
    };
    let read = |tool: &str| ScriptedResponse::function_call(tool, json!({}));
    let home_partition = table.get(&PartitionId::new("home")).await?.context("home")?;
    let work_partition = table.get(&PartitionId::new("work")).await?.context("work")?;
    let coordinator = partition_harness(
        &shared,
        home_partition,
        true,
        vec![
            read("work__read_note"),
            read("work__read_note"),
            ScriptedResponse::text(crate::notes_driver::CODEWORD),
        ],
        |builder| {
            builder.partition_drivers(&drivers).partition_driver_crossings(&drivers, ["work"])
        },
    )?;
    let work = partition_harness(
        &shared,
        work_partition,
        false,
        vec![read("read_note"), ScriptedResponse::text(crate::notes_driver::CODEWORD)],
        |builder| builder.partition_drivers(&drivers),
    )?;
    let kernel = work
        .host_partition(coordinator.kernel())
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_home(&home)
                .with_scheduler_tick(Duration::from_millis(250))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .process_store(stores.processes.clone())
        .drivers(drivers.clone())
        .build()?;
    kernel.start().await?;
    let spawn = |target: &str, session: &str| {
        let run =
            NewRun::new(QueueClass::INTERACTIVE, APP, "alice", target, json!({ "text": "notes?" }))
                .with_session_id(session)
                .with_authenticated_user();
        kernel.processes().spawn(run, ProcessSpec::new(ProcessKind::Interactive))
    };
    let reaches = |pid: String, wanted: RunState| {
        let runs = stores.runs.clone();
        async move {
            until(Duration::from_secs(20), || {
                let (runs, pid) = (runs.clone(), pid.clone());
                async move { Ok(runs.get(&pid).await?.map(|run| run.state) == Some(wanted)) }
            })
            .await
        }
    };

    let direct = spawn("adk-rust@work", "w1").await?;
    checks.check(
        "work's harness calls its own driver as a plain tool",
        reaches(direct.meta.pid.clone(), RunState::Completed).await?
            && table.crossings(&CrossingFilter::default()).await?.is_empty(),
    );

    let parked = spawn("adk-rust@home", "h1").await?;
    let pid = parked.meta.pid.clone();
    checks.check(
        "the coordinator's call is a crossing its process parks on",
        reaches(pid.clone(), RunState::Waiting).await?,
    );
    let alice = adk_core::UserId::new("alice")?;
    let h1 = adk_core::SessionId::new("h1")?;
    let pending = coordinator.pending_approvals(&alice, &h1).await?;
    checks.check(
        "one crossing waits in the inbox",
        pending.len() == 1 && pending[0].request.tool_name == "work__read_note",
    );
    if let Some(item) = pending.first() {
        coordinator
            .decide(&alice, &h1, &item.fingerprint, adk_harness::HarnessDecision::Approve)
            .await?;
    }
    checks
        .check("the approved process completes", reaches(pid.clone(), RunState::Completed).await?);
    let ledger = table.crossings(&CrossingFilter::default().run(&pid)).await?;
    checks.check(
        "the driver crossing was held, approved, and its identical read let through",
        ledger.iter().map(|record| record.outcome).collect::<Vec<_>>()
            == [CrossingOutcome::Held, CrossingOutcome::Approved, CrossingOutcome::RunGrant]
            && ledger.iter().all(|record| record.resource == CrossingResource::Driver),
    );
    kernel.stop().await?;
    Ok(checks)
}
