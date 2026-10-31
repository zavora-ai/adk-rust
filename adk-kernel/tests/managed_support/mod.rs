//! Shared fixtures for the managed-form tests (spec 3.2 §7.13, §10 M1): tenants over their
//! own in-memory stores, an agent resolver standing in for the operator's, and a model that
//! calls one tool and then answers, whatever order concurrent runs ask in.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, Agent, ApprovalPolicy, ApprovalService,
    ApproverSelector, Content, DurableRunStore, FinishReason, InMemoryActionLedger,
    InMemoryApprovalStore, InMemoryRunStore, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part,
    RunFilter, RunRecord, StoreApprovalService, Tool, ToolContext, ToolEffect,
};
use adk_kernel::KernelConfig;
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::managed::{TenantKernel, TenantKernelBuilder};
use adk_managed::resolver::{ModelResolver, ResolverResult};
use adk_managed::types::{ManagedAgentDef, ModelRef, RuntimeError};
use adk_managed::{
    AgentResolver, DefaultManagedAgentRuntime, InMemoryManagedStateStore, ManagedStateStore,
    ManagedStores,
};
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

/// The approver group of every tenant's approval service.
pub const OPERATOR: &str = "operator";

/// Calls `step` once, then answers: it answers when the request's last content is the tool's
/// response, so runs that interleave each get both turns.
pub struct StepModel {
    pub requests: AtomicUsize,
}

