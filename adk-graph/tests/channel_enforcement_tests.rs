//! Tests for channel enforcement.
//!
//! Without enforcement a node may write any channel name. An undeclared name
//! gets `Reducer::Overwrite`, because that is the fallback for a channel the
//! schema does not hold. A graph that declared a list channel and then wrote a
//! near-miss name therefore loses every value but the last, and reports no
//! error. These tests pin the opt-in check that catches it.

use adk_graph::checkpoint::MemoryCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::error::GraphError;
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::{State, StateSchema};
use adk_graph::stream::StreamMode;
use futures::StreamExt;
use serde_json::json;

#[tokio::test]
async fn an_undeclared_channel_is_rejected_under_enforcement() {
    let graph = StateGraph::with_channels(&["declared"])
        .add_node_fn(
            "writer",
            |_ctx| async move { Ok(NodeOutput::new().with_update("typo", json!(1))) },
        )
        .add_edge(START, "writer")
        .add_edge("writer", END)
        .compile()
        .unwrap()
        .with_strict_channels();

    let error = graph
        .invoke(State::new(), ExecutionConfig::new("strict"))
        .await
        .expect_err("an undeclared channel must fail the run");

    match error {
        GraphError::UndeclaredChannel { node, channel } => {
            assert_eq!(node, "writer");
            assert_eq!(channel, "typo");
        }
        other => panic!("expected UndeclaredChannel, got {other:?}"),
    }
}

#[tokio::test]
async fn without_enforcement_an_undeclared_channel_is_accepted() {
    // The opt-in guarantee. Every graph built before this feature keeps working.
    let graph = StateGraph::with_channels(&["declared"])
        .add_node_fn(
            "writer",
            |_ctx| async move { Ok(NodeOutput::new().with_update("typo", json!(1))) },
        )
        .add_edge(START, "writer")
        .add_edge("writer", END)
        .compile()
        .unwrap();

    let state = graph.invoke(State::new(), ExecutionConfig::new("lax")).await.unwrap();
    assert_eq!(state.get("typo"), Some(&json!(1)));
}

#[tokio::test]
async fn a_declared_channel_passes_under_enforcement() {
    let graph = StateGraph::with_channels(&["declared"])
        .add_node_fn("writer", |_ctx| async move {
            Ok(NodeOutput::new().with_update("declared", json!("ok")))
        })
        .add_edge(START, "writer")
        .add_edge("writer", END)
        .compile()
        .unwrap()
        .with_strict_channels();

    let state = graph.invoke(State::new(), ExecutionConfig::new("strict-ok")).await.unwrap();
    assert_eq!(state.get("declared"), Some(&json!("ok")));
}

#[tokio::test]
async fn a_graph_with_no_declared_channels_accepts_anything() {
    // Enforcement has nothing to check against, so it stays inert rather than
    // rejecting every write.
    let graph = StateGraph::new(StateSchema::new())
        .add_node_fn("writer", |_ctx| async move {
            Ok(NodeOutput::new().with_update("anything", json!(1)))
        })
        .add_edge(START, "writer")
        .add_edge("writer", END)
        .compile()
        .unwrap()
        .with_strict_channels();

    let state = graph.invoke(State::new(), ExecutionConfig::new("empty")).await.unwrap();
    assert_eq!(state.get("anything"), Some(&json!(1)));
}

#[tokio::test]
async fn the_near_miss_that_enforcement_exists_to_catch() {
    // `messages` appends; `message` does not exist, so it overwrites. Without
    // enforcement the run reports success and keeps only the last value.
    let schema = StateSchema::builder().list_channel("messages").build();

    let build = |strict: bool| {
        let graph = StateGraph::new(schema.clone())
            .add_node_fn("first", |_ctx| async move {
                Ok(NodeOutput::new().with_update("message", json!("a")))
            })
            .add_node_fn("second", |_ctx| async move {
                Ok(NodeOutput::new().with_update("message", json!("b")))
            })
            .add_edge(START, "first")
            .add_edge("first", "second")
            .add_edge("second", END)
            .compile()
            .unwrap();
        if strict { graph.with_strict_channels() } else { graph }
    };

    // Silent loss: two writes, one survivor, no error.
    let state = build(false).invoke(State::new(), ExecutionConfig::new("lossy")).await.unwrap();
    assert_eq!(state.get("message"), Some(&json!("b")), "the first value is gone");
    assert_eq!(state.get("messages"), Some(&json!([])), "the intended channel stayed empty");

    // Under enforcement the same graph names the mistake.
    let error = build(true)
        .invoke(State::new(), ExecutionConfig::new("caught"))
        .await
        .expect_err("the near miss must be reported");
    assert!(
        matches!(error, GraphError::UndeclaredChannel { ref channel, .. } if channel == "message")
    );
}

#[tokio::test]
async fn enforcement_also_covers_the_streamed_path() {
    let graph = StateGraph::with_channels(&["declared"])
        .add_node_fn(
            "writer",
            |_ctx| async move { Ok(NodeOutput::new().with_update("typo", json!(1))) },
        )
        .add_edge(START, "writer")
        .add_edge("writer", END)
        .compile()
        .unwrap()
        .with_checkpointer(MemoryCheckpointer::new())
        .with_strict_channels();

    let mut stream =
        Box::pin(graph.stream(State::new(), ExecutionConfig::new("streamed"), StreamMode::Values));
    let mut saw_error = false;
    while let Some(item) = stream.next().await {
        if item.is_err() {
            saw_error = true;
        }
    }
    assert!(saw_error, "the streamed path must reject an undeclared channel too");
}
