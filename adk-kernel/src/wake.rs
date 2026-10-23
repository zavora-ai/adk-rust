//! Sleep and wake (spec 3.2 §7.4).
//!
//! Asleep is `Waiting` with a recorded [`WakeSet`] and no lease (§6 D-4, K-I2). Every wake
//! produces a `NewRun` or wakes a parked run; no wake source calls a tool (§6 D-3, K-I1).
//!
//! How a wake becomes work:
//!
//! 1. A [`WakeEvent`] arrives from a [`WakeSource`]. Every source sets [`WakeEvent::key`],
//!    the idempotency key, as [`keys`] derives it (§13 Q7).
//! 2. The event becomes its keyed `kernel.wake` maintenance run ([`kernel_wake_run`]):
//!    in-process sources through [`WakeRouter::submit`], webhooks and queues through their
//!    ingress. The run store keeps one run per key, so a duplicate delivery reaches the
//!    router never (K-I3); the key lives until `DurableRunStore::prune` frees it, so the
//!    prune policy is the dedup window.
//! 3. The maintenance worker's [`KernelWakeExecutor`] hands the event to the
//!    [`WakeRouter`], which matches it against the wake sets of `Waiting` processes and
//!    against the kernel's [`Trigger`]s.
//! 4. A matched `Waiting` process is woken with `DurableRunStore::wake` and the wake reason
//!    recorded. When no sleeper matched, each matched trigger enqueues its run with
//!    `idempotency_key` = [`keys::trigger`] (the event key and the trigger's name),
//!    `concurrency_key` = the session or partition, the trigger's queue, and the principal
//!    as a `Service` subject (`NewRun::with_service_subject`). `NewRun::authenticated` is
//!    set only when the wake carries an authenticated subject.
//! 5. The run executes under the harness's `configure_run`, as an interactive turn does.
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
//! A trigger's run is keyed [`keys::trigger`], `<event key>#<trigger name>`, rather than by
//! the bare event key of the §7.4 table, so two triggers that match one event on one queue
//! each run; the `kernel.wake` run keeps the bare key.
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

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adk_agent::ambient::{CronTrigger, EventSource, FileWatchTrigger};
use adk_core::approval::{ApprovalEventKind, ApprovalNotification, ApprovalNotifier};
use adk_core::queue_source::{QueueIngress, QueueSource};
use adk_core::schedule::MissedFirePolicy;
use adk_core::{Clock, DurableRunStore, Enqueued, NewRun, RunRecord, RunState, SystemClock};
use adk_server::background::{WorkerHooks, WorkflowExecutor, WorkflowState};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::bus::{EventBus, KernelEvent};
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::{ProcessKind, ProcessMeta};
use crate::sched::QueueClass;
use crate::stores::{ProcessFilter, ProcessStore};

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
/// assert_eq!(keys::trigger("hook:github:msg_1", "triage"), "hook:github:msg_1#triage");
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

    /// `<event key>#<trigger name>`: the run a [`Trigger`](crate::Trigger) enqueues for an
    /// event, so two triggers that match one event on one queue each run.
    pub fn trigger(event_key: &str, trigger: &str) -> String {
        format!("{event_key}#{trigger}")
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

    /// `budget:<partition>:<UTC day>`: the partition's budget replenished on that day
    /// (spec 3.2 §7.9), at most one wake per partition and day.
    pub fn budget(partition: &str, day: chrono::NaiveDate) -> String {
        format!("budget:{partition}:{day}")
    }
}

/// Builds the `kernel.wake` run a webhook or queue delivery enqueues: target
/// [`KERNEL_WAKE_TARGET`] on the maintenance queue, keyed by `event.key`, with the event as
/// input, acting as a `Service` subject (spec 3.2 §7.4).
///
/// The run's user is the event's principal, or the source's name when the event carries
/// none. `NewRun::authenticated` is set only when the event carries a principal, which a
/// source sets only for a subject it verified (spec 3.1 I6).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{KERNEL_WAKE_TARGET, QueueClass, WakeEvent, kernel_wake_run};
///
/// let event = WakeEvent::new("hook:github", "hook:github:msg_1", serde_json::json!({"action": "opened"}));
/// let run = kernel_wake_run(&event, "adk-rust").unwrap();
/// assert_eq!(run.target, KERNEL_WAKE_TARGET);
/// assert_eq!(run.queue, QueueClass::MAINTENANCE);
/// assert_eq!(run.idempotency_key.as_deref(), Some("hook:github:msg_1"));
/// assert!(!run.authenticated);
/// assert_eq!(run.input["payload"]["action"], "opened");
/// ```
///
/// # Errors
///
/// Returns [`KernelError::Serde`] when the event cannot be encoded, or
/// [`KernelError::Core`] when the run is invalid, such as an event with an empty key or
/// source.
pub fn kernel_wake_run(event: &WakeEvent, app_name: &str) -> Result<NewRun> {
    let user = event.principal.clone().unwrap_or_else(|| event.source.clone());
    let mut run = NewRun::new(
        QueueClass::MAINTENANCE,
        app_name,
        user,
        KERNEL_WAKE_TARGET,
        serde_json::to_value(event)?,
    )
    .with_idempotency_key(event.key.clone());
    if event.principal.is_some() {
        run = run.with_authenticated_user();
    }
    let run = run.with_service_subject();
    run.validate()?;
    Ok(run)
}

