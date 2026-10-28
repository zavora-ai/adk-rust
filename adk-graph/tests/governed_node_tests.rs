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
    // A standalone run acts as the standalone graph agent, and its decision key names it
    // (#1210).
    let agent = "urn:adk:agent:graph_app/graph";
    assert_eq!(pending[0].request.acting.as_ref().map(|a| a.agent.id.as_str()), Some(agent));
    assert_eq!(
        adk_core::ToolConfirmationRequest::fingerprint_agent(&pending[0].fingerprint),
        Some(agent)
    );

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

/// A confirmation handler that never answers in time, so each call it is asked about
/// waits in the approval store.
#[derive(Debug, Default)]
struct SlowHandler(AtomicUsize);

#[async_trait]
impl adk_core::ToolConfirmationHandler for SlowHandler {
    async fn decide(
        &self,
        _request: &adk_core::ToolConfirmationRequest,
    ) -> adk_core::Result<adk_core::ToolConfirmationDecision> {
        self.0.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(adk_core::ToolConfirmationDecision::Deny)
    }
}

#[tokio::test]
async fn a_waiting_call_resumes_on_its_decision_and_never_asks_again() {
    let rig = Rig::new();
    let handler = Arc::new(SlowHandler::default());
    let run_config = RunConfig::builder()
        .approval_store(rig.store.clone())
        .action_ledger(rig.ledger.clone())
        .tool_confirmation_handler(handler.clone())
        .tool_confirmation_timeout(Duration::from_millis(50))
        .build();
    let invoke = || {
        rig.graph.invoke_with_run_config(
            State::new(),
            ExecutionConfig::new("t1"),
            run_config.clone(),
        )
    };

    // The handler did not answer in time, so the call waits in the store.
    assert!(matches!(invoke().await, Err(GraphError::Interrupted(_))));
    assert_eq!((handler.0.load(Ordering::SeqCst), rig.pending("t1").await.len()), (1, 1));

    // A turn before anyone decides keeps waiting, without asking anyone again.
    assert!(matches!(invoke().await, Err(GraphError::Interrupted(_))));
    assert_eq!((handler.0.load(Ordering::SeqCst), rig.pending("t1").await.len()), (1, 1));
    assert_eq!(rig.provider.hits(), 0);

    rig.decide("t1", ToolApproval::approve()).await;
    invoke().await.unwrap();
    assert_eq!((handler.0.load(Ordering::SeqCst), rig.provider.hits()), (1, 1));
}

#[tokio::test]
async fn a_call_changed_while_it_waited_replaces_its_hold() {
    let rig = Rig::new();
    assert!(rig.invoke("t1").await.is_err());
    rig.graph.update_state("t1", [("invoice".to_string(), invoice(5000))]).await.unwrap();

    assert!(matches!(rig.invoke("t1").await, Err(GraphError::Interrupted(_))));
    let pending = rig.pending("t1").await;
    assert_eq!(
        pending.iter().map(|entry| entry.request.args.clone()).collect::<Vec<_>>(),
        vec![invoice(5000)],
        "the hold for 4200 is withdrawn; only the changed call waits"
    );
    rig.decide("t1", ToolApproval::approve()).await;
    let state = rig.invoke("t1").await.unwrap();
    assert_eq!((rig.provider.hits(), &state["charge"]["amountCents"]), (1, &json!(5000)));
}

#[cfg(feature = "time-travel")]
#[tokio::test]
async fn a_fork_of_the_parent_answers_a_governed_node_inside_a_subgraph_from_its_record() {
    let provider = Arc::new(Provider::default());
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let inner = Arc::new(
        StateGraph::with_channels(&["invoice", "charge"])
            .add_node(Pay(Arc::clone(&provider)))
            .add_edge(START, "pay")
            .add_edge("pay", END)
            .compile()
            .unwrap()
            .with_checkpointer_arc(checkpointer.clone())
            .with_governed_node("pay", GovernedNode::new()),
    );
    let graph = StateGraph::with_channels(&["invoice", "note", "charge"])
        .add_node_fn("draft", |_ctx| async {
            Ok(NodeOutput::new().with_update("invoice", invoice(4200)))
        })
        .add_node(adk_graph::subgraph::SubgraphNode::new("billing", inner))
        .add_edge(START, "draft")
        .add_edge("draft", "billing")
        .add_edge("billing", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone());
    let run_config =
        RunConfig::builder().action_ledger(Arc::new(InMemoryActionLedger::new())).build();

    let original = graph
        .invoke_with_run_config(State::new(), ExecutionConfig::new("t1"), run_config.clone())
        .await
        .unwrap();
    assert_eq!(provider.hits(), 1);

    let after_draft = checkpointer.list("t1").await.unwrap()[0].checkpoint_id.clone();
    graph
        .time_travel("t1")
        .unwrap()
        .fork(adk_graph::time_travel::ForkSpec::new(&after_draft, "t2").set("note", json!("again")))
        .await
        .unwrap();
    let forked = graph
        .invoke_with_run_config(State::new(), ExecutionConfig::new("t2"), run_config)
        .await
        .unwrap();
    assert_eq!(provider.hits(), 1, "the inner call is not sent again");
    assert_eq!(forked["charge"], original["charge"]);
}

