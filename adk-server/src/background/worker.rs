//! The durable worker: claims runs from a [`DurableRunStore`] under fenced leases and
//! executes them.
//!
//! # Lifecycle of one claimed run
//!
//! 1. [`claim`](DurableRunStore::claim) returns the run with a fencing epoch.
//! 2. The worker heartbeats every `lease_ttl / 3`. A heartbeat that reports a
//!    cancellation request stops the run cooperatively; one that fails with
//!    `run_store.stale_epoch` (another worker recovered the run) drops the execution at
//!    once, and the worker writes nothing more.
//! 3. The target runs: an agent through its [`Runner`], or a workflow through the
//!    [`WorkflowExecutor`]. The worker records where the run is checkpointed with
//!    [`set_checkpoint`](DurableRunStore::set_checkpoint) before any progress is made.
//! 4. [`finish`](DurableRunStore::finish) ends the attempt, fenced by the epoch. An agent
//!    run that ends holding a tool call for approval is parked instead: the worker records
//!    [`RunCheckpointRef::AwaitingApproval`] and calls
//!    [`wait`](DurableRunStore::wait); a [`wake`](DurableRunStore::wake) after the
//!    decision makes it claimable, and the claim resumes the same session so the held call
//!    executes with its decision.
//!
//! # Fencing a stalled worker
//!
//! A worker that stalls (a long pause, a partition) can wake after its lease expired and
//! another worker recovered the run. Every run-store write carries the epoch and is
//! refused. For agent runs the worker also installs a lease fence as invocation hooks:
//! before each model call and tool call, and after each model response and tool result,
//! the fence checks that the lease is still provably held — the last successful heartbeat
//! was sent less than `lease_ttl` ago. When it is not, the fence never returns, the
//! execution is dropped, and no further session event, tool call, or store write happens.
//!
//! # Kill switch
//!
//! A worker with a governance control ([`DurableWorker::with_governance`]) claims nothing
//! while the control refuses work. A claim already in flight when the control starts
//! refusing can still return a run, so the worker checks the control again before it runs
//! the target:
//!
//! | Refused | Worker |
//! |---------|--------|
//! | Once the claim returns | Hands the run back unstarted |
//! | By the runner before the agent produced an event (its start check, or the check before the first model call) | Hands the run back unstarted, when the worker's own control also refuses |
//! | After the run made a model or tool call | Ends the attempt with the control's error |
//!
//! A run handed back is parked with [`wait`](DurableRunStore::wait) until the time it was
//! claimed: it is `Waiting`, claimable at once, and the claim that resumes it counts no
//! further attempt.
//!
//! # Identity
//!
//! An agent run acts as the runner's own principal for its root agent (its identity
//! source, or the derived `urn:adk:agent:{app}/{agent}`), on behalf of the run's user.
//! A principal stored on the run ([`RunRecord::principal`]) is a claim made by whoever
//! enqueued it, so the worker honours it only within these rules, and otherwise fails the
//! run without retry and records the refusal on the audit trail:
//!
//! | Stored principal | Worker |
//! |------------------|--------|
//! | None | Acts as its own principal, for the run's user as a human |
//! | Agent id is the worker's own or the derived id of the target | Acts as its own principal |
//! | Agent id is in [`DurableWorkerConfig::allowed_principals`] | Acts as the stored principal |
//! | Any other agent id | Refused with `identity.principal_not_allowed` |
//! | Subject is the run's user | Acts for the user as the stored subject's kind (a scheduled, webhook, or queue run is a service) |
//! | Subject is anyone else | Passed to the runner, which refuses it with `identity.subject_mismatch` unless its `SubjectDelegationPolicy` allows it; a delegation chain headed by the run's user gives the user's subject kind |
//!
//! The allowlist is empty by default.
//!
//! # Crash resume
//!
//! A run recovered after its lease expired resumes rather than restarting:
//!
//! | Target | Checkpoint | What the recovering worker does |
//! |--------|------------|--------------------------------|
//! | Agent | `Session { session_id, invocation_id }` | Answers calls the stopped worker left unanswered from the action ledger ([`Runner::recover_unanswered_calls`]); completes the run from the session when it already holds the final answer; otherwise continues the same session with an empty user turn, so the model sees the whole conversation, including the recovered call outcomes, and nothing is sent twice |
//! | Workflow | `Graph { thread_id, checkpoint_id }` | Calls the workflow again with [`WorkflowRunContext::resumed`] set and the same `thread_id`; a graph workflow resumes from its checkpointer |
//!
//! No tool executes again during recovery. A non-idempotent call that began but recorded
//! no outcome is answered `outcome_unknown`; one that completed is answered from its
//! ledger record. A run whose original user turn never reached the session (the worker
//! stopped before the checkpoint was recorded) starts again from its input.
//!
//! # Lifecycle hooks
//!
//! [`WorkerHooks`] registered with [`DurableWorker::with_hooks`] observe every state change
//! the worker writes (spec 3.2 §10 S1). Each hook runs after the store write it reports, so
//! the store already shows the state the hook receives:
//!
//! | Hook | Called | The run it receives |
//! |------|--------|---------------------|
//! | [`on_claim`](WorkerHooks::on_claim) | Once the worker starts the claimed run's target, before any model call or workflow step | `Running`, at the claim's epoch |
//! | [`on_park`](WorkerHooks::on_park) | After [`wait`](DurableRunStore::wait) parked the run, with the [`ParkReason`] | `Waiting`, holding no lease |
//! | [`on_finish`](WorkerHooks::on_finish) | After [`finish`](DurableRunStore::finish) wrote the attempt's outcome | `Completed`, `Failed`, or `Cancelled`; `Queued` when a retryable failure requeued it |
//!
//! A run the worker hands back at once, because its governance control refused work when
//! the claim returned, is reported to `on_park` only: its target never started. An attempt
//! that lost its lease writes nothing and calls no hook; the worker that recovers the run
//! reports it.
//!
//! # Suspending a run
//!
//! [`DurableWorker::suspend`] (or [`WorkerHandle::suspend`]) asks an agent run in flight on
//! this worker to stop at its next model-call boundary and park, so it resumes later rather
//! than ending (spec 3.2 §10 S1):
//!
//! 1. The tool calls the model already asked for finish and their responses reach the
//!    session; no further model call is made.
//! 2. The worker interrupts the turn, records `RunCheckpointRef::Session` on the
//!    interrupted invocation, and parks the run with [`wait`](DurableRunStore::wait) until
//!    woken: it is `Waiting`, holds no lease, and is claimable only after
//!    [`wake`](DurableRunStore::wake).
//! 3. The claim after the wake, by any worker, continues the session at the same attempt,
//!    as [Crash resume](self#crash-resume) describes; a non-idempotent call that completed
//!    before the suspension is never executed again.
//!
//! | Requested | Outcome |
//! |-----------|---------|
//! | `suspend` | Parked `Waiting` on its `Session` checkpoint; [`on_park`](WorkerHooks::on_park) with [`ParkReason::Suspended`] |
//! | `suspend`, then the governance control refuses the run at that boundary | Parked the same way, rather than failing with the control's error; refused before its first event, it is handed back as [Kill switch](self#kill-switch) describes |
//! | [`request_cancel`](DurableRunStore::request_cancel), with or without `suspend` | Finished `Cancelled`, as before |
//! | `suspend`, and the turn ends without another model call | Finished, or parked on its held tool calls, as without the request |
//!
//! Only agent runs are suspendable: a workflow run has no model-call boundary the worker
//! can stop at, so `suspend` returns `false` for it and it runs on.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome, AuditTrail,
};
use adk_core::{
    ActingConfig, AdkError, AgentId, AgentPrincipal, BeforeModelResult, CallbackContext,
    ClaimedRun, Content, DurableRunStore, ErrorCategory, ErrorComponent, GOVERNANCE_FROZEN_CODE,
    GOVERNANCE_STALE_CODE, GovernanceControl, InvocationHooks, LlmRequest, LlmResponse,
    PRINCIPAL_NOT_ALLOWED_CODE, Part, RunCheckpointRef, RunOutcome, RunRecord, RunState, SessionId,
    UserId,
    run_store::{RUN_NOT_FOUND_CODE, RUN_STALE_EPOCH_CODE},
};
use adk_runner::Runner;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{WorkflowExecutor, WorkflowRunContext, WorkflowState};

