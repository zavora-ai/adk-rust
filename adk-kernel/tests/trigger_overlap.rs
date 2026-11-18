//! A standing cron trigger on a running kernel, over a scripted model and a manual clock
//! (spec 3.2 §7.4, §7.9):
//!
//! - under the default overlap, `skip`, a run that holds a call for a person keeps later
//!   occurrences and a run now from queueing behind it, and `ps` counts what it skipped;
//! - `queueOne` queues one run behind it and coalesces the rest; `queue` keeps every one;
//! - `MissedFirePolicy::FireOnce` fires one run for the occurrences a freeze held, whatever
//!   the overlap.

use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::audit_log::AuditActor;
use adk_core::{
    ApprovalService, DeclarativePolicy, DurableRunStore, GovernanceControl, InMemoryActionLedger,
    InMemoryApprovalStore, InMemoryRunStore, Llm, ManualClock, MissedFirePolicy, NewRun,
    RunCheckpointRef, RunConfig, RunFilter, RunRecord, RunState, StoreApprovalService, SubjectRef,
    Tool, ToolContext, ToolEffect,
};
use adk_kernel::{
    COALESCED_LABEL, EventSubscriber, InMemoryProcessStore, KERNEL_WAKE_TARGET, Kernel,
    KernelConfig, KernelError, KernelEvent, ProcessFilter, ProcessKind, ProcessStore, RunNowTarget,
    SKIPPED_LABEL, Trigger, Wake, WakeEvent, WakeOutcome, WakeOverlap, keys,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};

const APP: &str = "ops";
/// New Year at midnight UTC: no real tick comes due while a test runs.
const NEW_YEAR: &str = "0 0 0 1 1 *";
const TRIGGER: &str = "new-year";

/// Publishing needs a person's approval.
struct Publish;

#[async_trait]
impl Tool for Publish {
    fn name(&self) -> &str {
        "publish_status"
    }

    fn description(&self) -> &str {
        "publishes STATUS.md"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "published": true }))
    }
}

/// A started kernel whose one trigger fires on [`NEW_YEAR`] with `FireOnce`.
struct Ops {
    kernel: Kernel,
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<dyn ProcessStore>,
    clock: ManualClock,
}

