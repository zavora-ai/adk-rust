//! `adk-rust up`: the harness as the agent kernel, in the foreground (spec 3.2 §8.1, §8.2).
//!
//! `up` builds what `serve` builds — the approval service with `operator` routing and
//! run-store wake-ups, background runs with the harness targets, the extractor and its
//! non-loopback refusal, the `--store` backend (§6 D-9) — and adds the kernel: a worker per
//! queue class, the scheduler, the process table and the control plane on the kernel
//! home's socket and under `/api/kernel` on HTTP. The kernel keeps its content-free
//! activity ledger beside its other stores, with the subject key in the home, and runs the
//! daily baseline pass, which the harness's model words (spec 3.2 §7.10).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_cli::harness::{BearerTokenExtractor, HarnessLauncher, KernelSettings, OPERATOR_SCOPE};
use adk_core::audit_log::{AuditRecorder, AuditTrail};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, GovernanceControl, GovernanceStateStore,
    ScheduleStore, StoreApprovalService,
};
use adk_harness::HarnessMode;
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::proc::baselines::LlmPhraser;
use adk_kernel::proc::{Ledger, LedgerKey, LedgerStore};
use adk_kernel::{KernelConfig, ProcessStore, QueueClass};
use adk_server::background::{BackgroundState, DurableWorkerConfig};
use anyhow::{Context, Result, bail};

use super::{APP_NAME, AgentExtras, Setup, user_config};
use crate::ResolvedModel;
use crate::cli::UpArgs;

/// Where `up` listens over HTTP when neither `--bind` nor `[kernel] bind` says.
pub(super) const DEFAULT_BIND: &str = "127.0.0.1:8080";

/// The kernel's own stores, in the backend the harness's stores are in.
struct KernelStores {
    processes: Arc<dyn ProcessStore>,
    agenda: Arc<dyn adk_kernel::AgendaStore>,
    schedules: Arc<dyn ScheduleStore>,
    /// Mounted drivers and their parked calls (spec 3.2 §7.7).
    drivers: Arc<dyn adk_kernel::drivers::DriverStore>,
    /// The activity ledger, its observations and pass state (spec 3.2 §7.10).
    ledger: Arc<dyn LedgerStore>,
    /// Paired devices and their waiting codes, hashed (spec 3.3 §10 K2).
    devices: Arc<dyn adk_kernel::pairing::DeviceStore>,
    /// Whether nothing outlives the kernel (`--ephemeral`).
    ephemeral: bool,
    /// The kill switch's shared state; `None` keeps it in this process.
    governance: Option<Arc<dyn GovernanceStateStore>>,
    /// Where they are, for the banner.
    label: String,
}

