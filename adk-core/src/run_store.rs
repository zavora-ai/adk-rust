//! Durable runs: a queue of agent or workflow runs that workers claim under fenced leases.
//!
//! A [`DurableRunStore`] holds the durable truth about every run: its input, state,
//! attempt count, lease, checkpoint, and outcome. Workers [`claim`](DurableRunStore::claim)
//! runs, [`heartbeat`](DurableRunStore::heartbeat) while they work, and
//! [`finish`](DurableRunStore::finish) them. Every worker write carries the run's lease
//! epoch, and a store refuses a write whose epoch is not current with
//! `run_store.stale_epoch`, so a worker that stalled and lost its run can never
//! overwrite the work of the worker that recovered it.
//!
//! # Lifecycle
//!
//! | From | Event | To | Attempt | Epoch |
//! |------|-------|----|---------|-------|
//! | — | `enqueue` | `Queued` | 0 | 0 |
//! | `Queued` | `claim` (`not_before` passed, no earlier run holds its concurrency key) | `Running` | + 1 | + 1 |
//! | `Running` | `claim` after the lease expired (recovery) | `Running` | + 1 | + 1 |
//! | `Running` | `wait` | `Waiting` (lease released) | — | — |
//! | `Waiting` | `claim` once the wait's `until` passed or `wake` was called | `Running` | — | + 1 |
//! | `Running` | `finish(Completed)` | `Completed` | — | — |
//! | `Running` | `finish(Failed { retryable: true })` with attempts left and no cancel request | `Queued` | — | — |
//! | `Running` | `finish(Failed)` otherwise | `Failed` | — | — |
//! | `Running` | `finish(Cancelled)` | `Cancelled` | — | — |
//! | `Queued`, `Waiting` | `request_cancel` | `Cancelled` | — | — |
//! | `Running` | `request_cancel` | `Running`, `cancel_requested` (seen by the next heartbeat) | — | — |
//!
//! A recovery claim of a run that already used `max_attempts` attempts fails the run
//! with an `attempts exhausted` error instead of claiming it.
//!
//! A `wake` that arrives while the run is still `Running` (an approval decided before
//! the worker parked the run) is kept in `wake_pending`, and the next `wait` parks the
//! run as already woken, so the wake is never lost.
//!
//! # Double texting
//!
//! Runs that share a `concurrency_key` in one queue run one at a time, in enqueue
//! order. [`OnConflict`] picks what `enqueue` does when an unfinished run holds the key:
//!
//! | Policy | Effect |
//! |--------|--------|
//! | [`Reject`](OnConflict::Reject) | Returns [`Enqueued::Rejected`] naming the oldest unfinished run |
//! | [`Enqueue`](OnConflict::Enqueue) | Queues the run behind the unfinished ones |
//! | [`Interrupt`](OnConflict::Interrupt) | Cancels queued and waiting runs, requests cancellation of running ones, and queues the run behind them |
//!
//! # Backend primitives
//!
//! | Operation | SQL | Redis | MongoDB | Neo4j, Firestore |
//! |-----------|-----|-------|---------|------------------|
//! | Idempotent `enqueue` | Unique index on `(queue, idempotency_key)`, `INSERT ... ON CONFLICT DO NOTHING` then read | Lua: check the key index, then write | Unique index, `insert_one`, read on duplicate-key error | Transaction over the key node or document |
//! | Single-winner `claim` | `SELECT ... FOR UPDATE SKIP LOCKED` (PostgreSQL) or `BEGIN IMMEDIATE` (SQLite), then `UPDATE` | One Lua script per claim | `findOneAndUpdate` on state and epoch | Transaction that reads the candidate and writes the new epoch |
//! | Fenced writes | `UPDATE ... WHERE run_id = $1 AND lease_epoch = $2 AND state = 'running'` | Lua compare of the epoch field | `updateOne` filtered on `leaseEpoch` | Transaction that checks the epoch |
//! | `prune` | `DELETE ... WHERE state IN (...) AND updated_at < $1` | Sorted set of finished runs by time | `deleteMany` | Batched transactional deletes |

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::lease::deadline;
use crate::principal::ActingContext;
use crate::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};

