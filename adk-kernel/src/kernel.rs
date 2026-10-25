//! The supervisor (spec 3.2 §7.2).
//!
//! A [`Kernel`] composes the background machinery `adk-rust serve` already builds (§6 D-9)
//! and adds a worker per queue class, the scheduler, the wake sources, the process table,
//! the event bus and the control plane:
//!
//! | Kernel part | Built from |
//! |---|---|
//! | Workers | One `DurableWorker` per [`QueueClass`], each with [`KernelConfig::worker_config`]: `on_governance_refusal: Park` (§10 S3), the class's poll interval, the S1 `WorkerHooks` turned into [`KernelEvent`]s, and the targets registered with [`KernelBuilder::agent`] (the harness runner as `adk-rust` and `harness.goal`) |
//! | Scheduler | `DurableScheduler::new(schedules, runs).with_governance(..).with_clock(..)`, ticking every [`KernelConfig::scheduler_tick`] on [`KernelConfig::clock`] |
//! | Wake sources | [`KernelBuilder::wake_source`], [`KernelBuilder::trigger`] and [`KernelBuilder::queue_ingress`], routed by the [`WakeRouter`] (§7.4) |
//! | Approvals | `StoreApprovalService::with_run_store`, so a decision wakes the parked run; `spawn_approval_ticker` for expiries |
//! | Kill switch | [`KernelConfig::governance`]: `GovernanceControl::mirrored` with a 250 ms poll on multi-replica ([`KernelConfig::mirrored_governance`], K-I8) |
//! | Control plane | [`ControlPlane`] over [`KernelConfig::control`] (§7.11) |
//! | `/proc` (feature `proc`) | `KernelBuilder::ledger`: a sink on the bus writes every event's ledger row; the hosted runtime's `ProgressSource` gets the ledger at start; with `proc-baselines` the maintenance tick enqueues the daily `kernel.proc` pass, and [`KernelBuilder::observation_audience`] puts a partition's observations to a person as agenda questions (§7.10) |
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

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::queue_source::QueueIngress;
use adk_core::{
    ApprovalService, Clock, DurableRunStore, Enqueued, GovernanceControl, GovernanceMirrorConfig,
    GovernanceStateStore, ScheduleStore, SpendLedger, SystemClock,
};
use adk_runner::Runner;
use adk_server::background::{
    DurableScheduler, DurableWorker, DurableWorkerConfig, WorkflowExecutor, WorkflowRunContext,
    WorkflowState,
};
use adk_server::rest::{FreezeDelegate, FreezeReport};
use adk_session::SessionService;
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agenda::{Agenda, AgendaHandle, AgendaStore, ObservationAudience, ProactivityPolicy};
use crate::audit;
use crate::bus::{EventBus, EventSink, EventSubscriber, KernelEvent};
use crate::consolidation::{ConsolidationExecutor, ConsolidationTicker, Consolidator};
use crate::control::{ControlPlane, ControlPlaneConfig};
use crate::error::{KernelError, Result};
use crate::partition::{PartitionId, Partitions};
#[cfg(feature = "proc")]
use crate::proc::{Ledger, LedgerEventSink};
use crate::process::{ProcessHandle, ProcessTable, ProgressSource, Suspension};
use crate::sched::{QueueClass, QuietHours, Quotas};
use crate::stores::ProcessFilter;
use crate::stores::{
    InMemoryAgendaStore, InMemoryCronWatermarkStore, InMemoryProcessStore, ProcessStore,
};
use crate::supervise::{KernelHooks, Supervision};
use crate::wake::{
    CronWakeSource, FileWakeSource, KernelWakeExecutor, RunFinishedWakeSource, Trigger, Wake,
    WakeRouter, WakeSource,
};
use crate::watermark::{CronWatermarkStore, SourceWatermark};

/// How the kernel runs (spec 3.2 §7.2).
///
/// `proactivity`, `quiet_hours` and `quotas` apply to every partition (§7.5, §7.9); the
/// partition records of phase 3 carry their own.
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
/// // No control plane unless one is configured.
/// assert_eq!((config.control.socket, config.control.http), (None, None));
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct KernelConfig {
    /// The application runs belong to. Defaults to `adk-rust`.
    pub app_name: String,
    /// The kernel home: stores, socket and logs. Defaults to `dirs::config_dir()/adk-rust`.
    pub home: PathBuf,
    /// The queue classes, one worker each (spec 3.2 §7.9). Defaults to interactive,
    /// background and maintenance. Under a kernel, runs submitted without a queue — through
    /// `/api/runs` or `adk-rust serve`'s background state — land on `interactive`; there is
    /// no worker for the `default` queue.
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
    /// `RequestContextExtractor` (spec 3.2 §7.11). Defaults to none, neither a socket nor
    /// an HTTP bind, so a kernel built from the defaults never takes the socket of a kernel
    /// already serving the same home; `with_control(ControlPlaneConfig::default())` serves
    /// the socket at [`home`](Self::home), as `adk-rust up` does.
    pub control: ControlPlaneConfig,
    /// Refuse to start a driver where no sandbox enforcer exists (spec 3.2 §7.7). Defaults
    /// to `false`.
    pub fail_if_sandbox_unavailable: bool,
    /// The time the kernel reads: [`Wake::At`] times and wake-set expiry, the scheduler's
    /// due schedules, process ages and spend days. Defaults to `SystemClock`. Give the run
    /// store the same clock (`SqliteRunStore::with_clock`), since it decides leases and
    /// `not_before`.
    pub clock: Arc<dyn Clock>,
    /// The proactivity limits of every partition: open proposals, the daily proactive
    /// budget, quiet hours, how proposals are approved, the consolidation tick (spec 3.2
    /// §7.5). Defaults to [`ProactivityPolicy::default`].
    pub proactivity: ProactivityPolicy,
    /// A local-time window in which no proactive or maintenance process starts, beside the
    /// proactivity policy's (spec 3.2 §7.2, §7.9). Defaults to none.
    pub quiet_hours: Option<QuietHours>,
    /// Every partition's quotas: max running, max queued, the daily spend cap (spec 3.2
    /// §7.9). Defaults to none.
    pub quotas: Quotas,
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
            control: ControlPlaneConfig::default().without_socket(),
            fail_if_sandbox_unavailable: false,
            clock: Arc::new(SystemClock),
            proactivity: ProactivityPolicy::default(),
            quiet_hours: None,
            quotas: Quotas::default(),
        }
    }
}

impl KernelConfig {
    /// Sets the proactivity limits (spec 3.2 §7.5).
    #[must_use]
    pub fn with_proactivity(mut self, proactivity: ProactivityPolicy) -> Self {
        self.proactivity = proactivity;
        self
    }

    /// Sets the kernel's quiet hours (spec 3.2 §7.2, §7.9).
    #[must_use]
    pub fn with_quiet_hours(mut self, quiet_hours: QuietHours) -> Self {
        self.quiet_hours = Some(quiet_hours);
        self
    }

    /// Sets every partition's quotas (spec 3.2 §7.9).
    #[must_use]
    pub fn with_quotas(mut self, quotas: Quotas) -> Self {
        self.quotas = quotas;
        self
    }

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

    /// Sets the clock the kernel reads, such as a `ManualClock` a test advances.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
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

struct KernelInner {
    config: KernelConfig,
    sup: Arc<Supervision>,
    processes: ProcessTable,
    agenda: Agenda,
    partitions: Partitions,
    router: WakeRouter,
    schedules: Option<Arc<dyn ScheduleStore>>,
    approvals: Option<Arc<dyn ApprovalService>>,
    sessions: Option<Arc<dyn SessionService>>,
    agents: BTreeMap<String, Arc<Runner>>,
    /// The agents a person adds (spec 3.3 §5.7, §10 K7).
    agent_table: crate::agents::Agents,
    workflows: Option<Arc<dyn WorkflowExecutor>>,
    wake_sources: Vec<Arc<dyn WakeSource>>,
    queue_ingresses: Vec<QueueIngress>,
    consolidation: Option<ConsolidationTicker>,
    #[cfg(feature = "drivers")]
    drivers: Option<crate::drivers::DriverSupervisor>,
    runtime: tokio::sync::Mutex<Runtime>,
    status: Arc<RwLock<Status>>,
    #[cfg(feature = "proc")]
    ledger: Option<Ledger>,
}

/// What a started kernel runs besides its claim loops.
#[derive(Default)]
struct Runtime {
    stop: Option<CancellationToken>,
    tasks: Vec<JoinHandle<()>>,
    control: Option<ControlPlane>,
    /// Claim loops a pause shut down, finishing their attempts.
    stopping: Vec<JoinHandle<()>>,
}

/// The health the loops measure, read by [`Kernel::health`].
#[derive(Debug, Clone, Default)]
struct Status {
    state: KernelState,
    scheduler_lag: Option<Duration>,
    store_latency: Option<Duration>,
    active: HashMap<String, usize>,
}

impl fmt::Debug for Kernel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Kernel")
            .field("app_name", &self.inner.config.app_name)
            .field("home", &self.inner.config.home)
            .field("queues", &self.inner.config.queues)
            .field("state", &self.state())
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

    fn state(&self) -> KernelState {
        self.inner.status.read().unwrap_or_else(|e| e.into_inner()).state
    }

    fn set_state(&self, state: KernelState) {
        self.inner.status.write().unwrap_or_else(|e| e.into_inner()).state = state;
    }

