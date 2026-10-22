//! The process model (spec 3.2 §7.3).
//!
//! A process is a durable run (§6 D-2, K-I1): its id is the run id, its state is the run's
//! [`RunState`], and `Waiting` is sleep. The kernel keeps what the run
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

use adk_core::{
    AdkError, DurableRunStore, Enqueued, ErrorCategory, ErrorComponent, NewRun, RunFilter,
    RunRecord, RunState, SpendLedger,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit;
use crate::bus::KernelEvent;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::sched::QueueClass;
use crate::stores::{ProcessFilter, ProcessStore, SpendScope};
use crate::supervise::{SPAWN_GRACE, Supervision, transcript_counts};
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
/// computed by sum and count over the kernel's records. Spend is in micro-USD, as
/// `SpendLedger` counts it; the kernel attributes each charge to the run that made it and
/// keeps daily totals per process ([`ProcessStore::spend`]).
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
    /// Spend today (UTC), from the kernel's daily totals.
    pub spend_today: u64,
    /// Spend over the last seven UTC days, today included, from the kernel's daily totals.
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
    /// A suspension was requested and the run has not parked yet. Every replica's kernel
    /// mirrors pending requests and suspends the runs its own workers lease, so a process
    /// leased by another replica suspends too; the requester clears the flag once the run
    /// parks (spec 3.2 §7.3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suspend_requested: bool,
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
            suspend_requested: false,
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

/// What a session's runs have used so far, as the hosted agent counts it (spec 3.2 §7.3,
/// §7.10): model calls, tool calls and cost in micro-USD.
///
/// The counter is monotonic for the life of the agent: the kernel attributes the
/// difference between two readings to the process running in the session, so a process
/// sees only what it used.
///
/// # Example
///
/// ```rust
/// use adk_kernel::RunUsage;
///
/// let usage = RunUsage::new(3, 2, 12_500);
/// assert_eq!(usage.cost_micro_usd, 12_500);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RunUsage {
    /// Model calls.
    pub model_calls: u64,
    /// Tool calls.
    pub tool_calls: u64,
    /// Cost, in micro-USD.
    pub cost_micro_usd: u64,
}

impl RunUsage {
    /// Usage of `model_calls`, `tool_calls` and `cost_micro_usd`.
    pub fn new(model_calls: u64, tool_calls: u64, cost_micro_usd: u64) -> Self {
        Self { model_calls, tool_calls, cost_micro_usd }
    }
}

/// Reports what only the hosted agent knows about a process: a goal's progress, the loop
/// guard's verdict and the session's usage (spec 3.2 §7.3, §10 HK3).
///
/// `adk-kernel` depends on no harness type (§6 D-1); the `kernel` feature of `adk-harness`
/// implements this over its goal store, loop guard and spend accounts and registers it with
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

    /// The [`RunUsage`] counter of the session `run` continues, or `None` when the agent
    /// keeps none. The kernel reads it when a claim starts and on every scheduler tick, and
    /// attributes the cost between readings to the process ([`ProcessStore::add_spend`]).
    ///
    /// # Errors
    ///
    /// Returns the source's error; the kernel then attributes nothing for that reading.
    async fn usage(&self, _run: &RunRecord) -> Result<Option<RunUsage>> {
        Ok(None)
    }

    /// Called by [`Kernel::start`](crate::Kernel::start) with a handle on the kernel's
    /// processes, so a source that detects a condition that should park a process — a goal
    /// that stopped making progress (spec 3.2 §7.3, §10 HK3) — can suspend it at once.
    fn attach(&self, _processes: ProcessHandle) {}
}

/// A handle on a running kernel's processes that does not keep the kernel alive, given to
/// a [`ProgressSource`] by [`ProgressSource::attach`] (spec 3.2 §7.3).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{ProcessHandle, ProgressSource};
/// use std::sync::OnceLock;
///
/// #[derive(Default)]
/// struct Stalls(OnceLock<ProcessHandle>);
///
/// impl ProgressSource for Stalls {
///     fn attach(&self, processes: ProcessHandle) {
///         let _ = self.0.set(processes);
///     }
/// }
/// ```
#[derive(Clone)]
pub struct ProcessHandle {
    sup: std::sync::Weak<Supervision>,
}