impl Ops {
    /// `publishes`: each run calls `publish_status` and waits for a person; otherwise each run
    /// answers and completes.
    async fn start(trigger: impl FnOnce(Trigger) -> Trigger, publishes: bool) -> Self {
        let clock = ManualClock::new(at(2026, 12, 31, 12));
        let runs: Arc<dyn DurableRunStore> =
            Arc::new(InMemoryRunStore::new().with_clock(Arc::new(clock.clone())));
        let processes: Arc<dyn ProcessStore> = Arc::new(InMemoryProcessStore::new());
        let approvals: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
                .with_run_store(runs.clone()),
        );
        let script: Vec<ScriptedResponse> = if publishes {
            (0..8)
                .flat_map(|_| {
                    [
                        ScriptedResponse::function_call("publish_status", json!({})),
                        ScriptedResponse::text("published"),
                    ]
                })
                .collect()
        } else {
            (0..8).map(|_| ScriptedResponse::text("STATUS.md is current")).collect()
        };
        let sessions = Arc::new(InMemorySessionService::new());
        let control = GovernanceControl::new();
        let agent = LlmAgentBuilder::new("ops")
            .model(Arc::new(ScriptedLlm::new("scripted", script)) as Arc<dyn Llm>)
            .tool(Arc::new(Publish))
            .build()
            .unwrap();
        let runner = Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(sessions.clone() as Arc<dyn SessionService>)
            .run_config(RunConfig::builder().approval_service(approvals.clone()).build())
            .action_ledger(Arc::new(InMemoryActionLedger::new()))
            .tool_policy(Arc::new(
                DeclarativePolicy::builder()
                    .require_approval("publish_status", "publishing needs a person")
                    .build(),
            ))
            .governance(control.clone())
            .build()
            .unwrap();
        let worker = DurableWorkerConfig::default()
            .with_lease_ttl(Duration::from_secs(30))
            .with_poll_interval(Duration::from_millis(20));
        let template = NewRun::new("background", APP, "ops", APP, json!({ "text": "refresh" }));
        let wake = Wake::Cron { expr: NEW_YEAR.into(), missed: MissedFirePolicy::FireOnce };
        let kernel = Kernel::builder()
            .config(
                KernelConfig::default()
                    .with_app_name(APP)
                    .with_worker(worker)
                    .with_scheduler_tick(Duration::from_millis(50))
                    .with_governance(control)
                    .with_clock(Arc::new(clock.clone())),
            )
            .run_store(runs.clone())
            .process_store(processes.clone())
            .session_service(sessions as Arc<dyn SessionService>)
            .approval_service(approvals)
            .agent(APP, Arc::new(runner))
            .trigger(trigger(Trigger::new(TRIGGER, wake, template)))
            .build()
            .unwrap();
        kernel.start().await.unwrap();
        Self { kernel, runs, processes, clock }
    }

    /// Hands the occurrence at `at` to the kernel as its cron source does, with the clock
    /// set just after it, and returns the event.
    async fn tick(&self, at: DateTime<Utc>) -> WakeEvent {
        self.clock.set(at + chrono::Duration::seconds(1));
        let source = source();
        let event = WakeEvent::new(
            source.clone(),
            keys::cron(&source, at),
            json!({ "expression": NEW_YEAR, "scheduled_for": at, "catch_up": false }),
        );
        self.kernel.wake_router().submit(&event, APP).await.unwrap();
        event
    }

    /// Waits until every `kernel.wake` run handed to the kernel was routed.
    async fn routed(&self) {
        eventually("every wake to be routed", || async {
            let pending = self
                .runs
                .list(RunFilter::default().states([
                    RunState::Queued,
                    RunState::Running,
                    RunState::Waiting,
                ]))
                .await
                .unwrap();
            (!pending.iter().any(|run| run.target == KERNEL_WAKE_TARGET)).then_some(())
        })
        .await;
    }

    /// The trigger's processes, oldest first, with their runs.
    async fn fired(&self) -> Vec<RunRecord> {
        let mut fired = Vec::new();
        for meta in self.processes.list(&ProcessFilter::default()).await.unwrap() {
            if meta.kind == (ProcessKind::Triggered { source: TRIGGER.into() }) {
                fired.push(self.runs.get(&meta.pid).await.unwrap().unwrap());
            }
        }
        fired.sort_by_key(|run| run.created_at);
        fired
    }

    /// The label `label` on `pid`'s row.
    async fn label(&self, pid: &str, label: &str) -> Option<String> {
        self.processes.get(pid).await.unwrap().unwrap().labels.get(label).cloned()
    }

    /// Ticks at New Year 2027, waits until its run holds a call for a person, then ticks at
    /// each New Year of `years`; returns the held run.
    async fn held_then_ticked(&self, years: std::ops::RangeInclusive<i32>) -> String {
        self.tick(new_year(2027)).await;
        let held = eventually("the first run to wait for a person", || async {
            let fired = self.fired().await;
            let first = fired.first()?;
            (first.state == RunState::Waiting
                && matches!(first.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. })))
            .then(|| first.run_id.clone())
        })
        .await;
        // Each tick is routed before the clock reaches the next; one routed after it would be
        // missed.
        for year in years {
            self.tick(new_year(year)).await;
            self.routed().await;
        }
        held
    }

    async fn stop(self) {
        self.kernel.stop().await.unwrap();
    }
}

fn source() -> String {
    format!("trigger-cron:{}", NEW_YEAR.replace(' ', "_"))
}

/// The trigger outcomes the bus published so far, by tick key.
async fn outcomes(events: &mut EventSubscriber) -> Vec<(String, WakeOutcome)> {
    let mut outcomes = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(50), events.recv()).await
    {
        if let KernelEvent::Wake { key, outcome: Some(outcome), .. } = record.event
            && !matches!(outcome, WakeOutcome::Woke { .. })
        {
            outcomes.push((key, outcome));
        }
    }
    outcomes
}

fn operator() -> AuditActor {
    AuditActor::subject(SubjectRef::human("operator"))
}

fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).unwrap()
}

fn new_year(year: i32) -> DateTime<Utc> {
    at(year, 1, 1, 0)
}

