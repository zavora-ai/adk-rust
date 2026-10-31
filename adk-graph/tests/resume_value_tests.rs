//! The answer to a node's pause answers that pause only.
//!
//! A dynamic pause could be answered only through state, and state outlives the
//! pause: a review node in a loop, or on the next turn of a `GraphAgent`, read
//! the earlier approval and let the next request through without asking.

use crate::support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_core::{Agent, Event};
use adk_graph::agent::GraphAgent;
use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{CompiledGraph, StateGraph};
use adk_graph::interrupt::GraphInterruptPayload;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use futures::StreamExt;
use serde_json::json;

/// `review` pauses until answered; `execute` counts the refunds it issues.
fn refund_graph(executions: Arc<AtomicUsize>) -> CompiledGraph {
    StateGraph::with_channels(&["request", "decision"])
        .add_node_fn("review", |ctx| async move {
            match ctx.resume_value().and_then(|answer| answer.as_str()) {
                Some("approve") => Ok(NodeOutput::new().with_update("decision", json!("approved"))),
                Some(_) => Ok(NodeOutput::new()
                    .with_update("decision", json!("rejected"))
                    .with_goto([END])),
                None => Ok(NodeOutput::interrupt("approve this refund?")),
            }
        })
        .add_node_fn("execute", move |_| {
            let executions = Arc::clone(&executions);
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new())
            }
        })
        .add_edge(START, "review")
        .add_edge("review", "execute")
        .add_edge("execute", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
}

#[tokio::test]
async fn an_answer_reaches_the_paused_node_without_entering_state() {
    let executions = Arc::new(AtomicUsize::new(0));
    let graph = refund_graph(Arc::clone(&executions));

    let paused = graph.invoke(State::new(), ExecutionConfig::new("refund-1")).await;
    assert!(matches!(paused, Err(GraphError::Interrupted(_))));

    let state = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("refund-1").with_resume_value("review", json!("approve")),
        )
        .await
        .unwrap();

    assert_eq!(state.get("decision"), Some(&json!("approved")));
    assert!(!state.values().any(|value| value == &json!("approve")), "{state:?}");
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_answer_for_a_node_that_did_not_pause_is_not_delivered() {
    let executions = Arc::new(AtomicUsize::new(0));
    let graph = refund_graph(Arc::clone(&executions));

    let outcome = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("refund-2").with_resume_value("review", json!("approve")),
        )
        .await;

    assert!(matches!(outcome, Err(GraphError::Interrupted(_))), "the first run must still ask");
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

async fn turn(agent: &GraphAgent, text: &str) -> Vec<Event> {
    agent
        .run(support::test_context_with_text(text))
        .await
        .expect("run")
        .map(|event| event.expect("event"))
        .collect()
        .await
}

#[tokio::test]
async fn each_turn_of_a_graph_agent_asks_again() {
    let executions = Arc::new(AtomicUsize::new(0));
    let agent = GraphAgent::from_graph("refunds", refund_graph(Arc::clone(&executions)));
    let paused =
        |events: &[Event]| events.iter().any(|e| GraphInterruptPayload::from_event(e).is_some());

    let first = turn(&agent, "refund order A-1").await;
    let second = turn(&agent, "approve").await;
    let after_approval = executions.load(Ordering::SeqCst);
    let third = turn(&agent, "refund order B-2").await;

    assert_eq!(
        (paused(&first), paused(&second), after_approval, paused(&third)),
        (true, false, 1, true),
        "the second refund must wait for its own approval"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_resume_mapper_shapes_the_answer() {
    let executions = Arc::new(AtomicUsize::new(0));
    let runs = Arc::clone(&executions);
    let agent = GraphAgent::builder("refunds")
        .channels(&["decision"])
        .node_fn("review", |ctx| async move {
            match ctx.resume_value().and_then(|answer| answer.get("approved")) {
                Some(approved) => Ok(NodeOutput::new().with_update("decision", approved.clone())),
                None => Ok(NodeOutput::interrupt("approve this refund?")),
            }
        })
        .node_fn("execute", move |_| {
            let runs = Arc::clone(&runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new())
            }
        })
        .edge(START, "review")
        .edge("review", "execute")
        .edge("execute", END)
        .checkpointer(MemoryCheckpointer::new())
        .resume_mapper(|ctx| {
            let text: String = ctx.user_content().parts.iter().filter_map(|p| p.text()).collect();
            Some(json!({ "approved": text == "yes" }))
        })
        .build()
        .unwrap();

    turn(&agent, "refund order A-1").await;
    turn(&agent, "yes").await;

    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_answer_to_a_subgraphs_pause_reaches_the_node_inside() {
    let executions = Arc::new(AtomicUsize::new(0));
    let inner = Arc::new(refund_graph(Arc::clone(&executions)));
    let graph = StateGraph::with_channels(&["decision"])
        .add_node(adk_graph::subgraph::SubgraphNode::new("refund", inner))
        .add_edge(START, "refund")
        .add_edge("refund", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let paused = graph.invoke(State::new(), ExecutionConfig::new("nested-refund")).await;
    assert!(matches!(paused, Err(GraphError::Interrupted(_))));
    let state = graph
        .invoke(
            State::new(),
            ExecutionConfig::new("nested-refund").with_resume_value("refund", json!("approve")),
        )
        .await
        .unwrap();

    assert_eq!(
        (state.get("decision"), executions.load(Ordering::SeqCst)),
        (Some(&json!("approved")), 1)
    );
}
