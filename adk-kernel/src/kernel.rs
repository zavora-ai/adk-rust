//! The supervisor (spec 3.2 §7.2).
//!
//! A [`Kernel`] composes the background machinery `adk-rust serve` already builds (§6 D-9)
//! and adds a worker per queue class, the scheduler, the wake sources, the process table,
//! the event bus and the control plane:
//!
//! | Kernel part | Built from |
//! |---|---|
//! | Workers | One `DurableWorker` per [`QueueClass`], each with [`KernelConfig::worker_config`]: `on_governance_refusal: Park` (§10 S3), the class's poll interval, the S1 `WorkerHooks` turned into [`KernelEvent`](crate::KernelEvent)s, and the targets registered with [`KernelBuilder::agent`] (the harness runner as `adk-rust` and `harness.goal`) |
//! | Scheduler | `DurableScheduler::new(schedules, runs).with_governance(..)`, ticking every [`KernelConfig::scheduler_tick`] |
//! | Wake sources | [`KernelBuilder::wake_source`], [`KernelBuilder::trigger`] and [`KernelBuilder::queue_ingress`], routed by the [`WakeRouter`] (§7.4) |
//! | Approvals | `StoreApprovalService::with_run_store`, so a decision wakes the parked run; `spawn_approval_ticker` for expiries |
//! | Kill switch | [`KernelConfig::governance`]: `GovernanceControl::mirrored` with a 250 ms poll on multi-replica ([`KernelConfig::mirrored_governance`], K-I8) |
//! | Control plane | [`ControlPlane`](crate::control::ControlPlane) over [`KernelConfig::control`] (§7.11) |
//!
//! Lifecycle: [`pause`](Kernel::pause) shuts each worker's claim loop down and lets running
//! processes finish their attempt; [`drain`](Kernel::drain) suspends running processes at
//! their next checkpoint through S1's suspend path, then shuts the loops down, and on
//! timeout aborts them so their leases lapse; [`stop`](Kernel::stop) also stops drivers and
//! closes the control plane. A `SIGTERM` is `drain(30 s)` then `stop`
//! ([`shutdown_on_signal`](Kernel::shutdown_on_signal)). Nothing is lost on a hard kill:
//! leases expire and another claim resumes from the checkpoint.
//!
//! [`freeze`](Kernel::freeze) calls `GovernanceControl::freeze_recorded`; parking is the
//! worker's (§10 S3), so every replica's worker parks its own running processes
//! `Waiting` with `ParkReason::Frozen` as its mirrored control observes the freeze.
//! [`unfreeze`](Kernel::unfreeze) calls `unfreeze_recorded` and wakes exactly the
//! freeze-parked processes (K-I21). The kernel registers itself as the
//! `GovernanceController`'s `FreezeDelegate` (§10 S4), so `POST /api/admin/freeze` parks.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::queue_source::QueueIngress;
use adk_core::{
    ApprovalService, DurableRunStore, GovernanceControl, GovernanceMirrorConfig,
    GovernanceStateStore, ScheduleStore, SpendLedger,
};
use adk_runner::Runner;
use adk_server::background::{DurableWorkerConfig, WorkflowExecutor};
use adk_server::rest::{FreezeDelegate, FreezeReport};
use adk_session::SessionService;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::bus::{EventBus, EventSink, EventSubscriber};
use crate::control::ControlPlaneConfig;
use crate::error::{KernelError, Result};
use crate::process::{ProcessTable, ProgressSource};
use crate::sched::QueueClass;
use crate::stores::{InMemoryProcessStore, ProcessStore};
use crate::wake::{Trigger, WakeRouter, WakeSource};

