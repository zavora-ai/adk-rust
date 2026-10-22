//! The harness hosted by the Agent Kernel (spec 3.2 §10 HK1, HK3): `Harness::kernel` hosts
//! the harness runner as the app's and the goal target, a kernel-hosted goal on the `Graph`
//! engine keeps a bounded checkpoint thread and parks when it stops making progress, and a
//! decision through the harness's approval service wakes a process sleeping on it.

#![cfg(all(feature = "kernel", feature = "graph"))]

mod common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService,
    ApprovalStore, ApproverSelector, DurableRunStore, InMemoryApprovalStore, InMemoryRunStore,
    NewRun, RunState, StoreApprovalService, SubjectRef, ToolConfirmationRequest,
};
use adk_harness::{GOAL_STALL_WINDOW, GOAL_TARGET, GoalSpec, GoalStatus, HarnessMode};
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