/// How a [`DurableWorker`] claims and runs work.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_server::background::DurableWorkerConfig;
///
/// let config = DurableWorkerConfig::default()
///     .with_queue("payments")
///     .with_lease_ttl(Duration::from_secs(15))
///     .with_max_concurrent_runs(8);
/// assert_eq!(config.queue, "payments");
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DurableWorkerConfig {
    /// The owner name written into every lease. Defaults to a random `worker-{uuid}`.
    pub worker_id: String,
    /// The queue claimed from. Defaults to `default`.
    pub queue: String,
    /// How long a claim or heartbeat holds the run. Defaults to 30 seconds.
    pub lease_ttl: Duration,
    /// How long the worker waits when nothing is claimable. Defaults to 500 ms.
    pub poll_interval: Duration,
    /// The most runs executed at once. Defaults to 4.
    pub max_concurrent_runs: usize,
    /// Fails an attempt that runs longer than this; `None` never times out.
    pub run_timeout: Option<Duration>,
    /// Finished runs older than this are pruned by the worker loop; `None` keeps them.
    /// Defaults to 24 hours.
    pub retain_finished: Option<Duration>,
    /// How long an agent run parked on a held tool call waits for a decision; `None`
    /// waits until woken. A run resumed past this deadline with the call still held
    /// fails with `approval expired`.
    pub approval_timeout: Option<Duration>,
    /// Agent ids a run's stored principal may name for the worker to act as it. Empty by
    /// default: a stored principal other than the worker's own or the derived one is
    /// refused. See [Identity](self#identity).
    pub allowed_principals: BTreeSet<AgentId>,
}

impl Default for DurableWorkerConfig {
    fn default() -> Self {
        Self {
            worker_id: format!("worker-{}", uuid::Uuid::new_v4()),
            queue: "default".to_string(),
            lease_ttl: Duration::from_secs(30),
            poll_interval: Duration::from_millis(500),
            max_concurrent_runs: 4,
            run_timeout: None,
            retain_finished: Some(Duration::from_secs(24 * 60 * 60)),
            approval_timeout: None,
            allowed_principals: BTreeSet::new(),
        }
    }
}

impl DurableWorkerConfig {
    /// Sets the owner name written into every lease.
    pub fn with_worker_id(mut self, worker_id: impl Into<String>) -> Self {
        self.worker_id = worker_id.into();
        self
    }

    /// Sets the queue claimed from.
    pub fn with_queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = queue.into();
        self
    }

    /// Sets how long a claim or heartbeat holds the run.
    pub fn with_lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = ttl;
        self
    }

    /// Sets how long the worker waits when nothing is claimable.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Sets the most runs executed at once.
    pub fn with_max_concurrent_runs(mut self, max: usize) -> Self {
        self.max_concurrent_runs = max.max(1);
        self
    }

    /// Fails an attempt that runs longer than `timeout`.
    pub fn with_run_timeout(mut self, timeout: Duration) -> Self {
        self.run_timeout = Some(timeout);
        self
    }

    /// Sets how long a run parked on a held tool call waits for a decision.
    pub fn with_approval_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.approval_timeout = timeout;
        self
    }

    /// Lets a run's stored principal name `agent_id` for the worker to act as it.
    pub fn with_allowed_principal(mut self, agent_id: AgentId) -> Self {
        self.allowed_principals.insert(agent_id);
        self
    }

    /// Sets how long finished runs are kept before the worker loop prunes them.
    pub fn with_retain_finished(mut self, retain: Option<Duration>) -> Self {
        self.retain_finished = retain;
        self
    }
}

/// Why a [`DurableWorker`] parked a run as `Waiting`, as [`WorkerHooks::on_park`] reports it
/// (spec 3.2 §10 S1).
///
/// # Example
///
/// ```rust
/// use adk_server::background::ParkReason;
///
/// let reason = ParkReason::Suspended;
/// assert_ne!(reason, ParkReason::AwaitingApproval);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ParkReason {
    /// The run holds tool calls for approval, on a `RunCheckpointRef::AwaitingApproval`
    /// checkpoint; a wake after the decision resumes it.
    AwaitingApproval,
    /// The run was asked to [`suspend`](DurableWorker::suspend) and stopped at a model-call
    /// boundary, on a `RunCheckpointRef::Session` checkpoint; it waits until woken.
    Suspended,
    /// The governance control refused the run before it made a model or tool call; it is
    /// claimable again at once.
    HandedBack,
}

/// Lifecycle hooks a [`DurableWorker`] calls as it moves a run between states (spec 3.2
/// §10 S1).
///
/// Every method has an empty default, so an implementation overrides only the transitions
/// it reports. A hook runs after the store write it reports and observes it: it cannot change
/// what the worker does. The worker awaits each hook before it continues the attempt, so a
/// hook hands slow work, such as delivery to a remote sink, off to another task. See
/// [Lifecycle hooks](self#lifecycle-hooks) for when each is called.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{InMemoryRunStore, RunRecord};
/// use adk_server::background::{DurableWorker, ParkReason, WorkerHooks};
/// use async_trait::async_trait;
///
/// #[derive(Debug)]
/// struct LogTransitions;
///
/// #[async_trait]
/// impl WorkerHooks for LogTransitions {
///     async fn on_park(&self, run: &RunRecord, reason: ParkReason) {
///         tracing::info!(run.id = %run.run_id, park.reason = ?reason, "run parked");
///     }
/// }
///
/// let worker =
///     DurableWorker::new(Arc::new(InMemoryRunStore::new())).with_hooks(Arc::new(LogTransitions));
/// # let _ = worker;
/// ```
#[async_trait]
pub trait WorkerHooks: Send + Sync {
    /// Called once the worker starts the target of a run it claimed, before any model call
    /// or workflow step. `run` is `Running` at the claim's epoch.
    async fn on_claim(&self, _run: &RunRecord) {}

    /// Called after the worker parked `run` as `Waiting`, with why. `run` holds no lease.
    async fn on_park(&self, _run: &RunRecord, _reason: ParkReason) {}