/// Whether `wake` fires for `event` (spec 3.2 §7.4 step 2).
///
/// Each kernel source keys its events with its own prefix ([`keys`]); the payload carries
/// what a variant compares. `At` and `Manual` match no event: a time wake is swept by
/// [`WakeRouter::sweep_expired`], and a manual wake is `adk-rust resume`. A budget wake
/// matches the kernel's `budget:` event for its partition (spec 3.2 §7.9).
fn wake_matches(wake: &Wake, event: &WakeEvent) -> bool {
    let text = |field: &str| event.payload.get(field).and_then(Value::as_str);
    match wake {
        Wake::Cron { expr, missed } => {
            let catch_up = event.payload.get("catch_up").and_then(Value::as_bool) == Some(true);
            event.key.starts_with("cron:")
                && text("expression") == Some(expr.as_str())
                && !(catch_up && *missed == MissedFirePolicy::Skip)
        }
        Wake::FileChange { root, glob, .. } => {
            event.key.starts_with("file:")
                && text("path").is_some_and(|path| path_matches(root, glob, Path::new(path)))
        }
        Wake::Webhook { hook } => {
            event.key.starts_with("hook:") && event.source == format!("hook:{hook}")
        }
        Wake::Queue { source } => event.key.starts_with("queue:") && event.source == *source,
        Wake::Approval { fingerprint } => {
            event.source == ApprovalWakeSource::NAME && text("fingerprint") == Some(fingerprint)
        }
        Wake::RunFinished { pid } => {
            event.key.starts_with("run:") && text("pid") == Some(pid.as_str())
        }
        Wake::BudgetReplenished { partition } => {
            event.key.starts_with("budget:") && text("partition") == Some(partition.as_str())
        }
        Wake::At(_) | Wake::Manual => false,
    }
}

/// Whether `path` lies under `root` and its file name matches `glob`, as
/// `FileWatchTrigger` matches: `*` is any run of characters, `?` one character.
fn path_matches(root: &Path, glob: &str, path: &Path) -> bool {
    let under = path.starts_with(root)
        || std::fs::canonicalize(root).is_ok_and(|root| path.starts_with(root));
    under
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| glob_matches(glob, name))
}

/// `*` and `?` glob matching over characters, without backtracking blow-up.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// `path` with symbolic links and `.` resolved, so keys and matches agree however a watcher
/// reports it; a removed file resolves through its parent.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        match (path.parent().map(std::fs::canonicalize), path.file_name()) {
            (Some(Ok(parent)), Some(name)) => parent.join(name),
            _ => path.to_path_buf(),
        }
    })
}

/// A standing wake that starts a new process each time it fires, when no sleeping process
/// matched (spec 3.2 §7.4 step 3): a sleeper consumes the event, a trigger is the fallback.
///
/// `run` is the template: the router sets its idempotency key ([`keys::trigger`]),
/// concurrency key, service subject and `authenticated` from the event.
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

/// The source of the [`KernelEvent::Wake`] a wake set's expiry publishes.
const EXPIRY_SOURCE: &str = "expiry";
/// The source of the [`KernelEvent::Wake`] a passed [`Wake::At`] publishes.
const AT_SOURCE: &str = "at";
/// The source of the [`KernelEvent::Wake`] [`WakeRouter::reconcile`] publishes.
const RECONCILE_SOURCE: &str = "reconcile";
/// The most directory entries one [`WakeRouter::reconcile`] scan of a root visits.
const RECONCILE_SCAN_LIMIT: usize = 100_000;

/// The wake matcher (spec 3.2 §7.4 steps 2–3): routes each [`WakeEvent`] to the sleeping
/// processes and [`Trigger`]s it matches.
///
/// | Match | What the router does | Outcome |
/// |---|---|---|
/// | A `Waiting` process whose wake set holds a matching [`Wake`] | `DurableRunStore::wake`, then clears the wake set | [`WakeOutcome::Woke`] |
/// | No sleeper matched; a [`Trigger`] whose wake matches | Enqueues the trigger's run keyed [`keys::trigger`] and records its process | [`WakeOutcome::Spawned`], or [`WakeOutcome::Deduplicated`] when the key already holds a run |
///
/// Events reach [`route`](Self::route) through their keyed `kernel.wake` run
/// ([`submit`](Self::submit), the webhook and queue ingresses), so a duplicate delivery
/// never reaches it, and a key that woke a sleeper never later starts a trigger run (K-I3).
/// A woken process's wake set is cleared, so a later event wakes it only once it sleeps
/// again. A process that is suspended
/// ([`ProcessMeta::suspended_by`]), not `Waiting`, or whose set expired is not woken; a
/// freeze-parked process wakes only on `unfreeze` (K-I21). Routing on one router is
/// serialized, so two sources delivering the same key at once wake once.
///
/// Every routed event is published on the bus as a [`KernelEvent::Wake`] per outcome, or
/// one with no process when it matched nothing.
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
    bus: Option<EventBus>,
    gate: Arc<tokio::sync::Mutex<()>>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for WakeRouter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WakeRouter").field("triggers", &self.triggers).finish_non_exhaustive()
    }
}

impl WakeRouter {
    /// A router over the run store and the process side table, with no triggers.
    pub fn new(runs: Arc<dyn DurableRunStore>, processes: Arc<dyn ProcessStore>) -> Self {
        Self {
            runs,
            processes,
            triggers: Vec::new(),
            bus: None,
            gate: Arc::new(tokio::sync::Mutex::new(())),
            clock: Arc::new(SystemClock),
        }
    }

    /// Reads wake-set expiry against `clock`: the kernel's [`KernelConfig::clock`](crate::KernelConfig::clock).
    pub(crate) fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
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