impl fmt::Debug for ProcessHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessHandle").field("alive", &(self.sup.strong_count() > 0)).finish()
    }
}

impl ProcessHandle {
    pub(crate) fn new(sup: &Arc<Supervision>) -> Self {
        Self { sup: Arc::downgrade(sup) }
    }

    /// Suspends `pid` as [`ProcessTable::suspend`] does: a running process parks at its
    /// next model-call boundary with [`Suspension::Manual`]. Returns `false` when the
    /// kernel is gone.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`ProcessTable::suspend`].
    pub async fn suspend(&self, pid: &str) -> Result<bool> {
        let Some(sup) = self.sup.upgrade() else { return Ok(false) };
        ProcessTable::supervised(sup, None).suspend(pid).await?;
        Ok(true)
    }
}

/// The process table: a view over `DurableRunStore` plus the kernel's
/// [`ProcessStore`] (spec 3.2 §7.3).
///
/// [`Kernel::processes`](crate::Kernel::processes) returns the kernel's table, which also
/// lists runs of the kernel's queues the side table has no row for yet, reaches the
/// kernel's workers for `suspend`, and reads transcripts for the call counts; a table built
/// with [`new`](Self::new) reads and writes the stores only.
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
    spend: Option<Arc<dyn SpendLedger>>,
    progress: Option<Arc<dyn ProgressSource>>,
    sup: Option<Arc<Supervision>>,
}

impl fmt::Debug for ProcessTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessTable")
            .field("spend", &self.spend.is_some())
            .field("progress", &self.progress.is_some())
            .field("supervised", &self.sup.is_some())
            .finish_non_exhaustive()
    }
}

fn not_found(pid: &str) -> KernelError {
    KernelError::NotFound { pid: pid.to_string() }
}

impl ProcessTable {
    /// A table over `runs` and `processes`.
    pub fn new(runs: Arc<dyn DurableRunStore>, processes: Arc<dyn ProcessStore>) -> Self {
        Self { runs, processes, spend: None, progress: None, sup: None }
    }

    /// The kernel's table: the stores, progress and workers of `sup`.
    pub(crate) fn supervised(sup: Arc<Supervision>, spend: Option<Arc<dyn SpendLedger>>) -> Self {
        Self {
            runs: Arc::clone(&sup.runs),
            processes: Arc::clone(&sup.processes),
            spend,
            progress: sup.progress.clone(),
            sup: Some(sup),
        }
    }

    /// The ledger the processes' spend is budgeted against. The view reads the kernel's
    /// own daily totals ([`ProcessStore::spend`]) rather than this ledger, which has no
    /// per-run dimension.
    #[must_use]
    pub fn with_spend_ledger(mut self, ledger: Arc<dyn SpendLedger>) -> Self {
        self.spend = Some(ledger);
        self
    }

