//! One kernel per tenant over that tenant's `ManagedStores` (spec 3.2 §7.13, §10 M1).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::schedule::MissedFirePolicy;
use adk_core::{
    ActionLedger, Agent, ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalRisk,
    ApprovalScope, ApprovalState, ApproverSelector, GovernanceControl, NewRun, OnConflict,
    RunConfig, ScheduleStore, SpendLedger, SubjectKind, SubjectRef, ToolConfirmationRequest,
};
use adk_managed::state_store::SessionRecord;
use adk_managed::types::{ManagedAgentDef, RuntimeError};
use adk_managed::{AgentResolver, DefaultManagedAgentRuntime, ManagedStores, ModelResolver};
use adk_runner::Runner;
use adk_session::SessionService;
use async_trait::async_trait;
use serde_json::json;
use sha2::Digest as _;

use super::def::{KernelDef, WakeOverlap, WakeSetDef};
use crate::agenda::{AgendaHandle, AgendaStore, ProactivityPolicy};
use crate::consolidation::Consolidator;
use crate::control::ControlPlaneConfig;
use crate::drivers::{DriverManifest, DriverProposal, DriverRecord, DriverSupervisor};
use crate::error::{KernelError, Result};
use crate::kernel::{Kernel, KernelConfig, KernelState};
use crate::partition::{Partition, PartitionId, PartitionStore, Partitions};
use crate::stores::{
    InMemoryAgendaStore, InMemoryCronWatermarkStore, InMemoryPartitionStore, InMemoryProcessStore,
    ProcessStore,
};
use crate::wake::{CronWakeSource, Trigger, Wake};
use crate::watermark::{CronWatermarkStore, SourceWatermark};

/// One kernel over one tenant's [`ManagedStores`]: the managed form of the kernel (spec 3.2
/// §7.13, §10 M1).
///
/// The kernel hosts a user's or a team's agents on their own infrastructure; the managed
/// runtime hosts tenants. A `TenantKernel` joins the two: the tenant's
/// [`DefaultManagedAgentRuntime`] ([`runtime`](Self::runtime)) runs its sessions, and the
/// tenant's kernel runs the processes its definitions' wake sets start, over the same
/// stores, session service and approvals.
///
/// | Kernel part | From the tenant |
/// |---|---|
/// | Run store | [`ManagedStores::runs`]: the kernel's queues (`interactive`, `background`, `maintenance`) beside the sessions' per-session queues |
/// | Approvals | [`ManagedStores::approval_service`], whose run store wakes the parked process, and [`ManagedStores::decisions`] as each run's approval store |
/// | Targets | one per hosted definition, named like it: the agent the tenant's resolver builds |
/// | Triggers | the definitions' wake sets ([`KernelDef::wake_sets`]); a cron source per expression, its watermark in the tenant's [`CronWatermarkStore`], so ticks missed while no replica of the tenant ran fire once each as the wake sets' `missed` policies pick them (spec 3.2 §7.4) |
/// | Proactivity | the hosted definitions' [`KernelDef::proactivity`], one policy per tenant; else the operator's with the consolidation tick off (§13 Q1); the tick runs where the builder registered a consolidator |
/// | Agenda | the tenant's agenda store, and [`agenda_handle`](Self::agenda_handle) rebound to each kernel built |
/// | Partitions | the tenant's partition table ([`partitions`](Self::partitions)) holding the definitions' [`KernelDef::partitions`], each with the tenant as its tenant; with a [`PartitionAgentResolver`], one agent per partition — one harness per partition — as `<definition>@<partition>`, bound to its partition and acting with its class and tenant (spec 3.2 §7.8) |
/// | Drivers | the tenant's own [`DriverSupervisor`] ([`TenantKernelBuilder::drivers`]), its drivers under a home of the tenant's own: each driver a definition's partition mounts is proposed in the tenant's approval inbox, mounted when a person approves it ([`apply_driver_decisions`](Self::apply_driver_decisions)), its tools' declarations pinned at the first listing, and served to that partition's harness only (spec 3.2 §7.7, §7.8, K-I13) |
/// | App name | the tenant: the `ManagedOwner::app_name` its sessions are created under |
/// | Kill switch | its own `GovernanceControl`, unless the builder is given one |
/// | Control plane | none of its own; mount `control::router(kernel)` on the host's server to serve one |
///
/// A definition is hosted when the runtime resolves it through the [`KernelResolver`]
/// ([`resolver`](Self::resolver)): on `create`, and when a session is rebuilt from the state
/// store. A definition hosted after [`start`](Self::start), or hosted again changed,
/// restarts the kernel over the same stores: its running processes park at their next
/// model-call boundary and the new kernel resumes them. Every runtime instance that hosts
/// the tenant's sessions needs a `TenantKernel` over the same stores.
///
/// Each tenant's stores are its own: a run store two tenants share would let one tenant's
/// workers claim the other's runs. [`ManagedKernels`] refuses a tenant whose stores
/// another tenant holds.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::managed::{KernelDef, TenantKernel, WakeSetDef};
/// use adk_managed::resolver::DefaultModelResolver;
/// use adk_managed::types::{ManagedAgentDef, ModelRef};
/// use adk_managed::{AgentResolver, ManagedAgentRuntime, ManagedStores};
/// use adk_session::InMemorySessionService;
///
/// # async fn example(harnesses: Arc<dyn AgentResolver>) -> Result<(), Box<dyn std::error::Error>> {
/// let tenant = TenantKernel::builder(
///     "tenant-a",
///     ManagedStores::in_memory(),
///     Arc::new(InMemorySessionService::new()),
/// )
/// .resolver(harnesses)
/// .build()?;
/// let runtime = tenant.runtime(Arc::new(DefaultModelResolver::new()));
/// let def = KernelDef::new()
///     .with_wake_set(WakeSetDef::new("morning", [WakeSetDef::cron("0 0 9 * * *")], "Summarize the night."))
///     .apply_to(ManagedAgentDef::new("digest", ModelRef::Shorthand("gemini-3.5-flash-lite".into())));
/// runtime.create(def).await?;
/// tenant.start().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct TenantKernel {
    inner: Arc<TenantInner>,
}

struct TenantInner {
    tenant: String,
    stores: ManagedStores,
    sessions: Arc<dyn SessionService>,
    config: KernelConfig,
    processes: Arc<dyn ProcessStore>,
    schedules: Option<Arc<dyn ScheduleStore>>,
    resolver: Option<Arc<dyn AgentResolver>>,
    action_ledger: Option<Arc<dyn ActionLedger>>,
    spend: Option<Arc<dyn SpendLedger>>,
    audit: Option<AuditTrail>,
    agenda: Arc<dyn AgendaStore>,
    agenda_handle: AgendaHandle,
    consolidators: BTreeMap<PartitionId, Arc<dyn Consolidator>>,
    partitions: Partitions,
    partition_resolver: Option<Arc<dyn PartitionAgentResolver>>,
    watermarks: Arc<dyn CronWatermarkStore>,
    drivers: Option<DriverSupervisor>,
    driver_approvers: ApprovalPolicy,
    /// Applies inbox decisions on driver mounts while the tenant's kernel runs.
    driver_decisions: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    hosting: tokio::sync::Mutex<Hosting>,
}

