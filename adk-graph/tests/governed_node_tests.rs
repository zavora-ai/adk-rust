//! Governed graph nodes: one decision per call, at most one execution per call in a
//! lineage (spec 3.3 §12 developer track, §11 S-I1).

use adk_core::{
    ActionLedger, ActionOutcome, Agent, ApprovalScope, ApprovalStore, Content, GovernanceControl,
    InMemoryActionLedger, InMemoryApprovalStore, InvocationContext, Part, PendingApproval,
    RunConfig, Session, ToolApproval, ToolEffect,
};
use adk_graph::agent::GraphAgent;
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::governed::{GovernedError, GovernedNode, effect_log};
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, Node, NodeContext, NodeOutput};
use adk_graph::record::{NodeStatus, StepKind, StepRecord};
use adk_graph::state::State;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A payment provider standing in for a real one: it counts every charge it receives.
#[derive(Default)]
struct Provider {
    hits: AtomicUsize,
    sent: Mutex<Vec<Value>>,
    /// Fail every charge.
    decline: AtomicBool,
    /// Hang after receiving a charge, as a crashed call would.
    hang: AtomicBool,
}

impl Provider {
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    async fn charge(&self, args: Value) -> adk_graph::Result<NodeOutput> {
        let id = self.hits.fetch_add(1, Ordering::SeqCst) + 1;
        self.sent.lock().unwrap().push(args.clone());
        if self.hang.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        if self.decline.load(Ordering::SeqCst) {
            return Err(GraphError::NodeExecutionFailed {
                node: "pay".to_string(),
                message: "card declined".to_string(),
            });
        }
        Ok(NodeOutput::new().with_update(
            "charge",
            json!({ "id": format!("ch_{id}"), "amountCents": args["amountCents"] }),
        ))
    }
}

/// Charges the invoice; an approver may edit the amount.
struct Pay(Arc<Provider>);

#[async_trait]
impl Node for Pay {
    fn name(&self) -> &str {
        "pay"
    }

    fn call_args(&self, ctx: &NodeContext) -> Option<Value> {
        let invoice = ctx.get("invoice")?;
        Some(json!({ "customer": invoice["customer"], "amountCents": invoice["amountCents"] }))
    }

    async fn execute(&self, ctx: &NodeContext) -> adk_graph::Result<NodeOutput> {
        self.0.charge(self.call_args(ctx).unwrap_or_default()).await
    }

    async fn run_call(&self, _ctx: &NodeContext, args: Value) -> adk_graph::Result<NodeOutput> {
        self.0.charge(args).await
    }
}

/// draft → pay (governed) → receipt.
struct Rig {
    checkpointer: Arc<MemoryCheckpointer>,
    store: Arc<InMemoryApprovalStore>,
    ledger: Arc<InMemoryActionLedger>,
    governance: GovernanceControl,
    provider: Arc<Provider>,
    graph: CompiledGraph,
}

fn invoice(amount: i64) -> Value {
    json!({ "customer": "Acme", "amountCents": amount })
}

impl Rig {
    fn new() -> Self {
        Self::with_pay(|provider| Arc::new(Pay(provider)) as Arc<dyn Node>)
    }

    fn with_pay(pay: impl FnOnce(Arc<Provider>) -> Arc<dyn Node>) -> Self {
        let checkpointer = Arc::new(MemoryCheckpointer::new());
        let provider = Arc::new(Provider::default());
        let mut graph = StateGraph::with_channels(&["invoice", "note", "charge", "receipt"])
            .add_node_fn("draft", |_ctx| async {
                Ok(NodeOutput::new().with_update("invoice", invoice(4200)))
            })
            .add_node_fn("receipt", |ctx| async move {
                Ok(NodeOutput::new()
                    .with_update("receipt", ctx.get("charge").cloned().unwrap_or_default()))
            })
            .add_edge(START, "draft")
            .add_edge("draft", "pay")
            .add_edge("pay", "receipt")
            .add_edge("receipt", END);
        graph.nodes.insert("pay".to_string(), pay(Arc::clone(&provider)));
        let graph = graph
            .compile()
            .unwrap()
            .with_checkpointer_arc(checkpointer.clone())
            .with_node_effect("pay", ToolEffect::NonIdempotent)
            .with_governed_node("pay", GovernedNode::new().with_approval().spending_money());
        Self {
            checkpointer,
            store: Arc::new(InMemoryApprovalStore::new()),
            ledger: Arc::new(InMemoryActionLedger::new()),
            governance: GovernanceControl::new(),
            provider,
            graph,
        }
    }