    /// Called after the worker wrote the attempt's outcome. `run` is `Completed`, `Failed`,
    /// or `Cancelled`, or `Queued` when a retryable failure requeued it.
    async fn on_finish(&self, _run: &RunRecord) {}
}

/// Claims runs from a [`DurableRunStore`] and executes them: agents through their
/// [`Runner`], workflows through a [`WorkflowExecutor`].
///
/// Run one per replica with [`spawn`](Self::spawn); every replica claiming from the same
/// store and queue shares the work, and a run whose worker stopped is recovered by
/// another once its lease expires. See the [module documentation](self) for fencing and
/// crash resume.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_server::background::{DurableWorker, DurableWorkerConfig};
///
/// let worker = DurableWorker::new(run_store)
///     .with_agent("support", Arc::new(runner))
///     .with_workflows(Arc::new(registry))
///     .with_governance(governance.clone())
///     .with_config(DurableWorkerConfig::default().with_queue("default"));
/// let handle = worker.spawn();
/// // ...
/// handle.shutdown().await;
/// ```
#[derive(Clone)]
pub struct DurableWorker {
    runs: Arc<dyn DurableRunStore>,
    agents: BTreeMap<String, Arc<Runner>>,
    workflows: Option<Arc<dyn WorkflowExecutor>>,
    governance: Option<GovernanceControl>,
    audit: Option<AuditTrail>,
    config: DurableWorkerConfig,
    /// Makes every in-flight attempt heartbeat now, so a cancel requested in this
    /// process is seen without waiting a heartbeat interval.
    heartbeat_now: Arc<Notify>,
    hooks: Vec<Arc<dyn WorkerHooks>>,
    /// The agent attempts in flight, shared with clones and the handle, so a suspend
    /// request reaches the attempt wherever it was asked.
    suspensions: Arc<Suspensions>,
}

impl std::fmt::Debug for DurableWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableWorker")
            .field("agents", &self.agents.keys().collect::<Vec<_>>())
            .field("workflows", &self.workflows.is_some())
            .field("config", &self.config)
            .field("hooks", &self.hooks.len())
            .finish_non_exhaustive()
    }
}

/// Why an attempt ended without an outcome to write.
enum Stop {
    /// The lease is no longer held; another worker owns the run.
    LeaseLost,
}

/// How an attempt that ran to its end left the run.
enum Attempt {
    /// The run finished with a result.
    Done(Value),
    /// The run holds tool calls for approval and waits, until this time when set.
    Parked(Option<chrono::DateTime<chrono::Utc>>),
    /// The governance control refused the run before it made a model or tool call.
    Refused(AdkError),
    /// The run was asked to suspend and stopped at a model-call boundary; its `Session`
    /// checkpoint is recorded.
    Suspended,
}

/// A failed attempt.
struct Failure {
    error: String,
    retryable: bool,
}

impl From<AdkError> for Failure {
    fn from(error: AdkError) -> Self {
        Self { retryable: error.is_retryable(), error: error.to_string() }
    }
}

impl DurableWorker {
    /// A worker over `runs` with no targets and the default configuration.
    pub fn new(runs: Arc<dyn DurableRunStore>) -> Self {
        Self {
            runs,
            agents: BTreeMap::new(),
            workflows: None,
            governance: None,
            audit: None,
            config: DurableWorkerConfig::default(),
            heartbeat_now: Arc::new(Notify::new()),
            hooks: Vec::new(),
            suspensions: Arc::default(),
        }
    }

    /// Runs whose target is `target` execute through `runner`.
    pub fn with_agent(mut self, target: impl Into<String>, runner: Arc<Runner>) -> Self {
        self.agents.insert(target.into(), runner);
        self
    }

    /// Runs whose target is not an agent execute through `workflows`.
    pub fn with_workflows(mut self, workflows: Arc<dyn WorkflowExecutor>) -> Self {
        self.workflows = Some(workflows);
        self
    }

    /// Claims nothing while `control` refuses work (frozen, or a stale fail-closed mirror).
    ///
    /// A run that a claim in flight returns after `control` started refusing is handed
    /// back unstarted; see [Kill switch](self#kill-switch).
    pub fn with_governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Records refused run identities on `trail`; without one, an agent run's refusal is
    /// recorded on its runner's audit trail.
    pub fn with_audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Sets the configuration.
    pub fn with_config(mut self, config: DurableWorkerConfig) -> Self {
        self.config = config;
        self
    }

    /// Adds lifecycle hooks, called after those added before (spec 3.2 §10 S1).
    ///
    /// See [Lifecycle hooks](self#lifecycle-hooks) for when each hook is called.
    pub fn with_hooks(mut self, hooks: Arc<dyn WorkerHooks>) -> Self {
        self.hooks.push(hooks);
        self
    }

    /// Asks the agent run `run_id`, in flight on this worker, to stop at its next model-call
    /// boundary and park as `Waiting` on its `Session` checkpoint until woken (spec 3.2 §10
    /// S1).
    ///
    /// Returns whether the request reached an attempt: `false` when this worker has no
    /// attempt of `run_id` in flight, or the run is a workflow run, which is not
    /// suspendable. A turn that ends without another model call finishes as it would
    /// without the request, and a cancellation requested through the run store still
    /// finishes the run `Cancelled`. Clones of the worker and its [`WorkerHandle`] share
    /// the attempts in flight. See [Suspending a run](self#suspending-a-run).
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_core::InMemoryRunStore;
    /// use adk_server::background::DurableWorker;
    ///
    /// let worker = DurableWorker::new(Arc::new(InMemoryRunStore::new()));
    /// assert!(!worker.suspend("run-1"), "nothing is in flight");
    /// ```
    pub fn suspend(&self, run_id: &str) -> bool {
        self.suspensions.request(run_id)
    }

    /// The configuration.
    pub fn config(&self) -> &DurableWorkerConfig {
        &self.config
    }

    /// The run store.
    pub fn run_store(&self) -> &Arc<dyn DurableRunStore> {
        &self.runs
    }

    /// Whether `target` names a registered agent or a workflow the executor knows.
    pub fn has_target(&self, target: &str) -> bool {
        self.agents.contains_key(target)
            || self.workflows.as_ref().is_some_and(|workflows| workflows.has_workflow(target))
    }

    /// The governance control's refusal, while it refuses work.
    fn refusal(&self) -> Option<AdkError> {
        self.governance.as_ref().and_then(|control| control.check().err())
    }

    /// Claims one run and executes it to the end of the attempt.
    ///
    /// Returns the run as the attempt left it, or `None` when nothing was claimable, the
    /// governance control refused work, or the lease was lost mid-attempt.
    ///
    /// # Errors
    ///
    /// Returns the run store's error from the claim.
    pub async fn run_next(&self) -> adk_core::Result<Option<RunRecord>> {
        if let Some(error) = self.refusal() {
            tracing::debug!(error = %error, "governance refuses work; not claiming");
            return Ok(None);
        }
        let Some(claimed) = self
            .runs
            .claim(&self.config.queue, &self.config.worker_id, self.config.lease_ttl)
            .await?
        else {
            return Ok(None);
        };
        Ok(self.execute(claimed).await)
    }