/// Error code returned when a worker write carries an epoch that is not current.
pub const RUN_STALE_EPOCH_CODE: &str = "run_store.stale_epoch";
/// Error code returned for an unknown run identifier.
pub const RUN_NOT_FOUND_CODE: &str = "run_store.not_found";
/// Error code returned for a [`NewRun`] that fails validation.
pub const INVALID_RUN_CODE: &str = "run_store.invalid_run";

/// Where a run is in its lifecycle.
///
/// # Example
///
/// ```rust
/// use adk_core::run_store::RunState;
///
/// assert!(RunState::Completed.is_terminal());
/// assert!(!RunState::Waiting.is_terminal());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum RunState {
    /// Waiting for a worker to claim it.
    Queued,
    /// Claimed by a worker under a lease.
    Running,
    /// Parked until a timer passes or `wake` is called; holds no lease.
    Waiting,
    /// Finished with a result.
    Completed,
    /// Finished with an error.
    Failed,
    /// Cancelled before or during execution.
    Cancelled,
}

impl RunState {
    /// Whether the run has finished and never changes state again.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Where a run's progress is checkpointed, so a recovering worker resumes it.
///
/// # Example
///
/// ```rust
/// use adk_core::run_store::RunCheckpointRef;
///
/// let checkpoint = RunCheckpointRef::Graph { thread_id: "t1".into(), checkpoint_id: Some("c7".into()) };
/// assert_eq!(serde_json::to_value(&checkpoint).unwrap()["kind"], "graph");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum RunCheckpointRef {
    /// A session, optionally at one invocation.
    Session {
        /// The session identifier.
        session_id: String,
        /// The last invocation recorded in the session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invocation_id: Option<String>,
    },
    /// A session whose run is parked until tool calls held for approval are decided.
    ///
    /// A worker records it before [`wait`](DurableRunStore::wait)ing; the claim that
    /// follows a [`wake`](DurableRunStore::wake) resumes the same session, and the agent
    /// dispatches each held call that has a decision.
    AwaitingApproval {
        /// The session identifier.
        session_id: String,
        /// The invocation that held the calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invocation_id: Option<String>,
        /// The fingerprints of the held calls, the keys their decisions are recorded under.
        fingerprints: Vec<String>,
        /// When the hold expires; `None` waits until woken.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<DateTime<Utc>>,
    },
    /// A graph thread, optionally at one checkpoint.
    Graph {
        /// The graph thread identifier.
        thread_id: String,
        /// The checkpoint within the thread.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint_id: Option<String>,
    },
}

/// What `enqueue` does when an unfinished run holds the same concurrency key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum OnConflict {
    /// Refuse the new run.
    Reject,
    /// Queue the new run behind the unfinished ones.
    #[default]
    Enqueue,
    /// Cancel the unfinished runs and queue the new one behind them.
    Interrupt,
}

/// A run to enqueue.
///
/// # Example
///
/// ```rust
/// use adk_core::run_store::{NewRun, OnConflict};
/// use serde_json::json;
///
/// let run = NewRun::new("default", "billing", "user-1", "invoice_agent", json!({"text": "hi"}))
///     .with_idempotency_key("webhook:evt_123")
///     .with_concurrency_key("thread:42", OnConflict::Interrupt)
///     .with_max_attempts(5);
/// assert!(run.validate().is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRun {
    /// The queue workers claim from.
    pub queue: String,
    /// The application the run belongs to.
    pub app_name: String,
    /// The user the run belongs to.
    pub user_id: String,
    /// The session the run continues, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The agent or workflow name to run.
    pub target: String,
    /// The run's input.
    pub input: Value,
    /// Unique per queue: enqueueing an existing key returns the existing run until it is pruned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Runs sharing a key in one queue run one at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency_key: Option<String>,
    /// What happens when an unfinished run holds `concurrency_key`.
    #[serde(default)]
    pub on_conflict: OnConflict,
    /// The run is not claimed before this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<DateTime<Utc>>,
    /// The most attempts the run may take, counting recoveries; at least 1.
    pub max_attempts: u32,
    /// The agent identity the run acts under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<ActingContext>,
}

impl NewRun {
    /// The `max_attempts` a run gets from [`NewRun::new`].
    pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