    fn lifecycle_fields(&self, reason: Option<&str>) -> serde_json::Value {
        json!({
            "kernelId": self.inner.sup.kernel_id,
            "home": self.inner.config.home,
            "replicas": serde_json::Value::Null,
            "reason": reason,
        })
    }

    async fn record_lifecycle(&self, action: &str, reason: Option<&str>) {
        let sup = &self.inner.sup;
        sup.record(action, sup.actor_for(None), self.lifecycle_fields(reason)).await;
    }

    /// Starts a claim loop for every queue class.
    fn spawn_workers(&self) {
        let sup = &self.inner.sup;
        let mut handles = sup.handles.lock().unwrap_or_else(|e| e.into_inner());
        for (queue, worker) in sup.workers() {
            handles.push((queue.clone(), worker.clone().spawn()));
        }
    }

    /// Starts the workers, scheduler, wake sources and control plane, after reconciling
    /// the wakes missed while the kernel was down. Records `kernel.started`.
    ///
    /// Processes a drain parked, and those a freeze parked when the freeze has been lifted
    /// since, are woken first.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Lifecycle`] when the kernel already runs,
    /// [`KernelError::ControlPlane`] when the socket or bind is taken, or a store error.
    pub async fn start(&self) -> Result<()> {
        let mut runtime = self.inner.runtime.lock().await;
        let state = self.state();
        if !matches!(state, KernelState::Built | KernelState::Stopped) {
            return Err(KernelError::Lifecycle { state, action: "start" });
        }
        let sup = Arc::clone(&self.inner.sup);
        let config = &self.inner.config;
        let governance = &config.governance;
        if let Some(trail) = &sup.audit
            && governance.audit().is_none()
        {
            governance.attach_audit(trail.clone());
        }
        let stop = CancellationToken::new();
        let mut tasks = Vec::new();
        if let Some(mirror) = governance.spawn_mirror() {
            tasks.push(mirror);
        }
        sup.draining.store(false, Ordering::SeqCst);
        // Sleepers' cron occurrences missed while stopped are planned from the stored cursors.
        sup.crons.lock().unwrap_or_else(|e| e.into_inner()).clear();
        *sup.cron_started.lock().unwrap_or_else(|e| e.into_inner()) = None;
        for progress in sup.progress_sources() {
            progress.attach(ProcessHandle::new(&sup));
            #[cfg(feature = "proc")]
            if let Some(ledger) = &self.inner.ledger {
                progress.attach_ledger(Arc::new(ledger.clone()));
            }
        }
        for partition in sup.partition_progress.keys() {
            if matches!(sup.partitions.get(partition).await, Ok(None)) {
                tracing::warn!(
                    partition = %partition,
                    "a progress source is registered for a partition the partition table does not hold"
                );
            }
        }
        if !governance.is_frozen() {
            sup.wake_suspended(&Suspension::Freeze).await;
        }
        sup.wake_suspended(&Suspension::Drain).await;
        #[cfg(feature = "drivers")]
        if let Some(drivers) = &self.inner.drivers {
            drivers.attach(
                &sup.bus,
                sup.audit.as_ref(),
                &sup.kernel_id,
                &config.app_name,
                &config.home,
                config.fail_if_sandbox_unavailable,
            );
            if let Err(error) = drivers.start().await {
                tracing::warn!(error = %error, "could not load the mounted drivers");
            }
        }
        match self.inner.router.reconcile().await {
            Ok(outcomes) if !outcomes.is_empty() => {
                tracing::info!(
                    wakes = outcomes.len(),
                    "woke processes for changes missed while the kernel was down"
                );
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(error = %error, "could not reconcile missed wakes"),
        }
        // Every active added agent is built before the first claim (spec 3.3 §5.7).
        if let Err(error) = self.inner.agent_table.reconcile().await {
            tracing::warn!(error = %error, "could not host the added agents yet; the next tick retries");
        }
        self.spawn_workers();
        if let Some(schedules) = &self.inner.schedules {
            let scheduler = DurableScheduler::new(Arc::clone(schedules), Arc::clone(&sup.runs))
                .with_governance(governance.clone())
                .with_clock(Arc::clone(&config.clock));
            tasks.push(spawn_scheduler(
                scheduler,
                Arc::clone(&sup),
                config.scheduler_tick,
                Arc::clone(&self.inner.status),
                stop.clone(),
            ));
        }
        for source in &self.inner.wake_sources {
            tasks.push(spawn_wake_source(
                Arc::clone(source),
                Arc::clone(&sup),
                self.inner.router.clone(),
                stop.clone(),
            ));
        }
        for ingress in &self.inner.queue_ingresses {
            tasks.push(spawn_queue_ingress(ingress.clone(), Arc::clone(&sup), stop.clone()));
        }
        if let Some(approvals) = &self.inner.approvals {
            tasks.push(adk_server::approvals::spawn_approval_ticker(
                Arc::clone(approvals),
                config.scheduler_tick,
            ));
        }
        tasks.push(spawn_mirror(Arc::clone(&sup), stop.clone()));
        tasks.push(spawn_file_watch(
            Arc::clone(&sup),
            self.inner.router.clone(),
            config.scheduler_tick,
            stop.clone(),
        ));
        tasks.push(spawn_maintenance(
            Arc::clone(&sup),
            self.inner.router.clone(),
            Ticks {
                agenda: self.inner.agenda.clone(),
                agents: self.inner.agent_table.clone(),
                consolidation: self.inner.consolidation.clone(),
            },
            Arc::clone(&self.inner.status),
            config.scheduler_tick,
            stop.clone(),
        ));
        if config.control.socket.is_some() || config.control.http.is_some() {
            match ControlPlane::start(self).await {
                Ok(plane) => runtime.control = Some(plane),
                Err(error) => {
                    stop.cancel();
                    for task in tasks {
                        task.abort();
                    }
                    let handles: Vec<_> =
                        sup.handles.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
                    for (_, handle) in handles {
                        handle.shutdown().await;
                    }
                    return Err(error);
                }
            }
        }
        runtime.stop = Some(stop);
        runtime.tasks = tasks;
        drop(runtime);
        self.set_state(KernelState::Running);
        self.record_lifecycle(audit::KERNEL_STARTED, None).await;
        sup.publish(KernelEvent::Health(self.health()));
        Ok(())
    }

    /// Stops claiming; running processes run their attempt to its end. Records
    /// `kernel.paused`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Lifecycle`] when the kernel is not running.
    pub async fn pause(&self) -> Result<()> {
        let mut runtime = self.inner.runtime.lock().await;
        let state = self.state();
        if state != KernelState::Running {
            return Err(KernelError::Lifecycle { state, action: "pause" });
        }
        let handles: Vec<_> =
            self.inner.sup.handles.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        for (_, handle) in handles {
            runtime.stopping.push(tokio::spawn(handle.shutdown()));
        }
        drop(runtime);
        self.set_state(KernelState::Paused);
        self.record_lifecycle(audit::KERNEL_PAUSED, None).await;
        Ok(())
    }

    /// Claims again after [`pause`](Self::pause) or [`drain`](Self::drain), waking the
    /// processes a drain parked. Records `kernel.resumed`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Lifecycle`] when the kernel is not paused.
    pub async fn resume(&self) -> Result<()> {
        let _runtime = self.inner.runtime.lock().await;
        let state = self.state();
        if state != KernelState::Paused {
            return Err(KernelError::Lifecycle { state, action: "resume" });
        }
        self.spawn_workers();
        self.inner.sup.wake_suspended(&Suspension::Drain).await;
        self.set_state(KernelState::Running);
        self.record_lifecycle(audit::KERNEL_RESUMED, None).await;
        Ok(())
    }

    /// Suspends running processes at their next checkpoint through S1's suspend path
    /// ([`Suspension::Drain`]), then stops claiming; on
    /// `timeout` aborts the claim loops so their leases lapse. Records `kernel.drained`.
    /// The kernel is paused afterwards: [`resume`](Self::resume) or a later
    /// [`start`](Self::start) wakes what the drain parked.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Lifecycle`] when the kernel is not running or paused, or a
    /// store error.
    pub async fn drain(&self, timeout: Duration) -> Result<()> {
        let mut runtime = self.inner.runtime.lock().await;
        let state = self.state();
        if !matches!(state, KernelState::Running | KernelState::Paused) {
            return Err(KernelError::Lifecycle { state, action: "drain" });
        }
        self.set_state(KernelState::Draining);
        let sup = &self.inner.sup;
        sup.draining.store(true, Ordering::SeqCst);
        let ours = sup.local_worker_ids();
        let deadline = tokio::time::Instant::now() + timeout;
        let mut busy;
        loop {
            busy = 0;
            for run in sup.running().await {
                if run.lease_owner.as_ref().is_some_and(|owner| ours.contains(owner)) {
                    busy += 1;
                    sup.suspend_local(&run.run_id);
                }
            }
            if busy == 0 || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(DRAIN_POLL).await;
        }
        let handles: Vec<_> =
            sup.handles.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        if busy == 0 {
            for (_, handle) in handles {
                handle.shutdown().await;
            }
        } else {
            tracing::warn!(
                runs = busy,
                "drain timed out; dropping the attempts still running so their leases lapse"
            );
            for (_, handle) in &handles {
                handle.abort();
            }
        }
        for stopping in runtime.stopping.drain(..) {
            if busy == 0 {
                let _ = stopping.await;
            } else {
                stopping.abort();
            }
        }
        drop(runtime);
        sup.draining.store(false, Ordering::SeqCst);
        self.set_state(KernelState::Paused);
        let reason = (busy > 0).then_some("timeout");
        self.record_lifecycle(audit::KERNEL_DRAINED, reason).await;
        Ok(())
    }

    /// Drains, stops drivers and closes the control plane. Records `kernel.stopped`.
    ///
    /// # Errors
    ///
    /// Returns a store or control-plane error; the kernel is stopped regardless.
    pub async fn stop(&self) -> Result<()> {
        let mut first_error = None;
        if matches!(self.state(), KernelState::Running | KernelState::Paused)
            && let Err(error) = self.drain(Self::SIGTERM_DRAIN).await
        {
            first_error = Some(error);
        }
        let mut runtime = self.inner.runtime.lock().await;
        if let Some(stop) = runtime.stop.take() {
            stop.cancel();
        }
        for task in runtime.tasks.drain(..) {
            task.abort();
        }
        for task in runtime.stopping.drain(..) {
            task.abort();
        }
        let watched: Vec<_> =
            std::mem::take(&mut *self.inner.sup.watched.lock().unwrap_or_else(|e| e.into_inner()))
                .into_values()
                .collect();
        for task in watched {
            task.abort();
        }
        let handles: Vec<_> =
            self.inner.sup.handles.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        for (_, handle) in &handles {
            handle.abort();
        }
        if let Some(plane) = runtime.control.take()
            && let Err(error) = plane.shutdown().await
        {
            first_error.get_or_insert(error);
        }
        #[cfg(feature = "drivers")]
        if let Some(drivers) = &self.inner.drivers {
            drivers.shutdown().await;
        }
        // The agents a person added close with the kernel (spec 3.3 §5.7).
        self.inner.agent_table.close_hosted().await;
        drop(runtime);
        let was = self.state();
        self.set_state(KernelState::Stopped);
        if was != KernelState::Stopped && was != KernelState::Built {
            self.record_lifecycle(audit::KERNEL_STOPPED, None).await;
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Waits for `SIGTERM` or Ctrl-C — or, in a Windows service, the service manager's stop
    /// (spec 3.2 §10 W3) — then [`drain`](Self::drain)s for
    /// [`SIGTERM_DRAIN`](Self::SIGTERM_DRAIN) and [`stop`](Self::stop)s.
    ///
    /// # Errors
    ///
    /// Returns the error of `drain` or `stop`, or [`KernelError::Io`] when the signal
    /// handler cannot be installed.
    pub async fn shutdown_on_signal(&self) -> Result<()> {
        #[cfg(unix)]
        {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                result = tokio::signal::ctrl_c() => result?,
                _ = terminate.recv() => {}
            }
        }
        #[cfg(all(not(unix), feature = "service"))]
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            () = crate::service::stop_requested() => {}
        }
        #[cfg(all(not(unix), not(feature = "service")))]
        tokio::signal::ctrl_c().await?;
        tracing::info!("shutdown signal received; draining the kernel");
        if matches!(self.state(), KernelState::Running | KernelState::Paused) {
            self.drain(Self::SIGTERM_DRAIN).await?;
        }
        self.stop().await
    }

    /// Freezes execution as `by`: `GovernanceControl::freeze_recorded`, then the running
    /// processes park `Waiting` at their next checkpoint with
    /// [`Suspension::Freeze`], one `kernel.process.suspended`
    /// record each, on every replica within one second (K-I8). No process starts until
    /// [`unfreeze`](Self::unfreeze).
    ///
    /// # Errors
    ///
    /// Returns the control's error when the freeze cannot be recorded; this replica is
    /// frozen regardless.
    pub async fn freeze(&self, reason: &str, by: AuditActor) -> Result<()> {
        self.inner.config.governance.freeze_recorded(reason, by).await?;
        Ok(())
    }

    /// Lifts the freeze as `by`: `GovernanceControl::unfreeze_recorded`, then wakes exactly
    /// the processes the freeze parked, never a manual or preempted suspension (K-I21).
    ///
    /// # Errors
    ///
    /// Returns the control's error when the unfreeze cannot be recorded; execution stays
    /// frozen.
    pub async fn unfreeze(&self, by: AuditActor) -> Result<()> {
        self.inner.config.governance.unfreeze_recorded(by).await?;
        let sup = &self.inner.sup;
        for (_, worker) in sup.workers() {
            worker.wake_frozen().await;
        }
        sup.wake_suspended(&Suspension::Freeze).await;
        sup.nudge();
        Ok(())
    }

    /// The process table.
    pub fn processes(&self) -> &ProcessTable {
        &self.inner.processes
    }

    /// The agenda (spec 3.2 §7.5).
    pub fn agenda(&self) -> &Agenda {
        &self.inner.agenda
    }

    /// The partition table and its crossings (spec 3.2 §7.8).
    pub fn partitions(&self) -> &Partitions {
        &self.inner.partitions
    }

    /// The partition the target `target` is bound to, when it is a partition agent's.
    pub fn partition_of_target(&self, target: &str) -> Option<&PartitionId> {
        self.inner.sup.targets.statics().get(target)
    }

    /// The agents a person adds, by asking or by hand (spec 3.3 §5.7, §10 K7): every
    /// change to one goes through this table.
    pub fn agents(&self) -> &crate::agents::Agents {
        &self.inner.agent_table
    }

    /// The event bus.
    pub fn bus(&self) -> &EventBus {
        &self.inner.sup.bus
    }

    /// The wake matcher.
    pub fn wake_router(&self) -> &WakeRouter {
        &self.inner.router
    }

    /// The address the control plane's HTTP listener is bound to while the kernel runs, so
    /// a bind on port 0 is discoverable (spec 3.2 §7.11, spec 3.5 §8.2); `None` when the
    /// control plane serves no HTTP or the kernel is not running.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let control = ControlPlaneConfig::default().without_socket().with_http("127.0.0.1:0".parse()?);
    /// kernel.start().await?;
    /// let addr = kernel.control_addr().await.expect("the control plane listens");
    /// ```
    pub async fn control_addr(&self) -> Option<std::net::SocketAddr> {
        self.inner.runtime.lock().await.control.as_ref().and_then(ControlPlane::http_addr)
    }

    /// The content-free activity ledger the kernel writes, when it was built with one
    /// (spec 3.2 §7.10, feature `proc`).
    #[cfg(feature = "proc")]
    pub fn ledger(&self) -> Option<&Ledger> {
        self.inner.ledger.as_ref()
    }

    /// What every kernel part shares, for the routes the surfaces need (spec 3.3 §10 K3,
    /// K4).
    pub(crate) fn supervision(&self) -> &Arc<Supervision> {
        &self.inner.sup
    }

    /// The end of the quiet time that holds `at` in `partition`, followed through windows
    /// that touch; `None` when `at` is not quiet (spec 3.3 §5.4, §10 K3). It reads the
    /// windows in force now: the kernel's, the proactivity policy's and the partition's own,
    /// with a quiet time the person changed while the kernel runs in place of the kernel's and
    /// the partition's. A phone's wake-up waits for it.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example(kernel: adk_kernel::Kernel) {
    /// use adk_kernel::PartitionId;
    ///
    /// if let Some(until) = kernel.quiet_until(&PartitionId::default(), chrono::Utc::now()).await {
    ///     println!("quiet until {until}");
    /// }
    /// # }
    /// ```
    pub async fn quiet_until(
        &self,
        partition: &PartitionId,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let windows = self.inner.sup.quiet_windows(partition).await;
        let mut until = at;
        // Two windows release into each other at most once each.
        for _ in 0..=windows.len() {
            match windows
                .iter()
                .filter(|quiet| quiet.contains(until))
                .map(|quiet| quiet.release_at(until))
                .max()
            {
                Some(next) if next > until => until = next,
                _ => break,
            }
        }
        (until > at).then_some(until)
    }

    /// Subscribes to the event bus from now on.
    pub fn events(&self) -> EventSubscriber {
        self.inner.sup.bus.subscribe()
    }

    /// The kernel's health: per worker, scheduler lag, store latency.
    pub fn health(&self) -> KernelHealth {
        let status = self.inner.status.read().unwrap_or_else(|e| e.into_inner()).clone();
        let sup = &self.inner.sup;
        let claiming: Vec<QueueClass> = sup
            .handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, handle)| !handle.is_finished())
            .map(|(queue, _)| queue.clone())
            .collect();
        let workers = sup
            .workers()
            .iter()
            .map(|(queue, worker)| {
                let worker_id = worker.config().worker_id.clone();
                let mut health = WorkerHealth::new(queue.clone(), worker_id.clone());
                health.claiming = claiming.contains(queue);
                health.active_runs = status.active.get(&worker_id).copied().unwrap_or(0);
                health
            })
            .collect();
        KernelHealth {
            state: status.state,
            frozen: self.inner.config.governance.is_frozen(),
            workers,
            scheduler_lag: status.scheduler_lag,
            store_latency: status.store_latency,
        }
    }
}