#[cfg(feature = "time-travel")]
#[tokio::test]
async fn a_waiting_call_another_fork_ran_is_answered_and_its_decision_withdrawn() {
    let rig = Rig::new();
    rig.approved("t1", ToolApproval::approve()).await.unwrap();
    rig.fork("t1", "t2", Some(invoice(5000))).await;
    rig.fork("t1", "t3", Some(invoice(5000))).await;
    // Both forks wait for the same changed call; t3 is answered first.
    assert!(rig.invoke("t2").await.is_err());
    rig.approved("t3", ToolApproval::approve()).await.unwrap();
    assert_eq!(rig.provider.hits(), 2);

    let t2 = rig.invoke("t2").await.unwrap();
    assert_eq!(rig.provider.hits(), 2, "t2 is answered from t3's record");
    assert_eq!(t2["charge"]["amountCents"], json!(5000));
    assert!(rig.pending("t2").await.is_empty(), "t2's waiting decision is withdrawn");
}

/// Spec 3.3 §12: a node that pauses itself after its call was approved does not wait on the
/// request it was approved for, which no longer waits in the inbox.
#[tokio::test]
async fn a_node_that_pauses_itself_after_its_approval_is_never_stuck() {
    let rig = Rig::with_pay(|provider| {
        Arc::new(adk_graph::node::FunctionNode::new("pay", move |ctx| {
            let provider = Arc::clone(&provider);
            async move {
                if ctx.resume_value().is_none() {
                    return Ok(NodeOutput::interrupt("which card?"));
                }
                provider.charge(ctx.get("invoice").cloned().unwrap_or_default()).await
            }
        })) as Arc<dyn Node>
    });
    let answered = |thread: &str| {
        rig.graph.invoke_with_run_config(
            State::new(),
            ExecutionConfig::new(thread).with_resume_value("pay", json!("the usual card")),
            rig.run_config(),
        )
    };
    // Approved, the node runs and asks its own question.
    assert!(matches!(rig.invoke("t1").await, Err(GraphError::Interrupted(_))));
    rig.decide("t1", ToolApproval::approve()).await;
    assert!(matches!(rig.invoke("t1").await, Err(GraphError::Interrupted(_))));
    assert_eq!(rig.provider.hits(), 0);

    // Its answer runs it again: as a new call, which waits for its own decision.
    assert!(matches!(answered("t1").await, Err(GraphError::Interrupted(_))));
    assert_eq!(rig.pending("t1").await.len(), 1, "a decision to answer, not a stale wait");
    rig.decide("t1", ToolApproval::approve()).await;
    answered("t1").await.unwrap();
    assert_eq!(rig.provider.hits(), 1);
}

/// Spec 3.3 §11 S-I1: a node bound to the whole state is the same call in a fork, whose
/// graph agent writes a new session id into the state.
#[cfg(feature = "time-travel")]
#[tokio::test]
async fn a_fork_in_a_new_session_answers_a_node_bound_to_its_state_from_its_record() {
    let rig = Rig::with_pay(|provider| {
        Arc::new(adk_graph::node::FunctionNode::new("pay", move |ctx| {
            let provider = Arc::clone(&provider);
            async move { provider.charge(ctx.get("invoice").cloned().unwrap_or_default()).await }
        })) as Arc<dyn Node>
    });
    let turn = |thread: &str, session: &str| {
        rig.graph.invoke_with_run_config(
            State::from([("session_id".to_string(), json!(session))]),
            ExecutionConfig::new(thread),
            rig.run_config(),
        )
    };
    assert!(turn("t1", "s1").await.is_err());
    rig.decide("t1", ToolApproval::approve()).await;
    turn("t1", "s1").await.unwrap();
    assert_eq!(rig.provider.hits(), 1);

    // The fork changes nothing the node reads; only the session differs.
    let after_draft = rig
        .checkpointer
        .list("t1")
        .await
        .unwrap()
        .into_iter()
        .find(|checkpoint| checkpoint.pending_nodes == ["pay"] && !checkpoint.is_paused())
        .expect("a completed checkpoint before pay");
    rig.graph
        .time_travel("t1")
        .unwrap()
        .fork(adk_graph::time_travel::ForkSpec::new(&after_draft.checkpoint_id, "t2"))
        .await
        .unwrap();
    let forked = turn("t2", "s2").await.expect("answered from its record");
    assert_eq!(rig.provider.hits(), 1, "not sent again");
    assert!(rig.pending("t2").await.is_empty(), "no decision was created");
    assert_eq!(forked["charge"]["id"], json!("ch_1"));
}