/// The hosted definitions, and the kernel while the tenant runs.
#[derive(Default)]
struct Hosting {
    hosted: BTreeMap<String, Hosted>,
    kernel: Option<Kernel>,
}

/// One hosted definition.
#[derive(Clone)]
struct Hosted {
    kernel: KernelDef,
    /// The definition as a session record keeps it, to tell a changed definition from the
    /// same one rebuilt.
    digest: String,
    agent: Arc<dyn Agent>,
    /// Each partition's agent, when the definition declares partitions and the tenant has a
    /// partition resolver (spec 3.2 §7.8).
    partitions: BTreeMap<PartitionId, (Partition, Arc<dyn Agent>)>,
}

impl fmt::Debug for TenantKernel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TenantKernel")
            .field("tenant", &self.inner.tenant)
            .field("stores", &self.inner.stores)
            .finish_non_exhaustive()
    }
}

impl TenantKernel {
    /// A builder for `tenant`'s kernel over its `stores` and the session service its
    /// sessions live in.
    pub fn builder(
        tenant: impl Into<String>,
        stores: ManagedStores,
        sessions: Arc<dyn SessionService>,
    ) -> TenantKernelBuilder {
        TenantKernelBuilder {
            tenant: tenant.into(),
            stores,
            sessions,
            config: None,
            processes: None,
            schedules: None,
            resolver: None,
            action_ledger: None,
            spend: None,
            audit: None,
            governance: None,
            agenda: None,
            agenda_handle: None,
            consolidators: BTreeMap::new(),
            partition_store: None,
            partition_resolver: None,
            watermark_store: None,
            drivers: None,
            driver_approvers: None,
        }
    }

    /// The tenant: the app name its runs and sessions belong to.
    pub fn tenant(&self) -> &str {
        &self.inner.tenant
    }

    /// The tenant's stores.
    pub fn stores(&self) -> &ManagedStores {
        &self.inner.stores
    }

    /// The process side table.
    pub fn process_store(&self) -> &Arc<dyn ProcessStore> {
        &self.inner.processes
    }

    /// The tenant's kill switch.
    pub fn governance(&self) -> &GovernanceControl {
        &self.inner.config.governance
    }

    /// The tenant's partition table: its definitions' partitions, each with the tenant as
    /// its tenant, and the crossing ledger (spec 3.2 §7.8).
    pub fn partitions(&self) -> &Partitions {
        &self.inner.partitions
    }

    /// The tenant's cron watermarks: where each cron expression of its wake sets left off
    /// (spec 3.2 §7.4).
    pub fn watermark_store(&self) -> &Arc<dyn CronWatermarkStore> {
        &self.inner.watermarks
    }

    /// The tenant's driver supervisor, when the builder was given one (spec 3.2 §7.7).
    pub fn drivers(&self) -> Option<&DriverSupervisor> {
        self.inner.drivers.as_ref()
    }

    /// Mounts every driver a hosted definition's partition mounts whose proposal a person
    /// approved in the tenant's approval inbox, and returns the ids mounted (spec 3.2 §7.7,
    /// K-I13). While the tenant's kernel runs it does this every scheduler tick; a host may
    /// call it to apply a decision at once.
    ///
    /// The person whose approval completed the request is recorded as the mount's approver
    /// (`kernel.driver.mounted`). A decision by anyone but a person is ignored, and a
    /// rejected proposal mounts nothing. A driver already mounted as proposed is left as it
    /// is, its pins included.
    ///
    /// # Errors
    ///
    /// Returns the approval service's or the driver store's error.
    pub async fn apply_driver_decisions(&self) -> Result<Vec<String>> {
        let (Some(drivers), Some(service)) =
            (&self.inner.drivers, &self.inner.stores.approval_service)
        else {
            return Ok(Vec::new());
        };
        let manifests = {
            let hosting = self.inner.hosting.lock().await;
            served_drivers(&hosting.hosted)
        };
        let mut mounted = Vec::new();
        for (definition, manifest) in manifests {
            if mounted_as_proposed(drivers, &manifest) {
                continue;
            }
            let Some(request) = service.get(&driver_request_id(&manifest)).await? else {
                continue;
            };
            if request.state != ApprovalState::Approved {
                continue;
            }
            let Some(decision) = request.decisions.last() else { continue };
            if decision.approver.kind != SubjectKind::Human {
                tracing::warn!(
                    tenant = %self.inner.tenant,
                    driver.id = %manifest.id,
                    approver = %decision.approver.id,
                    "a driver mount approved by someone other than a person was ignored"
                );
                continue;
            }
            let id = manifest.id.clone();
            drivers.mount(manifest, &decision.approver.id).await?;
            tracing::info!(
                tenant = %self.inner.tenant,
                definition = %definition,
                driver.id = %id,
                by = %decision.approver.id,
                "driver mounted on a person's approval"
            );
            mounted.push(id);
        }
        Ok(mounted)
    }

    /// The handle of the tenant kernel's agenda, rebound to every kernel the tenant builds:
    /// give it to the harnesses the tenant's resolver builds, so their `propose_followup`
    /// reaches the tenant's agenda (spec 3.2 §7.5).
    pub fn agenda_handle(&self) -> &AgendaHandle {
        &self.inner.agenda_handle
    }

    /// The tenant kernel's proactivity policy: the one its hosted definitions set, or the
    /// operator's with the consolidation tick off (spec 3.2 §7.5, §13 Q1).
    pub async fn proactivity(&self) -> ProactivityPolicy {
        let hosting = self.inner.hosting.lock().await;
        self.policy(&hosting.hosted)
    }

    /// The resolver to install with `DefaultManagedAgentRuntime::with_agent_resolver`: it
    /// builds each definition through the tenant's resolver and hosts a definition carrying
    /// a kernel block on this kernel.
    pub fn resolver(&self) -> Arc<dyn AgentResolver> {
        Arc::new(KernelResolver { tenant: self.clone() })
    }

    /// The tenant's managed runtime: its session service and stores, with
    /// [`resolver`](Self::resolver) installed. Further `with_*` settings chain on the value.
    pub fn runtime(&self, models: Arc<dyn ModelResolver>) -> DefaultManagedAgentRuntime {
        DefaultManagedAgentRuntime::new(models, Arc::clone(&self.inner.sessions))
            .with_stores(self.inner.stores.clone())
            .with_agent_resolver(self.resolver())
    }

    /// The kernel while the tenant runs. A definition hosted later restarts the kernel, so
    /// the value returned here stops then; read it again.
    pub async fn kernel(&self) -> Option<Kernel> {
        self.inner.hosting.lock().await.kernel.clone()
    }

    /// The hosted definitions' kernel blocks, by definition name.
    pub async fn definitions(&self) -> BTreeMap<String, KernelDef> {
        let hosting = self.inner.hosting.lock().await;
        hosting.hosted.iter().map(|(name, hosted)| (name.clone(), hosted.kernel.clone())).collect()
    }