    /// Creates a run with no keys, no delay, no principal, [`OnConflict::Enqueue`], and
    /// [`DEFAULT_MAX_ATTEMPTS`](Self::DEFAULT_MAX_ATTEMPTS).
    pub fn new(
        queue: impl Into<String>,
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        target: impl Into<String>,
        input: Value,
    ) -> Self {
        Self {
            queue: queue.into(),
            app_name: app_name.into(),
            user_id: user_id.into(),
            session_id: None,
            target: target.into(),
            input,
            idempotency_key: None,
            concurrency_key: None,
            on_conflict: OnConflict::Enqueue,
            not_before: None,
            max_attempts: Self::DEFAULT_MAX_ATTEMPTS,
            principal: None,
        }
    }

    /// Sets the session the run continues.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Sets the idempotency key.
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Sets the concurrency key and the policy applied when it is held.
    pub fn with_concurrency_key(mut self, key: impl Into<String>, on_conflict: OnConflict) -> Self {
        self.concurrency_key = Some(key.into());
        self.on_conflict = on_conflict;
        self
    }

    /// Delays the run until `at`.
    pub fn with_not_before(mut self, at: DateTime<Utc>) -> Self {
        self.not_before = Some(at);
        self
    }

    /// Sets the most attempts the run may take.
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Sets the agent identity the run acts under.
    pub fn with_principal(mut self, principal: ActingContext) -> Self {
        self.principal = Some(principal);
        self
    }

    /// Checks the fields every store requires.
    ///
    /// # Errors
    ///
    /// Returns [`INVALID_RUN_CODE`] when `queue`, `app_name`, `user_id`, or `target` is
    /// empty, a key is present but empty, or `max_attempts` is zero.
    pub fn validate(&self) -> Result<()> {
        let empty = [
            ("queue", self.queue.is_empty()),
            ("appName", self.app_name.is_empty()),
            ("userId", self.user_id.is_empty()),
            ("target", self.target.is_empty()),
            ("idempotencyKey", self.idempotency_key.as_deref() == Some("")),
            ("concurrencyKey", self.concurrency_key.as_deref() == Some("")),
        ];
        if let Some((field, _)) = empty.iter().find(|(_, is_empty)| *is_empty) {
            return Err(invalid_run_error(&format!("{field} must not be empty")));
        }
        if self.max_attempts == 0 {
            return Err(invalid_run_error("maxAttempts must be at least 1"));
        }
        Ok(())
    }
}

/// A run as the store holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// The store-assigned run identifier.
    pub run_id: String,
    /// The queue workers claim from.
    pub queue: String,
    /// The application the run belongs to.
    pub app_name: String,
    /// The user the run belongs to.
    pub user_id: String,
    /// The session the run continues, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The agent or workflow name to run.
    pub target: String,
    /// The run's input.
    pub input: Value,
    /// The idempotency key the run was enqueued with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The concurrency key the run was enqueued with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency_key: Option<String>,
    /// The double-texting policy the run was enqueued with.
    #[serde(default)]
    pub on_conflict: OnConflict,
    /// Not claimed before this time; while `Waiting`, the time the wait ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<DateTime<Utc>>,
    /// The most attempts the run may take.
    pub max_attempts: u32,
    /// The agent identity the run acts under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<ActingContext>,
    /// Where the run is in its lifecycle.
    pub state: RunState,
    /// Attempts started so far, counting recoveries; 0 until first claimed.
    pub attempt: u32,
    /// The worker holding the lease while `Running`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_owner: Option<String>,
    /// The fencing epoch of the latest claim; 0 until first claimed.
    pub lease_epoch: u64,
    /// When the current lease lapses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// The last claim or heartbeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_at: Option<DateTime<Utc>>,
    /// Where the run's progress is checkpointed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<RunCheckpointRef>,
    /// Whether cancellation was requested while the run was `Running`.
    #[serde(default)]
    pub cancel_requested: bool,
    /// Whether `wake` arrived while the run was `Running`, before it parked.
    #[serde(default)]
    pub wake_pending: bool,
    /// The result of a completed run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error of the latest failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When the run was enqueued.
    pub created_at: DateTime<Utc>,
    /// When the run last changed; for a finished run, when it finished.
    pub updated_at: DateTime<Utc>,
    /// Increases by one on every change.
    pub revision: u64,
}

