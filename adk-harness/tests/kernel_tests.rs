//! The harness hosted by the Agent Kernel (spec 3.2 §10 HK1, HK3): `Harness::kernel` hosts
//! the harness runner as the app's and the goal target, a kernel-hosted goal on the `Graph`
//! engine keeps a bounded checkpoint thread and parks when it stops making progress, and a
//! decision through the harness's approval service wakes a process sleeping on it.

#![cfg(all(feature = "kernel", feature = "graph"))]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalDecision, ApprovalDelegation, ApprovalPolicy, ApprovalRequest, ApprovalRequestQuery,
    ApprovalScope, ApprovalService, ApprovalStore, ApproverSelector, DurableRunStore,
    InMemoryApprovalStore, InMemoryRunStore, NewRun, PendingApproval, RunState,
    StoreApprovalService, SubjectRef, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationRequest,
};
use adk_harness::{
    GOAL_STALL_WINDOW, GOAL_TARGET, GoalSpec, GoalStatus, HarnessDecision, HarnessMode,
};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    InMemoryProcessStore, Kernel, KernelConfig, ProcessFilter, ProcessKind, ProcessSpec,
    ProcessStore, QueueClass, Suspension, Wake, WakeSet,
};
use adk_model::ScriptedResponse;
use adk_server::background::DurableWorkerConfig;
use common::*;
use serde_json::{Value, json};

fn write(path: &str) -> ScriptedResponse {
    ScriptedResponse::function_call("write_file", json!({ "path": path, "content": path }))
}

fn text(text: &str) -> ScriptedResponse {
    ScriptedResponse::text(text)
}

/// A kernel over the fixture's harness, with fast polls and no control plane.
fn kernel(
    f: Fixture,
    runs: &Arc<dyn DurableRunStore>,
) -> (Kernel, Fixture, Arc<InMemoryProcessStore>) {
    let Fixture { dir, model, harness, audit, user, session } = f;
    let harness = harness.with_run_store(Arc::clone(runs)).with_goal_queue(QueueClass::BACKGROUND);
    let processes = Arc::new(InMemoryProcessStore::new());
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("k1")
                        .with_lease_ttl(Duration::from_millis(600)),
                )
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .process_store(Arc::clone(&processes) as Arc<dyn ProcessStore>)
        .build()
        .unwrap();
    (kernel, Fixture { dir, model, harness, audit, user, session }, processes)
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(10) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn in_state(runs: &Arc<dyn DurableRunStore>, pid: &str, state: RunState) {
    eventually(&format!("{pid} to be {state:?}"), || async {
        runs.get(pid).await.ok().flatten().filter(|run| run.state == state).map(|_| ())
    })
    .await;
}

fn goal(until: &str, max_iterations: u32) -> GoalSpec {
    GoalSpec {
        goal: "make progress".into(),
        until: until.into(),
        max_iterations,
        check_timeout: Duration::from_secs(30),
    }
}