    /// Hosts `def` with `agent` as its target when `def` carries a kernel block, and
    /// returns whether it does (spec 3.2 §7.13). The [`KernelResolver`] calls this for
    /// every definition it resolves.
    ///
    /// The same definition hosted again changes nothing; a changed one replaces it, and a
    /// running kernel restarts with it.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for an invalid kernel block, and the error of the
    /// restart when the kernel runs; a refused restart leaves the running kernel as it was.
    pub async fn host(&self, def: &ManagedAgentDef, agent: Arc<dyn Agent>) -> Result<bool> {
        self.host_with(def, agent, BTreeMap::new()).await
    }

    /// Hosts `def` with one agent per partition its kernel block declares (spec 3.2 §7.8),
    /// `agents` keyed by partition id: each runs as `<definition>@<partition>`, bound to its
    /// partition and acting with its class and the tenant, and the coordinator's agent —
    /// else the first partition's by id — also as the definition's own target. A wake set
    /// in a partition starts that partition's agent. Every partition is written to the
    /// tenant's partition table with the tenant as its tenant. [`KernelResolver`] calls this
    /// with the agents the tenant's [`PartitionAgentResolver`] builds.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for an invalid kernel block, a block without
    /// partitions, `agents` not naming exactly the declared partitions, or a partition
    /// another hosted definition declares; and the table's or the restart's error.
    pub async fn host_partitions(
        &self,
        def: &ManagedAgentDef,
        agents: BTreeMap<PartitionId, Arc<dyn Agent>>,
    ) -> Result<bool> {
        let Some(kernel) = KernelDef::from_agent_def(def)? else {
            return Ok(false);
        };
        let declared: BTreeSet<PartitionId> =
            kernel.partitions.iter().map(|partition| partition.id.clone()).collect();
        let given: BTreeSet<PartitionId> = agents.keys().cloned().collect();
        if declared.is_empty() || declared != given {
            return Err(KernelError::Config {
                reason: format!(
                    "definition '{}' declares partitions {declared:?}, and agents were given for \
                     {given:?}: host one agent per declared partition",
                    def.name
                ),
            });
        }
        let primary = kernel.primary_partition().expect("a block with partitions has a primary");
        let agent = Arc::clone(&agents[&primary]);
        self.host_with(def, agent, agents).await
    }

    async fn host_with(
        &self,
        def: &ManagedAgentDef,
        agent: Arc<dyn Agent>,
        agents: BTreeMap<PartitionId, Arc<dyn Agent>>,
    ) -> Result<bool> {
        let Some(kernel) = KernelDef::from_agent_def(def)? else {
            return Ok(false);
        };
        if def.name.trim().is_empty() {
            return Err(KernelError::Config {
                reason: "a definition with a kernel block needs a name: it names the kernel's \
                         target"
                    .into(),
            });
        }
        let digest = serde_json::to_string(&SessionRecord::new("", "", def.clone()).agent)?;
        let mut hosting = self.inner.hosting.lock().await;
        if hosting.hosted.get(&def.name).is_some_and(|hosted| hosted.digest == digest) {
            return Ok(true);
        }
        if let Some(policy) = &kernel.proactivity {
            let other = hosting.hosted.iter().find(|(name, hosted)| {
                **name != def.name
                    && hosted.kernel.proactivity.as_ref().is_some_and(|theirs| theirs != policy)
            });
            if let Some((other, _)) = other {
                return Err(KernelError::Config {
                    reason: format!(
                        "definition '{}' sets a proactivity policy other than definition \
                         '{other}''s, and tenant `{}` has one kernel with one policy: set the \
                         same policy in both, or leave it out of one",
                        def.name, self.inner.tenant
                    ),
                });
            }
        }
        for partition in &kernel.partitions {
            let other = hosting.hosted.iter().find(|(name, hosted)| {
                **name != def.name
                    && hosted.kernel.partitions.iter().any(|theirs| theirs.id == partition.id)
            });
            if let Some((other, _)) = other {
                return Err(KernelError::Config {
                    reason: format!(
                        "definition '{}' declares partition `{}`, which definition '{other}' \
                         declares too, and tenant `{}` has one partition table: declare it in \
                         one definition",
                        def.name, partition.id, self.inner.tenant
                    ),
                });
            }
        }
        if agents.is_empty() && !kernel.partitions.is_empty() {
            tracing::warn!(
                tenant = %self.inner.tenant,
                definition = %def.name,
                partitions = kernel.partitions.len(),
                "the definition's partitions are recorded, and without a partition resolver its \
                 agent runs in the default partition (TenantKernelBuilder::partition_resolver)"
            );
        }
        self.propose_drivers(&def.name, &kernel).await?;
        // The tenant's partition table holds the definition's partitions, the tenant theirs.
        let mut partitions = BTreeMap::new();
        for declared in &kernel.partitions {
            let record = declared.to_partition(&self.inner.tenant)?;
            if self.inner.partitions.get(&record.id).await?.as_ref() != Some(&record) {
                self.inner.partitions.put(record.clone(), AuditActor::default()).await?;
            }
            if let Some(agent) = agents.get(&record.id) {
                partitions.insert(record.id.clone(), (record, Arc::clone(agent)));
            }
        }
        let mut hosted = hosting.hosted.clone();
        hosted.insert(def.name.clone(), Hosted { kernel, digest, agent, partitions });
        if let Some(running) = hosting.kernel.take() {
            match self.restart(running, &hosted).await {
                Ok(kernel) => hosting.kernel = Some(kernel),
                Err((kernel, error)) => {
                    hosting.kernel = kernel;
                    return Err(error);
                }
            }
        }
        let dropped: Vec<String> = cron_expressions(&hosting.hosted)
            .difference(&cron_expressions(&hosted))
            .cloned()
            .collect();
        hosting.hosted = hosted;
        // An expression no hosted wake set uses any more was idle from here on, not missed:
        // its cursor goes with its source, so a wake set that uses it later starts at its
        // first watch (spec 3.2 §7.4).
        for expr in dropped {
            let source = cron_source_name(&expr);
            match self.inner.watermarks.delete(&source).await {
                Ok(_) => {
                    tracing::info!(tenant = %self.inner.tenant, cron = %expr, "cron watermark dropped with the last wake set on it")
                }
                Err(error) => {
                    tracing::warn!(tenant = %self.inner.tenant, cron = %expr, error = %error, "could not drop the cron watermark of an expression no wake set uses")
                }
            }
        }
        tracing::info!(tenant = %self.inner.tenant, definition = %def.name, "definition hosted");
        Ok(true)
    }

    /// Starts the tenant's kernel with the definitions hosted so far.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Lifecycle`] when it already runs, or the kernel's build or
    /// start error.
    pub async fn start(&self) -> Result<()> {
        let mut hosting = self.inner.hosting.lock().await;
        if hosting.kernel.is_some() {
            return Err(KernelError::Lifecycle { state: KernelState::Running, action: "start" });
        }
        let kernel = self.build(&hosting.hosted)?;
        kernel.start().await?;
        hosting.kernel = Some(kernel);
        drop(hosting);
        self.watch_driver_decisions();
        Ok(())
    }