impl RunRecord {
    /// Builds the record a store writes for a newly enqueued run.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::run_store::{NewRun, RunRecord, RunState};
    ///
    /// let run = NewRun::new("q", "app", "user", "agent", serde_json::Value::Null);
    /// let record = RunRecord::queued("run-1", run, chrono::Utc::now());
    /// assert_eq!((record.state, record.attempt, record.lease_epoch, record.revision), (RunState::Queued, 0, 0, 1));
    /// ```
    pub fn queued(run_id: impl Into<String>, run: NewRun, now: DateTime<Utc>) -> Self {
        Self {
            run_id: run_id.into(),
            queue: run.queue,
            app_name: run.app_name,
            user_id: run.user_id,
            session_id: run.session_id,
            target: run.target,
            input: run.input,
            idempotency_key: run.idempotency_key,
            concurrency_key: run.concurrency_key,
            on_conflict: run.on_conflict,
            not_before: run.not_before,
            max_attempts: run.max_attempts,
            principal: run.principal,
            state: RunState::Queued,
            attempt: 0,
            lease_owner: None,
            lease_epoch: 0,
            lease_expires_at: None,
            heartbeat_at: None,
            checkpoint: None,
            cancel_requested: false,
            wake_pending: false,
            result: None,
            error: None,
            created_at: now,
            updated_at: now,
            revision: 1,
        }
    }
}

/// The result of [`DurableRunStore::enqueue`].
#[derive(Debug, Clone, PartialEq)]
pub enum Enqueued {
    /// A new run was created.
    Created(RunRecord),
    /// A run with the same idempotency key already exists in the queue.
    Existing(RunRecord),
    /// [`OnConflict::Reject`] refused the run because an unfinished run holds its concurrency key.
    Rejected {
        /// The oldest unfinished run holding the key.
        blocking_run_id: String,
    },
}

impl Enqueued {
    /// Returns the created or existing record, or `None` when rejected.
    pub fn record(&self) -> Option<&RunRecord> {
        match self {
            Self::Created(record) | Self::Existing(record) => Some(record),
            Self::Rejected { .. } => None,
        }
    }
}

/// A run a worker claimed, with the epoch its writes must carry.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedRun {
    /// The run after the claim.
    pub record: RunRecord,
    /// The fencing epoch of this claim.
    pub epoch: u64,
}

/// How a worker ends an attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    /// The run finished with a result.
    Completed(Value),
    /// The attempt failed; `retryable` failures requeue while attempts remain.
    Failed {
        /// What went wrong.
        error: String,
        /// Whether another attempt may succeed.
        retryable: bool,
    },
    /// The run stopped after a cancellation request.
    Cancelled,
}

/// Selects runs for [`DurableRunStore::list`]. Every set field must match.
///
/// # Example
///
/// ```rust
/// use adk_core::run_store::{RunFilter, RunState};
///
/// let filter = RunFilter::default().queue("default").states([RunState::Queued, RunState::Running]).limit(50);
/// assert_eq!(filter.limit, Some(50));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunFilter {
    /// Only runs in this queue.
    pub queue: Option<String>,
    /// Only runs in one of these states; empty matches every state.
    pub states: Vec<RunState>,
    /// Only runs of this application.
    pub app_name: Option<String>,
    /// Only runs of this user.
    pub user_id: Option<String>,
    /// Only runs continuing this session.
    pub session_id: Option<String>,
    /// Only runs holding this concurrency key.
    pub concurrency_key: Option<String>,
    /// At most this many runs, oldest first.
    pub limit: Option<usize>,
}

