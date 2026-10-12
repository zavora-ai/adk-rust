//! One replica: a durable worker running the clerk and a durable scheduler, both under a
//! kill switch mirrored from the governance store, writing to its own signed audit log.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_auth::signed_audit::{AuditLog, Ed25519Signer};
use adk_core::audit_log::AuditTrail;
use adk_core::{
    ApprovalPolicy, ApproverSelector, GovernanceControl, GovernanceMirrorConfig, RunConfig,
    StoreApprovalService,
};
use adk_runner::Runner;
use adk_server::background::{DurableScheduler, DurableWorker, DurableWorkerConfig, WorkerHandle};

use crate::company::{APP, Bank, Provider, QUEUE, QUORUM, TARGET, clerk, policy, principal};
use crate::stores::Stores;

/// How long a claim holds a run; a killed worker's run is recovered after this.
pub const LEASE: Duration = Duration::from_secs(4);

/// The approval policy every held payment is routed to.
pub fn approval_service(stores: &Stores) -> StoreApprovalService {
    StoreApprovalService::new(stores.approvals.clone())
        .with_run_store(stores.runs.clone())
        .with_router(
            ApprovalPolicy::new([ApproverSelector::group("finance")])
                .with_quorum(QUORUM)
                .expires_after(Duration::from_secs(3600)),
        )
}

/// Opens the signed audit log `log_id`, signing with the key in `key_file`.
pub async fn audit_log(
    stores: &Stores,
    key_file: &Path,
    log_id: &str,
) -> anyhow::Result<Arc<AuditLog>> {
    let key = std::fs::read_to_string(key_file)?;
    let signer = Arc::new(Ed25519Signer::from_note_key(key.trim())?);
    // Every record is signed as it is written, so a killed process leaves no unsigned tail.
    Ok(Arc::new(
        AuditLog::builder(stores.audit.clone(), log_id, signer).checkpoint_every(1).open().await?,
    ))
}

/// The kill switch every replica mirrors.
pub fn kill_switch(stores: &Stores) -> GovernanceControl {
    GovernanceControl::mirrored(
        stores.governance.clone(),
        GovernanceMirrorConfig::default().with_poll_interval(Duration::from_millis(500)),
    )
}

/// A running replica.
pub struct Replica {
    pub worker: WorkerHandle,
    pub scheduler: tokio::task::JoinHandle<()>,
    pub mirror: Option<tokio::task::JoinHandle<()>>,
    pub log: Arc<AuditLog>,
}

impl Replica {
    /// Starts the replica `worker_id`. With `crash_marker`, its first lookup never returns.
    pub async fn start(
        stores: &Stores,
        provider: Provider,
        data_dir: &Path,
        worker_id: &str,
        crash_marker: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let log =
            audit_log(stores, &data_dir.join("audit.key"), &format!("audit/{worker_id}")).await?;
        let control = kill_switch(stores);
        control.attach_audit(AuditTrail::new(log.clone()));
        let mirror = control.spawn_mirror();
        control.refresh().await?;

        let bank = Arc::new(Bank::new(data_dir));
        let agent = clerk(provider.model()?, bank, crash_marker)?;
        let runner = Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(stores.sessions.clone())
            .agent_principal(principal(worker_id)?)
            .tool_policy(Arc::new(policy()))
            .governance(control.clone())
            .action_ledger(stores.actions.clone())
            .spend_ledger(stores.spend.clone())
            .durable_run_store(stores.runs.clone())
            .audit_log(log.clone())
            .run_config(
                RunConfig::builder().approval_service(Arc::new(approval_service(stores))).build(),
            )
            .build()?;
        let worker = DurableWorker::new(stores.runs.clone())
            .with_agent(TARGET, Arc::new(runner))
            .with_governance(control.clone())
            .with_config(
                DurableWorkerConfig::default()
                    .with_worker_id(worker_id)
                    .with_queue(QUEUE)
                    .with_lease_ttl(LEASE)
                    .with_poll_interval(Duration::from_millis(250))
                    .with_approval_timeout(Some(Duration::from_secs(3600))),
            )
            .spawn();
        let scheduler = DurableScheduler::new(stores.schedules.clone(), stores.runs.clone())
            .with_governance(control)
            .with_tick_interval(Duration::from_millis(500))
            .spawn();
        tracing::info!(worker.id = %worker_id, "replica started");
        Ok(Self { worker, scheduler, mirror, log })
    }

    /// Stops claiming, lets in-flight runs write their outcome, and signs the log.
    pub async fn stop(self) -> anyhow::Result<()> {
        self.scheduler.abort();
        self.worker.shutdown().await;
        if let Some(mirror) = self.mirror {
            mirror.abort();
        }
        if self.log.size().await > 0 {
            self.log.checkpoint().await?;
        }
        Ok(())
    }

    /// Drops everything without writing, as a killed process would.
    pub fn abort(self) {
        self.scheduler.abort();
        self.worker.abort();
        if let Some(mirror) = self.mirror {
            mirror.abort();
        }
    }
}

/// The body of `durable_company worker`: runs one replica until `stop_file` appears.
pub async fn run_process(
    stores: Stores,
    provider: Provider,
    data_dir: PathBuf,
    worker_id: String,
    crash_marker: Option<PathBuf>,
) -> anyhow::Result<()> {
    let stop_file = data_dir.join(format!("stop-{worker_id}"));
    let replica = Replica::start(&stores, provider, &data_dir, &worker_id, crash_marker).await?;
    while !stop_file.exists() {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    replica.stop().await?;
    tracing::info!(worker.id = %worker_id, "replica stopped");
    Ok(())
}