/// `adk-rust up` (spec 3.2 §8.1): runs until Ctrl-C or `SIGTERM`, then drains and stops.
pub async fn up(model: ResolvedModel, instruction: Option<String>, up: UpArgs) -> Result<()> {
    let UpArgs { home, bind, auth_token, store, harness, .. } = up;
    // The baseline pass's observations are worded by the harness's model (spec 3.2 §7.10).
    let phraser = Arc::new(LlmPhraser::new(model.llm.clone()));
    // Only `max_concurrent_runs` of a workspace's [kernel] table applies, so the directory
    // the workspace resolves from is enough here.
    let workspace = match &harness.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().context("no current directory")?,
    };
    let mut settings = KernelSettings::resolve(user_config().as_deref(), &workspace)?;
    let home = match home.or_else(|| settings.home.clone()) {
        Some(home) => home,
        None => default_home()?,
    };
    let bind = match bind.or(settings.bind) {
        Some(bind) => bind,
        None => DEFAULT_BIND.parse()?,
    };
    let token = auth_token.filter(|token| !token.trim().is_empty());
    if !bind.ip().is_loopback() && token.is_none() {
        bail!(
            "serving the kernel on {bind} needs authentication: pass --auth-token or set \
             ADK_SERVE_TOKEN, or bind 127.0.0.1 (spec 3.2 K-I18)"
        );
    }

    let setup =
        Setup::open(model, instruction, harness, HarnessMode::Plan, store.as_deref(), Some(&home))
            .await?;
    setup.print_notes();
    setup.memory_notice();
    for note in &settings.notes {
        println!("note: {note}");
    }
    if !bind.ip().is_loopback()
        && setup.resolved.mode == HarnessMode::Auto
        && !setup.resolved.sandbox
    {
        bail!(
            "--mode auto beyond loopback needs --sandbox: edits would run unattended on this host"
        );
    }

    let (approval_store, run_store) = run_and_approval_stores(&setup);
    // The person running the kernel is the operator: their own run's calls are theirs to
    // approve, as `serve` routes them.
    let routing =
        ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(approval_store)
            .with_router(routing)
            .with_run_store(run_store.clone()),
    );
    let stores = kernel_stores(&setup, store.as_deref()).await?;
    // One kill switch for the runner, the kernel and the server, mirrored from the store
    // replicas share (K-I8).
    let governance = match &stores.governance {
        Some(state) => KernelConfig::mirrored_governance(state.clone()),
        None => GovernanceControl::new(),
    };
    let audit = kernel_audit(&home, &setup, store.as_deref()).await?;
    if let Some(recorder) = &audit {
        governance.attach_audit(AuditTrail::new(recorder.clone()));
    }
    // Drivers (spec 3.2 §7.7): supervised MCP servers under the manifests the user mounted,
    // kept beside the kernel's other stores; their tools reach the harness with the classes
    // the manifests give them. A driver that may sample does so on the harness model,
    // metered against the run that called it.
    let sampling_model = setup.models.harness.llm.clone();
    let drivers = adk_kernel::drivers::DriverSupervisor::builder(stores.drivers.clone())
        .app_name(APP_NAME)
        .home(&home)
        .sampling_handlers(move |_| {
            Some(Arc::new(adk_tool::sampling::LlmSamplingHandler::new(sampling_model.clone()))
                as Arc<dyn adk_tool::sampling::SamplingHandler>)
        })
        .build();
    let agent = Arc::new(setup.agent_with(
        setup.resolved.effort,
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        None,
        Some(service),
        AgentExtras {
            budget: None,
            audit,
            memory_with_inbox: true,
            drivers: Some(drivers.clone()),
        },
    )?);
    let runner = adk_runner::Runner::builder()
        .app_name(APP_NAME)
        .agent(agent.agent())
        .session_service(setup.sessions())
        .approval_store(agent.runtime().approval_store().clone())
        .governance(governance.clone())
        .build()?;
    // The harness glue registers the runner as `adk-rust` and `harness.goal`, with the
    // harness's sessions, approval service, kill switch, audit trail and goal progress
    // (spec 3.2 §10 HK1); goals land on the background queue.
    let harness = adk_harness::Harness::with_runner(agent.clone(), runner)?
        .with_run_store(run_store.clone())
        .with_goal_queue(QueueClass::BACKGROUND);
    // Re-approving a driver carries the re-approving person's standing rules forward.
    drivers.set_rule_pins(harness.driver_rule_pins());
    let mut worker = DurableWorkerConfig::default();
    if let Some(max) = settings.max_concurrent_runs {
        worker = worker.with_max_concurrent_runs(max);
    }
    // The consolidation tick: on in Ask and Auto with long-term memory (spec 3.2 §13 Q1).
    let tick = settings.enable_consolidation(
        setup.resolved.mode,
        setup.resolved.consolidation && setup.memory_on(),
    );
    // The run store reads the system clock too, so leases, `not_before` and wake times agree.
    let config = settings.apply(
        KernelConfig::default()
            .with_app_name(APP_NAME)
            .with_home(&home)
            .with_worker(worker)
            // The socket at the kernel home, or on Windows the named pipe named after it
            // (spec 3.2 §10 W2), pairing devices beside the kernel's other stores (spec 3.3
            // §10 K2).
            .with_control(
                ControlPlaneConfig::default()
                    .with_pairing(adk_kernel::pairing::Pairing::new(stores.devices.clone())),
            )
            .with_clock(Arc::new(adk_core::SystemClock)),
    );
    // The subject key stays in the home; an ephemeral kernel's ledger and key end with it.
    let key = if stores.ephemeral {
        LedgerKey::generate()?
    } else {
        let path = home.join(LedgerKey::FILE);
        // A protected DACL stands in for mode 0600 (spec 3.2 §10 W4); the kernel fills the
        // empty file with the key.
        #[cfg(all(windows, any(feature = "audit", feature = "persist")))]
        if !path.exists() {
            std::fs::create_dir_all(&home)?;
            if let Err(error) = crate::owner_only::create_new(&path)
                && error.kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(error).with_context(|| format!("failed to create {}", path.display()));
            }
        }
        LedgerKey::load_or_create(&path)?
    };
    let mut kernel = harness
        .kernel()
        .config(config)
        .process_store(stores.processes)
        .agenda_store(stores.agenda)
        .schedule_store(stores.schedules)
        .drivers(drivers)
        .ledger(Ledger::new(stores.ledger, key))
        .observation_phraser(phraser);
    if tick {
        // The tick distils this workspace's sessions, and the kernel's own, which name none.
        let recorded = setup.resolved.workspace.display().to_string();
        let user = adk_core::UserId::new(setup.user.clone())?;
        let consolidator = harness.consolidator(&user)?.with_session_filter(Arc::new(
            move |state: &dyn adk_session::State| match state.get("harness:workspace") {
                Some(value) => value.as_str() == Some(recorded.as_str()),
                None => true,
            },
        ));
        kernel = kernel.consolidator(adk_kernel::PartitionId::default(), Arc::new(consolidator));
    }
    if setup.memory_on() {
        // The pass's observations are put to the user; a confirmed one is remembered.
        let user = adk_core::UserId::new(setup.user.clone())?;
        let audience = harness.observation_audience(&user)?;
        kernel =
            kernel.observation_audience(adk_kernel::PartitionId::default(), Arc::new(audience));
    }
    let kernel = kernel.build()?;
    // The inbox decides through the harness's approval service, which wakes the kernel's
    // approval sleepers; the service the builder was given would not.
    let decisions = agent
        .runtime()
        .approval_service()
        .cloned()
        .context("the served harness has an approval service")?;

    // `/api/runs` submissions land on the interactive queue, where the kernel's worker
    // claims them; the server runs no worker of its own.
    let (target, runner) = harness.goal_target();
    let background = BackgroundState::new()
        .with_run_store(run_store)
        .with_app_name(APP_NAME)
        .with_agent(APP_NAME, runner.clone())
        .with_agent(target, runner)
        .with_worker_config(DurableWorkerConfig::default().with_queue(QueueClass::INTERACTIVE))
        .with_embedded_worker(false)
        .with_governance(governance);
    let mut launcher = HarnessLauncher::shared(agent)
        .app_name(APP_NAME)
        .with_session_service(setup.sessions())
        .with_user_id(setup.user.clone())
        .with_approval_service(decisions)
        .with_background_runs(background)
        .with_bind_address(bind.ip())
        .with_kernel(kernel);
    if let Some(token) = token {
        launcher = launcher.with_request_context_extractor(Arc::new(BearerTokenExtractor::new(
            token,
            setup.user.clone(),
        )));
    }
    println!(
        "  store {}: sessions, approvals, runs, processes, agenda, schedules, drivers, the activity ledger, paired devices",
        stores.label
    );
    println!("  plans {}", setup.plans_note());
    println!(
        "  tick  {}",
        if tick {
            "consolidation on: distils missed sessions and tidies assumed facts (--no-consolidation turns it off)"
        } else {
            "consolidation off"
        }
    );
    launcher.run_serve_directly(bind.port()).await.map_err(Into::into)
}