    /// Routes `event`: wakes each `Waiting` process whose wake set it matches; when it woke
    /// none, enqueues the run of each matching trigger; returns what it did, one outcome per
    /// process. An event that matches nothing returns an empty list.
    ///
    /// The [`KernelWakeExecutor`] calls this for each `kernel.wake` run; a source hands its
    /// events to [`submit`](Self::submit) instead, so each key is routed once.
    ///
    /// A trigger's run is its template with `idempotency_key` = [`keys::trigger`], the
    /// concurrency key `<session id>` or `partition:<partition>` when the template sets
    /// none, acting as a `Service` subject (`NewRun::with_service_subject`). With an event
    /// principal the run's user is the principal and `NewRun::authenticated` is set;
    /// without one `authenticated` is cleared, so standing rules and Auto stay inert for the
    /// run (spec 3.1 I6).
    ///
    /// # Errors
    ///
    /// Returns a store error; a partially routed event is safe to route again, since its
    /// key deduplicates what already ran.
    pub async fn route(&self, event: WakeEvent) -> Result<Vec<WakeOutcome>> {
        let _gate = self.gate.lock().await;
        let now = self.clock.now();
        let mut outcomes = Vec::new();
        for (meta, wake, run) in self.sleepers().await? {
            let expired = wake.expires_at.is_some_and(|at| at <= now);
            if expired || !wake.any_of.iter().any(|wake| wake_matches(wake, &event)) {
                continue;
            }
            self.wake_sleeper(&meta, &run).await?;
            outcomes.push(WakeOutcome::Woke { pid: meta.pid });
        }
        if outcomes.is_empty() {
            for trigger in self.triggers.iter().filter(|t| wake_matches(&t.wake, &event)) {
                if let Some(outcome) = self.fire(trigger, &event).await? {
                    outcomes.push(outcome);
                }
            }
        }
        tracing::debug!(
            wake.source = %event.source,
            wake.key = %event.key,
            wake.outcomes = outcomes.len(),
            "wake routed"
        );
        self.publish(&event.source, &event.key, &outcomes);
        Ok(outcomes)
    }

    /// Submits `event` from an in-process source as its keyed `kernel.wake` maintenance run
    /// ([`kernel_wake_run`]), the way a source's event reaches [`route`](Self::route) (spec
    /// 3.2 §7.4, K-I3). The run store keeps one run per key, so a repeat of a key, even one
    /// that already woke a sleeper, never reaches the router and cannot start a trigger run.
    /// The maintenance worker's [`KernelWakeExecutor`] routes the run.
    ///
    /// Returns `Enqueued::Created` for a new key and `Enqueued::Existing` for a repeat.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_core::{Enqueued, InMemoryRunStore};
    /// use adk_kernel::{InMemoryProcessStore, WakeEvent, WakeRouter};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> adk_kernel::Result<()> {
    /// let router = WakeRouter::new(Arc::new(InMemoryRunStore::new()), Arc::new(InMemoryProcessStore::new()));
    /// let event = WakeEvent::new("lockfile", "file:/repo:/repo/Cargo.lock:2026-10-22T09:00:00Z", serde_json::json!({}));
    /// assert!(matches!(router.submit(&event, "adk-rust").await?, Enqueued::Created(_)));
    /// assert!(matches!(router.submit(&event, "adk-rust").await?, Enqueued::Existing(_)));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`kernel_wake_run`]'s error, or the run store's.
    pub async fn submit(&self, event: &WakeEvent, app_name: &str) -> Result<Enqueued> {
        let enqueued = self.runs.enqueue(kernel_wake_run(event, app_name)?).await?;
        if let Enqueued::Existing(run) = &enqueued {
            tracing::debug!(wake.key = %event.key, run.id = %run.run_id, "repeated wake not routed");
        }
        Ok(enqueued)
    }

