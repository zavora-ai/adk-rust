//! Sleep and wake (spec 3.2 §7.4).
//!
//! Asleep is `Waiting` with a recorded [`WakeSet`] and no lease (§6 D-4, K-I2). Every wake
//! produces a `NewRun` or wakes a parked run; no wake source calls a tool (§6 D-3, K-I1).
//!
//! How a wake becomes work:
//!
//! 1. A [`WakeEvent`] arrives from a [`WakeSource`]. Every source sets [`WakeEvent::key`],
//!    the idempotency key, as [`keys`] derives it (§13 Q7).
//! 2. The [`WakeRouter`] matches it against the wake sets of `Waiting` processes and against
//!    the kernel's [`Trigger`]s.
//! 3. A matched `Waiting` process is woken with `DurableRunStore::wake` and the wake reason
//!    recorded; a matched trigger enqueues its run with `idempotency_key` = the event key,
//!    `concurrency_key` = the session or partition, the partition's queue, and the principal
//!    as a `Service` subject (`NewRun::with_service_subject`). `NewRun::authenticated` is
//!    set only when the wake carries an authenticated subject. A duplicate delivery hits
//!    the key and wakes nothing (K-I3); the key lives until `DurableRunStore::prune` frees
//!    it, so the prune policy is the dedup window.
//! 4. The run executes under the harness's `configure_run`, as an interactive turn does.
//!
//! | Source | Built on | Key |
//! |---|---|---|
//! | Cron | [`CronWakeSource`] over `CronTrigger` | [`keys::cron`]: `cron:<source>:<scheduled_for>` |
//! | File change | [`FileWakeSource`] over `FileWatchTrigger` | [`keys::file`]: `file:<root>:<path>:<modification time>` |
//! | Webhook | `webhook_wake_hook` over `WebhookIngress` (feature `webhooks`) | [`keys::webhook`]: `hook:<name>:<webhook-id>`, set by the ingress |
//! | Queue | [`queue_wake_ingress`] over `QueueIngress` | [`keys::queue`]: `queue:<source>:<message id>`, set by the ingress |
//! | Schedule | `DurableScheduler` | [`keys::schedule`]: `schedule:<id>:<time>` |
//! | Approval decided | [`ApprovalWakeSource`], an `ApprovalNotifier` on `StoreApprovalService` | [`keys::approval`]: the approval request id |
//! | Run finished | [`RunFinishedWakeSource`], the S1 `on_finish` hook | [`keys::run_finished`]: `run:<pid>:<final state>` |
//!
//! Webhook and queue deliveries become `kernel.wake` runs ([`KERNEL_WAKE_TARGET`]) on the
//! maintenance queue, which the [`KernelWakeExecutor`] turns into a routed wake, so their
//! signature checks, keys and back-pressure hold unchanged.
//!
//! Missed wakes while the kernel was down follow `MissedFirePolicy` for schedules and
//! `MissedTickPolicy` with watermarks for cron sources; file changes are reconciled by
//! [`WakeRouter::reconcile`] on start for every sleeper whose wake set holds a
//! [`Wake::FileChange`]. Parked work has no expiry of its own: a process sleeps until its
//! wake set fires or [`WakeSet::on_expiry`] acts.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_agent::ambient::{CronTrigger, FileWatchTrigger};
use adk_core::approval::{ApprovalNotification, ApprovalNotifier};
use adk_core::queue_source::{QueueIngress, QueueSource};
use adk_core::schedule::MissedFirePolicy;
use adk_core::{DurableRunStore, NewRun, RunRecord};
use adk_server::background::{WorkerHooks, WorkflowExecutor, WorkflowState};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::bus::EventBus;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::stores::ProcessStore;

/// The target of a `kernel.wake` run: a webhook or queue delivery the maintenance worker
/// turns into a routed wake (spec 3.2 §7.4).
pub const KERNEL_WAKE_TARGET: &str = "kernel.wake";