/// Starts `spec` as a durable goal run and returns its pid.
async fn start(f: &Fixture, spec: GoalSpec) -> String {
    let events = collect(f.harness.start_goal(&f.user, &f.session, spec).await.unwrap()).await;
    events
        .iter()
        .find_map(|event| event.provider_metadata.get("harness.goal.run_id").cloned())
        .expect("the goal runs as a durable run")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_kernel_hosts_the_harness_as_the_app_and_goal_targets() {
    let f = fixture(vec![text("hello"), write("done.txt"), text("done")], |b| {
        b.mode(HarnessMode::Auto).without_default_members()
    })
    .await;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let (kernel, f, _) = kernel(f, &runs);
    kernel.start().await.unwrap();

    let turn = kernel
        .processes()
        .spawn(
            NewRun::new("interactive", APP, f.user.as_ref(), APP, json!({ "text": "hi" }))
                .with_session_id(f.session.as_ref()),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await
        .unwrap();
    in_state(&runs, &turn.meta.pid, RunState::Completed).await;

    let goal_pid = start(&f, goal("test -f done.txt", 3)).await;
    in_state(&runs, &goal_pid, RunState::Completed).await;
    let goal_run = runs.get(&goal_pid).await.unwrap().unwrap();
    assert_eq!((goal_run.target.as_str(), goal_run.queue.as_str()), (GOAL_TARGET, "background"));
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!(state.status, GoalStatus::Done);
    let listed = kernel.processes().ps(ProcessFilter::default()).await.unwrap();
    let goal_process = listed.iter().find(|process| process.meta.pid == goal_pid).unwrap();
    assert_eq!(goal_process.meta.kind, ProcessKind::Goal);
    let progress = goal_process.view.goal.clone().expect("the goal's progress is in the view");
    assert_eq!((progress.iteration, progress.max_iterations), (1, 3));
    assert_eq!(progress.last_check_exit, Some(0));
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_goal_that_repeats_its_check_result_parks_suspended_with_a_record() {
    let mut script = Vec::new();
    for n in 0..8 {
        script.push(write(&format!("step-{n}.txt")));
        script.push(text("working"));
    }
    let checkpointer: Arc<dyn adk_graph::Checkpointer> =
        Arc::new(adk_graph::MemoryCheckpointer::new());
    let probe = checkpointer.clone();
    let f = fixture(script, move |b| {
        b.mode(HarnessMode::Auto).without_default_members().plan_checkpointer(checkpointer)
    })
    .await;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let (kernel, f, processes) = kernel(f, &runs);
    kernel.start().await.unwrap();

    // `false` fails the same way every iteration: no progress.
    let pid = start(&f, goal("false", 8)).await;
    in_state(&runs, &pid, RunState::Waiting).await;
    let meta = processes.get(&pid).await.unwrap().unwrap();
    assert_eq!(meta.suspended_by, Some(Suspension::Manual), "parked as a manual suspension");
    let stalled = f.records("harness.goal.stalled");
    assert_eq!(stalled.len(), 1);
    assert_eq!(stalled[0].ext["reason"], "same_check");
    assert_eq!(stalled[0].ext["pid"], Value::String(pid.clone()));
    assert_eq!(stalled[0].ext["iteration"], GOAL_STALL_WINDOW);
    let state = f.harness.goal(&f.user, &f.session).await.unwrap().unwrap();
    assert_eq!(state.status, GoalStatus::Running, "the goal keeps its iterations for a resume");
    assert_eq!(state.iteration, GOAL_STALL_WINDOW);
    let calls = f.model.requests().len();
    assert_eq!(calls, 2 * GOAL_STALL_WINDOW as usize, "no model call after the stall");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(f.model.requests().len(), calls);

    let record = f
        .harness
        .agent()
        .runtime()
        .goal_store()
        .get(APP, f.user.as_ref(), f.session.as_ref())
        .await
        .unwrap()
        .unwrap();
    let thread = format!(
        "{}:goal:{}",
        adk_graph::agent::session_thread_id(APP, f.user.as_ref(), f.session.as_ref()),
        record.run_id
    );
    let kept = probe.list(&thread).await.unwrap().len();
    assert!(kept <= 2 * GOAL_STALL_WINDOW as usize, "{kept} checkpoints kept");

    let view = kernel.processes().get(&pid).await.unwrap().unwrap().view;
    let progress = view.goal.expect("the stalled goal shows its progress");
    assert_eq!(progress.iteration, GOAL_STALL_WINDOW);
    assert_eq!(progress.last_check_exit, Some(1));
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_iteration_without_a_tool_call_parks_the_goal() {
    let f = fixture(vec![text("thinking"), text("still thinking")], |b| {
        b.mode(HarnessMode::Auto)
            .without_default_members()
            .goal_engine(adk_harness::GoalEngine::Graph)
    })
    .await;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let (kernel, f, processes) = kernel(f, &runs);
    kernel.start().await.unwrap();

    let pid = start(&f, goal("false", 5)).await;
    in_state(&runs, &pid, RunState::Waiting).await;
    assert_eq!(processes.get(&pid).await.unwrap().unwrap().suspended_by, Some(Suspension::Manual));
    let stalled = f.records("harness.goal.stalled");
    assert_eq!(stalled[0].ext["reason"], "no_tool_call");
    assert_eq!(stalled[0].ext["iteration"], 1);
    assert_eq!(f.model.requests().len(), 1);
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_process_sleeping_on_an_approval_wakes_when_the_decision_lands() {
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let service: Arc<dyn ApprovalService> = Arc::new(StoreApprovalService::new(approvals.clone()));
    let f = fixture(vec![text("the transfer was decided")], {
        let service = service.clone();
        move |b| b.mode(HarnessMode::Auto).without_default_members().approval_service(service)
    })
    .await;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let (kernel, f, _) = kernel(f, &runs);
    kernel.start().await.unwrap();

    let scope = ApprovalScope::new(APP, f.user.as_ref(), "s-transfer");
    let call = ToolConfirmationRequest {
        tool_name: "transfer".into(),
        function_call_id: Some("call-1".into()),
        args: json!({ "amount": 10 }),
        acting: None,
        origin: None,
    };
    approvals.record_pending(&scope, &call).await.unwrap();
    let wake = WakeSet::any_of([Wake::Approval { fingerprint: call.fingerprint() }]);
    let pid = kernel
        .processes()
        .spawn(
            NewRun::new(
                "background",
                APP,
                f.user.as_ref(),
                APP,
                json!({ "text": "report the outcome" }),
            )
            .with_session_id(f.session.as_ref()),
            ProcessSpec::new(ProcessKind::Interactive).with_wake(wake),
        )
        .await
        .unwrap()
        .meta
        .pid;
    in_state(&runs, &pid, RunState::Waiting).await;
    assert_eq!(f.model.requests().len(), 0);

    // Decided through the harness's own approval service, as its inbox does.
    let harness_service = f.harness.agent().runtime().approval_service().unwrap().clone();
    let request = harness_service
        .submit(ApprovalRequest::new(
            "req-1",
            scope,
            &call,
            ApprovalPolicy::new([ApproverSelector::human("bob")]),
        ))
        .await
        .unwrap();
    harness_service
        .decide(&request.id, request.version, ApprovalDecision::approve(SubjectRef::human("bob")))
        .await
        .unwrap();
    in_state(&runs, &pid, RunState::Completed).await;
    assert_eq!(f.model.requests().len(), 1);
    kernel.stop().await.unwrap();
}

/// An in-memory approval store that, when a decision is recorded, notes whether a watched
/// process was still asleep — given time to wake first — so a test can tell whether anything
/// woke it before the decision existed.
#[derive(Default)]
struct WatchedDecisions {
    inner: InMemoryApprovalStore,
    watch: std::sync::OnceLock<(Arc<dyn DurableRunStore>, String)>,
    asleep_at_decision: std::sync::Mutex<Option<bool>>,
}

impl std::fmt::Debug for WatchedDecisions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchedDecisions").field("inner", &self.inner).finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl ApprovalStore for WatchedDecisions {
    async fn record_pending(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> adk_core::Result<()> {
        self.inner.record_pending(scope, request).await
    }
    async fn record_pending_keyed(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        request: &ToolConfirmationRequest,
    ) -> adk_core::Result<()> {
        self.inner.record_pending_keyed(scope, fingerprint, request).await
    }
    async fn pending(&self, scope: &ApprovalScope) -> adk_core::Result<Vec<PendingApproval>> {
        self.inner.pending(scope).await
    }
    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: ToolApproval,
    ) -> adk_core::Result<()> {
        if let Some((runs, pid)) = self.watch.get() {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            let mut asleep = true;
            while asleep && tokio::time::Instant::now() < deadline {
                asleep = runs.get(pid).await?.is_some_and(|run| run.state == RunState::Waiting);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            *self.asleep_at_decision.lock().unwrap() = Some(asleep);
        }
        self.inner.decide(scope, fingerprint, approval).await
    }
    async fn take_decision(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> adk_core::Result<Option<ToolConfirmationDecision>> {
        self.inner.take_decision(scope, fingerprint).await
    }
    async fn take_approval(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> adk_core::Result<Option<ToolApproval>> {
        self.inner.take_approval(scope, fingerprint).await
    }
    async fn has_decision(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> adk_core::Result<bool> {
        self.inner.has_decision(scope, fingerprint).await
    }
    async fn create_request(&self, request: &ApprovalRequest) -> adk_core::Result<ApprovalRequest> {
        self.inner.create_request(request).await
    }
    async fn get_request(&self, id: &str) -> adk_core::Result<Option<ApprovalRequest>> {
        self.inner.get_request(id).await
    }
    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> adk_core::Result<bool> {
        self.inner.replace_request(request, expected_version).await
    }
    async fn list_requests(
        &self,
        query: &ApprovalRequestQuery,
    ) -> adk_core::Result<Vec<ApprovalRequest>> {
        self.inner.list_requests(query).await
    }
    async fn put_delegation(&self, delegation: &ApprovalDelegation) -> adk_core::Result<()> {
        self.inner.put_delegation(delegation).await
    }
    async fn delegations_to(&self, to: &SubjectRef) -> adk_core::Result<Vec<ApprovalDelegation>> {
        self.inner.delegations_to(to).await
    }
    async fn delete_delegation(&self, id: &str) -> adk_core::Result<bool> {
        self.inner.delete_delegation(id).await
    }
}

/// Wave 2 review finding 21 (spec 3.3 §7.3): `Harness::decide` withdraws the call's inbox
/// request before it records the person's decision; under the kernel that withdrawal wakes no
/// process sleeping on the call, which wakes once the decision is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_harness_decision_wakes_a_sleeper_only_once_it_is_recorded() {
    let approvals = Arc::new(WatchedDecisions::default());
    let service: Arc<dyn ApprovalService> = Arc::new(StoreApprovalService::new(approvals.clone()));
    let f = fixture(vec![text("the transfer was decided")], {
        let service = service.clone();
        move |b| b.mode(HarnessMode::Auto).without_default_members().approval_service(service)
    })
    .await;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let (kernel, f, _) = kernel(f, &runs);
    kernel.start().await.unwrap();

    let scope = ApprovalScope::new(APP, f.user.as_ref(), f.session.as_ref());
    let call = ToolConfirmationRequest {
        tool_name: "transfer".into(),
        function_call_id: Some("call-1".into()),
        args: json!({ "amount": 10 }),
        acting: None,
        origin: None,
    };
    approvals.record_pending(&scope, &call).await.unwrap();
    let harness_service = f.harness.agent().runtime().approval_service().unwrap().clone();
    harness_service
        .submit(ApprovalRequest::new(
            "req-1",
            scope,
            &call,
            ApprovalPolicy::new([ApproverSelector::human("bob")]),
        ))
        .await
        .unwrap();
    let wake = WakeSet::any_of([Wake::Approval { fingerprint: call.fingerprint() }]);
    let pid = kernel
        .processes()
        .spawn(
            NewRun::new(
                "background",
                APP,
                f.user.as_ref(),
                APP,
                json!({ "text": "report the outcome" }),
            )
            .with_session_id("s-report"),
            ProcessSpec::new(ProcessKind::Interactive).with_wake(wake),
        )
        .await
        .unwrap()
        .meta
        .pid;
    in_state(&runs, &pid, RunState::Waiting).await;
    assert!(approvals.watch.set((Arc::clone(&runs), pid.clone())).is_ok());

    f.harness
        .decide(&f.user, &f.session, &call.fingerprint(), HarnessDecision::Approve)
        .await
        .unwrap();
    assert_eq!(
        *approvals.asleep_at_decision.lock().unwrap(),
        Some(true),
        "nothing woke the sleeper before the decision was recorded"
    );
    in_state(&runs, &pid, RunState::Completed).await;
    assert_eq!(f.model.requests().len(), 1);
    kernel.stop().await.unwrap();
}

/// `propose_followup` (spec 3.2 §7.5): in every mode a turn's proposal becomes a `Proposed`
/// agenda item whose origin is the proposing run. ReadOnly and Plan hold it; in Ask nothing
/// runs until the inbox approves it, then it runs as a `Proposed` process whose parent is
/// the proposing turn; in Auto the partition's policy schedules it at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn propose_followup_is_a_proposed_item_in_every_mode_and_ask_waits_for_the_inbox() {
    for mode in HarnessMode::ALL {
        let propose = ScriptedResponse::function_call(
            "propose_followup",
            json!({ "title": "Re-run the tests", "rationale": "the dependency bump lands tonight" }),
        );
        let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
        let service: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
                .with_run_store(Arc::clone(&runs)),
        );
        let f = fixture(vec![propose, text("Proposed."), text("Re-ran the tests.")], |b| {
            b.mode(mode).without_default_members().approval_service(Arc::clone(&service))
        })
        .await;
        let (kernel, f, processes) = kernel(f, &runs);
        kernel.start().await.unwrap();

        let turn = kernel
            .processes()
            .spawn(
                NewRun::new("interactive", APP, f.user.as_ref(), APP, json!({ "text": "bump it" }))
                    .with_session_id(f.session.as_ref()),
                ProcessSpec::new(ProcessKind::Interactive),
            )
            .await
            .unwrap();
        in_state(&runs, &turn.meta.pid, RunState::Completed).await;
        let items = kernel.agenda().list(&adk_kernel::AgendaFilter::default()).await.unwrap();
        assert_eq!(items.len(), 1, "{mode}: one item");
        let item = items[0].clone();
        assert_eq!(
            item.origin,
            adk_kernel::AgendaOrigin::Agent { pid: turn.meta.pid.clone(), confidence: 1.0 },
            "{mode}"
        );
        let followup = || async {
            processes
                .list(&ProcessFilter::default())
                .await
                .unwrap()
                .into_iter()
                .find(|meta| meta.agenda_item.as_deref() == Some(item.id.as_str()))
        };
        let inbox = f.harness.agent().runtime().approval_service().unwrap().clone();
        let request = inbox.get(&format!("agenda_{}", item.id)).await.unwrap();

        if mode != HarnessMode::Auto {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let held = kernel.agenda().get(&item.id).await.unwrap().unwrap();
            assert_eq!(held.state, adk_kernel::AgendaState::Proposed, "{mode}: still proposed");
            assert!(followup().await.is_none(), "{mode}: nothing runs (K-I10)");
            assert_eq!(f.model.requests().len(), 2, "{mode}: no model call for the follow-up");
            assert_eq!(request.is_some(), mode == HarnessMode::Ask, "{mode}: only Ask asks");
        }
        if mode == HarnessMode::Ask {
            let request = request.unwrap();
            inbox
                .decide(
                    &request.id,
                    request.version,
                    ApprovalDecision::approve(SubjectRef::human(f.user.as_ref())),
                )
                .await
                .unwrap();
        }
        if matches!(mode, HarnessMode::Ask | HarnessMode::Auto) {
            let done = eventually(&format!("{mode}: the follow-up done"), || async {
                kernel
                    .agenda()
                    .get(&item.id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|item| item.state == adk_kernel::AgendaState::Done)
            })
            .await;
            assert_eq!(done.title, "Re-run the tests");
            let meta = followup().await.expect("the follow-up's process");
            assert_eq!(meta.kind, ProcessKind::Proposed);
            assert_eq!(meta.parent.as_deref(), Some(turn.meta.pid.as_str()));
            assert_eq!(f.model.requests().len(), 3, "{mode}");
            // The follow-up's session ran in the proposing session's mode (K-I7).
            let session = adk_kernel::agenda::item_session(&item.id);
            let follow_mode = f.harness.mode(&f.user, &session_id(&session)).await;
            assert_eq!(follow_mode.unwrap(), mode);
        }
        kernel.stop().await.unwrap();
    }
}

/// Under a kernel with a ledger, a harness run writes its activity — its intent, the tool
/// call with its effect class and duration, the turn its `harness.spend` record describes —
/// beside the kernel's process rows, and no row holds the run's content (spec 3.2 §7.10,
/// K-I19).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_harness_writes_its_activity_to_the_kernels_ledger() {
    use adk_kernel::proc::{InMemoryLedgerStore, Ledger, LedgerKey, LedgerQuery, LedgerStore};

    let f = fixture(
        vec![
            ScriptedResponse::function_call("read_file", json!({ "path": "salary.md" })),
            text("PAYLOAD-ANSWER read it"),
        ],
        |b| b.mode(HarnessMode::Auto).without_default_members(),
    )
    .await;
    std::fs::write(f.path("salary.md"), "PAYLOAD-SALARY 120000").unwrap();
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let store = Arc::new(InMemoryLedgerStore::new());
    let Fixture { dir, model, harness, audit, user, session } = f;
    let harness = harness.with_run_store(Arc::clone(&runs));
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .ledger(Ledger::new(store.clone(), LedgerKey::from_bytes([2; 32])))
        .build()
        .unwrap();
    let f = Fixture { dir, model, harness, audit, user, session };
    kernel.start().await.unwrap();
    let turn = kernel
        .processes()
        .spawn(
            NewRun::new(
                "interactive",
                APP,
                f.user.as_ref(),
                APP,
                json!({ "text": "PAYLOAD-PROMPT read salary.md" }),
            )
            .with_session_id(f.session.as_ref()),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await
        .unwrap();
    in_state(&runs, &turn.meta.pid, RunState::Completed).await;
    let rows = eventually("the ledger rows of the turn", || async {
        let rows = store.list(&LedgerQuery::default()).await.unwrap();
        let kinds: Vec<&str> = rows.iter().map(|row| row.kind.as_str()).collect();
        (["intent", "tool_call", "turn"].iter().all(|kind| kinds.contains(kind))
            && rows
                .iter()
                .any(|row| row.kind == "process" && row.effect.as_deref() == Some("completed")))
        .then_some(rows)
    })
    .await;

    let tool = rows.iter().find(|row| row.kind == "tool_call").unwrap();
    assert_eq!(tool.effect.as_deref(), Some("read_only"));
    assert!(tool.duration_ms.is_some() && tool.subject_hash.is_some() && tool.trace_id.is_some());
    let harness_agent = format!("urn:adk:agent:{APP}/harness");
    assert_eq!(tool.agent, harness_agent);
    let intent = rows.iter().find(|row| row.kind == "intent").unwrap();
    assert_eq!((&intent.agent, &intent.trace_id), (&harness_agent, &tool.trace_id));
    let spent = rows.iter().find(|row| row.kind == "turn").unwrap();
    assert_eq!(spent.meta["model_calls"], 2);
    let finished = rows
        .iter()
        .find(|row| row.kind == "process" && row.effect.as_deref() == Some("completed"))
        .unwrap();
    assert!(finished.duration_ms.is_some(), "the time the process ran");
    for row in &rows {
        let line = row.to_jsonl().unwrap();
        assert!(!line.contains("PAYLOAD") && !line.contains("salary"), "{line}");
    }
    kernel.stop().await.unwrap();
}
