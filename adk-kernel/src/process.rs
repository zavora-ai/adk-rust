//! The process model (spec 3.2 §7.3).
//!
//! A process is a durable run (§6 D-2, K-I1): its id is the run id, its state is the run's
//! [`RunState`](adk_core::RunState), and `Waiting` is sleep. The kernel keeps what the run
//! store does not in a side table, [`ProcessMeta`] in a [`ProcessStore`],
//! and serves a live [`ProcessView`] of each process without `adk-eval` (§7.10, §13 Q4).
//!
//! | OS notion | Kernel mapping |
//! |---|---|
//! | Process states | `RunState::{Queued, Running, Waiting, Completed, Failed, Cancelled}`; `Waiting` is sleep |
//! | Process tree | [`ProcessMeta::parent`]: the run that caused the process |
//! | Time slice | The lease; a process yields at every model-call boundary |
//! | `kill -TERM` / `kill -KILL` | `request_cancel` at the next checkpoint / lease revocation |
//! | Zombie | A `Running` run whose lease expired; the next claim resumes it |
//!
//! [`ProcessTable`] is the view over `DurableRunStore` plus the side table, with
//! `ps`, `kill`, `nice`, `suspend` and `resume`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{DurableRunStore, NewRun, RunRecord, SpendLedger};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::sched::QueueClass;
use crate::stores::{ProcessFilter, ProcessStore};
use crate::wake::WakeSet;

/// A process as `ps` shows it: the run, the kernel's side table and the live view
/// (spec 3.2 §7.3).
///
/// # Example
///
/// ```rust
/// use adk_core::{NewRun, RunRecord};
/// use adk_kernel::{Process, ProcessKind, ProcessMeta, QueueClass};
///
/// let run = RunRecord::queued("run-1", NewRun::new("background", "adk-rust", "alice", "adk-rust", serde_json::json!({})), chrono::Utc::now());
/// let meta = ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal);
/// let process = Process::new(run, meta);
/// assert_eq!(process.meta.pid, process.run.run_id);
/// assert_eq!(process.view.model_calls, 0);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Process {
    /// The durable run (`adk-core/src/run_store.rs`).
    pub run: RunRecord,
    /// The kernel-owned side table, keyed by run id.
    pub meta: ProcessMeta,
    /// Live aggregates, served by the kernel without `adk-eval` (spec 3.2 §7.10).
    pub view: ProcessView,
}

impl Process {
    /// A process with an empty view.
    pub fn new(run: RunRecord, meta: ProcessMeta) -> Self {
        Self { run, meta, view: ProcessView::default() }
    }

    /// Sets the live view.
    #[must_use]
    pub fn with_view(mut self, view: ProcessView) -> Self {
        self.view = view;
        self
    }
}

/// A process's live aggregates (spec 3.2 §7.3, §7.10, §13 Q4): what `ps` and `top` show,
/// computed by sum and count over the `SpendLedger` and the kernel's records. Spend is in
/// micro-USD, as `SpendLedger` counts it.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::ProcessView;
///
/// let mut view = ProcessView::default();
/// view.age = Duration::from_secs(90);
/// view.model_calls = 3;
/// let json = serde_json::to_value(&view).unwrap();
/// assert_eq!(json["ageMs"], 90_000);
/// assert_eq!(json["modelCalls"], 3);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ProcessView {
    /// Since the last state change: a `Waiting` process's time asleep, a parked run's wait.
    #[serde(rename = "ageMs", with = "crate::serde_util::millis")]
    pub age: Duration,
    /// Spend today (UTC), from `SpendLedger::spent(key, SpendPeriod::Day)`.
    pub spend_today: u64,
    /// Spend over the last seven days.
    #[serde(rename = "spend7d")]
    pub spend_7d: u64,
    /// A `Running` process's spend per minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burn_rate: Option<f64>,
    /// A `Running` process's spend by the end of the UTC day at its burn rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<u64>,
    /// Model calls the process made.
    pub model_calls: u64,
    /// Tool calls the process made.
    pub tool_calls: u64,
    /// The loop guard's last verdict (spec 3.1 §7.13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_guard: Option<String>,
    /// A goal process's progress (spec 3.1 §7.11, spec 3.2 §10 HK3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalProgress>,
}