    /// Applies the [`ExpiryAction`] of every wake set that expired by `now`, and wakes every
    /// sleeper whose [`Wake::At`] time passed by `now`; called on the scheduler tick.
    ///
    /// [`ExpiryAction::Cancel`] cancels the process through `DurableRunStore::request_cancel`;
    /// [`ExpiryAction::Run`] wakes it as though a wake fired. Either way the wake set is
    /// cleared. A suspended process is left alone: it has no expiry of its own.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn sweep_expired(&self, now: DateTime<Utc>) -> Result<Vec<WakeOutcome>> {
        let _gate = self.gate.lock().await;
        let mut outcomes = Vec::new();
        for (meta, wake, run) in self.sleepers().await? {
            if let Some(expires_at) = wake.expires_at.filter(|at| *at <= now) {
                match wake.on_expiry {
                    ExpiryAction::Cancel => {
                        self.runs.request_cancel(&meta.pid).await?;
                        self.processes.set_wake(&meta.pid, None).await?;
                        if let Some(bus) = &self.bus {
                            bus.publish(KernelEvent::Process {
                                pid: meta.pid.clone(),
                                from: RunState::Waiting,
                                to: RunState::Cancelled,
                                reason: Some("wake set expired".into()),
                            });
                        }
                    }
                    ExpiryAction::Run => self.wake_sleeper(&meta, &run).await?,
                }
                let outcome =
                    WakeOutcome::Expired { pid: meta.pid.clone(), action: wake.on_expiry };
                let key = format!("{EXPIRY_SOURCE}:{}:{}", meta.pid, expires_at.to_rfc3339());
                self.publish(EXPIRY_SOURCE, &key, std::slice::from_ref(&outcome));
                outcomes.push(outcome);
                continue;
            }
            let due = wake.any_of.iter().find_map(|wake| match wake {
                Wake::At(at) if *at <= now => Some(*at),
                _ => None,
            });
            if let Some(at) = due {
                self.wake_sleeper(&meta, &run).await?;
                let outcome = WakeOutcome::Woke { pid: meta.pid.clone() };
                let key = format!("{AT_SOURCE}:{}:{}", meta.pid, at.to_rfc3339());
                self.publish(AT_SOURCE, &key, std::slice::from_ref(&outcome));
                outcomes.push(outcome);
            }
        }
        Ok(outcomes)
    }

    /// Wakes every sleeper whose [`Wake::FileChange`] matches a file modified since the
    /// process went to sleep (its run's last change), for the changes missed while the
    /// kernel was down (spec 3.2 §7.4 missed wakes). Runs on start, before the wake sources
    /// subscribe.
    ///
    /// The scan walks each root without following symbolic links, at most 100,000 entries
    /// per root. The wake is published keyed [`keys::file`] by the newest such file.
    ///
    /// # Errors
    ///
    /// Returns a store error, or [`KernelError::Io`] when a scan cannot run.
    pub async fn reconcile(&self) -> Result<Vec<WakeOutcome>> {
        let _gate = self.gate.lock().await;
        let mut outcomes = Vec::new();
        for (meta, wake, run) in self.sleepers().await? {
            let mut newest: Option<(PathBuf, PathBuf, DateTime<Utc>)> = None;
            for watched in &wake.any_of {
                let Wake::FileChange { root, glob, .. } = watched else { continue };
                let (root, glob, since) = (canonical(root), glob.clone(), run.updated_at);
                let scan_root = root.clone();
                let found =
                    tokio::task::spawn_blocking(move || newest_since(&scan_root, &glob, since))
                        .await
                        .map_err(|error| KernelError::Io(std::io::Error::other(error)))?;
                if let Some((path, modified)) = found
                    && newest.as_ref().is_none_or(|(_, _, newest)| modified > *newest)
                {
                    newest = Some((root, path, modified));
                }
            }
            let Some((root, path, modified)) = newest else { continue };
            self.wake_sleeper(&meta, &run).await?;
            let outcome = WakeOutcome::Woke { pid: meta.pid.clone() };
            self.publish(
                RECONCILE_SOURCE,
                &keys::file(&root, &path, modified),
                std::slice::from_ref(&outcome),
            );
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }

    /// The `Waiting` processes asleep on a wake set: not suspended, run still held.
    async fn sleepers(&self) -> Result<Vec<(ProcessMeta, WakeSet, RunRecord)>> {
        let mut sleepers = Vec::new();
        for meta in self.processes.list(&ProcessFilter::default()).await? {
            let Some(wake) = meta.wake.clone() else { continue };
            if meta.suspended_by.is_some() {
                continue;
            }
            let Some(run) = self.runs.get(&meta.pid).await? else { continue };
            if run.state == RunState::Waiting {
                sleepers.push((meta, wake, run));
            }
        }
        Ok(sleepers)
    }

    /// Wakes `meta`'s process and consumes its wake set. The row is marked awake before the
    /// run is claimable — a sleep request still recorded cleared, then the wake set — since
    /// the claim reads the wake set to tell a sleeper from a woken process (K-I2). A failed
    /// wake restores the set, so the process stays a sleeper its next wake can reach.
    async fn wake_sleeper(&self, meta: &ProcessMeta, run: &RunRecord) -> Result<()> {
        if meta.suspend_requested {
            self.processes.set_suspend_requested(&meta.pid, false).await?;
        }
        self.processes.set_wake(&meta.pid, None).await?;
        if let Err(error) = self.runs.wake(&run.run_id).await {
            if let Err(restore) = self.processes.set_wake(&meta.pid, meta.wake.clone()).await {
                tracing::warn!(
                    process.pid = %meta.pid,
                    error = %restore,
                    "could not restore the wake set of a process whose wake failed"
                );
            }
            return Err(error.into());
        }
        Ok(())
    }

    /// Enqueues `trigger`'s run for `event` and records its process.
    async fn fire(&self, trigger: &Trigger, event: &WakeEvent) -> Result<Option<WakeOutcome>> {
        let mut run = trigger.run.clone();
        run.idempotency_key = Some(keys::trigger(&event.key, &trigger.name));
        if run.concurrency_key.is_none() {
            run.concurrency_key = Some(
                run.session_id
                    .clone()
                    .unwrap_or_else(|| format!("partition:{}", trigger.partition)),
            );
        }
        match &event.principal {
            Some(principal) => {
                run.user_id = principal.clone();
                run.authenticated = true;
            }
            None => run.authenticated = false,
        }
        let run = run.with_service_subject();
        let (record, created) = match self.runs.enqueue(run).await? {
            Enqueued::Created(record) => (record, true),
            Enqueued::Existing(record) => (record, false),
            Enqueued::Rejected { blocking_run_id } => {
                tracing::info!(
                    wake.trigger = %trigger.name,
                    wake.key = %event.key,
                    run.blocking_id = %blocking_run_id,
                    "trigger run refused by its concurrency key"
                );
                return Ok(None);
            }
        };
        if created || self.processes.get(&record.run_id).await?.is_none() {
            let mut meta = ProcessMeta::new(
                record.run_id.clone(),
                QueueClass::from_name(&record.queue),
                ProcessKind::Triggered { source: trigger.name.clone() },
            )
            .with_partition(trigger.partition.clone());
            if let Some(parent) = event
                .key
                .starts_with("run:")
                .then(|| event.payload.get("pid"))
                .flatten()
                .and_then(Value::as_str)
            {
                meta = meta.with_parent(parent);
            }
            self.processes.put(meta).await?;
        }
        Ok(Some(if created {
            WakeOutcome::Spawned { pid: record.run_id, trigger: trigger.name.clone() }
        } else {
            WakeOutcome::Deduplicated { pid: record.run_id }
        }))
    }

    fn publish(&self, source: &str, key: &str, outcomes: &[WakeOutcome]) {
        let Some(bus) = &self.bus else { return };
        if outcomes.is_empty() {
            bus.publish(KernelEvent::Wake {
                pid: None,
                source: source.to_string(),
                key: key.to_string(),
                outcome: None,
            });
        }
        for outcome in outcomes {
            bus.publish(KernelEvent::Wake {
                pid: Some(outcome.pid().to_string()),
                source: source.to_string(),
                key: key.to_string(),
                outcome: Some(outcome.clone()),
            });
        }
    }
}