/// One condition that wakes a sleeping process (spec 3.2 §7.4).
///
/// Channel wakes (`Channel { channel, thread }`) join behind the `channels` feature in
/// phase 5.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::Wake;
///
/// let wake = Wake::FileChange { root: ".".into(), glob: "Cargo.lock".into(), debounce: Duration::from_millis(500) };
/// let json = serde_json::to_value(&wake).unwrap();
/// assert_eq!(json["fileChange"]["debounceMs"], 500);
/// assert_eq!(serde_json::to_value(Wake::Manual).unwrap(), "manual");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum Wake {
    /// At a time: `not_before` on the run.
    At(DateTime<Utc>),
    /// On a cron schedule (`adk-core/src/schedule.rs`).
    Cron {
        /// The cron expression.
        expr: String,
        /// Which missed occurrences fire after downtime.
        missed: MissedFirePolicy,
    },
    /// When a file under `root` matching `glob` changes.
    FileChange {
        /// The directory watched.
        root: PathBuf,
        /// The pattern a changed path must match.
        glob: String,
        /// How long changes settle before one wake.
        #[serde(rename = "debounceMs", with = "crate::serde_util::millis")]
        debounce: Duration,
    },
    /// When the named webhook delivers (`adk-server/src/webhooks/ingress.rs` `WebhookHook`).
    Webhook {
        /// The hook's name.
        hook: String,
    },
    /// When the named queue source delivers (`adk-core/src/queue_source.rs`).
    Queue {
        /// The queue source's name.
        source: String,
    },
    /// When the approval of a held call is decided (`RunCheckpointRef::AwaitingApproval`).
    Approval {
        /// The held call's fingerprint.
        fingerprint: String,
    },
    /// When another process finishes.
    RunFinished {
        /// The process waited on.
        pid: String,
    },
    /// When a partition's proactive budget replenishes (spec 3.2 §7.9, phase 2).
    BudgetReplenished {
        /// The partition.
        partition: PartitionId,
    },
    /// Only `adk-rust resume`.
    Manual,
}

/// What happens when a [`WakeSet`] expires before any wake fires (spec 3.2 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_kernel::ExpiryAction;
///
/// assert_eq!(ExpiryAction::default(), ExpiryAction::Cancel);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ExpiryAction {
    /// Cancel the process.
    #[default]
    Cancel,
    /// Run the process as though a wake fired.
    Run,
}

/// The conditions that wake a sleeping process: any one of them (spec 3.2 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{ExpiryAction, Wake, WakeSet};
///
/// let wake = WakeSet::any_of([Wake::Webhook { hook: "github".into() }, Wake::Manual])
///     .with_expiry(chrono::Utc::now() + chrono::Duration::days(7), ExpiryAction::Cancel);
/// assert_eq!(wake.any_of.len(), 2);
/// assert!(wake.expires_at.is_some());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct WakeSet {
    /// The process wakes when any of these fires.
    pub any_of: Vec<Wake>,
    /// When the set expires; `None` never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// What expiry does.
    #[serde(default)]
    pub on_expiry: ExpiryAction,
}

impl WakeSet {
    /// A set that wakes on any of `wakes` and never expires.
    pub fn any_of(wakes: impl IntoIterator<Item = Wake>) -> Self {
        Self {
            any_of: wakes.into_iter().collect(),
            expires_at: None,
            on_expiry: ExpiryAction::default(),
        }
    }

    /// Expires the set at `at`, applying `on_expiry`.
    #[must_use]
    pub fn with_expiry(mut self, at: DateTime<Utc>, on_expiry: ExpiryAction) -> Self {
        self.expires_at = Some(at);
        self.on_expiry = on_expiry;
        self
    }
}