    /// Stops the tenant's kernel: running processes park at their next checkpoint, and the
    /// next [`start`](Self::start) resumes them.
    ///
    /// # Errors
    ///
    /// Returns the kernel's stop error; it is stopped regardless.
    pub async fn stop(&self) -> Result<()> {
        if let Some(task) =
            self.inner.driver_decisions.lock().unwrap_or_else(|e| e.into_inner()).take()
        {
            task.abort();
        }
        let kernel = self.inner.hosting.lock().await.kernel.take();
        match kernel {
            Some(kernel) => kernel.stop().await,
            None => Ok(()),
        }
    }

    /// Proposes each driver of `kernel` that a declared partition mounts, unless it is
    /// mounted as proposed: an approval request in the tenant's inbox showing what a person
    /// approves — the command line, the partitions, the effect classes, the sandbox and the
    /// secrets it names (spec 3.2 §7.7, K-I13). Nothing runs before a person approves it.
    async fn propose_drivers(&self, definition: &str, kernel: &KernelDef) -> Result<()> {
        if kernel.drivers.is_empty() {
            return Ok(());
        }
        let served = served_by_partitions(kernel);
        for manifest in &kernel.drivers {
            if !served.contains(&manifest.id) {
                tracing::warn!(
                    tenant = %self.inner.tenant,
                    definition = %definition,
                    driver.id = %manifest.id,
                    "the driver is recorded and not proposed: no partition of the definition \
                     mounts it (list it in a partition's `drivers`, and the partition in its \
                     manifest's `partitions`)"
                );
            }
        }
        let Some(drivers) = &self.inner.drivers else {
            tracing::warn!(
                tenant = %self.inner.tenant,
                definition = %definition,
                drivers = served.len(),
                "the definition's driver manifests are recorded; the tenant has no driver \
                 supervisor to mount them on (TenantKernelBuilder::drivers)"
            );
            return Ok(());
        };
        let Some(service) = &self.inner.stores.approval_service else {
            tracing::warn!(
                tenant = %self.inner.tenant,
                definition = %definition,
                "the definition's drivers are recorded; the tenant has no approval service to \
                 ask a person to mount them"
            );
            return Ok(());
        };
        for manifest in kernel.drivers.iter().filter(|manifest| served.contains(&manifest.id)) {
            if mounted_as_proposed(drivers, manifest) {
                continue;
            }
            let proposal = DriverProposal::of(manifest);
            let args = serde_json::to_value(&proposal)?;
            let scope = ApprovalScope::new(
                self.inner.tenant.clone(),
                super::DEFAULT_WAKE_USER,
                format!("kernel-driver-{}", manifest.id),
            );
            let call = ToolConfirmationRequest::new(DRIVER_MOUNT_TOOL_NAME, args.clone());
            let request = ApprovalRequest::new(
                driver_request_id(manifest),
                scope,
                &call,
                self.inner.driver_approvers.clone(),
            )
            .with_redacted_args(args)
            .with_requester(SubjectRef::service(format!("definition:{definition}")))
            .with_summary(format!(
                "Mount driver `{}` for definition `{definition}`: {} (partitions {}; sandbox {})",
                manifest.id,
                proposal.command_line,
                proposal
                    .partitions
                    .iter()
                    .map(|partition| partition.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                proposal.sandbox.as_str(),
            ))
            .with_risk(ApprovalRisk::High)
            .with_allowed_actions([ApprovalAction::Approve, ApprovalAction::Reject]);
            service.submit(request).await?;
            tracing::info!(
                tenant = %self.inner.tenant,
                definition = %definition,
                driver.id = %manifest.id,
                "driver mount proposed in the tenant's approval inbox"
            );
        }
        Ok(())
    }

    /// Applies driver decisions every scheduler tick while the tenant's kernel runs.
    fn watch_driver_decisions(&self) {
        if self.inner.drivers.is_none() || self.inner.stores.approval_service.is_none() {
            return;
        }
        let tenant = self.clone();
        let tick = self.inner.config.scheduler_tick.max(std::time::Duration::from_millis(10));
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = tenant.apply_driver_decisions().await {
                    tracing::debug!(tenant = %tenant.inner.tenant, error = %error, "could not apply driver decisions this tick");
                }
            }
        });
        if let Some(previous) =
            self.inner.driver_decisions.lock().unwrap_or_else(|e| e.into_inner()).replace(task)
        {
            previous.abort();
        }
    }

    /// Replaces `running` with a kernel over `hosted`. Built before `running` stops, so a
    /// refused build changes nothing; a new kernel that cannot start gives way to the old
    /// one again.
    async fn restart(
        &self,
        running: Kernel,
        hosted: &BTreeMap<String, Hosted>,
    ) -> std::result::Result<Kernel, (Option<Kernel>, KernelError)> {
        let next = match self.build(hosted) {
            Ok(next) => next,
            Err(error) => return Err((Some(running), error)),
        };
        if let Err(error) = running.stop().await {
            tracing::warn!(tenant = %self.inner.tenant, error = %error, "the replaced kernel stopped with an error");
        }
        match next.start().await {
            Ok(()) => Ok(next),
            Err(error) => {
                let restored = running.start().await.is_ok().then_some(running);
                Err((restored, error))
            }
        }
    }

    /// A kernel over the tenant's stores hosting `hosted`.
    fn build(&self, hosted: &BTreeMap<String, Hosted>) -> Result<Kernel> {
        let inner = &self.inner;
        let config = inner.config.clone().with_proactivity(self.policy(hosted));
        let mut builder = Kernel::builder()
            .config(config)
            .run_store(Arc::clone(&inner.stores.runs))
            .process_store(Arc::clone(&inner.processes))
            .session_service(Arc::clone(&inner.sessions))
            .agenda_store(Arc::clone(&inner.agenda))
            .agenda_handle(inner.agenda_handle.clone())
            .partitions(inner.partitions.clone())
            .watermark_store(Arc::clone(&inner.watermarks));
        for (partition, consolidator) in &inner.consolidators {
            builder = builder.consolidator(partition.clone(), Arc::clone(consolidator));
        }
        if let Some(service) = &inner.stores.approval_service {
            builder = builder.approval_service(Arc::clone(service));
        }
        if let Some(schedules) = &inner.schedules {
            builder = builder.schedule_store(Arc::clone(schedules));
        }
        if let Some(spend) = &inner.spend {
            builder = builder.spend_ledger(Arc::clone(spend));
        }
        if let Some(trail) = &inner.audit {
            builder = builder.audit(trail.clone());
        }
        if let Some(drivers) = &inner.drivers {
            builder = builder.drivers(drivers.clone());
        }
        let mut crons: BTreeMap<String, MissedFirePolicy> = BTreeMap::new();
        for (target, hosted) in hosted {
            match hosted.kernel.primary_partition().filter(|_| !hosted.partitions.is_empty()) {
                // One agent per partition, each bound to its partition; the primary's also
                // serves the definition's own target (spec 3.2 §7.8).
                Some(primary) => {
                    for (id, (partition, agent)) in &hosted.partitions {
                        let runner = self.partition_runner(Arc::clone(agent), partition)?;
                        if *id == primary {
                            builder =
                                builder.partition_agent(id.clone(), target.clone(), runner.clone());
                        }
                        builder = builder.partition_agent(
                            id.clone(),
                            partition_target(target, id),
                            runner,
                        );
                    }
                }
                None => {
                    builder =
                        builder.agent(target.clone(), self.runner(Arc::clone(&hosted.agent))?);
                }
            }
            for set in &hosted.kernel.wake_sets {
                let run_target = match &set.partition {
                    Some(partition) if hosted.partitions.contains_key(partition) => {
                        partition_target(target, partition)
                    }
                    _ => target.clone(),
                };
                for trigger in triggers(&inner.tenant, target, &run_target, set) {
                    builder = builder.trigger(trigger);
                }
                for wake in &set.any_of {
                    if let Wake::Cron { expr, missed } = wake {
                        let widest = crons
                            .get(expr)
                            .map_or(*missed, |held| crate::wake::widest_policy(*held, *missed));
                        crons.insert(expr.clone(), widest);
                    }
                }
            }
        }
        // One source per expression: every trigger on it matches each tick once. Its
        // watermark is the tenant's, so the ticks missed while no replica ran fire once each,
        // as the widest policy on the expression plans them and each wake's own policy picks.
        for (expr, missed) in crons {
            let name = cron_source_name(&expr);
            let watermark = SourceWatermark::new(Arc::clone(&inner.watermarks), name.clone());
            let source = CronWakeSource::watermarked(name, &expr, missed, Arc::new(watermark))?;
            builder = builder.wake_source(Arc::new(source));
        }
        builder.build()
    }

    /// The policy of a kernel hosting `hosted`: the first definition's that sets one, or the
    /// operator's with the consolidation tick off, since only a definition turns it on in the
    /// managed form (spec 3.2 §13 Q1).
    fn policy(&self, hosted: &BTreeMap<String, Hosted>) -> ProactivityPolicy {
        if let Some(policy) = hosted.values().find_map(|hosted| hosted.kernel.proactivity.clone()) {
            return policy;
        }
        let mut policy = self.inner.config.proactivity.clone();
        policy.consolidation = None;
        policy
    }

    /// The runner of partition `partition`'s agent: the tenant's, acting with the
    /// partition's class and tenant (spec 3.2 §7.8).
    fn partition_runner(
        &self,
        agent: Arc<dyn Agent>,
        partition: &Partition,
    ) -> Result<Arc<Runner>> {
        let identity = partition.identity_source(&self.inner.tenant, agent.name());
        self.runner_with(agent, Some(identity))
    }

    /// The runner of a hosted definition's agent: the tenant's app, sessions, kill switch
    /// and approvals.
    fn runner(&self, agent: Arc<dyn Agent>) -> Result<Arc<Runner>> {
        self.runner_with(agent, None)
    }

    fn runner_with(
        &self,
        agent: Arc<dyn Agent>,
        identity: Option<Arc<dyn adk_core::IdentitySource>>,
    ) -> Result<Arc<Runner>> {
        let inner = &self.inner;
        let mut config = RunConfig::builder();
        if let Some(service) = &inner.stores.approval_service {
            config = config.approval_service(Arc::clone(service));
        }
        let mut builder = Runner::builder()
            .app_name(inner.tenant.as_str())
            .agent(agent)
            .session_service(Arc::clone(&inner.sessions))
            .run_config(config.build())
            .approval_store(Arc::clone(&inner.stores.decisions))
            .governance(inner.config.governance.clone());
        if let Some(ledger) = &inner.action_ledger {
            builder = builder.action_ledger(Arc::clone(ledger));
        }
        if let Some(spend) = &inner.spend {
            builder = builder.spend_ledger(Arc::clone(spend));
        }
        if let Some(identity) = identity {
            builder = builder.identity_source(identity);
        }
        Ok(Arc::new(builder.build()?))
    }
}