/// How the kernel runs (spec 3.2 §7.2).
///
/// The proactivity policy and quiet hours of §7.2 join in phase 2 (§7.5, §7.9).
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::{KernelConfig, QueueClass};
///
/// let config = KernelConfig::default().with_scheduler_tick(Duration::from_millis(500));
/// assert_eq!(config.app_name, "adk-rust");
/// assert_eq!(config.queues, QueueClass::defaults());
/// let background = config.worker_config(&QueueClass::Background);
/// assert_eq!(background.queue, "background");
/// assert_eq!(background.poll_interval, Duration::from_secs(2));
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct KernelConfig {
    /// The application runs belong to. Defaults to `adk-rust`.
    pub app_name: String,
    /// The kernel home: stores, socket and logs. Defaults to `dirs::config_dir()/adk-rust`.
    pub home: PathBuf,
    /// The queue classes, one worker each (spec 3.2 §7.9). Defaults to interactive,
    /// background and maintenance.
    pub queues: Vec<QueueClass>,
    /// The worker configuration every queue's worker starts from
    /// (`adk-server/src/background/worker.rs`); [`worker_config`](Self::worker_config) sets
    /// the queue and poll interval per class. Its `on_governance_refusal` is `Park` (§10 S3).
    pub worker: DurableWorkerConfig,
    /// How often the scheduler looks for due schedules. Defaults to 1 second.
    pub scheduler_tick: Duration,
    /// The kill switch: `GovernanceControl::mirrored(..)` on multi-replica, with a poll of at
    /// most 250 ms (K-I8). Defaults to a local control.
    pub governance: GovernanceControl,
    /// The control plane: socket path, HTTP bind and the caller's
    /// `RequestContextExtractor` (spec 3.2 §7.11).
    pub control: ControlPlaneConfig,
    /// Refuse to start a driver where no sandbox enforcer exists (spec 3.2 §7.7). Defaults
    /// to `false`.
    pub fail_if_sandbox_unavailable: bool,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            app_name: "adk-rust".to_string(),
            home: dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("adk-rust"),
            queues: QueueClass::defaults(),
            worker: DurableWorkerConfig::default(),
            scheduler_tick: Duration::from_secs(1),
            governance: GovernanceControl::new(),
            control: ControlPlaneConfig::default(),
            fail_if_sandbox_unavailable: false,
        }
    }
}

impl KernelConfig {
    /// The mirrored kill switch's poll interval on multi-replica (K-I8): no backend
    /// implements `GovernanceWatch`, so the default 2 s poll would miss the one-second bound.
    pub const GOVERNANCE_POLL: Duration = Duration::from_millis(250);

    /// Sets the application runs belong to.
    #[must_use]
    pub fn with_app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = app_name.into();
        self
    }

    /// Sets the kernel home.
    #[must_use]
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = home.into();
        self
    }

    /// Sets the queue classes.
    #[must_use]
    pub fn with_queues(mut self, queues: impl IntoIterator<Item = QueueClass>) -> Self {
        self.queues = queues.into_iter().collect();
        self
    }

    /// Sets the worker configuration every queue's worker starts from.
    #[must_use]
    pub fn with_worker(mut self, worker: DurableWorkerConfig) -> Self {
        self.worker = worker;
        self
    }

    /// Sets how often the scheduler looks for due schedules.
    #[must_use]
    pub fn with_scheduler_tick(mut self, tick: Duration) -> Self {
        self.scheduler_tick = tick;
        self
    }

    /// Sets the kill switch.
    #[must_use]
    pub fn with_governance(mut self, governance: GovernanceControl) -> Self {
        self.governance = governance;
        self
    }

    /// Sets the control plane.
    #[must_use]
    pub fn with_control(mut self, control: ControlPlaneConfig) -> Self {
        self.control = control;
        self
    }

    /// Refuses to start a driver where no sandbox enforcer exists.
    #[must_use]
    pub fn with_fail_if_sandbox_unavailable(mut self, fail: bool) -> Self {
        self.fail_if_sandbox_unavailable = fail;
        self
    }

    /// The configuration of the worker for `queue`: [`worker`](Self::worker) with the
    /// class's queue name, its poll interval (spec 3.2 §7.9, §13 Q2) and a worker id
    /// suffixed with the queue.
    pub fn worker_config(&self, queue: &QueueClass) -> DurableWorkerConfig {
        let worker_id = format!("{}:{}", self.worker.worker_id, queue.as_str());
        self.worker
            .clone()
            .with_worker_id(worker_id)
            .with_queue(queue.as_str())
            .with_poll_interval(queue.poll_interval())
    }

    /// A kill switch mirrored from `store` with the 250 ms poll K-I8 needs, for replicas
    /// that share one governance store.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_core::InMemoryGovernanceStateStore;
    /// use adk_kernel::KernelConfig;
    ///
    /// let control = KernelConfig::mirrored_governance(Arc::new(InMemoryGovernanceStateStore::new()));
    /// assert!(control.is_mirrored());
    /// ```
    pub fn mirrored_governance(store: Arc<dyn GovernanceStateStore>) -> GovernanceControl {
        GovernanceControl::mirrored(
            store,
            GovernanceMirrorConfig::default().with_poll_interval(Self::GOVERNANCE_POLL),
        )
    }
}