    fn run_config(&self) -> RunConfig {
        RunConfig::builder()
            .approval_store(self.store.clone())
            .action_ledger(self.ledger.clone())
            .governance(self.governance.clone())
            .build()
    }

    async fn invoke(&self, thread: &str) -> adk_graph::Result<State> {
        self.graph
            .invoke_with_run_config(State::new(), ExecutionConfig::new(thread), self.run_config())
            .await
    }

    fn scope(thread: &str) -> ApprovalScope {
        ApprovalScope::new("graph_app", "graph_user", thread)
    }

    async fn pending(&self, thread: &str) -> Vec<PendingApproval> {
        self.store.pending(&Self::scope(thread)).await.unwrap()
    }

    async fn decide(&self, thread: &str, approval: ToolApproval) {
        let pending = self.pending(thread).await;
        assert_eq!(pending.len(), 1, "one decision waits on {thread}");
        self.store.decide(&Self::scope(thread), &pending[0].fingerprint, approval).await.unwrap();
    }

    /// Runs `thread` to a pause, answers it, and runs it again.
    async fn approved(&self, thread: &str, approval: ToolApproval) -> adk_graph::Result<State> {
        assert!(matches!(self.invoke(thread).await, Err(GraphError::Interrupted(_))));
        self.decide(thread, approval).await;
        self.invoke(thread).await
    }

    async fn steps(&self, thread: &str) -> Vec<StepRecord> {
        let mut steps = Vec::new();
        for checkpoint in self.checkpointer.list(thread).await.unwrap() {
            steps.extend(checkpoint.step_records().unwrap());
        }
        steps
    }

    /// The pay node's record in each step of `thread`: (step, kind, status).
    async fn pay_steps(&self, thread: &str) -> Vec<(usize, StepKind, NodeStatus)> {
        self.steps(thread)
            .await
            .into_iter()
            .filter_map(|step| {
                let pay = step.nodes.iter().find(|node| node.node == "pay")?;
                Some((step.step, step.kind, pay.status))
            })
            .collect()
    }

    async fn pay_key(&self, thread: &str) -> String {
        self.steps(thread)
            .await
            .into_iter()
            .flat_map(|step| step.nodes)
            .filter(|node| node.node == "pay")
            .find_map(|node| node.effect_key)
            .expect("pay has an effect key")
    }

    /// Forks `thread` at its checkpoint after draft, overwriting the invoice if given.
    #[cfg(feature = "time-travel")]
    async fn fork(&self, thread: &str, to: &str, invoice: Option<Value>) {
        let after_draft = self
            .checkpointer
            .list(thread)
            .await
            .unwrap()
            .into_iter()
            .find(|checkpoint| checkpoint.pending_nodes == ["pay"] && !checkpoint.is_paused())
            .expect("a completed checkpoint before pay");
        let mut spec = adk_graph::time_travel::ForkSpec::new(&after_draft.checkpoint_id, to)
            .set("note", json!("re-run for audit"));
        if let Some(invoice) = invoice {
            spec = spec.set("invoice", invoice);
        }
        self.graph.time_travel(thread).unwrap().fork(spec).await.unwrap();
    }
}

fn code(result: &adk_graph::Result<State>) -> Option<&'static str> {
    result.as_ref().err().and_then(GovernedError::code_of)
}

