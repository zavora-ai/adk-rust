//! The event bus (spec 3.2 §7.6).
//!
//! Every state change in the kernel is a [`KernelEvent`]: process transitions from the
//! worker's S1 hooks (never polled), wakes, holds and decisions, rule hits, spend,
//! governance and health. [`EventBus`] numbers each one with a cursor and delivers it to
//! subscribers and [`EventSink`]s. The bus is bounded per sink; a slow sink drops `Health`
//! first and never blocks a process. A reconnecting surface catches up by cursor
//! ([`EventBus::subscribe_after`]) rather than by holding a subscription open.
//!
//! The receivers are the control plane's SSE stream (`GET /kernel/events`, a subscriber),
//! the signed audit log ([`AuditSink`]) for the events that are audit records (§9.3), and
//! outbound webhooks (`WebhookSink`, feature `webhooks`): each event an [`EventFilter`]
//! selects, signed with Standard Webhooks signatures as the webhook ingress verifies them.
//! `Driver` events come from the MCP manager's status channel (spec 3.2 §10 T1), never
//! polled. With a ledger (feature `proc`), the kernel's ledger sink writes a content-free
//! row for every event but `Health` (spec 3.2 §7.10).

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::{RunState, SpendKey, SubjectRef, ToolConfirmationDecision, ToolConfirmationRequest};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Notify, broadcast};

use crate::agenda::AgendaState;
use crate::error::{KernelError, Result};
use crate::kernel::KernelHealth;
use crate::partition::PartitionId;
use crate::review::ReviewReport;
use crate::wake::WakeOutcome;

#[cfg(feature = "webhooks")]
mod webhook;
#[cfg(feature = "webhooks")]
pub use webhook::WebhookSink;

/// How many events a subscriber may fall behind before it skips ahead.
pub const DEFAULT_EVENT_CAPACITY: usize = 1024;

