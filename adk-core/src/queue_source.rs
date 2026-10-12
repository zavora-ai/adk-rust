//! Queue wake-ups: messages from a broker become durable runs.
//!
//! A [`QueueSource`] is a pull-based, at-least-once message source: [`pull`](QueueSource::pull)
//! leases a batch, and each message is then [`ack`](QueueSource::ack)ed (removed) or
//! [`nack`](QueueSource::nack)ed (redelivered later). A message that is neither is
//! redelivered when its lease lapses.
//!
//! [`QueueIngress`] connects a source to a [`DurableRunStore`]:
//!
//! | Step | Effect |
//! |------|--------|
//! | Governance | While the [`GovernanceControl`] refuses work (frozen, or stale under a replicated mirror), nothing is pulled; a freeze that lands mid-batch nacks the rest |
//! | Back-pressure | With `max_pending`, a batch is never larger than the room left under that many unfinished runs from this source, and nothing is pulled while there is none |
//! | Map | The mapper turns the message into a [`NewRun`]; a mapping error nacks the message. The run acts for its user as a service ([`NewRun::with_service_subject`]) |
//! | Enqueue | The run's idempotency key is `queue:{source}:{message id}`, so a redelivered or duplicated message returns the existing run instead of creating a second one |
//! | Ack | Only after the enqueue succeeded (created or existing). A failed enqueue, or one [`OnConflict::Reject`](crate::OnConflict::Reject) refused, nacks the message for redelivery after `retry_after` |
//!
//! The run then executes like any other enqueued run: a worker claims it and runs it
//! under the runner's budget, policy, and kill switch.
//!
//! # Brokers
//!
//! [`InMemoryQueueSource`] ships here. Broker adapters implement [`QueueSource`] in
//! their own crates and prove the contract with `queue_source_conformance_tests!`
//! (feature `conformance`):
//!
//! | Broker | `pull` | `ack` | `nack` | Message id |
//! |--------|--------|-------|--------|------------|
//! | Amazon SQS | `ReceiveMessage` (long poll) | `DeleteMessage` with the receipt handle | `ChangeMessageVisibility` | `MessageId`, or the FIFO `MessageDeduplicationId` |
//! | Google Pub/Sub | `Pull` / streaming pull | `Acknowledge` with the `ackId` | `ModifyAckDeadline` | `messageId` |
//! | NATS JetStream | pull consumer `fetch` | `Ack` | `Nak` with delay | `Nats-Msg-Id` header, or stream sequence |
//! | Kafka | consumer `poll` | commit the offset | seek back, or a retry topic | `topic/partition/offset` or a producer key |
//! | Redis Streams | `XREADGROUP` | `XACK` | leave pending; `XAUTOCLAIM` redelivers | the entry id |
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use std::time::Duration;
//! use adk_core::queue_source::{InMemoryQueueSource, QueueIngress};
//! use adk_core::run_store::{DurableRunStore, InMemoryRunStore, NewRun};
//!
//! # futures::executor::block_on(async {
//! let source = Arc::new(InMemoryQueueSource::new("orders"));
//! let store = Arc::new(InMemoryRunStore::new());
//! let ingress = QueueIngress::new(source.clone(), store.clone(), |message| {
//!     Ok(NewRun::new("default", "shop", "system", "order_agent", message.body_json()?))
//! });
//!
//! source.publish("msg-1", br#"{"orderId": 7}"#.to_vec());
//! source.publish("msg-1", br#"{"orderId": 7}"#.to_vec()); // the broker delivered it twice
//! let first = ingress.poll_once().await?;
//! assert_eq!((first.created.len(), first.existing), (1, 1));
//! assert_eq!(source.unacked(), 0);
//! # Ok::<(), adk_core::AdkError>(())
//! # }).unwrap();
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::governance::GovernanceControl;
use crate::run_store::{DurableRunStore, Enqueued, NewRun, RunFilter, RunState};
use crate::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};