#[tokio::test]
async fn a_hold_records_one_decision_with_the_full_call_and_an_approval_runs_it_once() {
    let rig = Rig::new();
    let paused = rig.invoke("t1").await;
    let Err(GraphError::Interrupted(interrupted)) = paused else { panic!("expected a pause") };
    let pause = adk_graph::GraphToolConfirmationPause::from_interrupted_execution(&interrupted)
        .expect("a confirmation pause");
    assert_eq!(rig.provider.hits(), 0);

    let pending = rig.pending("t1").await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request, pause.request);
    assert_eq!(
        (pending[0].request.tool_name.as_str(), &pending[0].request.args),
        ("pay", &invoice(4200))
    );
    let origin = pending[0].request.origin.as_ref().expect("an origin");
    assert_eq!(origin.source, "graph:graph");

    rig.decide("t1", ToolApproval::approve()).await;
    let state = rig.invoke("t1").await.unwrap();
    assert_eq!(rig.provider.hits(), 1);
    assert_eq!(state["receipt"], json!({ "id": "ch_1", "amountCents": 4200 }));
    assert!(rig.pending("t1").await.is_empty());

    let key = rig.pay_key("t1").await;
    let digest = adk_core::json_digest(&invoice(4200));
    assert_eq!(key, format!("graph/urn:adk:agent:graph_app/graph/t1/pay/1/{digest}"));
    let record = rig.ledger.get(&key).await.unwrap().expect("a ledger record");
    assert!(matches!(record.outcome, Some(ActionOutcome::Succeeded { .. })));
    let effects = effect_log(rig.checkpointer.as_ref(), "t1").await.unwrap();
    assert_eq!(
        effects.iter().map(|e| (e.key.clone(), e.writes.clone())).collect::<Vec<_>>(),
        vec![(key, [("charge".to_string(), json!({ "id": "ch_1", "amountCents": 4200 }))].into())]
    );
    assert_eq!(
        rig.pay_steps("t1").await,
        vec![(2, StepKind::Paused, NodeStatus::Paused), (2, StepKind::Completed, NodeStatus::Done)]
    );

    // The finished thread does nothing more.
    rig.invoke("t1").await.unwrap();
    assert_eq!(rig.provider.hits(), 1);
}

#[tokio::test]
async fn a_denial_runs_nothing_and_the_next_run_asks_again() {
    let rig = Rig::new();
    let denied = rig.approved("t1", ToolApproval::deny()).await;
    assert_eq!(code(&denied), Some("graph.refused"));
    assert_eq!(rig.provider.hits(), 0);
    assert_eq!(
        rig.pay_steps("t1").await,
        vec![(2, StepKind::Paused, NodeStatus::Paused), (2, StepKind::Failed, NodeStatus::Refused)]
    );

    // A refused call asks again, and a yes runs it once.
    let state = rig.approved("t1", ToolApproval::approve()).await.unwrap();
    assert_eq!((rig.provider.hits(), &state["charge"]["amountCents"]), (1, &json!(4200)));
}

#[tokio::test]
async fn an_edited_amount_is_sent_once_as_edited() {
    let rig = Rig::new();
    let state =
        rig.approved("t1", ToolApproval::approve().with_edited_args(invoice(5000))).await.unwrap();
    assert_eq!(rig.provider.hits(), 1);
    assert_eq!(*rig.provider.sent.lock().unwrap(), vec![invoice(5000)]);
    assert_eq!(state["charge"]["amountCents"], json!(5000));
}

#[tokio::test]
async fn a_node_that_cannot_run_an_edited_call_refuses_it_and_asks_again() {
    // A function node has no `run_call`, so an edit cannot be honoured.
    let rig = Rig::with_pay(|provider| {
        Arc::new(adk_graph::node::FunctionNode::new("pay", move |ctx| {
            let provider = Arc::clone(&provider);
            async move { provider.charge(ctx.get("invoice").cloned().unwrap_or_default()).await }
        })) as Arc<dyn Node>
    });
    let edited = rig
        .approved("t1", ToolApproval::approve().with_edited_args(json!({ "anything": 1 })))
        .await;
    assert_eq!(code(&edited), Some("graph.edit_unsupported"));
    assert_eq!(rig.provider.hits(), 0, "neither the edited nor the original call ran");
    assert_eq!(rig.pay_steps("t1").await.last(), Some(&(2, StepKind::Failed, NodeStatus::Refused)));

    rig.approved("t1", ToolApproval::approve()).await.unwrap();
    assert_eq!(rig.provider.hits(), 1);
}