/// A change in the kernel (spec 3.2 §7.6).
///
/// # Example
///
/// ```rust
/// use adk_core::RunState;
/// use adk_kernel::KernelEvent;
///
/// let event = KernelEvent::Process {
///     pid: "run-1".into(),
///     from: RunState::Running,
///     to: RunState::Waiting,
///     reason: Some("suspended".into()),
/// };
/// let json = serde_json::to_value(&event).unwrap();
/// assert_eq!(json["type"], "process");
/// assert_eq!(json["to"], "waiting");
/// assert_eq!(event.name(), "process");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum KernelEvent {
    /// A process changed state.
    Process {
        /// The process.
        pid: String,
        /// The state it left.
        from: RunState,
        /// The state it entered.
        to: RunState,
        /// Why, such as a park reason.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A wake event was routed; one per process it concerned.
    Wake {
        /// The process it woke or started, `outcome`'s process; `None` when it matched
        /// nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<String>,
        /// The wake source.
        source: String,
        /// The event's idempotency key.
        key: String,
        /// What the wake did to `pid`: woke it, spawned it, found its key already used, or
        /// expired its wake set; `None` when it matched nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<WakeOutcome>,
    },
    /// A process holds a tool call for approval.
    Held {
        /// The process.
        pid: String,
        /// The held call.
        request: ToolConfirmationRequest,
    },
    /// A held call was decided.
    Decided {
        /// The process.
        pid: String,
        /// The call's fingerprint.
        fingerprint: String,
        /// The decision.
        decision: ToolConfirmationDecision,
        /// Who decided.
        by: SubjectRef,
    },
    /// A standing rule allowed a call.
    RuleHit {
        /// The process.
        pid: String,
        /// The rule.
        rule_id: String,
    },
    /// Spend was committed.
    Spend {
        /// The partition charged.
        partition: PartitionId,
        /// The spend key.
        key: SpendKey,
        /// How much.
        delta: SpendDelta,
    },
    /// An agenda item changed state (spec 3.2 §7.5); a proposal the policy refused is
    /// published `proposed` → `rejected` with the reason, though it was never stored.
    Agenda {
        /// The item.
        item: String,
        /// The state it left.
        from: AgendaState,
        /// The state it entered.
        to: AgendaState,
        /// Why, such as the limit a refused proposal would exceed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The maintenance tick's daily review: what is older than the partition's review
    /// period (spec 3.2 §7.4, §9.1).
    Review(ReviewReport),
    /// A driver's state changed: a status of its MCP server, or `needsReapproval` once a
    /// tool's declaration differs from the approved one (spec 3.2 §7.6, §7.7).
    Driver {
        /// The driver.
        id: String,
        /// The state it entered.
        state: crate::drivers::DriverState,
        /// Why, such as the tools that changed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A baseline pass recorded an observation (spec 3.2 §7.10, feature `proc-baselines`).
    Observation {
        /// The observation.
        id: String,
        /// Its partition.
        partition: PartitionId,
    },
    /// The kill switch changed.
    Governance {
        /// Whether execution is frozen.
        frozen: bool,
        /// The freeze reason.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A health report; the first event a slow sink drops.
    Health(KernelHealth),
}

impl KernelEvent {
    /// The event's type, as its `type` field serializes it.
    pub fn name(&self) -> &'static str {
        self.kind().as_str()
    }

    /// The event's kind, which an [`EventFilter`] selects on.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::{EventKind, KernelEvent};
    ///
    /// let event = KernelEvent::Governance { frozen: true, reason: None };
    /// assert_eq!(event.kind(), EventKind::Governance);
    /// ```
    pub fn kind(&self) -> EventKind {
        match self {
            Self::Process { .. } => EventKind::Process,
            Self::Wake { .. } => EventKind::Wake,
            Self::Held { .. } => EventKind::Held,
            Self::Decided { .. } => EventKind::Decided,
            Self::RuleHit { .. } => EventKind::RuleHit,
            Self::Spend { .. } => EventKind::Spend,
            Self::Agenda { .. } => EventKind::Agenda,
            Self::Review(_) => EventKind::Review,
            Self::Driver { .. } => EventKind::Driver,
            Self::Observation { .. } => EventKind::Observation,
            Self::Governance { .. } => EventKind::Governance,
            Self::Health(_) => EventKind::Health,
        }
    }
}

/// The kind of a [`KernelEvent`], named as its `type` field serializes it (spec 3.2 §7.6).
///
/// # Example
///
/// ```rust
/// use adk_kernel::EventKind;
///
/// let kind: EventKind = "ruleHit".parse().unwrap();
/// assert_eq!(kind, EventKind::RuleHit);
/// assert_eq!(kind.to_string(), "ruleHit");
/// assert!("rule_hit".parse::<EventKind>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum EventKind {
    /// [`KernelEvent::Process`].
    Process,
    /// [`KernelEvent::Wake`].
    Wake,
    /// [`KernelEvent::Held`].
    Held,
    /// [`KernelEvent::Decided`].
    Decided,
    /// [`KernelEvent::RuleHit`].
    RuleHit,
    /// [`KernelEvent::Spend`].
    Spend,
    /// [`KernelEvent::Agenda`].
    Agenda,
    /// [`KernelEvent::Review`].
    Review,
    /// [`KernelEvent::Driver`].
    Driver,
    /// [`KernelEvent::Observation`].
    Observation,
    /// [`KernelEvent::Governance`].
    Governance,
    /// [`KernelEvent::Health`].
    Health,
}

impl EventKind {
    /// Every kind, in declaration order.
    pub const ALL: [EventKind; 12] = [
        Self::Process,
        Self::Wake,
        Self::Held,
        Self::Decided,
        Self::RuleHit,
        Self::Spend,
        Self::Agenda,
        Self::Review,
        Self::Driver,
        Self::Observation,
        Self::Governance,
        Self::Health,
    ];

    /// The kind's name, as a [`KernelEvent`]'s `type` field serializes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Wake => "wake",
            Self::Held => "held",
            Self::Decided => "decided",
            Self::RuleHit => "ruleHit",
            Self::Spend => "spend",
            Self::Agenda => "agenda",
            Self::Review => "review",
            Self::Driver => "driver",
            Self::Observation => "observation",
            Self::Governance => "governance",
            Self::Health => "health",
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EventKind {
    type Err = KernelError;

    fn from_str(text: &str) -> Result<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text.trim()).ok_or_else(|| {
            KernelError::Config {
                reason: format!(
                    "unknown kernel event type `{text}`; use one of {}",
                    Self::ALL.map(Self::as_str).join(", ")
                ),
            }
        })
    }
}