impl RunFilter {
    /// Only runs in `queue`.
    pub fn queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = Some(queue.into());
        self
    }

    /// Only runs in one of `states`.
    pub fn states(mut self, states: impl IntoIterator<Item = RunState>) -> Self {
        self.states = states.into_iter().collect();
        self
    }

    /// Only runs of `app_name`.
    pub fn app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = Some(app_name.into());
        self
    }

    /// Only runs of `user_id`.
    pub fn user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// Only runs continuing `session_id`.
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Only runs holding `concurrency_key`.
    pub fn concurrency_key(mut self, key: impl Into<String>) -> Self {
        self.concurrency_key = Some(key.into());
        self
    }

    /// At most `limit` runs.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Whether `record` matches every set field except `limit`.
    pub fn matches(&self, record: &RunRecord) -> bool {
        fn field(want: &Option<String>, have: Option<&str>) -> bool {
            want.as_deref().is_none_or(|want| have == Some(want))
        }
        field(&self.queue, Some(&record.queue))
            && (self.states.is_empty() || self.states.contains(&record.state))
            && field(&self.app_name, Some(&record.app_name))
            && field(&self.user_id, Some(&record.user_id))
            && field(&self.session_id, record.session_id.as_deref())
            && field(&self.concurrency_key, record.concurrency_key.as_deref())
    }
}

/// The durable queue of runs.
///
/// Named `DurableRunStore` so it does not clash with `adk_server::background::RunStore`,
/// the server's process-local record of background runs.
///
/// Every backend passes `adk_core::run_store_conformance_tests!` (feature `conformance`).
/// The [module documentation](crate::run_store) lists the lifecycle and the primitive
/// each backend uses for each guarantee.
#[async_trait]
pub trait DurableRunStore: Send + Sync {
    /// Adds a run.
    ///
    /// An existing run with the same `(queue, idempotency_key)` is returned as
    /// [`Enqueued::Existing`] before any concurrency check. Concurrent enqueues of one
    /// key create exactly one run.
    ///
    /// # Errors
    ///
    /// Returns [`INVALID_RUN_CODE`] when [`NewRun::validate`] fails.
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued>;

    /// Claims the oldest claimable run in `queue` for `worker` with a lease of `ttl`.
    ///
    /// Claimable, oldest enqueued first: a `Queued` run whose `not_before` passed and
    /// that no earlier unfinished run with its concurrency key precedes; a `Waiting` run
    /// whose wait ended or that was woken; a `Running` run whose lease expired
    /// (recovery). The run's epoch increases by one on every claim. Concurrent claims
    /// never return the same run at the same epoch.
    ///
    /// # Errors
    ///
    /// Returns `run_store.invalid_ttl` for a zero `ttl`.
    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>>;

    /// Extends the lease to `ttl` from now and returns whether cancellation was requested.
    ///
    /// # Errors
    ///
    /// Returns [`RUN_STALE_EPOCH_CODE`] when the run is not `Running` at `epoch`, and
    /// [`RUN_NOT_FOUND_CODE`] for an unknown run.
    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool>;

    /// Records where the run's progress is checkpointed.
    ///
    /// # Errors
    ///
    /// Same as [`heartbeat`](Self::heartbeat).
    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<()>;

    /// Releases the lease and parks the run until `until` passes or [`wake`](Self::wake)
    /// is called. `None` parks it until woken.
    ///
    /// # Errors
    ///
    /// Same as [`heartbeat`](Self::heartbeat).
    async fn wait(&self, run_id: &str, epoch: u64, until: Option<DateTime<Utc>>) -> Result<()>;

    /// Makes a `Waiting` run claimable now; records a pending wake for a `Running` run;
    /// does nothing for a queued or finished run.
    ///
    /// # Errors
    ///
    /// Returns [`RUN_NOT_FOUND_CODE`] for an unknown run.
    async fn wake(&self, run_id: &str) -> Result<()>;

    /// Ends the attempt with `outcome` and returns the updated run.
    ///
    /// `Failed { retryable: true }` requeues the run while `attempt < max_attempts` and
    /// no cancellation was requested; every other outcome is terminal.
    ///
    /// # Errors
    ///
    /// Same as [`heartbeat`](Self::heartbeat).
    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord>;

    /// Cancels a queued or waiting run, or flags a running one for its worker to stop.
    /// A finished run is left unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`RUN_NOT_FOUND_CODE`] for an unknown run.
    async fn request_cancel(&self, run_id: &str) -> Result<()>;

    /// Returns a run, or `None` when it does not exist or was pruned.
    async fn get(&self, run_id: &str) -> Result<Option<RunRecord>>;

    /// Returns the runs matching `filter`, oldest enqueued first.
    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>>;