/// Where the kernel is in its lifecycle (spec 3.2 §7.2).
///
/// # Example
///
/// ```rust
/// use adk_kernel::KernelState;
///
/// assert_eq!(KernelState::Paused.to_string(), "paused");
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum KernelState {
    /// Built, not started.
    #[default]
    Built,
    /// Workers claim, the scheduler ticks, wake sources deliver.
    Running,
    /// Workers do not claim; running processes finish their attempt.
    Paused,
    /// Running processes are being suspended at their next checkpoint.
    Draining,
    /// Stopped; the control plane is closed.
    Stopped,
}

impl fmt::Display for KernelState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Built => "built",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Draining => "draining",
            Self::Stopped => "stopped",
        })
    }
}

/// The kernel's health (spec 3.2 §7.2): per worker, scheduler lag and store latency.
///
/// Driver states join in phase 3.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{KernelHealth, KernelState};
///
/// let health = KernelHealth::default();
/// assert_eq!(health.state, KernelState::Built);
/// assert_eq!(serde_json::to_value(&health).unwrap()["state"], "built");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct KernelHealth {
    /// The lifecycle state.
    pub state: KernelState,
    /// Whether the kill switch is engaged.
    pub frozen: bool,
    /// One entry per queue's worker.
    #[serde(default)]
    pub workers: Vec<WorkerHealth>,
    /// How far the scheduler's last tick ran behind its due time.
    #[serde(
        default,
        rename = "schedulerLagMs",
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_util::option_millis"
    )]
    pub scheduler_lag: Option<Duration>,
    /// The run store's latest round-trip time.
    #[serde(
        default,
        rename = "storeLatencyMs",
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_util::option_millis"
    )]
    pub store_latency: Option<Duration>,
}

/// One worker's health (spec 3.2 §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct WorkerHealth {
    /// The queue class it claims.
    pub queue: QueueClass,
    /// The lease owner name it writes.
    pub worker_id: String,
    /// Whether its claim loop is running.
    pub claiming: bool,
    /// Runs it executes now.
    pub active_runs: usize,
}

impl WorkerHealth {
    /// A worker of `queue` named `worker_id`, not claiming, with no active runs.
    pub fn new(queue: QueueClass, worker_id: impl Into<String>) -> Self {
        Self { queue, worker_id: worker_id.into(), claiming: false, active_runs: 0 }
    }
}

/// The resident supervisor (spec 3.2 §7.2). Cloning shares the kernel.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_kernel::{Kernel, KernelConfig};
///
/// # async fn example(runner: Arc<adk_runner::Runner>) -> adk_kernel::Result<()> {
/// let kernel = Kernel::builder()
///     .config(KernelConfig::default())
///     .run_store(Arc::new(InMemoryRunStore::new()))
///     .agent("adk-rust", runner)
///     .build()?;
/// kernel.start().await?;
/// kernel.shutdown_on_signal().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Kernel {
    inner: Arc<KernelInner>,
}