/// Which [`KernelEvent`]s a sink delivers, by kind (spec 3.2 §7.6).
///
/// The default selects every kind but [`EventKind::Health`], the periodic report a
/// receiver outside the kernel rarely needs and the first event a slow sink drops.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{EventFilter, EventKind, KernelEvent};
///
/// let frozen = KernelEvent::Governance { frozen: true, reason: None };
/// assert!(EventFilter::default().matches(&frozen));
/// assert!(!EventFilter::default().selects(EventKind::Health));
/// assert!(EventFilter::all().selects(EventKind::Health));
///
/// let decisions = EventFilter::only([EventKind::Held, EventKind::Decided]);
/// assert!(!decisions.matches(&frozen));
/// assert_eq!(EventFilter::parse(["held", "decided"]).unwrap(), decisions);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilter {
    kinds: BTreeSet<EventKind>,
}

impl Default for EventFilter {
    fn default() -> Self {
        Self::all().without(EventKind::Health)
    }
}

impl EventFilter {
    /// Every kind, `Health` included.
    pub fn all() -> Self {
        Self::only(EventKind::ALL)
    }

    /// Only `kinds`.
    pub fn only(kinds: impl IntoIterator<Item = EventKind>) -> Self {
        Self { kinds: kinds.into_iter().collect() }
    }

    /// Only the kinds named in `names`, as [`EventKind`] parses them.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] naming the first unknown type, so a misspelt name
    /// is refused rather than silently selecting nothing.
    pub fn parse<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Result<Self> {
        names
            .into_iter()
            .map(|name| name.as_ref().parse())
            .collect::<Result<Vec<EventKind>>>()
            .map(Self::only)
    }

    /// These kinds without `kind`.
    #[must_use]
    pub fn without(mut self, kind: EventKind) -> Self {
        self.kinds.remove(&kind);
        self
    }

    /// Whether the filter selects `kind`.
    pub fn selects(&self, kind: EventKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// Whether the filter selects `event`.
    pub fn matches(&self, event: &KernelEvent) -> bool {
        self.selects(event.kind())
    }

    /// The selected kinds, in declaration order.
    pub fn kinds(&self) -> impl Iterator<Item = EventKind> + '_ {
        self.kinds.iter().copied()
    }
}

/// An amount of committed spend (spec 3.2 §7.6), in micro-USD as `SpendLedger` counts it.
///
/// # Example
///
/// ```rust
/// use adk_kernel::SpendDelta;
///
/// assert_eq!(SpendDelta::new(12_500).micro_usd, 12_500);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SpendDelta {
    /// The amount, in micro-USD.
    pub micro_usd: u64,
}

impl SpendDelta {
    /// A delta of `micro_usd`.
    pub fn new(micro_usd: u64) -> Self {
        Self { micro_usd }
    }
}

/// A numbered [`KernelEvent`] as the bus delivers it (spec 3.2 §7.6).
///
/// `cursor` increases by one per event on one kernel; a surface reconnects with the last
/// cursor it saw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EventRecord {
    /// The event's position on the bus.
    pub cursor: u64,
    /// When the bus published it.
    pub at: DateTime<Utc>,
    /// The event.
    pub event: KernelEvent,
}

