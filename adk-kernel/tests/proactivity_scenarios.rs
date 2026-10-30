//! The phase-2 kernel scenarios of spec 3.2 §5 and §11 on a `ScriptedLlm` with real
//! `DurableWorker`s: K4 (it proposes its own work, under control) and K8 (attention: a human
//! message preempts background work; quotas hold), plus the consolidation-origin rule
//! (K-I23). The quiet-hours and budget cases run on a `ManualClock` shared by the kernel and
//! the run store.

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalAction, ApprovalDecision, ApprovalService, Clock, DurableRunStore, GovernanceControl,
    InMemoryApprovalStore, InMemoryRunStore, Llm, ManualClock, NewRun, RunBudget, RunFilter,
    RunState, StoreApprovalService, Tool, ToolContext, ToolEffect,
};
use adk_kernel::{
    AGENDA_TOOL_NAME, AgendaItem, AgendaOrigin, AgendaState, AgendaStateKind, Kernel, KernelConfig,
    KernelError, KernelEvent, PartitionId, PartitionMode, ProactivityPolicy, ProcessFilter,
    ProcessKind, ProcessSpec, ProcessStore, ProposalOutcome, QueueClass, Quotas, SpendDelta,
    Suspension, Wake,
};
use adk_model::ScriptedResponse;
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::SessionService;
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use common::*;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

/// A tool each session's run holds in until the test releases that session.
#[derive(Default)]
struct Held {
    gates: Mutex<HashMap<String, Arc<Semaphore>>>,
    calls: AtomicUsize,
}

impl Held {
    fn gate(&self, session: &str) -> Arc<Semaphore> {
        let mut gates = self.gates.lock().unwrap();
        Arc::clone(gates.entry(session.to_string()).or_insert_with(|| Arc::new(Semaphore::new(0))))
    }

    fn release(&self, pid: &str) {
        self.gate(&format!("run-{pid}")).add_permits(1);
    }
}

struct HeldStep(Arc<Held>);

#[async_trait]
impl Tool for HeldStep {
    fn name(&self) -> &str {
        "step"
    }

    fn description(&self) -> &str {
        "does one step of the work"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        let gate = self.0.gate(ctx.session_id());
        gate.acquire().await.expect("the semaphore stays open").forget();
        Ok(json!({ "status": "done" }))
    }
}

fn runner(world: &World, control: &GovernanceControl, tool: Arc<dyn Tool>) -> Arc<Runner> {
    let agent = LlmAgentBuilder::new("agent")
        .model(Arc::clone(&world.model) as Arc<dyn Llm>)
        .tool(tool)
        .build()
        .unwrap();
    Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(Arc::clone(&world.sessions) as Arc<dyn SessionService>)
            .action_ledger(Arc::clone(&world.ledger) as Arc<dyn adk_core::ActionLedger>)
            .governance(control.clone())
            .build()
            .unwrap(),
    )
}

/// The kernel configuration of these scenarios: fast polls and ticks on `clock`.
fn config(control: &GovernanceControl, clock: Arc<dyn Clock>) -> KernelConfig {
    let worker = DurableWorkerConfig::default()
        .with_worker_id("k")
        .with_lease_ttl(Duration::from_millis(600))
        .with_poll_interval(Duration::from_millis(20));
    KernelConfig::default()
        .with_app_name(APP)
        .with_worker(worker)
        .with_scheduler_tick(Duration::from_millis(50))
        .with_governance(control.clone())
        .with_clock(clock)
        .with_control(adk_kernel::control::ControlPlaneConfig::default().without_socket())
}

fn kernel(
    world: &World,
    control: &GovernanceControl,
    config: KernelConfig,
    approvals: Option<Arc<dyn ApprovalService>>,
    tool: Arc<dyn Tool>,
) -> Kernel {
    let mut builder = Kernel::builder()
        .config(config)
        .run_store(Arc::clone(&world.runs))
        .process_store(Arc::clone(&world.processes) as Arc<dyn ProcessStore>)
        .session_service(Arc::clone(&world.sessions) as Arc<dyn SessionService>)
        .agent(TARGET, runner(world, control, tool));
    if let Some(approvals) = approvals {
        builder = builder.approval_service(approvals);
    }
    builder.build().unwrap()
}

