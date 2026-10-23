//! One kernel per tenant over that tenant's `ManagedStores` (spec 3.2 §7.13, §10 M1).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use adk_agent::ambient::CronTrigger;
use adk_core::audit_log::AuditTrail;
use adk_core::{
    ActionLedger, Agent, GovernanceControl, NewRun, OnConflict, RunConfig, ScheduleStore,
    SpendLedger,
};
use adk_managed::state_store::SessionRecord;
use adk_managed::types::{ManagedAgentDef, RuntimeError};
use adk_managed::{AgentResolver, DefaultManagedAgentRuntime, ManagedStores, ModelResolver};
use adk_runner::Runner;
use adk_session::SessionService;
use async_trait::async_trait;
use serde_json::json;

use super::def::{KernelDef, WakeOverlap, WakeSetDef};
use crate::agenda::{AgendaHandle, AgendaStore, ProactivityPolicy};
use crate::consolidation::Consolidator;
use crate::control::ControlPlaneConfig;
use crate::error::{KernelError, Result};
use crate::kernel::{Kernel, KernelConfig, KernelState};
use crate::partition::PartitionId;
use crate::stores::{InMemoryAgendaStore, InMemoryProcessStore, ProcessStore};
use crate::wake::{CronWakeSource, Trigger, Wake};

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
/// | Triggers | the definitions' wake sets ([`KernelDef::wake_sets`]); a cron source per expression |
/// | Proactivity | the hosted definitions' [`KernelDef::proactivity`], one policy per tenant; else the operator's with the consolidation tick off (§13 Q1); the tick runs where the builder registered a consolidator |
/// | Agenda | the tenant's agenda store, and [`agenda_handle`](Self::agenda_handle) rebound to each kernel built |
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
                        "definition '{}' sets a proactivity policy other than definition                          '{other}''s, and tenant `{}` has one kernel with one policy: set the                          same policy in both, or leave it out of one",
                        def.name, self.inner.tenant
                    ),
                });
            }
        }
        if !kernel.partitions.is_empty() || !kernel.drivers.is_empty() {
            tracing::warn!(
                tenant = %self.inner.tenant,
                definition = %def.name,
                partitions = kernel.partitions.len(),
                drivers = kernel.drivers.len(),
                "the definition's partitions and driver manifests are recorded; its agent runs \
                 in the default partition, and no driver is mounted without a person's approval"
            );
        }
        let mut hosted = hosting.hosted.clone();
        hosted.insert(def.name.clone(), Hosted { kernel, digest, agent });
        if let Some(running) = hosting.kernel.take() {
            match self.restart(running, &hosted).await {
                Ok(kernel) => hosting.kernel = Some(kernel),
                Err((kernel, error)) => {
                    hosting.kernel = kernel;
                    return Err(error);
                }
            }
        }
        hosting.hosted = hosted;
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
        Ok(())
    }

    /// Stops the tenant's kernel: running processes park at their next checkpoint, and the
    /// next [`start`](Self::start) resumes them.
    ///
    /// # Errors
    ///
    /// Returns the kernel's stop error; it is stopped regardless.
    pub async fn stop(&self) -> Result<()> {
        let kernel = self.inner.hosting.lock().await.kernel.take();
        match kernel {
            Some(kernel) => kernel.stop().await,
            None => Ok(()),
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
            .agenda_handle(inner.agenda_handle.clone());
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
        let mut crons = BTreeSet::new();
        for (target, hosted) in hosted {
            builder = builder.agent(target.clone(), self.runner(Arc::clone(&hosted.agent))?);
            for set in &hosted.kernel.wake_sets {
                for trigger in triggers(&inner.tenant, target, set) {
                    builder = builder.trigger(trigger);
                }
                for wake in &set.any_of {
                    if let Wake::Cron { expr, .. } = wake {
                        crons.insert(expr.clone());
                    }
                }
            }
        }
        // One source per expression: every trigger on it matches each tick once.
        for expr in crons {
            let source = CronWakeSource::new(cron_source_name(&expr), CronTrigger::new(&expr)?);
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

    /// The runner of a hosted definition's agent: the tenant's app, sessions, kill switch
    /// and approvals.
    fn runner(&self, agent: Arc<dyn Agent>) -> Result<Arc<Runner>> {
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
        Ok(Arc::new(builder.build()?))
    }
}

/// The kernel triggers of `set` on definition `target`: one per wake, named
/// `<target>/<set>`, each enqueueing a run of `target` for the tenant.
fn triggers(tenant: &str, target: &str, set: &WakeSetDef) -> Vec<Trigger> {
    let name = format!("{target}/{}", set.name);
    let on_conflict = match set.overlap {
        WakeOverlap::Queue => OnConflict::Enqueue,
        WakeOverlap::Skip => OnConflict::Reject,
    };
    let run = NewRun::new(
        set.queue_class().as_str(),
        tenant,
        set.user(),
        target,
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

/// The cron source of `expr`, named by the expression with `_` for spaces.
fn cron_source_name(expr: &str) -> String {
    expr.split_whitespace().collect::<Vec<_>>().join("_")
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
        self.tenant.host(def, Arc::clone(&agent)).await.map_err(runtime_error)?;
        Ok(Some(agent))
    }
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
/// store, decision store, state store, approval service or process store another tenant
/// already holds, since a shared run store lets one tenant's workers claim the other's
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
    None
}