    /// Spawns the claim loop and returns its handle.
    ///
    /// The loop claims while it has capacity, waits `poll_interval` when nothing is
    /// claimable, and prunes finished runs past `retain_finished`. Aborting the handle
    /// drops every in-flight attempt without writing, as a crash would; their leases
    /// expire and another worker recovers them.
    pub fn spawn(self) -> WorkerHandle {
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let wake = Arc::new(Notify::new());
        let woken = Arc::clone(&wake);
        let heartbeat_now = Arc::clone(&self.heartbeat_now);
        let suspensions = Arc::clone(&self.suspensions);
        let task = tokio::spawn(async move { self.claim_loop(stop, woken).await });
        WorkerHandle { shutdown, task, wake, heartbeat_now, suspensions }
    }

    async fn claim_loop(self, shutdown: CancellationToken, wake: Arc<Notify>) {
        let worker = Arc::new(self);
        let mut in_flight = tokio::task::JoinSet::new();
        let mut last_prune: Option<Instant> = None;
        tracing::info!(
            worker.id = %worker.config.worker_id,
            worker.queue = %worker.config.queue,
            "durable worker started"
        );
        loop {
            while in_flight.try_join_next().is_some() {}
            if shutdown.is_cancelled() {
                break;
            }
            if let Some(retain) = worker.config.retain_finished
                && last_prune.is_none_or(|at| at.elapsed() >= Duration::from_secs(60))
            {
                last_prune = Some(Instant::now());
                let cutoff = chrono::Duration::from_std(retain)
                    .ok()
                    .and_then(|retain| chrono::Utc::now().checked_sub_signed(retain));
                if let Some(cutoff) = cutoff {
                    match worker.runs.prune(cutoff).await {
                        Ok(0) => {}
                        Ok(pruned) => tracing::debug!(runs.pruned = pruned, "pruned finished runs"),
                        Err(error) => {
                            tracing::warn!(error = %error, "could not prune finished runs")
                        }
                    }
                }
            }
            let mut claimed = None;
            if in_flight.len() < worker.config.max_concurrent_runs {
                match worker.refusal() {
                    Some(error) => {
                        tracing::debug!(error = %error, "governance refuses work; not claiming")
                    }
                    None => match worker
                        .runs
                        .claim(
                            &worker.config.queue,
                            &worker.config.worker_id,
                            worker.config.lease_ttl,
                        )
                        .await
                    {
                        Ok(run) => claimed = run,
                        Err(error) => tracing::warn!(error = %error, "claim failed"),
                    },
                }
            }
            match claimed {
                Some(run) => {
                    let worker = Arc::clone(&worker);
                    in_flight.spawn(async move { worker.execute(run).await });
                }
                None => {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = tokio::time::sleep(worker.config.poll_interval) => {}
                        () = wake.notified() => {}
                        Some(_) = in_flight.join_next(), if !in_flight.is_empty() => {}
                    }
                }
            }
        }
        // A graceful shutdown lets in-flight attempts finish and write their outcome.
        while in_flight.join_next().await.is_some() {}
        tracing::info!(worker.id = %worker.config.worker_id, "durable worker stopped");
    }

    /// Executes one claimed attempt and writes its outcome, fenced by the claim's epoch.
    async fn execute(&self, claimed: ClaimedRun) -> Option<RunRecord> {
        let ClaimedRun { record, epoch } = claimed;
        let run_id = record.run_id.clone();
        let lease = Arc::new(LeaseState::new(self.config.lease_ttl));
        let cancel = CancellationToken::new();
        let span = tracing::info_span!(
            "durable_run",
            run.id = %run_id,
            run.target = %record.target,
            run.attempt = record.attempt,
            run.epoch = epoch
        );
        tracing::info!(parent: &span, "run claimed");
        // The control was read before the claim, which can return after a freeze landed;
        // the same check now keeps a run that nothing has started from failing at its start.
        if let Some(error) = self.refusal() {
            return self.hand_back(&record, epoch, &error, &span).await;
        }
        // Registered before `on_claim`, so a suspend asked from that hook reaches the attempt.
        let _suspendable = self
            .agents
            .contains_key(&record.target)
            .then(|| Suspensions::register(&self.suspensions, &run_id, &lease));

        let work = async {
            for hooks in &self.hooks {
                hooks.on_claim(&record).await;
            }
            let attempt = self.run_target(&record, epoch, &lease, &cancel);
            match self.config.run_timeout {
                Some(timeout) => match tokio::time::timeout(timeout, attempt).await {
                    Ok(result) => result,
                    Err(_) => Ok(Err(Failure {
                        error: format!("run timed out after {}s", timeout.as_secs()),
                        retryable: false,
                    })),
                },
                None => attempt.await,
            }
        };
        let result = tokio::select! {
            result = work => result,
            () = self.heartbeat(&run_id, epoch, &lease, &cancel) => Err(Stop::LeaseLost),
            () = lease.lost.notified() => Err(Stop::LeaseLost),
        };
        let outcome = match result {
            Err(Stop::LeaseLost) => {
                tracing::warn!(parent: &span, "lease lost; the attempt was dropped without writing");
                return None;
            }
            Ok(_) if lease.cancel_requested.load(Ordering::SeqCst) => RunOutcome::Cancelled,
            Ok(Ok(Attempt::Refused(error))) => {
                return self.hand_back(&record, epoch, &error, &span).await;
            }
            Ok(Ok(Attempt::Parked(until))) => {
                return match self.runs.wait(&run_id, epoch, until).await {
                    Ok(()) => {
                        tracing::info!(parent: &span, wait.until = ?until, "run parked until its held tool calls are decided");
                        self.parked(&record, ParkReason::AwaitingApproval).await
                    }
                    Err(error) => {
                        tracing::warn!(parent: &span, error = %error, "could not park the run; another worker owns it");
                        None
                    }
                };
            }
            Ok(Ok(Attempt::Suspended)) => {
                return match self.runs.wait(&run_id, epoch, None).await {
                    Ok(()) => {
                        tracing::info!(parent: &span, "run suspended at a model-call boundary; parked until woken");
                        self.parked(&record, ParkReason::Suspended).await
                    }
                    Err(error) => {
                        tracing::warn!(parent: &span, error = %error, "could not park the suspended run; another worker owns it");
                        None
                    }
                };
            }
            Ok(Ok(Attempt::Done(value))) => RunOutcome::Completed(value),
            Ok(Err(failure)) => {
                RunOutcome::Failed { error: failure.error, retryable: failure.retryable }
            }
        };
        match self.runs.finish(&run_id, epoch, outcome).await {
            Ok(finished) => {
                tracing::info!(parent: &span, run.state = ?finished.state, "attempt finished");
                for hooks in &self.hooks {
                    hooks.on_finish(&finished).await;
                }
                Some(finished)
            }
            Err(error) => {
                tracing::warn!(parent: &span, error = %error, "could not write the outcome; another worker owns the run");
                None
            }
        }
    }

    /// Gives a claimed run that made no model or tool call back to its queue.
    ///
    /// The run is parked until the time it was claimed, so it is claimable at once — by
    /// this worker once its control allows work again — and the claim that resumes it
    /// counts no further attempt.
    async fn hand_back(
        &self,
        record: &RunRecord,
        epoch: u64,
        refusal: &AdkError,
        span: &tracing::Span,
    ) -> Option<RunRecord> {
        match self.runs.wait(&record.run_id, epoch, Some(record.updated_at)).await {
            Ok(()) => {
                tracing::info!(parent: span, error = %refusal.diagnostics(), "governance refuses work; the run was handed back unstarted");
                self.parked(record, ParkReason::HandedBack).await;
            }
            Err(error) => {
                tracing::warn!(parent: span, error = %error, "could not hand the run back; it is recovered once its lease expires");
            }
        }
        None
    }

    /// Reads back a run this worker just parked, reports it to the `on_park` hooks, and
    /// returns it as read.
    ///
    /// When the store cannot be read, the hooks receive the claimed record as parked, so a
    /// hook never misses a transition the store accepted.
    async fn parked(&self, claimed: &RunRecord, reason: ParkReason) -> Option<RunRecord> {
        let read = self.runs.get(&claimed.run_id).await;
        let parked = match &read {
            Ok(Some(parked)) => parked.clone(),
            Ok(None) | Err(_) => {
                tracing::warn!(run.id = %claimed.run_id, error = ?read.as_ref().err(), "could not read the parked run back; reporting it from its claim");
                RunRecord {
                    state: RunState::Waiting,
                    lease_owner: None,
                    lease_expires_at: None,
                    ..claimed.clone()
                }
            }
        };
        for hooks in &self.hooks {
            hooks.on_park(&parked, reason).await;
        }
        read.ok().flatten()
    }

    /// Heartbeats until the lease is lost; returns only then.
    async fn heartbeat(
        &self,
        run_id: &str,
        epoch: u64,
        lease: &LeaseState,
        cancel: &CancellationToken,
    ) {
        let ttl = self.config.lease_ttl;
        loop {
            tokio::select! {
                () = tokio::time::sleep(ttl / 3) => {}
                () = self.heartbeat_now.notified() => {}
            }
            let sent = Instant::now();
            match self.runs.heartbeat(run_id, epoch, ttl).await {
                Ok(cancel_requested) => {
                    lease.renewed(sent);
                    if cancel_requested && !lease.cancel_requested.swap(true, Ordering::SeqCst) {
                        tracing::info!(run.id = run_id, "cancellation requested; stopping the run");
                        cancel.cancel();
                    }
                }
                Err(error)
                    if error.code == RUN_STALE_EPOCH_CODE || error.code == RUN_NOT_FOUND_CODE =>
                {
                    lease.lose();
                    cancel.cancel();
                    return;
                }
                Err(error) => {
                    tracing::warn!(run.id = run_id, error = %error, "heartbeat failed; retrying");
                    if !lease.is_valid() {
                        lease.lose();
                        cancel.cancel();
                        return;
                    }
                }
            }
        }
    }

    async fn run_target(
        &self,
        record: &RunRecord,
        epoch: u64,
        lease: &Arc<LeaseState>,
        cancel: &CancellationToken,
    ) -> Result<Result<Attempt, Failure>, Stop> {
        if let Some(runner) = self.agents.get(&record.target) {
            let root = runner.root_agent_name();
            let own = match runner.default_principal(root).await {
                Ok(principal) => principal,
                Err(error) => return Ok(Err(error.into())),
            };
            let mut acting = runner.run_config().acting.clone();
            let defaults = [
                own.id,
                AgentPrincipal::derived(runner.app_name(), root).id,
                AgentPrincipal::derived(&record.app_name, &record.target).id,
            ];
            if let Err(error) = self.admit_principal(record, &defaults, root, true, &mut acting) {
                return Ok(Err(self.refuse(record, error, runner.audit_trail()).await));
            }
            return self.run_agent(runner, record, acting, epoch, lease, cancel).await;
        }
        let Some(workflows) = self.workflows.as_ref().filter(|w| w.has_workflow(&record.target))
        else {
            return Ok(Err(Failure {
                error: format!(
                    "no agent or workflow named '{}' is registered on worker '{}'",
                    record.target, self.config.worker_id
                ),
                retryable: false,
            }));
        };
        let defaults = [AgentPrincipal::derived(&record.app_name, &record.target).id];
        let mut acting = ActingConfig::default();
        if let Err(error) =
            self.admit_principal(record, &defaults, &record.target, false, &mut acting)
        {
            return Ok(Err(self.refuse(record, error, None).await));
        }
        let (thread_id, checkpoint_id) = match &record.checkpoint {
            Some(RunCheckpointRef::Graph { thread_id, checkpoint_id }) => {
                (thread_id.clone(), checkpoint_id.clone())
            }
            _ => (record.run_id.clone(), None),
        };
        let resumed = record.checkpoint.is_some();
        if !resumed {
            self.checkpoint(
                &record.run_id,
                epoch,
                RunCheckpointRef::Graph { thread_id: thread_id.clone(), checkpoint_id: None },
            )
            .await?;
        }
        let context = WorkflowRunContext {
            run_id: record.run_id.clone(),
            attempt: record.attempt,
            thread_id,
            checkpoint_id,
            resumed,
            cancel: cancel.clone(),
            sink: Some(super::CheckpointSink {
                runs: Arc::clone(&self.runs),
                run_id: record.run_id.clone(),
                epoch,
            }),
        };
        let input = workflow_input(&record.input);
        Ok(workflows
            .execute_durable(&record.target, input, context)
            .await
            .map(Attempt::Done)
            .map_err(|error| Failure { error, retryable: true }))
    }

    /// Applies the run's stored principal to `acting` under the rules in
    /// [Identity](self#identity).
    ///
    /// `defaults` are the agent ids the worker acts as anyway; `root` is the agent an
    /// allowed principal applies to. A subject other than the run's user is left on
    /// `acting` for the runner's delegation policy when `delegable`, and refused otherwise.
    fn admit_principal(
        &self,
        record: &RunRecord,
        defaults: &[AgentId],
        root: &str,
        delegable: bool,
        acting: &mut ActingConfig,
    ) -> Result<(), AdkError> {
        let Some(stored) = &record.principal else {
            return Ok(());
        };
        if !defaults.contains(&stored.agent.id) {
            if !self.config.allowed_principals.contains(&stored.agent.id) {
                return Err(AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::Forbidden,
                    PRINCIPAL_NOT_ALLOWED_CODE,
                    format!(
                        "run '{}' asks to act as agent '{}', which worker '{}' may not act as; \
                         add the id with DurableWorkerConfig::with_allowed_principal, or \
                         enqueue the run without a principal",
                        record.run_id, stored.agent.id, self.config.worker_id
                    ),
                ));
            }
            acting.principals.insert(root.to_string(), stored.agent.clone());
        }
        match &stored.on_behalf_of {
            Some(subject) if subject.id == record.user_id => acting.subject_kind = subject.kind,
            Some(subject) if delegable => {
                // A delegated principal names the session's subject at the head of its
                // chain, as the runner records it; its kind is the run's subject kind.
                if let Some(head) =
                    stored.delegation_chain.first().filter(|head| head.id == record.user_id)
                {
                    acting.subject_kind = head.kind;
                }
                acting.on_behalf_of = Some(subject.clone());
            }
            Some(subject) => {
                let session = adk_core::SubjectRef::new(subject.kind, record.user_id.clone());
                return Err(adk_core::subject_mismatch_error(&session, subject));
            }
            None => {}
        }
        acting.delegation_chain.clone_from(&stored.delegation_chain);
        Ok(())
    }

    /// Records a refused run identity and returns the run's non-retryable failure.
    async fn refuse(
        &self,
        record: &RunRecord,
        error: AdkError,
        runner_trail: Option<&AuditTrail>,
    ) -> Failure {
        tracing::warn!(run.id = %record.run_id, error = %error, "run identity refused");
        if let Some(trail) = self.audit.as_ref().or(runner_trail) {
            let actor = record
                .principal
                .as_ref()
                .map(|principal| AuditActor::new(principal, ""))
                .unwrap_or_default();
            let draft = AuditRecordDraft::new(AuditRecordKind::Access, error.code)
                .actor(actor)
                .decision(AuditDecision::Denied)
                .outcome(AuditRecordOutcome::NotRun)
                .ext("runId", Value::String(record.run_id.clone()))
                .ext("userId", Value::String(record.user_id.clone()))
                .ext("workerId", Value::String(self.config.worker_id.clone()));
            if let Err(audit_error) = trail.record(draft).await {
                tracing::warn!(run.id = %record.run_id, error = %audit_error, "could not record the refused run");
            }
        }
        Failure { error: error.to_string(), retryable: false }
    }

    async fn checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<(), Stop> {
        match self.runs.set_checkpoint(run_id, epoch, checkpoint).await {
            Ok(()) => Ok(()),
            Err(error) => {
                tracing::warn!(run.id = run_id, error = %error, "could not record the checkpoint; stopping");
                Err(Stop::LeaseLost)
            }
        }
    }

    async fn run_agent(
        &self,
        runner: &Arc<Runner>,
        record: &RunRecord,
        acting: ActingConfig,
        epoch: u64,
        lease: &Arc<LeaseState>,
        cancel: &CancellationToken,
    ) -> Result<Result<Attempt, Failure>, Stop> {
        let user_id = record.user_id.as_str();
        let session_id = match (&record.checkpoint, &record.session_id) {
            (
                Some(
                    RunCheckpointRef::Session { session_id, .. }
                    | RunCheckpointRef::AwaitingApproval { session_id, .. },
                ),
                _,
            ) => session_id.clone(),
            (_, Some(session_id)) => session_id.clone(),
            _ => format!("run-{}", record.run_id),
        };
        let mut config = runner.run_config().clone();
        config.acting = acting;
        // A run an authenticated caller enqueued runs as that authenticated user (I6).
        if record.authenticated {
            config.request_context = Some(adk_core::RequestContext {
                user_id: record.user_id.clone(),
                scopes: Vec::new(),
                metadata: std::collections::HashMap::new(),
            });
        }
        // First, so no other hook acts on a model call a suspension stops.
        config.invocation_hooks.insert(0, Arc::new(SuspendGate(Arc::clone(lease))));
        config.invocation_hooks.push(Arc::new(LeaseFence(Arc::clone(lease))));
        // Approval requests for calls this run holds carry its id, so their decision
        // wakes exactly this run.
        config.durable_run_id = Some(record.run_id.clone());
        let resume_from = match &record.checkpoint {
            Some(
                RunCheckpointRef::Session { invocation_id, .. }
                | RunCheckpointRef::AwaitingApproval { invocation_id, .. },
            ) => invocation_id.clone(),
            _ => None,
        };
        let prepared = async {
            let session = ensure_session(runner, user_id, &session_id).await?;
            let Some(invocation_id) = resume_from else {
                return Ok::<_, AdkError>(Prepared::Start);
            };
            let events = session.events().all();
            if let Some(answer) = completed_answer(&events, &invocation_id) {
                return Ok(Prepared::Done(answer));
            }
            let answered = runner
                .recover_unanswered_calls_with_config(user_id, &session_id, Some(config.clone()))
                .await?;
            let turn_persisted = events
                .iter()
                .any(|event| event.invocation_id == invocation_id && event.author == "user");
            tracing::info!(
                run.id = %record.run_id,
                session.id = %session_id,
                resumed.from = %invocation_id,
                calls.answered = answered,
                user_turn.persisted = turn_persisted,
                "resuming agent run"
            );
            Ok(if turn_persisted { Prepared::Continue } else { Prepared::Start })
        };
        let content = match prepared.await {
            Ok(Prepared::Done(text)) => {
                return Ok(Ok(Attempt::Done(agent_result(&text, &session_id, None))));
            }
            Ok(Prepared::Continue) => Content::new("user").with_text(""),
            Ok(Prepared::Start) => input_content(&record.input),
            Err(error) => return Ok(Err(error.into())),
        };

        let (user, session) =
            match (UserId::try_from(user_id), SessionId::try_from(session_id.as_str())) {
                (Ok(user), Ok(session)) => (user, session),
                (Err(error), _) | (_, Err(error)) => {
                    return Ok(Err(Failure { error: error.to_string(), retryable: false }));
                }
            };
        let invocation = match runner.start_with_config(user, session, content, Some(config)).await
        {
            Ok(invocation) => invocation,
            Err(error) => return Ok(Err(error.into())),
        };
        let invocation_id = invocation.invocation_id().to_string();
        // Recorded before the stream is polled, so the user turn never reaches the session
        // without a checkpoint naming it.
        self.checkpoint(
            &record.run_id,
            epoch,
            RunCheckpointRef::Session {
                session_id: session_id.clone(),
                invocation_id: Some(invocation_id.clone()),
            },
        )
        .await?;

        let mut events = invocation.into_events();
        let mut text = String::new();
        let mut interrupted = false;
        let mut produced = false;
        let mut refused_while_suspending = false;
        loop {
            let next = tokio::select! {
                next = events.next() => next,
                () = cancel.cancelled(), if !interrupted => {
                    interrupted = true;
                    runner.interrupt_identity(runner.app_name(), user_id, &session_id);
                    continue;
                }
                () = lease.suspended.cancelled(), if !interrupted => {
                    interrupted = true;
                    tracing::info!(run.id = %record.run_id, "suspend requested; interrupting the run at its model-call boundary");
                    runner.interrupt_identity(runner.app_name(), user_id, &session_id);
                    continue;
                }
            };
            let Some(event) = next else { break };
            match event {
                Ok(event) => {
                    produced = true;
                    if !event.llm_response.partial
                        && event.author != "user"
                        && event.is_final_response()
                        && let Some(content) = &event.llm_response.content
                    {
                        text = content
                            .parts
                            .iter()
                            .filter_map(Part::text)
                            .collect::<Vec<_>>()
                            .join("");
                    }
                }
                // Before any event, a kill-switch refusal comes from the runner's start
                // check or the agent's check before its first model call: no model or tool
                // call ran, and the checkpoint resumes the run from its input or its
                // persisted user turn.
                Err(error)
                    if !produced
                        && (error.code == GOVERNANCE_FROZEN_CODE
                            || error.code == GOVERNANCE_STALE_CODE)
                        && self.refusal().is_some() =>
                {
                    return Ok(Ok(Attempt::Refused(error)));
                }
                // A refusal at the boundary the run was asked to suspend at stops it there
                // as the suspension would, so the run parks instead of failing.
                Err(error)
                    if lease.suspend_requested.load(Ordering::SeqCst)
                        && (error.code == GOVERNANCE_FROZEN_CODE
                            || error.code == GOVERNANCE_STALE_CODE) =>
                {
                    tracing::info!(run.id = %record.run_id, error = %error.diagnostics(), "governance refused the run it was asked to suspend; suspending it");
                    refused_while_suspending = true;
                    break;
                }
                Err(error) => return Ok(Err(error.into())),
            }
        }
        drop(events);

        if refused_while_suspending || lease.suspended.is_cancelled() {
            self.checkpoint(
                &record.run_id,
                epoch,
                RunCheckpointRef::Session { session_id, invocation_id: Some(invocation_id) },
            )
            .await?;
            return Ok(Ok(Attempt::Suspended));
        }

        let held = match held_by_invocation(runner, user_id, &session_id, &invocation_id).await {
            Ok(held) => held,
            Err(error) => return Ok(Err(error.into())),
        };
        if held.is_empty() {
            return Ok(Ok(Attempt::Done(agent_result(&text, &session_id, Some(&invocation_id)))));
        }
        let now = chrono::Utc::now();
        let previous = match &record.checkpoint {
            Some(RunCheckpointRef::AwaitingApproval { fingerprints, expires_at, .. })
                if fingerprints.iter().any(|fingerprint| held.contains(fingerprint)) =>
            {
                Some(*expires_at)
            }
            _ => None,
        };
        if let Some(Some(expired)) = previous
            && expired <= now
        {
            return Ok(Err(Failure {
                error: format!(
                    "approval expired at {expired} for held tool calls {held:?}; no decision was recorded"
                ),
                retryable: false,
            }));
        }
        let until = previous.unwrap_or_else(|| {
            self.config
                .approval_timeout
                .and_then(|timeout| chrono::Duration::from_std(timeout).ok())
                .and_then(|timeout| now.checked_add_signed(timeout))
        });
        tracing::info!(
            run.id = %record.run_id,
            session.id = %session_id,
            held.fingerprints = ?held,
            "agent run holds tool calls for approval"
        );
        self.checkpoint(
            &record.run_id,
            epoch,
            RunCheckpointRef::AwaitingApproval {
                session_id,
                invocation_id: Some(invocation_id),
                fingerprints: held,
                expires_at: until,
            },
        )
        .await?;
        Ok(Ok(Attempt::Parked(until)))
    }
}