#[tokio::test]
async fn a_call_that_crashed_mid_flight_is_never_run_again() {
    let rig = Rig::new();
    assert!(rig.invoke("t1").await.is_err());
    rig.decide("t1", ToolApproval::approve()).await;
    rig.provider.hang.store(true, Ordering::SeqCst);
    // The process dies while the provider holds the charge.
    let crashed = tokio::time::timeout(Duration::from_millis(300), rig.invoke("t1")).await;
    assert!(crashed.is_err(), "the run was cut off mid-call");
    assert_eq!(rig.provider.hits(), 1);

    rig.provider.hang.store(false, Ordering::SeqCst);
    let resumed = rig.invoke("t1").await;
    assert_eq!(code(&resumed), Some("graph.outcome_unknown"));
    assert_eq!(rig.provider.hits(), 1);
    assert!(rig.pending("t1").await.is_empty(), "no new decision is asked for");
}

#[cfg(feature = "time-travel")]
#[tokio::test]
async fn a_fork_with_the_same_call_is_answered_from_its_record() {
    let rig = Rig::new();
    let original = rig.approved("t1", ToolApproval::approve()).await.unwrap();
    let t1_before = rig.checkpointer.list("t1").await.unwrap();

    rig.fork("t1", "t2", None).await;
    let forked = rig.invoke("t2").await.unwrap();
    assert_eq!(rig.provider.hits(), 1, "not sent again");
    assert!(rig.pending("t2").await.is_empty(), "no decision was created");
    assert_eq!(forked["charge"], original["charge"]);
    assert_eq!(forked["note"], json!("re-run for audit"));
    assert_eq!(rig.pay_steps("t2").await, vec![(2, StepKind::Completed, NodeStatus::Recorded)]);
    assert_eq!(rig.pay_key("t2").await, rig.pay_key("t1").await);
    assert_eq!(rig.checkpointer.list("t1").await.unwrap(), t1_before, "the source is unchanged");
}

#[cfg(feature = "time-travel")]
#[tokio::test]
async fn a_changed_call_waits_and_a_sibling_with_the_same_change_reads_the_effect_log() {
    let rig = Rig::new();
    rig.approved("t1", ToolApproval::approve()).await.unwrap();

    rig.fork("t1", "t2", Some(invoice(5000))).await;
    let changed = rig.approved("t2", ToolApproval::approve()).await.unwrap();
    assert_eq!(rig.provider.hits(), 2);
    assert_eq!(changed["charge"], json!({ "id": "ch_2", "amountCents": 5000 }));

    rig.fork("t1", "t3", Some(invoice(5000))).await;
    let sibling = rig.invoke("t3").await.unwrap();
    assert_eq!(rig.provider.hits(), 2);
    assert_eq!(sibling["charge"], changed["charge"]);
    assert_eq!(rig.pay_steps("t3").await, vec![(2, StepKind::Completed, NodeStatus::Recorded)]);
}

#[tokio::test]
async fn a_recorded_failure_is_not_run_again() {
    let rig = Rig::new();
    rig.provider.decline.store(true, Ordering::SeqCst);
    let declined = rig.approved("t1", ToolApproval::approve()).await;
    assert!(declined.unwrap_err().to_string().contains("card declined"));
    assert_eq!(rig.provider.hits(), 1);

    rig.provider.decline.store(false, Ordering::SeqCst);
    let again = rig.invoke("t1").await;
    assert_eq!(code(&again), Some("graph.recorded_failure"));
    assert!(again.unwrap_err().to_string().contains("card declined"));
    assert_eq!(rig.provider.hits(), 1);
    assert!(rig.pending("t1").await.is_empty());
}

#[cfg(feature = "time-travel")]
#[tokio::test]
async fn a_frozen_kill_switch_refuses_before_the_ledger() {
    let rig = Rig::new();
    rig.approved("t1", ToolApproval::approve()).await.unwrap();
    rig.fork("t1", "t2", None).await;

    rig.governance.freeze("incident 42");
    // The fork's call is in the ledger, and still the switch answers first.
    let frozen = rig.invoke("t2").await;
    assert_eq!(code(&frozen), Some("graph.frozen"));
    assert_eq!(rig.pay_steps("t2").await, vec![(2, StepKind::Failed, NodeStatus::Refused)]);

    let fresh = rig.invoke("t3").await;
    assert_eq!(code(&fresh), Some("graph.frozen"));
    assert!(rig.pending("t3").await.is_empty());
    assert_eq!(rig.provider.hits(), 1);
}