    /// Reads goal progress, loop-guard verdicts and usage from `source` for each view.
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
    /// processes show their age (spec 3.2 §7.3, §8.1 `adk-rust ps`). Processes are ordered
    /// by creation. A side-table row whose run the run store pruned is deleted. The
    /// kernel's `kernel.wake` runs, which route wake events, are not listed.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] or [`KernelError::Core`] when a store fails.
    pub async fn ps(&self, filter: ProcessFilter) -> Result<Vec<Process>> {
        let mut meta_filter = filter.clone();
        meta_filter.states = Vec::new();
        meta_filter.limit = None;
        let mut rows: BTreeMap<String, (RunRecord, ProcessMeta)> = BTreeMap::new();
        for meta in self.processes.list(&meta_filter).await? {
            match self.runs.get(&meta.pid).await? {
                Some(run) => {
                    rows.insert(meta.pid.clone(), (run, meta));
                }
                None => {
                    self.processes.delete(&meta.pid).await?;
                }
            }
        }
        if let Some(sup) = &self.sup {
            for queue in &sup.queues {
                let mut runs = RunFilter::default().queue(queue.as_str());
                if !filter.states.is_empty() {
                    runs = runs.states(filter.states.iter().copied());
                }
                for run in self.runs.list(runs).await? {
                    if rows.contains_key(&run.run_id) {
                        continue;
                    }
                    let meta = Supervision::infer_meta(&run);
                    if filter.matches(&meta) {
                        rows.insert(run.run_id.clone(), (run, meta));
                    }
                }
            }
        }
        let mut rows: Vec<(RunRecord, ProcessMeta)> = rows
            .into_values()
            .filter(|(run, _)| run.target != crate::wake::KERNEL_WAKE_TARGET)
            .filter(|(run, _)| filter.states.is_empty() || filter.states.contains(&run.state))
            .collect();
        rows.sort_by(|(a, _), (b, _)| {
            a.created_at.cmp(&b.created_at).then(a.run_id.cmp(&b.run_id))
        });
        if let Some(limit) = filter.limit {
            rows.truncate(limit);
        }
        let now = Utc::now();
        let mut processes = Vec::with_capacity(rows.len());
        for (run, meta) in rows {
            let view = self.view(&run, &meta, now).await;
            processes.push(Process { run, meta, view });
        }
        Ok(processes)
    }

    /// The process `pid` with its live view, or `None` when no run has that id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] or [`KernelError::Core`] when a store fails.
    pub async fn get(&self, pid: &str) -> Result<Option<Process>> {
        let Some(run) = self.runs.get(pid).await? else {
            return Ok(None);
        };
        let meta = self.meta(&run).await?;
        let view = self.view(&run, &meta, Utc::now()).await;
        Ok(Some(Process { run, meta, view }))
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
    /// `run` is invalid or its concurrency key refuses it, or a store error.
    pub async fn spawn(&self, mut run: NewRun, spec: ProcessSpec) -> Result<Process> {
        self.refuse_while_frozen()?;
        let asleep = spec.wake.is_some();
        if asleep {
            let grace = Utc::now()
                + chrono::Duration::from_std(SPAWN_GRACE).unwrap_or(chrono::Duration::zero());
            run.not_before = Some(run.not_before.map_or(grace, |at| at.max(grace)));
        }
        let record = match self.runs.enqueue(run).await? {
            Enqueued::Created(record) => record,
            Enqueued::Existing(record) => {
                let meta = self.meta(&record).await?;
                let view = self.view(&record, &meta, Utc::now()).await;
                return Ok(Process { run: record, meta, view });
            }
            Enqueued::Rejected { blocking_run_id } => {
                return Err(AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::Conflict,
                    "kernel.spawn_rejected",
                    format!(
                        "run {blocking_run_id} holds the concurrency key and the run refuses to \
                         queue behind it; wait for it to finish or enqueue with OnConflict::Enqueue"
                    ),
                )
                .into());
            }
        };
        let meta = ProcessMeta {
            pid: record.run_id.clone(),
            parent: spec.parent,
            partition: spec.partition,
            queue: QueueClass::from_name(&record.queue),
            kind: spec.kind,
            wake: spec.wake,
            suspended_by: None,
            suspend_requested: asleep,
            agenda_item: spec.agenda_item,
            labels: spec.labels,
        };
        self.processes.put(meta.clone()).await?;
        if let Some(sup) = &self.sup {
            if asleep {
                sup.roots_changed.notify_one();
            }
            sup.publish(KernelEvent::Process {
                pid: meta.pid.clone(),
                from: RunState::Queued,
                to: RunState::Queued,
                reason: Some(if asleep { "spawned asleep" } else { "spawned" }.to_string()),
            });
            sup.record(
                audit::PROCESS_SPAWNED,
                sup.actor_for(Some(&record)),
                Supervision::process_fields(&meta, None, None),
            )
            .await;
            sup.nudge();
        }
        let view = self.view(&record, &meta, Utc::now()).await;
        Ok(Process { run: record, meta, view })
    }