/// The kernel home when neither `--home` nor `[kernel] home` names one: the directory
/// `harness.db` is in.
pub(super) fn default_home() -> Result<PathBuf> {
    #[cfg(feature = "persist")]
    return crate::sessions::default_dir();
    #[cfg(not(feature = "persist"))]
    Ok(dirs::config_dir()
        .context("could not determine the config directory; pass --home")?
        .join("adk-rust"))
}

/// The approval and run stores `serve` uses: the `--store` backend, `harness.db`, or memory.
fn run_and_approval_stores(
    setup: &Setup,
) -> (Arc<dyn adk_core::ApprovalStore>, Arc<dyn adk_core::DurableRunStore>) {
    if let Some(shared) = &setup.shared {
        return (shared.approvals.clone(), shared.runs.clone());
    }
    #[cfg(feature = "persist")]
    if let Some(db) = &setup.db {
        return (db.approvals.clone(), db.runs.clone());
    }
    (Arc::new(adk_core::InMemoryApprovalStore::new()), Arc::new(adk_core::InMemoryRunStore::new()))
}

/// The process, schedule and governance stores, beside the harness's (spec 3.2 §7.1, §13
/// Q8): on `harness.db`'s pool with their own migration tables, in the `--store` backend,
/// or in memory with `--ephemeral`.
async fn kernel_stores(setup: &Setup, store: Option<&str>) -> Result<KernelStores> {
    if let Some(url) = store {
        return shared_kernel_stores(url).await;
    }
    #[cfg(feature = "persist")]
    if let Some(db) = &setup.db {
        let processes = adk_kernel::SqliteProcessStore::new(db.pool.clone());
        processes.migrate().await?;
        let agenda = adk_kernel::SqliteAgendaStore::new(db.pool.clone());
        agenda.migrate().await?;
        let schedules = adk_session::SqliteScheduleStore::from_pool(db.pool.clone());
        schedules.migrate().await?;
        let governance = adk_session::SqliteGovernanceStateStore::from_pool(db.pool.clone());
        governance.migrate().await?;
        let drivers = adk_kernel::stores::SqliteDriverStore::new(db.pool.clone());
        drivers.migrate().await?;
        let ledger = adk_kernel::proc::SqliteLedgerStore::new(db.pool.clone());
        ledger.migrate().await?;
        let devices = adk_kernel::pairing::SqliteDeviceStore::new(db.pool.clone());
        devices.migrate().await?;
        return Ok(KernelStores {
            processes: Arc::new(processes),
            agenda: Arc::new(agenda),
            schedules: Arc::new(schedules),
            drivers: Arc::new(drivers),
            ledger: Arc::new(ledger),
            devices: Arc::new(devices),
            ephemeral: false,
            governance: Some(Arc::new(governance)),
            label: "harness.db".into(),
        });
    }
    let _ = setup;
    Ok(KernelStores {
        processes: Arc::new(adk_kernel::InMemoryProcessStore::new()),
        agenda: Arc::new(adk_kernel::InMemoryAgendaStore::new()),
        schedules: Arc::new(adk_core::InMemoryScheduleStore::new()),
        drivers: Arc::new(adk_kernel::stores::InMemoryDriverStore::new()),
        ledger: Arc::new(adk_kernel::proc::InMemoryLedgerStore::new()),
        devices: Arc::new(adk_kernel::pairing::InMemoryDeviceStore::new()),
        ephemeral: true,
        governance: None,
        label: "memory (--ephemeral): nothing outlives the kernel".into(),
    })
}

