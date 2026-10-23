//! The supervision state a kernel shares between its process table, its worker hooks and
//! its loops (spec 3.2 §7.2, §7.3, §7.6, §9.3).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::{
    Clock, DurableRunStore, GovernanceControl, Part, RunCheckpointRef, RunFilter, RunRecord,
    RunState, SpendKey,
};
use adk_server::background::{DurableWorker, ParkReason, WorkerHandle, WorkerHooks};
use adk_session::{GetRequest, SessionService};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::audit;
use crate::bus::{EventBus, KernelEvent, SpendDelta};
use crate::partition::PartitionId;
use crate::process::{ProcessKind, ProcessMeta, ProgressSource, RunUsage, Suspension};
use crate::sched::QueueClass;
use crate::stores::{ProcessFilter, ProcessStore};
use crate::wake::KERNEL_WAKE_TARGET;

/// The target `adk-harness` runs goals under (`adk_harness::GOAL_TARGET`); the kernel names
/// it without depending on the harness (spec 3.2 §6 D-1).
pub(crate) const GOAL_TARGET: &str = "harness.goal";

/// The idempotency-key prefix of a run [`ProcessTable::spawn`](crate::ProcessTable::spawn)
/// enqueued asleep. The run itself carries the intent, so its first claim parks it before
/// its first model call however long its side-table row takes to write.
pub(crate) const ASLEEP_KEY_PREFIX: &str = "kernel:asleep:";
/// The idempotency-key prefix of a run `spawn` enqueued awake.
pub(crate) const SPAWN_KEY_PREFIX: &str = "kernel:spawn:";

/// Whether `ProcessTable::spawn` enqueued `run`, so `spawn`, not a hook, writes its row.
pub(crate) fn spawned_by_kernel(run: &RunRecord) -> bool {
    run.idempotency_key
        .as_deref()
        .is_some_and(|key| key.starts_with(ASLEEP_KEY_PREFIX) || key.starts_with(SPAWN_KEY_PREFIX))
}

/// Whether a claim of `run` must park it before its first model call (K-I2): its process
/// sleeps until a wake consumes its wake set. The process row says so once `spawn` wrote
/// it, and the run's key until then. A run can become claimable without a wake — a freeze
/// hands a claimed run back, a resume after a drain wakes what the drain parked, a lost
/// lease is recovered — so neither a checkpoint nor a cleared sleep request means the
/// process was woken; only the consumed wake set does, and every wake clears it before it
/// makes the run claimable.
pub(crate) fn asleep_at_claim(run: &RunRecord, stored: Option<&ProcessMeta>) -> bool {
    match stored {
        Some(meta) => meta.wake.is_some(),
        None => {
            run.idempotency_key.as_deref().is_some_and(|key| key.starts_with(ASLEEP_KEY_PREFIX))
        }
    }
}

/// How many spend samples a running process keeps for its burn rate.
const SAMPLES: usize = 64;
/// How many queued maintenance runs the tick's probe reads, oldest first.
const EXTERNAL_WAKE_PROBE: usize = 8;

/// What this replica knows about a process its own workers run.
#[derive(Debug, Clone)]
pub(crate) struct LocalRun {
    pub(crate) run: RunRecord,
    pub(crate) partition: PartitionId,
    pub(crate) claimed_at: DateTime<Utc>,
    /// The hosted agent's usage counter for the run's session when the claim started.
    pub(crate) baseline: Option<RunUsage>,
    /// The counter at the last attribution.
    pub(crate) last: Option<RunUsage>,
    /// Spend attributed to this claim so far, and when, for the burn rate.
    pub(crate) samples: VecDeque<(DateTime<Utc>, u64)>,
    pub(crate) attributed: u64,
}

