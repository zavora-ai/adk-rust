//! A driver call parked on its server's question survives a kernel restart by re-issue
//! (spec 3.2 §7.7, §13 Q9): an agent run on a real worker calls a stdio driver whose tool
//! asks first; the call parks as a held call with its checkpoint, the kernel and the
//! driver process stop, a new kernel starts, the person answers in the inbox, and the run
//! resumes and completes with the tool executed once.

#![cfg(all(feature = "drivers", feature = "sqlite"))]

mod driver_support;

use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalService, ApproverSelector, DurableRunStore,
    InMemoryActionLedger, InMemoryApprovalStore, InMemoryRunStore, InboxQuery, Llm, NewRun,
    RunConfig, RunState, StoreApprovalService, SubjectRef,
};
use adk_kernel::drivers::{DriverStore, DriverSupervisor};
use adk_kernel::stores::SqliteDriverStore;
use adk_kernel::{
    InMemoryProcessStore, Kernel, KernelConfig, PartitionId, ProcessKind, ProcessSpec,
    ProcessStore, QueueClass,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use driver_support::Workdir;
use serde_json::json;

#[tokio::test]
#[ignore = "runs only as the stdio driver the tests start"]
async fn stdio_driver() {
    driver_support::serve_stdio().await;
}

const APP: &str = "adk-rust";

/// What outlives a kernel: the stores, the model and the server's work directory.
struct World {
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<InMemoryProcessStore>,
    sessions: Arc<InMemorySessionService>,
    approvals: Arc<InMemoryApprovalStore>,
    ledger: Arc<InMemoryActionLedger>,
    drivers: Arc<dyn DriverStore>,
    model: Arc<ScriptedLlm>,
    work: Workdir,
}

impl World {
    async fn new() -> Self {
        let work = Workdir::new();
        // The driver store is a file, as `harness.db` is, so the park outlives the kernel.
        let url = format!("sqlite://{}?mode=rwc", work.path().join("kernel.db").display());
        let drivers = SqliteDriverStore::new(sqlx::SqlitePool::connect(&url).await.unwrap());
        drivers.migrate().await.unwrap();
        Self {
            runs: Arc::new(InMemoryRunStore::new()),
            processes: Arc::new(InMemoryProcessStore::new()),
            sessions: Arc::new(InMemorySessionService::new()),
            approvals: Arc::new(InMemoryApprovalStore::new()),
            ledger: Arc::new(InMemoryActionLedger::new()),
            drivers: Arc::new(drivers),
            model: Arc::new(ScriptedLlm::new(
                "scripted",
                vec![
                    ScriptedResponse::function_call("send_note", json!({ "text": "hello" })),
                    ScriptedResponse::text("the note is sent"),
                ],
            )),
            work,
        }
    }

    /// The inbox, routing held calls to alice and waking the run that holds them.
    fn service(&self) -> Arc<dyn ApprovalService> {
        Arc::new(
            StoreApprovalService::new(self.approvals.clone())
                .with_router(
                    ApprovalPolicy::new([ApproverSelector::human("alice")]).allow_self_approval(),
                )
                .with_run_store(self.runs.clone()),
        )
    }

    /// A kernel, as one process of `adk-rust up` would build it, with a fresh supervisor.
    fn kernel(&self, name: &str) -> (Kernel, DriverSupervisor) {
        self.kernel_with(name, false)
    }

    /// A kernel whose run policy asks before every `send_note` when `ask` is set.
    fn kernel_with(&self, name: &str, ask: bool) -> (Kernel, DriverSupervisor) {
        let drivers = DriverSupervisor::builder(self.drivers.clone())
            .health_check_interval(Duration::from_millis(200))
            .build();
        let service = self.service();
        let agent = LlmAgentBuilder::new("agent")
            .model(self.model.clone() as Arc<dyn Llm>)
            .toolset(drivers.toolset_for(&PartitionId::default(), &["notes".to_string()]))
            .build()
            .unwrap();
        let runner = Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(self.sessions.clone() as Arc<dyn SessionService>)
            .run_config(RunConfig::builder().approval_service(service.clone()).build())
            .approval_store(self.approvals.clone())
            .action_ledger(self.ledger.clone())
            .tool_policy(if ask {
                Arc::new(adk_core::StrictestPolicy::new(vec![
                    drivers.policy(),
                    Arc::new(
                        adk_core::DeclarativePolicy::builder()
                            .require_approval("send_note", "sending needs a person")
                            .build(),
                    ),
                ]))
            } else {
                drivers.policy()
            })
            .build()
            .unwrap();
        let worker = DurableWorkerConfig::default()
            .with_worker_id(name)
            .with_lease_ttl(Duration::from_secs(2))
            .with_poll_interval(Duration::from_millis(20));
        let kernel = Kernel::builder()
            .config(
                KernelConfig::default()
                    .with_app_name(APP)
                    .with_home(self.work.path())
                    .with_worker(worker)
                    .with_scheduler_tick(Duration::from_millis(50))
                    .with_control(
                        adk_kernel::control::ControlPlaneConfig::default().without_socket(),
                    ),
            )
            .run_store(self.runs.clone())
            .process_store(self.processes.clone() as Arc<dyn ProcessStore>)
            .session_service(self.sessions.clone() as Arc<dyn SessionService>)
            .approval_service(service)
            .agent(APP, Arc::new(runner))
            .drivers(drivers.clone())
            .build()
            .unwrap();
        (kernel, drivers)
    }
}

async fn eventually<T, Fut: std::future::Future<Output = Option<T>>>(
    what: &str,
    mut probe: impl FnMut() -> Fut,
) -> T {
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(20) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parked_elicitation_is_answered_after_a_kernel_restart_and_runs_once() {
    let world = World::new().await;
    let (kernel, drivers) = world.kernel("first");
    kernel.start().await.unwrap();
    drivers
        .mount(world.work.manifest("notes", driver_support::UNSANDBOXED), "alice")
        .await
        .unwrap();

    let run = NewRun::new(QueueClass::INTERACTIVE, APP, "alice", APP, json!({ "text": "send it" }));
    let pid = kernel
        .processes()
        .spawn(run, ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;

    // The server asks; the call parks as a held call with its checkpoint, and the run waits.
    let runs = world.runs.clone();
    eventually("the run to park", || {
        let runs = runs.clone();
        let pid = pid.clone();
        async move { runs.get(&pid).await.unwrap().filter(|run| run.state == RunState::Waiting) }
    })
    .await;
    let parks = world.drivers.list_parks(Some("notes")).await.unwrap();
    assert_eq!(parks.len(), 1, "{parks:?}");
    let park = parks[0].clone();
    assert_eq!(park.call.kind, adk_tool::mcp::ParkedCallKind::Mrtr);
    assert_eq!(park.call.request_state.as_deref(), Some("state-1"));
    assert_eq!(park.run_id.as_deref(), Some(pid.as_str()));
    assert!(world.work.executions().is_empty(), "nothing ran before the answer");

    // The kernel stops: the worker, the agent's call and the driver's process are gone.
    kernel.stop().await.unwrap();
    drop((kernel, drivers));

    // A new kernel starts over the same stores, and the person answers in the inbox.
    let (kernel, _drivers) = world.kernel("second");
    kernel.start().await.unwrap();
    let service = world.service();
    let inbox = service.inbox(InboxQuery::default()).await.unwrap();
    let request = inbox
        .items
        .iter()
        .find(|item| item.tool_name == "driver:notes/elicitation")
        .expect("the question waits in the inbox")
        .clone();
    assert_eq!(request.run_id.as_deref(), Some(pid.as_str()));
    service
        .decide(&request.id, request.version, ApprovalDecision::approve(SubjectRef::human("alice")))
        .await
        .unwrap();

    let completed = eventually("the run to complete", || {
        let runs = runs.clone();
        let pid = pid.clone();
        async move { runs.get(&pid).await.unwrap().filter(|run| run.state.is_terminal()) }
    })
    .await;
    assert_eq!(completed.state, RunState::Completed, "{completed:?}");
    assert_eq!(world.work.executions(), ["hello"], "the call ran exactly once");
    assert!(world.drivers.list_parks(None).await.unwrap().is_empty(), "the checkpoint is spent");
    assert_eq!(world.model.requests().len(), 2);
    kernel.stop().await.unwrap();
}

/// Waits for the one pending inbox request for `tool` and approves it.
async fn approve(service: &Arc<dyn ApprovalService>, tool: &str) {
    let request = eventually(&format!("a request for {tool}"), || {
        let service = service.clone();
        async move {
            let inbox = service.inbox(InboxQuery::default()).await.unwrap();
            let pending: Vec<_> =
                inbox.items.into_iter().filter(|item| item.tool_name == tool).collect();
            assert!(pending.len() <= 1, "one request for {tool} at a time: {pending:?}");
            pending.into_iter().next()
        }
    })
    .await;
    service
        .decide(&request.id, request.version, ApprovalDecision::approve(SubjectRef::human("alice")))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_approved_before_its_question_is_not_asked_again_after_a_restart() {
    let world = World::new().await;
    let service = world.service();
    let (kernel, drivers) = world.kernel_with("first", true);
    kernel.start().await.unwrap();
    drivers
        .mount(world.work.manifest("notes", driver_support::UNSANDBOXED), "alice")
        .await
        .unwrap();
    let run = NewRun::new(QueueClass::INTERACTIVE, APP, "alice", APP, json!({ "text": "send it" }));
    let pid = kernel
        .processes()
        .spawn(run, ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    let runs = world.runs.clone();
    let waiting = || {
        let runs = runs.clone();
        let pid = pid.clone();
        async move { runs.get(&pid).await.unwrap().filter(|run| run.state == RunState::Waiting) }
    };

    // First the policy holds the call itself.
    approve(&service, "send_note").await;

    // The approved call runs, the server asks, and the call parks on the question.
    let drivers_store = world.drivers.clone();
    eventually("the call to park on its question", || {
        let store = drivers_store.clone();
        async move { (!store.list_parks(None).await.unwrap().is_empty()).then_some(()) }
    })
    .await;
    eventually("the run to wait for the answer", waiting).await;

    kernel.stop().await.unwrap();
    drop((kernel, drivers));
    let (kernel, _drivers) = world.kernel_with("second", true);
    kernel.start().await.unwrap();
    approve(&service, "driver:notes/elicitation").await;
    let completed = eventually("the run to complete", || {
        let runs = runs.clone();
        let pid = pid.clone();
        async move { runs.get(&pid).await.unwrap().filter(|run| run.state.is_terminal()) }
    })
    .await;
    assert_eq!(completed.state, RunState::Completed, "{completed:?}");
    assert_eq!(world.work.executions(), ["hello"]);
    // The default query lists pending requests.
    let inbox = service.inbox(InboxQuery::default()).await.unwrap();
    assert!(inbox.items.is_empty(), "the call was not asked about again: {:?}", inbox.items);
    kernel.stop().await.unwrap();
}