/// A source of wake events: an adapter over `adk_agent::ambient::EventSource` where one
/// exists (spec 3.2 §7.4).
///
/// Every source sets [`WakeEvent::key`] (§13 Q7). A third-party source with a natural
/// delivery id, such as a provider's event id, implements this and sets the key from it.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{WakeEvent, WakeSource};
/// use async_trait::async_trait;
/// use futures::stream::{self, BoxStream, StreamExt};
///
/// struct Deliveries(Vec<(String, serde_json::Value)>);
///
/// #[async_trait]
/// impl WakeSource for Deliveries {
///     fn name(&self) -> &str {
///         "provider"
///     }
///
///     async fn subscribe(&self) -> adk_kernel::Result<BoxStream<'static, WakeEvent>> {
///         let events: Vec<WakeEvent> = self
///             .0
///             .iter()
///             .map(|(id, payload)| WakeEvent::new("provider", format!("provider:{id}"), payload.clone()))
///             .collect();
///         Ok(stream::iter(events).boxed())
///     }
/// }
/// ```
#[async_trait]
pub trait WakeSource: Send + Sync {
    /// The source's name, recorded as [`WakeEvent::source`].
    fn name(&self) -> &str;

    /// Subscribes to the source's events until the stream is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the source cannot start, such as a watch on a missing
    /// directory.
    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>>;
}

/// One wake delivered by a [`WakeSource`] (spec 3.2 §7.4).
///
/// # Example
///
/// ```rust
/// use adk_kernel::WakeEvent;
///
/// let event = WakeEvent::new("hook:github", "hook:github:msg_1", serde_json::json!({"action": "opened"}))
///     .with_principal("github-app");
/// assert_eq!(event.key, "hook:github:msg_1");
/// assert_eq!(serde_json::to_value(&event).unwrap()["principal"], "github-app");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct WakeEvent {
    /// The source's name.
    pub source: String,
    /// The idempotency key, set by the source ([`keys`]).
    pub key: String,
    /// Source-specific detail: the changed path, the webhook body, the decided request.
    pub payload: Value,
    /// The verified subject the event is attributed to, when the source authenticates one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// When the event happened.
    pub at: DateTime<Utc>,
}

impl WakeEvent {
    /// An event from `source` keyed `key`, happening now, with no principal.
    pub fn new(source: impl Into<String>, key: impl Into<String>, payload: Value) -> Self {
        Self { source: source.into(), key: key.into(), payload, principal: None, at: Utc::now() }
    }

    /// Attributes the event to the verified subject `principal`.
    #[must_use]
    pub fn with_principal(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }

    /// Sets when the event happened.
    #[must_use]
    pub fn with_at(mut self, at: DateTime<Utc>) -> Self {
        self.at = at;
        self
    }
}

/// The idempotency key of each wake source (spec 3.2 §7.4 key table, §13 Q7).
///
/// # Example
///
/// ```rust
/// use chrono::{TimeZone, Utc};
/// use adk_kernel::keys;
///
/// let at = Utc.with_ymd_and_hms(2026, 10, 22, 9, 0, 0).unwrap();
/// assert_eq!(keys::cron("nightly", at), "cron:nightly:2026-10-22T09:00:00Z");
/// assert_eq!(keys::schedule("nightly-report", at), "schedule:nightly-report:2026-10-22T09:00:00Z");
/// assert_eq!(keys::webhook("github", "msg_1"), "hook:github:msg_1");
/// assert_eq!(keys::queue("orders", "m-1"), "queue:orders:m-1");
/// assert_eq!(keys::run_finished("run-7", adk_core::RunState::Completed), "run:run-7:completed");
/// ```
pub mod keys {
    use std::path::Path;

    use adk_core::RunState;
    use chrono::{DateTime, SecondsFormat, Utc};

    /// `cron:<source>:<scheduled_for>`; every tick's payload carries `scheduled_for`,
    /// catch-up ticks included.
    pub fn cron(source: &str, scheduled_for: DateTime<Utc>) -> String {
        format!("cron:{source}:{}", scheduled_for.to_rfc3339_opts(SecondsFormat::AutoSi, true))
    }