impl Kernel {
    /// The session service `attach` reads transcripts and holds from (spec 3.2 §7.11).
    pub(crate) fn session_service(&self) -> Option<&Arc<dyn SessionService>> {
        self.inner.sessions.as_ref()
    }

    /// The approval service the socket serves the `/api/approvals` inbox from (K5).
    pub(crate) fn approval_service(&self) -> Option<&Arc<dyn ApprovalService>> {
        self.inner.approvals.as_ref()
    }

    /// The audit trail `kernel.control` records go to (K-I16, spec 3.2 §9.3).
    pub(crate) fn audit_trail(&self) -> Option<&AuditTrail> {
        self.inner.sup.audit.as_ref()
    }

    /// The registered agent target names, sorted; workflow ids cannot be listed
    /// (spec 3.2 §7.11, `POST /api/kernel/processes`).
    pub(crate) fn targets(&self) -> Vec<String> {
        let mut targets: std::collections::BTreeSet<String> =
            self.inner.agents.keys().cloned().collect();
        targets.extend(self.inner.sup.targets.table.targets());
        targets.into_iter().collect()
    }

    /// The driver supervisor, when the kernel was built with one (spec 3.2 §7.7).
    #[cfg(feature = "drivers")]
    pub fn drivers(&self) -> Option<&crate::drivers::DriverSupervisor> {
        self.inner.drivers.as_ref()
    }