/// What every kernel part shares: the stores, the bus, the kill switch, the workers.
pub(crate) struct Supervision {
    pub(crate) app_name: String,
    pub(crate) kernel_id: String,
    pub(crate) queues: Vec<QueueClass>,
    pub(crate) runs: Arc<dyn DurableRunStore>,
    pub(crate) processes: Arc<dyn ProcessStore>,
    pub(crate) bus: EventBus,
    pub(crate) governance: GovernanceControl,
    pub(crate) audit: Option<AuditTrail>,
    pub(crate) sessions: Option<Arc<dyn SessionService>>,
    pub(crate) progress: Option<Arc<dyn ProgressSource>>,
    /// One worker per queue class, set once the hooks that reference this state exist.
    pub(crate) workers: OnceLock<Vec<(QueueClass, DurableWorker)>>,
    /// The claim loops while the kernel runs.
    pub(crate) handles: Mutex<Vec<(QueueClass, WorkerHandle)>>,
    pub(crate) draining: AtomicBool,
    pub(crate) local: Mutex<HashMap<String, LocalRun>>,
    /// Signalled when a process may need a file root watched that no source covers yet.
    pub(crate) roots_changed: tokio::sync::Notify,
    /// One file wake source per watched root, while the kernel runs (spec 3.2 §7.4).
    pub(crate) watched: Mutex<BTreeMap<PathBuf, tokio::task::JoinHandle<()>>>,
    /// The kernel's clock ([`KernelConfig::clock`](crate::KernelConfig::clock)).
    pub(crate) clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Supervision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervision")
            .field("app_name", &self.app_name)
            .field("kernel_id", &self.kernel_id)
            .field("queues", &self.queues)
            .finish_non_exhaustive()
    }
}

impl Supervision {
    /// The side table a run gets when nothing recorded one: the kind read from its target
    /// and idempotency key, in the default partition.
    pub(crate) fn infer_meta(run: &RunRecord) -> ProcessMeta {
        let key = run.idempotency_key.as_deref().unwrap_or_default();
        let source = |prefix: &str| {
            key.strip_prefix(prefix)
                .and_then(|rest| rest.split(':').next())
                .map(|name| format!("{}{name}", prefix))
        };
        let kind = if run.target == KERNEL_WAKE_TARGET || run.queue == QueueClass::MAINTENANCE {
            ProcessKind::Maintenance
        } else if run.target == GOAL_TARGET {
            ProcessKind::Goal
        } else if key.starts_with("schedule:") {
            ProcessKind::Scheduled
        } else if let Some(source) =
            ["hook:", "queue:", "cron:", "file:", "run:"].iter().find_map(|prefix| source(prefix))
        {
            ProcessKind::Triggered { source }
        } else {
            ProcessKind::Interactive
        };
        ProcessMeta::new(run.run_id.clone(), QueueClass::from_name(&run.queue), kind)
    }

    /// The stored side table of `run`, or `None`.
    pub(crate) async fn stored_meta(&self, run: &RunRecord) -> Option<ProcessMeta> {
        match self.processes.get(&run.run_id).await {
            Ok(meta) => meta,
            Err(error) => {
                tracing::warn!(run.id = %run.run_id, error = %error, "could not read the process side table");
                None
            }
        }
    }

    /// Writes `meta`, logging a failure: a hook cannot fail the attempt it observes.
    pub(crate) async fn put_meta(&self, meta: ProcessMeta) {
        let pid = meta.pid.clone();
        if let Err(error) = self.processes.put(meta).await {
            tracing::warn!(process.pid = %pid, error = %error, "could not write the process side table");
        }
    }

    /// Publishes `event` on the bus.
    pub(crate) fn publish(&self, event: KernelEvent) {
        self.bus.publish(event);
    }

    /// The actor of a record about `run`: its acting context, on this replica.
    pub(crate) fn actor_for(&self, run: Option<&RunRecord>) -> AuditActor {
        let mut actor = match run.and_then(|run| run.principal.as_ref()) {
            Some(principal) => AuditActor::new(principal, ""),
            None => AuditActor::default(),
        };
        actor.instance = Some(self.kernel_id.clone());
        actor
    }