    /// `file:<root>:<path>:<modification time>`, the time read when the event arrives, so
    /// the several notifications one save produces wake once and a later save, even back
    /// to earlier contents, wakes again.
    pub fn file(root: &Path, path: &Path, modified: DateTime<Utc>) -> String {
        format!(
            "file:{}:{}:{}",
            root.display(),
            path.display(),
            modified.to_rfc3339_opts(SecondsFormat::Nanos, true)
        )
    }

    /// `hook:<name>:<webhook-id>`, as `WebhookHook::idempotency_key` sets it.
    pub fn webhook(hook: &str, webhook_id: &str) -> String {
        format!("hook:{hook}:{webhook_id}")
    }

    /// `queue:<source>:<message id>`, as `QueueIngress::idempotency_key` sets it.
    pub fn queue(source: &str, message_id: &str) -> String {
        adk_core::queue_source::QueueIngress::idempotency_key(source, message_id)
    }

    /// `schedule:<id>:<time>`, as `Schedule::run_for` sets it.
    pub fn schedule(schedule_id: &str, fire_time: DateTime<Utc>) -> String {
        adk_core::firing_idempotency_key(schedule_id, fire_time)
    }

    /// The approval request id.
    pub fn approval(request_id: &str) -> String {
        request_id.to_string()
    }

    /// `run:<pid>:<final state>`, the state as the run store serializes it (`completed`,
    /// `failed`, `cancelled`).
    pub fn run_finished(pid: &str, state: RunState) -> String {
        let state = serde_json::to_value(state)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{state:?}").to_lowercase());
        format!("run:{pid}:{state}")
    }
}

/// Builds the `kernel.wake` run a webhook or queue delivery enqueues: target
/// [`KERNEL_WAKE_TARGET`] on the maintenance queue, keyed by `event.key`, with the event as
/// input, acting as a `Service` subject (spec 3.2 §7.4).
///
/// # Errors
///
/// Returns [`KernelError::Serde`] when the event cannot be encoded.
pub fn kernel_wake_run(_event: &WakeEvent, _app_name: &str) -> Result<NewRun> {
    Err(KernelError::Unimplemented { item: "wake::kernel_wake_run" })
}

/// A standing wake that starts a new process each time it fires, when no sleeping process
/// matched (spec 3.2 §7.4 step 3).
///
/// `run` is the template: the router sets its idempotency key, concurrency key, service
/// subject and `authenticated` from the event.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::NewRun;
/// use adk_kernel::{Trigger, Wake};
///
/// let trigger = Trigger::new(
///     "cargo-lock",
///     Wake::FileChange { root: ".".into(), glob: "Cargo.lock".into(), debounce: Duration::from_millis(500) },
///     NewRun::new("background", "adk-rust", "alice", "adk-rust", serde_json::json!({"text": "audit the dependency change"})),
/// )
/// .with_partition("work");
/// assert_eq!(trigger.partition.as_str(), "work");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Trigger {
    /// The trigger's name, recorded as the process kind's source.
    pub name: String,
    /// What fires it.
    pub wake: Wake,
    /// The run each firing enqueues.
    pub run: NewRun,
    /// The partition its processes run in.
    #[serde(default)]
    pub partition: PartitionId,
}

impl Trigger {
    /// A trigger named `name` that enqueues `run` when `wake` fires, in the default
    /// partition.
    pub fn new(name: impl Into<String>, wake: Wake, run: NewRun) -> Self {
        Self { name: name.into(), wake, run, partition: PartitionId::default() }
    }

    /// Sets the partition its processes run in.
    #[must_use]
    pub fn with_partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = partition.into();
        self
    }
}