/// The fingerprints of the tool calls `invocation_id` holds for approval: calls it
/// surfaced as confirmation requests that no later response answers.
async fn held_by_invocation(
    runner: &Runner,
    user_id: &str,
    session_id: &str,
    invocation_id: &str,
) -> adk_core::Result<Vec<String>> {
    let identity = adk_core::AdkIdentity::new(
        adk_core::AppName::try_from(runner.app_name())?,
        UserId::try_from(user_id)?,
        SessionId::try_from(session_id)?,
    );
    let events = runner.session_service().get_for_identity(&identity).await?.events().all();
    let mut surfaced: Vec<(&str, &str, Option<&str>)> = events
        .iter()
        .filter(|event| event.invocation_id == invocation_id)
        .filter_map(|event| {
            event.actions.tool_confirmation.as_ref().map(|request| {
                (event.author.as_str(), event.branch.as_str(), request.function_call_id.as_deref())
            })
        })
        .collect();
    surfaced.dedup();
    let mut held = Vec::new();
    for (author, branch, _) in &surfaced {
        for call in adk_core::held_tool_calls(&events, author, branch) {
            let from_this_invocation = surfaced.iter().any(|(a, b, id)| {
                a == author && b == branch && *id == call.request.function_call_id.as_deref()
            });
            let fingerprint = call.request.fingerprint();
            if from_this_invocation && !held.contains(&fingerprint) {
                held.push(fingerprint);
            }
        }
    }
    Ok(held)
}