    /// Kills `pid` through `DurableRunStore::request_cancel`: cooperative at the next
    /// checkpoint, never leaving a lease held (K-I5); a `Queued` or `Waiting` process,
    /// freeze-parked ones included, is `Cancelled` at once (K-I21).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`] for an unknown `pid`, [`KernelError::InvalidState`]
    /// for a finished one, or a store error.
    pub async fn kill(&self, pid: &str, reason: &str) -> Result<()> {
        let run = self.runs.get(pid).await?.ok_or_else(|| not_found(pid))?;
        if run.state.is_terminal() {
            return Err(KernelError::InvalidState {
                pid: pid.to_string(),
                state: run.state,
                action: "killed",
            });
        }
        self.runs.request_cancel(pid).await?;
        let meta = self.meta(&run).await?;
        if run.state != RunState::Running {
            // Cancelled at once, so it will not sleep again; a running one clears its wake
            // set when its attempt finishes.
            let mut cleared = meta.clone();
            if cleared.wake.take().is_some() {
                self.processes.put(cleared).await?;
                if let Some(sup) = &self.sup {
                    sup.roots_changed.notify_one();
                }
            }
        }
        if let Some(sup) = &self.sup {
            sup.heartbeat_now();
            if run.state != RunState::Running {
                sup.publish(KernelEvent::Process {
                    pid: pid.to_string(),
                    from: run.state,
                    to: RunState::Cancelled,
                    reason: Some(reason.to_string()),
                });
            }
            sup.record(
                audit::PROCESS_KILLED,
                sup.actor_for(Some(&run)),
                Supervision::process_fields(&meta, Some(reason), None),
            )
            .await;
        }
        Ok(())
    }

    /// Moves a `Queued` run or a sleeping process (`Waiting` with a wake set) to `queue`
    /// (spec 3.2 §6 D-5) and returns the moved process.
    ///
    /// The run store has no queue change, so `nice` cancels the run and enqueues it again on
    /// `queue`, in the same session, with the same wake set: **the pid changes**. The new
    /// process's [`ProcessMeta::parent`] is the old pid. A sleeper whose turn already reached
    /// the session continues it with an empty user turn, as a resumed run does, so its
    /// prompt is not sent twice.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`] for an unknown `pid`; [`KernelError::InvalidState`]
    /// for a `Running` process (suspend it first), a suspended or approval-parked one, or a
    /// finished one; or a store error.
    pub async fn nice(&self, pid: &str, queue: QueueClass) -> Result<Process> {
        let run = self.runs.get(pid).await?.ok_or_else(|| not_found(pid))?;
        let meta = self.meta(&run).await?;
        let movable = match run.state {
            RunState::Queued => true,
            RunState::Waiting => meta.wake.is_some() && meta.suspended_by.is_none(),
            _ => false,
        };
        if !movable {
            return Err(KernelError::InvalidState {
                pid: pid.to_string(),
                state: run.state,
                action: "moved to another queue (only a queued or sleeping process moves; \
                         suspend a running one first)",
            });
        }
        if meta.queue == queue {
            let view = self.view(&run, &meta, Utc::now()).await;
            return Ok(Process { run, meta, view });
        }
        // Checked before the cancel, so a freeze never leaves the process cancelled and not
        // re-enqueued.
        self.refuse_while_frozen()?;
        self.runs.request_cancel(pid).await?;
        if meta.wake.is_some() {
            let mut cleared = meta.clone();
            cleared.wake = None;
            self.processes.put(cleared).await?;
        }
        let input = if run.checkpoint.is_some() {
            serde_json::Value::String(String::new())
        } else {
            run.input.clone()
        };
        let mut next = NewRun::new(
            queue.as_str(),
            run.app_name.clone(),
            run.user_id.clone(),
            run.target.clone(),
            input,
        )
        .with_max_attempts(run.max_attempts);
        next.session_id = Some(crate::supervise::run_session(&run));
        if let Some(key) = &run.concurrency_key {
            next = next.with_concurrency_key(key.clone(), run.on_conflict);
        }
        next.not_before = run.not_before;
        next.principal = run.principal.clone();
        next.authenticated = run.authenticated;
        let mut spec = ProcessSpec::new(meta.kind.clone()).with_parent(pid);
        spec.partition = meta.partition.clone();
        spec.wake = meta.wake.clone();
        spec.agenda_item = meta.agenda_item.clone();
        spec.labels = meta.labels.clone();
        let moved = self.spawn(next, spec).await?;
        if let Some(sup) = &self.sup {
            sup.record(
                audit::PROCESS_REQUEUED,
                sup.actor_for(Some(&run)),
                json!({
                    "pid": moved.meta.pid,
                    "parent": pid,
                    "partition": moved.meta.partition,
                    "queue": queue,
                    "kind": moved.meta.kind,
                    "from": meta.queue,
                }),
            )
            .await;
        }
        Ok(moved)
    }