// Unimplemented: the supervisor reads every part once feat/kernel-supervisor lands
// (spec 3.2 §7.2).
#[allow(dead_code)]
struct KernelInner {
    config: KernelConfig,
    runs: Arc<dyn DurableRunStore>,
    processes: ProcessTable,
    bus: EventBus,
    router: WakeRouter,
    schedules: Option<Arc<dyn ScheduleStore>>,
    approvals: Option<Arc<dyn ApprovalService>>,
    sessions: Option<Arc<dyn SessionService>>,
    spend: Option<Arc<dyn SpendLedger>>,
    audit: Option<AuditTrail>,
    agents: BTreeMap<String, Arc<Runner>>,
    workflows: Option<Arc<dyn WorkflowExecutor>>,
    wake_sources: Vec<Arc<dyn WakeSource>>,
    queue_ingresses: Vec<QueueIngress>,
}

impl fmt::Debug for Kernel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Kernel")
            .field("app_name", &self.inner.config.app_name)
            .field("home", &self.inner.config.home)
            .field("queues", &self.inner.config.queues)
            .finish_non_exhaustive()
    }
}

impl Kernel {
    /// How long [`shutdown_on_signal`](Self::shutdown_on_signal) drains before it stops.
    pub const SIGTERM_DRAIN: Duration = Duration::from_secs(30);

    /// A builder.
    pub fn builder() -> KernelBuilder {
        KernelBuilder::new()
    }

    /// The configuration the kernel was built with.
    pub fn config(&self) -> &KernelConfig {
        &self.inner.config
    }

    /// Starts the workers, scheduler, wake sources and control plane, after reconciling
    /// the wakes missed while the kernel was down. Records `kernel.started`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Lifecycle`] when the kernel already runs,
    /// [`KernelError::ControlPlane`] when the socket or bind is taken, or a store error.
    pub async fn start(&self) -> Result<()> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel::start" })
    }

    /// Stops claiming; running processes run their attempt to its end. Records
    /// `kernel.paused`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Lifecycle`] when the kernel is not running.
    pub async fn pause(&self) -> Result<()> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel::pause" })
    }

    /// Claims again after [`pause`](Self::pause). Records `kernel.resumed`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Lifecycle`] when the kernel is not paused.
    pub async fn resume(&self) -> Result<()> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel::resume" })
    }

    /// Suspends running processes at their next checkpoint through S1's suspend path
    /// ([`Suspension::Drain`](crate::Suspension::Drain)), then stops claiming; on
    /// `timeout` aborts the claim loops so their leases lapse. Records `kernel.drained`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Lifecycle`] when the kernel is not running or paused, or a
    /// store error.
    pub async fn drain(&self, _timeout: Duration) -> Result<()> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel::drain" })
    }

    /// Drains, stops drivers and closes the control plane. Records `kernel.stopped`.
    ///
    /// # Errors
    ///
    /// Returns a store or control-plane error; the kernel is stopped regardless.
    pub async fn stop(&self) -> Result<()> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel::stop" })
    }

    /// Waits for `SIGTERM` or Ctrl-C, then [`drain`](Self::drain)s for
    /// [`SIGTERM_DRAIN`](Self::SIGTERM_DRAIN) and [`stop`](Self::stop)s.
    ///
    /// # Errors
    ///
    /// Returns the error of `drain` or `stop`.
    pub async fn shutdown_on_signal(&self) -> Result<()> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel::shutdown_on_signal" })
    }

    /// Freezes execution as `by`: `GovernanceControl::freeze_recorded`, then the running
    /// processes park `Waiting` at their next checkpoint with
    /// [`Suspension::Freeze`](crate::Suspension::Freeze), one `kernel.process.suspended`
    /// record each, on every replica within one second (K-I8). No process starts until
    /// [`unfreeze`](Self::unfreeze).
    ///
    /// # Errors
    ///
    /// Returns the control's error when the freeze cannot be recorded; this replica is
    /// frozen regardless.
    pub async fn freeze(&self, _reason: &str, _by: AuditActor) -> Result<()> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel::freeze" })
    }

    /// Lifts the freeze as `by`: `GovernanceControl::unfreeze_recorded`, then wakes exactly
    /// the processes the freeze parked, never a manual or preempted suspension (K-I21).
    ///
    /// # Errors
    ///
    /// Returns the control's error when the unfreeze cannot be recorded; execution stays
    /// frozen.
    pub async fn unfreeze(&self, _by: AuditActor) -> Result<()> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel::unfreeze" })
    }

    /// The process table.
    pub fn processes(&self) -> &ProcessTable {
        &self.inner.processes
    }

    /// The event bus.
    pub fn bus(&self) -> &EventBus {
        &self.inner.bus
    }

    /// The wake matcher.
    pub fn wake_router(&self) -> &WakeRouter {
        &self.inner.router
    }

    /// Subscribes to the event bus from now on.
    pub fn events(&self) -> EventSubscriber {
        self.inner.bus.subscribe()
    }

    /// The kernel's health: per worker, scheduler lag, store latency.
    pub fn health(&self) -> KernelHealth {
        // Unimplemented: kernel::Kernel::health reports the live workers, scheduler lag and
        // store latency once feat/kernel-supervisor lands (spec 3.2 §7.2).
        KernelHealth { frozen: self.inner.config.governance.is_frozen(), ..KernelHealth::default() }
    }
}