/// Error code for an ack or nack whose receipt is not (or no longer) leased.
pub const QUEUE_UNKNOWN_RECEIPT_CODE: &str = "queue.unknown_receipt";

/// One leased message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueMessage {
    /// The broker's message identifier: stable across redeliveries of one message.
    pub id: String,
    /// The handle that acks or nacks this delivery; changes on every redelivery.
    pub receipt: String,
    /// The raw body.
    pub body: Vec<u8>,
    /// Broker attributes or headers.
    pub attributes: HashMap<String, String>,
    /// How many times the message has been delivered, counting this one.
    pub delivery_count: u32,
}

impl QueueMessage {
    /// Parses the body as JSON.
    ///
    /// # Errors
    ///
    /// Returns an `InvalidInput` error when the body is not JSON.
    pub fn body_json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).map_err(|e| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "queue.invalid_body",
                format!("queue message '{}' is not JSON: {e}", self.id),
            )
        })
    }
}

/// A pull-based, at-least-once message source.
///
/// Every implementation passes `adk_core::queue_source_conformance_tests!` (feature
/// `conformance`).
#[async_trait]
pub trait QueueSource: Send + Sync {
    /// A stable name for this source, used in idempotency keys and logs.
    fn name(&self) -> &str;

    /// Leases up to `max_messages` messages, waiting up to `wait` for the first one.
    ///
    /// A leased message is not returned again until it is nacked or its lease lapses.
    ///
    /// # Errors
    ///
    /// Returns an error when the broker cannot be reached.
    async fn pull(&self, max_messages: usize, wait: Duration) -> Result<Vec<QueueMessage>>;

    /// Removes the message leased under `receipt`.
    ///
    /// # Errors
    ///
    /// Returns [`QUEUE_UNKNOWN_RECEIPT_CODE`] when the lease lapsed or the receipt is unknown;
    /// the message will be redelivered.
    async fn ack(&self, receipt: &str) -> Result<()>;

    /// Returns the message leased under `receipt` for redelivery after `redeliver_after`.
    ///
    /// # Errors
    ///
    /// Same as [`ack`](Self::ack).
    async fn nack(&self, receipt: &str, redeliver_after: Duration) -> Result<()>;
}

/// Builds the error for an unknown or lapsed receipt.
///
/// # Example
///
/// ```rust
/// let error = adk_core::queue_source::unknown_receipt_error("r-1");
/// assert_eq!(error.code, adk_core::queue_source::QUEUE_UNKNOWN_RECEIPT_CODE);
/// ```
pub fn unknown_receipt_error(receipt: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Conflict,
        QUEUE_UNKNOWN_RECEIPT_CODE,
        format!(
            "queue receipt '{receipt}' is not leased; its lease lapsed or it was already acked, \
             so the message is redelivered and handled again"
        ),
    )
}

#[derive(Debug)]
struct Stored {
    id: String,
    body: Vec<u8>,
    attributes: HashMap<String, String>,
    visible_at: DateTime<Utc>,
    receipt: Option<String>,
    delivery_count: u32,
    seq: u64,
}

#[derive(Debug, Default)]
struct QueueTable {
    messages: Vec<Stored>,
    acked: Vec<String>,
    next_seq: u64,
}

/// Process-local [`QueueSource`] for tests, examples, and single-process deployments.
///
/// [`publish`](Self::publish) appends a message; publishing one id twice models a
/// broker's duplicate delivery. A pulled message is leased for the visibility timeout
/// (default 30 seconds).
#[derive(Debug)]
pub struct InMemoryQueueSource {
    name: String,
    table: std::sync::Mutex<QueueTable>,
    published: tokio::sync::Notify,
    visibility_timeout: Duration,
    clock: Arc<dyn Clock>,
}