    /// Suspends `pid` through the worker's suspend path (§10 S1): it stops at its next
    /// model-call boundary and parks `Waiting` with [`Suspension::Manual`]; a `Queued` or
    /// `Waiting` process is marked suspended where it is.
    ///
    /// A `Running` process gets [`ProcessMeta::suspend_requested`]: every replica's kernel
    /// mirrors pending requests every 250 ms and suspends the runs its own workers lease, so
    /// the request reaches the replica that holds the lease without a run-store change; the
    /// flag is cleared once the run parks.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`] for an unknown `pid`, [`KernelError::InvalidState`]
    /// for a finished one, or a store error.
    pub async fn suspend(&self, pid: &str) -> Result<()> {
        let run = self.runs.get(pid).await?.ok_or_else(|| not_found(pid))?;
        if run.state.is_terminal() {
            return Err(KernelError::InvalidState {
                pid: pid.to_string(),
                state: run.state,
                action: "suspended",
            });
        }
        let mut meta = self.meta(&run).await?;
        if meta.suspended_by.is_none() {
            meta.suspended_by = Some(Suspension::Manual);
        }
        // A running or queued process parks at its next model-call boundary; a waiting
        // one is already parked.
        meta.suspend_requested = run.state != RunState::Waiting;
        self.processes.put(meta.clone()).await?;
        match (run.state, &self.sup) {
            (RunState::Running, Some(sup)) => {
                sup.suspend_local(pid);
            }
            (RunState::Waiting, Some(sup)) => {
                sup.record(
                    audit::PROCESS_SUSPENDED,
                    sup.actor_for(Some(&run)),
                    Supervision::process_fields(&meta, Some("suspended"), None),
                )
                .await;
            }
            _ => {}
        }
        Ok(())
    }

    /// Resumes a suspended or sleeping `pid` through `DurableRunStore::wake`. A process a
    /// freeze parked resumes only on `unfreeze` (K-I21).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::NotFound`] for an unknown `pid`, [`KernelError::InvalidState`]
    /// when it is not `Waiting` or was parked by a freeze, or a store error.
    pub async fn resume(&self, pid: &str) -> Result<()> {
        let run = self.runs.get(pid).await?.ok_or_else(|| not_found(pid))?;
        let mut meta = self.meta(&run).await?;
        let queued_and_suspended = run.state == RunState::Queued && meta.suspend_requested;
        if run.state != RunState::Waiting && !queued_and_suspended {
            return Err(KernelError::InvalidState {
                pid: pid.to_string(),
                state: run.state,
                action: "resumed (it is not suspended or asleep)",
            });
        }
        if meta.suspended_by == Some(Suspension::Freeze) {
            return Err(KernelError::InvalidState {
                pid: pid.to_string(),
                state: run.state,
                action: "resumed before the freeze is lifted; run `adk-rust unfreeze`",
            });
        }
        if run.state == RunState::Waiting {
            self.runs.wake(pid).await?;
        }
        meta.suspended_by = None;
        meta.suspend_requested = false;
        meta.wake = None;
        self.processes.put(meta.clone()).await?;
        if let Some(sup) = &self.sup {
            sup.record(
                audit::PROCESS_RESUMED,
                sup.actor_for(Some(&run)),
                Supervision::process_fields(&meta, Some("resumed"), None),
            )
            .await;
            sup.nudge();
        }
        Ok(())
    }

