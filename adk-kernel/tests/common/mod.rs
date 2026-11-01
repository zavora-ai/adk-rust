//! Shared fixtures for the supervisor tests: a scripted agent with a tool a test can hold
//! mid-run, and kernels over in-memory stores.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    DurableRunStore, GovernanceControl, InMemoryActionLedger, InMemoryRunStore, Llm, NewRun,
    RunRecord, RunState, Tool, ToolContext, ToolEffect,
};
use adk_kernel::{InMemoryProcessStore, Kernel, KernelConfig, ProcessStore, QueueClass};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

pub const APP: &str = "adk-rust";
pub const TARGET: &str = "adk-rust";

/// A tool that counts its executions and, when held, waits for the test to release it.
pub struct Gate {
    pub calls: Arc<AtomicUsize>,
    pub release: Arc<Semaphore>,
}

#[async_trait]
impl Tool for Gate {
    fn name(&self) -> &str {
        "step"
    }

    fn description(&self) -> &str {
        "does one step of the work"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.release.acquire().await.expect("the semaphore stays open").forget();
        Ok(json!({ "status": "done" }))
    }
}

/// The stores, the scripted model and the gate shared by every kernel replica of one test.
pub struct World {
    pub runs: Arc<dyn DurableRunStore>,
    pub processes: Arc<InMemoryProcessStore>,
    pub sessions: Arc<InMemorySessionService>,
    pub ledger: Arc<InMemoryActionLedger>,
    pub model: Arc<ScriptedLlm>,
    pub calls: Arc<AtomicUsize>,
    pub release: Arc<Semaphore>,
}

impl World {
    /// A world whose model answers with `script`.
    pub fn new(script: Vec<ScriptedResponse>) -> Self {
        Self {
            runs: Arc::new(InMemoryRunStore::new()),
            processes: Arc::new(InMemoryProcessStore::new()),
            sessions: Arc::new(InMemorySessionService::new()),
            ledger: Arc::new(InMemoryActionLedger::new()),
            model: Arc::new(ScriptedLlm::new("scripted", script)),
            calls: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }

    /// A model that calls the gated tool once, then answers.
    pub fn one_step() -> Self {
        Self::new(vec![
            ScriptedResponse::function_call("step", json!({})),
            ScriptedResponse::text("done"),
        ])
    }

    /// A model that answers at once.
    pub fn answer() -> Self {
        Self::new(vec![ScriptedResponse::text("done")])
    }

    pub fn model_calls(&self) -> usize {
        self.model.requests().len()
    }

    /// A runner over the world's model, sessions and ledger, governed by `control`.
    pub fn runner(&self, control: &GovernanceControl) -> Arc<Runner> {
        let agent = LlmAgentBuilder::new("agent")
            .model(Arc::clone(&self.model) as Arc<dyn Llm>)
            .tool(Arc::new(Gate {
                calls: Arc::clone(&self.calls),
                release: Arc::clone(&self.release),
            }))
            .build()
            .unwrap();
        Arc::new(
            Runner::builder()
                .app_name(APP)
                .agent(Arc::new(agent))
                .session_service(Arc::clone(&self.sessions) as Arc<dyn SessionService>)
                .action_ledger(Arc::clone(&self.ledger) as Arc<dyn adk_core::ActionLedger>)
                .governance(control.clone())
                .build()
                .unwrap(),
        )
    }

    /// A kernel replica named `name` over the world, with fast polls and no control plane.
    pub fn kernel(&self, name: &str, control: &GovernanceControl) -> Kernel {
        self.kernel_with(name, control, |builder| builder)
    }

    pub fn kernel_with(
        &self,
        name: &str,
        control: &GovernanceControl,
        extend: impl FnOnce(adk_kernel::KernelBuilder) -> adk_kernel::KernelBuilder,
    ) -> Kernel {
        let worker = DurableWorkerConfig::default()
            .with_worker_id(name)
            .with_lease_ttl(Duration::from_millis(600))
            .with_poll_interval(Duration::from_millis(20));
        let config = KernelConfig::default()
            .with_app_name(APP)
            .with_worker(worker)
            .with_scheduler_tick(Duration::from_millis(50))
            .with_governance(control.clone())
            .with_control(adk_kernel::control::ControlPlaneConfig::default().without_socket());
        let builder = Kernel::builder()
            .config(config)
            .run_store(Arc::clone(&self.runs))
            .process_store(Arc::clone(&self.processes) as Arc<dyn ProcessStore>)
            .session_service(Arc::clone(&self.sessions) as Arc<dyn SessionService>)
            .agent(TARGET, self.runner(control));
        extend(builder).build().unwrap()
    }
}

/// A run of the scripted agent on `queue`.
pub fn run_on(queue: QueueClass, text: &str) -> NewRun {
    NewRun::new(queue.as_str(), APP, "alice", TARGET, json!({ "text": text }))
}

/// Polls `probe` every 10 ms until it returns `Some`, for at most five seconds.
pub async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(5) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Waits until run `pid` is in `state`.
pub async fn in_state(runs: &Arc<dyn DurableRunStore>, pid: &str, state: RunState) -> RunRecord {
    eventually(&format!("{pid} to be {state:?}"), || async {
        runs.get(pid).await.ok().flatten().filter(|run| run.state == state)
    })
    .await
}

/// Waits until the gated tool has been called `n` times.
pub async fn tool_called(world: &World, n: usize) {
    let calls = Arc::clone(&world.calls);
    eventually(&format!("{n} tool calls"), || {
        let calls = Arc::clone(&calls);
        async move { (calls.load(Ordering::SeqCst) >= n).then_some(()) }
    })
    .await;
}

/// A `ProcessStore` whose writes take `delay`, as a remote store's might, so a test can
/// check that no correctness depends on a row being written before a worker claims.
pub struct SlowPuts {
    pub inner: Arc<dyn ProcessStore>,
    pub delay: Duration,
}

#[async_trait]
impl ProcessStore for SlowPuts {
    async fn put(&self, meta: adk_kernel::ProcessMeta) -> adk_kernel::Result<()> {
        tokio::time::sleep(self.delay).await;
        self.inner.put(meta).await
    }

