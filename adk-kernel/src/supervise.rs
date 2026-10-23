//! The supervision state a kernel shares between its process table, its worker hooks and
//! its loops (spec 3.2 §7.2, §7.3, §7.6, §9.3).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{
    Clock, DurableRunStore, GovernanceControl, Part, RunCheckpointRef, RunFilter, RunRecord,
    RunState, SpendKey,
};
use adk_server::background::{DurableWorker, ParkReason, WorkerHandle, WorkerHooks};
use adk_session::{GetRequest, SessionService};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::agenda::{AgendaItem, AgendaState, AgendaStore, ProactivityPolicy};
use crate::audit;
use crate::bus::{EventBus, KernelEvent, SpendDelta};
use crate::partition::PartitionId;
use crate::process::{ProcessKind, ProcessMeta, ProgressSource, RunUsage, Suspension};
use crate::sched::QueueClass;
use crate::sched::{
    Admission, AdmissionInput, ProcessClass, QuietHours, Quotas, admit, youngest_background,
};
use crate::stores::{ProcessFilter, ProcessStore};
use crate::wake::KERNEL_WAKE_TARGET;
use crate::wake::{Wake, WakeSet};

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

/// Whether the kernel writes `run`'s row itself after enqueueing it, so no hook may write an
/// inferred one: a run `spawn` enqueued, or one a trigger in `triggers` fired, keyed
/// `<event key>#<trigger name>` ([`keys::trigger`](crate::keys::trigger)). A claim can
/// overtake that write, and an inferred row written then would replace the recorded one.
pub(crate) fn writes_own_row(run: &RunRecord, triggers: &BTreeSet<String>) -> bool {
    spawned_by_kernel(run)
        || (run.target != KERNEL_WAKE_TARGET
            && run
                .idempotency_key
                .as_deref()
                .and_then(|key| key.rsplit_once('#'))
                .is_some_and(|(_, trigger)| triggers.contains(trigger)))
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

/// How often, and how many times, a claim waits for the row of a run `spawn` enqueued.
const SPAWN_ROW_POLL: std::time::Duration = std::time::Duration::from_millis(20);
const SPAWN_ROW_POLLS: usize = 50;

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
    /// Every partition's proactivity limits (spec 3.2 §7.5).
    pub(crate) proactivity: ProactivityPolicy,
    /// The kernel's quiet hours, beside the proactivity policy's (spec 3.2 §7.2, §7.9).
    pub(crate) quiet_hours: Option<QuietHours>,
    /// Every partition's quotas (spec 3.2 §7.9).
    pub(crate) quotas: Quotas,
    /// The agenda's items (spec 3.2 §7.5).
    pub(crate) agenda: Arc<dyn AgendaStore>,
    /// Serializes admission decisions on this replica, so two claims never both take the
    /// partition's last slot.
    pub(crate) admission: tokio::sync::Mutex<()>,
    /// The standing rules the daily review reads (spec 3.2 §9.1).
    pub(crate) rule_source: Option<Arc<dyn crate::review::StandingRuleSource>>,
    /// The UTC day this replica last reviewed.
    pub(crate) reviewed: Mutex<Option<chrono::NaiveDate>>,
    /// The cron expressions of sleepers the kernel fires itself, each with the occurrence
    /// it last fired, or when it was first needed (spec 3.2 §7.4).
    pub(crate) crons: Mutex<BTreeMap<String, DateTime<Utc>>>,
    /// The names of the router's [`Trigger`](crate::Trigger)s, whose runs the router
    /// records itself.
    pub(crate) triggers: BTreeSet<String>,
    /// The daily baseline pass the maintenance tick enqueues (spec 3.2 §7.10).
    #[cfg(feature = "proc-baselines")]
    pub(crate) proc: Option<Arc<crate::proc::baselines::ProcRuntime>>,
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

/// The label of a process spawned asleep on a wake set with a cron wake: the wake set, as
/// JSON, its successor sleeps on (spec 3.2 §7.4, §8.1 `adk-rust wake --cron`).
pub(crate) const RECUR_LABEL: &str = "kernel.recur";

/// The label a process deferred by admission carries: `quiet_hours`, `budget` or `quota`
/// (spec 3.2 §7.9).
pub(crate) const ADMISSION_LABEL: &str = "kernel.admission";

/// Whether `run` is one of the kernel's own `kernel.wake` runs, which admission never
/// counts or holds.
fn is_wake_run(run: &RunRecord) -> bool {
    run.target == KERNEL_WAKE_TARGET
}

/// How admission treats a process (spec 3.2 §7.9): a human turn on the `interactive`
/// queue; agenda follow-ups and the kernel's maintenance processes as proactive; the rest
/// as background.
pub(crate) fn class_of(run: &RunRecord, meta: &ProcessMeta) -> ProcessClass {
    match meta.kind {
        ProcessKind::Interactive => ProcessClass::Interactive,
        ProcessKind::Proposed | ProcessKind::Maintenance => ProcessClass::Proactive,
        _ if run.queue == QueueClass::INTERACTIVE => ProcessClass::Interactive,
        _ => ProcessClass::Background,
    }
}

impl Supervision {
    /// The partition's spend today, in micro-USD, from the kernel's daily totals.
    pub(crate) async fn partition_spent(&self, partition: &PartitionId) -> u64 {
        let today = self.clock.now().date_naive();
        let scope = crate::stores::SpendScope::Partition(partition.clone());
        self.processes.spend(&scope, today, today).await.unwrap_or_else(|error| {
            tracing::warn!(partition = %partition, error = %error, "could not read the partition's spend; treating it as spent");
            u64::MAX
        })
    }

    /// The partition's proactive spend today: the daily totals of its agenda and maintenance
    /// processes, in micro-USD.
    pub(crate) async fn proactive_spent(&self, partition: &PartitionId) -> u64 {
        let today = self.clock.now().date_naive();
        let metas = match self
            .processes
            .list(&ProcessFilter::default().partition(partition.clone()))
            .await
        {
            Ok(metas) => metas,
            Err(error) => {
                tracing::warn!(partition = %partition, error = %error, "could not list the partition's processes; treating its budget as spent");
                return u64::MAX;
            }
        };
        let mut total = 0u64;
        for meta in metas
            .iter()
            .filter(|meta| matches!(meta.kind, ProcessKind::Proposed | ProcessKind::Maintenance))
        {
            let scope = crate::stores::SpendScope::Process(meta.pid.clone());
            match self.processes.spend(&scope, today, today).await {
                Ok(spent) => total = total.saturating_add(spent),
                Err(error) => {
                    tracing::warn!(process.pid = %meta.pid, error = %error, "could not read a process's spend; treating the budget as spent");
                    return u64::MAX;
                }
            }
        }
        total
    }

    /// The processes of `partition` queued on the kernel's queues, its `kernel.wake` runs
    /// aside (spec 3.2 §7.9 `max_queued`).
    pub(crate) async fn queued_in(&self, partition: &PartitionId) -> usize {
        let mut queued = 0;
        for queue in &self.queues {
            let filter = RunFilter::default().queue(queue.as_str()).states([RunState::Queued]);
            let Ok(runs) = self.runs.list(filter).await else { continue };
            for run in runs.iter().filter(|run| !is_wake_run(run)) {
                let meta = self.stored_meta(run).await.unwrap_or_else(|| Self::infer_meta(run));
                if meta.partition == *partition {
                    queued += 1;
                }
            }
        }
        queued
    }

    /// The cost cap of the daily proactive budget.
    fn daily_budget_cost(&self) -> Option<u64> {
        self.proactivity.daily_budget.max_cost_micro_usd
    }

    /// Every quiet-hours window that applies: the kernel's and the proactivity policy's.
    fn quiet_windows(&self) -> Vec<QuietHours> {
        self.quiet_hours.iter().chain(self.proactivity.quiet_hours.iter()).copied().collect()
    }

    /// The earliest instant at or after `now` outside every quiet-hours window: when a
    /// proactive process may start (spec 3.2 §7.9).
    pub(crate) fn quiet_release(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let quiet = self.quiet_windows();
        let probe = AdmissionInput {
            class: ProcessClass::Proactive,
            approval_woken: false,
            now,
            quiet_hours: &quiet,
            quotas: &Quotas::default(),
            running: 0,
            preemptible: false,
            partition_spent: 0,
            proactive_spent: 0,
            daily_budget: None,
            need: 0,
        };
        match admit(&probe) {
            Admission::QuietHours { until } => until,
            _ => now,
        }
    }

    /// The admitted processes of `partition` running now, `except` excluded: their run is
    /// `Running`, it is not a `kernel.wake` run, and it is not on its way to park. Returns
    /// their count and the `background` ones a human turn may preempt, with their creation
    /// time.
    async fn admitted(
        &self,
        partition: &PartitionId,
        except: &str,
    ) -> (usize, Vec<(String, DateTime<Utc>)>) {
        let mut count = 0;
        let mut background = Vec::new();
        for run in self.running().await {
            if run.run_id == except || is_wake_run(&run) {
                continue;
            }
            let meta = self.stored_meta(&run).await.unwrap_or_else(|| Self::infer_meta(&run));
            if meta.partition != *partition
                || meta.wake.is_some()
                || meta.suspend_requested
                || meta.suspended_by.is_some()
            {
                continue;
            }
            count += 1;
            if run.queue == QueueClass::BACKGROUND {
                background.push((run.run_id.clone(), run.created_at));
            }
        }
        (count, background)
    }

    /// The cost cap of the budget of the agenda item `meta` runs; 0 without one.
    async fn item_need(&self, meta: &ProcessMeta) -> u64 {
        let Some(id) = &meta.agenda_item else { return 0 };
        match self.agenda.get(id).await {
            Ok(Some(item)) => item.budget.and_then(|budget| budget.max_cost_micro_usd).unwrap_or(0),
            _ => 0,
        }
    }

    /// Admits the claimed `run` or defers it (spec 3.2 §7.9, K-I11): a deferred process gets
    /// a wake set — the end of quiet hours, its partition's `BudgetReplenished`, or a slot —
    /// and the [`ADMISSION_LABEL`], written before the gate is released, and parks at its
    /// first model-call boundary. An interactive process that finds its partition at quota
    /// preempts the youngest background process first.
    pub(crate) async fn admit_claim(&self, run: &RunRecord, meta: &mut ProcessMeta) -> Admission {
        if is_wake_run(run) {
            return Admission::Admit;
        }
        let class = class_of(run, meta);
        let quiet = self.quiet_windows();
        let _gate = self.admission.lock().await;
        let (running, background) = self.admitted(&meta.partition, &run.run_id).await;
        let budgeted = class != ProcessClass::Interactive
            && (self.quotas.daily_spend_cap.is_some()
                || (class == ProcessClass::Proactive && self.daily_budget_cost().is_some()));
        let (partition_spent, proactive_spent, need) = if budgeted {
            (
                self.partition_spent(&meta.partition).await,
                if class == ProcessClass::Proactive {
                    self.proactive_spent(&meta.partition).await
                } else {
                    0
                },
                self.item_need(meta).await,
            )
        } else {
            (0, 0, 0)
        };
        let decision = admit(&AdmissionInput {
            class,
            approval_woken: matches!(
                run.checkpoint,
                Some(RunCheckpointRef::AwaitingApproval { .. })
            ),
            now: self.clock.now(),
            quiet_hours: &quiet,
            quotas: &self.quotas,
            running,
            preemptible: !background.is_empty(),
            partition_spent,
            proactive_spent,
            daily_budget: self.daily_budget_cost(),
            need,
        });
        let wake = match decision {
            Admission::Admit => {
                if meta.labels.remove(ADMISSION_LABEL).is_some() {
                    self.put_meta(meta.clone()).await;
                }
                return decision;
            }
            Admission::QuietHours { until } => Wake::At(until),
            Admission::Budget => Wake::BudgetReplenished { partition: meta.partition.clone() },
            Admission::Quota { preempt } => {
                if preempt
                    && let Some(victim) =
                        youngest_background(background.iter().map(|(pid, at)| (pid.as_str(), *at)))
                {
                    // The victim makes no further model call, so its slot is this run's now,
                    // while it winds down to its checkpoint.
                    self.preempt(victim, &run.run_id).await;
                    if meta.labels.remove(ADMISSION_LABEL).is_some() {
                        self.put_meta(meta.clone()).await;
                    }
                    return Admission::Admit;
                }
                Wake::Manual
            }
        };
        meta.wake = Some(WakeSet::any_of([wake]));
        if let Some(reason) = decision.reason() {
            meta.labels.insert(ADMISSION_LABEL.to_string(), reason.to_string());
        }
        self.put_meta(meta.clone()).await;
        self.suspend_local(&run.run_id);
        tracing::info!(process.pid = %run.run_id, admission = ?decision, "process deferred by admission");
        decision
    }

    /// Suspends `victim` at its next checkpoint for `by`, an interactive process (spec 3.2
    /// §7.9): `Waiting` with [`Suspension::Preempted`] and a `RunFinished` wake on `by`, so it
    /// is requeued when `by` finishes.
    async fn preempt(&self, victim: &str, by: &str) {
        let Ok(Some(run)) = self.runs.get(victim).await else { return };
        let mut meta = self.stored_meta(&run).await.unwrap_or_else(|| Self::infer_meta(&run));
        meta.suspended_by = Some(Suspension::Preempted { by: by.to_string() });
        meta.wake = Some(WakeSet::any_of([Wake::RunFinished { pid: by.to_string() }]));
        meta.suspend_requested = true;
        self.put_meta(meta).await;
        // Another replica's worker leases it otherwise; its mirror reads the request.
        self.suspend_local(victim);
        tracing::info!(process.pid = victim, preempted.by = by, "background process preempted");
    }

    /// Requeues the processes `by` preempted, now that it finished (spec 3.2 §7.9).
    pub(crate) async fn requeue_preempted(&self, by: Option<&str>) {
        let metas = match self.processes.list(&ProcessFilter::default()).await {
            Ok(metas) => metas,
            Err(error) => {
                tracing::debug!(error = %error, "could not list preempted processes");
                return;
            }
        };
        let mut woken = 0;
        for mut meta in metas {
            let Some(Suspension::Preempted { by: preemptor }) = &meta.suspended_by else {
                continue;
            };
            let finished = match by {
                Some(by) => preemptor == by,
                None => match self.runs.get(preemptor).await {
                    Ok(Some(run)) => run.state.is_terminal(),
                    Ok(None) => true,
                    Err(_) => false,
                },
            };
            if !finished {
                continue;
            }
            let Ok(Some(run)) = self.runs.get(&meta.pid).await else { continue };
            if run.state != RunState::Waiting {
                continue;
            }
            let preemptor = preemptor.clone();
            meta.suspended_by = None;
            meta.wake = None;
            meta.suspend_requested = false;
            self.put_meta(meta.clone()).await;
            match self.runs.wake(&meta.pid).await {
                Ok(()) => {
                    woken += 1;
                    self.record(
                        audit::PROCESS_REQUEUED,
                        self.actor_for(Some(&run)),
                        Self::process_fields(&meta, Some("preemptor finished"), Some(&preemptor)),
                    )
                    .await;
                }
                Err(error) => {
                    tracing::warn!(process.pid = %meta.pid, error = %error, "could not requeue a preempted process")
                }
            }
        }
        if woken > 0 {
            self.nudge();
        }
    }

    /// Wakes the processes admission holds for a slot, while their partition has one: a
    /// human turn first, then oldest first (spec 3.2 §7.9).
    pub(crate) async fn admit_waiters(&self) {
        let metas = match self.processes.list(&ProcessFilter::default()).await {
            Ok(metas) => metas,
            Err(error) => {
                tracing::debug!(error = %error, "could not list processes waiting for a slot");
                return;
            }
        };
        let mut waiting: BTreeMap<PartitionId, Vec<(ProcessMeta, RunRecord)>> = BTreeMap::new();
        for meta in metas {
            if meta.labels.get(ADMISSION_LABEL).map(String::as_str) != Some("quota")
                || meta.suspended_by.is_some()
            {
                continue;
            }
            let Ok(Some(run)) = self.runs.get(&meta.pid).await else { continue };
            if run.state == RunState::Waiting {
                waiting.entry(meta.partition.clone()).or_default().push((meta, run));
            }
        }
        let mut woken = 0;
        for (partition, mut waiters) in waiting {
            let _gate = self.admission.lock().await;
            let (running, _) = self.admitted(&partition, "").await;
            let free =
                self.quotas.max_running.map_or(usize::MAX, |max| max.saturating_sub(running));
            waiters.sort_by(|(a, x), (b, y)| {
                let human = |meta: &ProcessMeta, run: &RunRecord| {
                    class_of(run, meta) != ProcessClass::Interactive
                };
                human(a, x)
                    .cmp(&human(b, y))
                    .then(x.created_at.cmp(&y.created_at))
                    .then(x.run_id.cmp(&y.run_id))
            });
            for (mut meta, run) in waiters.into_iter().take(free) {
                meta.wake = None;
                meta.labels.remove(ADMISSION_LABEL);
                self.put_meta(meta.clone()).await;
                match self.runs.wake(&run.run_id).await {
                    Ok(()) => woken += 1,
                    Err(error) => {
                        tracing::warn!(process.pid = %run.run_id, error = %error, "could not wake a process waiting for a slot")
                    }
                }
            }
        }
        if woken > 0 {
            self.nudge();
        }
    }

    /// The partitions with processes asleep on `BudgetReplenished` whose budget has room
    /// again (spec 3.2 §7.9): a new UTC day, or a cap raised.
    pub(crate) async fn replenished_partitions(&self) -> Vec<PartitionId> {
        let metas = match self.processes.list(&ProcessFilter::default()).await {
            Ok(metas) => metas,
            Err(_) => return Vec::new(),
        };
        let mut partitions: BTreeMap<PartitionId, (bool, u64)> = BTreeMap::new();
        for meta in &metas {
            let Some(wake) = &meta.wake else { continue };
            for wake in &wake.any_of {
                if let Wake::BudgetReplenished { partition } = wake {
                    let proactive =
                        matches!(meta.kind, ProcessKind::Proposed | ProcessKind::Maintenance);
                    let entry = partitions.entry(partition.clone()).or_insert((false, u64::MAX));
                    entry.0 |= proactive;
                    entry.1 = entry.1.min(self.item_need(meta).await);
                }
            }
        }
        let mut replenished = Vec::new();
        for (partition, (proactive, need)) in partitions {
            // Without a cap there is nothing to replenish: such a sleeper waits on its other
            // wakes.
            let budget = if proactive { self.daily_budget_cost() } else { None };
            if self.quotas.daily_spend_cap.is_none() && budget.is_none() {
                continue;
            }
            let fits = |cap: Option<u64>, spent: u64| {
                cap.is_none_or(|cap| spent.saturating_add(need.max(1)) <= cap)
            };
            let mut room =
                fits(self.quotas.daily_spend_cap, self.partition_spent(&partition).await);
            if budget.is_some() {
                room &= fits(budget, self.proactive_spent(&partition).await);
            }
            if room {
                replenished.push(partition);
            }
        }
        replenished
    }

    /// Moves the stored `item` from a state of `from`'s kind to `item.state`, then publishes
    /// the change and writes its audit record; `false` when another writer moved it first
    /// (spec 3.2 §7.5, §9.3).
    pub(crate) async fn move_item(
        &self,
        item: &AgendaItem,
        from: &AgendaState,
        reason: Option<&str>,
        by: Option<&SubjectRef>,
    ) -> crate::error::Result<bool> {
        if !self.agenda.transition(item.clone(), from.kind()).await? {
            return Ok(false);
        }
        self.agenda_changed(item, from, reason, by).await;
        Ok(true)
    }

    /// Publishes an agenda change and writes the audit record its new state has.
    pub(crate) async fn agenda_changed(
        &self,
        item: &AgendaItem,
        from: &AgendaState,
        reason: Option<&str>,
        by: Option<&SubjectRef>,
    ) {
        self.publish(KernelEvent::Agenda {
            item: item.id.clone(),
            from: from.clone(),
            to: item.state.clone(),
            reason: reason.map(str::to_string),
        });
        let action = match (&item.state, from) {
            (AgendaState::Proposed, AgendaState::Proposed) => Some(audit::AGENDA_PROPOSED),
            (AgendaState::Approved, _) => Some(audit::AGENDA_APPROVED),
            (AgendaState::Rejected, _) => Some(audit::AGENDA_REJECTED),
            (AgendaState::Scheduled { .. }, _) => Some(audit::AGENDA_SCHEDULED),
            (AgendaState::Expired, _) => Some(audit::AGENDA_EXPIRED),
            _ => None,
        };
        if let Some(action) = action {
            let mut actor = match by {
                Some(subject) => AuditActor::subject(subject.clone()),
                None => AuditActor::default(),
            };
            actor.instance = Some(self.kernel_id.clone());
            let budget = item.budget.as_ref().map(|budget| {
                json!({
                    "maxModelCalls": budget.max_model_calls,
                    "maxTotalTokens": budget.max_total_tokens,
                    "maxCostMicroUsd": budget.max_cost_micro_usd,
                })
            });
            self.record(
                action,
                actor,
                json!({
                    "item": item.id,
                    "partition": item.partition,
                    "origin": item.origin.to_string(),
                    "by": by.map(|subject| subject.id.clone()).unwrap_or_else(|| "kernel".to_string()),
                    "budget": budget,
                    "reason": reason,
                }),
            )
            .await;
        }
    }

    /// Marks the agenda item `meta` runs `Running` once its process is admitted.
    pub(crate) async fn item_started(&self, meta: &ProcessMeta) {
        let Some(id) = &meta.agenda_item else { return };
        let Ok(Some(item)) = self.agenda.get(id).await else { return };
        if !matches!(item.state, AgendaState::Scheduled { .. }) {
            return;
        }
        let from = item.state.clone();
        let next = item.with_state(AgendaState::Running { pid: meta.pid.clone() });
        if let Err(error) = self.move_item(&next, &from, Some("admitted"), None).await {
            tracing::warn!(agenda.item = %id, error = %error, "could not mark an agenda item running");
        }
    }

    /// Marks the agenda item `meta` ran `Done` once its process finished.
    pub(crate) async fn item_finished(&self, meta: &ProcessMeta, state: RunState) {
        let Some(id) = &meta.agenda_item else { return };
        let Ok(Some(item)) = self.agenda.get(id).await else { return };
        if !matches!(item.state, AgendaState::Scheduled { .. } | AgendaState::Running { .. }) {
            return;
        }
        let from = item.state.clone();
        let state = serde_json::to_value(state)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        let reason = format!("process {} {state}", meta.pid);
        let next = item.with_state(AgendaState::Done);
        if let Err(error) = self.move_item(&next, &from, Some(&reason), None).await {
            tracing::warn!(agenda.item = %id, error = %error, "could not mark an agenda item done");
        }
    }
}

impl Supervision {
    /// The daily review (spec 3.2 §7.4, §9.1): once per UTC day, for every partition, the
    /// standing rules without an expiry, the sleepers and the freeze-parked processes older
    /// than the review period, published and audited when there are any. Changes nothing.
    pub(crate) async fn review(&self) {
        let now = self.clock.now();
        let today = now.date_naive();
        {
            let mut reviewed = self.reviewed.lock().unwrap_or_else(|e| e.into_inner());
            if *reviewed == Some(today) {
                return;
            }
            *reviewed = Some(today);
        }
        let period = self.proactivity.review_period;
        let Some(cutoff) =
            chrono::Duration::from_std(period).ok().and_then(|age| now.checked_sub_signed(age))
        else {
            return;
        };
        let age = |since: DateTime<Utc>| (now - since).to_std().unwrap_or_default();
        let metas = match self.processes.list(&ProcessFilter::default()).await {
            Ok(metas) => metas,
            Err(error) => {
                tracing::warn!(error = %error, "could not list processes for the review");
                return;
            }
        };
        let empty = |partition: &PartitionId| crate::review::ReviewReport {
            partition: partition.clone(),
            review_period: period,
            at: now,
            rules: Vec::new(),
            sleepers: Vec::new(),
            frozen: Vec::new(),
        };
        let mut reports: BTreeMap<PartitionId, crate::review::ReviewReport> =
            std::iter::once(PartitionId::default())
                .chain(metas.iter().map(|meta| meta.partition.clone()))
                .map(|partition| (partition.clone(), empty(&partition)))
                .collect();
        for meta in &metas {
            let Ok(Some(run)) = self.runs.get(&meta.pid).await else { continue };
            if run.state != RunState::Waiting || is_wake_run(&run) || run.updated_at > cutoff {
                continue;
            }
            let aged =
                crate::review::AgedProcess { pid: meta.pid.clone(), age: age(run.updated_at) };
            let Some(entry) = reports.get_mut(&meta.partition) else { continue };
            if meta.suspended_by == Some(Suspension::Freeze) {
                entry.frozen.push(aged);
            } else {
                entry.sleepers.push(aged);
            }
        }
        // Standing rules carry no partition until partitions land (phase 3).
        if let Some(source) = &self.rule_source {
            let users: std::collections::BTreeSet<String> =
                match self.runs.list(RunFilter::default().app_name(self.app_name.clone())).await {
                    Ok(runs) => runs.into_iter().map(|run| run.user_id).collect(),
                    Err(error) => {
                        tracing::warn!(error = %error, "could not list the users for the review");
                        Default::default()
                    }
                };
            let Some(entry) = reports.get_mut(&PartitionId::default()) else { return };
            for user in users {
                match source.rules(&self.app_name, &user).await {
                    Ok(rules) => entry.rules.extend(
                        rules
                            .into_iter()
                            .filter(|rule| rule.expires_at.is_none() && rule.created_at <= cutoff)
                            .map(|rule| crate::review::StaleRule {
                                id: rule.id,
                                user: rule.user,
                                age: age(rule.created_at),
                            }),
                    ),
                    Err(error) => {
                        tracing::warn!(user = %user, error = %error, "could not read standing rules for the review")
                    }
                }
            }
        }
        for report in reports.into_values().filter(|report| !report.is_empty()) {
            tracing::info!(
                partition = %report.partition,
                review.rules = report.rules.len(),
                review.sleepers = report.sleepers.len(),
                review.frozen = report.frozen.len(),
                "review found work older than the review period"
            );
            let mut fields = serde_json::to_value(&report).unwrap_or_default();
            if let Value::Object(map) = &mut fields {
                map.remove("at");
                map.insert("ask".into(), json!(report.renewal_ask()));
            }
            self.record(audit::REVIEW, self.actor_for(None), fields).await;
            self.publish(KernelEvent::Review(report));
        }
    }
}

impl Supervision {
    /// Fires the cron wakes of sleepers on the kernel's clock (spec 3.2 §7.4): for each
    /// expression a sleeper's wake set names and no standing trigger uses (a trigger's cron
    /// expression has its registered `CronWakeSource`), the latest occurrence due since the
    /// last one fired is submitted as a `kernel.wake` event keyed as a cron source keys it,
    /// so replicas fire it once. An expression no sleeper names any more is forgotten.
    pub(crate) async fn fire_crons(&self, router: &crate::wake::WakeRouter) {
        let now = self.clock.now();
        let triggered: std::collections::BTreeSet<&str> = router
            .triggers()
            .iter()
            .filter_map(|trigger| match &trigger.wake {
                Wake::Cron { expr, .. } => Some(expr.as_str()),
                _ => None,
            })
            .collect();
        let needed: std::collections::BTreeSet<String> =
            match self.processes.list(&ProcessFilter::default()).await {
                Ok(metas) => metas
                    .iter()
                    .filter_map(|meta| meta.wake.as_ref())
                    .flat_map(|wake| wake.any_of.iter())
                    .filter_map(|wake| match wake {
                        Wake::Cron { expr, .. } if !triggered.contains(expr.as_str()) => {
                            Some(expr.clone())
                        }
                        _ => None,
                    })
                    .collect(),
                Err(error) => {
                    tracing::debug!(error = %error, "could not list the sleepers' cron wakes");
                    return;
                }
            };
        let due: Vec<(String, DateTime<Utc>)> = {
            let mut crons = self.crons.lock().unwrap_or_else(|e| e.into_inner());
            crons.retain(|expr, _| needed.contains(expr));
            let mut due = Vec::new();
            for expr in &needed {
                let since = *crons.entry(expr.clone()).or_insert(now);
                let spec = adk_core::schedule::ScheduleSpec::Cron {
                    expr: expr.clone(),
                    timezone: "UTC".into(),
                };
                let mut latest = None;
                let mut cursor = since;
                while let Ok(Some(next)) = spec.next_after(cursor) {
                    if next > now {
                        break;
                    }
                    latest = Some(next);
                    cursor = next;
                }
                if let Some(at) = latest {
                    crons.insert(expr.clone(), at);
                    due.push((expr.clone(), at));
                }
            }
            due
        };
        for (expr, at) in due {
            let source = format!("kernel:{expr}");
            let event = crate::wake::WakeEvent::new(
                source.clone(),
                crate::wake::keys::cron(&source, at),
                json!({ "expression": expr, "scheduled_for": at, "catch_up": false }),
            )
            .with_at(at);
            if let Err(error) = router.submit(&event, &self.app_name).await {
                tracing::warn!(cron = %expr, error = %error, "could not submit a cron wake");
            }
        }
    }

    /// The cron expressions this kernel fires itself.
    #[cfg(test)]
    pub(crate) fn watched_crons(&self) -> Vec<String> {
        self.crons.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
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

impl KernelHooks {
    /// Spawns the successor of the finished cron sleeper `run`, asleep on `wake`, keyed by
    /// the finished run so a repeated finish spawns it once.
    async fn rearm(&self, run: &RunRecord, meta: &ProcessMeta, wake: WakeSet) {
        let mut next = adk_core::NewRun::new(
            run.queue.clone(),
            run.app_name.clone(),
            run.user_id.clone(),
            run.target.clone(),
            run.input.clone(),
        )
        .with_max_attempts(run.max_attempts);
        next.session_id = run.session_id.clone();
        next.principal = run.principal.clone();
        next.authenticated = run.authenticated;
        next.idempotency_key = Some(format!("recur:{}", run.run_id));
        let mut spec = crate::process::ProcessSpec::new(meta.kind.clone())
            .with_parent(run.run_id.clone())
            .with_partition(meta.partition.clone())
            .with_wake(wake);
        for (key, value) in &meta.labels {
            spec = spec.with_label(key.clone(), value.clone());
        }
        let table = crate::process::ProcessTable::supervised(Arc::clone(&self.sup), None);
        match table.spawn(next, spec).await {
            Ok(successor) => {
                tracing::info!(process.pid = %run.run_id, successor.pid = %successor.meta.pid, "cron sleeper re-armed")
            }
            Err(error) => {
                tracing::warn!(process.pid = %run.run_id, error = %error, "could not re-arm a cron sleeper")
            }
        }
    }
}

#[async_trait]
impl WorkerHooks for KernelHooks {
    async fn on_claim(&self, run: &RunRecord) {
        let sup = &self.sup;
        let mut stored = sup.stored_meta(run).await;
        if stored.is_none() && writes_own_row(run, &sup.triggers) && !asleep_at_claim(run, None) {
            // `spawn` or the trigger's router writes the row right after the enqueue this
            // claim may have overtaken; admission needs the row's kind and partition, so wait
            // for it briefly.
            for _ in 0..SPAWN_ROW_POLLS {
                tokio::time::sleep(SPAWN_ROW_POLL).await;
                stored = sup.stored_meta(run).await;
                if stored.is_some() {
                    break;
                }
            }
        }
        let persisted = stored.is_some();
        let asleep = asleep_at_claim(run, stored.as_ref());
        let mut meta = stored.unwrap_or_else(|| Supervision::infer_meta(run));
        let from = if meta.wake.is_some() || meta.suspended_by.is_some() || run.attempt > 1 {
            RunState::Waiting
        } else {
            RunState::Queued
        };
        // A run `spawn` enqueued or a trigger fired gets its row from the kernel, which may
        // still be writing it; an inferred row written here could replace it.
        let mut changed = !persisted && !writes_own_row(run, &sup.triggers);
        let mut admitted = false;
        if asleep || meta.suspend_requested || sup.draining.load(Ordering::SeqCst) {
            // A sleeper, a suspension asked while it queued, or a drain: the attempt parks
            // at its first model-call boundary.
            sup.suspend_local(&run.run_id);
        } else {
            if persisted {
                // Woken, or its suspension lifted: the wake set is already consumed.
                changed |= meta.suspended_by.take().is_some();
            }
            if changed {
                sup.put_meta(meta.clone()).await;
                changed = false;
            }
            // Admission (spec 3.2 §7.9) writes the row itself when it defers the process.
            admitted = sup.admit_claim(run, &mut meta).await == Admission::Admit;
        }
        if changed {
            sup.put_meta(meta.clone()).await;
        }
        if admitted {
            sup.item_started(&meta).await;
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
        if stored.is_none() && writes_own_row(run, &sup.triggers) {
            // The kernel has not written the row yet; the row it writes is the run's, and
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
        // A slot may have freed in the partition, and a preempted process whose preemptor
        // already finished goes back to its queue (spec 3.2 §7.9).
        if sup.quotas.max_running.is_some() {
            sup.admit_waiters().await;
        }
        if matches!(meta.suspended_by, Some(Suspension::Preempted { .. })) {
            sup.requeue_preempted(None).await;
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
        if run.state.is_terminal() && run.target != KERNEL_WAKE_TARGET {
            // The processes this one preempted go back to their queue, and a slot freed
            // (spec 3.2 §7.9); the item this one ran is done (§7.5).
            sup.requeue_preempted(Some(&run.run_id)).await;
            if sup.quotas.max_running.is_some() {
                sup.admit_waiters().await;
            }
            if let Some(meta) = sup.stored_meta(run).await {
                sup.item_finished(&meta, run.state).await;
                // A cron sleeper recurs: its successor sleeps on the same wake set. A kill
                // ends the series.
                if run.state != RunState::Cancelled
                    && let Some(wake) = meta.labels.get(RECUR_LABEL)
                    && let Ok(wake) = serde_json::from_str::<WakeSet>(wake)
                {
                    self.rearm(run, &meta, wake).await;
                }
            }
        }
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
    fn the_kernel_writes_the_rows_of_runs_it_spawned_or_a_trigger_fired() {
        let triggers = BTreeSet::from(["triage".to_string()]);
        let writes = |target, key| writes_own_row(&run("background", target, key), &triggers);
        assert!(writes("adk-rust", Some("kernel:spawn:7f3a")));
        assert!(writes("adk-rust", Some("kernel:asleep:7f3a")));
        assert!(writes("adk-rust", Some("hook:github:msg_1#triage")));
        // Another trigger's name, a wake run, a submitted key or none: a hook writes the row.
        assert!(!writes("adk-rust", Some("hook:github:msg_1#nightly")));
        assert!(!writes(KERNEL_WAKE_TARGET, Some("file:/w/a#triage")));
        assert!(!writes("adk-rust", Some("hook:github:msg_1")));
        assert!(!writes("adk-rust", None));
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