    /// Whether `target` is a registered agent target or a workflow the executor has.
    pub(crate) fn has_target(&self, target: &str) -> bool {
        let table = &self.inner.sup.targets.table;
        self.inner.agents.contains_key(target)
            || table.contains(target)
            || table.is_expected(target)
            || self.inner.workflows.as_ref().is_some_and(|workflows| workflows.has_workflow(target))
    }
}

#[async_trait]
impl FreezeDelegate for Kernel {
    async fn freeze(&self, reason: &str, by: AuditActor) -> adk_core::Result<FreezeReport> {
        // The running processes park at their next model-call boundary.
        let parked_runs = self.inner.sup.running().await.len();
        Kernel::freeze(self, reason, by).await?;
        Ok(FreezeReport { parked_runs, cancelled_runs: 0 })
    }

    async fn unfreeze(&self, by: AuditActor) -> adk_core::Result<()> {
        Kernel::unfreeze(self, by).await.map_err(Into::into)
    }
}

/// How long a queue ingress waits for a message before it pulls again.
const INGRESS_IDLE: Duration = Duration::from_secs(1);
/// How often a drain checks whether the suspended processes have parked.
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// Ticks the scheduler every `tick`, recording how late each tick ran.
fn spawn_scheduler(
    scheduler: DurableScheduler,
    sup: Arc<Supervision>,
    tick: Duration,
    status: Arc<RwLock<Status>>,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut due = tokio::time::Instant::now();
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep_until(due) => {}
            }
            let lag = tokio::time::Instant::now().saturating_duration_since(due);
            match scheduler.tick().await {
                // A fired schedule runs now, not at its queue's next poll.
                Ok(fired) if fired.enqueued > 0 => sup.nudge(),
                Ok(_) => {}
                Err(error) => tracing::warn!(error = %error, "scheduler tick failed"),
            }
            status.write().unwrap_or_else(|e| e.into_inner()).scheduler_lag = Some(lag);
            due = (due + tick).max(tokio::time::Instant::now());
        }
    })
}

/// Runs `ingress` as `QueueIngress::run` does, waking the workers when a message became a
/// `kernel.wake` run, so a queue wake is claimed at once rather than at the maintenance
/// queue's next poll.
fn spawn_queue_ingress(
    ingress: QueueIngress,
    sup: Arc<Supervision>,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let pause = match ingress.poll_once().await {
                Ok(report) => {
                    if !report.created.is_empty() {
                        sup.nudge();
                    }
                    if report.pulled > 0 { Duration::ZERO } else { INGRESS_IDLE }
                }
                Err(error) => {
                    tracing::warn!(error = %error, "queue ingress pull failed");
                    INGRESS_IDLE
                }
            };
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(pause) => {}
            }
        }
    })
}

/// Hands every event of `source` to [`WakeRouter::submit`], which enqueues a `kernel.wake`
/// run keyed by the event, until the kernel stops (spec 3.2 §7.4). The maintenance worker's
/// [`KernelWakeExecutor`] routes it, so a delivery is routed once however often it arrives
/// (K-I3) and survives a restart.
fn spawn_wake_source(
    source: Arc<dyn WakeSource>,
    sup: Arc<Supervision>,
    router: WakeRouter,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = match source.subscribe().await {
            Ok(events) => events,
            Err(error) => {
                tracing::warn!(wake.source = source.name(), error = %error, "could not subscribe to a wake source");
                return;
            }
        };
        loop {
            let event = tokio::select! {
                () = stop.cancelled() => return,
                event = events.next() => event,
            };
            let Some(event) = event else { return };
            let key = event.key.clone();
            match router.submit(&event, &sup.app_name).await {
                Ok(Enqueued::Created(_)) => sup.nudge(),
                Ok(_) => {
                    tracing::debug!(wake.source = source.name(), wake.key = %key, "duplicate wake delivery");
                }
                Err(error) => {
                    tracing::warn!(wake.source = source.name(), wake.key = %key, error = %error, "could not hand a wake to the maintenance queue");
                }
            }
        }
    })
}

/// Keeps one file source per root a sleeper or trigger watches (spec 3.2 §7.4): a spawn
/// that names a new root signals at once, and every tick drops the roots no sleeper needs
/// any more. A source whose subscription failed is not retried until its root is dropped
/// and needed again.
fn spawn_file_watch(
    sup: Arc<Supervision>,
    router: WakeRouter,
    tick: Duration,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let roots = file_roots(&sup, &router).await;
            let mut started = Vec::new();
            {
                let mut watched = sup.watched.lock().unwrap_or_else(|e| e.into_inner());
                watched.retain(|root, task| {
                    let needed = roots.contains_key(root);
                    if !needed {
                        task.abort();
                        tracing::debug!(root = %root.display(), "no sleeper watches this root any more");
                    }
                    needed
                });
                for (root, debounce) in &roots {
                    if !watched.contains_key(root) {
                        started.push((root.clone(), *debounce));
                    }
                }
            }
            for (root, debounce) in started {
                let name = format!("file:{}", root.display());
                match FileWakeSource::new(name, root.clone(), "*") {
                    Ok(source) => {
                        let source: Arc<dyn WakeSource> = Arc::new(source.with_debounce(debounce));
                        let task = spawn_wake_source(
                            source,
                            Arc::clone(&sup),
                            router.clone(),
                            stop.clone(),
                        );
                        sup.watched.lock().unwrap_or_else(|e| e.into_inner()).insert(root, task);
                    }
                    Err(error) => {
                        tracing::warn!(root = %root.display(), error = %error, "could not watch a file wake's root");
                    }
                }
            }
            tokio::select! {
                () = stop.cancelled() => return,
                () = sup.roots_changed.notified() => {}
                () = tokio::time::sleep(tick) => {}
            }
        }
    })
}

/// The roots of the file wakes of every sleeper and trigger, each with the shortest
/// debounce asked for it.
async fn file_roots(sup: &Supervision, router: &WakeRouter) -> BTreeMap<PathBuf, Duration> {
    let mut roots: BTreeMap<PathBuf, Duration> = BTreeMap::new();
    let mut add = |wake: &Wake| {
        if let Wake::FileChange { root, debounce, .. } = wake {
            let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            let entry = roots.entry(root).or_insert(*debounce);
            *entry = (*entry).min(*debounce);
        }
    };
    for trigger in router.triggers() {
        add(&trigger.wake);
    }
    match sup.processes.list(&ProcessFilter::default()).await {
        Ok(metas) => {
            for wake in metas.iter().filter_map(|meta| meta.wake.as_ref()) {
                wake.any_of.iter().for_each(&mut add);
            }
        }
        Err(error) => tracing::debug!(error = %error, "could not list the sleepers' file wakes"),
    }
    roots
}

/// Every 250 ms (K-I8): suspends the running processes this replica leases that have a
/// pending request, and wakes the freeze-parked processes once the control allows work
/// again, publishing each change of the kill switch.
fn spawn_mirror(sup: Arc<Supervision>, stop: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut frozen = sup.governance.is_frozen();
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(KernelConfig::GOVERNANCE_POLL) => {}
            }
            sup.mirror_suspend_requests().await;
            let now = sup.governance.is_frozen();
            if now != frozen {
                frozen = now;
                sup.publish(KernelEvent::Governance { frozen, reason: sup.governance.reason() });
                if !frozen {
                    for (_, worker) in sup.workers() {
                        worker.wake_frozen().await;
                    }
                    sup.wake_suspended(&Suspension::Freeze).await;
                }
            }
        }
    })
}

/// What the maintenance tick moves forward besides the supervision state: the agenda, the
/// agent table (spec 3.3 §5.7) and the consolidation tick.
struct Ticks {
    agenda: Agenda,
    agents: crate::agents::Agents,
    consolidation: Option<ConsolidationTicker>,
}