/// Spec 3.3 §12: a governed call is kept to once by the action ledger and the graph's
/// checkpoints; without either it is not run, never run without the guarantee.
#[tokio::test]
async fn without_a_ledger_or_a_checkpointer_a_governed_node_is_not_run() {
    let rig = Rig::new();
    let no_ledger = rig
        .graph
        .invoke_with_run_config(
            State::new(),
            ExecutionConfig::new("t1"),
            RunConfig::builder().approval_store(rig.store.clone()).build(),
        )
        .await;
    assert_eq!(code(&no_ledger), Some("graph.unavailable"));
    assert!(no_ledger.unwrap_err().to_string().contains("no action ledger"));

    let graph = StateGraph::with_channels(&["invoice", "charge"])
        .add_node(Pay(Arc::clone(&rig.provider)))
        .add_edge(START, "pay")
        .add_edge("pay", END)
        .compile()
        .unwrap()
        .with_governed_node("pay", GovernedNode::new());
    let no_checkpointer = graph
        .invoke_with_run_config(
            State::from([("invoice".to_string(), invoice(4200))]),
            ExecutionConfig::new("t2"),
            rig.run_config(),
        )
        .await;
    assert_eq!(code(&no_checkpointer), Some("graph.unavailable"));
    assert!(no_checkpointer.unwrap_err().to_string().contains("no checkpointer"));
    assert_eq!(rig.provider.hits(), 0);
    assert!(rig.pending("t1").await.is_empty());
}

