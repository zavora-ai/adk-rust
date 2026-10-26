//! A governed call runs once per distinct approved call in a lineage, however forks and
//! edits interleave (spec 3.3 §12 developer track, §11 S-I1).

#![cfg(feature = "time-travel")]

use adk_core::{
    ApprovalScope, ApprovalStore, InMemoryActionLedger, InMemoryApprovalStore, RunConfig,
    ToolApproval,
};
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::governed::GovernedNode;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::node::{ExecutionConfig, Node, NodeContext, NodeOutput};
use adk_graph::state::State;
use adk_graph::time_travel::ForkSpec;
use async_trait::async_trait;
use proptest::prelude::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Pay(Arc<AtomicUsize>);

#[async_trait]
impl Node for Pay {
    fn name(&self) -> &str {
        "pay"
    }
    fn call_args(&self, ctx: &NodeContext) -> Option<Value> {
        Some(json!({ "amountCents": ctx.get("amountCents")? }))
    }
    async fn execute(&self, ctx: &NodeContext) -> adk_graph::Result<NodeOutput> {
        let args = self.call_args(ctx).unwrap_or_default();
        self.run_call(ctx, args).await
    }
    async fn run_call(&self, _ctx: &NodeContext, args: Value) -> adk_graph::Result<NodeOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(NodeOutput::new().with_update("charged", args["amountCents"].clone()))
    }
}

/// How a person answers a waiting call.
#[derive(Debug, Clone, Copy)]
enum Answer {
    Approve,
    /// Approve with the amount changed to this one.
    Edit(i64),
    Deny,
}

/// One step of the scenario: fork an existing thread at its checkpoint before `pay`,
/// maybe with a new amount, run it, and answer it if it waits.
#[derive(Debug, Clone)]
struct Fork {
    source: usize,
    amount: Option<i64>,
    answer: Answer,
}

fn answer() -> impl Strategy<Value = Answer> {
    prop_oneof![
        3 => Just(Answer::Approve),
        1 => prop_oneof![Just(1), Just(2), Just(9)].prop_map(Answer::Edit),
        1 => Just(Answer::Deny),
    ]
}

fn fork() -> impl Strategy<Value = Fork> {
    (0usize..8, proptest::option::of(prop_oneof![Just(1i64), Just(2), Just(3)]), answer())
        .prop_map(|(source, amount, answer)| Fork { source, amount, answer })
}

fn graph(checkpointer: Arc<MemoryCheckpointer>, hits: Arc<AtomicUsize>) -> CompiledGraph {
    StateGraph::with_channels(&["amountCents", "charged"])
        .add_node_fn("draft", |_ctx| async {
            Ok(NodeOutput::new().with_update("amountCents", json!(1)))
        })
        .add_node(Pay(hits))
        .add_edge(START, "draft")
        .add_edge("draft", "pay")
        .add_edge("pay", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer)
        .with_governed_node("pay", GovernedNode::new().with_approval())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn hits_equal_the_distinct_approved_calls(first in answer(), forks in proptest::collection::vec(fork(), 0..6)) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let checkpointer = Arc::new(MemoryCheckpointer::new());
            let hits = Arc::new(AtomicUsize::new(0));
            let graph = graph(checkpointer.clone(), Arc::clone(&hits));
            let store = Arc::new(InMemoryApprovalStore::new());
            let run_config = RunConfig::builder()
                .approval_store(store.clone())
                .action_ledger(Arc::new(InMemoryActionLedger::new()))
                .build();
            let mut approved: BTreeSet<i64> = BTreeSet::new();
            let mut threads: Vec<(String, i64)> = Vec::new();

            let steps = std::iter::once(Fork { source: usize::MAX, amount: None, answer: first }).chain(forks);
            for (index, step) in steps.enumerate() {
                let thread = format!("t{index}");
                let amount = if step.source == usize::MAX {
                    1
                } else {
                    let (source, source_amount) = threads[step.source % threads.len()].clone();
                    let before_pay = checkpointer
                        .list(&source)
                        .await
                        .unwrap()
                        .into_iter()
                        .find(|c| c.pending_nodes == ["pay"] && !c.is_paused())
                        .unwrap();
                    let mut spec = ForkSpec::new(&before_pay.checkpoint_id, &thread);
                    if let Some(amount) = step.amount {
                        spec = spec.set("amountCents", json!(amount));
                    }
                    graph.time_travel(&source).unwrap().fork(spec).await.unwrap();
                    step.amount.unwrap_or(source_amount)
                };
                threads.push((thread.clone(), amount));

                let config = || ExecutionConfig::new(&thread);
                match graph.invoke_with_run_config(State::new(), config(), run_config.clone()).await {
                    Ok(_) => prop_assert!(approved.contains(&amount), "ran without a decision"),
                    Err(GraphError::Interrupted(_)) => {
                        prop_assert!(!approved.contains(&amount), "asked again for an approved call");
                        let scope = ApprovalScope::new("graph_app", "graph_user", &thread);
                        let pending = store.pending(&scope).await.unwrap();
                        prop_assert_eq!(pending.len(), 1);
                        let approval = match step.answer {
                            Answer::Approve => ToolApproval::approve(),
                            Answer::Edit(to) => ToolApproval::approve().with_edited_args(json!({ "amountCents": to })),
                            Answer::Deny => ToolApproval::deny(),
                        };
                        store.decide(&scope, &pending[0].fingerprint, approval).await.unwrap();
                        let outcome = graph.invoke_with_run_config(State::new(), config(), run_config.clone()).await;
                        match step.answer {
                            Answer::Deny => prop_assert!(outcome.is_err()),
                            Answer::Approve | Answer::Edit(_) => {
                                prop_assert!(outcome.is_ok());
                                approved.insert(amount);
                            }
                        }
                    }
                    Err(other) => prop_assert!(false, "unexpected failure: {other}"),
                }
                prop_assert_eq!(hits.load(Ordering::SeqCst), approved.len());
            }
            Ok::<(), TestCaseError>(())
        })?;
    }
}
