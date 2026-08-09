//! Tests for graph-wide node defaults, node failure handlers, and a subgraph
//! handing control to its parent.

use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::{NodeDefaults, StateGraph};
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::retry::RetryPolicy;
use adk_graph::state::State;
use adk_graph::subgraph::SubgraphNode;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A node that fails a set number of times, then succeeds.
fn flaky(
    failures: usize,
    attempts: Arc<AtomicUsize>,
) -> impl Fn(
    adk_graph::node::NodeContext,
) -> std::pin::Pin<Box<dyn Future<Output = adk_graph::error::Result<NodeOutput>> + Send>>
+ Send
+ Sync
+ 'static {
    move |_ctx| {
        let attempts = Arc::clone(&attempts);
        Box::pin(async move {
            let seen = attempts.fetch_add(1, Ordering::SeqCst);
            if seen < failures {
                Err(GraphError::Other(format!("transient {seen}")))
            } else {
                Ok(NodeOutput::new().with_update("done", json!(true)))
            }
        })
    }
}

#[tokio::test]
async fn a_graph_default_retry_applies_to_a_node_that_sets_none() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let graph = StateGraph::with_channels(&["done"])
        .add_node_fn("flaky", flaky(2, Arc::clone(&attempts)))
        .add_edge(START, "flaky")
        .add_edge("flaky", END)
        .compile()
        .unwrap()
        .with_node_defaults(
            NodeDefaults::new()
                .with_retry(RetryPolicy::new(3).with_initial_delay(Duration::from_millis(1))),
        );

    let state = graph.invoke(State::new(), ExecutionConfig::new("default-retry")).await.unwrap();
    assert_eq!(state.get("done"), Some(&json!(true)));
    assert_eq!(attempts.load(Ordering::SeqCst), 3, "two failures then a success");
}

#[tokio::test]
async fn without_a_default_the_first_failure_still_ends_the_run() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let graph = StateGraph::with_channels(&["done"])
        .add_node_fn("flaky", flaky(2, Arc::clone(&attempts)))
        .add_edge(START, "flaky")
        .add_edge("flaky", END)
        .compile()
        .unwrap();

    assert!(graph.invoke(State::new(), ExecutionConfig::new("no-default")).await.is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "one attempt, as before");
}

#[tokio::test]
async fn a_per_node_retry_wins_over_the_graph_default() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let graph = StateGraph::with_channels(&["done"])
        .add_node_fn("flaky", flaky(4, Arc::clone(&attempts)))
        .add_edge(START, "flaky")
        .add_edge("flaky", END)
        .compile()
        .unwrap()
        // The default would give up after 2.
        .with_node_defaults(
            NodeDefaults::new()
                .with_retry(RetryPolicy::new(2).with_initial_delay(Duration::from_millis(1))),
        )
        // This node gets 5, so it reaches its success on the fifth attempt.
        .with_node_retry("flaky", RetryPolicy::new(5).with_initial_delay(Duration::from_millis(1)));

    let state = graph.invoke(State::new(), ExecutionConfig::new("per-node-wins")).await.unwrap();
    assert_eq!(state.get("done"), Some(&json!(true)));
    assert_eq!(attempts.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn an_error_handler_routes_to_a_recovery_node() {
    let graph = StateGraph::with_channels(&["status", "recovered"])
        .add_node_fn(
            "charge",
            |_ctx| async move { Err(GraphError::Other("card declined".to_string())) },
        )
        .add_node_fn("compensate", |_ctx| async move {
            Ok(NodeOutput::new().with_update("recovered", json!(true)))
        })
        .add_edge(START, "charge")
        .add_edge("compensate", END)
        .compile()
        .unwrap()
        .with_node_error_handler("charge", |node, error, _state| {
            Ok(NodeOutput::new()
                .with_update("status", json!(format!("{node} failed: {error}")))
                .with_goto(["compensate"]))
        });

    let state = graph.invoke(State::new(), ExecutionConfig::new("handled")).await.unwrap();
    assert_eq!(state.get("recovered"), Some(&json!(true)), "the recovery node ran");
    let status = state.get("status").and_then(|v| v.as_str()).unwrap_or("");
    assert!(status.contains("charge failed"), "the handler recorded the failure: {status}");
    assert!(status.contains("card declined"), "and the reason: {status}");
}

#[tokio::test]
async fn a_handler_returning_an_error_still_ends_the_run() {
    let graph = StateGraph::with_channels(&["status"])
        .add_node_fn(
            "charge",
            |_ctx| async move { Err(GraphError::Other("card declined".to_string())) },
        )
        .add_edge(START, "charge")
        .add_edge("charge", END)
        .compile()
        .unwrap()
        .with_node_error_handler("charge", |_node, error, _state| {
            Err(GraphError::Other(format!("unrecoverable: {error}")))
        });

    let error = graph
        .invoke(State::new(), ExecutionConfig::new("unhandled"))
        .await
        .expect_err("a handler may decline to recover");
    assert!(error.to_string().contains("unrecoverable"));
}

#[tokio::test]
async fn a_handler_runs_only_after_the_retry_budget_is_spent() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let handled = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&handled);

    let graph = StateGraph::with_channels(&["done", "status"])
        .add_node_fn("flaky", flaky(1, Arc::clone(&attempts)))
        .add_edge(START, "flaky")
        .add_edge("flaky", END)
        .compile()
        .unwrap()
        .with_node_retry("flaky", RetryPolicy::new(3).with_initial_delay(Duration::from_millis(1)))
        .with_node_error_handler("flaky", move |_node, _error, _state| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(NodeOutput::new().with_update("status", json!("handled")))
        });

    let state =
        graph.invoke(State::new(), ExecutionConfig::new("retry-then-handle")).await.unwrap();
    assert_eq!(state.get("done"), Some(&json!(true)), "the retry succeeded");
    assert_eq!(handled.load(Ordering::SeqCst), 0, "so the handler was never needed");
}