async fn eventually<T, Fut: std::future::Future<Output = Option<T>>>(
    what: &str,
    mut probe: impl FnMut() -> Fut,
) -> T {
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(10) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

/// The live autonomy check's first defect: a triggered run parked on `publish_status`, and
/// every later occurrence queued another run behind it, without bound. Fails on
/// `origin/main`, where the three later occurrences queue three runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn later_occurrences_queue_nothing_behind_a_run_waiting_for_a_person() {
    let ops = Ops::start(|trigger| trigger, true).await;
    let mut events = ops.kernel.bus().subscribe();
    let held = ops.held_then_ticked(2028..=2030).await;
    let fired = ops.fired().await;
    assert_eq!(
        fired.iter().map(|run| run.run_id.as_str()).collect::<Vec<_>>(),
        [held.as_str()],
        "nothing queues behind the held run"
    );
    let skipped = |year| {
        (
            keys::cron(&source(), new_year(year)),
            WakeOutcome::Skipped { pid: held.clone(), trigger: TRIGGER.into() },
        )
    };
    let published = outcomes(&mut events).await;
    for year in 2028..=2030 {
        assert!(published.contains(&skipped(year)), "{year} is skipped: {published:#?}");
    }
    assert_eq!(ops.label(&held, SKIPPED_LABEL).await.as_deref(), Some("3"), "`ps` counts them");

    // A run now follows the overlap: refused, naming the unfinished process.
    let refused = ops
        .kernel
        .run_now(RunNowTarget::Trigger { name: TRIGGER.into() }, None, operator())
        .await
        .unwrap_err();
    assert!(matches!(&refused, KernelError::InvalidState { pid, .. } if *pid == held), "{refused}");

    // Once the held run is gone, the next occurrence starts a run.
    ops.kernel.processes().kill(&held, "denied").await.unwrap();
    eventually("the held run to end", || async {
        let run = ops.runs.get(&held).await.unwrap()?;
        run.state.is_terminal().then_some(())
    })
    .await;
    ops.tick(new_year(2031)).await;
    ops.routed().await;
    assert_eq!(ops.fired().await.len(), 2, "the occurrence after it starts a run");
    ops.stop().await;
}

/// `queueOne`: one run waits behind the held one; later occurrences and a run now join it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_one_keeps_one_run_waiting_and_coalesces_the_rest() {
    let ops = Ops::start(|trigger| trigger.with_overlap(WakeOverlap::QueueOne), true).await;
    let mut events = ops.kernel.bus().subscribe();
    let held = ops.held_then_ticked(2028..=2030).await;
    let fired = ops.fired().await;
    assert_eq!(fired.len(), 2, "the held run and one behind it: {fired:#?}");
    let waiting = fired[1].run_id.clone();
    assert_eq!(fired[1].state, RunState::Queued);
    let coalesced = WakeOutcome::Coalesced { pid: waiting.clone(), trigger: TRIGGER.into() };
    let published = outcomes(&mut events).await;
    for year in 2029..=2030 {
        let tick = (keys::cron(&source(), new_year(year)), coalesced.clone());
        assert!(published.contains(&tick), "{year} joins the waiting run: {published:#?}");
    }
    ops.kernel
        .run_now(RunNowTarget::Trigger { name: TRIGGER.into() }, Some("again"), operator())
        .await
        .unwrap();
    ops.routed().await;
    assert_eq!(ops.fired().await.len(), 2, "a run now joins the waiting run too");
    assert_eq!(ops.label(&waiting, COALESCED_LABEL).await.as_deref(), Some("3"));
    assert_eq!(ops.label(&held, SKIPPED_LABEL).await, None);
    ops.stop().await;
}

/// `queue` keeps the behaviour of a trigger that must run once per occurrence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_queues_a_run_per_occurrence() {
    let ops = Ops::start(|trigger| trigger.with_overlap(WakeOverlap::Queue), true).await;
    ops.held_then_ticked(2028..=2030).await;
    assert_eq!(ops.fired().await.len(), 4);
    ops.stop().await;
}

/// The live autonomy check's second defect: a kernel frozen across two occurrences started
/// two runs on `unfreeze`. Fails on `origin/main`, where both held wakes start a run. The
/// trigger queues every occurrence, so its overlap cannot hide the second run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fire_once_starts_one_run_for_the_occurrences_a_freeze_held() {
    let ops = Ops::start(|trigger| trigger.with_overlap(WakeOverlap::Queue), false).await;
    let mut events = ops.kernel.bus().subscribe();
    ops.kernel.freeze("investigating", operator()).await.unwrap();
    // The cron source keeps handing occurrences to the maintenance queue while frozen.
    ops.tick(new_year(2027)).await;
    ops.tick(new_year(2028)).await;
    ops.clock.set(new_year(2028) + chrono::Duration::seconds(76));
    ops.kernel.unfreeze(operator()).await.unwrap();
    ops.routed().await;
    let fired = eventually("the run to finish", || async {
        let fired = ops.fired().await;
        let done = !fired.is_empty() && fired.iter().all(|run| run.state == RunState::Completed);
        done.then_some(fired)
    })
    .await;
    assert_eq!(fired.len(), 1, "one run for both occurrences: {fired:#?}");
    let latest = keys::cron(&source(), new_year(2028));
    assert_eq!(
        fired[0].idempotency_key.as_deref(),
        Some(keys::trigger(&latest, TRIGGER).as_str()),
        "the latest occurrence fires"
    );
    let published = outcomes(&mut events).await;
    let missed =
        (keys::cron(&source(), new_year(2027)), WakeOutcome::Missed { trigger: TRIGGER.into() });
    assert!(published.contains(&missed), "the earlier one is missed: {published:#?}");
    ops.stop().await;
}
