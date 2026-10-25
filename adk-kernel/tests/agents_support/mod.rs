//! Fixtures for the agent table's tests (spec 3.3 §5.7, §10 K7): a factory over a scripted
//! model, a kernel over in-memory stores with an inbox and an audit trail, and callers.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::audit_log::{AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{
    AgentId, ApprovalService, DurableRunStore, GovernanceControl, InMemoryApprovalStore,
    InMemoryRunStore, Llm, RunRecord, RunState, StoreApprovalService,
};
use adk_kernel::agents::{
    AgentAction, AgentAssessment, AgentCaller, AgentCeiling, AgentChoices, AgentDefinition,
    AgentFactory, AgentRecord, AgentStore, AgentUses, HostedAgent, ModelChoice, Presence,
};
use adk_kernel::stores::InMemoryAgentStore;
use adk_kernel::{
    InMemoryProcessStore, Kernel, KernelConfig, KernelError, ProcessStore, ProgressSource,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;

pub const APP: &str = "adk-rust";
pub const PERSON: &str = "alice";

/// A runner whose root agent `name` answers every turn, under `control`.
pub fn runner(
    name: &str,
    sessions: &Arc<InMemorySessionService>,
    control: &GovernanceControl,
) -> Arc<Runner> {
    let replies: Vec<ScriptedResponse> = (0..32).map(|_| ScriptedResponse::text("done")).collect();
    let model = Arc::new(ScriptedLlm::new(format!("{name}-model"), replies));
    let agent = LlmAgentBuilder::new(name).model(model as Arc<dyn Llm>).build().unwrap();
    Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(Arc::clone(sessions) as Arc<dyn SessionService>)
            .governance(control.clone())
            .build()
            .unwrap(),
    )
}

/// A progress source whose loop-guard verdict names its agent, so a process view shows
/// which source answered for it.
pub struct NamedProgress(pub String);

#[async_trait]
impl ProgressSource for NamedProgress {
    async fn loop_guard(&self, _run: &RunRecord) -> adk_kernel::Result<Option<String>> {
        Ok(Some(format!("guard of {}", self.0)))
    }
}

/// A factory that builds every agent as a scripted runner sharing the kernel's sessions.
pub struct TestFactory {
    pub control: GovernanceControl,
    pub sessions: Arc<InMemorySessionService>,
    /// Builds every runner under a control of its own.
    pub foreign: AtomicBool,
    /// Ids whose build fails as a missing key does.
    pub broken: Mutex<BTreeSet<String>>,
    /// What a suggesting agent may give; `None` refuses every suggestion.
    pub ceiling: Mutex<Option<AgentCeiling>>,
    /// Ids retired, in order.
    pub retired: Mutex<Vec<String>>,
    pub builds: AtomicUsize,
}

