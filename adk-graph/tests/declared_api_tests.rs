//! Tests for API that was declared and never reached.
//!
//! Three items, each of which reported nothing when it should have reported
//! something:
//!
//! | Item | Before |
//! |------|--------|
//! | An unknown route key | the branch stopped and the run ended, reporting success |
//! | `StreamEvent::RouteDispatched` | declared with a constructor nothing called |
//! | `CompiledGraph::time_travel` without a checkpointer | panicked inside a library |

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::stream::{StreamEvent, StreamMode};
use futures::StreamExt;
use serde_json::json;

/// A graph whose router answers with `key`, against declared targets.
fn routed_graph(key: &'static str) -> adk_graph::graph::CompiledGraph {
    StateGraph::with_channels(&["value", "seen"])
        .add_node_fn(
            "start",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) },
        )
        .add_node_fn("left", |_ctx| async move {
            Ok(NodeOutput::new().with_update("seen", json!("left")))
        })
        .add_edge(START, "start")
        .add_conditional_edges(
            "start",
            move |_state: &State| key.to_string(),
            [("go_left", "left")],
        )
        .add_edge("left", END)
        .compile()
        .unwrap()
}

#[tokio::test]
async fn a_route_key_nobody_declared_is_an_error() {
    // "go_right" is not among the declared route keys. The router is wrong, and
    // before this the run simply stopped and reported success.
    let error = routed_graph("go_right")
        .invoke(State::new(), ExecutionConfig::new("bad-route"))
        .await
        .expect_err("an undeclared route key must fail the run");

    match error {
        GraphError::UnknownRouteTarget(key) => assert!(
            key.contains("go_right"),
            "the message must name the key the router returned, got {key:?}"
        ),
        other => panic!("expected UnknownRouteTarget, got {other:?}"),
    }
}

#[tokio::test]
async fn a_declared_route_key_still_routes() {
    let state = routed_graph("go_left")
        .invoke(State::new(), ExecutionConfig::new("good-route"))
        .await
        .unwrap();
    assert_eq!(state.get("seen"), Some(&json!("left")));
}

#[tokio::test]
async fn a_conditional_dispatch_is_reported_on_the_debug_stream() {
    let graph = routed_graph("go_left").with_checkpointer(MemoryCheckpointer::new());

    let mut stream =
        Box::pin(graph.stream(State::new(), ExecutionConfig::new("dbg"), StreamMode::Debug));
    let mut dispatches = Vec::new();
    while let Some(Ok(event)) = stream.next().await {
        if let StreamEvent::RouteDispatched { source, targets } = event {
            dispatches.push((source, targets));
        }
    }

    assert_eq!(
        dispatches,
        vec![("start".to_string(), vec!["left".to_string()])],
        "the conditional edge from `start` must be reported once"
    );
}

#[tokio::test]
async fn an_unconditional_edge_reports_no_dispatch() {
    // Only a conditional edge involves a routing decision, so a plain chain
    // must stay quiet rather than reporting every edge it follows.
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "only",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) },
        )
        .add_edge(START, "only")
        .add_edge("only", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    let mut stream =
        Box::pin(graph.stream(State::new(), ExecutionConfig::new("plain"), StreamMode::Debug));
    let mut saw_dispatch = false;
    while let Some(Ok(event)) = stream.next().await {
        if matches!(event, StreamEvent::RouteDispatched { .. }) {
            saw_dispatch = true;
        }
    }
    assert!(!saw_dispatch, "a graph with no conditional edge reports no dispatch");
}

#[cfg(feature = "time-travel")]
#[tokio::test]
async fn time_travel_without_a_checkpointer_is_an_error_not_a_panic() {
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "only",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) },
        )
        .add_edge(START, "only")
        .add_edge("only", END)
        .compile()
        .unwrap();

    let error = match graph.time_travel("no-checkpointer") {
        Err(error) => error,
        Ok(_) => panic!("a graph with no checkpointer must not produce a handle"),
    };
    assert!(
        matches!(error, GraphError::CheckpointError(_)),
        "expected a checkpoint error, got {error:?}"
    );
}

#[cfg(feature = "time-travel")]
#[tokio::test]
async fn time_travel_with_a_checkpointer_succeeds() {
    let graph = StateGraph::with_channels(&["value"])
        .add_node_fn(
            "only",
            |_ctx| async move { Ok(NodeOutput::new().with_update("value", json!(1))) },
        )
        .add_edge(START, "only")
        .add_edge("only", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new());

    assert!(graph.time_travel("thread").is_ok());
}