/// What one routed [`WakeEvent`] did to one process (spec 3.2 §7.4, §9.3 `kernel.wake`).
///
/// # Example
///
/// ```rust
/// use adk_kernel::WakeOutcome;
///
/// let outcome = WakeOutcome::Deduplicated { pid: "run-3".into() };
/// assert_eq!(outcome.pid(), "run-3");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum WakeOutcome {
    /// A sleeping process matched and was woken.
    Woke {
        /// The woken process.
        pid: String,
    },
    /// A trigger matched and enqueued a new process.
    Spawned {
        /// The new process.
        pid: String,
        /// The trigger's name.
        trigger: String,
    },
    /// The event's key was already used; nothing new ran (K-I3).
    Deduplicated {
        /// The process the key already names.
        pid: String,
    },
    /// A wake set expired and its [`ExpiryAction`] applied.
    Expired {
        /// The process.
        pid: String,
        /// What expiry did.
        action: ExpiryAction,
    },
}

impl WakeOutcome {
    /// The process the outcome concerns.
    pub fn pid(&self) -> &str {
        match self {
            Self::Woke { pid }
            | Self::Spawned { pid, .. }
            | Self::Deduplicated { pid }
            | Self::Expired { pid, .. } => pid,
        }
    }
}

/// The wake matcher (spec 3.2 §7.4 steps 2–3): routes each [`WakeEvent`] to the sleeping
/// processes and [`Trigger`]s it matches.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_kernel::{InMemoryProcessStore, WakeEvent, WakeRouter};
///
/// # async fn example() -> adk_kernel::Result<()> {
/// let router = WakeRouter::new(Arc::new(InMemoryRunStore::new()), Arc::new(InMemoryProcessStore::new()));
/// let outcomes = router.route(WakeEvent::new("hook:github", "hook:github:msg_1", serde_json::json!({}))).await?;
/// # let _ = outcomes;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct WakeRouter {
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<dyn ProcessStore>,
    triggers: Vec<Trigger>,
    // Unimplemented: read by `route` once feat/kernel-wake lands (spec 3.2 §7.4).
    #[allow(dead_code)]
    bus: Option<EventBus>,
}

impl fmt::Debug for WakeRouter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WakeRouter").field("triggers", &self.triggers).finish_non_exhaustive()
    }
}

impl WakeRouter {
    /// A router over the run store and the process side table, with no triggers.
    pub fn new(runs: Arc<dyn DurableRunStore>, processes: Arc<dyn ProcessStore>) -> Self {
        Self { runs, processes, triggers: Vec::new(), bus: None }
    }

    /// Adds a standing trigger.
    #[must_use]
    pub fn with_trigger(mut self, trigger: Trigger) -> Self {
        self.triggers.push(trigger);
        self
    }

    /// Publishes a `KernelEvent::Wake` on `bus` for every routed event.
    #[must_use]
    pub fn with_bus(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// The router's triggers.
    pub fn triggers(&self) -> &[Trigger] {
        &self.triggers
    }

    /// The run store the router wakes and enqueues into.
    pub fn run_store(&self) -> &Arc<dyn DurableRunStore> {
        &self.runs
    }

    /// The side table the router reads wake sets from.
    pub fn process_store(&self) -> &Arc<dyn ProcessStore> {
        &self.processes
    }

    /// Routes `event`: wakes each `Waiting` process whose wake set it matches and enqueues
    /// the run of each matching trigger, keyed by `event.key`; returns what it did, one
    /// outcome per process. An event that matches nothing returns an empty list.
    ///
    /// # Errors
    ///
    /// Returns a store error; a partially routed event is safe to route again, since its
    /// key deduplicates what already ran.
    pub async fn route(&self, _event: WakeEvent) -> Result<Vec<WakeOutcome>> {
        Err(KernelError::Unimplemented { item: "wake::WakeRouter::route" })
    }

    /// Applies the [`ExpiryAction`] of every wake set that expired by `now`.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn sweep_expired(&self, _now: DateTime<Utc>) -> Result<Vec<WakeOutcome>> {
        Err(KernelError::Unimplemented { item: "wake::WakeRouter::sweep_expired" })
    }