/// Where a goal process stands (spec 3.2 §7.3, §10 HK3): iteration of max, spend, duration,
/// and the last check's exit code and output tail.
///
/// The kernel cannot read harness types (§6 D-1), so the harness reports it through a
/// [`ProgressSource`].
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::GoalProgress;
///
/// let progress = GoalProgress::new(3, 10)
///     .with_spend(12_500)
///     .with_duration(Duration::from_secs(240))
///     .with_last_check(1, "test result: FAILED. 1 passed; 2 failed");
/// assert_eq!((progress.iteration, progress.max_iterations), (3, 10));
/// assert_eq!(progress.last_check_exit, Some(1));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GoalProgress {
    /// The current iteration.
    pub iteration: u32,
    /// The most iterations the goal may take.
    pub max_iterations: u32,
    /// Spend so far, in micro-USD, from the `SpendLedger`.
    pub spend: u64,
    /// How long the goal has run.
    #[serde(rename = "durationMs", with = "crate::serde_util::millis")]
    pub duration: Duration,
    /// The last check's exit code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_exit: Option<i32>,
    /// The tail of the last check's output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_tail: Option<String>,
}

impl GoalProgress {
    /// Progress at `iteration` of `max_iterations`.
    pub fn new(iteration: u32, max_iterations: u32) -> Self {
        Self { iteration, max_iterations, ..Self::default() }
    }

    /// Sets the spend so far, in micro-USD.
    #[must_use]
    pub fn with_spend(mut self, micro_usd: u64) -> Self {
        self.spend = micro_usd;
        self
    }

    /// Sets how long the goal has run.
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = duration;
        self
    }

    /// Sets the last check's exit code and output tail.
    #[must_use]
    pub fn with_last_check(mut self, exit: i32, tail: impl Into<String>) -> Self {
        self.last_check_exit = Some(exit);
        self.last_check_tail = Some(tail.into());
        self
    }
}

/// The kernel-owned side table of one process, keyed by run id (spec 3.2 §7.3, §6 D-2).
///
/// `RunRecord` already carries the run's principal and whether its user was authenticated;
/// this holds only what the run store does not.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{PartitionId, ProcessKind, ProcessMeta, QueueClass, Suspension};
///
/// let meta = ProcessMeta::new("run-9", QueueClass::Background, ProcessKind::Triggered { source: "file".into() })
///     .with_parent("run-1")
///     .with_suspension(Suspension::Freeze)
///     .with_label("repo", "adk-rust");
/// assert_eq!(meta.partition, PartitionId::default());
/// let json = serde_json::to_value(&meta).unwrap();
/// assert_eq!(json["suspendedBy"], "freeze");
/// assert_eq!(json["kind"]["triggered"]["source"], "file");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ProcessMeta {
    /// The process id, equal to the run id.
    pub pid: String,
    /// The run that caused it: a proposal, a run-finished wake, a trigger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The partition it runs in.
    #[serde(default)]
    pub partition: PartitionId,
    /// Its priority class (spec 3.2 §7.9).
    pub queue: QueueClass,
    /// What kind of work it is.
    pub kind: ProcessKind,
    /// Its wake set, while `Waiting`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<WakeSet>,
    /// Why it is suspended, while parked by a suspension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_by: Option<Suspension>,
    /// The agenda item it runs (spec 3.2 §7.5, phase 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agenda_item: Option<String>,
    /// Free-form labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl ProcessMeta {
    /// The side table of process `pid` in the default partition, with no parent, wake set,
    /// suspension, agenda item or labels.
    pub fn new(pid: impl Into<String>, queue: QueueClass, kind: ProcessKind) -> Self {
        Self {
            pid: pid.into(),
            parent: None,
            partition: PartitionId::default(),
            queue,
            kind,
            wake: None,
            suspended_by: None,
            agenda_item: None,
            labels: BTreeMap::new(),
        }
    }

    /// Sets the run that caused the process.
    #[must_use]
    pub fn with_parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Sets the partition.
    #[must_use]
    pub fn with_partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = partition.into();
        self
    }

    /// Sets the wake set.
    #[must_use]
    pub fn with_wake(mut self, wake: WakeSet) -> Self {
        self.wake = Some(wake);
        self
    }

    /// Sets why the process is suspended.
    #[must_use]
    pub fn with_suspension(mut self, suspension: Suspension) -> Self {
        self.suspended_by = Some(suspension);
        self
    }

    /// Sets the agenda item.
    #[must_use]
    pub fn with_agenda_item(mut self, item: impl Into<String>) -> Self {
        self.agenda_item = Some(item.into());
        self
    }

    /// Adds a label.
    #[must_use]
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }
}