    /// Writes a `kernel.*` audit record, when the kernel has a trail (spec 3.2 §9.3).
    pub(crate) async fn record(&self, action: &str, actor: AuditActor, fields: Value) {
        let Some(trail) = &self.audit else { return };
        let mut draft = AuditRecordDraft::new(
            AuditRecordKind::Other(audit::KERNEL_AUDIT_KIND.to_string()),
            action,
        )
        .actor(actor);
        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                draft = draft.ext(key, value);
            }
        }
        if let Err(error) = trail.record(draft).await {
            tracing::warn!(audit.action = action, error = %error, "could not write a kernel audit record");
        }
    }

    /// The fields of a `kernel.process.*` record (spec 3.2 §9.3).
    pub(crate) fn process_fields(
        meta: &ProcessMeta,
        reason: Option<&str>,
        by: Option<&str>,
    ) -> Value {
        json!({
            "pid": meta.pid,
            "parent": meta.parent,
            "partition": meta.partition,
            "queue": meta.queue,
            "kind": meta.kind,
            "wake": meta.wake,
            "suspendedBy": meta.suspended_by,
            "reason": reason,
            "by": by,
        })
    }

    /// The workers of every queue class.
    pub(crate) fn workers(&self) -> &[(QueueClass, DurableWorker)] {
        self.workers.get().map(Vec::as_slice).unwrap_or_default()
    }

    /// The lease owner names this replica's workers write.
    pub(crate) fn local_worker_ids(&self) -> Vec<String> {
        self.workers().iter().map(|(_, worker)| worker.config().worker_id.clone()).collect()
    }

    /// Asks the attempt of `pid` running on this replica to stop at its next model-call
    /// boundary; `false` when none of this replica's workers runs it.
    pub(crate) fn suspend_local(&self, pid: &str) -> bool {
        self.workers().iter().any(|(_, worker)| worker.suspend(pid))
    }

    /// The roots this kernel watches for file wakes.
    #[cfg(test)]
    pub(crate) fn watched_roots(&self) -> Vec<PathBuf> {
        self.watched.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
    }

    /// Makes every claim loop try a claim now.
    pub(crate) fn nudge(&self) {
        for (_, handle) in self.handles.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            handle.wake();
        }
    }

    /// Makes every in-flight attempt heartbeat now, so a cancellation is seen at once.
    pub(crate) fn heartbeat_now(&self) {
        for (_, handle) in self.handles.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            handle.heartbeat_now();
        }
    }

    /// Whether a `kernel.wake` run is due on the maintenance queue: one the worker would
    /// claim at its next poll. Reads at most a few rows, oldest first.
    pub(crate) async fn external_wakes_due(&self) -> bool {
        if !self.queues.iter().any(|queue| queue.as_str() == QueueClass::MAINTENANCE) {
            return false;
        }
        let filter = RunFilter::default()
            .queue(QueueClass::MAINTENANCE)
            .states([RunState::Queued])
            .limit(EXTERNAL_WAKE_PROBE);
        let now = self.clock.now();
        match self.runs.list(filter).await {
            Ok(queued) => queued.iter().any(|run| {
                run.target == KERNEL_WAKE_TARGET && run.not_before.is_none_or(|at| at <= now)
            }),
            Err(error) => {
                tracing::debug!(error = %error, "could not look for queued wake runs");
                false
            }
        }
    }

    /// The running runs of the kernel's queues.
    pub(crate) async fn running(&self) -> Vec<RunRecord> {
        let mut running = Vec::new();
        for queue in &self.queues {
            let filter = RunFilter::default().queue(queue.as_str()).states([RunState::Running]);
            match self.runs.list(filter).await {
                Ok(runs) => running.extend(runs),
                Err(error) => {
                    tracing::warn!(queue = %queue, error = %error, "could not list running runs")
                }
            }
        }
        running
    }

    /// Wakes every `Waiting` process parked by `suspension` (a freeze or a drain) and clears
    /// the mark; returns how many it woke. A manual or preempted suspension is never woken
    /// here (K-I21).
    pub(crate) async fn wake_suspended(&self, suspension: &Suspension) -> usize {
        let metas = match self.processes.list(&ProcessFilter::default()).await {
            Ok(metas) => metas,
            Err(error) => {
                tracing::warn!(error = %error, "could not list processes to wake");
                return 0;
            }
        };
        let mut woken = 0;
        for mut meta in
            metas.into_iter().filter(|meta| meta.suspended_by.as_ref() == Some(suspension))
        {
            let run = match self.runs.get(&meta.pid).await {
                Ok(Some(run)) => run,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(process.pid = %meta.pid, error = %error, "could not read a parked run");
                    continue;
                }
            };
            if run.state == RunState::Waiting {
                match self.runs.wake(&meta.pid).await {
                    Ok(()) => woken += 1,
                    Err(error) => {
                        tracing::warn!(process.pid = %meta.pid, error = %error, "could not wake a parked run");
                        continue;
                    }
                }
            }
            if run.state != RunState::Running {
                meta.suspended_by = None;
                self.put_meta(meta).await;
            }
        }
        if woken > 0 {
            self.nudge();
        }
        woken
    }

    /// Asks this replica's workers to suspend every running process with a pending request
    /// they lease, and clears requests whose run already parked or ended (spec 3.2 §7.3).
    pub(crate) async fn mirror_suspend_requests(&self) {
        let pending =
            match self.processes.list(&ProcessFilter::default().suspend_requested(true)).await {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::debug!(error = %error, "could not list pending suspensions");
                    return;
                }
            };
        if pending.is_empty() {
            return;
        }
        let ours = self.local_worker_ids();
        for meta in pending {
            let Ok(Some(run)) = self.runs.get(&meta.pid).await else { continue };
            match run.state {
                RunState::Running => {
                    if run.lease_owner.as_ref().is_some_and(|owner| ours.contains(owner)) {
                        self.suspend_local(&meta.pid);
                    }
                }
                // A queued run parks when it is claimed.
                RunState::Queued => {}
                _ => {
                    if let Err(error) = self.processes.set_suspend_requested(&meta.pid, false).await
                    {
                        tracing::debug!(process.pid = %meta.pid, error = %error, "could not clear a suspension request");
                    }
                }
            }
        }
    }

    /// Attributes the spend the hosted agent counted since the last attribution to `pid`,
    /// on today's row (spec 3.2 §7.10).
    pub(crate) async fn attribute(&self, pid: &str) {
        let Some(progress) = &self.progress else { return };
        let Some(local) = self.local.lock().unwrap_or_else(|e| e.into_inner()).get(pid).cloned()
        else {
            return;
        };
        let usage = match progress.usage(&local.run).await {
            Ok(Some(usage)) => usage,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(process.pid = pid, error = %error, "could not read the run's usage");
                return;
            }
        };
        let previous = local.last.or(local.baseline).unwrap_or_default();
        // A counter that went down was restarted with the agent; everything since counts.
        let delta = if usage.cost_micro_usd >= previous.cost_micro_usd {
            usage.cost_micro_usd - previous.cost_micro_usd
        } else {
            usage.cost_micro_usd
        };
        let now = self.clock.now();
        {
            let mut table = self.local.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = table.get_mut(pid) {
                entry.last = Some(usage);
                entry.attributed = entry.attributed.saturating_add(delta);
                entry.samples.push_back((now, entry.attributed));
                if entry.samples.len() > SAMPLES {
                    entry.samples.pop_front();
                }
            }
        }
        if delta == 0 {
            return;
        }
        let delta = SpendDelta::new(delta);
        if let Err(error) =
            self.processes.add_spend(pid, &local.partition, now.date_naive(), delta).await
        {
            tracing::warn!(process.pid = pid, error = %error, "could not record the process's spend");
        }
        let mut key = SpendKey::org(self.app_name.clone());
        if let Some(principal) = &local.run.principal {
            key = key.with_agent(principal.agent.id.to_string());
        }
        self.publish(KernelEvent::Spend { partition: local.partition.clone(), key, delta });
    }

    /// Attributes the spend of every process this replica runs.
    pub(crate) async fn attribute_all(&self) {
        let pids: Vec<String> =
            self.local.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        for pid in pids {
            self.attribute(&pid).await;
        }
    }

    /// Ends this replica's watch of `pid`, attributing what it spent since the last tick.
    pub(crate) async fn release(&self, pid: &str) {
        self.attribute(pid).await;
        self.local.lock().unwrap_or_else(|e| e.into_inner()).remove(pid);
    }
}