/// A destination for kernel events (spec 3.2 §7.6): the SSE stream, a channel notifier,
/// an outbound webhook, the audit log.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{EventSink, KernelEvent};
/// use async_trait::async_trait;
///
/// struct Log;
///
/// #[async_trait]
/// impl EventSink for Log {
///     async fn emit(&self, event: &KernelEvent) -> adk_kernel::Result<()> {
///         tracing::info!(event.kind = event.name(), "kernel event");
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Delivers `event`.
    ///
    /// # Errors
    ///
    /// Returns the sink's delivery error; the bus logs it and carries on.
    async fn emit(&self, event: &KernelEvent) -> Result<()>;

    /// Delivers `record`, the event with its cursor and time; the bus calls this. The
    /// default delivers the event alone through [`emit`](Self::emit); a sink whose receiver
    /// orders or deduplicates by cursor, such as an outbound webhook, overrides it.
    ///
    /// # Errors
    ///
    /// Returns the sink's delivery error; the bus logs it and carries on.
    async fn emit_record(&self, record: &EventRecord) -> Result<()> {
        self.emit(&record.event).await
    }
}

/// The kernel's event bus (spec 3.2 §7.6).
///
/// Cloning shares the bus. Each event gets the next cursor and goes to every subscriber and
/// sink:
///
/// | Receiver | Delivery | When it falls behind |
/// |---|---|---|
/// | [`EventSubscriber`] (the control plane's SSE stream) | A broadcast of `capacity` events | Skips to the oldest event still held |
/// | [`EventSink`] | Its own queue of `capacity` events, drained in order by a task on the Tokio runtime through [`EventSink::emit_record`] | Drops a queued `Health` first, then its oldest event; [`publish`](Self::publish) never waits |
///
/// The bus keeps its last `capacity` events, so [`subscribe_after`](Self::subscribe_after)
/// replays what a reconnecting surface missed before the live events, with no gap and no
/// repeat.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{EventBus, KernelEvent};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let bus = EventBus::new(16);
/// let mut subscriber = bus.subscribe();
/// let record = bus.publish(KernelEvent::Governance { frozen: true, reason: Some("incident".into()) });
/// assert_eq!(subscriber.recv().await.unwrap(), record);
/// # }
/// ```
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<BusInner>,
}

struct BusInner {
    sender: broadcast::Sender<EventRecord>,
    capacity: usize,
    /// The next cursor and the recent events; held across the broadcast so a replay and a
    /// live subscription meet exactly.
    log: Mutex<BusLog>,
    sinks: Mutex<Vec<Arc<SinkQueue>>>,
}

struct BusLog {
    next: u64,
    history: VecDeque<EventRecord>,
}

impl Drop for BusInner {
    fn drop(&mut self) {
        for sink in self.sinks.get_mut().unwrap_or_else(PoisonError::into_inner).iter() {
            sink.close();
        }
    }
}

/// One sink's bounded queue and the task that drains it.
struct SinkQueue {
    sink: Arc<dyn EventSink>,
    capacity: usize,
    queue: Mutex<VecDeque<EventRecord>>,
    ready: Notify,
    started: AtomicBool,
    closed: AtomicBool,
    dropped: AtomicU64,
}

impl SinkQueue {
    /// Queues `event`, shedding a `Health` event first and then the oldest when full, and
    /// starts the drain task when a runtime is available.
    fn push(self: &Arc<Self>, record: EventRecord) {
        {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            if queue.len() >= self.capacity {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if matches!(record.event, KernelEvent::Health(_)) {
                    tracing::warn!(sink.dropped = dropped, "event sink is behind; health dropped");
                    return;
                }
                let health =
                    queue.iter().position(|queued| matches!(queued.event, KernelEvent::Health(_)));
                let shed = match health {
                    Some(index) => queue.remove(index),
                    None => queue.pop_front(),
                };
                let shed = shed.as_ref().map_or("none", |shed| shed.event.name());
                tracing::warn!(
                    event.dropped = shed,
                    sink.dropped = dropped,
                    "event sink is behind; event dropped"
                );
            }
            queue.push_back(record);
        }
        self.ready.notify_one();
        self.start();
    }

    fn start(self: &Arc<Self>) {
        if self.started.load(Ordering::Acquire) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let queue = Arc::clone(self);
        runtime.spawn(async move { queue.drain().await });
    }