#[tokio::test]
async fn a_subgraph_hands_control_to_a_node_of_its_parent() {
    let escalated = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&escalated);

    // The inner graph gives up and names a node of the graph that holds it.
    let inner = Arc::new(
        StateGraph::with_channels(&["question", "answer"])
            .add_node_fn("try_answer", |_ctx| async move {
                Ok(NodeOutput::new()
                    .with_update("answer", json!("not confident"))
                    .with_goto_parent(["escalate"]))
            })
            .add_edge(START, "try_answer")
            .add_edge("try_answer", END)
            .compile()
            .unwrap(),
    );

    let outer = StateGraph::with_channels(&["question", "answer", "escalated"])
        .add_node(SubgraphNode::new("attempt", inner))
        .add_node_fn("normal_path", |_ctx| async move {
            Ok(NodeOutput::new().with_update("escalated", json!("no")))
        })
        .add_node_fn("escalate", move |_ctx| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(NodeOutput::new().with_update("escalated", json!("yes")))
            }
        })
        .add_edge(START, "attempt")
        // The declared path out of the subgraph goes to `normal_path`.
        .add_edge("attempt", "normal_path")
        .add_edge("normal_path", END)
        .add_edge("escalate", END)
        .compile()
        .unwrap();

    let state = outer.invoke(State::new(), ExecutionConfig::new("to-parent")).await.unwrap();

    assert_eq!(escalated.load(Ordering::SeqCst), 1, "the parent's escalation node ran");
    assert_eq!(
        state.get("escalated"),
        Some(&json!("yes")),
        "the parent-goto replaced the declared edge to `normal_path`"
    );
    assert_eq!(
        state.get("answer"),
        Some(&json!("not confident")),
        "and the output still projected"
    );
}

#[tokio::test]
async fn a_parent_goto_naming_an_unknown_node_fails_the_run() {
    let inner = Arc::new(
        StateGraph::with_channels(&["answer"])
            .add_node_fn("try_answer", |_ctx| async move {
                Ok(NodeOutput::new()
                    .with_update("answer", json!("x"))
                    .with_goto_parent(["nowhere_in_parent"]))
            })
            .add_edge(START, "try_answer")
            .add_edge("try_answer", END)
            .compile()
            .unwrap(),
    );

    let outer = StateGraph::with_channels(&["answer"])
        .add_node(SubgraphNode::new("attempt", inner))
        .add_edge(START, "attempt")
        .add_edge("attempt", END)
        .compile()
        .unwrap();

    let error = outer
        .invoke(State::new(), ExecutionConfig::new("bad-parent-goto"))
        .await
        .expect_err("a parent-goto naming no node must fail");
    assert!(
        matches!(error, GraphError::UnknownRouteTarget(ref m) if m.contains("nowhere_in_parent")),
        "the parent validates the target, got {error:?}"
    );
}