/// The target of partition `partition`'s agent of definition `target`:
/// `<definition>@<partition>` (spec 3.2 §7.8).
fn partition_target(target: &str, partition: &PartitionId) -> String {
    format!("{target}@{partition}")
}

/// The kernel triggers of `set` on definition `target`: one per wake, named
/// `<target>/<set>`, each enqueueing a run of `run_target` — the definition's target, or its
/// partition's agent — for the tenant.
fn triggers(tenant: &str, target: &str, run_target: &str, set: &WakeSetDef) -> Vec<Trigger> {
    let name = format!("{target}/{}", set.name);
    let on_conflict = match set.overlap {
        WakeOverlap::Queue => OnConflict::Enqueue,
        WakeOverlap::Skip => OnConflict::Reject,
    };
    let run = NewRun::new(
        set.queue_class().as_str(),
        tenant,
        set.user(),
        run_target,
        json!({ "text": set.input }),
    )
    .with_concurrency_key(format!("wake-set:{name}"), on_conflict);
    set.any_of
        .iter()
        .map(|wake| {
            Trigger::new(name.clone(), wake.clone(), run.clone())
                .with_partition(KernelDef::partition_of(set))
        })
        .collect()
}

/// The cron expressions the wake sets of `hosted` use.
fn cron_expressions(hosted: &BTreeMap<String, Hosted>) -> BTreeSet<String> {
    hosted
        .values()
        .flat_map(|hosted| hosted.kernel.wake_sets.iter())
        .flat_map(|set| set.any_of.iter())
        .filter_map(|wake| match wake {
            Wake::Cron { expr, .. } => Some(expr.clone()),
            _ => None,
        })
        .collect()
}

/// The cron source of `expr`, named by the expression with `_` for spaces.
fn cron_source_name(expr: &str) -> String {
    expr.split_whitespace().collect::<Vec<_>>().join("_")
}

/// The approval-inbox tool name of a driver mount a hosted definition asks for (spec 3.2
/// §7.7): the proposal waits in the tenant's inbox as an `ApprovalRequest` with this
/// `tool_name`, the `DriverProposal` as `args_redacted` and `allowed_actions = [Approve,
/// Reject]`.
pub const DRIVER_MOUNT_TOOL_NAME: &str = "kernel:driver.mount";

/// The approver group of a driver mount unless [`TenantKernelBuilder::driver_approvers`]
/// names others: the control plane's operators.
const DRIVER_APPROVER_GROUP: &str = "operator";

/// The ids of `kernel`'s drivers that a declared partition mounts: listed in the partition's
/// `drivers`, with the partition in the manifest's `partitions`.
fn served_by_partitions(kernel: &KernelDef) -> Vec<String> {
    kernel
        .drivers
        .iter()
        .filter(|manifest| {
            kernel.partitions.iter().any(|partition| {
                partition.drivers.contains(&manifest.id)
                    && manifest.partitions.contains(&partition.id)
            })
        })
        .map(|manifest| manifest.id.clone())
        .collect()
}

