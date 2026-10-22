//! K1 and K5 on `ScriptedLlm` with a real kernel (spec 3.2 §5, §11): the kernel's durable
//! workers, its scheduler, SQLite stores in `harness.db`, and its control plane on a socket
//! and loopback HTTP, composed as `adk-rust up` composes them.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_auth::signed_audit::{AuditExport, AuditLog, AuditSigner, Ed25519Signer, NoteVerifier};
use adk_core::audit_log::{AuditRecorder, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalAction, ApprovalPolicy, ApprovalService, ApproverSelector, DurableRunStore,
    MissedFirePolicy, NewRun, RunFilter, RunState, Schedule, ScheduleSpec, ScheduleStore,
    StoreApprovalService,
};
use adk_harness::{HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::api::{AttachRequest, ProcessesQuery, SpawnRequest};
use adk_kernel::control::{ControlClient, ControlPlaneConfig};
use adk_kernel::{Kernel, KernelConfig, ProcessFilter};
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
        "k5_one_job_many_windows" => k5_one_job_many_windows().await,
        other => bail!("unknown scenario {other}"),
    }
}

/// The stores of one kernel home, in `harness.db` as `adk-rust up` keeps them.
struct Stores {
    sessions: Arc<SqliteSessionService>,
    runs: Arc<SqliteRunStore>,
    approvals: Arc<SqliteApprovalStore>,
    schedules: Arc<SqliteScheduleStore>,
    processes: Arc<adk_kernel::SqliteProcessStore>,
    logs: Arc<SqliteAuditLogStore>,
}

async fn stores(home: &Path) -> Result<Stores> {
    std::fs::create_dir_all(home)?;
    let url = format!("sqlite://{}?mode=rwc", home.join("harness.db").display());
    let sessions = SqliteSessionService::new(&url).await?;
    sessions.migrate().await?;
    let pool = sessions.pool().clone();
    let runs = SqliteRunStore::from_pool(pool.clone());
    runs.migrate().await?;
    let approvals = SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await?;
    let schedules = SqliteScheduleStore::from_pool(pool.clone());
    schedules.migrate().await?;
    let processes = adk_kernel::SqliteProcessStore::new(pool.clone());
    processes.migrate().await?;
    let logs = SqliteAuditLogStore::from_pool(pool);
    logs.migrate().await?;
    Ok(Stores {
        sessions: Arc::new(sessions),
        runs: Arc::new(runs),
        approvals: Arc::new(approvals),
        schedules: Arc::new(schedules),
        processes: Arc::new(processes),
        logs: Arc::new(logs),
    })
}

fn scripted(replies: Vec<ScriptedResponse>) -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm::new("scripted", replies))
}

/// One boot of the kernel over `stores`: the harness on `model` in `mode`, held calls
/// routed to the `operator` group, the control plane as `control` says.
fn boot(
    stores: &Stores,
    home: &Path,
    workspace: &Path,
    model: Arc<ScriptedLlm>,
    mode: HarnessMode,
    control: ControlPlaneConfig,
    audit: Arc<dyn AuditRecorder>,
) -> Result<Kernel> {
    let routing = ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(stores.approvals.clone())
            .with_router(routing)
            .with_run_store(stores.runs.clone()),
    );
    let agent = HarnessAgent::builder(APP)
        .model(model)
        .workspace(adk_devtools::Workspace::new(workspace).allow_bash(true))
        .project_instructions(InstructionsConfig {
            user_file: None,
            ..InstructionsConfig::default()
        })
        .without_default_members()
        .mode(mode)
        .approval_store(stores.approvals.clone())
        .approval_service(service.clone())
        .audit_log(audit.clone())
        .build()?;
    let runner = Arc::new(
        adk_runner::Runner::builder()
            .app_name(APP)
            .agent(agent.agent())
            .session_service(stores.sessions.clone())
            .approval_store(agent.runtime().approval_store().clone())
            .build()?,
    );
    let config = KernelConfig::default()
        .with_app_name(APP)
        .with_home(home)
        .with_scheduler_tick(Duration::from_millis(250))
        .with_control(control);
    Ok(Kernel::builder()
        .config(config)
        .run_store(stores.runs.clone())
        .process_store(stores.processes.clone())
        .schedule_store(stores.schedules.clone())
        .approval_service(service)
        .session_service(stores.sessions.clone())
        .audit(AuditTrail::new(audit))
        .agent(APP, runner.clone())
        .agent(adk_harness::GOAL_TARGET, runner)
        .build()?)
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