/// The newest file under `root` whose name matches `glob` and that was modified after
/// `since`, walking without following symbolic links.
fn newest_since(root: &Path, glob: &str, since: DateTime<Utc>) -> Option<(PathBuf, DateTime<Utc>)> {
    let mut newest: Option<(PathBuf, DateTime<Utc>)> = None;
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            visited += 1;
            if visited > RECONCILE_SCAN_LIMIT {
                tracing::warn!(wake.root = %root.display(), "reconcile scan limit reached");
                return newest;
            }
            let Ok(file_type) = entry.file_type() else { continue };
            let path = entry.path();
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            let name_matches = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| glob_matches(glob, name));
            if !file_type.is_file() || !name_matches {
                continue;
            }
            let Some(modified) = entry.metadata().ok().and_then(|meta| meta.modified().ok()) else {
                continue;
            };
            let modified = DateTime::<Utc>::from(modified);
            if modified > since && newest.as_ref().is_none_or(|(_, newest)| modified > *newest) {
                newest = Some((path, modified));
            }
        }
    }
    newest
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

    /// Decodes the run's input as a [`WakeEvent`] and routes it; the result is
    /// `{"outcomes": [..]}`. A failed route fails the attempt, and a retry routes again
    /// safely, since the event's key deduplicates what already ran.
    async fn execute(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        cancel_token: CancellationToken,
    ) -> std::result::Result<Value, String> {
        if workflow_id != KERNEL_WAKE_TARGET {
            return Err(format!(
                "the kernel wake executor runs only `{KERNEL_WAKE_TARGET}`, not `{workflow_id}`"
            ));
        }
        let event: WakeEvent = serde_json::from_value(Value::Object(input.into_iter().collect()))
            .map_err(|error| {
            format!(
                "a `{KERNEL_WAKE_TARGET}` run's input must be a wake event, as \
                     kernel_wake_run builds it: {error}"
            )
        })?;
        tokio::select! {
            routed = self.router.route(event) => routed
                .map(|outcomes| json!({ "outcomes": outcomes }))
                .map_err(|error| error.to_string()),
            () = cancel_token.cancelled() => Err("the wake run was cancelled".to_string()),
        }
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

    /// Subscribes to the cron trigger: each tick, catch-up ticks included, becomes an event
    /// keyed [`keys::cron`] by its `scheduled_for`, with the tick's payload (`expression`,
    /// `scheduled_for`, `catch_up`). Missed ticks follow the trigger's `MissedTickPolicy`
    /// and watermark.
    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>> {
        let name = self.name.clone();
        let ticks = self.trigger.subscribe().await?;
        Ok(ticks
            .filter_map(move |tick| {
                let scheduled_for = tick
                    .payload
                    .get("scheduled_for")
                    .and_then(Value::as_str)
                    .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                    .map(|at| at.with_timezone(&Utc));
                let event = match scheduled_for {
                    Some(at) => Some(WakeEvent::new(name.clone(), keys::cron(&name, at), tick.payload)),
                    None => {
                        tracing::warn!(wake.source = %name, "cron tick without scheduled_for; skipped");
                        None
                    }
                };
                futures::future::ready(event)
            })
            .boxed())
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

    /// A source named `name` watching `root`, recursively, for files whose name matches
    /// `glob` (`*` any run of characters, `?` one character).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Core`] when `root` does not exist.
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

    /// Subscribes to the watcher. Notifications for one path are debounced: the event is
    /// emitted once the path has been quiet for [`debounce`](Self::debounce), keyed
    /// [`keys::file`] by the modification time read then, so the several notifications
    /// one save produces wake once and a later save, even back to earlier contents, wakes
    /// again. A removed file is keyed by the time it was seen gone. The payload carries
    /// `event` (the last notification's kind), `path`, `root`, `modified` and `removed`;
    /// paths are canonical.
    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>> {
        let notifications = self.trigger.subscribe().await?;
        let state = FileDebounce {
            name: self.name.clone(),
            root: canonical(&self.root),
            debounce: self.debounce,
            notifications: Some(notifications),
            pending: HashMap::new(),
        };
        Ok(stream::unfold(state, |mut state| async move {
            state.next().await.map(|event| (event, state))
        })
        .boxed())
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
    hub: WakeHub,
}

impl ApprovalWakeSource {
    /// The source's name.
    pub const NAME: &'static str = "approval";

    /// A source named [`NAME`](Self::NAME).
    pub fn new() -> Self {
        Self { name: Self::NAME.to_string(), hub: WakeHub::default() }
    }
}

impl Default for ApprovalWakeSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ApprovalNotifier for ApprovalWakeSource {
    /// Publishes a request that reached its outcome — resolved (approved, rejected or
    /// answered), expired or cancelled — as one event keyed [`keys::approval`] by the request
    /// id, so each request wakes at most once. Submissions, escalations and the single
    /// decisions of a multi-approver policy are not wakes. The payload carries `requestId`,
    /// `fingerprint`, `runId`, `toolName`, `state` and `kind`; the event names no principal.
    async fn notify(&self, notification: &ApprovalNotification) -> adk_core::Result<()> {
        if !matches!(
            notification.kind,
            ApprovalEventKind::Resolved | ApprovalEventKind::Expired | ApprovalEventKind::Cancelled
        ) {
            return Ok(());
        }
        let request = &notification.request;
        let payload = json!({
            "requestId": request.id,
            "fingerprint": request.fingerprint,
            "runId": request.run_id,
            "toolName": request.tool_name,
            "state": request.state,
            "kind": notification.kind,
        });
        self.hub.publish(
            WakeEvent::new(self.name.clone(), keys::approval(&request.id), payload)
                .with_at(notification.at),
        );
        Ok(())
    }
}

#[async_trait]
impl WakeSource for ApprovalWakeSource {
    fn name(&self) -> &str {
        &self.name
    }

    /// Subscribes to decided approvals. Events published while no subscriber is attached are
    /// held, up to 1024, for the first subscriber.
    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>> {
        Ok(self.hub.subscribe())
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
    hub: WakeHub,
}

impl RunFinishedWakeSource {
    /// The source's name.
    pub const NAME: &'static str = "run-finished";

    /// A source named [`NAME`](Self::NAME).
    pub fn new() -> Self {
        Self { name: Self::NAME.to_string(), hub: WakeHub::default() }
    }
}

impl Default for RunFinishedWakeSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WorkerHooks for RunFinishedWakeSource {
    /// Publishes a run that finished — `Completed`, `Failed` or `Cancelled` — as one event
    /// keyed [`keys::run_finished`]; a retryable failure the worker requeued is not finished.
    /// The payload carries `pid`, `state`, `target` and `queue`. A [`KERNEL_WAKE_TARGET`] run
    /// is the kernel routing a wake, not a process: its finish publishes nothing, since its
    /// event would be routed by another `kernel.wake` run, without end.
    async fn on_finish(&self, run: &RunRecord) {
        if !run.state.is_terminal() || run.target == KERNEL_WAKE_TARGET {
            return;
        }
        let payload = json!({
            "pid": run.run_id,
            "state": run.state,
            "target": run.target,
            "queue": run.queue,
        });
        self.hub.publish(
            WakeEvent::new(self.name.clone(), keys::run_finished(&run.run_id, run.state), payload)
                .with_at(run.updated_at),
        );
    }
}

#[async_trait]
impl WakeSource for RunFinishedWakeSource {
    fn name(&self) -> &str {
        &self.name
    }

    /// Subscribes to finished runs. Events published while no subscriber is attached are
    /// held, up to 1024, for the first subscriber.
    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>> {
        Ok(self.hub.subscribe())
    }
}

/// How many events a push source holds for its first subscriber.
const HUB_BACKLOG: usize = 1024;

/// The fan-out behind the push sources: every subscriber gets every event; events published
/// with no subscriber attached wait, bounded, for the first one.
#[derive(Debug, Default)]
struct WakeHub {
    state: Mutex<HubState>,
}

#[derive(Debug, Default)]
struct HubState {
    subscribers: Vec<mpsc::UnboundedSender<WakeEvent>>,
    backlog: VecDeque<WakeEvent>,
}

impl WakeHub {
    fn publish(&self, event: WakeEvent) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.subscribers.retain(|subscriber| !subscriber.is_closed());
        if state.subscribers.is_empty() {
            if state.backlog.len() == HUB_BACKLOG {
                state.backlog.pop_front();
                tracing::warn!(wake.source = %event.source, "wake backlog full; oldest event dropped");
            }
            state.backlog.push_back(event);
            return;
        }
        state.subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }

    fn subscribe(&self) -> BoxStream<'static, WakeEvent> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for event in state.backlog.drain(..) {
            // The receiver is held here, so the send cannot fail.
            let _ = sender.send(event);
        }
        state.subscribers.push(sender);
        stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|event| (event, receiver))
        })
        .boxed()
    }
}