enum Prepared {
    Start,
    Continue,
    Done(String),
}

/// Handle to a spawned [`DurableWorker`] loop.
#[derive(Debug)]
pub struct WorkerHandle {
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    wake: Arc<Notify>,
    heartbeat_now: Arc<Notify>,
    suspensions: Arc<Suspensions>,
}

impl WorkerHandle {
    /// Makes the loop try a claim now instead of after its poll interval, for example
    /// right after this process enqueued a run.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Makes every in-flight attempt heartbeat now, so a cancellation requested through
    /// the run store is seen at once rather than at the next heartbeat interval.
    pub fn heartbeat_now(&self) {
        self.heartbeat_now.notify_waiters();
    }

    /// Asks the agent run `run_id`, in flight on this worker, to stop at its next model-call
    /// boundary and park as `Waiting` on its `Session` checkpoint until woken (spec 3.2 §10
    /// S1). Returns whether the request reached an attempt; see
    /// [`DurableWorker::suspend`].
    pub fn suspend(&self, run_id: &str) -> bool {
        self.suspensions.request(run_id)
    }

    /// Stops claiming and waits for in-flight attempts to finish and write their outcome.
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.task.await;
    }

    /// Drops the loop and every in-flight attempt without writing, as a crash would.
    /// Their leases expire and another worker recovers the runs.
    pub fn abort(&self) {
        self.task.abort();
    }

    /// Whether the loop has ended.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// The agent attempts in flight on one worker, by run id.