/// Every driver the hosted definitions' partitions mount, with its definition.
fn served_drivers(hosted: &BTreeMap<String, Hosted>) -> Vec<(String, DriverManifest)> {
    hosted
        .iter()
        .flat_map(|(definition, hosted)| {
            let served = served_by_partitions(&hosted.kernel);
            hosted
                .kernel
                .drivers
                .iter()
                .filter(move |manifest| served.contains(&manifest.id))
                .map(|manifest| (definition.clone(), manifest.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Whether `manifest` is mounted with what its proposal shows: the same command, partitions,
/// effect classes, sandbox, secrets and lifecycle.
fn mounted_as_proposed(drivers: &DriverSupervisor, manifest: &DriverManifest) -> bool {
    drivers.record(&manifest.id).is_some_and(|record: DriverRecord| {
        DriverProposal::of(&record.manifest) == DriverProposal::of(manifest)
    })
}

/// The inbox request id of mounting `manifest`: `kernel-driver-<id>-<digest>`, the digest of
/// what the proposal shows, so a changed manifest is asked again.
fn driver_request_id(manifest: &DriverManifest) -> String {
    let proposal =
        serde_json::to_vec(&DriverProposal::of(manifest)).expect("a proposal always serializes");
    let digest = sha2::Sha256::digest(&proposal);
    let hex: String = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    let id: String = manifest
        .id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("kernel-driver-{id}-{hex}")
}

/// The directory name of `tenant` under the kernel home: its letters, digits, `-` and `_`,
/// and a digest of the whole name, so two tenants never share one.
fn tenant_dir(tenant: &str) -> String {
    let readable: String = tenant
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let digest = sha2::Sha256::digest(tenant.as_bytes());
    let hex: String = digest[..4].iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{readable}-{hex}")
}

/// Builds `TenantKernel` (spec 3.2 §7.13, §10 M1).
pub struct TenantKernelBuilder {
    tenant: String,
    stores: ManagedStores,
    sessions: Arc<dyn SessionService>,
    config: Option<KernelConfig>,
    processes: Option<Arc<dyn ProcessStore>>,
    schedules: Option<Arc<dyn ScheduleStore>>,
    resolver: Option<Arc<dyn AgentResolver>>,
    action_ledger: Option<Arc<dyn ActionLedger>>,
    spend: Option<Arc<dyn SpendLedger>>,
    audit: Option<AuditTrail>,
    governance: Option<GovernanceControl>,
    agenda: Option<Arc<dyn AgendaStore>>,
    agenda_handle: Option<AgendaHandle>,
    consolidators: BTreeMap<PartitionId, Arc<dyn Consolidator>>,
    partition_store: Option<Arc<dyn PartitionStore>>,
    partition_resolver: Option<Arc<dyn PartitionAgentResolver>>,
    watermark_store: Option<Arc<dyn CronWatermarkStore>>,
    drivers: Option<DriverSupervisor>,
    driver_approvers: Option<ApprovalPolicy>,
}

impl fmt::Debug for TenantKernelBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TenantKernelBuilder").field("tenant", &self.tenant).finish_non_exhaustive()
    }
}

impl TenantKernelBuilder {
    /// Sets the kernel configuration. Its `app_name` is replaced by the tenant and its control
    /// plane cleared — every tenant's kernel would otherwise open the socket of one shared
    /// home — and its proactivity policy gives the limits of a tenant whose definitions set
    /// none, with the consolidation tick off (spec 3.2 §13 Q1). Serve a tenant's control
    /// plane by mounting `control::router(kernel)` on the host's server.
    #[must_use]
    pub fn config(mut self, config: KernelConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Sets the process side table, the tenant's own. Defaults to in memory.
    #[must_use]
    pub fn process_store(mut self, processes: Arc<dyn ProcessStore>) -> Self {
        self.processes = Some(processes);
        self
    }

    /// Sets the schedule store the kernel's scheduler fires, the tenant's own.
    #[must_use]
    pub fn schedule_store(mut self, schedules: Arc<dyn ScheduleStore>) -> Self {
        self.schedules = Some(schedules);
        self
    }

    /// Builds the definitions' agents with `resolver`, such as `adk-harness`'s
    /// `HarnessResolver`. A definition with a kernel block needs an agent it builds.
    #[must_use]
    pub fn resolver(mut self, resolver: Arc<dyn AgentResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Sets the action ledger the kernel's runs record non-idempotent calls in, so a resumed
    /// process never runs a completed call again (K-I4).
    #[must_use]
    pub fn action_ledger(mut self, ledger: Arc<dyn ActionLedger>) -> Self {
        self.action_ledger = Some(ledger);
        self
    }

    /// Sets the spend ledger.
    #[must_use]
    pub fn spend_ledger(mut self, spend: Arc<dyn SpendLedger>) -> Self {
        self.spend = Some(spend);
        self
    }

    /// Sets the signed audit trail of the kernel's records.
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Sets the kill switch, such as one mirrored across the tenant's replicas; it replaces
    /// the configuration's.
    #[must_use]
    pub fn governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Sets the agenda's store, the tenant's own. Defaults to in memory.
    #[must_use]
    pub fn agenda_store(mut self, store: Arc<dyn AgendaStore>) -> Self {
        self.agenda = Some(store);
        self
    }

    /// Binds `handle` to every kernel the tenant builds, such as the handle the tenant's
    /// harnesses were given (`HarnessAgentBuilder::agenda_handle`), so their follow-ups reach
    /// the tenant's agenda (spec 3.2 §7.5). Defaults to a new handle.
    #[must_use]
    pub fn agenda_handle(mut self, handle: AgendaHandle) -> Self {
        self.agenda_handle = Some(handle);
        self
    }

    /// Registers `consolidator` for `partition`: the tenant's kernel runs its consolidation
    /// tick there when a hosted definition's policy turns the tick on (spec 3.2 §7.5,
    /// §13 Q1).
    #[must_use]
    pub fn consolidator(
        mut self,
        partition: impl Into<PartitionId>,
        consolidator: Arc<dyn Consolidator>,
    ) -> Self {
        self.consolidators.insert(partition.into(), consolidator);
        self
    }

    /// Sets the tenant's partition table and crossing ledger, the tenant's own (spec 3.2
    /// §7.8). Defaults to in memory.
    #[must_use]
    pub fn partition_store(mut self, store: Arc<dyn PartitionStore>) -> Self {
        self.partition_store = Some(store);
        self
    }

    /// Builds one agent per partition a definition declares with `resolver`, such as
    /// `adk-harness`'s `HarnessResolver`: one harness per partition (spec 3.2 §7.8). Without
    /// one, a definition's partitions are recorded and its agent runs in the default
    /// partition.
    #[must_use]
    pub fn partition_resolver(mut self, resolver: Arc<dyn PartitionAgentResolver>) -> Self {
        self.partition_resolver = Some(resolver);
        self
    }

    /// Sets where the tenant's cron sources keep their watermarks, the tenant's own (spec 3.2
    /// §7.4, §7.13): on a durable store, a tick missed while no replica of the tenant ran
    /// fires once each as the wake set's `missed` policy picks it, when the next replica
    /// starts. Defaults to in memory, which keeps nothing across a restart.
    #[must_use]
    pub fn watermark_store(mut self, store: Arc<dyn CronWatermarkStore>) -> Self {
        self.watermark_store = Some(store);
        self
    }

    /// Mounts the drivers the tenant's definitions ask for on `drivers`, the tenant's own
    /// supervisor over the tenant's own driver store (spec 3.2 §7.7, §7.13). Each driver a
    /// definition's partition mounts is proposed in the tenant's approval inbox and mounted
    /// when a person approves it; its tools reach that partition's harness only. Its drivers
    /// run as the tenant's app under `<kernel home>/tenants/<tenant>` unless the
    /// supervisor's builder set an app name or a home. Without one, a definition's driver
    /// manifests are recorded and none is mounted.
    #[must_use]
    pub fn drivers(mut self, drivers: DriverSupervisor) -> Self {
        self.drivers = Some(drivers);
        self
    }

    /// Who approves a driver mount: by default anyone in the `operator` group, as the
    /// control plane's operators are. Only a person's approval mounts a driver.
    #[must_use]
    pub fn driver_approvers(mut self, approvers: ApprovalPolicy) -> Self {
        self.driver_approvers = Some(approvers);
        self
    }

    /// Builds the tenant's kernel. Nothing runs until [`TenantKernel::start`].
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when the tenant is blank.
    pub fn build(self) -> Result<TenantKernel> {
        if self.tenant.trim().is_empty() {
            return Err(KernelError::Config {
                reason: "a tenant kernel needs the tenant's name: the app name its sessions \
                         are created under"
                    .into(),
            });
        }
        let mut config = self.config.unwrap_or_default();
        config.app_name = self.tenant.clone();
        config.control = ControlPlaneConfig::default().without_socket();
        if let Some(governance) = self.governance {
            config.governance = governance;
        }
        if let Some(drivers) = &self.drivers {
            drivers.host_defaults(
                &self.tenant,
                &config.home.join("tenants").join(tenant_dir(&self.tenant)),
            );
        }
        let mut partitions = Partitions::new(
            self.partition_store.unwrap_or_else(|| Arc::new(InMemoryPartitionStore::new())),
        );
        if let Some(trail) = &self.audit {
            partitions = partitions.with_audit(trail.clone());
        }
        Ok(TenantKernel {
            inner: Arc::new(TenantInner {
                tenant: self.tenant,
                stores: self.stores,
                sessions: self.sessions,
                config,
                processes: self.processes.unwrap_or_else(|| Arc::new(InMemoryProcessStore::new())),
                schedules: self.schedules,
                resolver: self.resolver,
                action_ledger: self.action_ledger,
                spend: self.spend,
                audit: self.audit,
                agenda: self.agenda.unwrap_or_else(|| Arc::new(InMemoryAgendaStore::new())),
                agenda_handle: self.agenda_handle.unwrap_or_default(),
                consolidators: self.consolidators,
                partitions,
                partition_resolver: self.partition_resolver,
                watermarks: self
                    .watermark_store
                    .unwrap_or_else(|| Arc::new(InMemoryCronWatermarkStore::new())),
                drivers: self.drivers,
                driver_approvers: self.driver_approvers.unwrap_or_else(|| {
                    ApprovalPolicy::new([ApproverSelector::group(DRIVER_APPROVER_GROUP)])
                }),
                driver_decisions: std::sync::Mutex::new(None),
                hosting: tokio::sync::Mutex::new(Hosting::default()),
            }),
        })
    }
}

/// Resolves managed definitions for a [`TenantKernel`] (spec 3.2 §7.13, §10 M1): an
/// `AgentResolver` for `DefaultManagedAgentRuntime::with_agent_resolver`.
///
/// | Definition | Resolved as |
/// |---|---|
/// | no kernel block | whatever the tenant's resolver answers, `None` leaving it to `build_agent` |
/// | a kernel block, an agent the tenant's resolver builds | that agent, hosted on the tenant's kernel |
/// | a kernel block with partitions, and a [`PartitionAgentResolver`] | one agent per partition, hosted with [`TenantKernel::host_partitions`]; the runtime's sessions run the coordinator's, else the first partition's by id (spec 3.2 §7.8) |
/// | a kernel block, no agent the resolver builds | refused: a kernel hosts an agent the operator's resolver builds, such as a harness definition |
/// | an invalid kernel block | refused as an invalid request, naming the key |
///
/// Built by [`TenantKernel::resolver`].
pub struct KernelResolver {
    tenant: TenantKernel,
}

impl fmt::Debug for KernelResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KernelResolver").field("tenant", &self.tenant.tenant()).finish()
    }
}

#[async_trait]
impl AgentResolver for KernelResolver {
    async fn resolve(
        &self,
        def: &ManagedAgentDef,
        models: &dyn ModelResolver,
    ) -> std::result::Result<Option<Arc<dyn Agent>>, RuntimeError> {
        let block = KernelDef::from_agent_def(def).map_err(runtime_error)?;
        let agent = match &self.tenant.inner.resolver {
            Some(resolver) => resolver.resolve(def, models).await?,
            None => None,
        };
        if block.is_none() {
            return Ok(agent);
        }
        let Some(agent) = agent else {
            return Err(RuntimeError::invalid_request(format!(
                "definition '{}' carries a kernel block (`{}`), and the tenant's resolver builds \
                 no agent for it: a kernel hosts an agent the operator's resolver builds, such \
                 as a harness definition (`adk.harness`) under HarnessResolver",
                def.name,
                super::KERNEL_DEF_METADATA_KEY
            )));
        };
        let partitions = match &block {
            Some(block) if !block.partitions.is_empty() => {
                self.tenant.inner.partition_resolver.as_ref().map(|resolver| (block, resolver))
            }
            _ => None,
        };
        let Some((block, resolver)) = partitions else {
            self.tenant.host(def, Arc::clone(&agent)).await.map_err(runtime_error)?;
            return Ok(Some(agent));
        };
        // One agent per partition, each in the tenant's partition table (spec 3.2 §7.8).
        let mut agents = BTreeMap::new();
        for declared in &block.partitions {
            let host = PartitionHost {
                partition: declared.to_partition(self.tenant.tenant()).map_err(runtime_error)?,
                partitions: self.tenant.partitions().clone(),
                coordinator: block.coordinator.as_ref() == Some(&declared.id),
                drivers: self.tenant.drivers().cloned(),
            };
            let Some(agent) = resolver.resolve_partition(def, &host, models).await? else {
                return Err(RuntimeError::invalid_request(format!(
                    "definition '{}' declares partition `{}`, and the tenant's partition \
                     resolver builds no agent for it",
                    def.name, declared.id
                )));
            };
            agents.insert(declared.id.clone(), agent);
        }
        let primary = block.primary_partition().expect("a block with partitions has a primary");
        let agent = Arc::clone(&agents[&primary]);
        self.tenant.host_partitions(def, agents).await.map_err(runtime_error)?;
        Ok(Some(agent))
    }
}

/// Builds the agent of one partition of a hosted definition (spec 3.2 §7.8, §7.13): one
/// harness per partition, built with the partition's mode, class and tenant, memory scope,
/// drivers and crossing policy over the tenant's partition table. `adk-harness`'s
/// `HarnessResolver` implements it.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::Agent;
/// use adk_kernel::managed::{PartitionAgentResolver, PartitionHost};
/// use adk_managed::resolver::ModelResolver;
/// use adk_managed::types::{ManagedAgentDef, RuntimeError};
///
/// struct Builds;
///
/// #[async_trait::async_trait]
/// impl PartitionAgentResolver for Builds {
///     async fn resolve_partition(
///         &self,
///         _def: &ManagedAgentDef,
///         host: &PartitionHost,
///         _models: &dyn ModelResolver,
///     ) -> Result<Option<Arc<dyn Agent>>, RuntimeError> {
///         // Build the partition's harness with `host.partition` and `host.partitions`.
///         let _ = (&host.partition, &host.partitions, host.coordinator);
///         Ok(None)
///     }
/// }
/// ```
#[async_trait]
pub trait PartitionAgentResolver: Send + Sync {
    /// The agent of `host.partition` for `def`, or `None` when this resolver builds none.
    ///
    /// # Errors
    ///
    /// Returns the error that kept the agent from being built; the definition is refused.
    async fn resolve_partition(
        &self,
        def: &ManagedAgentDef,
        host: &PartitionHost,
        models: &dyn ModelResolver,
    ) -> std::result::Result<Option<Arc<dyn Agent>>, RuntimeError>;
}

/// The partition a [`PartitionAgentResolver`] builds an agent for (spec 3.2 §7.8).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PartitionHost {
    /// The partition's record, its tenant the hosting tenant.
    pub partition: Partition,
    /// The tenant's partition table, which the partition's crossing policy reads.
    pub partitions: Partitions,
    /// Whether the partition's agent is the coordinator ([`KernelDef::coordinator`]).
    pub coordinator: bool,
    /// The tenant's driver supervisor, when it has one: the partition's harness mounts the
    /// drivers its `drivers` names from it (`HarnessAgentBuilder::partition_drivers`), and
    /// the coordinator's offers the other partitions' drivers as crossings (spec 3.2 §7.7,
    /// §7.8).
    pub drivers: Option<DriverSupervisor>,
}

fn runtime_error(error: KernelError) -> RuntimeError {
    match error {
        KernelError::Config { reason } => RuntimeError::invalid_request(reason),
        other => RuntimeError::internal(other.to_string()),
    }
}

/// The tenants a host runs, one kernel each (spec 3.2 §7.13).
///
/// A tenant's stores are its own: [`insert`](Self::insert) refuses a tenant whose run
/// store, decision store, state store, approval service, process store, partition store, cron
/// watermark store, driver supervisor or driver store another tenant already holds, since a shared run store lets one tenant's workers claim the other's
/// runs. Stores that are distinct values over one database are not detected; give each
/// tenant its own database, schema or key namespace.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::managed::{ManagedKernels, TenantKernel};
/// use adk_managed::ManagedStores;
/// use adk_session::InMemorySessionService;
///
/// let sessions = Arc::new(InMemorySessionService::new());
/// let kernels = ManagedKernels::new();
/// let a = TenantKernel::builder("tenant-a", ManagedStores::in_memory(), sessions.clone()).build()?;
/// kernels.insert(a.clone())?;
/// let shared = TenantKernel::builder("tenant-b", a.stores().clone(), sessions).build()?;
/// assert!(kernels.insert(shared).is_err(), "tenant-b may not run on tenant-a's stores");
/// assert_eq!(kernels.tenants(), ["tenant-a"]);
/// # Ok::<(), adk_kernel::KernelError>(())
/// ```
#[derive(Clone, Default)]
pub struct ManagedKernels {
    tenants: Arc<std::sync::RwLock<BTreeMap<String, TenantKernel>>>,
}

impl fmt::Debug for ManagedKernels {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManagedKernels").field("tenants", &self.tenants()).finish()
    }
}

impl ManagedKernels {
    /// No tenants.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `tenant`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when a tenant of the same name exists or another
    /// tenant holds one of its stores.
    pub fn insert(&self, tenant: TenantKernel) -> Result<()> {
        let mut tenants = self.tenants.write().unwrap_or_else(|e| e.into_inner());
        if tenants.contains_key(tenant.tenant()) {
            return Err(KernelError::Config {
                reason: format!("tenant `{}` already has a kernel", tenant.tenant()),
            });
        }
        for other in tenants.values() {
            if let Some(store) = shared_store(&tenant, other) {
                return Err(KernelError::Config {
                    reason: format!(
                        "tenant `{}` would share its {store} with tenant `{}`; give each \
                         tenant its own stores",
                        tenant.tenant(),
                        other.tenant()
                    ),
                });
            }
        }
        tenants.insert(tenant.tenant().to_string(), tenant);
        Ok(())
    }

    /// The kernel of `tenant`.
    pub fn get(&self, tenant: &str) -> Option<TenantKernel> {
        self.tenants.read().unwrap_or_else(|e| e.into_inner()).get(tenant).cloned()
    }

    /// The tenants' names, sorted.
    pub fn tenants(&self) -> Vec<String> {
        self.tenants.read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
    }

    /// Starts every tenant's kernel that does not run yet.
    ///
    /// # Errors
    ///
    /// Returns the first start error; the tenants before it keep running.
    pub async fn start(&self) -> Result<()> {
        let tenants: Vec<TenantKernel> =
            self.tenants.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        for tenant in tenants {
            if tenant.kernel().await.is_none() {
                tenant.start().await?;
            }
        }
        Ok(())
    }

    /// Stops every tenant's kernel.
    ///
    /// # Errors
    ///
    /// Returns the first stop error; every kernel is stopped regardless.
    pub async fn stop(&self) -> Result<()> {
        let tenants: Vec<TenantKernel> =
            self.tenants.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        let mut first = None;
        for tenant in tenants {
            if let Err(error) = tenant.stop().await {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
    }
}

/// The store `a` and `b` share, by name, if any.
fn shared_store(a: &TenantKernel, b: &TenantKernel) -> Option<&'static str> {
    fn same<T: ?Sized>(a: &Arc<T>, b: &Arc<T>) -> bool {
        std::ptr::eq(Arc::as_ptr(a) as *const (), Arc::as_ptr(b) as *const ())
    }
    let (x, y) = (a.stores(), b.stores());
    if same(&x.runs, &y.runs) {
        return Some("run store");
    }
    if same(&x.decisions, &y.decisions) {
        return Some("decision store");
    }
    if same(&x.state, &y.state) {
        return Some("state store");
    }
    if let (Some(p), Some(q)) = (&x.approval_service, &y.approval_service)
        && same(p, q)
    {
        return Some("approval service");
    }
    if same(a.process_store(), b.process_store()) {
        return Some("process store");
    }
    if same(a.partitions().store(), b.partitions().store()) {
        return Some("partition store");
    }
    if same(a.watermark_store(), b.watermark_store()) {
        return Some("cron watermark store");
    }
    if let (Some(p), Some(q)) = (a.drivers(), b.drivers()) {
        if p.same(q) {
            return Some("driver supervisor");
        }
        if same(p.store(), q.store()) {
            return Some("driver store");
        }
    }
    None
}