/// What kind of work a process is (spec 3.2 §7.3).
///
/// # Example
///
/// ```rust
/// use adk_kernel::ProcessKind;
///
/// assert_eq!(ProcessKind::Goal.to_string(), "goal");
/// assert_eq!(ProcessKind::Triggered { source: "cron".into() }.to_string(), "triggered:cron");
/// assert_eq!(serde_json::to_value(ProcessKind::Interactive).unwrap(), "interactive");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum ProcessKind {
    /// A human turn.
    Interactive,
    /// A goal (spec 3.1 §7.11).
    Goal,
    /// A firing of a schedule.
    Scheduled,
    /// A run a wake source started.
    Triggered {
        /// The wake source's name.
        source: String,
    },
    /// A follow-up the agent proposed (spec 3.2 §7.5, phase 2).
    Proposed,
    /// The kernel's own work.
    Maintenance,
}

impl fmt::Display for ProcessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Interactive => f.write_str("interactive"),
            Self::Goal => f.write_str("goal"),
            Self::Scheduled => f.write_str("scheduled"),
            Self::Triggered { source } => write!(f, "triggered:{source}"),
            Self::Proposed => f.write_str("proposed"),
            Self::Maintenance => f.write_str("maintenance"),
        }
    }
}

/// Why a process is parked by a suspension rather than a wake set (spec 3.2 §7.3).
///
/// A suspended or freeze-parked process has no expiry of its own; `ps` shows its age
/// (§7.4).
///
/// # Example
///
/// ```rust
/// use adk_kernel::Suspension;
///
/// assert_eq!(serde_json::to_value(Suspension::Freeze).unwrap(), "freeze");
/// assert_eq!(Suspension::Preempted { by: "run-3".into() }.to_string(), "preempted by run-3");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum Suspension {
    /// `adk-rust suspend` or [`ProcessTable::suspend`]; resumes on `resume`.
    Manual,
    /// An interactive run preempted it (spec 3.2 §7.9, phase 2); resumes when that run
    /// finishes.
    Preempted {
        /// The process that preempted it.
        by: String,
    },
    /// A freeze parked it (`ParkReason::Frozen`, §10 S3); resumes only on `unfreeze`
    /// (K-I21).
    Freeze,
    /// `Kernel::drain` parked it; resumes when the kernel starts again.
    Drain,
}

impl fmt::Display for Suspension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manual => f.write_str("manual"),
            Self::Preempted { by } => write!(f, "preempted by {by}"),
            Self::Freeze => f.write_str("freeze"),
            Self::Drain => f.write_str("drain"),
        }
    }
}