fn inbox(runs: &Arc<dyn DurableRunStore>) -> Arc<dyn ApprovalService> {
    Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(runs)),
    )
}

/// A run nothing claims, standing in for the turn that proposed a follow-up.
async fn proposer(runs: &Arc<dyn DurableRunStore>) -> String {
    let run = NewRun::new("conversation", APP, "alice", TARGET, json!("bump the dependency"));
    match runs.enqueue(run).await.unwrap() {
        adk_core::Enqueued::Created(record) => record.run_id,
        other => panic!("{other:?}"),
    }
}

fn followup(pid: &str, title: &str) -> AgendaItem {
    AgendaItem::new(
        uuid::Uuid::new_v4().to_string(),
        SubjectRef::human("alice"),
        title,
        "the dependency bump lands tonight",
        AgendaOrigin::Agent { pid: pid.to_string(), confidence: 1.0 },
        Utc::now(),
    )
}

async fn item_in(kernel: &Kernel, id: &str, kind: AgendaStateKind) -> AgendaItem {
    eventually(&format!("item {id} to be {kind}"), || async {
        kernel.agenda().get(id).await.ok().flatten().filter(|item| item.state.kind() == kind)
    })
    .await
}

/// The process of agenda item `id`.
async fn process_of(kernel: &Kernel, id: &str) -> Option<adk_kernel::Process> {
    kernel
        .processes()
        .ps(ProcessFilter::default())
        .await
        .unwrap()
        .into_iter()
        .find(|process| process.meta.agenda_item.as_deref() == Some(id))
}

fn decide(action: ApprovalAction) -> ApprovalDecision {
    match action {
        ApprovalAction::Approve => ApprovalDecision::approve(SubjectRef::human("alice")),
        ApprovalAction::Reject => ApprovalDecision::reject(SubjectRef::human("alice"), "not now"),
        ApprovalAction::Edit => ApprovalDecision::edit(
            SubjectRef::human("alice"),
            json!({ "title": "Re-run the integration tests" }),
        ),
        ApprovalAction::Respond => unreachable!(),
    }
}