#[async_trait]
impl FreezeDelegate for Kernel {
    async fn freeze(&self, _reason: &str, _by: AuditActor) -> adk_core::Result<FreezeReport> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel as FreezeDelegate::freeze" }.into())
    }

    async fn unfreeze(&self, _by: AuditActor) -> adk_core::Result<()> {
        Err(KernelError::Unimplemented { item: "kernel::Kernel as FreezeDelegate::unfreeze" }
            .into())
    }
}

/// Builds a [`Kernel`] (spec 3.2 §7.2).
///
/// A run store is required; everything else is optional. The process store defaults to
/// [`InMemoryProcessStore`]. `Harness::kernel(..)` (feature `kernel` of `adk-harness`,
/// §10 HK1) returns one with the harness runner registered as the `adk-rust` and
/// `harness.goal` targets.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_kernel::{Kernel, KernelConfig};
///
/// let kernel = Kernel::builder()
///     .config(KernelConfig::default().with_app_name("ops"))
///     .run_store(Arc::new(InMemoryRunStore::new()))
///     .build()
///     .unwrap();
/// assert_eq!(kernel.config().app_name, "ops");
/// ```
#[derive(Default)]
pub struct KernelBuilder {
    config: KernelConfig,
    runs: Option<Arc<dyn DurableRunStore>>,
    processes: Option<Arc<dyn ProcessStore>>,
    schedules: Option<Arc<dyn ScheduleStore>>,
    approvals: Option<Arc<dyn ApprovalService>>,
    sessions: Option<Arc<dyn SessionService>>,
    spend: Option<Arc<dyn SpendLedger>>,
    audit: Option<AuditTrail>,
    agents: BTreeMap<String, Arc<Runner>>,
    workflows: Option<Arc<dyn WorkflowExecutor>>,
    wake_sources: Vec<Arc<dyn WakeSource>>,
    triggers: Vec<Trigger>,
    sinks: Vec<Arc<dyn EventSink>>,
    progress: Option<Arc<dyn ProgressSource>>,
    queue_ingresses: Vec<QueueIngress>,
}

impl fmt::Debug for KernelBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KernelBuilder")
            .field("config", &self.config)
            .field("agents", &self.agents.keys().collect::<Vec<_>>())
            .field("triggers", &self.triggers.len())
            .field("wake_sources", &self.wake_sources.len())
            .finish_non_exhaustive()
    }
}

impl KernelBuilder {
    /// A builder with the default configuration and no stores.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the configuration.
    #[must_use]
    pub fn config(mut self, config: KernelConfig) -> Self {
        self.config = config;
        self
    }

    /// Sets the durable run store every process lives in. Required.
    #[must_use]
    pub fn run_store(mut self, runs: Arc<dyn DurableRunStore>) -> Self {
        self.runs = Some(runs);
        self
    }

    /// Sets the process side table. Defaults to [`InMemoryProcessStore`].
    #[must_use]
    pub fn process_store(mut self, processes: Arc<dyn ProcessStore>) -> Self {
        self.processes = Some(processes);
        self
    }