/// The kernel's stores in the `--store` backend every replica shares.
async fn shared_kernel_stores(url: &str) -> Result<KernelStores> {
    let label = adk_harness::redact_store_url(url);
    match url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase()).as_deref() {
        #[cfg(feature = "postgres")]
        Some("postgres" | "postgresql") => {
            let processes = adk_kernel::PostgresProcessStore::connect(url).await?;
            processes.migrate().await?;
            let agenda = adk_kernel::PostgresAgendaStore::new(processes.pool().clone());
            agenda.migrate().await?;
            let schedules = adk_session::PostgresScheduleStore::new(url).await?;
            schedules.migrate().await?;
            let governance = adk_session::PostgresGovernanceStateStore::new(url).await?;
            governance.migrate().await?;
            let drivers = adk_kernel::stores::PostgresDriverStore::connect(url).await?;
            drivers.migrate().await?;
            let ledger = adk_kernel::proc::PostgresLedgerStore::new(processes.pool().clone());
            ledger.migrate().await?;
            let devices = adk_kernel::pairing::PostgresDeviceStore::new(processes.pool().clone());
            devices.migrate().await?;
            Ok(KernelStores {
                processes: Arc::new(processes),
                agenda: Arc::new(agenda),
                schedules: Arc::new(schedules),
                drivers: Arc::new(drivers),
                ledger: Arc::new(ledger),
                devices: Arc::new(devices),
                ephemeral: false,
                governance: Some(Arc::new(governance)),
                label,
            })
        }
        #[cfg(feature = "redis")]
        Some("redis" | "rediss") => {
            let processes = adk_kernel::RedisProcessStore::connect(url).await?;
            processes.migrate().await?;
            let agenda = adk_kernel::RedisAgendaStore::from_client(processes.client().clone());
            agenda.migrate().await?;
            let schedules = adk_session::RedisScheduleStore::new(url).await?;
            schedules.migrate().await?;
            let governance = adk_session::RedisGovernanceStateStore::new(url).await?;
            governance.migrate().await?;
            let drivers = adk_kernel::stores::RedisDriverStore::connect(url).await?;
            drivers.migrate().await?;
            let ledger =
                adk_kernel::proc::RedisLedgerStore::from_client(processes.client().clone());
            ledger.migrate().await?;
            let devices =
                adk_kernel::pairing::RedisDeviceStore::from_client(processes.client().clone());
            devices.migrate().await?;
            Ok(KernelStores {
                processes: Arc::new(processes),
                agenda: Arc::new(agenda),
                schedules: Arc::new(schedules),
                drivers: Arc::new(drivers),
                ledger: Arc::new(ledger),
                devices: Arc::new(devices),
                ephemeral: false,
                governance: Some(Arc::new(governance)),
                label,
            })
        }
        // `Setup::open` already refused a scheme this build cannot open.
        _ => bail!("--store {label}: no kernel store backend for it in this build"),
    }
}