impl InMemoryQueueSource {
    /// Creates an empty source named `name` that reads the system clock.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            table: std::sync::Mutex::new(QueueTable::default()),
            published: tokio::sync::Notify::new(),
            visibility_timeout: Duration::from_secs(30),
            clock: Arc::new(SystemClock),
        }
    }

    /// Sets how long a pulled message stays leased before it is redelivered.
    pub fn with_visibility_timeout(mut self, timeout: Duration) -> Self {
        self.visibility_timeout = timeout;
        self
    }

    /// Sets the clock that decides lease expiry and nack delays.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QueueTable> {
        self.table.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Appends a message.
    pub fn publish(&self, id: impl Into<String>, body: impl Into<Vec<u8>>) {
        self.publish_with_attributes(id, body, HashMap::new());
    }

    /// Appends a message with attributes.
    pub fn publish_with_attributes(
        &self,
        id: impl Into<String>,
        body: impl Into<Vec<u8>>,
        attributes: HashMap<String, String>,
    ) {
        let now = self.clock.now();
        let mut table = self.lock();
        let seq = table.next_seq;
        table.next_seq += 1;
        table.messages.push(Stored {
            id: id.into(),
            body: body.into(),
            attributes,
            visible_at: now,
            receipt: None,
            delivery_count: 0,
            seq,
        });
        drop(table);
        self.published.notify_waiters();
    }

    /// Messages not yet acked, leased or not.
    pub fn unacked(&self) -> usize {
        self.lock().messages.len()
    }

    /// The ids of acked messages, in ack order.
    pub fn acked(&self) -> Vec<String> {
        self.lock().acked.clone()
    }

    fn take_visible(&self, max_messages: usize) -> Vec<QueueMessage> {
        let now = self.clock.now();
        let lease_until = now
            + chrono::Duration::from_std(self.visibility_timeout).unwrap_or(chrono::Duration::MAX);
        let mut table = self.lock();
        table.messages.sort_by_key(|stored| stored.seq);
        table
            .messages
            .iter_mut()
            .filter(|stored| stored.visible_at <= now)
            .take(max_messages)
            .map(|stored| {
                let receipt = uuid::Uuid::new_v4().to_string();
                stored.receipt = Some(receipt.clone());
                stored.visible_at = lease_until;
                stored.delivery_count += 1;
                QueueMessage {
                    id: stored.id.clone(),
                    receipt,
                    body: stored.body.clone(),
                    attributes: stored.attributes.clone(),
                    delivery_count: stored.delivery_count,
                }
            })
            .collect()
    }

    fn leased_index(table: &QueueTable, receipt: &str, now: DateTime<Utc>) -> Result<usize> {
        table
            .messages
            .iter()
            .position(|stored| {
                stored.receipt.as_deref() == Some(receipt) && stored.visible_at > now
            })
            .ok_or_else(|| unknown_receipt_error(receipt))
    }
}

#[async_trait]
impl QueueSource for InMemoryQueueSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn pull(&self, max_messages: usize, wait: Duration) -> Result<Vec<QueueMessage>> {
        if max_messages == 0 {
            return Ok(Vec::new());
        }
        let notified = self.published.notified();
        let batch = self.take_visible(max_messages);
        if !batch.is_empty() || wait.is_zero() {
            return Ok(batch);
        }
        let _ = tokio::time::timeout(wait, notified).await;
        Ok(self.take_visible(max_messages))
    }

    async fn ack(&self, receipt: &str) -> Result<()> {
        let now = self.clock.now();
        let mut table = self.lock();
        let index = Self::leased_index(&table, receipt, now)?;
        let stored = table.messages.remove(index);
        table.acked.push(stored.id);
        Ok(())
    }

    async fn nack(&self, receipt: &str, redeliver_after: Duration) -> Result<()> {
        let now = self.clock.now();
        let mut table = self.lock();
        let index = Self::leased_index(&table, receipt, now)?;
        let stored = &mut table.messages[index];
        stored.receipt = None;
        stored.visible_at =
            now + chrono::Duration::from_std(redeliver_after).unwrap_or(chrono::Duration::MAX);
        Ok(())
    }
}