/// Every scheduler tick: expires wake sets and fires time wakes, claims wake runs enqueued
/// from outside the kernel, attributes spend, applies the agenda's decisions, makes the
/// daily review, runs the admission sweep (preempted processes whose preemptor finished,
/// processes waiting for a slot, budgets replenished), and measures the store.
fn spawn_maintenance(
    sup: Arc<Supervision>,
    router: WakeRouter,
    ticks: Ticks,
    status: Arc<RwLock<Status>>,
    tick: Duration,
    stop: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(tick) => {}
            }
            sup.fire_crons(&router).await;
            match router.sweep_expired(sup.clock.now()).await {
                Ok(outcomes) if !outcomes.is_empty() => sup.nudge(),
                Ok(_) => {}
                Err(error) => tracing::debug!(error = %error, "could not sweep expired wake sets"),
            }
            // A webhook ingress served outside the kernel, or another replica, enqueues
            // `kernel.wake` runs this kernel never sees enqueued: claim them within a tick
            // rather than at the maintenance queue's 5 s poll (§7.4, under 2 s).
            if sup.external_wakes_due().await {
                sup.nudge();
            }
            // The day that ended gets its baseline pass, once (spec 3.2 §7.10).
            #[cfg(feature = "proc-baselines")]
            if let Some(proc) = &sup.proc
                && proc.tick(&sup.runs, &sup.queues, sup.clock.now()).await
            {
                sup.nudge();
            }
            sup.attribute_all().await;
            ticks.agenda.tick().await;
            ticks.agents.tick().await;
            sup.review().await;
            if let Some(consolidation) = &ticks.consolidation {
                consolidation.tick().await;
            }
            // A preemptor that finished on another replica, a slot freed there, a new day.
            sup.requeue_preempted(None).await;
            if sup.caps_running().await {
                sup.admit_waiters().await;
            }
            let today = sup.clock.now().date_naive();
            for partition in sup.replenished_partitions().await {
                let event = crate::wake::WakeEvent::new(
                    "budget",
                    crate::wake::keys::budget(partition.as_str(), today),
                    json!({ "partition": partition, "day": today }),
                );
                if let Err(error) = router.submit(&event, &sup.app_name).await {
                    tracing::debug!(partition = %partition, error = %error, "could not submit a budget wake");
                }
            }
            let probe = tokio::time::Instant::now();
            let running = sup.running().await;
            let latency = probe.elapsed();
            let mut active: HashMap<String, usize> = HashMap::new();
            for run in running {
                if let Some(owner) = run.lease_owner {
                    *active.entry(owner).or_default() += 1;
                }
            }
            let mut status = status.write().unwrap_or_else(|e| e.into_inner());
            status.store_latency = Some(latency);
            status.active = active;
        }
    })
}

/// The workflows every kernel worker runs: the kernel's `kernel.wake` and the caller's.
struct KernelWorkflows {
    user: Option<Arc<dyn WorkflowExecutor>>,
    wake: KernelWakeExecutor,
    /// The daily baseline pass, `kernel.proc` (spec 3.2 §7.10).
    #[cfg(feature = "proc-baselines")]
    proc: Option<Arc<crate::proc::baselines::ProcRuntime>>,
}

impl KernelWorkflows {
    /// The baseline pass when `workflow_id` names it.
    #[cfg(feature = "proc-baselines")]
    fn proc_for(&self, workflow_id: &str) -> Option<&Arc<crate::proc::baselines::ProcRuntime>> {
        self.proc.as_ref().filter(|_| workflow_id == crate::proc::baselines::PROC_TARGET)
    }
}

#[async_trait]
impl WorkflowExecutor for KernelWorkflows {
    fn has_workflow(&self, workflow_id: &str) -> bool {
        #[cfg(feature = "proc-baselines")]
        if self.proc_for(workflow_id).is_some() {
            return true;
        }
        self.wake.has_workflow(workflow_id)
            || self.user.as_ref().is_some_and(|user| user.has_workflow(workflow_id))
    }

    async fn execute(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        cancel_token: CancellationToken,
    ) -> std::result::Result<serde_json::Value, String> {
        #[cfg(feature = "proc-baselines")]
        if let Some(proc) = self.proc_for(workflow_id) {
            let input = serde_json::Value::Object(input.into_iter().collect());
            return tokio::select! {
                done = proc.execute(input) => done,
                () = cancel_token.cancelled() => Err("the baseline pass was cancelled".to_string()),
            };
        }
        match &self.user {
            Some(user) if !self.wake.has_workflow(workflow_id) => {
                user.execute(workflow_id, input, cancel_token).await
            }
            _ => self.wake.execute(workflow_id, input, cancel_token).await,
        }
    }

    async fn execute_durable(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        run: WorkflowRunContext,
    ) -> std::result::Result<serde_json::Value, String> {
        #[cfg(feature = "proc-baselines")]
        if self.proc_for(workflow_id).is_some() {
            return self.execute(workflow_id, input, run.cancel).await;
        }
        match &self.user {
            Some(user) if !self.wake.has_workflow(workflow_id) => {
                user.execute_durable(workflow_id, input, run).await
            }
            _ => self.wake.execute_durable(workflow_id, input, run).await,
        }
    }
}