/// The kernel replica's signed audit log (spec 3.2 §9.3, K-I16): one hash chain per
/// replica in the store the harness's are in, signed with a key kept owner-only in the
/// kernel home. The harness's tool-call records and the kernel's `kernel.*` records share
/// it. `None` with `--ephemeral`, whose records would not outlive the kernel.
#[cfg(feature = "audit")]
async fn kernel_audit(
    home: &Path,
    setup: &Setup,
    store: Option<&str>,
) -> Result<Option<Arc<dyn AuditRecorder>>> {
    use adk_auth::signed_audit::{AuditLog, Ed25519Signer};
    use adk_core::audit_log::AuditLogStore;

    let logs: Arc<dyn AuditLogStore> = match store {
        Some(url) => shared_audit_store(url).await?,
        None => match harness_db_audit_store(setup).await? {
            Some(logs) => logs,
            None => return Ok(None),
        },
    };
    std::fs::create_dir_all(home)
        .with_context(|| format!("failed to create {}", home.display()))?;
    let id = kernel_id(home)?;
    let key_path = home.join("kernel-audit.key");
    let signer = match std::fs::read_to_string(&key_path) {
        Ok(key) => Ed25519Signer::from_note_key(key.trim())
            .with_context(|| format!("{} is not a signer key", key_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let signer = Ed25519Signer::generate(format!("kernel-{id}"))?;
            write_private(&key_path, signer.to_note_key().as_str())?;
            signer
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", key_path.display()));
        }
    };
    let log = AuditLog::builder(logs, format!("kernel-{id}"), Arc::new(signer)).open().await?;
    Ok(Some(Arc::new(log)))
}

#[cfg(not(feature = "audit"))]
async fn kernel_audit(
    _home: &Path,
    _setup: &Setup,
    _store: Option<&str>,
) -> Result<Option<Arc<dyn AuditRecorder>>> {
    Ok(None)
}

/// The audit log store on `harness.db`'s pool, or `None` without `harness.db`.
#[cfg(all(feature = "audit", feature = "persist"))]
async fn harness_db_audit_store(
    setup: &Setup,
) -> Result<Option<Arc<dyn adk_core::audit_log::AuditLogStore>>> {
    let Some(db) = &setup.db else {
        return Ok(None);
    };
    let logs = adk_session::SqliteAuditLogStore::from_pool(db.pool.clone());
    logs.migrate().await?;
    Ok(Some(Arc::new(logs)))
}

#[cfg(all(feature = "audit", not(feature = "persist")))]
async fn harness_db_audit_store(
    _setup: &Setup,
) -> Result<Option<Arc<dyn adk_core::audit_log::AuditLogStore>>> {
    Ok(None)
}

#[cfg(feature = "audit")]
async fn shared_audit_store(url: &str) -> Result<Arc<dyn adk_core::audit_log::AuditLogStore>> {
    match url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase()).as_deref() {
        #[cfg(feature = "postgres")]
        Some("postgres" | "postgresql") => {
            let logs = adk_session::PostgresAuditLogStore::new(url).await?;
            logs.migrate().await?;
            Ok(Arc::new(logs))
        }
        #[cfg(feature = "redis")]
        Some("redis" | "rediss") => {
            let logs = adk_session::RedisAuditLogStore::new(url).await?;
            logs.migrate().await?;
            Ok(Arc::new(logs))
        }
        _ => bail!(
            "--store {}: no audit log backend for it in this build",
            adk_harness::redact_store_url(url)
        ),
    }
}

