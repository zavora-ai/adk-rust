//! The event bus (spec 3.2 §7.6).
//!
//! Every state change in the kernel is a [`KernelEvent`]: process transitions from the
//! worker's S1 hooks (never polled), wakes, holds and decisions, rule hits, spend,
//! governance and health. [`EventBus`] numbers each one with a cursor and delivers it to
//! subscribers and [`EventSink`]s. The bus is bounded per sink; a slow sink drops `Health`
//! first and never blocks a process. A reconnecting surface catches up by cursor
//! ([`EventBus::subscribe_after`]) rather than by holding a subscription open.
//!
//! Phase 1's sinks are the control plane's SSE stream (`GET /kernel/events`, a subscriber)
//! and the signed audit log ([`AuditSink`]) for the events that are audit records (§9.3).
//! Channel notifiers and outbound webhooks arrive with later phases, as do the `Driver`,
//! `Agenda` and `Observation` events.

use std::fmt;
use std::sync::{Arc, Mutex};

use adk_core::audit_log::AuditTrail;
use adk_core::{RunState, SpendKey, SubjectRef, ToolConfirmationDecision, ToolConfirmationRequest};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::error::{KernelError, Result};
use crate::kernel::KernelHealth;
use crate::partition::PartitionId;

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
    /// A wake event was routed.
    Wake {
        /// The process it woke or started; `None` when it matched nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<String>,
        /// The wake source.
        source: String,
        /// The event's idempotency key.
        key: String,
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
        match self {
            Self::Process { .. } => "process",
            Self::Wake { .. } => "wake",
            Self::Held { .. } => "held",
            Self::Decided { .. } => "decided",
            Self::RuleHit { .. } => "ruleHit",
            Self::Spend { .. } => "spend",
            Self::Governance { .. } => "governance",
            Self::Health(_) => "health",
        }
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
}

/// The kernel's event bus (spec 3.2 §7.6).
///
/// Cloning shares the bus.
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
    next: Mutex<u64>,
    sinks: Mutex<Vec<Arc<dyn EventSink>>>,
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
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self {
            inner: Arc::new(BusInner {
                sender,
                next: Mutex::new(1),
                sinks: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Adds a sink that receives every later event.
    pub fn add_sink(&self, sink: Arc<dyn EventSink>) {
        self.inner.sinks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(sink);
    }

    /// Numbers `event`, delivers it to subscribers and sinks, and returns its record. Never
    /// blocks on a slow sink.
    pub fn publish(&self, event: KernelEvent) -> EventRecord {
        let cursor = {
            let mut next =
                self.inner.next.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let cursor = *next;
            *next += 1;
            cursor
        };
        let record = EventRecord { cursor, at: Utc::now(), event };
        // A send fails only when no one subscribes, which drops nothing anyone awaits.
        let _ = self.inner.sender.send(record.clone());
        // Unimplemented: bus::EventBus::publish delivers to the sinks through bounded
        // per-sink queues once feat/kernel-wake lands (spec 3.2 §7.6).
        record
    }

    /// Subscribes to every event published from now on.
    pub fn subscribe(&self) -> EventSubscriber {
        EventSubscriber { receiver: self.inner.sender.subscribe() }
    }

    /// Subscribes from just after `cursor`, replaying the events since then from the run
    /// store and the kernel stores before the live ones (spec 3.2 §7.6).
    ///
    /// # Errors
    ///
    /// Returns a store error when the replay cannot be read.
    pub async fn subscribe_after(&self, _cursor: u64) -> Result<EventSubscriber> {
        Err(KernelError::Unimplemented { item: "bus::EventBus::subscribe_after" })
    }
}

/// A subscription to the [`EventBus`] (spec 3.2 §7.6).
#[derive(Debug)]
pub struct EventSubscriber {
    receiver: broadcast::Receiver<EventRecord>,
}

impl EventSubscriber {
    /// The next event, or `None` once the bus is gone. A subscriber that fell more than the
    /// bus's capacity behind skips to the oldest event still held.
    pub async fn recv(&mut self) -> Option<EventRecord> {
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
/// kind `Other` with the `kernel.*` actions of [`crate::audit`].
///
/// # Example
///
/// ```rust,ignore
/// let sink = adk_kernel::AuditSink::new(trail);
/// bus.add_sink(std::sync::Arc::new(sink));
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
    async fn emit(&self, _event: &KernelEvent) -> Result<()> {
        Err(KernelError::Unimplemented { item: "bus::AuditSink::emit" })
    }
}