/// Spec 3.3 §12: governing a name that is not a node fails the run before anything runs,
/// instead of leaving the node meant ungoverned.
#[tokio::test]
async fn governing_a_name_that_is_not_a_node_fails_before_anything_runs() {
    let rig = Rig::new();
    let graph = StateGraph::with_channels(&["invoice", "charge"])
        .add_node(Pay(Arc::clone(&rig.provider)))
        .add_edge(START, "pay")
        .add_edge("pay", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
        .with_governed_node("Pay", GovernedNode::new().with_approval());
    let ran = graph
        .invoke_with_run_config(
            State::from([("invoice".to_string(), invoice(4200))]),
            ExecutionConfig::new("t1"),
            rig.run_config(),
        )
        .await;
    assert!(
        matches!(&ran, Err(GraphError::InvalidGraph(message)) if message.contains("'Pay'")),
        "{ran:?}"
    );
    assert_eq!(rig.provider.hits(), 0);

    let built = GraphAgent::builder("invoice-run")
        .channels(&["invoice", "charge"])
        .node(Pay(Arc::clone(&rig.provider)))
        .edge(START, "pay")
        .edge("pay", END)
        .governed_node("Pay", GovernedNode::new())
        .build();
    assert!(matches!(built, Err(GraphError::InvalidGraph(_))));
}

/// Spec 3.3 §11 S-I1: a governed node inside a subgraph that handed control to the parent
/// is answered from its record with the same hand-off, so a fork routes as the original.
#[cfg(feature = "time-travel")]
#[tokio::test]
async fn a_replayed_call_inside_a_subgraph_routes_the_parent_as_the_original() {
    struct Escalating(Arc<Provider>);

    #[async_trait]
    impl Node for Escalating {
        fn name(&self) -> &str {
            "pay"
        }
        async fn execute(&self, ctx: &NodeContext) -> adk_graph::Result<NodeOutput> {
            let charged = self.0.charge(ctx.get("invoice").cloned().unwrap_or_default()).await?;
            Ok(charged.with_goto_parent(["escalate"]))
        }
    }

    let provider = Arc::new(Provider::default());
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let inner = Arc::new(
        StateGraph::with_channels(&["invoice", "charge"])
            .add_node(Escalating(Arc::clone(&provider)))
            .add_edge(START, "pay")
            .add_edge("pay", END)
            .compile()
            .unwrap()
            .with_checkpointer_arc(checkpointer.clone())
            .with_governed_node("pay", GovernedNode::new()),
    );
    let graph = StateGraph::with_channels(&["invoice", "note", "charge", "next"])
        .add_node_fn("draft", |_ctx| async {
            Ok(NodeOutput::new().with_update("invoice", invoice(4200)))
        })
        .add_node(adk_graph::subgraph::SubgraphNode::new("billing", inner))
        .add_node_fn("receipt", |_ctx| async {
            Ok(NodeOutput::new().with_update("next", json!("receipt")))
        })
        .add_node_fn("escalate", |_ctx| async {
            Ok(NodeOutput::new().with_update("next", json!("escalate")))
        })
        .add_edge(START, "draft")
        .add_edge("draft", "billing")
        .add_edge("billing", "receipt")
        .add_edge("receipt", END)
        .add_edge("escalate", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone());
    let run_config =
        RunConfig::builder().action_ledger(Arc::new(InMemoryActionLedger::new())).build();

    let original = graph
        .invoke_with_run_config(State::new(), ExecutionConfig::new("t1"), run_config.clone())
        .await
        .unwrap();
    assert_eq!((original["next"].clone(), provider.hits()), (json!("escalate"), 1));

    let after_draft = checkpointer.list("t1").await.unwrap()[0].checkpoint_id.clone();
    graph
        .time_travel("t1")
        .unwrap()
        .fork(adk_graph::time_travel::ForkSpec::new(&after_draft, "t2").set("note", json!("again")))
        .await
        .unwrap();
    let forked = graph
        .invoke_with_run_config(State::new(), ExecutionConfig::new("t2"), run_config)
        .await
        .unwrap();
    assert_eq!((forked["next"].clone(), provider.hits()), (json!("escalate"), 1));
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
    // The decision key names the agent too, so no other agent's call can take it (#1210).
    assert_eq!(
        adk_core::ToolConfirmationRequest::fingerprint_agent(&pending[0].fingerprint),
        Some("urn:adk:agent:shop/invoice-run")
    );
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

// ── A governed HTTP call against a loopback provider ─────────────────

#[cfg(feature = "action-http")]
mod http {
    use super::*;
    use adk_action::{
        ActionNodeConfig, BearerAuth, Callbacks, ErrorHandling, ErrorMode, ExecutionControl,
        HttpAuth, HttpBody, HttpMethod, HttpNodeConfig, HttpResponse, InputOutputMapping, LogLevel,
        StandardProperties, Tracing,
    };
    use adk_graph::action::ActionNodeExecutor;
    use adk_graph::action::http::HttpActionPolicy;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One request the provider received.
    #[derive(Debug, Clone, PartialEq)]
    struct Seen {
        method: String,
        target: String,
        authorization: Option<String>,
        body: Value,
    }

    /// A loopback payment provider that counts and records every request.
    struct Mock {
        base: String,
        seen: Arc<Mutex<Vec<Seen>>>,
        /// Every request as it arrived, head and body.
        raw: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl Mock {
        async fn start() -> Self {
            Self::answering_after(Duration::ZERO).await
        }

        /// A provider that takes `delay` to answer each charge it received.
        async fn answering_after(delay: Duration) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
            let raw: Arc<Mutex<Vec<String>>> = Arc::default();
            let record = Arc::clone(&seen);
            let raw_record = Arc::clone(&raw);
            let task = tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let record = Arc::clone(&record);
                    let raw_record = Arc::clone(&raw_record);
                    tokio::spawn(async move {
                        let mut buffer = Vec::new();
                        let mut chunk = [0u8; 4096];
                        let head_end = loop {
                            let read = stream.read(&mut chunk).await.unwrap_or(0);
                            if read == 0 {
                                return;
                            }
                            buffer.extend_from_slice(&chunk[..read]);
                            if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                                break at + 4;
                            }
                        };
                        let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                        let mut lines = head.lines();
                        let mut request_line = lines.next().unwrap_or_default().split(' ');
                        let method = request_line.next().unwrap_or_default().to_string();
                        let target = request_line.next().unwrap_or_default().to_string();
                        let mut length = 0;
                        let mut authorization = None;
                        for line in lines {
                            if let Some((name, value)) = line.split_once(':') {
                                match name.trim().to_ascii_lowercase().as_str() {
                                    "content-length" => length = value.trim().parse().unwrap_or(0),
                                    "authorization" => {
                                        authorization = Some(value.trim().to_string())
                                    }
                                    _ => {}
                                }
                            }
                        }
                        while buffer.len() < head_end + length {
                            let read = stream.read(&mut chunk).await.unwrap_or(0);
                            if read == 0 {
                                break;
                            }
                            buffer.extend_from_slice(&chunk[..read]);
                        }
                        raw_record
                            .lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&buffer).into_owned());
                        let body =
                            serde_json::from_slice(&buffer[head_end..]).unwrap_or(Value::Null);
                        let count = {
                            let mut seen = record.lock().unwrap();
                            seen.push(Seen { method, target, authorization, body });
                            seen.len()
                        };
                        tokio::time::sleep(delay).await;
                        let reply = json!({ "id": format!("ch_{count}") }).to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                            reply.len()
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                    });
                }
            });
            Self { base, seen, raw, task }
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }

        fn raw(&self) -> Vec<String> {
            self.raw.lock().unwrap().clone()
        }
    }

    fn pay_node(base: &str) -> ActionNodeExecutor {
        pay_node_with(base, |_| {})
    }

    /// The pay node, changed by `change` before it is built.
    fn pay_node_with(base: &str, change: impl FnOnce(&mut HttpNodeConfig)) -> ActionNodeExecutor {
        let standard = StandardProperties {
            id: "pay".to_string(),
            name: "pay".to_string(),
            description: None,
            position: None,
            error_handling: ErrorHandling {
                mode: ErrorMode::Stop,
                retry_count: None,
                retry_delay: None,
                fallback_value: None,
            },
            tracing: Tracing { enabled: false, log_level: LogLevel::None },
            callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
            execution: ExecutionControl { timeout: 10_000, condition: None },
            mapping: InputOutputMapping { input_mapping: None, output_key: "charge".to_string() },
        };
        let mut config = HttpNodeConfig {
            standard,
            method: HttpMethod::Post,
            url: format!("{base}/charges?account=acme&api_key={{{{apiKey}}}}"),
            auth: HttpAuth::Bearer(BearerAuth { token: "{{token}}".to_string() }),
            headers: [("X-Request".to_string(), "inv-1".to_string())].into(),
            body: HttpBody::Json {
                data: json!({ "customer": "{{customer}}", "amountCents": 4200 }),
            },
            response: HttpResponse { response_type: "json".to_string(), status_validation: None },
            rate_limit: None,
        };
        change(&mut config);
        ActionNodeExecutor::new(ActionNodeConfig::Http(config)).with_http_policy(
            HttpActionPolicy::new()
                .allow_schemes(["http"])
                .allow_hosts(["127.0.0.1"])
                .max_redirects(0),
        )
    }

    fn state(token: &str) -> State {
        State::from([
            ("customer".to_string(), json!("Acme")),
            ("token".to_string(), json!(token)),
            ("apiKey".to_string(), json!("key-1")),
        ])
    }

    fn ctx(token: &str) -> NodeContext {
        NodeContext::new(state(token), ExecutionConfig::new("t1"), 0)
    }

    #[tokio::test]
    async fn the_call_carries_everything_sent_with_credentials_as_digests() {
        let mock = Mock::start().await;
        let node = pay_node(&mock.base);
        let args = node.call_args(&ctx("tok-1")).expect("an HTTP node describes its call");

        let digest = |value: &Value| value.as_str().unwrap_or_default().to_string();
        let authorization = digest(&args["headers"]["authorization"]);
        let url = digest(&args["url"]);
        assert!(authorization.starts_with("sha256:"), "{authorization}");
        assert!(
            url.starts_with(&format!("{}/charges?account=acme&api_key=sha256%3A", mock.base)),
            "{url}"
        );
        let shown = args.to_string();
        assert!(
            !shown.contains("tok-1") && !shown.contains("key-1"),
            "no secret is shown: {shown}"
        );
        let mut expected = json!({
            "method": "POST",
            "url": url,
            "headers": {
                "authorization": authorization,
                "content-type": "application/json",
                "x-request": "inv-1",
            },
            "body": { "customer": "Acme", "amountCents": 4200 },
        });
        assert_eq!(args, expected);

        // The digest binds the credential: a rotated token is a different call.
        let rotated = node.call_args(&ctx("tok-2")).unwrap();
        expected["headers"]["authorization"] = rotated["headers"]["authorization"].clone();
        assert_eq!(rotated, expected);
        assert_ne!(rotated["headers"]["authorization"], args["headers"]["authorization"]);
        assert!(mock.seen().is_empty(), "describing a call sends nothing");
    }

    #[tokio::test]
    async fn an_approved_call_is_sent_once_with_its_real_credentials() {
        let mock = Mock::start().await;
        let checkpointer = Arc::new(MemoryCheckpointer::new());
        let store = Arc::new(InMemoryApprovalStore::new());
        let run_config = RunConfig::builder()
            .approval_store(store.clone())
            .action_ledger(Arc::new(InMemoryActionLedger::new()))
            .build();
        let graph = StateGraph::with_channels(&["customer", "token", "apiKey", "charge"])
            .add_node(pay_node(&mock.base))
            .add_edge(START, "pay")
            .add_edge("pay", END)
            .compile()
            .unwrap()
            .with_checkpointer_arc(checkpointer.clone())
            .with_node_effect("pay", ToolEffect::NonIdempotent)
            .with_governed_node("pay", GovernedNode::new().with_approval().spending_money());
        let invoke = |thread: &'static str| {
            graph.invoke_with_run_config(
                state("tok-1"),
                ExecutionConfig::new(thread),
                run_config.clone(),
            )
        };

        assert!(matches!(invoke("t1").await, Err(GraphError::Interrupted(_))));
        assert!(mock.seen().is_empty());
        let scope = ApprovalScope::new("graph_app", "graph_user", "t1");
        let pending = store.pending(&scope).await.unwrap();
        assert_eq!(pending.len(), 1);
        let shown = pending[0].request.args.clone();
        assert_eq!(Some(shown.clone()), pay_node(&mock.base).call_args(&ctx("tok-1")));

        // The approver edits the amount; the edited call goes out once, as edited.
        let mut edited = shown;
        edited["body"]["amountCents"] = json!(5000);
        store
            .decide(
                &scope,
                &pending[0].fingerprint,
                ToolApproval::approve().with_edited_args(edited),
            )
            .await
            .unwrap();
        let done = invoke("t1").await.unwrap();
        assert_eq!(
            mock.seen(),
            vec![Seen {
                method: "POST".to_string(),
                target: "/charges?account=acme&api_key=key-1".to_string(),
                authorization: Some("Bearer tok-1".to_string()),
                body: json!({ "customer": "Acme", "amountCents": 5000 }),
            }]
        );
        assert_eq!(done["charge"], json!({ "status": 200, "data": { "id": "ch_1" } }));

        // Running the finished thread again sends nothing.
        invoke("t1").await.unwrap();
        assert_eq!(mock.seen().len(), 1);
    }

    #[tokio::test]
    async fn a_digest_that_no_longer_matches_the_credential_refuses_the_call() {
        let mock = Mock::start().await;
        let node = pay_node(&mock.base);
        let approved = node.call_args(&ctx("tok-1")).unwrap();

        // The token rotated after the approval: the call is refused, not sent.
        let refused = node.run_call(&ctx("tok-2"), approved.clone()).await.err().expect("refused");
        assert_eq!(GovernedError::code_of(&refused), Some("graph.refused"));
        assert!(refused.to_string().contains("header 'authorization'"), "{refused}");

        // A digest an approver altered is refused the same way.
        let mut forged = approved.clone();
        forged["url"] =
            json!(format!("{}/charges?account=acme&api_key=sha256%3Aforged", mock.base));
        let forged = node.run_call(&ctx("tok-1"), forged).await.err().expect("refused");
        assert_eq!(GovernedError::code_of(&forged), Some("graph.refused"));

        // A host outside the policy is refused too.
        let mut elsewhere = approved.clone();
        elsewhere["url"] = json!("http://example.com/charges");
        let elsewhere = node.run_call(&ctx("tok-1"), elsewhere).await.err().expect("refused");
        assert_eq!(GovernedError::code_of(&elsewhere), Some("graph.refused"));
        assert!(mock.seen().is_empty());

        // The unchanged credential sends exactly the approved call.
        node.run_call(&ctx("tok-1"), approved).await.unwrap();
        assert_eq!(mock.seen().len(), 1);
        assert_eq!(mock.seen()[0].authorization.as_deref(), Some("Bearer tok-1"));
    }

    /// A governed graph of the one HTTP node `node`, with its stores.
    struct HttpRig {
        checkpointer: Arc<MemoryCheckpointer>,
        store: Arc<InMemoryApprovalStore>,
        ledger: Arc<InMemoryActionLedger>,
        graph: CompiledGraph,
    }

    impl HttpRig {
        fn new(node: ActionNodeExecutor) -> Self {
            let checkpointer = Arc::new(MemoryCheckpointer::new());
            let graph = StateGraph::with_channels(&[
                "customer",
                "token",
                "apiKey",
                "charge",
                "shouldCharge",
            ])
            .add_node(node)
            .add_edge(START, "pay")
            .add_edge("pay", END)
            .compile()
            .unwrap()
            .with_checkpointer_arc(checkpointer.clone())
            .with_node_effect("pay", ToolEffect::NonIdempotent)
            .with_governed_node("pay", GovernedNode::new().with_approval().spending_money());
            Self {
                checkpointer,
                store: Arc::new(InMemoryApprovalStore::new()),
                ledger: Arc::new(InMemoryActionLedger::new()),
                graph,
            }
        }

        async fn invoke(&self, input: State) -> adk_graph::Result<State> {
            let run_config = RunConfig::builder()
                .approval_store(self.store.clone())
                .action_ledger(self.ledger.clone())
                .build();
            self.graph.invoke_with_run_config(input, ExecutionConfig::new("t1"), run_config).await
        }

        async fn pending(&self) -> Vec<PendingApproval> {
            self.store.pending(&ApprovalScope::new("graph_app", "graph_user", "t1")).await.unwrap()
        }

        /// Runs to the pause, approves the call as shown, and runs again.
        async fn approved(&self, input: State) -> adk_graph::Result<State> {
            assert!(matches!(self.invoke(input.clone()).await, Err(GraphError::Interrupted(_))));
            let pending = self.pending().await;
            assert_eq!(pending.len(), 1);
            self.store
                .decide(
                    &ApprovalScope::new("graph_app", "graph_user", "t1"),
                    &pending[0].fingerprint,
                    ToolApproval::approve(),
                )
                .await
                .unwrap();
            self.invoke(input).await
        }

        /// The pay call's ledger outcome; `None` while it is begun without one.
        async fn outcome(&self) -> Option<ActionOutcome> {
            let key = self
                .checkpointer
                .list("t1")
                .await
                .unwrap()
                .iter()
                .flat_map(|checkpoint| checkpoint.step_records().unwrap())
                .flat_map(|step| step.nodes)
                .find_map(|node| node.effect_key)
                .expect("pay has an effect key");
            self.ledger.get(&key).await.unwrap().expect("a ledger record").outcome
        }
    }

    /// Spec 3.3 §12: one approval sends the call once. The provider took the charge and its
    /// answer failed validation, so the call's outcome is unknown: the node's Retry mode
    /// does not send it again, and no later run does.
    #[tokio::test]
    async fn an_approved_call_is_never_retried_by_the_nodes_error_handling() {
        let mock = Mock::start().await;
        let rig = HttpRig::new(pay_node_with(&mock.base, |config| {
            config.standard.error_handling = ErrorHandling {
                mode: ErrorMode::Retry,
                retry_count: Some(2),
                retry_delay: None,
                fallback_value: None,
            };
            config.response.status_validation = Some("201".to_string());
        }));
        let failed = rig.approved(state("tok-1")).await;
        assert_eq!(code(&failed), Some("graph.outcome_unknown"));
        assert_eq!(mock.seen().len(), 1, "sent once");
        assert_eq!(rig.outcome().await, None, "the record stays begun");

        let again = rig.invoke(state("tok-1")).await;
        assert_eq!(code(&again), Some("graph.outcome_unknown"));
        assert_eq!(mock.seen().len(), 1);
        assert!(rig.pending().await.is_empty());
    }

    /// Spec 3.3 §12: a call whose node's own timeout fires may have taken effect, so its
    /// outcome is unknown, never a recorded failure that invites a new call.
    #[tokio::test]
    async fn a_call_cut_off_by_its_nodes_timeout_has_an_unknown_outcome() {
        let mock = Mock::answering_after(Duration::from_secs(5)).await;
        let rig = HttpRig::new(pay_node_with(&mock.base, |config| {
            config.standard.execution.timeout = 300;
        }));
        let cut = rig.approved(state("tok-1")).await;
        assert!(matches!(cut, Err(GraphError::NodeTimedOut { .. })), "{cut:?}");
        assert_eq!(rig.outcome().await, None, "the record stays begun");
        let again = rig.invoke(state("tok-1")).await;
        assert_eq!(code(&again), Some("graph.outcome_unknown"));
        assert_eq!(mock.seen().len(), 1);
    }

    /// Spec 3.3 §12: a call that failed is recorded as failed, not hidden by the node's
    /// Continue mode and recorded as a call that succeeded.
    #[tokio::test]
    async fn a_failed_call_is_not_recorded_as_sent_under_continue() {
        // Nothing listens on a closed port, so the call fails before it is sent.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let rig = HttpRig::new(pay_node_with(&base, |config| {
            config.standard.error_handling.mode = ErrorMode::Continue;
        }));
        let failed = rig.approved(state("tok-1")).await;
        assert!(failed.is_err(), "the failure is the node's: {failed:?}");
        assert!(
            matches!(rig.outcome().await, Some(ActionOutcome::Failed { .. })),
            "recorded as failed"
        );
    }

    /// Spec 3.3 §7.4, §12: a node whose condition is false makes no call, so no one is
    /// asked and nothing is recorded as sent.
    #[tokio::test]
    async fn a_node_whose_condition_is_false_asks_no_one_and_records_nothing() {
        let mock = Mock::start().await;
        let rig = HttpRig::new(pay_node_with(&mock.base, |config| {
            config.standard.execution.condition = Some("{{shouldCharge}}".to_string());
        }));
        let mut input = state("tok-1");
        input.insert("shouldCharge".to_string(), json!(false));
        let done = rig.invoke(input).await.expect("the run completes without a decision");
        assert!(!done.contains_key("charge"));
        assert!(rig.pending().await.is_empty());
        assert!(mock.seen().is_empty());
        assert!(effect_log(rig.checkpointer.as_ref(), "t1").await.unwrap().is_empty());
        let keys: Vec<Option<String>> = rig
            .checkpointer
            .list("t1")
            .await
            .unwrap()
            .iter()
            .flat_map(|checkpoint| checkpoint.step_records().unwrap())
            .flat_map(|step| step.nodes)
            .map(|node| node.effect_key)
            .collect();
        assert_eq!(keys, vec![None], "no ledger key was used");
    }

    /// Spec 3.3 §12: a request that cannot be built is never shown as the state it read,
    /// and no one is asked about a call that could not be sent.
    #[tokio::test]
    async fn a_request_that_cannot_be_built_asks_no_one_and_shows_no_secret() {
        let rig = HttpRig::new(pay_node("http://payments.example.com"));
        let refused = rig.invoke(state("tok-secret-1")).await;
        let error = refused.expect_err("the host is not allowed");
        assert!(error.to_string().contains("not in the HTTP action policy allowlist"), "{error}");
        assert!(!error.to_string().contains("tok-secret-1"));
        assert!(rig.pending().await.is_empty(), "no decision for a call that cannot be sent");

        let described = pay_node("http://payments.example.com")
            .call_args(&ctx("tok-secret-1"))
            .expect("an HTTP node always describes its call");
        assert!(!described.to_string().contains("tok-secret-1"), "{described}");
        assert!(!described.to_string().contains("key-1"), "{described}");
    }

    /// Spec 3.3 §12: an API key's header is a credential whatever its name, and so are JSON
    /// body fields with secret-looking names and secret state in a raw body; the approved
    /// call sends the real values.
    #[tokio::test]
    async fn api_keys_and_secrets_in_bodies_are_shown_as_digests_and_sent_as_themselves() {
        let mock = Mock::start().await;
        let node = pay_node_with(&mock.base, |config| {
            config.url = format!("{}/charges", mock.base);
            config.auth = HttpAuth::ApiKey(adk_action::ApiKeyAuth {
                header: "x-functions-key".to_string(),
                value: "{{apiKey}}".to_string(),
            });
            config.body = HttpBody::Json {
                data: json!({ "customer": "{{customer}}", "auth": { "client_secret": "{{token}}" } }),
            };
        });
        let args = node.call_args(&ctx("tok-1")).unwrap();
        let shown = args.to_string();
        assert!(!shown.contains("key-1") && !shown.contains("tok-1"), "{shown}");
        assert!(args["headers"]["x-functions-key"].as_str().unwrap().starts_with("sha256:"));
        assert!(args["body"]["auth"]["client_secret"].as_str().unwrap().starts_with("sha256:"));

        let mut edited = args.clone();
        edited["body"]["customer"] = json!("Acme Ltd");
        node.run_call(&ctx("tok-1"), edited.clone()).await.unwrap();
        let sent = mock.raw().remove(0);
        assert!(sent.contains("x-functions-key: key-1"), "{sent}");
        assert_eq!(
            mock.seen()[0].body,
            json!({ "customer": "Acme Ltd", "auth": { "client_secret": "tok-1" } })
        );
        // A rotated secret refuses the approved call.
        let refused = node.run_call(&ctx("tok-2"), edited).await.err().expect("refused");
        assert_eq!(GovernedError::code_of(&refused), Some("graph.refused"));

        let raw = pay_node_with(&mock.base, |config| {
            config.url = format!("{}/charges", mock.base);
            config.body = HttpBody::Raw {
                content: "customer={{customer}}&token={{token}}".to_string(),
                content_type: "application/x-www-form-urlencoded".to_string(),
            };
        });
        let args = raw.call_args(&ctx("tok-1")).unwrap();
        assert!(!args["rawBody"].as_str().unwrap().contains("tok-1"), "{args}");
        raw.run_call(&ctx("tok-1"), args).await.unwrap();
        assert!(mock.raw()[1].ends_with("customer=Acme&token=tok-1"), "{:?}", mock.raw()[1]);
    }
}