/// The model and tool calls of one process, read from its session (spec 3.2 §7.3, §7.10).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TranscriptCounts {
    pub(crate) model_calls: u64,
    pub(crate) tool_calls: u64,
    /// How long each answered tool call took, in seconds.
    pub(crate) tool_durations: Vec<f64>,
}

/// The session a run continues: the one its checkpoint names, its own, or the
/// `run-{run_id}` session a worker creates for a run that names none.
pub(crate) fn run_session(run: &RunRecord) -> String {
    match (&run.checkpoint, &run.session_id) {
        (
            Some(
                RunCheckpointRef::Session { session_id, .. }
                | RunCheckpointRef::AwaitingApproval { session_id, .. },
            ),
            _,
        ) => session_id.clone(),
        (_, Some(session_id)) => session_id.clone(),
        _ => format!("run-{}", run.run_id),
    }
}

/// Counts the model calls, tool calls and tool durations of `run` in its session: the
/// events from its creation on, and, for a finished run, up to its end.
pub(crate) async fn transcript_counts(
    sessions: &dyn SessionService,
    run: &RunRecord,
) -> Option<TranscriptCounts> {
    let session_id = run_session(run);
    let session = sessions
        .get(GetRequest {
            app_name: run.app_name.clone(),
            user_id: run.user_id.clone(),
            session_id,
            num_recent_events: None,
            after: Some(run.created_at),
        })
        .await
        .ok()?;
    let end = run.state.is_terminal().then_some(run.updated_at);
    let mut counts = TranscriptCounts::default();
    let mut started: HashMap<String, DateTime<Utc>> = HashMap::new();
    for event in session.events().all() {
        if event.timestamp < run.created_at || end.is_some_and(|end| event.timestamp > end) {
            continue;
        }
        let Some(content) = &event.llm_response.content else { continue };
        if content.role == "model" && !event.llm_response.partial && event.author != "user" {
            counts.model_calls += 1;
        }
        for part in &content.parts {
            match part {
                Part::FunctionCall { name, id, .. } if !event.llm_response.partial => {
                    counts.tool_calls += 1;
                    started.insert(id.clone().unwrap_or_else(|| name.clone()), event.timestamp);
                }
                Part::FunctionResponse { function_response, id, .. } => {
                    let key = id.clone().unwrap_or_else(|| function_response.name.clone());
                    if let Some(at) = started.remove(&key) {
                        let seconds =
                            (event.timestamp - at).num_milliseconds().max(0) as f64 / 1000.0;
                        counts.tool_durations.push(seconds);
                    }
                }
                _ => {}
            }
        }
    }
    Some(counts)
}

