//! A kernel built as `KernelBuilder::approval_service` documents it, over a
//! `StoreApprovalService` with a run store and no router of its own:
//!
//! - a call the agent holds is filed in the inbox for the `operator` group, so the control
//!   plane lists it under its request id;
//! - the bus carries `held` when it is filed and `decided` when a decision resolves it;
//! - a decision through the control plane resumes the parked process, and the call runs once;
//! - a service that takes neither the kernel's router nor its notifier is named in the
//!   inventory.

use crate::agents_support::FlakyInbox;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ApprovalAction, ApprovalService, ApprovalState, DeclarativePolicy, DurableRunStore,
    GovernanceControl, InMemoryActionLedger, InMemoryApprovalStore, InMemoryRunStore, Llm,
    RunConfig, RunState, StoreApprovalService, Tool, ToolConfirmationDecision, ToolContext,
    ToolEffect,
};
use adk_kernel::control::api::SpawnRequest;
use adk_kernel::control::{ControlClient, ControlPlane, ControlPlaneConfig};
use adk_kernel::inventory::InventoryWarning;
use adk_kernel::{InMemoryProcessStore, Kernel, KernelConfig, KernelEvent, ProcessStore};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

const APP: &str = "watchdog";

/// A report the agent may only file with a person's approval.
struct FileReport {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for FileReport {
    fn name(&self) -> &str {
        "file_report"
    }

    fn description(&self) -> &str {
        "files the folder report"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "status": "filed" }))
    }
}

/// A started kernel serving its control plane on loopback, whose agent holds `file_report`
/// for approval, with `approvals` as the agent's and the kernel's approval service.
struct Watchdog {
    kernel: Kernel,
    plane: ControlPlane,
    runs: Arc<dyn DurableRunStore>,
    calls: Arc<AtomicUsize>,
}

impl Watchdog {
    async fn start(runs: Arc<dyn DurableRunStore>, approvals: Arc<dyn ApprovalService>) -> Self {
        let sessions = Arc::new(InMemorySessionService::new());
        let model = Arc::new(ScriptedLlm::new(
            "scripted",
            vec![
                ScriptedResponse::function_call("file_report", json!({ "folder": "inbox" })),
                ScriptedResponse::text("filed"),
            ],
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let control = GovernanceControl::new();
        let agent = LlmAgentBuilder::new("watchdog")
            .model(model as Arc<dyn Llm>)
            .tool(Arc::new(FileReport { calls: calls.clone() }))
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
                    .require_approval("file_report", "filing needs a person")
                    .build(),
            ))
            .governance(control.clone())
            .build()
            .unwrap();
        let worker = DurableWorkerConfig::default()
            .with_lease_ttl(Duration::from_secs(2))
            .with_poll_interval(Duration::from_millis(20));
        let kernel = Kernel::builder()
            .config(
                KernelConfig::default()
                    .with_app_name(APP)
                    .with_worker(worker)
                    .with_scheduler_tick(Duration::from_millis(50))
                    .with_governance(control)
                    .with_control(
                        ControlPlaneConfig::default()
                            .without_socket()
                            .with_http("127.0.0.1:0".parse().unwrap()),
                    ),
            )
            .run_store(runs.clone())
            .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
            .session_service(sessions as Arc<dyn SessionService>)
            .approval_service(approvals)
            .agent(APP, Arc::new(runner))
            .build()
            .unwrap();
        kernel.start().await.unwrap();
        let plane = ControlPlane::start(&kernel).await.unwrap();
        Self { kernel, plane, runs, calls }
    }

    /// The person at the kernel: a loopback caller without a token is the operator.
    fn operator(&self) -> ControlClient {
        ControlClient::http(format!("http://{}", self.plane.http_addr().unwrap()))
    }

    async fn stop(self) {
        self.plane.shutdown().await.unwrap();
        self.kernel.stop().await.unwrap();
    }
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

/// K4, K10: fails on `origin/main`, where the documented service files no request, the
/// control plane lists nothing, no `held` or `decided` is published, and the process stays
/// parked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kernel_files_and_publishes_the_calls_of_a_service_built_without_a_router() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(runs.clone()),
    );
    let watchdog = Watchdog::start(runs, approvals).await;
    assert!(
        !watchdog.kernel.inventory().warnings.iter().any(|warning| {
            warning.code == InventoryWarning::APPROVALS_UNROUTED
                || warning.code == InventoryWarning::APPROVALS_UNHEARD
        }),
        "{:?}",
        watchdog.kernel.inventory().warnings
    );
    let mut events = watchdog.kernel.bus().subscribe();
    let operator = watchdog.operator();
    let spawned =
        operator.spawn(&SpawnRequest::new(APP, json!({ "text": "file the report" }))).await;
    let pid = spawned.unwrap().meta.pid;

    let item = eventually("the held call in the inbox", || async {
        operator.approvals().await.unwrap().into_iter().next()
    })
    .await;
    assert_eq!(
        (item.tool_name.as_str(), item.run_id.as_deref(), item.state),
        ("file_report", Some(pid.as_str()), ApprovalState::Pending)
    );
    let parked = eventually("the process to park", || async {
        let run = watchdog.runs.get(&pid).await.unwrap()?;
        (run.state == RunState::Waiting).then_some(run)
    })
    .await;
    assert!(parked.checkpoint.is_some());
    assert_eq!(watchdog.calls.load(Ordering::SeqCst), 0);

    let decided =
        operator.decide(&item.id, item.version, ApprovalAction::Approve, None).await.unwrap();
    assert_eq!(decided.state, ApprovalState::Approved);
    eventually("the process to finish", || async {
        let run = watchdog.runs.get(&pid).await.unwrap()?;
        (run.state == RunState::Completed).then_some(())
    })
    .await;
    assert_eq!(watchdog.calls.load(Ordering::SeqCst), 1, "the call runs once");

    let mut held = Vec::new();
    let mut verdicts = Vec::new();
    while held.is_empty() || verdicts.is_empty() {
        let record = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("a held and a decided event")
            .expect("the bus stays open");
        match record.event {
            KernelEvent::Held { pid, request } => held.push((pid, request.tool_name)),
            KernelEvent::Decided { pid, decision, by, .. } => {
                verdicts.push((pid, decision, by.id));
            }
            _ => {}
        }
    }
    assert_eq!(held, vec![(pid.clone(), "file_report".to_string())]);
    assert_eq!(verdicts.len(), 1);
    assert_eq!((&verdicts[0].0, verdicts[0].1), (&pid, ToolConfirmationDecision::Approve));
    watchdog.stop().await;
}

/// A service that takes neither the kernel's router nor its notifier is named in the
/// inventory, so a host learns its held calls may reach no inbox and no bus.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_service_that_takes_neither_router_nor_notifier_is_in_the_inventory() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let watchdog = Watchdog::start(runs, FlakyInbox::new()).await;
    let mut codes: Vec<String> = watchdog
        .kernel
        .inventory()
        .warnings
        .into_iter()
        .map(|warning| warning.code)
        .filter(|code| code.starts_with("kernel.approvals."))
        .collect();
    codes.sort();
    assert_eq!(codes, [InventoryWarning::APPROVALS_UNHEARD, InventoryWarning::APPROVALS_UNROUTED]);
    watchdog.stop().await;
}