/// The state of a [`FileWakeSource`] subscription: watcher notifications in, debounced wake
/// events out.
struct FileDebounce {
    name: String,
    root: PathBuf,
    debounce: Duration,
    notifications: Option<BoxStream<'static, adk_agent::ambient::TriggerEvent>>,
    /// Each changed path with the time it settles and its last notification's kind.
    pending: HashMap<PathBuf, (tokio::time::Instant, String)>,
}

impl FileDebounce {
    /// The next settled change, or `None` once the watcher ended and nothing is pending.
    async fn next(&mut self) -> Option<WakeEvent> {
        loop {
            let settles = self.pending.values().map(|(at, _)| *at).min();
            let notification = match (self.notifications.as_mut(), settles) {
                (None, None) => return None,
                (None, Some(at)) => {
                    tokio::time::sleep_until(at).await;
                    None
                }
                (Some(notifications), None) => match notifications.next().await {
                    Some(notification) => Some(notification),
                    None => {
                        self.notifications = None;
                        continue;
                    }
                },
                (Some(notifications), Some(at)) => tokio::select! {
                    notification = notifications.next() => match notification {
                        Some(notification) => Some(notification),
                        None => {
                            self.notifications = None;
                            continue;
                        }
                    },
                    () = tokio::time::sleep_until(at) => None,
                },
            };
            if let Some(notification) = notification {
                let Some(path) = notification.payload.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let kind = notification
                    .payload
                    .get("event")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let settles = tokio::time::Instant::now() + self.debounce;
                self.pending.insert(canonical(Path::new(path)), (settles, kind));
                continue;
            }
            let now = tokio::time::Instant::now();
            let due = self
                .pending
                .iter()
                .filter(|(_, (at, _))| *at <= now)
                .min_by_key(|(_, (at, _))| *at)
                .map(|(path, _)| path.clone());
            let Some(path) = due else { continue };
            let Some((_, kind)) = self.pending.remove(&path) else { continue };
            // Read before borrowing `self`: the watcher stream is not `Sync`, so `&self` may
            // not live across an await.
            let modified = tokio::fs::metadata(&path)
                .await
                .and_then(|meta| meta.modified())
                .map(DateTime::<Utc>::from)
                .ok();
            return Some(self.settled(path, kind, modified));
        }
    }