    async fn get(&self, pid: &str) -> adk_kernel::Result<Option<adk_kernel::ProcessMeta>> {
        self.inner.get(pid).await
    }

    async fn list(
        &self,
        filter: &adk_kernel::ProcessFilter,
    ) -> adk_kernel::Result<Vec<adk_kernel::ProcessMeta>> {
        self.inner.list(filter).await
    }

    async fn set_wake(
        &self,
        pid: &str,
        wake: Option<adk_kernel::WakeSet>,
    ) -> adk_kernel::Result<()> {
        tokio::time::sleep(self.delay).await;
        self.inner.set_wake(pid, wake).await
    }

    async fn set_queue(&self, pid: &str, queue: QueueClass) -> adk_kernel::Result<()> {
        self.inner.set_queue(pid, queue).await
    }

    async fn delete(&self, pid: &str) -> adk_kernel::Result<bool> {
        self.inner.delete(pid).await
    }

    async fn set_suspend_requested(&self, pid: &str, requested: bool) -> adk_kernel::Result<()> {
        tokio::time::sleep(self.delay).await;
        self.inner.set_suspend_requested(pid, requested).await
    }

    async fn add_spend(
        &self,
        pid: &str,
        partition: &adk_kernel::PartitionId,
        day: chrono::NaiveDate,
        delta: adk_kernel::SpendDelta,
    ) -> adk_kernel::Result<()> {
        self.inner.add_spend(pid, partition, day, delta).await
    }

    async fn spend(
        &self,
        scope: &adk_kernel::stores::SpendScope,
        from: chrono::NaiveDate,
        to_inclusive: chrono::NaiveDate,
    ) -> adk_kernel::Result<u64> {
        self.inner.spend(scope, from, to_inclusive).await
    }

    async fn usage(
        &self,
        scope: &adk_kernel::stores::SpendScope,
        from: chrono::NaiveDate,
        to_inclusive: chrono::NaiveDate,
    ) -> adk_kernel::Result<adk_kernel::UsageTotals> {
        self.inner.usage(scope, from, to_inclusive).await
    }
}

/// Whether the session of `pid` holds a model turn.
pub async fn made_a_model_call(world: &World, pid: &str) -> bool {
    let session = world
        .sessions
        .get(adk_session::GetRequest {
            app_name: APP.into(),
            user_id: "alice".into(),
            session_id: format!("run-{pid}"),
            num_recent_events: None,
            after: None,
        })
        .await;
    let Ok(session) = session else { return false };
    session.events().all().iter().any(|event| {
        event.llm_response.content.as_ref().is_some_and(|content| content.role == "model")
    })
}