    /// Sets the schedule store the kernel's `DurableScheduler` fires.
    #[must_use]
    pub fn schedule_store(mut self, schedules: Arc<dyn ScheduleStore>) -> Self {
        self.schedules = Some(schedules);
        self
    }

    /// Sets the approval service, built with `StoreApprovalService::with_run_store` so a
    /// decision wakes the parked run.
    #[must_use]
    pub fn approval_service(mut self, approvals: Arc<dyn ApprovalService>) -> Self {
        self.approvals = Some(approvals);
        self
    }

    /// Sets the session service `attach` reads transcripts from.
    #[must_use]
    pub fn session_service(mut self, sessions: Arc<dyn SessionService>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// Sets the spend ledger the process views read.
    #[must_use]
    pub fn spend_ledger(mut self, spend: Arc<dyn SpendLedger>) -> Self {
        self.spend = Some(spend);
        self
    }

    /// Sets the signed audit trail for the kernel's records (spec 3.2 §9.3).
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Hosts `runner` as the target `target` on every queue's worker, as
    /// `DurableWorker::with_agent` does.
    #[must_use]
    pub fn agent(mut self, target: impl Into<String>, runner: Arc<Runner>) -> Self {
        self.agents.insert(target.into(), runner);
        self
    }

    /// Hosts workflow targets, as `DurableWorker::with_workflows` does.
    #[must_use]
    pub fn workflows(mut self, workflows: Arc<dyn WorkflowExecutor>) -> Self {
        self.workflows = Some(workflows);
        self
    }

    /// Adds a wake source.
    #[must_use]
    pub fn wake_source(mut self, source: Arc<dyn WakeSource>) -> Self {
        self.wake_sources.push(source);
        self
    }

    /// Adds a standing trigger.
    #[must_use]
    pub fn trigger(mut self, trigger: Trigger) -> Self {
        self.triggers.push(trigger);
        self
    }

    /// Adds an event sink.
    #[must_use]
    pub fn event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sinks.push(sink);
        self
    }

    /// Sets the source of goal progress and loop-guard verdicts for process views.
    #[must_use]
    pub fn progress_source(mut self, source: Arc<dyn ProgressSource>) -> Self {
        self.progress = Some(source);
        self
    }

    /// Adds a queue ingress the kernel runs while started, such as
    /// [`queue_wake_ingress`](crate::queue_wake_ingress).
    #[must_use]
    pub fn queue_ingress(mut self, ingress: QueueIngress) -> Self {
        self.queue_ingresses.push(ingress);
        self
    }

    /// Builds the kernel. Nothing runs until [`Kernel::start`].
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when no run store is set or the control plane
    /// configuration is refused (K-I18).
    pub fn build(self) -> Result<Kernel> {
        let runs = self.runs.ok_or_else(|| KernelError::Config {
            reason: "a kernel needs a durable run store; call KernelBuilder::run_store".into(),
        })?;
        self.config.control.validate()?;
        let processes: Arc<dyn ProcessStore> =
            self.processes.unwrap_or_else(|| Arc::new(InMemoryProcessStore::new()));
        let mut table = ProcessTable::new(runs.clone(), processes.clone());
        if let Some(spend) = &self.spend {
            table = table.with_spend_ledger(spend.clone());
        }
        if let Some(progress) = self.progress {
            table = table.with_progress_source(progress);
        }
        let bus = EventBus::default();
        for sink in self.sinks {
            bus.add_sink(sink);
        }
        let router = self
            .triggers
            .into_iter()
            .fold(WakeRouter::new(runs.clone(), processes), WakeRouter::with_trigger)
            .with_bus(bus.clone());
        Ok(Kernel {
            inner: Arc::new(KernelInner {
                config: self.config,
                runs,
                processes: table,
                bus,
                router,
                schedules: self.schedules,
                approvals: self.approvals,
                sessions: self.sessions,
                spend: self.spend,
                audit: self.audit,
                agents: self.agents,
                workflows: self.workflows,
                wake_sources: self.wake_sources,
                queue_ingresses: self.queue_ingresses,
            }),
        })
    }
}