    fn settled(&self, path: PathBuf, kind: String, modified: Option<DateTime<Utc>>) -> WakeEvent {
        let at = modified.unwrap_or_else(Utc::now);
        let payload = json!({
            "event": kind,
            "path": path.display().to_string(),
            "root": self.root.display().to_string(),
            "modified": at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            "removed": modified.is_none(),
        });
        WakeEvent::new(self.name.clone(), keys::file(&self.root, &path, at), payload)
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
        assert_eq!(keys::approval("apr-7"), "apr-7");
        assert_eq!(keys::run_finished("run-7", RunState::Failed), "run:run-7:failed");
        assert_eq!(keys::run_finished("run-7", RunState::Cancelled), "run:run-7:cancelled");
        #[cfg(feature = "webhooks")]
        assert_eq!(
            keys::webhook("github", "msg_1"),
            adk_server::webhooks::WebhookHook::idempotency_key("github", "msg_1")
        );
    }

    fn event(source: &str, key: &str, payload: Value) -> WakeEvent {
        WakeEvent::new(source, key, payload)
    }

    fn cron_tick(expression: &str, catch_up: bool) -> WakeEvent {
        event(
            "nightly",
            "cron:nightly:2026-10-22T02:00:00Z",
            json!({"expression": expression, "scheduled_for": "2026-10-22T02:00:00+00:00", "catch_up": catch_up}),
        )
    }

    #[test]
    fn cron_wakes_match_their_expression_and_missed_policy() {
        let wake = |missed| Wake::Cron { expr: "0 0 2 * * *".into(), missed };
        assert!(wake_matches(&wake(MissedFirePolicy::Skip), &cron_tick("0 0 2 * * *", false)));
        assert!(!wake_matches(&wake(MissedFirePolicy::Skip), &cron_tick("0 0 2 * * *", true)));
        assert!(wake_matches(&wake(MissedFirePolicy::FireOnce), &cron_tick("0 0 2 * * *", true)));
        assert!(wake_matches(&wake(MissedFirePolicy::CatchUp(3)), &cron_tick("0 0 2 * * *", true)));
        assert!(!wake_matches(&wake(MissedFirePolicy::FireOnce), &cron_tick("0 0 3 * * *", false)));
        let not_cron = event("nightly", "queue:nightly:1", json!({"expression": "0 0 2 * * *"}));
        assert!(!wake_matches(&wake(MissedFirePolicy::FireOnce), &not_cron));
    }

    #[test]
    fn file_wakes_match_paths_under_their_root_by_file_name() {
        let wake = Wake::FileChange {
            root: "/work/repo".into(),
            glob: "*.lock".into(),
            debounce: FileWakeSource::DEFAULT_DEBOUNCE,
        };
        let change = |path: &str| {
            event("lockfile", &format!("file:/work/repo:{path}:t"), json!({"path": path}))
        };
        assert!(wake_matches(&wake, &change("/work/repo/Cargo.lock")));
        assert!(wake_matches(&wake, &change("/work/repo/crates/a/Cargo.lock")));
        assert!(!wake_matches(&wake, &change("/work/repo/Cargo.toml")));
        assert!(!wake_matches(&wake, &change("/work/other/Cargo.lock")));
        assert!(!wake_matches(&wake, &change("/work/repository/Cargo.lock")));
        let not_file = event("lockfile", "hook:x:1", json!({"path": "/work/repo/Cargo.lock"}));
        assert!(!wake_matches(&wake, &not_file));
    }