/// What [`ProcessTable::spawn`] records beside the run it enqueues (spec 3.2 §7.3).
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::{ProcessKind, ProcessSpec, Wake, WakeSet};
///
/// let spec = ProcessSpec::new(ProcessKind::Goal)
///     .with_partition("work")
///     .with_wake(WakeSet::any_of([Wake::FileChange {
///         root: ".".into(),
///         glob: "Cargo.lock".into(),
///         debounce: Duration::from_millis(500),
///     }]));
/// assert!(spec.wake.is_some());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ProcessSpec {
    /// What kind of work it is.
    pub kind: ProcessKind,
    /// The run that caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The partition it runs in.
    #[serde(default)]
    pub partition: PartitionId,
    /// When set, the process starts asleep on this wake set: it parks `Waiting` before its
    /// first model call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<WakeSet>,
    /// The agenda item it runs (phase 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agenda_item: Option<String>,
    /// Free-form labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl ProcessSpec {
    /// A spec of `kind` in the default partition that starts at once.
    pub fn new(kind: ProcessKind) -> Self {
        Self {
            kind,
            parent: None,
            partition: PartitionId::default(),
            wake: None,
            agenda_item: None,
            labels: BTreeMap::new(),
        }
    }

    /// Sets the run that caused the process.
    #[must_use]
    pub fn with_parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Sets the partition.
    #[must_use]
    pub fn with_partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = partition.into();
        self
    }

    /// Starts the process asleep on `wake`.
    #[must_use]
    pub fn with_wake(mut self, wake: WakeSet) -> Self {
        self.wake = Some(wake);
        self
    }

    /// Sets the agenda item.
    #[must_use]
    pub fn with_agenda_item(mut self, item: impl Into<String>) -> Self {
        self.agenda_item = Some(item.into());
        self
    }

    /// Adds a label.
    #[must_use]
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }
}

/// Reports what only the hosted agent knows about a process: a goal's progress and the loop
/// guard's verdict (spec 3.2 §7.3, §10 HK3).
///
/// `adk-kernel` depends on no harness type (§6 D-1); the `kernel` feature of `adk-harness`
/// implements this over its goal store and loop guard and registers it with
/// [`KernelBuilder::progress_source`](crate::KernelBuilder::progress_source). Every method
/// has a default that reports nothing.
///
/// # Example
///
/// ```rust
/// use adk_core::RunRecord;
/// use adk_kernel::{GoalProgress, ProgressSource};
/// use async_trait::async_trait;
///
/// struct FixedProgress;
///
/// #[async_trait]
/// impl ProgressSource for FixedProgress {
///     async fn goal_progress(&self, _run: &RunRecord) -> adk_kernel::Result<Option<GoalProgress>> {
///         Ok(Some(GoalProgress::new(2, 10)))
///     }
/// }
/// ```
#[async_trait]
pub trait ProgressSource: Send + Sync {
    /// The progress of the goal `run` executes, or `None` when it runs no goal.
    ///
    /// # Errors
    ///
    /// Returns the source's store error; the view then omits the goal.
    async fn goal_progress(&self, _run: &RunRecord) -> Result<Option<GoalProgress>> {
        Ok(None)
    }

    /// The loop guard's last verdict for `run`, or `None` when it has none.
    ///
    /// # Errors
    ///
    /// Returns the source's store error; the view then omits the verdict.
    async fn loop_guard(&self, _run: &RunRecord) -> Result<Option<String>> {
        Ok(None)
    }
}

/// The process table: a view over `DurableRunStore` plus the kernel's
/// [`ProcessStore`] (spec 3.2 §7.3).
///
/// [`Kernel::processes`](crate::Kernel::processes) returns the kernel's table, which reaches
/// the kernel's workers for `suspend`; a table built with [`new`](Self::new) reads and
/// writes the stores only.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_kernel::{InMemoryProcessStore, ProcessFilter, ProcessTable};
///
/// # async fn example() -> adk_kernel::Result<()> {
/// let table = ProcessTable::new(Arc::new(InMemoryRunStore::new()), Arc::new(InMemoryProcessStore::new()));
/// for process in table.ps(ProcessFilter::default()).await? {
///     println!("{} {:?} {}", process.meta.pid, process.run.state, process.meta.kind);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ProcessTable {
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<dyn ProcessStore>,
    // Unimplemented: read by `ps` once feat/kernel-supervisor lands (spec 3.2 §7.3).
    #[allow(dead_code)]
    spend: Option<Arc<dyn SpendLedger>>,
    // Unimplemented: read by `ps` once feat/kernel-supervisor lands (spec 3.2 §7.3).
    #[allow(dead_code)]
    progress: Option<Arc<dyn ProgressSource>>,
}

impl fmt::Debug for ProcessTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessTable")
            .field("spend", &self.spend.is_some())
            .field("progress", &self.progress.is_some())
            .finish_non_exhaustive()
    }
}

impl ProcessTable {
    /// A table over `runs` and `processes`.
    pub fn new(runs: Arc<dyn DurableRunStore>, processes: Arc<dyn ProcessStore>) -> Self {
        Self { runs, processes, spend: None, progress: None }
    }

