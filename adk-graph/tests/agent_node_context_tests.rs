//! An agent must not lose its runtime when it runs inside a graph.
//!
//! `AgentNode` built a `GraphInvocationContext` from scratch for every run: it
//! hardcoded `user_id = "graph_user"`, `app_name = "graph_app"`, and branch `main`,
//! used a default `RunConfig`, and returned `None` for artifacts and memory. Every
//! optional capability fell back to its default, so secrets, shared state,
//! cancellation, scopes, and request metadata all disappeared. An identity-dependent
//! tool therefore saw a synthetic principal inside a graph and the real one outside
//! it, and `Runner::interrupt` could not reach the agent at all.

use adk_core::{
    Agent, Content, EventStream, InvocationContext, Part, Result, RunConfig, SecretRequest,
    Session, State,
};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{AgentNode, ExecutionConfig};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// What the agent observed about the context it was handed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Observed {
    user_id: String,
    app_name: String,
    branch: String,
    scopes: Vec<String>,
    metadata_keys: Vec<String>,
    secret: Option<String>,
    cancelled: bool,
    has_memory: bool,
}

/// An agent that records the capabilities of the context it runs under.
struct ObservingAgent {
    observed: Arc<Mutex<Option<Observed>>>,
}

#[async_trait]
impl Agent for ObservingAgent {
    fn name(&self) -> &str {
        "observer"
    }
    fn description(&self) -> &str {
        "records what its context exposes"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let mut metadata_keys: Vec<String> = ctx.request_metadata().into_keys().collect();
        metadata_keys.sort();
        let observed = Observed {
            user_id: ctx.user_id().to_string(),
            app_name: ctx.app_name().to_string(),
            branch: ctx.branch().to_string(),
            scopes: ctx.user_scopes(),
            metadata_keys,
            secret: ctx.get_secret("api-key").await?,
            cancelled: ctx.is_cancelled(),
            has_memory: ctx.memory().is_some(),
        };
        *self.observed.lock().unwrap() = Some(observed);
        Ok(Box::pin(futures::stream::empty()))
    }
}

// ── A capability-rich sentinel context ────────────────────────────────

struct SentinelState;

impl State for SentinelState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct SentinelSession;

impl Session for SentinelSession {
    fn id(&self) -> &str {
        "caller-session"
    }
    fn app_name(&self) -> &str {
        "caller-app"
    }
    fn user_id(&self) -> &str {
        "caller-user"
    }
    fn state(&self) -> &dyn State {
        &SentinelState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct SentinelMemory;

#[async_trait]
impl adk_core::Memory for SentinelMemory {
    async fn search(&self, _query: &str) -> Result<Vec<adk_core::MemoryEntry>> {
        Ok(Vec::new())
    }
}

/// A context with a non-default value for every capability under test.
struct SentinelContext {
    user_content: Content,
    session: SentinelSession,
    cancelled: bool,
}

impl SentinelContext {
    fn new(cancelled: bool) -> Self {
        Self {
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "go".to_string() }],
            },
            session: SentinelSession,
            cancelled,
        }
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for SentinelContext {
    fn invocation_id(&self) -> &str {
        "caller-invocation"
    }
    fn agent_name(&self) -> &str {
        "caller-agent"
    }
    fn user_id(&self) -> &str {
        "caller-user"
    }
    fn app_name(&self) -> &str {
        "caller-app"
    }
    fn session_id(&self) -> &str {
        "caller-session"
    }
    fn branch(&self) -> &str {
        "caller-branch"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl adk_core::CallbackContext for SentinelContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for SentinelContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not exercised")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        Some(Arc::new(SentinelMemory))
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        static RUN_CONFIG: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        RUN_CONFIG.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled
    }
    fn user_scopes(&self) -> Vec<String> {
        vec!["tools:read".to_string(), "secrets:api".to_string()]
    }
    fn request_metadata(&self) -> HashMap<String, Value> {
        HashMap::from([("tenant".to_string(), json!("acme"))])
    }
    async fn get_secret(&self, _name: &str) -> Result<Option<String>> {
        Ok(Some("sentinel-secret".to_string()))
    }
    async fn get_secret_for(&self, _request: &SecretRequest) -> Result<Option<String>> {
        Ok(Some("sentinel-secret".to_string()))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Runs the observing agent through an `AgentNode`, with or without a parent.
async fn run_in_graph(parent: Option<Arc<dyn InvocationContext>>) -> Observed {
    let observed = Arc::new(Mutex::new(None));
    let agent = Arc::new(ObservingAgent { observed: observed.clone() });

    let node = AgentNode::new(agent)
        .with_input_mapper(|_state| Content::new("user").with_text("go"))
        .with_output_mapper(|_events| HashMap::new());

    let graph = StateGraph::with_channels(&["value"])
        .add_node(node)
        .add_edge(START, "observer")
        .add_edge("observer", END)
        .compile()
        .unwrap();

    let mut config = ExecutionConfig::new("thread-1");
    if let Some(parent) = parent {
        config = config.with_parent_context(parent);
    }
    graph.invoke(adk_graph::state::State::new(), config).await.unwrap();

    observed.lock().unwrap().clone().expect("the agent must have run")
}

// ── Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_graph_run_preserves_the_callers_identity_and_services() {
    let observed = run_in_graph(Some(Arc::new(SentinelContext::new(false)))).await;

    assert_eq!(observed.user_id, "caller-user", "the synthetic user replaced the real one");
    assert_eq!(observed.app_name, "caller-app", "the synthetic app replaced the real one");
    assert_eq!(
        observed.scopes,
        vec!["tools:read".to_string(), "secrets:api".to_string()],
        "scope checks inside a graph could not see the caller's grants"
    );
    assert_eq!(observed.metadata_keys, vec!["tenant".to_string()]);
    assert_eq!(
        observed.secret.as_deref(),
        Some("sentinel-secret"),
        "secret access disappeared inside the graph"
    );
    assert!(observed.has_memory, "memory access disappeared inside the graph");
}

#[tokio::test]
async fn cancellation_reaches_an_agent_inside_a_graph() {
    let observed = run_in_graph(Some(Arc::new(SentinelContext::new(true)))).await;
    assert!(
        observed.cancelled,
        "Runner::interrupt could not reach an agent running as a graph node"
    );
}

#[tokio::test]
async fn a_node_runs_on_its_own_branch_below_the_caller() {
    // Derived rather than inherited, so events a node produces are attributable and do
    // not read as the calling agent's own turn.
    let observed = run_in_graph(Some(Arc::new(SentinelContext::new(false)))).await;
    assert_eq!(observed.branch, "caller-branch.observer");
}

#[tokio::test]
async fn standalone_execution_still_works_without_a_parent() {
    // A graph invoked outside a Runner has no invocation to inherit. That is a
    // deliberate mode, not an accident, so the synthetic identity stays.
    let observed = run_in_graph(None).await;

    assert_eq!(observed.user_id, "graph_user");
    assert_eq!(observed.app_name, "graph_app");
    assert_eq!(observed.branch, "main");
    assert!(observed.scopes.is_empty());
    assert_eq!(observed.secret, None);
    assert!(!observed.has_memory);
    assert!(!observed.cancelled);
}