    #[test]
    fn file_wakes_match_a_canonical_path_against_a_relative_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical(dir.path());
        let relative = Wake::FileChange {
            root: dir.path().join("."),
            glob: "notes.md".into(),
            debounce: FileWakeSource::DEFAULT_DEBOUNCE,
        };
        let path = root.join("notes.md").display().to_string();
        assert!(wake_matches(&relative, &event("notes", "file:k", json!({ "path": path }))));
    }

    #[test]
    fn webhook_queue_approval_and_run_finished_wakes_match_their_source() {
        let webhook = Wake::Webhook { hook: "github".into() };
        assert!(wake_matches(&webhook, &event("hook:github", "hook:github:msg_1", json!({}))));
        assert!(!wake_matches(&webhook, &event("hook:gitlab", "hook:gitlab:msg_1", json!({}))));
        assert!(!wake_matches(&webhook, &event("hook:github", "queue:github:1", json!({}))));

        let queue = Wake::Queue { source: "orders".into() };
        assert!(wake_matches(&queue, &event("orders", "queue:orders:m-1", json!({}))));
        assert!(!wake_matches(&queue, &event("refunds", "queue:refunds:m-1", json!({}))));
        assert!(!wake_matches(&queue, &event("orders", "cron:orders:t", json!({}))));

        let approval = Wake::Approval { fingerprint: "fp-1".into() };
        let decided = |fingerprint: &str| {
            event(ApprovalWakeSource::NAME, "apr-1", json!({ "fingerprint": fingerprint }))
        };
        assert!(wake_matches(&approval, &decided("fp-1")));
        assert!(!wake_matches(&approval, &decided("fp-2")));
        assert!(!wake_matches(&approval, &event("other", "apr-1", json!({"fingerprint": "fp-1"}))));

        let finished = Wake::RunFinished { pid: "run-1".into() };
        let done = |pid: &str| {
            event(
                RunFinishedWakeSource::NAME,
                &keys::run_finished(pid, RunState::Completed),
                json!({ "pid": pid }),
            )
        };
        assert!(wake_matches(&finished, &done("run-1")));
        assert!(!wake_matches(&finished, &done("run-2")));
    }

    #[test]
    fn time_and_manual_wakes_match_no_event() {
        let any = [
            cron_tick("0 0 2 * * *", false),
            event("hook:github", "hook:github:msg_1", json!({})),
            event("budget", "budget:default:2026-10-23", json!({"partition": "default"})),
            event("manual", "manual:run-1", json!({})),
        ];
        for wake in [Wake::At(Utc::now()), Wake::Manual] {
            assert!(any.iter().all(|event| !wake_matches(&wake, event)), "{wake:?} matched");
        }
    }

    #[test]
    fn a_budget_wake_matches_its_partitions_replenishment_only() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 23).unwrap();
        let wake = Wake::BudgetReplenished { partition: PartitionId::new("work") };
        let work = event("budget", &keys::budget("work", day), json!({"partition": "work"}));
        let home = event("budget", &keys::budget("home", day), json!({"partition": "home"}));
        let forged = event("hook:x", "hook:x:1", json!({"partition": "work"}));
        assert_eq!(keys::budget("work", day), "budget:work:2026-10-23");
        assert!(wake_matches(&wake, &work));
        assert!(!wake_matches(&wake, &home));
        assert!(!wake_matches(&wake, &forged), "only the kernel's budget events match");
    }

    #[test]
    fn globs_match_like_the_file_watch_trigger() {
        let cases = [
            ("*.rs", "main.rs", true),
            ("*.rs", "main.rsx", false),
            ("Cargo.lock", "Cargo.lock", true),
            ("Cargo.?ock", "Cargo.lock", true),
            ("*", "", true),
            ("a*b*c", "aXXbYYc", true),
            ("a*b*c", "aXXbYY", false),
            ("**x", "abx", true),
            ("?", "", false),
        ];
        for (pattern, text, expected) in cases {
            assert_eq!(glob_matches(pattern, text), expected, "{pattern} vs {text}");
        }
    }

    #[test]
    fn kernel_wake_runs_are_maintenance_service_runs_keyed_by_the_event() {
        let anonymous = event("hook:github", "hook:github:msg_1", json!({"n": 1}));
        let run = kernel_wake_run(&anonymous, "adk-rust").unwrap();
        assert_eq!(
            (run.queue.as_str(), run.target.as_str(), run.user_id.as_str()),
            (QueueClass::MAINTENANCE, KERNEL_WAKE_TARGET, "hook:github")
        );
        assert_eq!(run.idempotency_key.as_deref(), Some("hook:github:msg_1"));
        assert!(!run.authenticated);
        let principal = run.principal.clone().unwrap();
        assert_eq!(principal.on_behalf_of, Some(adk_core::SubjectRef::service("hook:github")));
        let decoded: WakeEvent = serde_json::from_value(run.input).unwrap();
        assert_eq!(decoded, anonymous);

        let verified = anonymous.clone().with_principal("github-app");
        let run = kernel_wake_run(&verified, "adk-rust").unwrap();
        assert!(run.authenticated);
        assert_eq!(run.user_id, "github-app");

        assert!(kernel_wake_run(&event("hook:github", "", json!({})), "adk-rust").is_err());
    }

    #[tokio::test]
    async fn push_sources_hold_events_for_their_first_subscriber() {
        let source = RunFinishedWakeSource::new();
        let mut run = RunRecord::queued(
            "run-1",
            NewRun::new("background", "adk-rust", "alice", "adk-rust", Value::Null),
            Utc::now(),
        );
        source.on_finish(&run).await;
        run.state = RunState::Completed;
        source.on_finish(&run).await;
        let mut events = source.subscribe().await.unwrap();
        let first = events.next().await.unwrap();
        assert_eq!(first.key, "run:run-1:completed");
        assert_eq!(first.source, RunFinishedWakeSource::NAME);
        assert_eq!(first.payload["pid"], "run-1");

        let mut second = source.subscribe().await.unwrap();
        run.run_id = "run-2".into();
        run.state = RunState::Failed;
        source.on_finish(&run).await;
        assert_eq!(events.next().await.unwrap().key, "run:run-2:failed");
        assert_eq!(second.next().await.unwrap().key, "run:run-2:failed");
    }
}