    /// Reads each process's spend from `ledger` for its view.
    #[must_use]
    pub fn with_spend_ledger(mut self, ledger: Arc<dyn SpendLedger>) -> Self {
        self.spend = Some(ledger);
        self
    }

    /// Reads goal progress and loop-guard verdicts from `source` for each view.
    #[must_use]
    pub fn with_progress_source(mut self, source: Arc<dyn ProgressSource>) -> Self {
        self.progress = Some(source);
        self
    }

    /// The run store the table reads.
    pub fn run_store(&self) -> &Arc<dyn DurableRunStore> {
        &self.runs
    }

    /// The side table the table reads.
    pub fn process_store(&self) -> &Arc<dyn ProcessStore> {
        &self.processes
    }

    /// Lists the processes `filter` selects, each with its live [`ProcessView`]; `Waiting`
    /// processes show their age (spec 3.2 §7.3, §8.1 `adk-rust ps`).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] or [`KernelError::Core`] when a store fails.
    pub async fn ps(&self, _filter: ProcessFilter) -> Result<Vec<Process>> {
        Err(KernelError::Unimplemented { item: "process::ProcessTable::ps" })
    }

    /// The process `pid` with its live view, or `None` when no process has that id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] or [`KernelError::Core`] when a store fails.
    pub async fn get(&self, _pid: &str) -> Result<Option<Process>> {
        Err(KernelError::Unimplemented { item: "process::ProcessTable::get" })
    }

    /// Enqueues `run` and records its process (spec 3.2 §7.3, §7.4). The process's queue is
    /// the run's queue. With [`ProcessSpec::wake`] the process starts asleep: it parks
    /// `Waiting` on its wake set before its first model call and holds no lease (K-I2).
    ///
    /// An `idempotency_key` already held in the queue returns the existing process.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Frozen`] while the kernel is frozen, the run store's error when
    /// `run` is invalid, or a store error.
    pub async fn spawn(&self, _run: NewRun, _spec: ProcessSpec) -> Result<Process> {
        Err(KernelError::Unimplemented { item: "process::ProcessTable::spawn" })
    }

    /// Kills `pid` through `DurableRunStore::request_cancel`: cooperative at the next
    /// checkpoint, never leaving a lease held (K-I5); a `Queued` or `Waiting` process,
    /// freeze-parked ones included, is `Cancelled` at once (K-I21).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`] for an unknown `pid`, [`KernelError::InvalidState`]
    /// for a finished one, or a store error.
    pub async fn kill(&self, _pid: &str, _reason: &str) -> Result<()> {
        Err(KernelError::Unimplemented { item: "process::ProcessTable::kill" })
    }

    /// Moves a `Queued` or `Waiting` process to `queue` (spec 3.2 §6 D-5).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`] for an unknown `pid`,
    /// [`KernelError::InvalidState`] for a `Running` or finished one, or a store error.
    pub async fn nice(&self, _pid: &str, _queue: QueueClass) -> Result<()> {
        Err(KernelError::Unimplemented { item: "process::ProcessTable::nice" })
    }

    /// Suspends `pid` through the worker's suspend path (§10 S1): it stops at its next
    /// model-call boundary and parks `Waiting` with [`Suspension::Manual`]; a `Queued` or
    /// `Waiting` process is marked suspended where it is.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`] for an unknown `pid`, [`KernelError::InvalidState`]
    /// for a finished one, or a store error.
    pub async fn suspend(&self, _pid: &str) -> Result<()> {
        Err(KernelError::Unimplemented { item: "process::ProcessTable::suspend" })
    }

    /// Resumes a suspended or sleeping `pid` through `DurableRunStore::wake`. A process a
    /// freeze parked resumes only on `unfreeze` (K-I21).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`] for an unknown `pid`, [`KernelError::InvalidState`]
    /// when it is not `Waiting` or was parked by a freeze, or a store error.
    pub async fn resume(&self, _pid: &str) -> Result<()> {
        Err(KernelError::Unimplemented { item: "process::ProcessTable::resume" })
    }
}