/// The worker hooks that keep the side table, the bus, spend attribution and the audit log
/// in step with every run a kernel worker moves (spec 3.2 §10 S1, §7.6, §9.3).
pub(crate) struct KernelHooks {
    pub(crate) sup: Arc<Supervision>,
    /// Hears every finished run except the kernel's own `kernel.wake` runs, whose finish
    /// would otherwise start another `kernel.wake` run, without end.
    pub(crate) run_finished: Arc<crate::wake::RunFinishedWakeSource>,
}

impl std::fmt::Debug for KernelHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelHooks").finish_non_exhaustive()
    }
}

#[async_trait]
impl WorkerHooks for KernelHooks {
    async fn on_claim(&self, run: &RunRecord) {
        let sup = &self.sup;
        let stored = sup.stored_meta(run).await;
        let persisted = stored.is_some();
        let asleep = asleep_at_claim(run, stored.as_ref());
        let mut meta = stored.unwrap_or_else(|| Supervision::infer_meta(run));
        let from = if meta.wake.is_some() || meta.suspended_by.is_some() || run.attempt > 1 {
            RunState::Waiting
        } else {
            RunState::Queued
        };
        // A run `spawn` enqueued gets its row from `spawn`, which may still be writing it; an
        // inferred row written here could replace it.
        let mut changed = !persisted && !spawned_by_kernel(run);
        if asleep || meta.suspend_requested || sup.draining.load(Ordering::SeqCst) {
            // A sleeper, a suspension asked while it queued, or a drain: the attempt parks
            // at its first model-call boundary.
            sup.suspend_local(&run.run_id);
        } else if persisted {
            // Woken, or its suspension lifted: the wake set is already consumed.
            changed |= meta.suspended_by.take().is_some();
        }
        if changed {
            sup.put_meta(meta.clone()).await;
        }
        let baseline = match &sup.progress {
            Some(progress) => progress.usage(run).await.ok().flatten(),
            None => None,
        };
        sup.local.lock().unwrap_or_else(|e| e.into_inner()).insert(
            run.run_id.clone(),
            LocalRun {
                run: run.clone(),
                partition: meta.partition.clone(),
                claimed_at: sup.clock.now(),
                baseline,
                last: None,
                samples: VecDeque::new(),
                attributed: 0,
            },
        );
        sup.publish(KernelEvent::Process {
            pid: run.run_id.clone(),
            from,
            to: RunState::Running,
            reason: Some("claimed".to_string()),
        });
    }