    /// Deletes finished runs whose `updated_at` is before `finished_before` and returns
    /// how many were deleted. Their idempotency keys become free again.
    async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64>;
}

/// Builds the error a run store returns when a worker write carries a stale epoch.
///
/// # Example
///
/// ```rust
/// let error = adk_core::run_store::run_stale_epoch_error("run-1", 2);
/// assert_eq!(error.code, adk_core::run_store::RUN_STALE_EPOCH_CODE);
/// ```
pub fn run_stale_epoch_error(run_id: &str, epoch: u64) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Conflict,
        RUN_STALE_EPOCH_CODE,
        format!(
            "run '{run_id}' is no longer held at epoch {epoch}; another worker recovered it or it \
             finished, so stop work on it without writing results"
        ),
    )
}

/// Builds the error a run store returns for an unknown run.
///
/// # Example
///
/// ```rust
/// let error = adk_core::run_store::run_not_found_error("run-1");
/// assert!(error.is_not_found());
/// ```
pub fn run_not_found_error(run_id: &str) -> AdkError {
    AdkError::not_found(
        ErrorComponent::Agent,
        RUN_NOT_FOUND_CODE,
        format!("run store has no run '{run_id}'; it was never enqueued or was pruned"),
    )
}

/// Builds the error a run store returns for an invalid [`NewRun`].
///
/// # Example
///
/// ```rust
/// let error = adk_core::run_store::invalid_run_error("queue must not be empty");
/// assert_eq!(error.code, adk_core::run_store::INVALID_RUN_CODE);
/// ```
pub fn invalid_run_error(reason: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::InvalidInput,
        INVALID_RUN_CODE,
        format!("cannot enqueue run: {reason}"),
    )
}

#[derive(Debug)]
struct RunEntry {
    record: RunRecord,
    seq: u64,
}

impl RunEntry {
    fn order(&self) -> (DateTime<Utc>, u64) {
        (self.record.created_at, self.seq)
    }
}

#[derive(Debug, Default)]
struct RunTable {
    runs: HashMap<String, RunEntry>,
    by_idempotency_key: HashMap<(String, String), String>,
    next_seq: u64,
}

impl RunTable {
    /// Unfinished runs in `queue` holding `key`, oldest first.
    fn holders(&self, queue: &str, key: &str) -> Vec<(String, (DateTime<Utc>, u64))> {
        let mut holders: Vec<_> = self
            .runs
            .values()
            .filter(|entry| {
                entry.record.queue == queue
                    && entry.record.concurrency_key.as_deref() == Some(key)
                    && !entry.record.state.is_terminal()
            })
            .map(|entry| (entry.record.run_id.clone(), entry.order()))
            .collect();
        holders.sort_by_key(|(_, order)| *order);
        holders
    }

    fn fenced(&mut self, run_id: &str, epoch: u64) -> Result<&mut RunRecord> {
        let entry = self.runs.get_mut(run_id).ok_or_else(|| run_not_found_error(run_id))?;
        if entry.record.state != RunState::Running || entry.record.lease_epoch != epoch {
            return Err(run_stale_epoch_error(run_id, epoch));
        }
        Ok(&mut entry.record)
    }
}

fn touch(record: &mut RunRecord, now: DateTime<Utc>) {
    record.updated_at = now;
    record.revision += 1;
}

fn release_lease(record: &mut RunRecord) {
    record.lease_owner = None;
    record.lease_expires_at = None;
}

/// Process-local [`DurableRunStore`].
///
/// Runs do not survive a restart. Use it for tests and single-process deployments;
/// use a durable backend when runs must outlive the process.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::run_store::{DurableRunStore, InMemoryRunStore, NewRun, RunOutcome, RunState};
/// use serde_json::json;
///
/// # futures::executor::block_on(async {
/// let store = InMemoryRunStore::new();
/// store.enqueue(NewRun::new("default", "app", "user", "agent", json!({}))).await?;
/// let claimed = store.claim("default", "worker-1", Duration::from_secs(30)).await?.unwrap();
/// let done = store.finish(&claimed.record.run_id, claimed.epoch, RunOutcome::Completed(json!("ok"))).await?;
/// assert_eq!(done.state, RunState::Completed);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[derive(Debug)]
pub struct InMemoryRunStore {
    table: Mutex<RunTable>,
    clock: Arc<dyn Clock>,
}

