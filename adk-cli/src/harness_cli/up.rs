//! `adk-rust up`: the harness as the agent kernel, in the foreground (spec 3.2 §8.1, §8.2).
//!
//! `up` builds what `serve` builds — the approval service with `operator` routing and
//! run-store wake-ups, background runs with the harness targets, the extractor and its
//! non-loopback refusal, the `--store` backend (§6 D-9) — and adds the kernel: a worker per
//! queue class, the scheduler, the process table and the control plane on the kernel
//! home's socket and under `/api/kernel` on HTTP.

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
use adk_kernel::{Kernel, KernelConfig, ProcessStore, QueueClass};
use adk_server::background::{BackgroundState, DurableWorkerConfig};
use anyhow::{Context, Result, bail};

use super::{APP_NAME, AgentExtras, Setup, user_config};
use crate::ResolvedModel;
use crate::cli::UpArgs;

/// Where `up` listens over HTTP when neither `--bind` nor `[kernel] bind` says.
const DEFAULT_BIND: &str = "127.0.0.1:8080";

/// The kernel's own stores, in the backend the harness's stores are in.
struct KernelStores {
    processes: Arc<dyn ProcessStore>,
    schedules: Arc<dyn ScheduleStore>,
    /// The kill switch's shared state; `None` keeps it in this process.
    governance: Option<Arc<dyn GovernanceStateStore>>,
    /// Where they are, for the banner.
    label: String,
}

/// `adk-rust up` (spec 3.2 §8.1): runs until Ctrl-C or `SIGTERM`, then drains and stops.
pub async fn up(model: ResolvedModel, instruction: Option<String>, up: UpArgs) -> Result<()> {
    if up.install || up.uninstall {
        bail!(
            "service installation (spec 3.2 §7.12) is not in this build: run `adk-rust up` in \
             the foreground, or under launchd, systemd or a container's supervisor"
        );
    }
    let UpArgs { home, bind, auth_token, store, harness, .. } = up;
    // Only `max_concurrent_runs` of a workspace's [kernel] table applies, so the directory
    // the workspace resolves from is enough here.
    let workspace = match &harness.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().context("no current directory")?,
    };
    let settings = KernelSettings::resolve(user_config().as_deref(), &workspace)?;
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
    let audit = kernel_audit(&home, &setup, store.as_deref()).await?;
    let agent = setup.agent_with(
        setup.resolved.effort,
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        None,
        Some(service.clone()),
        AgentExtras { budget: None, audit: audit.clone() },
    )?;
    let runner = Arc::new(
        adk_runner::Runner::builder()
            .app_name(APP_NAME)
            .agent(agent.agent())
            .session_service(setup.sessions())
            .approval_store(agent.runtime().approval_store().clone())
            .build()?,
    );

    let stores = kernel_stores(&setup, store.as_deref()).await?;
    let governance = match &stores.governance {
        Some(state) => KernelConfig::mirrored_governance(state.clone()),
        None => GovernanceControl::new(),
    };
    let trail = audit.map(AuditTrail::new);
    if let Some(trail) = &trail {
        governance.attach_audit(trail.clone());
    }
    let mut worker = DurableWorkerConfig::default();
    if let Some(max) = settings.max_concurrent_runs {
        worker = worker.with_max_concurrent_runs(max);
    }
    let config = KernelConfig::default()
        .with_app_name(APP_NAME)
        .with_home(&home)
        .with_worker(worker)
        .with_governance(governance.clone())
        .with_control(control_plane());
    let mut kernel = Kernel::builder()
        .config(config)
        .run_store(run_store.clone())
        .process_store(stores.processes)
        .schedule_store(stores.schedules)
        .approval_service(service.clone())
        .session_service(setup.sessions())
        .agent(APP_NAME, runner.clone())
        .agent(adk_harness::GOAL_TARGET, runner.clone());
    if let Some(trail) = trail {
        kernel = kernel.audit(trail);
    }
    let kernel = kernel.build()?;

    // `/api/runs` submissions land on the interactive queue, where the kernel's worker
    // claims them; the server runs no worker of its own.
    let background = BackgroundState::new()
        .with_run_store(run_store)
        .with_app_name(APP_NAME)
        .with_agent(APP_NAME, runner.clone())
        .with_agent(adk_harness::GOAL_TARGET, runner)
        .with_worker_config(DurableWorkerConfig::default().with_queue(QueueClass::INTERACTIVE))
        .with_embedded_worker(false)
        .with_governance(governance);
    let mut launcher = HarnessLauncher::new(agent)
        .app_name(APP_NAME)
        .with_session_service(setup.sessions())
        .with_user_id(setup.user.clone())
        .with_approval_service(service)
        .with_background_runs(background)
        .with_bind_address(bind.ip())
        .with_kernel(kernel);
    if let Some(token) = token {
        launcher = launcher.with_request_context_extractor(Arc::new(BearerTokenExtractor::new(
            token,
            setup.user.clone(),
        )));
    }
    println!("  store {}: sessions, approvals, runs, processes, schedules", stores.label);
    println!("  plans {}", setup.plans_note());
    launcher.run_serve_directly(bind.port()).await.map_err(Into::into)
}