    /// [`KernelError::Frozen`] while the kernel's control refuses work.
    fn refuse_while_frozen(&self) -> Result<()> {
        match &self.sup {
            Some(sup) => sup.governance.check().map_err(|error| KernelError::Frozen {
                reason: sup.governance.reason().unwrap_or(error.message),
            }),
            None => Ok(()),
        }
    }

    /// The stored side table of `run`, or the inferred one when nothing recorded it.
    async fn meta(&self, run: &RunRecord) -> Result<ProcessMeta> {
        Ok(self.processes.get(&run.run_id).await?.unwrap_or_else(|| Supervision::infer_meta(run)))
    }

    /// The live view of one process (spec 3.2 §7.3, §7.10). A part a store cannot answer is
    /// left at its default rather than failing the listing.
    async fn view(&self, run: &RunRecord, meta: &ProcessMeta, now: DateTime<Utc>) -> ProcessView {
        let mut view = ProcessView::default();
        let local = self.sup.as_ref().and_then(|sup| {
            sup.local.lock().unwrap_or_else(|e| e.into_inner()).get(&run.run_id).cloned()
        });
        let since = match (run.state, &local) {
            (RunState::Running, Some(local)) => local.claimed_at,
            _ => run.updated_at,
        };
        view.age = (now - since).to_std().unwrap_or_default();
        let today = now.date_naive();
        let scope = SpendScope::Process(run.run_id.clone());
        match self.processes.spend(&scope, today, today).await {
            Ok(spend) => view.spend_today = spend,
            Err(error) => {
                tracing::debug!(process.pid = %run.run_id, error = %error, "could not read today's spend")
            }
        }
        match self.processes.spend(&scope, today - chrono::Days::new(6), today).await {
            Ok(spend) => view.spend_7d = spend,
            Err(error) => {
                tracing::debug!(process.pid = %run.run_id, error = %error, "could not read the week's spend")
            }
        }
        if run.state == RunState::Running
            && let Some(local) = &local
            && let (Some((first_at, first)), Some((last_at, last))) =
                (local.samples.front(), local.samples.back())
        {
            let minutes = (*last_at - *first_at).num_milliseconds() as f64 / 60_000.0;
            if minutes > 0.0 {
                let rate = (last.saturating_sub(*first)) as f64 / minutes;
                view.burn_rate = Some(rate);
                let end_of_day =
                    (today + chrono::Days::new(1)).and_time(chrono::NaiveTime::MIN).and_utc();
                let remaining = (end_of_day - now).num_milliseconds().max(0) as f64 / 60_000.0;
                view.projection = Some(view.spend_today + (rate * remaining) as u64);
            }
        }
        let sessions = self.sup.as_ref().and_then(|sup| sup.sessions.clone());
        if let Some(sessions) = sessions
            && let Some(counts) = transcript_counts(sessions.as_ref(), run).await
        {
            view.model_calls = counts.model_calls;
            view.tool_calls = counts.tool_calls;
        } else if let Some(local) = &local
            && let Some(last) = local.last
        {
            let base = local.baseline.unwrap_or_default();
            view.model_calls = last.model_calls.saturating_sub(base.model_calls);
            view.tool_calls = last.tool_calls.saturating_sub(base.tool_calls);
        }
        if let Some(progress) = &self.progress {
            match progress.loop_guard(run).await {
                Ok(verdict) => view.loop_guard = verdict,
                Err(error) => {
                    tracing::debug!(process.pid = %run.run_id, error = %error, "could not read the loop guard")
                }
            }
            if meta.kind == ProcessKind::Goal || run.target == crate::supervise::GOAL_TARGET {
                match progress.goal_progress(run).await {
                    Ok(Some(mut goal)) => {
                        if goal.spend == 0 {
                            let from = run.created_at.date_naive();
                            goal.spend =
                                self.processes.spend(&scope, from, today).await.unwrap_or(0);
                        }
                        view.goal = Some(goal);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::debug!(process.pid = %run.run_id, error = %error, "could not read the goal's progress")
                    }
                }
            }
        }
        view
    }
}