#[tokio::test]
async fn without_a_gate_a_governed_node_is_not_run() {
    let rig = Rig::new();
    let ungated = rig.graph.invoke(State::new(), ExecutionConfig::new("t1")).await;
    assert_eq!(code(&ungated), Some("graph.not_run"));
    assert_eq!(rig.provider.hits(), 0);
    assert_eq!(rig.pay_steps("t1").await, vec![(2, StepKind::Failed, NodeStatus::NotRun)]);
}

#[tokio::test]
async fn a_governed_node_cannot_be_invoked_as_a_child() {
    let rig = Rig::new();
    let ctx = NodeContext::new(State::new(), ExecutionConfig::new("t1"), 0);
    // Outside the executor there is no invoker at all; inside it the governed node is
    // missing from the invoker, which this checks through a node body.
    assert!(ctx.run_node("pay", json!({})).await.is_err());
    let graph = StateGraph::with_channels(&["out"])
        .add_node_fn("caller", |ctx| async move {
            let outcome = ctx.run_node("pay", json!({})).await;
            Ok(NodeOutput::new().with_update("out", json!(outcome.is_err())))
        })
        .add_node(Pay(Arc::clone(&rig.provider)))
        .add_edge(START, "caller")
        .add_edge("caller", END)
        .compile()
        .unwrap()
        .with_governed_node("pay", GovernedNode::new());
    let state = graph.invoke(State::new(), ExecutionConfig::new("c1")).await.unwrap();
    assert_eq!((state["out"].clone(), rig.provider.hits()), (json!(true), 0));
}

// ── Through a GraphAgent ─────────────────────────────────────────────

struct Turn {
    agent: String,
    invocation: String,
    content: Content,
    run_config: RunConfig,
}

struct TurnSession;

impl Session for TurnSession {
    fn id(&self) -> &str {
        "s1"
    }
    fn app_name(&self) -> &str {
        "shop"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn state(&self) -> &dyn adk_core::State {
        &NoState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct NoState;

impl adk_core::State for NoState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

impl adk_core::ReadonlyContext for Turn {
    fn invocation_id(&self) -> &str {
        &self.invocation
    }
    fn agent_name(&self) -> &str {
        &self.agent
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn app_name(&self) -> &str {
        "shop"
    }
    fn session_id(&self) -> &str {
        "s1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl adk_core::CallbackContext for Turn {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for Turn {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not exercised")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &TurnSession
    }
    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

fn turn(agent: &str, invocation: &str, run_config: RunConfig) -> Arc<dyn InvocationContext> {
    Arc::new(Turn {
        agent: agent.to_string(),
        invocation: invocation.to_string(),
        content: Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "bill acme".into() }],
        },
        run_config,
    })
}

async fn drain(agent: &GraphAgent, ctx: Arc<dyn InvocationContext>) -> Vec<adk_core::Event> {
    let mut events = agent.run(ctx).await.unwrap();
    let mut out = Vec::new();
    while let Some(event) = events.next().await {
        out.push(event.unwrap());
    }
    out
}

#[tokio::test]
async fn under_a_graph_agent_the_hold_names_the_agent_and_an_approval_turn_runs_it_once() {
    let rig = Rig::new();
    let agent = GraphAgent::from_graph(
        "invoice-run",
        Rig::with_pay(|_| Arc::new(Pay(Arc::clone(&rig.provider))) as Arc<dyn Node>).graph,
    );
    let scope = ApprovalScope::new("shop", "alice", "s1");

    let events = drain(&agent, turn("invoice-run", "inv-1", rig.run_config())).await;
    let request = events
        .iter()
        .find_map(|event| event.actions.tool_confirmation.clone())
        .expect("the turn reports the hold");
    assert_eq!(request.origin.as_ref().map(|o| o.source.as_str()), Some("graph:invoice-run"));
    assert_eq!(
        request.acting.as_ref().map(|a| a.agent.id.as_str()),
        Some("urn:adk:agent:shop/invoice-run")
    );
    let pending = rig.store.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1);
    rig.store.decide(&scope, &pending[0].fingerprint, ToolApproval::approve()).await.unwrap();