/// The kernel home when neither `--home` nor `[kernel] home` names one: the directory
/// `harness.db` is in.
fn default_home() -> Result<PathBuf> {
    #[cfg(feature = "persist")]
    return crate::sessions::default_dir();
    #[cfg(not(feature = "persist"))]
    Ok(dirs::config_dir()
        .context("could not determine the config directory; pass --home")?
        .join("adk-rust"))
}

/// The socket at the kernel home, where Unix sockets exist; off Unix the control plane is
/// HTTP only until the named pipe (spec 3.2 §10 W2).
fn control_plane() -> ControlPlaneConfig {
    let config = ControlPlaneConfig::default();
    #[cfg(not(unix))]
    let config = config.without_socket();
    config
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
        let schedules = adk_session::SqliteScheduleStore::from_pool(db.pool.clone());
        schedules.migrate().await?;
        let governance = adk_session::SqliteGovernanceStateStore::from_pool(db.pool.clone());
        governance.migrate().await?;
        return Ok(KernelStores {
            processes: Arc::new(processes),
            schedules: Arc::new(schedules),
            governance: Some(Arc::new(governance)),
            label: "harness.db".into(),
        });
    }
    let _ = setup;
    Ok(KernelStores {
        processes: Arc::new(adk_kernel::InMemoryProcessStore::new()),
        schedules: Arc::new(adk_core::InMemoryScheduleStore::new()),
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
            let schedules = adk_session::PostgresScheduleStore::new(url).await?;
            schedules.migrate().await?;
            let governance = adk_session::PostgresGovernanceStateStore::new(url).await?;
            governance.migrate().await?;
            Ok(KernelStores {
                processes: Arc::new(processes),
                schedules: Arc::new(schedules),
                governance: Some(Arc::new(governance)),
                label,
            })
        }
        #[cfg(feature = "redis")]
        Some("redis" | "rediss") => {
            let processes = adk_kernel::RedisProcessStore::connect(url).await?;
            processes.migrate().await?;
            let schedules = adk_session::RedisScheduleStore::new(url).await?;
            schedules.migrate().await?;
            let governance = adk_session::RedisGovernanceStateStore::new(url).await?;
            governance.migrate().await?;
            Ok(KernelStores {
                processes: Arc::new(processes),
                schedules: Arc::new(schedules),
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

/// Writes `contents` to a new owner-only file at `path`.
#[cfg(feature = "audit")]
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file =
        options.open(path).with_context(|| format!("failed to create {}", path.display()))?;
    writeln!(file, "{contents}").with_context(|| format!("failed to write {}", path.display()))
}