    /// Wakes every sleeper whose [`Wake::FileChange`] matches a file modified since the
    /// process went to sleep, for the changes missed while the kernel was down.
    ///
    /// # Errors
    ///
    /// Returns a store or file-system error.
    pub async fn reconcile(&self) -> Result<Vec<WakeOutcome>> {
        Err(KernelError::Unimplemented { item: "wake::WakeRouter::reconcile" })
    }
}

/// Executes `kernel.wake` runs on the maintenance worker: decodes the [`WakeEvent`] a
/// webhook or queue delivery carried and routes it (spec 3.2 §7.4).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_kernel::{InMemoryProcessStore, KernelWakeExecutor, WakeRouter, KERNEL_WAKE_TARGET};
/// use adk_server::background::WorkflowExecutor;
///
/// let router = WakeRouter::new(Arc::new(InMemoryRunStore::new()), Arc::new(InMemoryProcessStore::new()));
/// let executor = KernelWakeExecutor::new(router);
/// assert!(executor.has_workflow(KERNEL_WAKE_TARGET));
/// ```
#[derive(Debug, Clone)]
pub struct KernelWakeExecutor {
    router: WakeRouter,
}

impl KernelWakeExecutor {
    /// An executor that routes through `router`.
    pub fn new(router: WakeRouter) -> Self {
        Self { router }
    }

    /// The router wakes go through.
    pub fn router(&self) -> &WakeRouter {
        &self.router
    }
}

#[async_trait]
impl WorkflowExecutor for KernelWakeExecutor {
    fn has_workflow(&self, workflow_id: &str) -> bool {
        workflow_id == KERNEL_WAKE_TARGET
    }

    async fn execute(
        &self,
        _workflow_id: &str,
        _input: WorkflowState,
        _cancel_token: CancellationToken,
    ) -> std::result::Result<Value, String> {
        Err(KernelError::Unimplemented { item: "wake::KernelWakeExecutor::execute" }.to_string())
    }
}

/// A cron wake source over `adk_agent::ambient::CronTrigger` (spec 3.2 §7.4); keys
/// [`keys::cron`].
///
/// # Example
///
/// ```rust
/// use adk_agent::ambient::{CronTrigger, MissedTickPolicy};
/// use adk_kernel::{CronWakeSource, WakeSource};
///
/// let trigger = CronTrigger::new("0 0 2 * * *").unwrap().with_missed_tick_policy(MissedTickPolicy::CoalesceOne);
/// let source = CronWakeSource::new("nightly", trigger);
/// assert_eq!(source.name(), "nightly");
/// ```
pub struct CronWakeSource {
    name: String,
    // Unimplemented: read by `subscribe` once feat/kernel-wake lands (spec 3.2 §7.4).
    #[allow(dead_code)]
    trigger: CronTrigger,
}

impl fmt::Debug for CronWakeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CronWakeSource").field("name", &self.name).finish_non_exhaustive()
    }
}

impl CronWakeSource {
    /// A source named `name` over `trigger`, with its missed-tick policy and watermark.
    pub fn new(name: impl Into<String>, trigger: CronTrigger) -> Self {
        Self { name: name.into(), trigger }
    }
}

#[async_trait]
impl WakeSource for CronWakeSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>> {
        Err(KernelError::Unimplemented { item: "wake::CronWakeSource::subscribe" })
    }
}

/// A file-change wake source over `adk_agent::ambient::FileWatchTrigger` (spec 3.2 §7.4);
/// keys [`keys::file`], reading the modification time when the event arrives.
///
/// # Example
///
/// ```rust,no_run
/// use std::time::Duration;
/// use adk_kernel::{FileWakeSource, WakeSource};
///
/// let source = FileWakeSource::new("lockfile", ".", "Cargo.lock").unwrap()
///     .with_debounce(Duration::from_millis(500));
/// assert_eq!(source.name(), "lockfile");
/// ```
pub struct FileWakeSource {
    name: String,
    root: PathBuf,
    debounce: Duration,
    // Unimplemented: read by `subscribe` once feat/kernel-wake lands (spec 3.2 §7.4).
    #[allow(dead_code)]
    trigger: FileWatchTrigger,
}