    drain(&agent, turn("invoice-run", "inv-2", rig.run_config())).await;
    assert_eq!(rig.provider.hits(), 1);
}

#[tokio::test]
async fn two_graph_agents_in_one_session_never_share_a_record() {
    let provider = Arc::new(Provider::default());
    let ledger = Arc::new(InMemoryActionLedger::new());
    let run_config = RunConfig::builder().action_ledger(ledger.clone()).build();
    let mut keys = Vec::new();
    for name in ["billing-a", "billing-b"] {
        let checkpointer = Arc::new(MemoryCheckpointer::new());
        let graph = StateGraph::with_channels(&["invoice", "charge"])
            .add_node_fn("draft", |_ctx| async {
                Ok(NodeOutput::new().with_update("invoice", invoice(4200)))
            })
            .add_node(Pay(Arc::clone(&provider)))
            .add_edge(START, "draft")
            .add_edge("draft", "pay")
            .add_edge("pay", END)
            .compile()
            .unwrap()
            .with_checkpointer_arc(checkpointer.clone())
            .with_governed_node("pay", GovernedNode::new());
        let agent = GraphAgent::from_graph(name, graph);
        drain(&agent, turn(name, "inv-1", run_config.clone())).await;
        let thread = adk_graph::agent::session_thread_id("shop", "alice", "s1");
        let steps: Vec<StepRecord> = checkpointer
            .list(&thread)
            .await
            .unwrap()
            .iter()
            .flat_map(|c| c.step_records().unwrap())
            .collect();
        keys.extend(steps.into_iter().flat_map(|s| s.nodes).filter_map(|n| n.effect_key));
    }
    assert_eq!(provider.hits(), 2, "each agent's call ran");
    assert_eq!(keys.len(), 2);
    assert!(keys[0].starts_with("graph/urn:adk:agent:shop/billing-a/"));
    assert!(keys[1].starts_with("graph/urn:adk:agent:shop/billing-b/"));
}

#[tokio::test]
async fn the_messages_stream_runs_a_governed_node_through_its_bracket() {
    use adk_graph::stream::{StreamEvent, StreamMode};
    let rig = Rig::new();
    let first: Vec<_> = rig
        .graph
        .stream_with_run_config(
            State::new(),
            ExecutionConfig::new("t1"),
            StreamMode::Messages,
            rig.run_config(),
        )
        .collect()
        .await;
    assert!(first.iter().any(|event| {
        event
            .as_ref()
            .ok()
            .and_then(adk_graph::GraphToolConfirmationPause::from_stream_event)
            .is_some()
    }));
    assert_eq!(rig.provider.hits(), 0);

    rig.decide("t1", ToolApproval::approve()).await;
    let second: Vec<_> = rig
        .graph
        .stream_with_run_config(
            State::new(),
            ExecutionConfig::new("t1"),
            StreamMode::Messages,
            rig.run_config(),
        )
        .collect()
        .await;
    assert!(second.iter().any(|event| matches!(event, Ok(StreamEvent::Done { .. }))));
    assert_eq!(rig.provider.hits(), 1);
    assert_eq!(
        rig.pay_steps("t1").await,
        vec![(2, StepKind::Paused, NodeStatus::Paused), (2, StepKind::Completed, NodeStatus::Done)]
    );

    // Without a gate the stream refuses the node too.
    let ungated: Vec<_> = rig
        .graph
        .stream(State::new(), ExecutionConfig::new("t2"), StreamMode::Messages)
        .collect()
        .await;
    let error = ungated.into_iter().find_map(Result::err).expect("the stream fails");
    assert_eq!(GovernedError::code_of(&error), Some("graph.not_run"));
    assert_eq!(rig.provider.hits(), 1);
}