#[derive(Default)]
struct Suspensions(std::sync::Mutex<HashMap<String, Arc<LeaseState>>>);

impl std::fmt::Debug for Suspensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let attempts = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("Suspensions").field("in_flight", &attempts.len()).finish()
    }
}

impl Suspensions {
    /// Registers the agent attempt of `run_id` holding `lease` until the returned
    /// registration drops.
    fn register(
        suspensions: &Arc<Self>,
        run_id: &str,
        lease: &Arc<LeaseState>,
    ) -> SuspendRegistration {
        suspensions
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(run_id.to_string(), Arc::clone(lease));
        SuspendRegistration {
            suspensions: Arc::clone(suspensions),
            run_id: run_id.to_string(),
            lease: Arc::clone(lease),
        }
    }

    fn request(&self, run_id: &str) -> bool {
        let attempts = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lease) = attempts.get(run_id) else {
            tracing::debug!(
                run.id = run_id,
                "no agent attempt of the run is in flight here; not suspending"
            );
            return false;
        };
        if !lease.suspend_requested.swap(true, Ordering::SeqCst) {
            tracing::info!(
                run.id = run_id,
                "suspend requested; the run parks at its next model-call boundary"
            );
        }
        true
    }
}

/// One attempt's place in [`Suspensions`], removed when the attempt ends however it ends.
struct SuspendRegistration {
    suspensions: Arc<Suspensions>,
    run_id: String,
    lease: Arc<LeaseState>,
}