/// This replica's id, kept in the kernel home so its audit chain continues across restarts.
#[cfg(feature = "audit")]
fn kernel_id(home: &Path) -> Result<String> {
    let path = home.join("kernel-id");
    match std::fs::read_to_string(&path) {
        Ok(id) if !id.trim().is_empty() => Ok(id.trim().to_string()),
        Ok(_) => bail!("{} is empty; remove it to start a new audit chain", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = uuid::Uuid::new_v4().simple().to_string();
            write_private(&path, &id)?;
            Ok(id)
        }
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Writes `contents` to a new owner-only file at `path`: mode `0600` on Unix, a DACL granting
/// the owner alone access on Windows (spec 3.2 §10 W4).
#[cfg(feature = "audit")]
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    #[cfg(not(windows))]
    let created = {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    };
    #[cfg(windows)]
    let created = crate::owner_only::create_new(path);
    let mut file = created.with_context(|| format!("failed to create {}", path.display()))?;
    writeln!(file, "{contents}").with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(all(test, feature = "audit"))]
mod tests {
    use super::*;

    /// The replica id and the audit signing key in the kernel home are readable by their
    /// owner only (spec 3.2 §10 W4).
    #[test]
    fn the_kernel_home_files_are_owner_only() {
        let home = tempfile::tempdir().unwrap();
        let id = kernel_id(home.path()).unwrap();
        assert_eq!(kernel_id(home.path()).unwrap(), id, "the id survives a restart");
        let key = home.path().join("kernel-audit.key");
        write_private(&key, "secret").unwrap();
        for path in [home.path().join("kernel-id"), key] {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{}", path.display());
            }
            #[cfg(windows)]
            assert!(crate::owner_only::is_owner_only(&path).unwrap(), "{}", path.display());
        }
    }
}