    async fn drain(&self) {
        loop {
            let next = self.queue.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
            match next {
                Some(record) => {
                    if let Err(error) = self.sink.emit_record(&record).await {
                        tracing::warn!(event.kind = record.event.name(), error = %error, "event sink failed");
                    }
                }
                None if self.closed.load(Ordering::Acquire) => return,
                None => self.ready.notified().await,
            }
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.ready.notify_one();
    }
}

impl fmt::Debug for EventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventBus")
            .field("subscribers", &self.inner.sender.receiver_count())
            .finish_non_exhaustive()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_CAPACITY)
    }
}

impl EventBus {
    /// A bus whose subscribers may fall `capacity` events behind.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _) = broadcast::channel(capacity);
        Self {
            inner: Arc::new(BusInner {
                sender,
                capacity,
                log: Mutex::new(BusLog { next: 1, history: VecDeque::with_capacity(capacity) }),
                sinks: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Adds a sink that receives every later event, in order, through its own queue of the
    /// bus's capacity. The queue drains on the Tokio runtime of the first publish that finds
    /// one.
    pub fn add_sink(&self, sink: Arc<dyn EventSink>) {
        let queue = Arc::new(SinkQueue {
            sink,
            capacity: self.inner.capacity,
            queue: Mutex::new(VecDeque::new()),
            ready: Notify::new(),
            started: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        });
        queue.start();
        self.inner.sinks.lock().unwrap_or_else(PoisonError::into_inner).push(queue);
    }

    /// Numbers `event`, delivers it to subscribers and sinks, and returns its record. Never
    /// blocks on a slow sink.
    pub fn publish(&self, event: KernelEvent) -> EventRecord {
        let record = {
            let mut log = self.inner.log.lock().unwrap_or_else(PoisonError::into_inner);
            let record = EventRecord { cursor: log.next, at: Utc::now(), event };
            log.next += 1;
            if log.history.len() == self.inner.capacity {
                log.history.pop_front();
            }
            log.history.push_back(record.clone());
            // A send fails only when no one subscribes, which drops nothing anyone awaits.
            let _ = self.inner.sender.send(record.clone());
            record
        };
        let sinks = self.inner.sinks.lock().unwrap_or_else(PoisonError::into_inner).clone();
        for sink in sinks {
            sink.push(record.clone());
        }
        record
    }

    /// Subscribes to every event published from now on.
    pub fn subscribe(&self) -> EventSubscriber {
        EventSubscriber { backlog: VecDeque::new(), receiver: self.inner.sender.subscribe() }
    }

    /// Subscribes from just after `cursor`: the held events after it, then the live ones,
    /// with no gap and no repeat (spec 3.2 §7.6).
    ///
    /// The replay comes from the bus's own history of its last `capacity` events, not from
    /// the stores. A surface that fell further behind gets the oldest held event onward and
    /// re-reads the process table (`ProcessTable::ps`, `GET /api/kernel/processes`) for the
    /// state it missed. A cursor the bus never issued, such as one from before a kernel
    /// restart, replays every held event.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::{EventBus, KernelEvent};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> adk_kernel::Result<()> {
    /// let bus = EventBus::new(16);
    /// let seen = bus.publish(KernelEvent::Governance { frozen: true, reason: None });
    /// let missed = bus.publish(KernelEvent::Governance { frozen: false, reason: None });
    /// let mut subscriber = bus.subscribe_after(seen.cursor).await?;
    /// assert_eq!(subscriber.recv().await.unwrap(), missed);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// None today; a store-backed replay returns its store error.
    pub async fn subscribe_after(&self, cursor: u64) -> Result<EventSubscriber> {
        let log = self.inner.log.lock().unwrap_or_else(PoisonError::into_inner);
        let receiver = self.inner.sender.subscribe();
        let issued = cursor < log.next;
        let backlog = log
            .history
            .iter()
            .filter(|record| !issued || record.cursor > cursor)
            .cloned()
            .collect();
        Ok(EventSubscriber { backlog, receiver })
    }
}

/// A subscription to the [`EventBus`] (spec 3.2 §7.6).
#[derive(Debug)]
pub struct EventSubscriber {
    /// Replayed events, delivered before the live ones.
    backlog: VecDeque<EventRecord>,
    receiver: broadcast::Receiver<EventRecord>,
}

impl EventSubscriber {
    /// The next event, or `None` once the bus is gone. A subscriber that fell more than the
    /// bus's capacity behind skips to the oldest event still held.
    pub async fn recv(&mut self) -> Option<EventRecord> {
        if let Some(record) = self.backlog.pop_front() {
            return Some(record);
        }
        loop {
            match self.receiver.recv().await {
                Ok(record) => return Some(record),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(events.skipped = skipped, "event subscriber fell behind");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// The subscription as a stream that ends with the bus.
    pub fn into_stream(self) -> BoxStream<'static, EventRecord> {
        stream::unfold(self, |mut subscriber| async move {
            subscriber.recv().await.map(|record| (record, subscriber))
        })
        .boxed()
    }
}

/// Writes the events that are audit records to the signed audit log (spec 3.2 §7.6, §9.3),
/// kind `Other(`[`KERNEL_AUDIT_KIND`](crate::audit::KERNEL_AUDIT_KIND)`)` with the
/// `kernel.*` actions of [`crate::audit`].
///
/// | Event | Record |
/// |---|---|
/// | [`KernelEvent::Wake`] that concerned a process | [`audit::WAKE`](crate::audit::WAKE) with `source`, `key`, `matchedPid` (woken, deduplicated, expired) or `newPid` and `trigger` (spawned), `deduplicated`, and `expiry` for an expired wake set; the actor is the source as a service |
/// | Every other event | None: `governance.*` records are written by `freeze_recorded`, and the `kernel.process.*` records by the supervisor, which holds the side-table fields they carry |
///
/// A wake that matched nothing writes no record. The trail's failure mode applies: under
/// `AuditFailureMode::Block` a failed write is an error the bus logs.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
/// use adk_kernel::{AuditSink, EventBus};
///
/// let bus = EventBus::default();
/// bus.add_sink(Arc::new(AuditSink::new(AuditTrail::new(Arc::new(InMemoryAuditRecorder::new())))));
/// ```
#[derive(Debug, Clone)]
pub struct AuditSink {
    trail: AuditTrail,
}

impl AuditSink {
    /// A sink writing to `trail`.
    pub fn new(trail: AuditTrail) -> Self {
        Self { trail }
    }

    /// The trail the sink writes to.
    pub fn trail(&self) -> &AuditTrail {
        &self.trail
    }
}

#[async_trait]
impl EventSink for AuditSink {
    async fn emit(&self, event: &KernelEvent) -> Result<()> {
        let KernelEvent::Wake { source, key, outcome: Some(outcome), .. } = event else {
            return Ok(());
        };
        let mut draft = AuditRecordDraft::new(
            AuditRecordKind::Other(crate::audit::KERNEL_AUDIT_KIND.to_string()),
            crate::audit::WAKE,
        )
        .actor(AuditActor::subject(SubjectRef::service(source.clone())))
        .ext("source", json!(source))
        .ext("key", json!(key))
        .ext("deduplicated", json!(matches!(outcome, WakeOutcome::Deduplicated { .. })));
        draft = match outcome {
            WakeOutcome::Spawned { pid, trigger } => {
                draft.ext("newPid", json!(pid)).ext("trigger", json!(trigger))
            }
            WakeOutcome::Expired { pid, action } => {
                draft.ext("matchedPid", json!(pid)).ext("expiry", json!(action))
            }
            WakeOutcome::Woke { pid } | WakeOutcome::Deduplicated { pid } => {
                draft.ext("matchedPid", json!(pid))
            }
        };
        self.trail.record(draft).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use adk_core::audit_log::InMemoryAuditRecorder;
    use tokio::sync::Semaphore;

    use super::*;
    use crate::wake::ExpiryAction;

    fn governance(frozen: bool) -> KernelEvent {
        KernelEvent::Governance { frozen, reason: None }
    }

    fn health() -> KernelEvent {
        KernelEvent::Health(KernelHealth::default())
    }

    fn wake(outcome: Option<WakeOutcome>) -> KernelEvent {
        KernelEvent::Wake {
            pid: outcome.as_ref().map(|outcome| outcome.pid().to_string()),
            source: "hook:github".into(),
            key: "hook:github:msg_1".into(),
            outcome,
        }
    }

    /// Records every event; each `emit` waits for a permit, so the test decides when the
    /// sink catches up.
    struct GatedSink {
        permits: Semaphore,
        seen: Mutex<Vec<KernelEvent>>,
    }

    #[async_trait]
    impl EventSink for GatedSink {
        async fn emit(&self, event: &KernelEvent) -> Result<()> {
            self.permits.acquire().await.unwrap().forget();
            self.seen.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    async fn seen(sink: &GatedSink, count: usize) -> Vec<KernelEvent> {
        for _ in 0..500 {
            if sink.seen.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        sink.seen.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn a_reconnecting_subscriber_replays_from_its_cursor_without_gap_or_repeat() {
        let bus = EventBus::new(8);
        let first = bus.publish(governance(true));
        let missed = [bus.publish(governance(false)), bus.publish(governance(true))];
        let mut subscriber = bus.subscribe_after(first.cursor).await.unwrap();
        let live = bus.publish(governance(false));
        for expected in missed.iter().chain([&live]) {
            assert_eq!(&subscriber.recv().await.unwrap(), expected);
        }

        let mut current = bus.subscribe_after(live.cursor).await.unwrap();
        let next = bus.publish(governance(true));
        assert_eq!(current.recv().await.unwrap(), next);
    }

    #[tokio::test]
    async fn a_cursor_from_before_a_restart_replays_everything_held() {
        let bus = EventBus::new(2);
        bus.publish(governance(true));
        let held = [bus.publish(governance(false)), bus.publish(governance(true))];
        let mut subscriber = bus.subscribe_after(9_999).await.unwrap();
        for expected in &held {
            assert_eq!(&subscriber.recv().await.unwrap(), expected);
        }
        let mut lagging = bus.subscribe_after(0).await.unwrap();
        assert_eq!(lagging.recv().await.unwrap(), held[0], "only the last two are held");
    }

    #[tokio::test]
    async fn sinks_receive_every_event_in_order() {
        let bus = EventBus::new(16);
        let sink = Arc::new(GatedSink { permits: Semaphore::new(100), seen: Mutex::default() });
        bus.add_sink(sink.clone());
        let events = [governance(true), wake(None), governance(false)];
        for event in &events {
            bus.publish(event.clone());
        }
        assert_eq!(seen(&sink, 3).await, events);
    }

    #[tokio::test]
    async fn a_slow_sink_drops_health_first_and_never_blocks_the_publisher() {
        let bus = EventBus::new(3);
        let sink = Arc::new(GatedSink { permits: Semaphore::new(0), seen: Mutex::default() });
        bus.add_sink(sink.clone());
        // The drain task takes the first event and waits on the gate; three more fill the queue.
        bus.publish(governance(true));
        tokio::time::sleep(Duration::from_millis(50)).await;
        bus.publish(health());
        bus.publish(governance(false));
        bus.publish(health());
        // Full: a new health event is dropped, then each other event sheds a queued health.
        bus.publish(health());
        bus.publish(wake(None));
        bus.publish(governance(true));
        sink.permits.add_permits(100);
        assert_eq!(
            seen(&sink, 4).await,
            [governance(true), governance(false), wake(None), governance(true)]
        );
        // With no health left to shed, the oldest queued event goes.
        let bus = EventBus::new(1);
        let sink = Arc::new(GatedSink { permits: Semaphore::new(0), seen: Mutex::default() });
        bus.add_sink(sink.clone());
        bus.publish(governance(true));
        tokio::time::sleep(Duration::from_millis(50)).await;
        bus.publish(governance(false));
        bus.publish(wake(None));
        sink.permits.add_permits(100);
        assert_eq!(seen(&sink, 2).await, [governance(true), wake(None)]);
    }

    #[tokio::test]
    async fn the_audit_sink_writes_kernel_wake_records() {
        let recorder = Arc::new(InMemoryAuditRecorder::new());
        let sink = AuditSink::new(AuditTrail::new(recorder.clone()));
        let outcomes = [
            WakeOutcome::Woke { pid: "run-1".into() },
            WakeOutcome::Spawned { pid: "run-2".into(), trigger: "triage".into() },
            WakeOutcome::Deduplicated { pid: "run-2".into() },
            WakeOutcome::Expired { pid: "run-3".into(), action: ExpiryAction::Cancel },
        ];
        for outcome in &outcomes {
            sink.emit(&wake(Some(outcome.clone()))).await.unwrap();
        }
        sink.emit(&wake(None)).await.unwrap();
        sink.emit(&governance(true)).await.unwrap();
        sink.emit(&health()).await.unwrap();

        let records = recorder.records();
        assert_eq!(records.len(), 4, "only wakes that concerned a process are records");
        for record in &records {
            assert_eq!(record.kind, AuditRecordKind::Other(crate::audit::KERNEL_AUDIT_KIND.into()));
            assert_eq!(record.action, crate::audit::WAKE);
            assert_eq!(record.actor, AuditActor::subject(SubjectRef::service("hook:github")));
            assert_eq!(record.ext["source"], "hook:github");
            assert_eq!(record.ext["key"], "hook:github:msg_1");
        }
        let fields = |index: usize, field: &str| records[index].ext.get(field).cloned();
        assert_eq!(fields(0, "matchedPid"), Some(json!("run-1")));
        assert_eq!(fields(0, "deduplicated"), Some(json!(false)));
        assert_eq!(fields(1, "newPid"), Some(json!("run-2")));
        assert_eq!(fields(1, "trigger"), Some(json!("triage")));
        assert_eq!(fields(1, "matchedPid"), None);
        assert_eq!(fields(2, "matchedPid"), Some(json!("run-2")));
        assert_eq!(fields(2, "deduplicated"), Some(json!(true)));
        assert_eq!(fields(3, "matchedPid"), Some(json!("run-3")));
        assert_eq!(fields(3, "expiry"), Some(json!("cancel")));
    }

    #[test]
    fn every_event_kind_round_trips_by_its_type_name() {
        for kind in EventKind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
            assert_eq!(kind.as_str().parse::<EventKind>().unwrap(), kind);
            assert_eq!(kind.to_string(), kind.as_str());
        }
        let names: BTreeSet<&str> = EventKind::ALL.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(names.len(), EventKind::ALL.len(), "each kind has its own name");
        for event in [governance(true), health(), wake(None)] {
            assert_eq!(serde_json::to_value(&event).unwrap()["type"], event.kind().as_str());
            assert_eq!(event.name(), event.kind().as_str());
        }
        assert!("Governance".parse::<EventKind>().is_err());
    }

    #[test]
    fn a_filter_selects_by_kind_and_refuses_unknown_type_names() {
        let filter = EventFilter::parse(["decided", "held"]).unwrap();
        assert_eq!(filter.kinds().collect::<Vec<_>>(), [EventKind::Held, EventKind::Decided]);
        assert!(!filter.matches(&governance(true)));
        let error = EventFilter::parse(["held", "helth"]).unwrap_err().to_string();
        assert!(error.contains("`helth`") && error.contains("governance"), "{error}");

        let default = EventFilter::default();
        assert!(!default.matches(&health()) && default.matches(&governance(false)));
        assert_eq!(default.kinds().count(), EventKind::ALL.len() - 1);
        assert_eq!(EventFilter::all().without(EventKind::Health), default);
    }

    #[test]
    fn wake_events_serialize_their_outcome() {
        let event =
            wake(Some(WakeOutcome::Spawned { pid: "run-2".into(), trigger: "triage".into() }));
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "wake");
        assert_eq!(json["pid"], "run-2");
        assert_eq!(json["outcome"], json!({"spawned": {"pid": "run-2", "trigger": "triage"}}));
        assert_eq!(serde_json::from_value::<KernelEvent>(json).unwrap(), event);
        let unmatched = serde_json::to_value(wake(None)).unwrap();
        assert!(unmatched.get("outcome").is_none() && unmatched.get("pid").is_none());
    }
}