    async fn on_park(&self, run: &RunRecord, reason: ParkReason) {
        let sup = &self.sup;
        sup.release(&run.run_id).await;
        let stored = sup.stored_meta(run).await;
        if stored.is_none() && spawned_by_kernel(run) {
            // `spawn` has not written the row yet; the row it writes is the sleeper's, and
            // the 250 ms mirror clears its pending request now that the run parked.
            let label = if reason == ParkReason::Suspended { "asleep" } else { "parked" };
            sup.publish(KernelEvent::Process {
                pid: run.run_id.clone(),
                from: RunState::Running,
                to: RunState::Waiting,
                reason: Some(label.to_string()),
            });
            return;
        }
        let mut meta = stored.unwrap_or_else(|| Supervision::infer_meta(run));
        let draining = sup.draining.load(Ordering::SeqCst);
        let label = match reason {
            ParkReason::Suspended => {
                meta.suspend_requested = false;
                if meta.suspended_by.is_none() {
                    if draining {
                        meta.suspended_by = Some(Suspension::Drain);
                    } else if meta.wake.is_none() {
                        meta.suspended_by = Some(Suspension::Manual);
                    }
                }
                if meta.suspended_by.is_some() { "suspended" } else { "asleep" }
            }
            ParkReason::Frozen => {
                meta.suspend_requested = false;
                meta.suspended_by = Some(Suspension::Freeze);
                "frozen"
            }
            ParkReason::AwaitingApproval => "awaiting_approval",
            ParkReason::HandedBack => "handed_back",
            _ => "parked",
        };
        sup.put_meta(meta.clone()).await;
        sup.publish(KernelEvent::Process {
            pid: run.run_id.clone(),
            from: RunState::Running,
            to: RunState::Waiting,
            reason: Some(label.to_string()),
        });
        if matches!(reason, ParkReason::Suspended | ParkReason::Frozen) {
            sup.record(
                audit::PROCESS_SUSPENDED,
                sup.actor_for(Some(run)),
                Supervision::process_fields(&meta, Some(label), None),
            )
            .await;
        }
    }