/// K4, Ask: a follow-up an agent proposes waits `Proposed` in the agenda and the inbox,
/// and nothing runs until the inbox approves it; then it runs as a `Proposed` process whose
/// parent is the proposing run. An edit is applied before it runs; a rejection runs
/// nothing; ReadOnly and Plan hold the proposal without asking.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k4_in_ask_nothing_runs_until_the_inbox_approves() {
    let world = World::new(vec![
        ScriptedResponse::text("The tests pass after the bump."),
        ScriptedResponse::text("The integration tests pass."),
    ]);
    let control = GovernanceControl::new();
    let approvals = inbox(&world.runs);
    let kernel = kernel(
        &world,
        &control,
        config(&control, Arc::new(adk_core::SystemClock)),
        Some(Arc::clone(&approvals)),
        Arc::new(HeldStep(Arc::default())),
    );
    let mut events = kernel.events();
    kernel.start().await.unwrap();
    let parent = proposer(&world.runs).await;

    let item = followup(&parent, "Re-run the tests");
    let ProposalOutcome::AwaitingApproval { item: proposed, request_id } =
        kernel.agenda().propose(item.clone(), PartitionMode::Ask).await.unwrap()
    else {
        panic!("an Ask proposal goes to the inbox");
    };
    assert_eq!(proposed.state, AgendaState::Proposed);
    let request = approvals.get(&request_id).await.unwrap().expect("in the inbox");
    assert_eq!(request.tool_name, AGENDA_TOOL_NAME);
    assert_eq!(
        request.allowed_actions,
        [ApprovalAction::Approve, ApprovalAction::Edit, ApprovalAction::Reject]
    );
    assert_eq!(request.args_redacted, serde_json::to_value(&item).unwrap());

    // Ten ticks: still proposed, no process, no model call (K-I10).
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(kernel.agenda().get(&item.id).await.unwrap().unwrap().state, AgendaState::Proposed);
    assert!(process_of(&kernel, &item.id).await.is_none());
    assert_eq!(world.model_calls(), 0);

    approvals.decide(&request_id, request.version, decide(ApprovalAction::Approve)).await.unwrap();
    item_in(&kernel, &item.id, AgendaStateKind::Done).await;
    let process = process_of(&kernel, &item.id).await.expect("the item's process");
    assert_eq!(process.meta.kind, ProcessKind::Proposed);
    assert_eq!(process.meta.parent.as_deref(), Some(parent.as_str()));
    assert_eq!(process.run.state, RunState::Completed);
    assert_eq!(process.run.session_id.as_deref(), Some(format!("agenda-{}", item.id).as_str()));
    assert_eq!(world.model_calls(), 1);

    // The bus carried every state change, in order.
    let mut seen = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(100), events.recv()).await
    {
        if let KernelEvent::Agenda { item: id, to, .. } = record.event
            && id == item.id
        {
            seen.push(to.kind());
        }
    }
    assert_eq!(
        seen,
        [
            AgendaStateKind::Proposed,
            AgendaStateKind::Approved,
            AgendaStateKind::Scheduled,
            AgendaStateKind::Running,
            AgendaStateKind::Done
        ]
    );

    // An edit in the inbox changes the item before it runs.
    let edited = followup(&parent, "Re-run the tests");
    let ProposalOutcome::AwaitingApproval { request_id, .. } =
        kernel.agenda().propose(edited.clone(), PartitionMode::Ask).await.unwrap()
    else {
        panic!("to the inbox");
    };
    let version = approvals.get(&request_id).await.unwrap().unwrap().version;
    approvals.decide(&request_id, version, decide(ApprovalAction::Edit)).await.unwrap();
    let done = item_in(&kernel, &edited.id, AgendaStateKind::Done).await;
    assert_eq!(done.title, "Re-run the integration tests");
    let process = process_of(&kernel, &edited.id).await.unwrap();
    assert!(process.run.input.as_str().unwrap().contains("Re-run the integration tests"));

    // A rejection runs nothing.
    let rejected = followup(&parent, "Delete the lockfile");
    let ProposalOutcome::AwaitingApproval { request_id, .. } =
        kernel.agenda().propose(rejected.clone(), PartitionMode::Ask).await.unwrap()
    else {
        panic!("to the inbox");
    };
    let version = approvals.get(&request_id).await.unwrap().unwrap().version;
    approvals.decide(&request_id, version, decide(ApprovalAction::Reject)).await.unwrap();
    item_in(&kernel, &rejected.id, AgendaStateKind::Rejected).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(process_of(&kernel, &rejected.id).await.is_none());

    // ReadOnly and Plan hold the proposal: no inbox request, no process.
    for mode in [PartitionMode::ReadOnly, PartitionMode::Plan] {
        let held = followup(&parent, "Tidy the changelog");
        let outcome = kernel.agenda().propose(held.clone(), mode).await.unwrap();
        assert!(matches!(outcome, ProposalOutcome::Held { .. }), "{mode}: {outcome:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(approvals.get(&format!("agenda_{}", held.id)).await.unwrap().is_none());
        assert!(process_of(&kernel, &held.id).await.is_none());
        assert_eq!(
            kernel.agenda().get(&held.id).await.unwrap().unwrap().state,
            AgendaState::Proposed
        );
    }
    assert_eq!(world.model_calls(), 2);
    kernel.stop().await.unwrap();
}