impl Default for InMemoryRunStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRunStore {
    /// Creates an empty store that reads the system clock.
    pub fn new() -> Self {
        Self { table: Mutex::new(RunTable::default()), clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides delays, waits, and lease expiry.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

#[async_trait]
impl DurableRunStore for InMemoryRunStore {
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
        run.validate()?;
        let now = self.clock.now();
        let mut table = self.table.lock().await;
        if let Some(key) = &run.idempotency_key
            && let Some(existing) = table.by_idempotency_key.get(&(run.queue.clone(), key.clone()))
            && let Some(entry) = table.runs.get(existing)
        {
            return Ok(Enqueued::Existing(entry.record.clone()));
        }
        if let Some(key) = &run.concurrency_key {
            let holders = table.holders(&run.queue, key);
            match run.on_conflict {
                OnConflict::Reject => {
                    if let Some((blocking_run_id, _)) = holders.into_iter().next() {
                        return Ok(Enqueued::Rejected { blocking_run_id });
                    }
                }
                OnConflict::Interrupt => {
                    for (run_id, _) in holders {
                        if let Some(entry) = table.runs.get_mut(&run_id) {
                            let record = &mut entry.record;
                            if record.state == RunState::Running {
                                record.cancel_requested = true;
                            } else {
                                record.state = RunState::Cancelled;
                                release_lease(record);
                            }
                            touch(record, now);
                        }
                    }
                }
                OnConflict::Enqueue => {}
            }
        }
        let run_id = uuid::Uuid::new_v4().to_string();
        if let Some(key) = &run.idempotency_key {
            table.by_idempotency_key.insert((run.queue.clone(), key.clone()), run_id.clone());
        }
        let record = RunRecord::queued(run_id.clone(), run, now);
        let seq = table.next_seq;
        table.next_seq += 1;
        table.runs.insert(run_id, RunEntry { record: record.clone(), seq });
        Ok(Enqueued::Created(record))
    }

    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>> {
        let now = self.clock.now();
        let expires_at = deadline(now, ttl, "run_store.invalid_ttl")?;
        let mut table = self.table.lock().await;
        let mut candidates: Vec<_> = table
            .runs
            .values()
            .filter(|entry| entry.record.queue == queue && !entry.record.state.is_terminal())
            .map(|entry| (entry.order(), entry.record.run_id.clone()))
            .collect();
        candidates.sort();
        for (order, run_id) in candidates {
            let Some(entry) = table.runs.get(&run_id) else { continue };
            let record = &entry.record;
            let started = match record.state {
                RunState::Queued => {
                    let delayed = record.not_before.is_some_and(|at| at > now);
                    let blocked = record.concurrency_key.as_deref().is_some_and(|key| {
                        table.holders(queue, key).first().is_some_and(|(_, first)| *first < order)
                    });
                    if delayed || blocked {
                        continue;
                    }
                    1
                }
                RunState::Waiting => {
                    if record.not_before.is_none_or(|at| at > now) {
                        continue;
                    }
                    0
                }
                RunState::Running => {
                    if record.lease_expires_at.is_some_and(|at| at > now) {
                        continue;
                    }
                    1
                }
                RunState::Completed | RunState::Failed | RunState::Cancelled => continue,
            };
            let Some(entry) = table.runs.get_mut(&run_id) else { continue };
            let record = &mut entry.record;
            if record.state == RunState::Running && record.attempt >= record.max_attempts {
                record.state = RunState::Failed;
                record.error = Some(format!(
                    "attempts exhausted: the lease of attempt {} of {} expired",
                    record.attempt, record.max_attempts
                ));
                release_lease(record);
                touch(record, now);
                continue;
            }
            record.state = RunState::Running;
            record.attempt += started;
            record.lease_epoch += 1;
            record.lease_owner = Some(worker.to_string());
            record.lease_expires_at = Some(expires_at);
            record.heartbeat_at = Some(now);
            record.not_before = None;
            touch(record, now);
            return Ok(Some(ClaimedRun { epoch: record.lease_epoch, record: record.clone() }));
        }
        Ok(None)
    }

    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
        let now = self.clock.now();
        let expires_at = deadline(now, ttl, "run_store.invalid_ttl")?;
        let mut table = self.table.lock().await;
        let record = table.fenced(run_id, epoch)?;
        record.lease_expires_at = Some(expires_at);
        record.heartbeat_at = Some(now);
        touch(record, now);
        Ok(record.cancel_requested)
    }

    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<()> {
        let now = self.clock.now();
        let mut table = self.table.lock().await;
        let record = table.fenced(run_id, epoch)?;
        record.checkpoint = Some(checkpoint);
        touch(record, now);
        Ok(())
    }

    async fn wait(&self, run_id: &str, epoch: u64, until: Option<DateTime<Utc>>) -> Result<()> {
        let now = self.clock.now();
        let mut table = self.table.lock().await;
        let record = table.fenced(run_id, epoch)?;
        record.state = RunState::Waiting;
        record.not_before = if record.wake_pending { Some(now) } else { until };
        record.wake_pending = false;
        release_lease(record);
        touch(record, now);
        Ok(())
    }

    async fn wake(&self, run_id: &str) -> Result<()> {
        let now = self.clock.now();
        let mut table = self.table.lock().await;
        let entry = table.runs.get_mut(run_id).ok_or_else(|| run_not_found_error(run_id))?;
        let record = &mut entry.record;
        match record.state {
            RunState::Waiting => record.not_before = Some(now),
            RunState::Running => record.wake_pending = true,
            RunState::Queued | RunState::Completed | RunState::Failed | RunState::Cancelled => {
                return Ok(());
            }
        }
        touch(record, now);
        Ok(())
    }

    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord> {
        let now = self.clock.now();
        let mut table = self.table.lock().await;
        let record = table.fenced(run_id, epoch)?;
        match outcome {
            RunOutcome::Completed(result) => {
                record.state = RunState::Completed;
                record.result = Some(result);
            }
            RunOutcome::Failed { error, retryable } => {
                let retry =
                    retryable && record.attempt < record.max_attempts && !record.cancel_requested;
                record.state = if retry { RunState::Queued } else { RunState::Failed };
                record.error = Some(error);
            }
            RunOutcome::Cancelled => record.state = RunState::Cancelled,
        }
        record.wake_pending = false;
        release_lease(record);
        touch(record, now);
        Ok(record.clone())
    }

    async fn request_cancel(&self, run_id: &str) -> Result<()> {
        let now = self.clock.now();
        let mut table = self.table.lock().await;
        let entry = table.runs.get_mut(run_id).ok_or_else(|| run_not_found_error(run_id))?;
        let record = &mut entry.record;
        match record.state {
            RunState::Queued | RunState::Waiting => {
                record.state = RunState::Cancelled;
                release_lease(record);
            }
            RunState::Running => record.cancel_requested = true,
            RunState::Completed | RunState::Failed | RunState::Cancelled => return Ok(()),
        }
        touch(record, now);
        Ok(())
    }

    async fn get(&self, run_id: &str) -> Result<Option<RunRecord>> {
        Ok(self.table.lock().await.runs.get(run_id).map(|entry| entry.record.clone()))
    }

    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>> {
        let table = self.table.lock().await;
        let mut matching: Vec<_> =
            table.runs.values().filter(|entry| filter.matches(&entry.record)).collect();
        matching.sort_by_key(|entry| entry.order());
        let limit = filter.limit.unwrap_or(usize::MAX);
        Ok(matching.into_iter().take(limit).map(|entry| entry.record.clone()).collect())
    }

    async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64> {
        let mut table = self.table.lock().await;
        let pruned: Vec<_> = table
            .runs
            .values()
            .filter(|entry| {
                entry.record.state.is_terminal() && entry.record.updated_at < finished_before
            })
            .map(|entry| entry.record.run_id.clone())
            .collect();
        for run_id in &pruned {
            if let Some(entry) = table.runs.remove(run_id)
                && let Some(key) = entry.record.idempotency_key
            {
                table.by_idempotency_key.remove(&(entry.record.queue, key));
            }
        }
        Ok(pruned.len() as u64)
    }
}