/// Emits a finished run's model calls, tool calls and tool durations as the OTel GenAI
/// histograms (spec 3.2 §7.10, §13 Q4).
#[cfg(feature = "proc")]
pub(crate) fn record_histograms(target: &str, counts: &crate::supervise::TranscriptCounts) {
    use opentelemetry::KeyValue;
    use std::sync::OnceLock;

    struct Instruments {
        inference_calls: opentelemetry::metrics::Histogram<u64>,
        tool_calls: opentelemetry::metrics::Histogram<u64>,
        tool_duration: opentelemetry::metrics::Histogram<f64>,
    }
    static INSTRUMENTS: OnceLock<Instruments> = OnceLock::new();
    let instruments = INSTRUMENTS.get_or_init(|| {
        let meter = opentelemetry::global::meter("adk-kernel");
        Instruments {
            inference_calls: meter
                .u64_histogram("gen_ai.invoke_agent.inference_calls")
                .with_description("Model calls per agent run")
                .build(),
            tool_calls: meter
                .u64_histogram("gen_ai.invoke_agent.tool_calls")
                .with_description("Tool calls per agent run")
                .build(),
            tool_duration: meter
                .f64_histogram("gen_ai.execute_tool.duration")
                .with_unit("s")
                .with_description("Tool call duration")
                .build(),
        }
    });
    let invoke = [
        KeyValue::new(adk_telemetry::semconv::GEN_AI_OPERATION_NAME, "invoke_agent"),
        KeyValue::new("gen_ai.agent.name", target.to_string()),
    ];
    instruments.inference_calls.record(counts.model_calls, &invoke);
    instruments.tool_calls.record(counts.tool_calls, &invoke);
    let execute = [KeyValue::new(adk_telemetry::semconv::GEN_AI_OPERATION_NAME, "execute_tool")];
    for seconds in &counts.tool_durations {
        instruments.tool_duration.record(*seconds, &execute);
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
    governance: Option<GovernanceControl>,
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
    partition_progress: Vec<(PartitionId, Arc<dyn ProgressSource>)>,
    queue_ingresses: Vec<QueueIngress>,
    agenda: Option<Arc<dyn AgendaStore>>,
    agenda_handles: Vec<AgendaHandle>,
    agent_store: Option<Arc<dyn crate::agents::AgentStore>>,
    agent_factory: Option<Arc<dyn crate::agents::AgentFactory>>,
    agents_handles: Vec<crate::agents::AgentsHandle>,
    rule_source: Option<Arc<dyn crate::review::StandingRuleSource>>,
    consolidators: BTreeMap<PartitionId, Arc<dyn Consolidator>>,
    #[cfg(feature = "drivers")]
    drivers: Option<crate::drivers::DriverSupervisor>,
    audiences: BTreeMap<PartitionId, Arc<dyn ObservationAudience>>,
    #[cfg(feature = "proc")]
    ledger: Option<Ledger>,
    #[cfg(feature = "proc-baselines")]
    baselines: Option<crate::proc::baselines::BaselinePolicy>,
    #[cfg(feature = "proc-baselines")]
    phraser: Option<Arc<dyn crate::proc::baselines::ObservationPhraser>>,
    partitions: Option<Partitions>,
    partition_targets: BTreeMap<String, PartitionId>,
    watermarks: Option<Arc<dyn CronWatermarkStore>>,
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

    /// Sets the kill switch. It replaces [`KernelConfig::governance`] at
    /// [`build`](Self::build), whenever [`config`](Self::config) is called, so a builder
    /// from `Harness::kernel()` keeps the harness's control when the caller sets its own
    /// configuration.
    #[must_use]
    pub fn governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
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
    /// decision wakes the parked run. The kernel cannot add a notifier to a built service:
    /// for [`Wake::Approval`] sleepers, build the service with
    /// `with_notifier(source)` over an [`ApprovalWakeSource`](crate::ApprovalWakeSource) and
    /// pass the same source to [`wake_source`](Self::wake_source).
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

    /// Adds an event sink, such as an outbound `WebhookSink` (feature `webhooks`), that
    /// receives every later event through its own bounded queue (spec 3.2 §7.6).
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

    /// Sets the progress source of partition `partition` (spec 3.2 §7.8, §7.10): the
    /// kernel asks it, not the [`progress_source`](Self::progress_source), for the goal
    /// progress, loop-guard verdict and usage of that partition's processes — routed by the
    /// process's own partition — and hands it the process table and the ledger at start, as
    /// it does the main source. A partition without a source of its own falls back to the
    /// main source; its processes keep their partition.
    ///
    /// [`build`](Self::build) refuses a second source for the same partition, and
    /// [`Kernel::start`] warns about a partition the partition table does not hold.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_core::InMemoryRunStore;
    /// use adk_kernel::{Kernel, ProgressSource};
    ///
    /// struct Work;
    /// impl ProgressSource for Work {}
    ///
    /// let builder = || Kernel::builder().run_store(Arc::new(InMemoryRunStore::new()));
    /// assert!(builder().partition_progress_source("work", Arc::new(Work)).build().is_ok());
    /// let twice = builder()
    ///     .partition_progress_source("work", Arc::new(Work))
    ///     .partition_progress_source("work", Arc::new(Work))
    ///     .build();
    /// assert!(twice.is_err());
    /// ```
    #[must_use]
    pub fn partition_progress_source(
        mut self,
        partition: impl Into<PartitionId>,
        source: Arc<dyn ProgressSource>,
    ) -> Self {
        self.partition_progress.push((partition.into(), source));
        self
    }

    /// Adds a queue ingress the kernel runs while started, such as
    /// [`queue_wake_ingress`](crate::queue_wake_ingress).
    #[must_use]
    pub fn queue_ingress(mut self, ingress: QueueIngress) -> Self {
        self.queue_ingresses.push(ingress);
        self
    }

    /// Sets the agenda's store (spec 3.2 §7.5). Defaults to [`InMemoryAgendaStore`].
    #[must_use]
    pub fn agenda_store(mut self, store: Arc<dyn AgendaStore>) -> Self {
        self.agenda = Some(store);
        self
    }

    /// Binds `handle` to the built kernel's agenda, so a tool created before the kernel —
    /// the harness's `propose_followup` — reaches it (spec 3.2 §7.5, §10 HK1).
    #[must_use]
    pub fn agenda_handle(mut self, handle: AgendaHandle) -> Self {
        self.agenda_handles.push(handle);
        self
    }

    /// Sets where the agents a person adds are kept (spec 3.3 §5.7, §10 K7). Defaults to
    /// [`InMemoryAgentStore`](crate::stores::InMemoryAgentStore), which keeps nothing across
    /// a restart.
    #[must_use]
    pub fn agent_store(mut self, store: Arc<dyn crate::agents::AgentStore>) -> Self {
        self.agent_store = Some(store);
        self
    }

    /// Sets how the agents a person adds are built (spec 3.3 §5.7, §10 K7). Without one the
    /// kernel lists and removes agents but adds none.
    #[must_use]
    pub fn agent_factory(mut self, factory: Arc<dyn crate::agents::AgentFactory>) -> Self {
        self.agent_factory = Some(factory);
        self
    }

    /// Binds `handle` to the built kernel's agent table, so a tool created before the kernel
    /// — the harness's `propose_agent` — reaches it (spec 3.3 §10 K7), as
    /// [`agenda_handle`](Self::agenda_handle) does for `propose_followup`.
    #[must_use]
    pub fn agents_handle(mut self, handle: crate::agents::AgentsHandle) -> Self {
        self.agents_handles.push(handle);
        self
    }

    /// Sets where the daily review reads standing rules (spec 3.2 §9.1); `Harness::kernel`
    /// sets the harness's rule store.
    #[must_use]
    pub fn standing_rules(mut self, source: Arc<dyn crate::review::StandingRuleSource>) -> Self {
        self.rule_source = Some(source);
        self
    }

    /// Registers `consolidator` for `partition`: the kernel runs its consolidation tick
    /// there when [`ProactivityPolicy::consolidation`] is set (spec 3.2 §7.5, §10 HK2).
    #[must_use]
    pub fn consolidator(
        mut self,
        partition: impl Into<PartitionId>,
        consolidator: Arc<dyn Consolidator>,
    ) -> Self {
        self.consolidators.insert(partition.into(), consolidator);
        self
    }

    /// Supervises drivers (spec 3.2 §7.7): `Kernel::start` loads the mounted drivers and
    /// starts the eager ones, `Kernel::stop` stops them, and their state changes reach the
    /// kernel's bus and audit trail.
    #[cfg(feature = "drivers")]
    #[must_use]
    pub fn drivers(mut self, drivers: crate::drivers::DriverSupervisor) -> Self {
        self.drivers = Some(drivers);
        self
    }

    /// Registers `audience` for `partition` (spec 3.2 §7.10): the daily baseline pass puts
    /// the partition's observations to the person it names as
    /// [`AgendaOrigin::Observation`](crate::AgendaOrigin::Observation) questions, and a
    /// confirmation is remembered through it. Without one, observations stay at
    /// `/api/kernel/proc/observations` and nobody is asked.
    #[must_use]
    pub fn observation_audience(
        mut self,
        partition: impl Into<PartitionId>,
        audience: Arc<dyn ObservationAudience>,
    ) -> Self {
        self.audiences.insert(partition.into(), audience);
        self
    }

    /// Keeps the content-free activity ledger in `ledger` (spec 3.2 §7.10, feature `proc`):
    /// every event but `Health` writes its row, the [`ProgressSource`] gets the ledger at
    /// start so the hosted runtime writes its own activity, and rows are stamped on
    /// [`KernelConfig::clock`].
    #[cfg(feature = "proc")]
    #[must_use]
    pub fn ledger(mut self, ledger: Ledger) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// Runs the daily baseline pass under `policy` (spec 3.2 §7.10, feature
    /// `proc-baselines`). With a ledger and no policy, the pass runs under
    /// `BaselinePolicy::default()`; `BaselinePolicy::disabled()` turns it off.
    #[cfg(feature = "proc-baselines")]
    #[must_use]
    pub fn baseline_policy(mut self, policy: crate::proc::baselines::BaselinePolicy) -> Self {
        self.baselines = Some(policy);
        self
    }

    /// Words observations with `phraser`, a model used only after the deterministic
    /// detection (spec 3.2 §7.10, feature `proc-baselines`); without one the kernel's
    /// template words them.
    #[cfg(feature = "proc-baselines")]
    #[must_use]
    pub fn observation_phraser(
        mut self,
        phraser: Arc<dyn crate::proc::baselines::ObservationPhraser>,
    ) -> Self {
        self.phraser = Some(phraser);
        self
    }

    /// Sets the partition table (spec 3.2 §7.8). Defaults to one in memory. Give the kernel
    /// the [`Partitions`] its partition harnesses were built with, so their policies, the
    /// control plane and the daily review read one table.
    #[must_use]
    pub fn partitions(mut self, partitions: Partitions) -> Self {
        self.partitions = Some(partitions);
        self
    }

    /// Hosts `runner`, the harness of partition `partition`, as the target `target`, bound to
    /// the partition (spec 3.2 §7.8, K-I15): a process of another partition cannot run it,
    /// a spawn of it in the default partition runs in `partition`, and a run of it enqueued
    /// outside the kernel is listed in `partition`.
    #[must_use]
    pub fn partition_agent(
        mut self,
        partition: impl Into<PartitionId>,
        target: impl Into<String>,
        runner: Arc<Runner>,
    ) -> Self {
        let target = target.into();
        self.partition_targets.insert(target.clone(), partition.into());
        self.agents.insert(target, runner);
        self
    }

    /// Sets where the cron wakes of sleepers keep their cursors (spec 3.2 §7.4). Defaults to
    /// [`InMemoryCronWatermarkStore`], which keeps nothing across a restart.
    ///
    /// The kernel fires a sleeper's cron wake itself and keeps the expression's cursor here
    /// under `kernel:<expression>`. With a durable store shared by every replica, a restart
    /// or another replica fires each occurrence missed while no kernel ran once, as the
    /// sleepers' `missed` policies pick them, and never one that already fired.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_core::InMemoryRunStore;
    /// use adk_kernel::{InMemoryCronWatermarkStore, Kernel};
    ///
    /// let kernel = Kernel::builder()
    ///     .run_store(Arc::new(InMemoryRunStore::new()))
    ///     .watermark_store(Arc::new(InMemoryCronWatermarkStore::new()))
    ///     .build()
    ///     .unwrap();
    /// # let _ = kernel;
    /// ```
    #[must_use]
    pub fn watermark_store(mut self, store: Arc<dyn CronWatermarkStore>) -> Self {
        self.watermarks = Some(store);
        self
    }

    /// Builds the kernel. Nothing runs until [`Kernel::start`].
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when no run store is set, the control plane
    /// configuration is refused (K-I18), or a partition has two progress sources.
    pub fn build(self) -> Result<Kernel> {
        let runs = self.runs.ok_or_else(|| KernelError::Config {
            reason: "a kernel needs a durable run store; call KernelBuilder::run_store".into(),
        })?;
        let mut partition_progress = BTreeMap::new();
        for (partition, source) in self.partition_progress {
            if partition_progress.contains_key(&partition) {
                return Err(KernelError::Config {
                    reason: format!(
                        "partition `{partition}` has two progress sources; register one per \
                         partition with KernelBuilder::partition_progress_source"
                    ),
                });
            }
            partition_progress.insert(partition, source);
        }
        let mut config = self.config;
        if let Some(governance) = self.governance {
            config.governance = governance;
        }
        config.control.validate()?;
        let mut names = std::collections::BTreeSet::new();
        if config.queues.is_empty()
            || !config.queues.iter().all(|queue| names.insert(queue.as_str().to_string()))
        {
            return Err(KernelError::Config {
                reason: format!(
                    "the kernel needs at least one queue class and each once; got {:?}",
                    config.queues
                ),
            });
        }
        let processes: Arc<dyn ProcessStore> =
            self.processes.unwrap_or_else(|| Arc::new(InMemoryProcessStore::new()));
        let partitions = self.partitions.unwrap_or_else(Partitions::in_memory);
        if let Some(trail) = &self.audit {
            partitions.attach_audit(trail.clone());
        }
        let bus = EventBus::default();
        for sink in self.sinks {
            bus.add_sink(sink);
        }
        let trigger_names = self.triggers.iter().map(|trigger| trigger.name.clone()).collect();
        let watermarks: Arc<dyn CronWatermarkStore> =
            self.watermarks.unwrap_or_else(|| Arc::new(InMemoryCronWatermarkStore::new()));
        let trigger_crons = trigger_cron_sources(&self.triggers, &self.wake_sources, &watermarks)?;
        #[cfg(feature = "proc")]
        let ledger = self.ledger.map(|ledger| ledger.with_clock(Arc::clone(&config.clock)));
        #[cfg(feature = "proc")]
        if let Some(ledger) = &ledger {
            bus.add_sink(Arc::new(LedgerEventSink::new(
                ledger.clone(),
                runs.clone(),
                processes.clone(),
            )));
        }
        let kernel_id = format!("kernel-{}", uuid::Uuid::new_v4());
        // The kernel's own proposers reach the agenda through this handle, bound below.
        let kernel_agenda = AgendaHandle::new();
        #[cfg(feature = "proc-baselines")]
        let proc = ledger.as_ref().map(|ledger| {
            let mut pass = crate::proc::baselines::BaselinePass::new(
                ledger.clone(),
                self.baselines.clone().unwrap_or_default(),
            );
            if let Some(phraser) = self.phraser.clone() {
                pass = pass.with_phraser(phraser);
            }
            Arc::new(crate::proc::baselines::ProcRuntime::new(
                pass,
                bus.clone(),
                self.audit.clone(),
                kernel_id.clone(),
                config.app_name.clone(),
                kernel_agenda.clone(),
            ))
        });
        let router = self
            .triggers
            .into_iter()
            .fold(WakeRouter::new(runs.clone(), processes.clone()), WakeRouter::with_trigger)
            .with_bus(bus.clone())
            .with_clock(Arc::clone(&config.clock));
        // The agents a person adds share one live target table with every worker (spec 3.3
        // §10 S4).
        let agent_store: Arc<dyn crate::agents::AgentStore> =
            self.agent_store.unwrap_or_else(|| Arc::new(crate::stores::InMemoryAgentStore::new()));
        let targets = Arc::new(crate::agents::Targets::new(
            self.partition_targets,
            Some(Arc::clone(&agent_store)),
        ));
        let sup = Arc::new(Supervision {
            app_name: config.app_name.clone(),
            kernel_id,
            queues: config.queues.clone(),
            runs: runs.clone(),
            processes,
            bus,
            governance: config.governance.clone(),
            audit: self.audit.clone(),
            sessions: self.sessions.clone(),
            progress: self.progress,
            partition_progress,
            workers: std::sync::OnceLock::new(),
            handles: std::sync::Mutex::new(Vec::new()),
            draining: std::sync::atomic::AtomicBool::new(false),
            local: std::sync::Mutex::new(HashMap::new()),
            roots_changed: tokio::sync::Notify::new(),
            watched: std::sync::Mutex::new(BTreeMap::new()),
            clock: Arc::clone(&config.clock),
            proactivity: config.proactivity.clone(),
            quiet_hours: config.quiet_hours,
            quotas: config.quotas.clone(),
            agenda: self.agenda.unwrap_or_else(|| Arc::new(InMemoryAgendaStore::new())),
            admission: tokio::sync::Mutex::new(()),
            rule_source: self.rule_source,
            reviewed: std::sync::Mutex::new(None),
            crons: std::sync::Mutex::new(BTreeMap::new()),
            watermarks,
            cron_started: std::sync::Mutex::new(None),
            triggers: trigger_names,
            #[cfg(feature = "proc-baselines")]
            proc: proc.clone(),
            targets: Arc::clone(&targets),
            partitions: partitions.clone(),
            control: Arc::new(crate::control::LastRead::new(config.control.control_store.clone())),
        });
        let run_finished = Arc::new(RunFinishedWakeSource::new());
        let hooks =
            Arc::new(KernelHooks { sup: Arc::clone(&sup), run_finished: run_finished.clone() });
        // The consolidation tick's workflow sits before the caller's (spec 3.2 §7.5).
        let consolidators = Arc::new(self.consolidators);
        let user_workflows = if consolidators.is_empty() {
            self.workflows.clone()
        } else {
            Some(Arc::new(ConsolidationExecutor::new(
                Arc::clone(&sup),
                Arc::clone(&consolidators),
                kernel_agenda.clone(),
                self.workflows.clone(),
            )) as Arc<dyn WorkflowExecutor>)
        };
        let workflows = Arc::new(KernelWorkflows {
            user: user_workflows,
            wake: KernelWakeExecutor::new(router.clone()),
            #[cfg(feature = "proc-baselines")]
            proc,
        });
        let workers = config
            .queues
            .iter()
            .map(|queue| {
                let mut worker = DurableWorker::new(runs.clone())
                    .with_config(config.worker_config(queue))
                    .with_governance(config.governance.clone())
                    .with_workflows(workflows.clone())
                    .with_hooks(hooks.clone());
                if let Some(trail) = &self.audit {
                    worker = worker.with_audit(trail.clone());
                }
                for (target, runner) in &self.agents {
                    worker = worker.with_agent(target.clone(), runner.clone());
                }
                // The agents a person adds, hosted while the kernel runs (spec 3.3 §10 S4).
                worker = worker.with_agent_table(targets.table.clone());
                (queue.clone(), worker)
            })
            .collect();
        let _ = sup.workers.set(workers);
        let mut wake_sources = self.wake_sources;
        wake_sources.extend(trigger_crons);
        wake_sources.push(run_finished);
        let processes = ProcessTable::supervised(Arc::clone(&sup), self.spend);
        let agenda = Agenda::new(
            Arc::clone(&sup),
            processes.clone(),
            self.approvals.clone(),
            self.audiences,
        );
        for handle in self.agenda_handles.iter().chain([&kernel_agenda]) {
            handle.bind(&agenda);
        }
        let consolidation = (!consolidators.is_empty())
            .then(|| ConsolidationTicker::new(Arc::clone(&sup), processes.clone(), consolidators));
        let agent_table = crate::agents::Agents::new(crate::agents::AgentsParts {
            sup: Arc::clone(&sup),
            processes: processes.clone(),
            agenda: agenda.clone(),
            targets,
            store: agent_store,
            factory: self.agent_factory,
            approvals: self.approvals.clone(),
            statics: self.agents.keys().cloned().collect(),
            start_modes: config.control.start_modes.clone(),
            workflows: self.workflows.clone(),
            #[cfg(feature = "drivers")]
            drivers: self.drivers.clone(),
            #[cfg(feature = "proc")]
            ledger: ledger.clone(),
        });
        for handle in &self.agents_handles {
            handle.bind(&agent_table);
        }
        Ok(Kernel {
            inner: Arc::new(KernelInner {
                config,
                sup,
                processes,
                agenda,
                partitions,
                router,
                schedules: self.schedules,
                approvals: self.approvals,
                sessions: self.sessions,
                agents: self.agents,
                agent_table,
                workflows: self.workflows,
                wake_sources,
                queue_ingresses: self.queue_ingresses,
                consolidation,
                #[cfg(feature = "drivers")]
                drivers: self.drivers,
                runtime: tokio::sync::Mutex::new(Runtime::default()),
                status: Arc::new(RwLock::new(Status::default())),
                #[cfg(feature = "proc")]
                ledger,
            }),
        })
    }
}

/// The kernel's own source for each cron expression of `triggers` no source in `sources`
/// ticks on (spec 3.2 §7.4, spec 3.5 §7.11): one per expression, so every trigger on it
/// matches each tick once, planned with the widest missed-tick policy of its triggers, its
/// cursor kept in `watermarks` across restarts.
fn trigger_cron_sources(
    triggers: &[Trigger],
    sources: &[Arc<dyn WakeSource>],
    watermarks: &Arc<dyn CronWatermarkStore>,
) -> Result<Vec<Arc<dyn WakeSource>>> {
    let covered: std::collections::BTreeSet<&str> =
        sources.iter().filter_map(|source| source.cron_expression()).collect();
    let mut crons: BTreeMap<&str, adk_core::schedule::MissedFirePolicy> = BTreeMap::new();
    for trigger in triggers {
        if let Wake::Cron { expr, missed } = &trigger.wake
            && !covered.contains(expr.as_str())
        {
            let widest = crons
                .get(expr.as_str())
                .map_or(*missed, |held| crate::wake::widest_policy(*held, *missed));
            crons.insert(expr, widest);
        }
    }
    crons
        .into_iter()
        .map(|(expr, missed)| {
            let name =
                format!("trigger-cron:{}", expr.split_whitespace().collect::<Vec<_>>().join("_"));
            let watermark = SourceWatermark::new(Arc::clone(watermarks), name.clone());
            let source = CronWakeSource::watermarked(name, expr, missed, Arc::new(watermark))?;
            Ok(Arc::new(source) as Arc<dyn WakeSource>)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use adk_core::{InMemoryRunStore, NewRun};
    use serde_json::json;

    use super::*;
    use crate::process::{ProcessKind, ProcessSpec};
    use crate::wake::WakeSet;

    async fn eventually(what: &str, mut probe: impl FnMut() -> bool) {
        let started = tokio::time::Instant::now();
        while started.elapsed() < Duration::from_secs(3) {
            if probe() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// Two kernels built from the defaults in one process and one home both start: the
    /// defaults serve no control plane, so neither takes the other's socket or pipe.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kernels_built_from_the_defaults_share_a_home() {
        let home = tempfile::tempdir().unwrap();
        let config = KernelConfig::default().with_home(home.path());
        assert_eq!((config.control.socket.as_deref(), config.control.http), (None, None));
        let build = || {
            Kernel::builder()
                .config(config.clone())
                .run_store(Arc::new(InMemoryRunStore::new()))
                .build()
                .unwrap()
        };
        let (first, second) = (build(), build());
        first.start().await.unwrap();
        second.start().await.unwrap();
        assert!(!home.path().join(ControlPlaneConfig::DEFAULT_SOCKET).exists());
        second.stop().await.unwrap();
        first.stop().await.unwrap();

        // Asked for, the socket at the home is still one kernel's.
        #[cfg(unix)]
        {
            let served = config.with_control(ControlPlaneConfig::default());
            let build = || {
                Kernel::builder()
                    .config(served.clone())
                    .run_store(Arc::new(InMemoryRunStore::new()))
                    .build()
                    .unwrap()
            };
            let (first, second) = (build(), build());
            first.start().await.unwrap();
            let error = second.start().await.unwrap_err();
            assert!(error.to_string().contains("already listens"), "{error}");
            first.stop().await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_file_wake_spawned_after_start_is_watched_until_no_sleeper_needs_it() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = Kernel::builder()
            .config(
                KernelConfig::default()
                    // Long enough that only the spawn's signal can explain a prompt watch.
                    .with_scheduler_tick(Duration::from_secs(60))
                    .with_control(ControlPlaneConfig::default().without_socket()),
            )
            .run_store(Arc::new(InMemoryRunStore::new()))
            .build()
            .unwrap();
        kernel.start().await.unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(kernel.inner.sup.watched_roots().is_empty());

        let wake = WakeSet::any_of([Wake::FileChange {
            root: dir.path().to_path_buf(),
            glob: "notes.md".into(),
            debounce: Duration::from_millis(100),
        }]);
        // Not claimable for an hour: it stays a queued sleeper the whole test.
        let run =
            NewRun::new("background", "adk-rust", "alice", "adk-rust", json!({ "text": "x" }))
                .with_not_before(chrono::Utc::now() + chrono::Duration::hours(1));
        let process = kernel
            .processes()
            .spawn(run, ProcessSpec::new(ProcessKind::Interactive).with_wake(wake))
            .await
            .unwrap();
        eventually("the root to be watched", || kernel.inner.sup.watched_roots() == [root.clone()])
            .await;

        kernel.processes().kill(&process.meta.pid, "not needed").await.unwrap();
        eventually("the root to be dropped", || kernel.inner.sup.watched_roots().is_empty()).await;
        kernel.stop().await.unwrap();
    }

    /// A standing cron trigger fires with no source registered for it: the kernel adds one
    /// watermarked source per expression, so two triggers on one expression each fire once
    /// per tick, and a registered source on the expression takes its place (spec 3.5 §7.11).
    /// The control plane's address is readable once it listens.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_standing_cron_trigger_fires_through_the_kernels_own_source() {
        let every_second = "* * * * * *";
        let wake =
            || Wake::Cron { expr: every_second.into(), missed: adk_core::MissedFirePolicy::Skip };
        let template = |target: &str| {
            NewRun::new("background", "adk-rust", "alice", target, json!({ "text": "tick" }))
        };
        let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
        let loopback: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
        let kernel =
            Kernel::builder()
                .config(KernelConfig::default().with_control(
                    ControlPlaneConfig::default().without_socket().with_http(loopback),
                ))
                .run_store(Arc::clone(&runs))
                .trigger(Trigger::new("digest-a", wake(), template("digest-a")))
                .trigger(Trigger::new("digest-b", wake(), template("digest-b")))
                .build()
                .unwrap();
        let crons: Vec<&str> =
            kernel.inner.wake_sources.iter().filter_map(|s| s.cron_expression()).collect();
        assert_eq!(crons, [every_second], "one source for the expression");
        assert_eq!(kernel.control_addr().await, None, "nothing listens before the start");
        kernel.start().await.unwrap();
        let addr = kernel.control_addr().await.expect("the control plane listens");
        assert_ne!(addr.port(), 0, "the bound port, not the configured 0");

        let started = tokio::time::Instant::now();
        let fired = loop {
            let targets: Vec<String> = runs
                .list(adk_core::RunFilter::default())
                .await
                .unwrap()
                .into_iter()
                .map(|run| run.target)
                .collect();
            if ["digest-a", "digest-b"].iter().all(|t| targets.iter().any(|run| run == t)) {
                break targets;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "no trigger fired: {targets:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        kernel.stop().await.unwrap();
        let count = |t: &str| fired.iter().filter(|run| *run == t).count();
        assert!(count("digest-a").abs_diff(count("digest-b")) <= 1, "{fired:?}");

        // A registered source on the expression takes the kernel's place.
        let watermarks: Arc<dyn CronWatermarkStore> = Arc::new(InMemoryCronWatermarkStore::new());
        let own = CronWakeSource::watermarked(
            "mine",
            every_second,
            adk_core::MissedFirePolicy::Skip,
            Arc::new(SourceWatermark::new(watermarks, "mine")),
        )
        .unwrap();
        let kernel = Kernel::builder()
            .run_store(Arc::new(InMemoryRunStore::new()))
            .wake_source(Arc::new(own))
            .trigger(Trigger::new("digest-a", wake(), template("digest-a")))
            .build()
            .unwrap();
        let names: Vec<&str> = kernel
            .inner
            .wake_sources
            .iter()
            .filter(|s| s.cron_expression().is_some())
            .map(|s| s.name())
            .collect();
        assert_eq!(names, ["mine"]);
    }

    /// A sleeper on a cron wake no registered source covers (spec 3.2 §7.4, §8.1 `wake
    /// --cron`), on a manual clock: the kernel fires it at each occurrence — the first
    /// sleeper, then the successor it re-armed — and forgets the expression once the
    /// series is killed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cron_sleeper_wakes_at_each_occurrence_until_it_is_killed() {
        use adk_core::{Llm, ManualClock, RunState};
        use adk_model::{ScriptedLlm, ScriptedResponse};

        let clock = ManualClock::new(chrono::Utc::now());
        let runs: Arc<dyn DurableRunStore> =
            Arc::new(InMemoryRunStore::new().with_clock(Arc::new(clock.clone())));
        let model = Arc::new(ScriptedLlm::new(
            "scripted",
            ["first occurrence", "second occurrence"].map(ScriptedResponse::text),
        ));
        let agent = adk_agent::LlmAgentBuilder::new("agent")
            .model(Arc::clone(&model) as Arc<dyn Llm>)
            .build()
            .unwrap();
        let runner = Arc::new(
            Runner::builder()
                .app_name("adk-rust")
                .agent(Arc::new(agent))
                .session_service(Arc::new(adk_session::InMemorySessionService::new()))
                .build()
                .unwrap(),
        );
        let kernel = Kernel::builder()
            .config(
                KernelConfig::default()
                    .with_scheduler_tick(Duration::from_millis(50))
                    .with_clock(Arc::new(clock.clone()))
                    .with_control(ControlPlaneConfig::default().without_socket()),
            )
            .run_store(Arc::clone(&runs))
            .agent("adk-rust", runner)
            .build()
            .unwrap();
        kernel.start().await.unwrap();
        let hourly = "0 0 * * * *";
        let wake = WakeSet::any_of([Wake::Cron {
            expr: hourly.into(),
            missed: adk_core::MissedFirePolicy::Skip,
        }]);
        let run =
            NewRun::new("background", "adk-rust", "alice", "adk-rust", json!({ "text": "x" }));
        let first = kernel
            .processes()
            .spawn(run, ProcessSpec::new(ProcessKind::Scheduled).with_wake(wake.clone()))
            .await
            .unwrap()
            .meta
            .pid;
        let state = |pid: String| {
            let runs = Arc::clone(&runs);
            async move { runs.get(&pid).await.unwrap().map(|run| run.state) }
        };
        let wait_for = |what: &'static str, pid: String, wanted: RunState| async move {
            let started = tokio::time::Instant::now();
            while state(pid.clone()).await != Some(wanted) {
                assert!(started.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let successor = |parent: String| {
            let kernel = kernel.clone();
            async move {
                let started = tokio::time::Instant::now();
                loop {
                    let processes = kernel.processes().ps(ProcessFilter::default()).await.unwrap();
                    if let Some(next) =
                        processes.iter().find(|p| p.meta.parent.as_deref() == Some(parent.as_str()))
                    {
                        return next.meta.pid.clone();
                    }
                    assert!(
                        started.elapsed() < Duration::from_secs(5),
                        "timed out waiting for the successor of {parent}"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        wait_for("the sleeper asleep", first.clone(), RunState::Waiting).await;
        eventually("the expression to be fired by the kernel", || {
            kernel.inner.sup.watched_crons() == [hourly]
        })
        .await;
        assert_eq!(model.requests().len(), 0, "asleep until the hour");

        clock.advance(Duration::from_secs(3_601));
        wait_for("the first occurrence's run", first.clone(), RunState::Completed).await;
        let second = successor(first.clone()).await;
        wait_for("the successor asleep", second.clone(), RunState::Waiting).await;
        let meta = kernel.processes().get(&second).await.unwrap().unwrap().meta;
        assert_eq!(meta.wake, Some(wake.clone()), "the successor sleeps on the same cron wake");

        clock.advance(Duration::from_secs(3_601));
        wait_for("the second occurrence's run", second.clone(), RunState::Completed).await;
        assert_eq!(model.requests().len(), 2, "one run per occurrence");
        let third = successor(second).await;
        wait_for("the next successor asleep", third.clone(), RunState::Waiting).await;

        kernel.processes().kill(&third, "the series ends").await.unwrap();
        eventually("the expression to be dropped", || kernel.inner.sup.watched_crons().is_empty())
            .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let processes = kernel.processes().ps(ProcessFilter::default()).await.unwrap();
        assert!(
            !processes.iter().any(|p| p.meta.parent.as_deref() == Some(third.as_str())),
            "a killed sleeper is not re-armed"
        );
        kernel.stop().await.unwrap();
    }
}