/// K4, Auto: a follow-up proposed inside quiet hours is approved by the partition's policy
/// and runs only once they end, within the daily budget; a proposal past
/// `max_proposed_open`, or once the day's proactive budget is spent, is rejected with a
/// reason on the bus; a proactive process over budget sleeps until `BudgetReplenished`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k4_in_auto_it_runs_after_quiet_hours_within_the_daily_budget() {
    let today = Utc::now().date_naive();
    let night = Utc.from_utc_datetime(&today.and_hms_opt(23, 0, 0).unwrap());
    let clock = ManualClock::new(night);
    let mut world = World::new(vec![
        ScriptedResponse::text("Re-ran the tests at seven."),
        ScriptedResponse::text("Ran after the budget replenished."),
    ]);
    world.runs = Arc::new(InMemoryRunStore::new().with_clock(Arc::new(clock.clone())));
    let control = GovernanceControl::new();
    let policy = ProactivityPolicy::default()
        .with_max_proposed_open(2)
        .with_daily_budget(RunBudget::new().max_cost_micro_usd(1_000))
        .with_quiet_hours(
            "22:00-07:00".parse::<adk_kernel::QuietHours>().unwrap().with_utc_offset_minutes(0),
        );
    let kernel = kernel(
        &world,
        &control,
        config(&control, Arc::new(clock.clone())).with_proactivity(policy),
        None,
        Arc::new(HeldStep(Arc::default())),
    );
    let mut events = kernel.events();
    kernel.start().await.unwrap();
    let parent = proposer(&world.runs).await;

    let item = followup(&parent, "Re-run the tests");
    let outcome = kernel.agenda().propose(item.clone(), PartitionMode::Auto).await.unwrap();
    let ProposalOutcome::Scheduled { pid, .. } = outcome else {
        panic!("an Auto partition schedules: {outcome:?}");
    };
    let seven =
        Utc.from_utc_datetime(&(today + chrono::Days::new(1)).and_hms_opt(7, 0, 0).unwrap());
    let run = world.runs.get(&pid).await.unwrap().unwrap();
    assert_eq!(run.not_before, Some(seven), "quiet hours through not_before");
    assert!(matches!(
        kernel.agenda().get(&item.id).await.unwrap().unwrap().state,
        AgendaState::Scheduled { .. }
    ));

    // Inside quiet hours nothing starts (K-I11).
    tokio::time::sleep(Duration::from_millis(300)).await;
    clock.advance(Duration::from_secs(7 * 60 * 60 + 59 * 60)); // 06:59
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(world.runs.get(&pid).await.unwrap().unwrap().state, RunState::Queued);
    assert_eq!(world.model_calls(), 0);
    clock.advance(Duration::from_secs(2 * 60)); // 07:01
    item_in(&kernel, &item.id, AgendaStateKind::Done).await;
    assert_eq!(world.model_calls(), 1);

    // Two held proposals fill `max_proposed_open`; a third is refused with a reason.
    for title in ["Tidy the changelog", "Update the README"] {
        let held = followup(&parent, title);
        assert!(matches!(
            kernel.agenda().propose(held, PartitionMode::Plan).await.unwrap(),
            ProposalOutcome::Held { .. }
        ));
    }
    let third = followup(&parent, "One more thing");
    let ProposalOutcome::Rejected { reason } =
        kernel.agenda().propose(third.clone(), PartitionMode::Auto).await.unwrap()
    else {
        panic!("past max_proposed_open");
    };
    assert!(reason.contains("max_proposed_open"), "{reason}");
    assert!(kernel.agenda().get(&third.id).await.unwrap().is_none(), "nothing stored");
    let mut refused = None;
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(200), events.recv()).await
    {
        if let KernelEvent::Agenda { to: AgendaState::Rejected, reason, .. } = record.event {
            refused = reason;
            break;
        }
    }
    let refused = refused.expect("the refusal on the bus");
    assert!(refused.contains("max_proposed_open"));

    // The day's proactive budget spent: a proposal is refused, and a proactive process
    // admission meets sleeps until the budget replenishes.
    let partition = PartitionId::default();
    let now = clock.now();
    world
        .processes
        .add_spend(&pid, &partition, now.date_naive(), SpendDelta::new(1_000))
        .await
        .unwrap();
    for held in kernel.agenda().list(&adk_kernel::AgendaFilter::default().open()).await.unwrap() {
        kernel.agenda().reject(&held.id, &SubjectRef::human("alice"), None).await.unwrap();
    }
    let ProposalOutcome::Rejected { reason } = kernel
        .agenda()
        .propose(followup(&parent, "Over budget"), PartitionMode::Auto)
        .await
        .unwrap()
    else {
        panic!("the budget is spent");
    };
    assert!(reason.contains("daily proactive budget"), "{reason}");
    let tick = kernel
        .processes()
        .spawn(
            run_on(QueueClass::Background, "consolidate"),
            ProcessSpec::new(ProcessKind::Maintenance),
        )
        .await
        .unwrap();
    let asleep = eventually("the over-budget process asleep", || async {
        let process = kernel.processes().get(&tick.meta.pid).await.ok().flatten()?;
        (process.run.state == RunState::Waiting && process.meta.wake.is_some()).then_some(process)
    })
    .await;
    assert_eq!(
        asleep.meta.wake.unwrap().any_of,
        [Wake::BudgetReplenished { partition: partition.clone() }]
    );
    assert_eq!(asleep.meta.labels.get("kernel.admission").map(String::as_str), Some("budget"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(world.model_calls(), 1, "nothing runs over budget");

    // The next day, outside quiet hours: the budget replenishes and the process runs.
    clock.advance(Duration::from_secs(24 * 60 * 60));
    in_state(&world.runs, &tick.meta.pid, RunState::Completed).await;
    assert_eq!(world.model_calls(), 2);
    let key = format!("budget:default:{}", clock.now().date_naive());
    let wakes = world.runs.list(RunFilter::default().queue(QueueClass::MAINTENANCE)).await.unwrap();
    assert!(wakes.iter().any(|run| run.idempotency_key.as_deref() == Some(key.as_str())), "{key}");
    kernel.stop().await.unwrap();
}

/// K-I23: an item the consolidation tick proposed waits for a person even in an Auto
/// partition; a service cannot approve it; a person's inbox approval runs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_consolidation_item_runs_only_after_a_person_approves_it_in_auto() {
    let world = World::new(vec![ScriptedResponse::text("Merged the duplicate notes.")]);
    let control = GovernanceControl::new();
    let approvals = inbox(&world.runs);
    let kernel = kernel(
        &world,
        &control,
        config(&control, Arc::new(adk_core::SystemClock)),
        Some(Arc::clone(&approvals)),
        Arc::new(HeldStep(Arc::default())),
    );
    kernel.start().await.unwrap();
    let item = AgendaItem::new(
        "consolidation-1",
        SubjectRef::human("alice"),
        "Review two contradicting facts",
        "The tick found facts that disagree",
        AgendaOrigin::Consolidation,
        Utc::now(),
    );
    let ProposalOutcome::AwaitingApproval { request_id, .. } =
        kernel.agenda().propose(item.clone(), PartitionMode::Auto).await.unwrap()
    else {
        panic!("a consolidation item goes to the inbox in Auto (K-I23)");
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(process_of(&kernel, &item.id).await.is_none());
    let error =
        kernel.agenda().approve(&item.id, &SubjectRef::service("scheduler")).await.unwrap_err();
    assert!(matches!(error, KernelError::Unauthorized { .. }), "{error}");
    assert_eq!(kernel.agenda().get(&item.id).await.unwrap().unwrap().state, AgendaState::Proposed);

    let version = approvals.get(&request_id).await.unwrap().unwrap().version;
    approvals.decide(&request_id, version, decide(ApprovalAction::Approve)).await.unwrap();
    item_in(&kernel, &item.id, AgendaStateKind::Done).await;
    assert_eq!(world.model_calls(), 1);
    kernel.stop().await.unwrap();
}

/// The admitted processes of the default partition running now: `Running`, not a
/// `kernel.wake` run, claimed, and not on their way to park. A run is `Running` from its claim,
/// before admission preempts anything, so only a run whose claim the kernel published — it
/// publishes after admission wrote the preemption it decided — counts; `claimed` is read
/// before the stores, so they already show the preempted process parking.
async fn admitted(world: &World, claimed: &HashSet<String>) -> usize {
    let mut count = 0;
    for run in world.runs.list(RunFilter::default().states([RunState::Running])).await.unwrap() {
        if run.target == adk_kernel::KERNEL_WAKE_TARGET || !claimed.contains(&run.run_id) {
            continue;
        }
        let meta = world.processes.get(&run.run_id).await.unwrap();
        let parking = meta.as_ref().is_some_and(|meta| {
            meta.wake.is_some() || meta.suspend_requested || meta.suspended_by.is_some()
        });
        if !parking {
            count += 1;
        }
    }
    count
}

/// K8: three background runs at the partition's quota of three; a typed message's run
/// starts at once, the youngest background run parks at its next checkpoint and is
/// requeued when the message's run finishes, no tool runs twice, and the quota is never
/// exceeded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k8_a_human_message_preempts_background_work_and_quotas_hold() {
    let mut script: Vec<ScriptedResponse> =
        (0..3).map(|_| ScriptedResponse::function_call("step", json!({}))).collect();
    script.extend(
        [
            "Hello, here is the answer.",
            "Background one done.",
            "Background two done.",
            "Background three done.",
        ]
        .map(ScriptedResponse::text),
    );
    let world = World::new(script);
    let control = GovernanceControl::new();
    let held = Arc::new(Held::default());
    let kernel = kernel(
        &world,
        &control,
        config(&control, Arc::new(adk_core::SystemClock))
            .with_quotas(Quotas::new().with_max_running(3)),
        None,
        Arc::new(HeldStep(Arc::clone(&held))),
    );
    kernel.start().await.unwrap();

    // Samples the admitted processes every 5 ms for the whole scenario.
    let peak = Arc::new(AtomicUsize::new(0));
    let sampling = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let claimed = Arc::new(Mutex::new(HashSet::<String>::new()));
    let listener = {
        let (mut events, claimed) = (kernel.events(), Arc::clone(&claimed));
        tokio::spawn(async move {
            while let Some(record) = events.recv().await {
                if let KernelEvent::Process { pid, to, .. } = record.event {
                    let mut claimed = claimed.lock().unwrap_or_else(|e| e.into_inner());
                    if to == RunState::Running {
                        claimed.insert(pid);
                    } else {
                        claimed.remove(&pid);
                    }
                }
            }
        })
    };
    let sampler = {
        let (runs, processes) = (Arc::clone(&world.runs), Arc::clone(&world.processes));
        let (peak, sampling, claimed) =
            (Arc::clone(&peak), Arc::clone(&sampling), Arc::clone(&claimed));
        tokio::spawn(async move {
            let probe = World { runs, processes, ..World::answer() };
            while sampling.load(Ordering::SeqCst) {
                let claimed = claimed.lock().unwrap_or_else(|e| e.into_inner()).clone();
                peak.fetch_max(admitted(&probe, &claimed).await, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };

    let mut background = Vec::new();
    for n in 0..3 {
        let process = kernel
            .processes()
            .spawn(
                run_on(QueueClass::Background, &format!("background {n}")),
                ProcessSpec::new(ProcessKind::Goal),
            )
            .await
            .unwrap();
        in_state(&world.runs, &process.meta.pid, RunState::Running).await;
        background.push(process.meta.pid);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    eventually("three tool calls in flight", || async {
        (held.calls.load(Ordering::SeqCst) == 3).then_some(())
    })
    .await;
    let youngest = background[2].clone();

    // The message.
    let typed = tokio::time::Instant::now();
    let message = kernel
        .processes()
        .spawn(
            run_on(QueueClass::Interactive, "a question"),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await
        .unwrap();
    in_state(&world.runs, &message.meta.pid, RunState::Completed).await;
    assert!(
        typed.elapsed() < Duration::from_secs(1),
        "the message ran within one tick: {:?}",
        typed.elapsed()
    );
    let preempted = world.processes.get(&youngest).await.unwrap().unwrap();
    assert_eq!(
        preempted.suspended_by,
        Some(Suspension::Preempted { by: message.meta.pid.clone() })
    );

    // The youngest parks at its next checkpoint and is requeued; the others finish.
    held.release(&youngest);
    in_state(&world.runs, &youngest, RunState::Completed).await;
    for pid in &background[..2] {
        held.release(pid);
        in_state(&world.runs, pid, RunState::Completed).await;
    }
    sampling.store(false, Ordering::SeqCst);
    sampler.await.unwrap();
    listener.abort();

    assert!(
        peak.load(Ordering::SeqCst) <= 3,
        "the quota was exceeded: {}",
        peak.load(Ordering::SeqCst)
    );
    assert_eq!(held.calls.load(Ordering::SeqCst), 3, "no tool ran twice");
    assert_eq!(world.model_calls(), 7);
    let youngest = kernel.processes().get(&youngest).await.unwrap().unwrap();
    assert_eq!(youngest.meta.suspended_by, None);
    let requests = world.model.requests();
    assert!(requests.len() == 7);
    kernel.stop().await.unwrap();
}

/// Priority among waiters (spec 3.2 §7.9): at a quota of one held by a human turn, a
/// background run and then a second human turn wait for the slot; when it frees, the human
/// turn is admitted first, though the background run waited longer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_waiting_human_turn_takes_a_freed_slot_before_older_background_work() {
    let world = World::new(vec![
        ScriptedResponse::function_call("step", json!({})),
        ScriptedResponse::text("First turn done."),
        ScriptedResponse::text("Second turn done."),
        ScriptedResponse::text("Background done."),
    ]);
    let control = GovernanceControl::new();
    let held = Arc::new(Held::default());
    let kernel = kernel(
        &world,
        &control,
        config(&control, Arc::new(adk_core::SystemClock))
            .with_quotas(Quotas::new().with_max_running(1)),
        None,
        Arc::new(HeldStep(Arc::clone(&held))),
    );
    let mut events = kernel.events();
    kernel.start().await.unwrap();
    let spawn = |queue: QueueClass, kind: ProcessKind, text: &'static str| {
        let kernel = kernel.clone();
        async move {
            kernel.processes().spawn(run_on(queue, text), ProcessSpec::new(kind)).await.unwrap()
        }
    };
    let first = spawn(QueueClass::Interactive, ProcessKind::Interactive, "first").await;
    eventually("the first turn in its tool", || async {
        (held.calls.load(Ordering::SeqCst) == 1).then_some(())
    })
    .await;
    let waiting = |pid: String| {
        let processes = Arc::clone(&world.processes);
        let runs = Arc::clone(&world.runs);
        async move {
            eventually(&format!("{pid} waiting for a slot"), || async {
                let meta = processes.get(&pid).await.ok().flatten()?;
                let run = runs.get(&pid).await.ok().flatten()?;
                (run.state == RunState::Waiting
                    && meta.labels.get("kernel.admission").map(String::as_str) == Some("quota"))
                .then_some(())
            })
            .await;
        }
    };
    let background = spawn(QueueClass::Background, ProcessKind::Goal, "background").await;
    waiting(background.meta.pid.clone()).await;
    let second = spawn(QueueClass::Interactive, ProcessKind::Interactive, "second").await;
    waiting(second.meta.pid.clone()).await;

    held.release(&first.meta.pid);
    in_state(&world.runs, &background.meta.pid, RunState::Completed).await;
    in_state(&world.runs, &second.meta.pid, RunState::Completed).await;
    let mut claims = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(100), events.recv()).await
    {
        if let KernelEvent::Process { pid, to: RunState::Running, .. } = record.event {
            claims.push(pid);
        }
    }
    let last = |pid: &str| claims.iter().rposition(|claimed| claimed == pid).unwrap();
    assert!(
        last(&second.meta.pid) < last(&background.meta.pid),
        "the human turn was admitted first: {claims:?}"
    );
    kernel.stop().await.unwrap();
}

/// Standing rules for the review, as a harness would report them.
struct Rules(Vec<adk_kernel::RuleAge>);

#[async_trait]
impl adk_kernel::StandingRuleSource for Rules {
    async fn rules(&self, _app: &str, user: &str) -> adk_kernel::Result<Vec<adk_kernel::RuleAge>> {
        Ok(self.0.iter().filter(|rule| rule.user == user).cloned().collect())
    }
}

/// The next `KernelEvent::Review` on `events` within `wait`, if any.
async fn next_review(
    events: &mut adk_kernel::EventSubscriber,
    wait: Duration,
) -> Option<adk_kernel::ReviewReport> {
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(record)) = tokio::time::timeout_at(deadline, events.recv()).await {
        if let KernelEvent::Review(report) = record.event {
            return Some(report);
        }
    }
    None
}

/// The daily review (spec 3.2 §7.4, §9.1 permission drift), on the kernel's clock: a
/// standing rule without an expiry granted longer ago than the review period, a process
/// asleep longer than it and a process a freeze parked longer ago than it are each
/// reported, with a renewal ask for the rule, as one event and one `kernel.review` record;
/// nothing is revoked, woken or killed; and the report repeats no more than daily.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daily_review_reports_stale_rules_sleepers_and_freezes_once_a_day() {
    // Noon, so "an hour later the same day" never crosses midnight in UTC.
    let start = Utc::now().date_naive().and_hms_opt(12, 0, 0).unwrap().and_utc();
    let clock = ManualClock::new(start);
    let mut world = World::answer();
    world.runs = Arc::new(InMemoryRunStore::new().with_clock(Arc::new(clock.clone())));
    let control = GovernanceControl::new();
    let day = chrono::Duration::days(1);
    let rules = Rules(vec![
        adk_kernel::RuleAge::new("rule-stale", "alice", start - day * 10),
        adk_kernel::RuleAge::new("rule-expiring", "alice", start - day * 10)
            .with_expiry(start + day * 365),
        adk_kernel::RuleAge::new("rule-of-bob", "bob", start - day * 400),
    ]);
    let recorder = adk_core::audit_log::InMemoryAuditRecorder::new();
    let policy = ProactivityPolicy::default().with_review_period(Duration::from_secs(30 * 86_400));
    let kernel = Kernel::builder()
        .config(config(&control, Arc::new(clock.clone())).with_proactivity(policy))
        .run_store(Arc::clone(&world.runs))
        .process_store(Arc::clone(&world.processes) as Arc<dyn ProcessStore>)
        .session_service(Arc::clone(&world.sessions) as Arc<dyn SessionService>)
        .agent(TARGET, runner(&world, &control, Arc::new(HeldStep(Arc::default()))))
        .standing_rules(Arc::new(rules))
        .audit(adk_core::audit_log::AuditTrail::new(Arc::new(recorder.clone())))
        .build()
        .unwrap();
    let mut events = kernel.events();
    kernel.start().await.unwrap();

    // Two processes asleep from the start; the second as a freeze would leave it.
    let mut pids = Vec::new();
    for text in ["asleep", "frozen"] {
        let process = kernel
            .processes()
            .spawn(
                run_on(QueueClass::Background, text),
                ProcessSpec::new(ProcessKind::Goal)
                    .with_wake(adk_kernel::WakeSet::any_of([Wake::Manual])),
            )
            .await
            .unwrap();
        in_state(&world.runs, &process.meta.pid, RunState::Waiting).await;
        pids.push(process.meta.pid);
    }
    let mut frozen = world.processes.get(&pids[1]).await.unwrap().unwrap();
    frozen.wake = None;
    frozen.suspended_by = Some(Suspension::Freeze);
    world.processes.put(frozen).await.unwrap();

    // Today nothing is older than thirty days but the rule, which no user of this kernel
    // owns yet: the first tick's review reports nothing.
    assert_eq!(next_review(&mut events, Duration::from_millis(300)).await, None);

    // Thirty-one days on: one report with all three.
    clock.advance(Duration::from_secs(31 * 86_400));
    let report = next_review(&mut events, Duration::from_secs(3)).await.expect("a review");
    assert_eq!(report.partition, PartitionId::default());
    let sleepers: Vec<_> = report.sleepers.iter().map(|aged| aged.pid.clone()).collect();
    let frozen: Vec<_> = report.frozen.iter().map(|aged| aged.pid.clone()).collect();
    assert_eq!((sleepers, frozen), (vec![pids[0].clone()], vec![pids[1].clone()]));
    assert!(report.sleepers[0].age >= Duration::from_secs(31 * 86_400));
    let rules: Vec<_> = report.rules.iter().map(|rule| rule.id.as_str()).collect();
    assert_eq!(rules, ["rule-stale"], "only alice runs here; an expiring rule is not asked about");
    assert!(report.renewal_ask().unwrap().contains("rule-stale"));
    let records: Vec<_> = recorder
        .records()
        .into_iter()
        .filter(|record| record.action == adk_kernel::audit::REVIEW)
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].ext["rules"][0]["id"], "rule-stale");
    assert!(records[0].ext["ask"].as_str().unwrap().contains("Renew"));
    // The review changes nothing.
    for pid in &pids {
        assert_eq!(world.runs.get(pid).await.unwrap().unwrap().state, RunState::Waiting);
    }

    // Later the same day: no second report. The next day: one more.
    clock.advance(Duration::from_secs(60 * 60));
    assert_eq!(next_review(&mut events, Duration::from_millis(400)).await, None);
    clock.advance(Duration::from_secs(86_400));
    assert!(next_review(&mut events, Duration::from_secs(3)).await.is_some());
    kernel.stop().await.unwrap();
}