impl fmt::Debug for FileWakeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileWakeSource")
            .field("name", &self.name)
            .field("root", &self.root)
            .field("debounce", &self.debounce)
            .finish_non_exhaustive()
    }
}

impl FileWakeSource {
    /// The default debounce: changes within 500 ms of each other wake once.
    pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(500);

    /// A source named `name` watching `root` for paths matching `glob`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Core`] when `glob` is not a valid pattern.
    pub fn new(name: impl Into<String>, root: impl Into<PathBuf>, glob: &str) -> Result<Self> {
        let root = root.into();
        let trigger = FileWatchTrigger::new(root.clone(), glob)?;
        Ok(Self { name: name.into(), root, debounce: Self::DEFAULT_DEBOUNCE, trigger })
    }

    /// Sets how long changes settle before one wake.
    #[must_use]
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// The directory watched.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How long changes settle before one wake.
    pub fn debounce(&self) -> Duration {
        self.debounce
    }
}

#[async_trait]
impl WakeSource for FileWakeSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>> {
        Err(KernelError::Unimplemented { item: "wake::FileWakeSource::subscribe" })
    }
}

/// Wakes on decided approvals (spec 3.2 §7.4): an `ApprovalNotifier` registered on
/// `StoreApprovalService::with_notifier` whose decisions it publishes as wake events keyed
/// [`keys::approval`].
///
/// `StoreApprovalService::with_run_store` already wakes the run a decision resolves; this
/// source lets [`Wake::Approval`] sleepers and the event bus see the decision too.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{InMemoryApprovalStore, StoreApprovalService};
/// use adk_kernel::ApprovalWakeSource;
///
/// let source = Arc::new(ApprovalWakeSource::new());
/// let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
///     .with_notifier(source.clone());
/// # let _ = service;
/// ```
#[derive(Debug)]
pub struct ApprovalWakeSource {
    name: String,
}

impl ApprovalWakeSource {
    /// The source's name.
    pub const NAME: &'static str = "approval";

    /// A source named [`NAME`](Self::NAME).
    pub fn new() -> Self {
        Self { name: Self::NAME.to_string() }
    }
}

impl Default for ApprovalWakeSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ApprovalNotifier for ApprovalWakeSource {
    async fn notify(&self, _notification: &ApprovalNotification) -> adk_core::Result<()> {
        Err(KernelError::Unimplemented { item: "wake::ApprovalWakeSource::notify" }.into())
    }
}

#[async_trait]
impl WakeSource for ApprovalWakeSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>> {
        Err(KernelError::Unimplemented { item: "wake::ApprovalWakeSource::subscribe" })
    }
}

/// Wakes on finished processes (spec 3.2 §7.4): fed by the worker's S1 `on_finish` hook,
/// it publishes each finished run as a wake event keyed [`keys::run_finished`].
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_kernel::RunFinishedWakeSource;
/// use adk_server::background::DurableWorker;
///
/// let source = Arc::new(RunFinishedWakeSource::new());
/// let worker = DurableWorker::new(Arc::new(InMemoryRunStore::new())).with_hooks(source.clone());
/// # let _ = worker;
/// ```
#[derive(Debug)]
pub struct RunFinishedWakeSource {
    name: String,
}

impl RunFinishedWakeSource {
    /// The source's name.
    pub const NAME: &'static str = "run-finished";

    /// A source named [`NAME`](Self::NAME).
    pub fn new() -> Self {
        Self { name: Self::NAME.to_string() }
    }
}

impl Default for RunFinishedWakeSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WorkerHooks for RunFinishedWakeSource {
    async fn on_finish(&self, run: &RunRecord) {
        // Unimplemented: wake::RunFinishedWakeSource::on_finish publishes a
        // `keys::run_finished` wake event once feat/kernel-wake lands (spec 3.2 §7.4).
        tracing::trace!(run.id = %run.run_id, "run finished");
    }
}