impl Drop for SuspendRegistration {
    fn drop(&mut self) {
        let mut attempts = self.suspensions.0.lock().unwrap_or_else(|e| e.into_inner());
        if attempts.get(&self.run_id).is_some_and(|lease| Arc::ptr_eq(lease, &self.lease)) {
            attempts.remove(&self.run_id);
        }
    }
}

/// Stops an agent run at its next model-call boundary once it was asked to suspend: the
/// model call never starts, and the worker interrupts the turn.
struct SuspendGate(Arc<LeaseState>);

impl std::fmt::Debug for SuspendGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuspendGate").finish_non_exhaustive()
    }
}

#[async_trait]
impl InvocationHooks for SuspendGate {
    async fn before_model(
        &self,
        _ctx: Arc<dyn CallbackContext>,
        request: LlmRequest,
    ) -> adk_core::Result<BeforeModelResult> {
        if self.0.suspend_requested.load(Ordering::SeqCst) {
            self.0.suspended.cancel();
            // The worker interrupts the run, which drops this call before it returns.
            std::future::pending::<()>().await;
        }
        Ok(BeforeModelResult::Continue(request))
    }
}

/// What a worker knows about the lease it holds on one run, and the requests that stop
/// the attempt.
struct LeaseState {
    ttl: Duration,
    /// The lease is provably held until this instant: the last successful heartbeat (or
    /// the claim) was sent before it by `ttl`.
    valid_until: std::sync::Mutex<Instant>,
    lost_flag: AtomicBool,
    lost: Notify,
    cancel_requested: AtomicBool,
    /// Set by a suspend request; the attempt stops at its next model-call boundary.
    suspend_requested: AtomicBool,
    /// Cancelled by [`SuspendGate`] once the attempt stopped at a model-call boundary.
    suspended: CancellationToken,
}

impl LeaseState {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            valid_until: std::sync::Mutex::new(Instant::now() + ttl),
            lost_flag: AtomicBool::new(false),
            lost: Notify::new(),
            cancel_requested: AtomicBool::new(false),
            suspend_requested: AtomicBool::new(false),
            suspended: CancellationToken::new(),
        }
    }

    fn renewed(&self, sent: Instant) {
        *self.valid_until.lock().unwrap_or_else(|e| e.into_inner()) = sent + self.ttl;
    }

    fn is_valid(&self) -> bool {
        !self.lost_flag.load(Ordering::SeqCst)
            && Instant::now() < *self.valid_until.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lose(&self) {
        if !self.lost_flag.swap(true, Ordering::SeqCst) {
            self.lost.notify_waiters();
        }
        self.lost.notify_one();
    }

    /// Never returns once the lease is not provably held, so the attempt is dropped by
    /// the worker before it makes another call or writes another event.
    async fn fence(&self) {
        if !self.is_valid() {
            tracing::warn!("lease no longer provably held; fencing the attempt");
            self.lose();
            std::future::pending::<()>().await;
        }
    }
}

/// Fences an agent run: every model call, model response, tool call, and tool result
/// waits on [`LeaseState::fence`].
struct LeaseFence(Arc<LeaseState>);

impl std::fmt::Debug for LeaseFence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseFence").finish_non_exhaustive()
    }
}

#[async_trait]
impl InvocationHooks for LeaseFence {
    async fn before_model(
        &self,
        _ctx: Arc<dyn CallbackContext>,
        request: LlmRequest,
    ) -> adk_core::Result<BeforeModelResult> {
        self.0.fence().await;
        Ok(BeforeModelResult::Continue(request))
    }

    async fn after_model(
        &self,
        _ctx: Arc<dyn CallbackContext>,
        _response: LlmResponse,
    ) -> adk_core::Result<Option<LlmResponse>> {
        self.0.fence().await;
        Ok(None)
    }

    async fn before_tool(
        &self,
        _ctx: Arc<dyn CallbackContext>,
    ) -> adk_core::Result<Option<Content>> {
        self.0.fence().await;
        Ok(None)
    }

    async fn after_tool(
        &self,
        _ctx: Arc<dyn CallbackContext>,
    ) -> adk_core::Result<Option<Content>> {
        self.0.fence().await;
        Ok(None)
    }
}

async fn ensure_session(
    runner: &Runner,
    user_id: &str,
    session_id: &str,
) -> adk_core::Result<Box<dyn adk_session::Session>> {
    let identity = adk_core::AdkIdentity::new(
        adk_core::AppName::try_from(runner.app_name())?,
        UserId::try_from(user_id)?,
        SessionId::try_from(session_id)?,
    );
    // Only a missing session is created: any other read failure is returned, so a transient
    // backend error never replaces the stored session. A create that loses a race to another
    // worker reads the session that worker created.
    match runner.session_service().get_for_identity(&identity).await {
        Err(error) if error.is_not_found() => {
            let created = runner
                .session_service()
                .create(adk_session::CreateRequest {
                    app_name: runner.app_name().to_string(),
                    user_id: user_id.to_string(),
                    session_id: Some(session_id.to_string()),
                    state: std::collections::HashMap::new(),
                })
                .await;
            match created {
                Err(error) if error.is_conflict() => {
                    runner.session_service().get_for_identity(&identity).await
                }
                other => other,
            }
        }
        other => other,
    }
}

/// The final answer of `invocation_id`, when it is the last thing in the session.
fn completed_answer(events: &[adk_core::Event], invocation_id: &str) -> Option<String> {
    // An invocation that held tool calls ended on a notice, not an answer.
    if events.iter().any(|event| {
        event.invocation_id == invocation_id && event.actions.tool_confirmation.is_some()
    }) {
        return None;
    }
    let last = events.iter().rev().find(|event| !event.llm_response.partial)?;
    if last.invocation_id != invocation_id || last.author == "user" || !last.is_final_response() {
        return None;
    }
    let text: String = last
        .llm_response
        .content
        .as_ref()?
        .parts
        .iter()
        .filter_map(Part::text)
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

fn agent_result(text: &str, session_id: &str, invocation_id: Option<&str>) -> Value {
    let mut result = serde_json::json!({ "text": text, "sessionId": session_id });
    if let Some(invocation_id) = invocation_id {
        result["invocationId"] = Value::String(invocation_id.to_string());
    }
    result
}

/// The user turn an agent run starts from: a string, `{"text": ...}`, or
/// `{"content": Content}`; any other value is sent as its JSON text.
pub(crate) fn input_content(input: &Value) -> Content {
    match input {
        Value::String(text) => Content::new("user").with_text(text),
        Value::Object(map) => {
            if let Some(content) = map
                .get("content")
                .and_then(|value| serde_json::from_value::<Content>(value.clone()).ok())
            {
                return content;
            }
            match map.get("text").and_then(Value::as_str) {
                Some(text) => Content::new("user").with_text(text),
                None => Content::new("user").with_text(input.to_string()),
            }
        }
        other => Content::new("user").with_text(other.to_string()),
    }
}

/// The workflow state a run's input becomes: an object's fields, or `{"input": value}`.
fn workflow_input(input: &Value) -> WorkflowState {
    match input {
        Value::Object(map) => map.clone().into_iter().collect(),
        Value::Null => WorkflowState::new(),
        other => [("input".to_string(), other.clone())].into_iter().collect(),
    }
}