impl TestFactory {
    pub fn new(control: &GovernanceControl, sessions: &Arc<InMemorySessionService>) -> Self {
        Self {
            control: control.clone(),
            sessions: Arc::clone(sessions),
            foreign: AtomicBool::new(false),
            broken: Mutex::default(),
            ceiling: Mutex::new(Some(AgentCeiling::new(
                adk_kernel::PartitionMode::Ask,
                AgentUses::default()
                    .with_files(adk_kernel::agents::FileAccess::ReadWrite)
                    .with_web(adk_kernel::agents::WebAccess::Any),
            ))),
            retired: Mutex::default(),
            builds: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl AgentFactory for TestFactory {
    fn assess(&self, record: &AgentRecord) -> AgentAssessment {
        AgentAssessment::new(adk_core::ApprovalRisk::Low)
            .with_ability(format!("{} answers in its own threads.", record.display_name))
    }

    async fn ceiling(
        &self,
        _agent: &AgentId,
        _session: &str,
    ) -> adk_kernel::Result<Option<AgentCeiling>> {
        Ok(self.ceiling.lock().unwrap().clone())
    }

    async fn choices(&self) -> adk_kernel::Result<AgentChoices> {
        Ok(AgentChoices::default().with_model(ModelChoice::new("anthropic", "claude-sonnet-5-5")))
    }

    async fn build(&self, record: &AgentRecord) -> adk_kernel::Result<HostedAgent> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        if self.broken.lock().unwrap().contains(&record.id) {
            return Err(KernelError::Config { reason: "No key for Anthropic".into() });
        }
        let control = if self.foreign.load(Ordering::SeqCst) {
            GovernanceControl::new()
        } else {
            self.control.clone()
        };
        Ok(HostedAgent::new(runner(&record.id, &self.sessions, &control))
            .with_progress(Arc::new(NamedProgress(record.id.clone()))))
    }

    async fn retired(&self, record: &AgentRecord) -> adk_kernel::Result<u64> {
        self.retired.lock().unwrap().push(record.id.clone());
        Ok(2)
    }

    fn reserved_names(&self) -> Vec<String> {
        vec!["ADK-Rust".to_string()]
    }
}

/// The stores one test's kernels share.
pub struct World {
    pub runs: Arc<dyn DurableRunStore>,
    pub processes: Arc<InMemoryProcessStore>,
    pub sessions: Arc<InMemorySessionService>,
    pub approvals: Arc<dyn ApprovalService>,
    pub audit: Arc<InMemoryAuditRecorder>,
    pub store: Arc<dyn AgentStore>,
    pub control: GovernanceControl,
    pub factory: Arc<TestFactory>,
}

impl World {
    pub fn new() -> Self {
        let control = GovernanceControl::new();
        let sessions = Arc::new(InMemorySessionService::new());
        Self {
            runs: Arc::new(InMemoryRunStore::new()),
            processes: Arc::new(InMemoryProcessStore::new()),
            approvals: Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))),
            audit: Arc::new(InMemoryAuditRecorder::new()),
            store: Arc::new(InMemoryAgentStore::new()),
            factory: Arc::new(TestFactory::new(&control, &sessions)),
            sessions,
            control,
        }
    }

    /// A kernel replica over the world, with fast polls and no control plane.
    pub fn kernel(&self, name: &str) -> Kernel {
        self.kernel_with(name, |builder| builder)
    }

    pub fn kernel_with(
        &self,
        name: &str,
        extend: impl FnOnce(adk_kernel::KernelBuilder) -> adk_kernel::KernelBuilder,
    ) -> Kernel {
        let control = adk_kernel::control::ControlPlaneConfig::default().without_socket();
        self.kernel_with_control(name, control, extend)
    }

    /// A kernel replica serving `control`.
    pub fn kernel_with_control(
        &self,
        name: &str,
        control: adk_kernel::control::ControlPlaneConfig,
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
            .with_governance(self.control.clone())
            .with_control(control);
        let builder = Kernel::builder()
            .config(config)
            .run_store(Arc::clone(&self.runs))
            .process_store(Arc::clone(&self.processes) as Arc<dyn ProcessStore>)
            .session_service(Arc::clone(&self.sessions) as Arc<dyn SessionService>)
            .approval_service(Arc::clone(&self.approvals))
            .audit(AuditTrail::new(self.audit.clone()))
            .agent(APP, runner(APP, &self.sessions, &self.control))
            .agent_store(Arc::clone(&self.store))
            .agent_factory(Arc::clone(&self.factory) as Arc<dyn AgentFactory>);
        extend(builder).build().unwrap()
    }

    /// The `kernel.agent.*` records written so far, in order.
    pub fn agent_records(&self) -> Vec<AuditRecordDraft> {
        self.audit
            .records()
            .into_iter()
            .filter(|record| record.action.starts_with("kernel.agent."))
            .collect()
    }

    /// The actions of the `kernel.agent.*` records about `id`, in order.
    pub fn actions_about(&self, id: &str) -> Vec<String> {
        self.agent_records()
            .into_iter()
            .filter(|record| record.ext.get("id").and_then(|v| v.as_str()) == Some(id))
            .map(|record| record.action)
            .collect()
    }
}

/// The person on the kernel's own terminal.
pub fn local() -> AgentCaller {
    AgentCaller::new(SubjectRef::human(PERSON), "terminal", "keyboard")
        .with_presence(Presence::PairedLocal)
        .with_transport(adk_kernel::control::ControlTransport::Socket)
        .with_peer_pid(4242)
}

/// The person in the web home, without presence.
pub fn web() -> AgentCaller {
    AgentCaller::new(SubjectRef::human(PERSON), "web", "pointer")
}

/// `caller` previews and applies `action`.
pub async fn apply(
    kernel: &Kernel,
    caller: &AgentCaller,
    action: AgentAction,
) -> adk_kernel::Result<adk_kernel::agents::AgentOutcome> {
    let preview = kernel.agents().preview(caller, &action).await?;
    kernel.agents().apply(caller, action, &preview.digest).await
}

/// Adds `definition` by hand from the local terminal.
pub async fn add(kernel: &Kernel, definition: AgentDefinition) -> AgentRecord {
    match apply(kernel, &local(), AgentAction::Create { definition, replaces: None }).await {
        Ok(adk_kernel::agents::AgentOutcome::Created { agent }) => agent,
        other => panic!("not added: {other:?}"),
    }
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