#[async_trait]
impl Llm for StepModel {
    fn name(&self) -> &str {
        "step-model"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let answered = request.contents.last().is_some_and(|content| {
            content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
        });
        let content = if answered {
            Content::new("model").with_text("done")
        } else {
            Content {
                role: "model".into(),
                parts: vec![Part::FunctionCall {
                    name: "step".into(),
                    args: json!({}),
                    id: Some(format!("call-{}", uuid::Uuid::new_v4())),
                    thought_signature: None,
                }],
            }
        };
        let response = LlmResponse {
            content: Some(content),
            partial: false,
            turn_complete: true,
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// A non-idempotent tool that counts its executions.
pub struct Step {
    pub calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for Step {
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
        Ok(json!({ "status": "done" }))
    }
}

/// The operator's resolver: builds every definition as an agent on the tenant's model with
/// the counting tool, held for confirmation when `confirm`.
pub struct TestResolver {
    pub model: Arc<StepModel>,
    pub calls: Arc<AtomicUsize>,
    pub confirm: bool,
}

#[async_trait]
impl AgentResolver for TestResolver {
    async fn resolve(
        &self,
        def: &ManagedAgentDef,
        _models: &dyn ModelResolver,
    ) -> Result<Option<Arc<dyn Agent>>, RuntimeError> {
        let mut builder = LlmAgentBuilder::new(def.name.clone())
            .model(Arc::clone(&self.model) as Arc<dyn Llm>)
            .tool(Arc::new(Step { calls: Arc::clone(&self.calls) }));
        if self.confirm {
            builder = builder.require_tool_confirmation("step");
        }
        Ok(Some(Arc::new(builder.build().map_err(|e| RuntimeError::internal(e.to_string()))?)))
    }
}

/// Resolves every model reference to a model that answers at once.
pub struct Models;

#[async_trait]
impl ModelResolver for Models {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(Arc::new(adk_model::ScriptedLlm::new(
            "scripted",
            vec![adk_model::ScriptedResponse::text("hello")],
        )))
    }
}

/// An action ledger that counts what it records.
#[derive(Debug, Default)]
pub struct CountingLedger {
    pub inner: InMemoryActionLedger,
    pub begun: AtomicUsize,
    pub completed: AtomicUsize,
}

#[async_trait]
impl ActionLedger for CountingLedger {
    async fn begin(&self, record: &ActionRecord) -> adk_core::Result<()> {
        self.begun.fetch_add(1, Ordering::SeqCst);
        self.inner.begin(record).await
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> adk_core::Result<()> {
        self.completed.fetch_add(1, Ordering::SeqCst);
        self.inner.complete(key, outcome).await
    }

    async fn get(&self, key: &str) -> adk_core::Result<Option<ActionRecord>> {
        self.inner.get(key).await
    }
}

/// One tenant: its stores, kernel, runtime, model and tool counter.
pub struct Tenant {
    pub name: String,
    pub runs: Arc<dyn DurableRunStore>,
    pub service: Arc<dyn ApprovalService>,
    pub kernel: TenantKernel,
    pub runtime: DefaultManagedAgentRuntime,
    pub model: Arc<StepModel>,
    pub calls: Arc<AtomicUsize>,
    pub ledger: Arc<CountingLedger>,
}

impl Tenant {
    /// A tenant over its own in-memory stores and `sessions`, its approval service routing
    /// held calls to the operator group and waking their runs.
    pub fn new(name: &str, sessions: Arc<InMemorySessionService>, confirm: bool) -> Self {
        Self::with_state(name, sessions, confirm, Arc::new(InMemoryManagedStateStore::new()))
    }

    pub fn with_state(
        name: &str,
        sessions: Arc<InMemorySessionService>,
        confirm: bool,
        state: Arc<dyn ManagedStateStore>,
    ) -> Self {
        Self::build(name, sessions, confirm, state, fast_config(name), |builder| builder)
    }

    /// A tenant whose signals are counted in `signals` (spec 3.5 §7.10).
    #[cfg(feature = "charter")]
    pub fn with_signals(
        name: &str,
        sessions: Arc<InMemorySessionService>,
        signals: Arc<dyn adk_kernel::charter::SignalStore>,
    ) -> Self {
        let state = Arc::new(InMemoryManagedStateStore::new());
        Self::build(name, sessions, false, state, fast_config(name), |builder| {
            builder.signal_store(signals)
        })
    }

    /// A tenant whose kernel starts from the operator's `config`.
    pub fn with_config(
        name: &str,
        sessions: Arc<InMemorySessionService>,
        config: KernelConfig,
    ) -> Self {
        let state = Arc::new(InMemoryManagedStateStore::new());
        Self::build(name, sessions, false, state, config, |builder| builder)
    }

    fn build(
        name: &str,
        sessions: Arc<InMemorySessionService>,
        confirm: bool,
        state: Arc<dyn ManagedStateStore>,
        config: KernelConfig,
        extra: impl FnOnce(TenantKernelBuilder) -> TenantKernelBuilder,
    ) -> Self {
        let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
        let decisions = Arc::new(InMemoryApprovalStore::new());
        let service: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(decisions.clone())
                .with_run_store(Arc::clone(&runs))
                .with_router(
                    ApprovalPolicy::new([ApproverSelector::group(OPERATOR)]).allow_self_approval(),
                ),
        );
        let stores = ManagedStores::new(state, Arc::clone(&runs), decisions)
            .with_approval_service(Arc::clone(&service));
        let model = Arc::new(StepModel { requests: AtomicUsize::new(0) });
        let calls = Arc::new(AtomicUsize::new(0));
        let ledger = Arc::new(CountingLedger::default());
        let resolver: Arc<dyn AgentResolver> = Arc::new(TestResolver {
            model: Arc::clone(&model),
            calls: Arc::clone(&calls),
            confirm,
        });
        let builder = TenantKernel::builder(name, stores, sessions as Arc<dyn SessionService>)
            .config(config)
            .resolver(resolver)
            .action_ledger(Arc::clone(&ledger) as Arc<dyn ActionLedger>);
        let kernel = extra(builder).build().unwrap();
        let runtime = kernel.runtime(Arc::new(Models));
        Self { name: name.into(), runs, service, kernel, runtime, model, calls, ledger }
    }

    /// Every run in the tenant's run store.
    pub async fn all_runs(&self) -> Vec<RunRecord> {
        self.runs.list(RunFilter::default()).await.unwrap()
    }

    pub fn tool_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn model_calls(&self) -> usize {
        self.model.requests.load(Ordering::SeqCst)
    }
}

/// Fast polls, a short lease, no control plane.
pub fn fast_config(worker: &str) -> KernelConfig {
    KernelConfig::default()
        .with_worker(
            DurableWorkerConfig::default()
                .with_worker_id(worker)
                .with_lease_ttl(Duration::from_millis(600))
                .with_poll_interval(Duration::from_millis(20)),
        )
        .with_scheduler_tick(Duration::from_millis(50))
        .with_control(ControlPlaneConfig::default().without_socket())
}

/// Polls `probe` every 20 ms until it answers, for at most ten seconds.
pub async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
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