/// Turns a queue message into the run it wakes.
pub type QueueRunMapper = Arc<dyn Fn(&QueueMessage) -> Result<NewRun> + Send + Sync>;

/// What one [`QueueIngress::poll_once`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngressReport {
    /// Messages pulled.
    pub pulled: usize,
    /// Run ids created, one per new message.
    pub created: Vec<String>,
    /// Messages whose run already existed (redeliveries and duplicates); acked.
    pub existing: usize,
    /// Messages nacked for redelivery.
    pub nacked: usize,
    /// Why nothing was pulled, when governance or back-pressure held the batch.
    pub held: Option<String>,
}

/// Enqueues each message of a [`QueueSource`] into a [`DurableRunStore`]; see the
/// [module documentation](self).
#[derive(Clone)]
pub struct QueueIngress {
    source: Arc<dyn QueueSource>,
    store: Arc<dyn DurableRunStore>,
    mapper: QueueRunMapper,
    governance: Option<GovernanceControl>,
    batch_size: usize,
    wait: Duration,
    retry_after: Duration,
    max_pending: Option<usize>,
}

impl std::fmt::Debug for QueueIngress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueueIngress")
            .field("source", &self.source.name())
            .field("batch_size", &self.batch_size)
            .field("wait", &self.wait)
            .field("retry_after", &self.retry_after)
            .field("max_pending", &self.max_pending)
            .finish()
    }
}

impl QueueIngress {
    /// Creates an ingress with a batch of 10, a 1-second pull wait, and a 30-second
    /// redelivery delay after a failed enqueue.
    pub fn new(
        source: Arc<dyn QueueSource>,
        store: Arc<dyn DurableRunStore>,
        mapper: impl Fn(&QueueMessage) -> Result<NewRun> + Send + Sync + 'static,
    ) -> Self {
        Self {
            source,
            store,
            mapper: Arc::new(mapper),
            governance: None,
            batch_size: 10,
            wait: Duration::from_secs(1),
            retry_after: Duration::from_secs(30),
            max_pending: None,
        }
    }

    /// Holds every pull while `governance` refuses work.
    pub fn with_governance(mut self, governance: GovernanceControl) -> Self {
        self.governance = Some(governance);
        self
    }