/// K1 without service installation (spec 3.2 §5): a nightly audit is scheduled, the host
/// goes down before its time and comes back after it; the next boot runs it once, the
/// process table shows it `Completed`, and the signed audit log verifies.
async fn k1_leave_it_running() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace) = (dir.path().join("home"), dir.path().join("ws"));
    std::fs::create_dir_all(&workspace)?;
    let stores = stores(&home).await?;
    let signer = Arc::new(Ed25519Signer::generate("kernel-k1")?);
    let control = ControlPlaneConfig::default().without_socket();
    let fire_at = Utc::now() + chrono::Duration::seconds(3);

    // First boot: the schedule is recorded, then the host goes down before its time.
    {
        let log: Arc<dyn AuditRecorder> = Arc::new(
            AuditLog::builder(stores.logs.clone(), "kernel-k1", signer.clone()).open().await?,
        );
        let kernel = boot(
            &stores,
            &home,
            &workspace,
            scripted(Vec::new()),
            HarnessMode::Auto,
            control.clone(),
            log,
        )?;
        kernel.start().await?;
        let run =
            NewRun::new("background", APP, "ana", APP, json!({ "text": "audit the repository" }))
                .with_session_id("nightly");
        let schedule = Schedule::new(
            "nightly-audit",
            ScheduleSpec::At(fire_at),
            run,
            MissedFirePolicy::FireOnce,
        );
        stores.schedules.upsert(schedule).await?;
        kernel.stop().await?;
    }
    tokio::time::sleep(
        (fire_at - Utc::now()).to_std().unwrap_or_default() + Duration::from_secs(1),
    )
    .await;
    let fired = |stores: &Stores| {
        let runs = stores.runs.clone();
        async move {
            let runs = runs.list(RunFilter::default().session_id("nightly")).await?;
            Ok::<_, anyhow::Error>(runs)
        }
    };
    checks.check("nothing ran while the host was down", fired(&stores).await?.is_empty());

    // Second boot, after the schedule's time: the missed firing runs once.
    let log =
        Arc::new(AuditLog::builder(stores.logs.clone(), "kernel-k1", signer.clone()).open().await?);
    let model = scripted(vec![
        ScriptedResponse::function_call(
            "write_file",
            json!({ "path": "audit.txt", "content": "audited" }),
        ),
        ScriptedResponse::text("audited"),
    ]);
    let kernel = boot(&stores, &home, &workspace, model, HarnessMode::Auto, control, log.clone())?;
    kernel.start().await?;
    let completed = until(Duration::from_secs(30), || {
        let runs = stores.runs.clone();
        async move {
            let runs = runs.list(RunFilter::default().session_id("nightly")).await?;
            Ok(runs.iter().any(|run| run.state.is_terminal()))
        }
    })
    .await?;
    // A second firing would show up within a few scheduler ticks.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let runs = fired(&stores).await?;
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
    let stores = stores(&home).await?;
    let addr = free_port()?;
    let recorder = InMemoryAuditRecorder::new();
    let model = scripted(vec![
        ScriptedResponse::function_call("bash", json!({ "command": "echo once >> marker.txt" })),
        ScriptedResponse::text("done"),
    ]);
    let kernel = boot(
        &stores,
        &home,
        &workspace,
        model,
        HarnessMode::Ask,
        ControlPlaneConfig::default().with_http(addr),
        Arc::new(recorder.clone()),
    )?;
    kernel.start().await?;

    // The terminal, on the socket.
    let terminal = ControlClient::connect_local(&home).await?.context("the kernel's socket")?;
    let request =
        SpawnRequest::new(APP, json!({ "text": "run the marker command" })).with_session_id("k5");
    let pid = terminal.spawn(&request).await?.meta.pid;
    let held =
        until(Duration::from_secs(20), || {
            let terminal = terminal.clone();
            async move {
                Ok(!terminal.attach("k5", &AttachRequest::default()).await?.pending.is_empty())
            }
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