#[async_trait]
impl WakeSource for RunFinishedWakeSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>> {
        Err(KernelError::Unimplemented { item: "wake::RunFinishedWakeSource::subscribe" })
    }
}

/// Builds the ingress that turns each message of `source` into a `kernel.wake` run keyed
/// [`keys::queue`] (spec 3.2 §7.4); the kernel runs it with `QueueIngress::run`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_core::queue_source::InMemoryQueueSource;
///
/// let ingress = adk_kernel::queue_wake_ingress(
///     Arc::new(InMemoryQueueSource::new("orders")),
///     Arc::new(InMemoryRunStore::new()),
///     "adk-rust",
/// );
/// # let _ = ingress;
/// ```
pub fn queue_wake_ingress(
    source: Arc<dyn QueueSource>,
    runs: Arc<dyn DurableRunStore>,
    app_name: impl Into<String>,
) -> QueueIngress {
    let app_name = app_name.into();
    let name = source.name().to_string();
    QueueIngress::new(source, runs, move |message| {
        let payload = message.body_json().unwrap_or(Value::Null);
        let event = WakeEvent::new(name.clone(), keys::queue(&name, &message.id), payload);
        kernel_wake_run(&event, &app_name).map_err(Into::into)
    })
}

/// Builds the webhook hook served at `/hooks/{name}` whose verified deliveries become
/// `kernel.wake` runs keyed [`keys::webhook`] by the ingress (spec 3.2 §7.4, feature
/// `webhooks`). Register it with `WebhookIngress::with_hook`.
///
/// # Example
///
/// ```rust
/// use adk_core::webhook::{WebhookSecret, WebhookVerifier};
///
/// let secret = WebhookSecret::from_bytes(b"a shared signing key".to_vec()).unwrap();
/// let hook = adk_kernel::webhook_wake_hook("github", WebhookVerifier::new(secret), "adk-rust");
/// assert_eq!(hook.name(), "github");
/// ```
#[cfg(feature = "webhooks")]
pub fn webhook_wake_hook(
    name: impl Into<String>,
    verifier: adk_core::webhook::WebhookVerifier,
    app_name: impl Into<String>,
) -> adk_server::webhooks::WebhookHook {
    let app_name = app_name.into();
    adk_server::webhooks::WebhookHook::new(name, verifier, app_name.clone(), KERNEL_WAKE_TARGET)
        .with_queue(crate::sched::QueueClass::MAINTENANCE)
        .with_mapper(move |delivery| {
            let event = WakeEvent::new(
                format!("hook:{}", delivery.hook),
                keys::webhook(delivery.hook, &delivery.webhook.id),
                delivery.payload.clone(),
            );
            kernel_wake_run(&event, &app_name).map_err(Into::into)
        })
}

#[cfg(test)]
mod tests {
    use chrono::SecondsFormat;

    use super::*;

    #[test]
    fn keys_match_the_ingress_and_scheduler_formats() {
        let at = DateTime::parse_from_rfc3339("2026-10-22T09:00:00Z").unwrap().with_timezone(&Utc);
        let schedule = adk_core::Schedule::new(
            "s",
            adk_core::ScheduleSpec::At(at),
            NewRun::new("q", "app", "user", "agent", Value::Null),
            MissedFirePolicy::FireOnce,
        );
        assert_eq!(Some(keys::schedule("s", at)), schedule.run_for(at).idempotency_key);
        assert_eq!(
            keys::cron("nightly", at),
            format!("cron:nightly:{}", at.to_rfc3339_opts(SecondsFormat::AutoSi, true))
        );
        assert_eq!(
            keys::file(Path::new("/w"), Path::new("/w/Cargo.lock"), at),
            "file:/w:/w/Cargo.lock:2026-10-22T09:00:00.000000000Z"
        );
        assert_eq!(keys::queue("orders", "m-1"), QueueIngress::idempotency_key("orders", "m-1"));
    }
}