    /// Sets the most messages pulled per batch.
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size.max(1);
        self
    }

    /// Sets how long one pull waits for the first message.
    pub fn with_pull_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// Sets how long a nacked message waits before redelivery.
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = retry_after;
        self
    }

    /// Stops pulling while `max_pending` runs from this source are unfinished.
    pub fn with_max_pending(mut self, max_pending: usize) -> Self {
        self.max_pending = Some(max_pending);
        self
    }

    /// The idempotency key of the run a message wakes.
    ///
    /// # Example
    ///
    /// ```rust
    /// assert_eq!(adk_core::queue_source::QueueIngress::idempotency_key("orders", "m-1"), "queue:orders:m-1");
    /// ```
    pub fn idempotency_key(source: &str, message_id: &str) -> String {
        format!("queue:{source}:{message_id}")
    }

    /// Unfinished runs this source created.
    async fn pending_runs(&self) -> Result<usize> {
        let prefix = format!("queue:{}:", self.source.name());
        let unfinished = self
            .store
            .list(RunFilter::default().states([
                RunState::Queued,
                RunState::Running,
                RunState::Waiting,
            ]))
            .await?;
        Ok(unfinished
            .iter()
            .filter(|run| {
                run.idempotency_key.as_deref().is_some_and(|key| key.starts_with(&prefix))
            })
            .count())
    }

    /// Pulls one batch and enqueues each message; see the [module documentation](self).
    ///
    /// # Errors
    ///
    /// Returns an error when the pull fails. Per-message failures nack the message and
    /// are counted in [`IngressReport::nacked`].
    pub async fn poll_once(&self) -> Result<IngressReport> {
        let mut report = IngressReport::default();
        if let Some(governance) = &self.governance
            && let Err(error) = governance.check()
        {
            report.held = Some(error.code.to_string());
            return Ok(report);
        }
        let mut batch_size = self.batch_size;
        if let Some(max_pending) = self.max_pending {
            let room = max_pending.saturating_sub(self.pending_runs().await?);
            if room == 0 {
                report.held = Some("queue.max_pending".to_string());
                return Ok(report);
            }
            batch_size = batch_size.min(room);
        }
        let batch = self.source.pull(batch_size, self.wait).await?;
        report.pulled = batch.len();
        for message in batch {
            match self.enqueue(&message).await {
                Ok(Enqueued::Created(record)) => {
                    self.settle_ack(&message).await;
                    report.created.push(record.run_id);
                }
                Ok(Enqueued::Existing(_)) => {
                    self.settle_ack(&message).await;
                    report.existing += 1;
                }
                Ok(Enqueued::Rejected { blocking_run_id }) => {
                    tracing::debug!(queue.message_id = %message.id, run.blocking_id = %blocking_run_id, "wake-up rejected by its concurrency key; nacking");
                    self.settle_nack(&message).await;
                    report.nacked += 1;
                }
                Err(error) => {
                    tracing::warn!(queue.source = %self.source.name(), queue.message_id = %message.id, error = %error, "wake-up not enqueued; nacking for redelivery");
                    self.settle_nack(&message).await;
                    report.nacked += 1;
                }
            }
        }
        Ok(report)
    }

    async fn enqueue(&self, message: &QueueMessage) -> Result<Enqueued> {
        let mut run = (self.mapper)(message)?.with_service_subject();
        if let Some(governance) = &self.governance {
            governance.check()?;
        }
        run.idempotency_key = Some(Self::idempotency_key(self.source.name(), &message.id));
        self.store.enqueue(run).await
    }

    async fn settle_ack(&self, message: &QueueMessage) {
        if let Err(error) = self.source.ack(&message.receipt).await {
            // The run exists, so the redelivery this causes finds it and is acked then.
            tracing::warn!(queue.message_id = %message.id, error = %error, "ack failed after enqueue");
        }
    }

    async fn settle_nack(&self, message: &QueueMessage) {
        if let Err(error) = self.source.nack(&message.receipt, self.retry_after).await {
            tracing::warn!(queue.message_id = %message.id, error = %error, "nack failed; the lease lapses instead");
        }
    }

    /// Polls until `shutdown` resolves, sleeping `idle` after a batch that pulled nothing.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use std::time::Duration;
    /// # use adk_core::queue_source::{InMemoryQueueSource, QueueIngress};
    /// # use adk_core::run_store::{InMemoryRunStore, NewRun};
    /// # async fn example() {
    /// # let ingress = QueueIngress::new(Arc::new(InMemoryQueueSource::new("q")), Arc::new(InMemoryRunStore::new()),
    /// #     |m| Ok(NewRun::new("default", "app", "user", "agent", m.body_json()?)));
    /// let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    /// let task = tokio::spawn(async move {
    ///     ingress.run(Duration::from_millis(200), async { let _ = stopped.await; }).await
    /// });
    /// stop.send(()).ok();
    /// task.await.unwrap();
    /// # }
    /// ```
    pub async fn run(&self, idle: Duration, shutdown: impl std::future::Future<Output = ()>) {
        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            let sleep = match self.poll_once().await {
                Ok(report) if report.pulled > 0 => Duration::ZERO,
                Ok(_) => idle,
                Err(error) => {
                    tracing::warn!(queue.source = %self.source.name(), error = %error, "queue pull failed");
                    idle
                }
            };
            let pause = std::pin::pin!(tokio::time::sleep(sleep));
            if let futures::future::Either::Left(_) =
                futures::future::select(shutdown.as_mut(), pause).await
            {
                return;
            }
        }
    }
}