    async fn on_finish(&self, run: &RunRecord) {
        let sup = &self.sup;
        if run.target == KERNEL_WAKE_TARGET {
            // A routed wake may have made sleepers claimable: claim them now rather than at
            // the next poll (spec 3.2 §7.4, local event to `Running` under 2 s).
            sup.nudge();
        } else {
            self.run_finished.on_finish(run).await;
        }
        sup.release(&run.run_id).await;
        if run.state.is_terminal()
            && let Some(mut meta) = sup.stored_meta(run).await
            && (meta.suspend_requested || meta.suspended_by.is_some() || meta.wake.is_some())
        {
            meta.suspend_requested = false;
            meta.suspended_by = None;
            let had_wake = meta.wake.take().is_some();
            sup.put_meta(meta).await;
            if had_wake {
                sup.roots_changed.notify_one();
            }
        }
        sup.publish(KernelEvent::Process {
            pid: run.run_id.clone(),
            from: RunState::Running,
            to: run.state,
            reason: run.error.clone(),
        });
        #[cfg(feature = "proc")]
        if let Some(sessions) = &sup.sessions
            && run.state.is_terminal()
            && let Some(counts) = transcript_counts(sessions.as_ref(), run).await
        {
            crate::kernel::record_histograms(&run.target, &counts);
        }
    }
}

#[cfg(test)]
mod tests {
    use adk_core::{NewRun, RunCheckpointRef};
    use serde_json::json;

    use super::*;

    fn run(queue: &str, target: &str, key: Option<&str>) -> RunRecord {
        let mut run = NewRun::new(queue, "adk-rust", "alice", target, json!({}));
        run.idempotency_key = key.map(str::to_string);
        RunRecord::queued("run-1", run, Utc::now())
    }

    #[test]
    fn the_kind_of_an_unrecorded_run_is_read_from_its_target_and_key() {
        let kind = |queue, target, key| Supervision::infer_meta(&run(queue, target, key)).kind;
        assert_eq!(kind("interactive", "adk-rust", None), ProcessKind::Interactive);
        assert_eq!(kind("background", GOAL_TARGET, None), ProcessKind::Goal);
        assert_eq!(
            kind("maintenance", KERNEL_WAKE_TARGET, Some("hook:gh:1")),
            ProcessKind::Maintenance
        );
        assert_eq!(
            kind("background", "adk-rust", Some("schedule:nightly:2026-10-22T02:00:00Z")),
            ProcessKind::Scheduled
        );
        assert_eq!(
            kind("background", "adk-rust", Some("hook:github:msg_1")),
            ProcessKind::Triggered { source: "hook:github".into() }
        );
        assert_eq!(
            kind("background", "adk-rust", Some("file:/w:/w/Cargo.lock:2026")),
            ProcessKind::Triggered { source: "file:/w".into() }
        );
        let meta = Supervision::infer_meta(&run("payments", "adk-rust", None));
        assert_eq!(meta.queue, QueueClass::Named("payments".into()));
        assert_eq!(meta.partition, PartitionId::default());
    }

    #[test]
    fn a_run_continues_the_session_its_checkpoint_names() {
        let mut record = run("interactive", "adk-rust", None);
        assert_eq!(run_session(&record), "run-run-1", "the worker's default session");
        record.session_id = Some("s1".into());
        assert_eq!(run_session(&record), "s1");
        record.checkpoint =
            Some(RunCheckpointRef::Session { session_id: "s2".into(), invocation_id: None });
        assert_eq!(run_session(&record), "s2");
    }
}
